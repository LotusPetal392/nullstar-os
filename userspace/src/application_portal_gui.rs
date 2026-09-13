//! Allocation-free presentation and input policy for the trusted application picker.
//!
//! The renderer emits compositor primitives through [`PortalPickerRenderTarget`]. It does not
//! map the boot framebuffer or trust application-supplied pixels. A future compositor process can
//! implement the target with its normal surface backend while keeping picker state and input in
//! the trusted portal.

use crate::{
    application_permission::{ApplicationResourceIdentity, ApplicationResourceKind},
    application_portal::ApplicationPortalOperation,
    application_portal_picker::{
        ApplicationPickerEntry, ApplicationPickerError, ApplicationPortalPicker,
        MAX_APPLICATION_PICKER_ENTRIES,
    },
};

pub const MIN_PICKER_WIDTH: u32 = 320;
pub const MIN_PICKER_HEIGHT: u32 = 160;
pub const PICKER_HEADER_HEIGHT: u32 = 56;
pub const PICKER_FOOTER_HEIGHT: u32 = 44;
pub const PICKER_ROW_HEIGHT: u32 = 28;

const OUTER_PADDING: u32 = 12;
const TEXT_INSET: u32 = 9;
const KIND_COLUMN_WIDTH: u32 = 54;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalColor(pub u32);

impl PortalColor {
    pub const BACKGROUND: Self = Self(0xff_181b20);
    pub const PANEL: Self = Self(0xff_232832);
    pub const BORDER: Self = Self(0xff_566171);
    pub const TEXT: Self = Self(0xff_f4f7fb);
    pub const MUTED_TEXT: Self = Self(0xff_aeb8c6);
    pub const SELECTED: Self = Self(0xff_315f98);
    pub const SELECTED_BORDER: Self = Self(0xff_83b8f4);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Minimal drawing contract required by the picker.
///
/// Text is passed as bytes because authenticated filesystem names need not be UTF-8. A backend
/// should render unsupported byte sequences with its replacement glyph rather than rejecting the
/// complete frame.
pub trait PortalPickerRenderTarget {
    type Error;

    fn dimensions(&self) -> (u32, u32);
    fn fill_rect(&mut self, rect: PortalRect, color: PortalColor) -> Result<(), Self::Error>;
    fn stroke_rect(&mut self, rect: PortalRect, color: PortalColor) -> Result<(), Self::Error>;
    fn draw_text(
        &mut self,
        x: u32,
        baseline_y: u32,
        text: &[u8],
        color: PortalColor,
    ) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalPickerRenderError<E> {
    SurfaceTooSmall { width: u32, height: u32 },
    Target(E),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalPickerEvent {
    MoveUp,
    MoveDown,
    PageUp,
    PageDown,
    SelectIndex(usize),
    ActivateIndex(usize),
    Activate,
    Confirm,
    NavigateBack,
    RequestNextPage,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalPickerOutcome {
    Updated,
    PageRequired {
        directory: ApplicationResourceIdentity,
        cookie: u64,
    },
    Selected {
        resource: ApplicationResourceIdentity,
        entry_index: Option<usize>,
    },
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalPickerInteractionError {
    InvalidSessionId,
    Completed,
    Picker(ApplicationPickerError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalPickerSessionState {
    Active,
    Selected {
        resource: ApplicationResourceIdentity,
        entry_index: Option<usize>,
    },
    Cancelled,
}

/// One trusted picker transaction plus its compositor-owned presentation state.
pub struct PortalPickerSession {
    id: u64,
    picker: ApplicationPortalPicker,
    selected_index: usize,
    first_visible: usize,
    visible_rows: usize,
    state: PortalPickerSessionState,
}

impl PortalPickerSession {
    pub fn new(
        id: u64,
        picker: ApplicationPortalPicker,
    ) -> Result<Self, PortalPickerInteractionError> {
        if id == 0 {
            return Err(PortalPickerInteractionError::InvalidSessionId);
        }
        Ok(Self {
            id,
            picker,
            selected_index: 0,
            first_visible: 0,
            visible_rows: 1,
            state: PortalPickerSessionState::Active,
        })
    }

    pub const fn id(&self) -> u64 {
        self.id
    }

    pub const fn picker(&self) -> &ApplicationPortalPicker {
        &self.picker
    }

    pub fn picker_mut(&mut self) -> &mut ApplicationPortalPicker {
        &mut self.picker
    }

    pub const fn state(&self) -> PortalPickerSessionState {
        self.state
    }

    pub const fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub const fn first_visible(&self) -> usize {
        self.first_visible
    }

    pub fn accept_authenticated_page(
        &mut self,
        directory: ApplicationResourceIdentity,
        cookie: u64,
        entries: &[ApplicationPickerEntry],
        next_cookie: u64,
        end: bool,
    ) -> Result<(), ApplicationPickerError> {
        self.picker
            .accept_authenticated_page(directory, cookie, entries, next_cookie, end)?;
        self.reconcile_selection();
        Ok(())
    }

    pub fn handle_event(
        &mut self,
        event: PortalPickerEvent,
    ) -> Result<PortalPickerOutcome, PortalPickerInteractionError> {
        if self.state != PortalPickerSessionState::Active {
            return Err(PortalPickerInteractionError::Completed);
        }

        let outcome = match event {
            PortalPickerEvent::MoveUp => {
                self.selected_index = self.selected_index.saturating_sub(1);
                self.keep_selection_visible();
                PortalPickerOutcome::Updated
            }
            PortalPickerEvent::MoveDown => {
                let entry_count = self.picker.entries().len();
                if entry_count != 0 {
                    self.selected_index = (self.selected_index + 1).min(entry_count - 1);
                }
                self.keep_selection_visible();
                PortalPickerOutcome::Updated
            }
            PortalPickerEvent::PageUp => {
                self.selected_index = self.selected_index.saturating_sub(self.visible_rows);
                self.keep_selection_visible();
                PortalPickerOutcome::Updated
            }
            PortalPickerEvent::PageDown => {
                let entry_count = self.picker.entries().len();
                if entry_count != 0 {
                    self.selected_index =
                        (self.selected_index + self.visible_rows).min(entry_count - 1);
                }
                self.keep_selection_visible();
                PortalPickerOutcome::Updated
            }
            PortalPickerEvent::SelectIndex(index) => {
                self.select_index(index)?;
                PortalPickerOutcome::Updated
            }
            PortalPickerEvent::ActivateIndex(index) => {
                self.select_index(index)?;
                self.activate_selected()?
            }
            PortalPickerEvent::Activate => self.activate_selected()?,
            PortalPickerEvent::Confirm => self.confirm()?,
            PortalPickerEvent::NavigateBack => {
                self.picker
                    .go_back()
                    .map_err(PortalPickerInteractionError::Picker)?;
                self.reset_view();
                PortalPickerOutcome::PageRequired {
                    directory: self.picker.current_directory(),
                    cookie: self.picker.expected_cookie(),
                }
            }
            PortalPickerEvent::RequestNextPage => {
                if self.picker.page_complete() {
                    PortalPickerOutcome::Updated
                } else {
                    PortalPickerOutcome::PageRequired {
                        directory: self.picker.current_directory(),
                        cookie: self.picker.expected_cookie(),
                    }
                }
            }
            PortalPickerEvent::Cancel => {
                self.state = PortalPickerSessionState::Cancelled;
                PortalPickerOutcome::Cancelled
            }
        };
        Ok(outcome)
    }

    pub fn render<T: PortalPickerRenderTarget>(
        &mut self,
        target: &mut T,
    ) -> Result<(), PortalPickerRenderError<T::Error>> {
        let (width, height) = target.dimensions();
        if width < MIN_PICKER_WIDTH || height < MIN_PICKER_HEIGHT {
            return Err(PortalPickerRenderError::SurfaceTooSmall { width, height });
        }

        let body_height = height - PICKER_HEADER_HEIGHT - PICKER_FOOTER_HEIGHT;
        self.visible_rows =
            ((body_height / PICKER_ROW_HEIGHT) as usize).clamp(1, MAX_APPLICATION_PICKER_ENTRIES);
        self.reconcile_selection();

        target
            .fill_rect(
                PortalRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                PortalColor::BACKGROUND,
            )
            .map_err(PortalPickerRenderError::Target)?;
        target
            .draw_text(OUTER_PADDING, 24, b"Trusted file access", PortalColor::TEXT)
            .map_err(PortalPickerRenderError::Target)?;
        target
            .draw_text(
                OUTER_PADDING,
                44,
                operation_title(self.picker.admission().request().operation()),
                PortalColor::MUTED_TEXT,
            )
            .map_err(PortalPickerRenderError::Target)?;

        let list = PortalRect {
            x: OUTER_PADDING,
            y: PICKER_HEADER_HEIGHT,
            width: width - OUTER_PADDING * 2,
            height: body_height,
        };
        target
            .fill_rect(list, PortalColor::PANEL)
            .map_err(PortalPickerRenderError::Target)?;
        target
            .stroke_rect(list, PortalColor::BORDER)
            .map_err(PortalPickerRenderError::Target)?;

        for (visible_offset, entry) in self
            .picker
            .entries()
            .skip(self.first_visible)
            .take(self.visible_rows)
            .enumerate()
        {
            let index = self.first_visible + visible_offset;
            let row = PortalRect {
                x: list.x + 1,
                y: list.y + visible_offset as u32 * PICKER_ROW_HEIGHT + 1,
                width: list.width - 2,
                height: PICKER_ROW_HEIGHT,
            };
            let selected = index == self.selected_index;
            if selected {
                target
                    .fill_rect(row, PortalColor::SELECTED)
                    .map_err(PortalPickerRenderError::Target)?;
                target
                    .stroke_rect(row, PortalColor::SELECTED_BORDER)
                    .map_err(PortalPickerRenderError::Target)?;
            }
            let baseline = row.y + 19;
            target
                .draw_text(
                    row.x + TEXT_INSET,
                    baseline,
                    entry_kind_label(entry.resource().kind()),
                    PortalColor::MUTED_TEXT,
                )
                .map_err(PortalPickerRenderError::Target)?;
            target
                .draw_text(
                    row.x + TEXT_INSET + KIND_COLUMN_WIDTH,
                    baseline,
                    entry.name(),
                    PortalColor::TEXT,
                )
                .map_err(PortalPickerRenderError::Target)?;
        }

        target
            .draw_text(
                OUTER_PADDING,
                height - 17,
                footer_hint(self.picker.admission().request().operation()),
                PortalColor::MUTED_TEXT,
            )
            .map_err(PortalPickerRenderError::Target)?;
        Ok(())
    }

    fn activate_selected(&mut self) -> Result<PortalPickerOutcome, PortalPickerInteractionError> {
        let Some(entry) = self.picker.entries().nth(self.selected_index) else {
            return Err(PortalPickerInteractionError::Picker(
                ApplicationPickerError::UnknownEntry,
            ));
        };
        if entry.resource().kind() == ApplicationResourceKind::Directory {
            self.picker
                .enter_directory(self.selected_index)
                .map_err(PortalPickerInteractionError::Picker)?;
            self.reset_view();
            return Ok(PortalPickerOutcome::PageRequired {
                directory: self.picker.current_directory(),
                cookie: self.picker.expected_cookie(),
            });
        }
        let resource = self
            .picker
            .select_entry(self.selected_index)
            .map_err(PortalPickerInteractionError::Picker)?;
        self.finish_selection(resource, Some(self.selected_index))
    }

    fn select_index(&mut self, index: usize) -> Result<(), PortalPickerInteractionError> {
        if index >= self.picker.entries().len() {
            return Err(PortalPickerInteractionError::Picker(
                ApplicationPickerError::UnknownEntry,
            ));
        }
        self.selected_index = index;
        self.keep_selection_visible();
        Ok(())
    }

    fn confirm(&mut self) -> Result<PortalPickerOutcome, PortalPickerInteractionError> {
        if self.picker.admission().request().operation()
            == ApplicationPortalOperation::SelectDirectory
        {
            let resource = self
                .picker
                .select_current_directory()
                .map_err(PortalPickerInteractionError::Picker)?;
            return self.finish_selection(resource, None);
        }
        let resource = self
            .picker
            .select_entry(self.selected_index)
            .map_err(PortalPickerInteractionError::Picker)?;
        self.finish_selection(resource, Some(self.selected_index))
    }

    fn finish_selection(
        &mut self,
        resource: ApplicationResourceIdentity,
        entry_index: Option<usize>,
    ) -> Result<PortalPickerOutcome, PortalPickerInteractionError> {
        self.state = PortalPickerSessionState::Selected {
            resource,
            entry_index,
        };
        Ok(PortalPickerOutcome::Selected {
            resource,
            entry_index,
        })
    }

    fn reset_view(&mut self) {
        self.selected_index = 0;
        self.first_visible = 0;
    }

    fn reconcile_selection(&mut self) {
        let entry_count = self.picker.entries().len();
        if entry_count == 0 {
            self.reset_view();
            return;
        }
        self.selected_index = self.selected_index.min(entry_count - 1);
        self.keep_selection_visible();
    }

    fn keep_selection_visible(&mut self) {
        if self.selected_index < self.first_visible {
            self.first_visible = self.selected_index;
        } else if self.selected_index >= self.first_visible + self.visible_rows {
            self.first_visible = self.selected_index + 1 - self.visible_rows;
        }
    }
}

const fn operation_title(operation: ApplicationPortalOperation) -> &'static [u8] {
    match operation {
        ApplicationPortalOperation::OpenFile => b"Choose a file to open",
        ApplicationPortalOperation::SaveFile => b"Choose an existing file to replace",
        ApplicationPortalOperation::SelectDirectory => b"Choose a folder",
    }
}

const fn footer_hint(operation: ApplicationPortalOperation) -> &'static [u8] {
    match operation {
        ApplicationPortalOperation::OpenFile | ApplicationPortalOperation::SaveFile => {
            b"Enter: open folder/file   Back: parent   Esc: cancel"
        }
        ApplicationPortalOperation::SelectDirectory => {
            b"Enter: open folder   Confirm: choose current folder   Esc: cancel"
        }
    }
}

const fn entry_kind_label(kind: ApplicationResourceKind) -> &'static [u8] {
    match kind {
        ApplicationResourceKind::File => b"FILE",
        ApplicationResourceKind::Directory => b"DIR",
    }
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

    fn admitted(
        operation: ApplicationPortalOperation,
    ) -> crate::application_portal::AdmittedPortalRequest {
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
        let rights = match operation {
            ApplicationPortalOperation::SaveFile => ApplicationGrantRights::WRITE,
            ApplicationPortalOperation::OpenFile | ApplicationPortalOperation::SelectDirectory => {
                ApplicationGrantRights::READ
            }
        };
        let request = ApplicationPortalRequest::new(
            80,
            40,
            60,
            operation,
            rights,
            ApplicationGrantScope::Session,
        )
        .unwrap();
        admission
            .admit_request(50, 101, authorization, request)
            .unwrap()
    }

    fn session(operation: ApplicationPortalOperation) -> PortalPickerSession {
        let picker = ApplicationPortalPicker::new(
            admitted(operation),
            resource(1, ApplicationResourceKind::Directory),
        )
        .unwrap();
        PortalPickerSession::new(1, picker).unwrap()
    }

    #[derive(Default)]
    struct RecordingTarget {
        width: u32,
        height: u32,
        selected_fills: usize,
        saw_title: bool,
        saw_second: bool,
        saw_third: bool,
    }

    impl PortalPickerRenderTarget for RecordingTarget {
        type Error = ();

        fn dimensions(&self) -> (u32, u32) {
            (self.width, self.height)
        }

        fn fill_rect(&mut self, _rect: PortalRect, color: PortalColor) -> Result<(), Self::Error> {
            if color == PortalColor::SELECTED {
                self.selected_fills += 1;
            }
            Ok(())
        }

        fn stroke_rect(
            &mut self,
            _rect: PortalRect,
            _color: PortalColor,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn draw_text(
            &mut self,
            _x: u32,
            _baseline_y: u32,
            text: &[u8],
            _color: PortalColor,
        ) -> Result<(), Self::Error> {
            self.saw_title |= text == b"Trusted file access";
            self.saw_second |= text == b"second";
            self.saw_third |= text == b"third";
            Ok(())
        }
    }

    #[test]
    fn file_activation_is_terminal() {
        let root = resource(1, ApplicationResourceKind::Directory);
        let file =
            ApplicationPickerEntry::new(b"notes.txt", resource(2, ApplicationResourceKind::File))
                .unwrap();
        let mut session = session(ApplicationPortalOperation::OpenFile);
        session
            .accept_authenticated_page(root, 0, &[file], 0, true)
            .unwrap();

        assert_eq!(
            session.handle_event(PortalPickerEvent::Activate),
            Ok(PortalPickerOutcome::Selected {
                resource: file.resource(),
                entry_index: Some(0),
            })
        );
        assert_eq!(
            session.handle_event(PortalPickerEvent::MoveDown),
            Err(PortalPickerInteractionError::Completed)
        );
    }

    #[test]
    fn indexed_input_is_bounds_checked_before_selection() {
        let root = resource(1, ApplicationResourceKind::Directory);
        let files = [
            ApplicationPickerEntry::new(b"first", resource(2, ApplicationResourceKind::File))
                .unwrap(),
            ApplicationPickerEntry::new(b"second", resource(3, ApplicationResourceKind::File))
                .unwrap(),
        ];
        let mut session = session(ApplicationPortalOperation::OpenFile);
        session
            .accept_authenticated_page(root, 0, &files, 0, true)
            .unwrap();

        assert_eq!(
            session.handle_event(PortalPickerEvent::SelectIndex(1)),
            Ok(PortalPickerOutcome::Updated)
        );
        assert_eq!(session.selected_index(), 1);
        assert_eq!(
            session.handle_event(PortalPickerEvent::ActivateIndex(2)),
            Err(PortalPickerInteractionError::Picker(
                ApplicationPickerError::UnknownEntry
            ))
        );
        assert_eq!(session.state(), PortalPickerSessionState::Active);
    }

    #[test]
    fn directory_activation_requests_an_authenticated_page_and_back_resets_it() {
        let root = resource(1, ApplicationResourceKind::Directory);
        let child = ApplicationPickerEntry::new(
            b"Documents",
            resource(2, ApplicationResourceKind::Directory),
        )
        .unwrap();
        let mut session = session(ApplicationPortalOperation::SelectDirectory);
        session
            .accept_authenticated_page(root, 0, &[child], 0, true)
            .unwrap();

        assert_eq!(
            session.handle_event(PortalPickerEvent::Activate),
            Ok(PortalPickerOutcome::PageRequired {
                directory: child.resource(),
                cookie: 0,
            })
        );
        assert_eq!(
            session.handle_event(PortalPickerEvent::NavigateBack),
            Ok(PortalPickerOutcome::PageRequired {
                directory: root,
                cookie: 0,
            })
        );
        assert_eq!(session.selected_index(), 0);
        assert_eq!(session.first_visible(), 0);
    }

    #[test]
    fn directory_confirmation_selects_the_displayed_directory() {
        let root = resource(1, ApplicationResourceKind::Directory);
        let mut session = session(ApplicationPortalOperation::SelectDirectory);

        assert_eq!(
            session.handle_event(PortalPickerEvent::Confirm),
            Ok(PortalPickerOutcome::Selected {
                resource: root,
                entry_index: None,
            })
        );
    }

    #[test]
    fn renderer_scrolls_selection_into_a_bounded_viewport() {
        let root = resource(1, ApplicationResourceKind::Directory);
        let entries = [
            ApplicationPickerEntry::new(b"first", resource(2, ApplicationResourceKind::File))
                .unwrap(),
            ApplicationPickerEntry::new(b"second", resource(3, ApplicationResourceKind::File))
                .unwrap(),
            ApplicationPickerEntry::new(b"third", resource(4, ApplicationResourceKind::File))
                .unwrap(),
            ApplicationPickerEntry::new(b"fourth", resource(5, ApplicationResourceKind::File))
                .unwrap(),
        ];
        let mut session = session(ApplicationPortalOperation::OpenFile);
        session
            .accept_authenticated_page(root, 0, &entries, 0, true)
            .unwrap();
        let mut target = RecordingTarget {
            width: MIN_PICKER_WIDTH,
            height: MIN_PICKER_HEIGHT,
            ..RecordingTarget::default()
        };
        session.render(&mut target).unwrap();
        session.handle_event(PortalPickerEvent::PageDown).unwrap();
        target.saw_second = false;
        target.saw_third = false;
        target.selected_fills = 0;
        session.render(&mut target).unwrap();

        assert_eq!(session.selected_index(), 2);
        assert_eq!(session.first_visible(), 1);
        assert!(target.saw_title);
        assert!(target.saw_second);
        assert!(target.saw_third);
        assert_eq!(target.selected_fills, 1);
    }

    #[test]
    fn renderer_rejects_an_undersized_surface_before_drawing() {
        let mut session = session(ApplicationPortalOperation::OpenFile);
        let mut target = RecordingTarget {
            width: MIN_PICKER_WIDTH - 1,
            height: MIN_PICKER_HEIGHT,
            ..RecordingTarget::default()
        };

        assert_eq!(
            session.render(&mut target),
            Err(PortalPickerRenderError::SurfaceTooSmall {
                width: MIN_PICKER_WIDTH - 1,
                height: MIN_PICKER_HEIGHT,
            })
        );
        assert!(!target.saw_title);
    }
}
