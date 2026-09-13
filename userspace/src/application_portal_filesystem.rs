//! Authenticated filesystem-session adapter for trusted application picker pages.

use core::{array, mem::size_of};

use crate::{
    application_permission::{
        ApplicationResourceIdentity, ApplicationResourceKind, ApplicationResourceResolveError,
        ApplicationResourceResolver, ApplicationResourceRestoreError, ApplicationResourceRestorer,
    },
    application_portal_picker::{
        ApplicationPickerEntry, ApplicationPickerError, ApplicationPortalPicker,
    },
    filesystem::{self, Node, Session, protocol},
    handle::{OwnedHandle, SharedMemory},
    ipc,
};

pub const APPLICATION_PICKER_FILESYSTEM_PAGE_ENTRIES: usize = 8;
pub const APPLICATION_PICKER_FILESYSTEM_BUFFER_BYTES: usize =
    APPLICATION_PICKER_FILESYSTEM_PAGE_ENTRIES * size_of::<protocol::DirectoryEntry>();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPortalFilesystemError {
    InvalidConfiguration,
    RequestIdExhausted,
    SharedMemory(ipc::Error),
    Filesystem(filesystem::Error),
    Restore(ApplicationResourceRestoreError),
    Resolve(ApplicationResourceResolveError),
    InvalidDirectoryEntry,
    EntryIdentityMismatch,
    Picker(ApplicationPickerError),
}

/// Owns the one shared-memory attachment used to fetch bounded directory pages from an
/// authenticated filesystem session.
pub struct ApplicationPortalFilesystemBrowser {
    session: Session,
    resolver: ApplicationResourceResolver,
    restorer: ApplicationResourceRestorer,
    shared: OwnedHandle<SharedMemory>,
    buffer_id: u64,
    next_request_id: u64,
}

impl ApplicationPortalFilesystemBrowser {
    /// Attaches one private page buffer and resolves the provider root to a stable identity.
    pub fn attach(
        session: Session,
        filesystem_uuid: [u8; 16],
        buffer_id: u64,
        first_request_id: u64,
    ) -> Result<(Self, ApplicationResourceIdentity), ApplicationPortalFilesystemError> {
        if buffer_id == 0 || first_request_id == 0 {
            return Err(ApplicationPortalFilesystemError::InvalidConfiguration);
        }
        let root_request_id = first_request_id
            .checked_add(1)
            .ok_or(ApplicationPortalFilesystemError::RequestIdExhausted)?;
        let next_request_id = root_request_id
            .checked_add(1)
            .ok_or(ApplicationPortalFilesystemError::RequestIdExhausted)?;
        let resolver = ApplicationResourceResolver::new(session, filesystem_uuid)
            .ok_or(ApplicationPortalFilesystemError::InvalidConfiguration)?;
        let restorer = ApplicationResourceRestorer::new(session, filesystem_uuid)
            .ok_or(ApplicationPortalFilesystemError::InvalidConfiguration)?;
        let shared =
            OwnedHandle::<SharedMemory>::create(APPLICATION_PICKER_FILESYSTEM_BUFFER_BYTES)
                .map_err(ApplicationPortalFilesystemError::SharedMemory)?;
        session
            .attach_shared_buffer(
                first_request_id,
                buffer_id,
                shared.as_raw(),
                APPLICATION_PICKER_FILESYSTEM_BUFFER_BYTES,
            )
            .map_err(ApplicationPortalFilesystemError::Filesystem)?;
        let mut browser = Self {
            session,
            resolver,
            restorer,
            shared,
            buffer_id,
            next_request_id: root_request_id,
        };
        let request_id = browser.take_request_id()?;
        debug_assert_eq!(browser.next_request_id, next_request_id);
        let root = match browser.resolver.resolve(
            request_id,
            Node::root(session),
            ApplicationResourceKind::Directory,
        ) {
            Ok(root) => root,
            Err(error) => {
                let _ = browser.detach();
                return Err(ApplicationPortalFilesystemError::Resolve(error));
            }
        };
        Ok((browser, root))
    }

    /// Fetches and authenticates the picker's next provider page before changing picker state.
    pub fn load_next_page(
        &mut self,
        picker: &mut ApplicationPortalPicker,
    ) -> Result<usize, ApplicationPortalFilesystemError> {
        let directory_identity = picker.current_directory();
        let cookie = picker.expected_cookie();
        let restore_request = self.take_request_id()?;
        let directory = self
            .restorer
            .restore(restore_request, directory_identity)
            .map_err(ApplicationPortalFilesystemError::Restore)?;
        let page_request = self.take_request_id()?;
        let bulk = protocol::BulkBuffer {
            buffer_id: self.buffer_id,
            offset: 0,
            length: APPLICATION_PICKER_FILESYSTEM_BUFFER_BYTES as u64,
        };
        let batch = self
            .session
            .read_directory_to_shared_buffer(page_request, directory, cookie, bulk)
            .map_err(ApplicationPortalFilesystemError::Filesystem)?;
        if batch.count > APPLICATION_PICKER_FILESYSTEM_PAGE_ENTRIES
            || batch.count == 0 && !batch.end
        {
            return Err(ApplicationPortalFilesystemError::InvalidDirectoryEntry);
        }

        let mut entries: [Option<ApplicationPickerEntry>;
            APPLICATION_PICKER_FILESYSTEM_PAGE_ENTRIES] = array::from_fn(|_| None);
        let mut next_cookie = cookie;
        for (index, slot) in entries.iter_mut().enumerate().take(batch.count) {
            let record = self.read_directory_entry(index)?;
            let (name, kind) = validate_directory_entry(&record, next_cookie)?;
            next_cookie = record.next_cookie;
            let lookup_request = self.take_request_id()?;
            let node = self
                .session
                .lookup_node(lookup_request, directory, name)
                .map_err(ApplicationPortalFilesystemError::Filesystem)?;
            if node.id() != record.node_id {
                return Err(ApplicationPortalFilesystemError::EntryIdentityMismatch);
            }
            let resolve_request = self.take_request_id()?;
            let resource = self
                .resolver
                .resolve(resolve_request, node, kind)
                .map_err(ApplicationPortalFilesystemError::Resolve)?;
            *slot = Some(
                ApplicationPickerEntry::new(name, resource)
                    .map_err(ApplicationPortalFilesystemError::Picker)?,
            );
        }
        let published_cookie = if batch.end { 0 } else { next_cookie };
        picker
            .accept_authenticated_page_slots(
                directory_identity,
                cookie,
                &entries,
                batch.count,
                published_cookie,
                batch.end,
            )
            .map_err(ApplicationPortalFilesystemError::Picker)?;
        Ok(batch.count)
    }

    /// Detaches the provider buffer. Dropping without this call remains safe because session
    /// teardown releases provider attachments, but explicit service shutdown should call it.
    pub fn detach(mut self) -> Result<(), ApplicationPortalFilesystemError> {
        let request_id = self.take_request_id()?;
        self.session
            .detach_shared_buffer(request_id, self.buffer_id)
            .map_err(ApplicationPortalFilesystemError::Filesystem)
    }

    fn read_directory_entry(
        &self,
        index: usize,
    ) -> Result<protocol::DirectoryEntry, ApplicationPortalFilesystemError> {
        let offset = index
            .checked_mul(size_of::<protocol::DirectoryEntry>())
            .ok_or(ApplicationPortalFilesystemError::InvalidDirectoryEntry)?;
        let mut bytes = [0; size_of::<protocol::DirectoryEntry>()];
        if self.shared.read(offset, &mut bytes).ok() != Some(bytes.len()) {
            return Err(ApplicationPortalFilesystemError::SharedMemory(
                ipc::Error::IO,
            ));
        }
        Ok(unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const protocol::DirectoryEntry) })
    }

    fn take_request_id(&mut self) -> Result<u64, ApplicationPortalFilesystemError> {
        let request_id = self.next_request_id;
        self.next_request_id = request_id
            .checked_add(1)
            .ok_or(ApplicationPortalFilesystemError::RequestIdExhausted)?;
        Ok(request_id)
    }
}

fn validate_directory_entry(
    entry: &protocol::DirectoryEntry,
    previous_cookie: u64,
) -> Result<(&[u8], ApplicationResourceKind), ApplicationPortalFilesystemError> {
    let name_length = usize::from(entry.name_length);
    if entry.node_id == protocol::INVALID_ID
        || entry.next_cookie <= previous_cookie
        || entry.reserved != 0
        || name_length == 0
        || name_length > protocol::MAX_NAME_BYTES
        || entry.name[name_length..].iter().any(|byte| *byte != 0)
    {
        return Err(ApplicationPortalFilesystemError::InvalidDirectoryEntry);
    }
    let name = &entry.name[..name_length];
    if name == b"." || name == b".." || name.contains(&0) || name.contains(&b'/') {
        return Err(ApplicationPortalFilesystemError::InvalidDirectoryEntry);
    }
    let kind = match entry.kind {
        protocol::node_kind::FILE => ApplicationResourceKind::File,
        protocol::node_kind::DIRECTORY => ApplicationResourceKind::Directory,
        _ => return Err(ApplicationPortalFilesystemError::InvalidDirectoryEntry),
    };
    Ok((name, kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &[u8], node: u64, cookie: u64, kind: u16) -> protocol::DirectoryEntry {
        let mut entry = protocol::DirectoryEntry::EMPTY;
        entry.node_id = node;
        entry.next_cookie = cookie;
        entry.kind = kind;
        entry.name_length = name.len() as u16;
        entry.name[..name.len()].copy_from_slice(name);
        entry
    }

    #[test]
    fn directory_records_are_canonical_monotonic_and_never_symlinks() {
        assert_eq!(
            validate_directory_entry(&entry(b"file", 2, 1, protocol::node_kind::FILE), 0),
            Ok((b"file".as_slice(), ApplicationResourceKind::File))
        );
        assert_eq!(
            validate_directory_entry(&entry(b"..", 2, 1, protocol::node_kind::DIRECTORY), 0),
            Err(ApplicationPortalFilesystemError::InvalidDirectoryEntry)
        );
        assert_eq!(
            validate_directory_entry(&entry(b"link", 2, 1, protocol::node_kind::SYMBOLIC_LINK), 0),
            Err(ApplicationPortalFilesystemError::InvalidDirectoryEntry)
        );
        assert_eq!(
            validate_directory_entry(&entry(b"late", 2, 4, protocol::node_kind::FILE), 4),
            Err(ApplicationPortalFilesystemError::InvalidDirectoryEntry)
        );
    }
}
