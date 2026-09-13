//! Bounded session registry for compositor-hosted application portal pickers.
//!
//! This is intentionally independent of the portal process loop. The later process-integration
//! increment will bind each session ID to its owned pending reply capability; this module already
//! keeps picker state, navigation, terminal UI state, and rendering inside trusted code.

use core::array;

use crate::{
    application_permission::ApplicationResourceIdentity,
    application_portal::AdmittedPortalRequest,
    application_portal_gui::{
        PortalPickerEvent, PortalPickerInteractionError, PortalPickerOutcome,
        PortalPickerRenderError, PortalPickerRenderTarget, PortalPickerSession,
    },
    application_portal_picker::{ApplicationPickerError, ApplicationPortalPicker},
    application_portal_surface::{
        PortalInputBindingError, PortalInputEnvelope, PortalInputError, PortalPickerInputRouter,
    },
};

pub const MAX_PORTAL_PICKER_SESSIONS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalPickerSessionCreateError {
    Capacity,
    IdentifierExhausted,
    Picker(ApplicationPickerError),
    Session(PortalPickerInteractionError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalPickerSessionError {
    UnknownSession,
    Interaction(PortalPickerInteractionError),
}

pub struct ApplicationPortalCompositor {
    sessions: [Option<PortalPickerSession>; MAX_PORTAL_PICKER_SESSIONS],
    next_session_id: u64,
}

impl ApplicationPortalCompositor {
    pub fn new() -> Self {
        Self {
            sessions: array::from_fn(|_| None),
            next_session_id: 1,
        }
    }

    pub fn create_session(
        &mut self,
        admission: AdmittedPortalRequest,
        root: ApplicationResourceIdentity,
    ) -> Result<u64, PortalPickerSessionCreateError> {
        let Some(slot_index) = self.sessions.iter().position(Option::is_none) else {
            return Err(PortalPickerSessionCreateError::Capacity);
        };
        let id = self.allocate_session_id()?;
        let picker = ApplicationPortalPicker::new(admission, root)
            .map_err(PortalPickerSessionCreateError::Picker)?;
        let session = PortalPickerSession::new(id, picker)
            .map_err(PortalPickerSessionCreateError::Session)?;
        self.sessions[slot_index] = Some(session);
        Ok(id)
    }

    pub fn session(&self, id: u64) -> Option<&PortalPickerSession> {
        self.sessions
            .iter()
            .flatten()
            .find(|session| session.id() == id)
    }

    pub fn session_mut(&mut self, id: u64) -> Option<&mut PortalPickerSession> {
        self.sessions
            .iter_mut()
            .flatten()
            .find(|session| session.id() == id)
    }

    pub fn handle_event(
        &mut self,
        id: u64,
        event: PortalPickerEvent,
    ) -> Result<PortalPickerOutcome, PortalPickerSessionError> {
        self.session_mut(id)
            .ok_or(PortalPickerSessionError::UnknownSession)?
            .handle_event(event)
            .map_err(PortalPickerSessionError::Interaction)
    }

    pub fn bind_surface(
        &self,
        router: &mut PortalPickerInputRouter,
        session_id: u64,
        surface_id: u64,
        width: u32,
        height: u32,
    ) -> Result<(), PortalPickerSurfaceBindingError> {
        if self.session(session_id).is_none() {
            return Err(PortalPickerSurfaceBindingError::UnknownSession);
        }
        router
            .bind(session_id, surface_id, width, height)
            .map_err(PortalPickerSurfaceBindingError::Binding)
    }

    pub fn handle_authenticated_input(
        &mut self,
        router: &mut PortalPickerInputRouter,
        envelope: PortalInputEnvelope,
    ) -> Result<Option<PortalPickerOutcome>, PortalPickerCompositorInputError> {
        let session_id = envelope.session_id();
        let session = self
            .session(session_id)
            .ok_or(PortalPickerCompositorInputError::UnknownSession)?;
        let event = router
            .translate(
                envelope,
                session.first_visible(),
                session.picker().entries().len(),
            )
            .map_err(PortalPickerCompositorInputError::Input)?;
        let Some(event) = event else {
            return Ok(None);
        };
        self.handle_event(session_id, event)
            .map(Some)
            .map_err(PortalPickerCompositorInputError::Session)
    }

    pub fn render<T: PortalPickerRenderTarget>(
        &mut self,
        id: u64,
        target: &mut T,
    ) -> Result<(), PortalPickerCompositorRenderError<T::Error>> {
        self.session_mut(id)
            .ok_or(PortalPickerCompositorRenderError::UnknownSession)?
            .render(target)
            .map_err(PortalPickerCompositorRenderError::Render)
    }

    pub fn remove_session(&mut self, id: u64) -> Option<PortalPickerSession> {
        let slot = self
            .sessions
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|session| session.id() == id))?;
        slot.take()
    }

    pub fn active_session_count(&self) -> usize {
        self.sessions.iter().flatten().count()
    }

    fn allocate_session_id(&mut self) -> Result<u64, PortalPickerSessionCreateError> {
        for _ in 0..=MAX_PORTAL_PICKER_SESSIONS {
            let candidate = self.next_session_id;
            self.next_session_id = self.next_session_id.wrapping_add(1);
            if self.next_session_id == 0 {
                self.next_session_id = 1;
            }
            if candidate != 0 && self.session(candidate).is_none() {
                return Ok(candidate);
            }
        }
        Err(PortalPickerSessionCreateError::IdentifierExhausted)
    }
}

impl Default for ApplicationPortalCompositor {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalPickerCompositorRenderError<E> {
    UnknownSession,
    Render(PortalPickerRenderError<E>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalPickerSurfaceBindingError {
    UnknownSession,
    Binding(PortalInputBindingError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalPickerCompositorInputError {
    UnknownSession,
    Input(PortalInputError),
    Session(PortalPickerSessionError),
}
