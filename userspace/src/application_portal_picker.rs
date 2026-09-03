//! Bounded trusted-picker and filesystem-browsing policy for application portals.
//!
//! Directory contents enter this state machine only after the portal has obtained them from an
//! authenticated filesystem provider. Applications never supply paths or node identifiers to it.
//! Navigation can target only entries in the current provider page, preventing ambient-path and
//! `..` traversal from crossing the selected volume boundary.

use core::array;

use crate::{
    application_permission::{
        ApplicationPermissionStore, ApplicationResourceIdentity, ApplicationResourceKind,
    },
    application_portal::{AdmittedPortalRequest, ApplicationPortalOperation},
    application_selection::{ApplicationSelectionPrepareError, PreparedApplicationSelection},
    filesystem::protocol,
};

pub const MAX_APPLICATION_PICKER_ENTRIES: usize = 32;
pub const MAX_APPLICATION_PICKER_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationPickerEntry {
    resource: ApplicationResourceIdentity,
    name: [u8; protocol::MAX_NAME_BYTES],
    name_length: u8,
}

impl ApplicationPickerEntry {
    pub fn new(
        name: &[u8],
        resource: ApplicationResourceIdentity,
    ) -> Result<Self, ApplicationPickerError> {
        if !canonical_name(name) {
            return Err(ApplicationPickerError::InvalidName);
        }
        let mut stored = [0; protocol::MAX_NAME_BYTES];
        stored[..name.len()].copy_from_slice(name);
        Ok(Self {
            resource,
            name: stored,
            name_length: name.len() as u8,
        })
    }

    pub const fn resource(self) -> ApplicationResourceIdentity {
        self.resource
    }

    pub fn name(&self) -> &[u8] {
        &self.name[..usize::from(self.name_length)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPickerError {
    RootNotDirectory,
    InvalidName,
    WrongDirectory,
    WrongPageCookie,
    ForeignFilesystem,
    DuplicateEntry,
    Capacity,
    UnknownEntry,
    NotDirectory,
    SelectionKind,
    HistoryFull,
    AtRoot,
    PageComplete,
}

/// One portal-owned picker transaction. UI code may render the exposed entries but cannot inject
/// a path: all state transitions name a slot populated by the authenticated provider.
pub struct ApplicationPortalPicker {
    admission: AdmittedPortalRequest,
    root: ApplicationResourceIdentity,
    directory: ApplicationResourceIdentity,
    history: [Option<ApplicationResourceIdentity>; MAX_APPLICATION_PICKER_DEPTH],
    depth: usize,
    entries: [Option<ApplicationPickerEntry>; MAX_APPLICATION_PICKER_ENTRIES],
    entry_count: usize,
    expected_cookie: u64,
    page_complete: bool,
}

impl ApplicationPortalPicker {
    pub fn new(
        admission: AdmittedPortalRequest,
        root: ApplicationResourceIdentity,
    ) -> Result<Self, ApplicationPickerError> {
        if root.kind() != ApplicationResourceKind::Directory {
            return Err(ApplicationPickerError::RootNotDirectory);
        }
        Ok(Self {
            admission,
            root,
            directory: root,
            history: [None; MAX_APPLICATION_PICKER_DEPTH],
            depth: 0,
            entries: array::from_fn(|_| None),
            entry_count: 0,
            expected_cookie: 0,
            page_complete: false,
        })
    }

    pub const fn admission(&self) -> AdmittedPortalRequest {
        self.admission
    }

    pub const fn current_directory(&self) -> ApplicationResourceIdentity {
        self.directory
    }

    pub const fn expected_cookie(&self) -> u64 {
        self.expected_cookie
    }

    pub const fn page_complete(&self) -> bool {
        self.page_complete
    }

    pub fn entries(&self) -> impl ExactSizeIterator<Item = ApplicationPickerEntry> + '_ {
        self.entries[..self.entry_count]
            .iter()
            .map(|entry| entry.expect("bounded picker entries remain dense"))
    }

    /// Appends one authenticated provider page. The caller must bind `directory` and `cookie` to
    /// the filesystem request whose reply supplied these entries.
    pub fn accept_authenticated_page(
        &mut self,
        directory: ApplicationResourceIdentity,
        cookie: u64,
        entries: &[ApplicationPickerEntry],
        next_cookie: u64,
        end: bool,
    ) -> Result<(), ApplicationPickerError> {
        self.accept_authenticated_page_iter(
            directory,
            cookie,
            entries.iter().copied(),
            next_cookie,
            end,
        )
    }

    pub(crate) fn accept_authenticated_page_slots<const N: usize>(
        &mut self,
        directory: ApplicationResourceIdentity,
        cookie: u64,
        entries: &[Option<ApplicationPickerEntry>; N],
        entry_count: usize,
        next_cookie: u64,
        end: bool,
    ) -> Result<(), ApplicationPickerError> {
        if entry_count > N
            || entries[..entry_count].iter().any(Option::is_none)
            || entries[entry_count..].iter().any(Option::is_some)
        {
            return Err(ApplicationPickerError::Capacity);
        }
        self.accept_authenticated_page_iter(
            directory,
            cookie,
            entries[..entry_count]
                .iter()
                .map(|entry| entry.expect("validated buffered picker entry remains present")),
            next_cookie,
            end,
        )
    }

    fn accept_authenticated_page_iter<I>(
        &mut self,
        directory: ApplicationResourceIdentity,
        cookie: u64,
        entries: I,
        next_cookie: u64,
        end: bool,
    ) -> Result<(), ApplicationPickerError>
    where
        I: Clone + ExactSizeIterator<Item = ApplicationPickerEntry>,
    {
        if directory != self.directory {
            return Err(ApplicationPickerError::WrongDirectory);
        }
        if cookie != self.expected_cookie {
            return Err(ApplicationPickerError::WrongPageCookie);
        }
        if self.page_complete {
            return Err(ApplicationPickerError::PageComplete);
        }
        if !end && next_cookie == 0 || end && next_cookie != 0 {
            return Err(ApplicationPickerError::WrongPageCookie);
        }
        if self.entry_count + entries.len() > MAX_APPLICATION_PICKER_ENTRIES {
            return Err(ApplicationPickerError::Capacity);
        }
        for (offset, candidate) in entries.clone().enumerate() {
            if candidate.resource.filesystem_uuid() != self.root.filesystem_uuid() {
                return Err(ApplicationPickerError::ForeignFilesystem);
            }
            if self.entries[..self.entry_count]
                .iter()
                .flatten()
                .copied()
                .chain(entries.clone().take(offset))
                .any(|existing| {
                    existing.resource == candidate.resource || existing.name() == candidate.name()
                })
            {
                return Err(ApplicationPickerError::DuplicateEntry);
            }
        }
        for entry in entries {
            self.entries[self.entry_count] = Some(entry);
            self.entry_count += 1;
        }
        self.expected_cookie = next_cookie;
        self.page_complete = end;
        Ok(())
    }

    pub fn enter_directory(&mut self, index: usize) -> Result<(), ApplicationPickerError> {
        let entry = self.entry(index)?;
        if entry.resource.kind() != ApplicationResourceKind::Directory {
            return Err(ApplicationPickerError::NotDirectory);
        }
        if self.depth == MAX_APPLICATION_PICKER_DEPTH {
            return Err(ApplicationPickerError::HistoryFull);
        }
        self.history[self.depth] = Some(self.directory);
        self.depth += 1;
        self.directory = entry.resource;
        self.clear_page();
        Ok(())
    }

    pub fn go_back(&mut self) -> Result<(), ApplicationPickerError> {
        if self.depth == 0 {
            return Err(ApplicationPickerError::AtRoot);
        }
        self.depth -= 1;
        self.directory = self.history[self.depth]
            .take()
            .expect("picker history remains dense");
        self.clear_page();
        Ok(())
    }

    /// Selects a visible entry only when its provider-authenticated kind matches the operation.
    pub fn select_entry(
        &self,
        index: usize,
    ) -> Result<ApplicationResourceIdentity, ApplicationPickerError> {
        let resource = self.entry(index)?.resource;
        let expected = match self.admission.request().operation() {
            ApplicationPortalOperation::OpenFile | ApplicationPortalOperation::SaveFile => {
                ApplicationResourceKind::File
            }
            ApplicationPortalOperation::SelectDirectory => ApplicationResourceKind::Directory,
        };
        if resource.kind() != expected {
            return Err(ApplicationPickerError::SelectionKind);
        }
        Ok(resource)
    }

    /// Prepares the permission mutation and grant-bound resource endpoint for one visible choice.
    /// The caller completes it through the pending portal reply, preserving the existing atomic
    /// move-transfer boundary.
    pub fn prepare_new_selection<'a>(
        &self,
        index: usize,
        store: &'a mut ApplicationPermissionStore,
    ) -> Result<PreparedApplicationSelection<'a>, ApplicationPickerSelectionError> {
        let resource = self
            .select_entry(index)
            .map_err(ApplicationPickerSelectionError::Picker)?;
        PreparedApplicationSelection::issue(store, self.admission, resource)
            .map_err(ApplicationPickerSelectionError::Selection)
    }

    /// Directory selection may choose the displayed directory without synthesizing a `.` entry.
    pub fn select_current_directory(
        &self,
    ) -> Result<ApplicationResourceIdentity, ApplicationPickerError> {
        if self.admission.request().operation() != ApplicationPortalOperation::SelectDirectory {
            return Err(ApplicationPickerError::SelectionKind);
        }
        Ok(self.directory)
    }

    fn entry(&self, index: usize) -> Result<ApplicationPickerEntry, ApplicationPickerError> {
        self.entries
            .get(index)
            .and_then(|entry| *entry)
            .ok_or(ApplicationPickerError::UnknownEntry)
    }

    fn clear_page(&mut self) {
        self.entries.fill(None);
        self.entry_count = 0;
        self.expected_cookie = 0;
        self.page_complete = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPickerSelectionError {
    Picker(ApplicationPickerError),
    Selection(ApplicationSelectionPrepareError),
}

fn canonical_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= protocol::MAX_NAME_BYTES
        && name != b"."
        && name != b".."
        && !name.contains(&0)
        && !name.contains(&b'/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application_identity::{
            ApplicationInstallScope, ApplicationInstallation, ApplicationLaunchSelection,
            ApplicationProfile, ApplicationProfileSet, ApplicationTrustClass,
            InstalledApplicationComponent, PackageVerification, authorize_application_launch,
        },
        application_permission::{ApplicationGrantRights, ApplicationGrantScope},
        application_portal::{
            ApplicationPortalAdmission, ApplicationPortalRequest, TrustedUserGestureTicket,
        },
    };

    fn resource(id: u64, kind: ApplicationResourceKind) -> ApplicationResourceIdentity {
        ApplicationResourceIdentity::new([7; 16], id, 1, kind).unwrap()
    }

    fn admitted(operation: ApplicationPortalOperation) -> AdmittedPortalRequest {
        let components = [InstalledApplicationComponent::new(
            21,
            b"/application",
            ApplicationProfileSet::DESKTOP,
            true,
        )];
        let authorization = authorize_application_launch(
            PackageVerification {
                package: 11,
                package_generation: 12,
                application: 13,
                publisher: 14,
                signing_lineage: 15,
                trust_class: ApplicationTrustClass::Repository,
                system_application: false,
                components: &components,
            },
            ApplicationInstallation {
                installation: 16,
                package: 11,
                package_generation: 12,
                application: 13,
                publisher: 14,
                signing_lineage: 15,
                trust_class: ApplicationTrustClass::Repository,
                scope: ApplicationInstallScope::User,
                owner_user: 17,
                system_application: false,
            },
            ApplicationLaunchSelection {
                component: 21,
                user: 17,
                session: 18,
                profile: ApplicationProfile::Desktop,
            },
        )
        .unwrap();
        let ticket =
            TrustedUserGestureTicket::new(40, 50, 17, 18, 13, 16, 60, 1, 1, 100, 200).unwrap();
        let mut admission = ApplicationPortalAdmission::new(70).unwrap();
        admission.register_ticket(70, 100, ticket).unwrap();
        let request = ApplicationPortalRequest::new(
            80,
            40,
            60,
            operation,
            ApplicationGrantRights::READ,
            ApplicationGrantScope::Session,
        )
        .unwrap();
        admission
            .admit_request(50, 101, authorization, request)
            .unwrap()
    }

    #[test]
    fn picker_navigation_is_rooted_in_authenticated_entries() {
        let root = resource(1, ApplicationResourceKind::Directory);
        let directory = ApplicationPickerEntry::new(
            b"Documents",
            resource(2, ApplicationResourceKind::Directory),
        )
        .unwrap();
        let file =
            ApplicationPickerEntry::new(b"notes.txt", resource(3, ApplicationResourceKind::File))
                .unwrap();
        let mut picker =
            ApplicationPortalPicker::new(admitted(ApplicationPortalOperation::OpenFile), root)
                .unwrap();
        picker
            .accept_authenticated_page(root, 0, &[directory, file], 0, true)
            .unwrap();
        assert_eq!(picker.select_entry(1), Ok(file.resource()));
        assert_eq!(
            picker.select_entry(0),
            Err(ApplicationPickerError::SelectionKind)
        );
        picker.enter_directory(0).unwrap();
        assert_eq!(picker.current_directory(), directory.resource());
        assert_eq!(picker.go_back(), Ok(()));
        assert_eq!(picker.current_directory(), root);
    }

    #[test]
    fn picker_rejects_traversal_foreign_and_duplicate_entries() {
        assert_eq!(
            ApplicationPickerEntry::new(b"..", resource(2, ApplicationResourceKind::Directory)),
            Err(ApplicationPickerError::InvalidName)
        );
        let root = resource(1, ApplicationResourceKind::Directory);
        let first =
            ApplicationPickerEntry::new(b"same", resource(2, ApplicationResourceKind::Directory))
                .unwrap();
        let duplicate =
            ApplicationPickerEntry::new(b"same", resource(3, ApplicationResourceKind::File))
                .unwrap();
        let mut picker = ApplicationPortalPicker::new(
            admitted(ApplicationPortalOperation::SelectDirectory),
            root,
        )
        .unwrap();
        assert_eq!(
            picker.accept_authenticated_page(root, 0, &[first, duplicate], 0, true),
            Err(ApplicationPickerError::DuplicateEntry)
        );
        assert_eq!(picker.select_current_directory(), Ok(root));
    }
}
