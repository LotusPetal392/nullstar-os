//! Live portal request, picker, and resource-broker ownership.
//!
//! The manager and compositor use separate channels. The loop authenticates kernel sender IDs
//! before decoding policy commands or constructing input envelopes. Application requests can
//! never supply picker entries, input events, or manager attestations.

use crate::{
    abi::limits,
    application_manager::{PortalManagerAdmission, PortalManagerCommand, get, put},
    application_permission::{ApplicationPermissionStore, ApplicationResourceIdentity},
    application_permission_persistence::{
        ApplicationPermissionCommit, ApplicationPermissionPersistence,
    },
    application_portal::ApplicationPortalStatus,
    application_portal_compositor::{ApplicationPortalCompositor, MAX_PORTAL_PICKER_SESSIONS},
    application_portal_filesystem::ApplicationPortalFilesystemBrowser,
    application_portal_gui::PortalPickerOutcome,
    application_portal_startup::ApplicationPortalStartup,
    application_portal_surface::{
        PortalButtonState, PortalInputEnvelope, PortalKey, PortalPickerInputRouter, PortalRawInput,
    },
    application_portal_transport::{ApplicationPortalTransport, PendingApplicationPortalRequest},
    application_resource_forwarding::{
        ApplicationResourceForwarder, ApplicationResourceForwarderId,
        ApplicationResourceForwarderRegistry, MAX_ACTIVE_APPLICATION_RESOURCE_FORWARDERS,
    },
    application_selection::PreparedApplicationSelection,
    filesystem,
    handle::{Endpoint, OwnedHandle},
    ipc::{self, Signals},
    platform,
};
use core::array;

pub const INPUT_BYTES: usize = 40;
pub const SNAPSHOT_BYTES: usize = 160;

/// Canonical keyboard-only first integration of the trusted compositor input protocol.
pub fn encode_input(
    session: u64,
    surface: u64,
    sequence: u64,
    key: PortalKey,
) -> [u8; INPUT_BYTES] {
    let mut bytes = [0; INPUT_BYTES];
    bytes[..4].copy_from_slice(b"NSPI");
    bytes[4] = 1;
    bytes[5] = match key {
        PortalKey::Up => 1,
        PortalKey::Down => 2,
        PortalKey::PageUp => 3,
        PortalKey::PageDown => 4,
        PortalKey::Enter => 5,
        PortalKey::Confirm => 6,
        PortalKey::Back => 7,
        PortalKey::Escape => 8,
        PortalKey::LoadMore => 9,
    };
    put(&mut bytes, 8, session);
    put(&mut bytes, 16, surface);
    put(&mut bytes, 24, sequence);
    bytes
}

fn decode_input(sender: u64, bytes: &[u8]) -> Option<PortalInputEnvelope> {
    if bytes.len() != INPUT_BYTES
        || bytes[..4] != *b"NSPI"
        || bytes[4] != 1
        || bytes[6..8] != [0; 2]
        || bytes[32..] != [0; 8]
    {
        return None;
    }
    let key = match bytes[5] {
        1 => PortalKey::Up,
        2 => PortalKey::Down,
        3 => PortalKey::PageUp,
        4 => PortalKey::PageDown,
        5 => PortalKey::Enter,
        6 => PortalKey::Confirm,
        7 => PortalKey::Back,
        8 => PortalKey::Escape,
        9 => PortalKey::LoadMore,
        _ => return None,
    };
    PortalInputEnvelope::new(
        sender,
        get(bytes, 8),
        get(bytes, 16),
        get(bytes, 24),
        PortalRawInput::Key {
            key,
            state: PortalButtonState::Pressed,
        },
    )
}

struct PendingPicker {
    id: u64,
    request: PendingApplicationPortalRequest,
    deadline_ns: u64,
}

#[derive(Clone, Copy)]
struct LiveBroker {
    owner: u64,
    id: ApplicationResourceForwarderId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalServiceError {
    ManagerLost,
    CompositorLost,
    ProviderLost,
    PersistenceAfterTransfer,
    ControlTransport,
    Clock,
}

pub struct ApplicationPortalService<'a, B> {
    startup: ApplicationPortalStartup,
    manager: OwnedHandle<Endpoint>,
    compositor: OwnedHandle<Endpoint>,
    filesystem: OwnedHandle<Endpoint>,
    transport: ApplicationPortalTransport,
    control: PortalManagerAdmission,
    picker_ui: &'a mut ApplicationPortalCompositor,
    input: PortalPickerInputRouter,
    browser: ApplicationPortalFilesystemBrowser,
    root: ApplicationResourceIdentity,
    pending: [Option<PendingPicker>; MAX_PORTAL_PICKER_SESSIONS],
    brokers: ApplicationResourceForwarderRegistry,
    owners: [Option<LiveBroker>; MAX_ACTIVE_APPLICATION_RESOURCE_FORWARDERS],
    store: ApplicationPermissionStore,
    persistence: B,
    commit: ApplicationPermissionCommit,
}

/// Already authenticated startup resources and recovered durable state.
pub struct PortalServiceResources<B> {
    pub manager: OwnedHandle<Endpoint>,
    pub compositor: OwnedHandle<Endpoint>,
    pub filesystem: OwnedHandle<Endpoint>,
    pub transport: ApplicationPortalTransport,
    pub browser: ApplicationPortalFilesystemBrowser,
    pub root: ApplicationResourceIdentity,
    pub store: ApplicationPermissionStore,
    pub persistence: B,
    pub commit: ApplicationPermissionCommit,
}

impl<'a, B: ApplicationPermissionPersistence> ApplicationPortalService<'a, B> {
    /// Normal shutdown releases provider attachments. Fatal errors instead require process exit.
    pub fn finish(mut self) -> B {
        self.cancel_all();
        self.brokers.shutdown();
        let _ = self.browser.detach();
        self.persistence
    }

    pub fn new(
        startup: ApplicationPortalStartup,
        resources: PortalServiceResources<B>,
        picker_ui: &'a mut ApplicationPortalCompositor,
    ) -> Self {
        Self {
            startup,
            manager: resources.manager,
            compositor: resources.compositor,
            filesystem: resources.filesystem,
            transport: resources.transport,
            control: PortalManagerAdmission::new(
                startup.application_manager_process_id(),
                startup.manager_generation(),
                startup.session_id(),
            )
            .expect("validated startup"),
            picker_ui,
            input: PortalPickerInputRouter::new(startup.trusted_compositor_process_id())
                .expect("validated startup"),
            browser: resources.browser,
            root: resources.root,
            pending: array::from_fn(|_| None),
            brokers: ApplicationResourceForwarderRegistry::new(),
            owners: [None; MAX_ACTIVE_APPLICATION_RESOURCE_FORWARDERS],
            store: resources.store,
            persistence: resources.persistence,
            commit: resources.commit,
        }
    }

    /// Services at most one message per ingress and one operation per live broker per turn.
    /// `false` is an authenticated, fully cleaned-up manager shutdown.
    ///
    /// `#[inline(never)]` is required: this function's steady-state frame holds its 256-byte ingress
    /// buffer and control admission state across all three ingress loops. Inlined into rust_main that
    /// allocation merges with rust_main's live loop-body frame, permanently consuming most of the 256 KiB
    /// userspace stack and leaving only a few dozen KiB for the nested poll -> handle_authenticated_input
    /// -> translate -> picker input routing chain on top, which faults with vector 14 inside
    /// PortalPickerSession::handle_event. As its own function rust_main polls from a small frame near the
    /// stack top, exactly as intended by isolating build_service and shutdown_service.
    #[inline(never)]
    pub fn poll(&mut self) -> Result<bool, PortalServiceError> {
        if self
            .manager
            .signal_state()
            .map_err(|_| PortalServiceError::ManagerLost)?
            .contains(Signals::PEER_CLOSED)
        {
            return Err(PortalServiceError::ManagerLost);
        }
        if self
            .compositor
            .signal_state()
            .map_err(|_| PortalServiceError::CompositorLost)?
            .contains(Signals::PEER_CLOSED)
        {
            return Err(PortalServiceError::CompositorLost);
        }
        let mut bytes = [0; limits::MAX_IPC_MESSAGE_BYTES];
        match self.manager.try_receive(&mut bytes) {
            Ok(message) if message.capability.is_none() => {
                if let Some((sequence, command)) = self
                    .control
                    .admit(message.sender_process_id, &bytes[..message.bytes])
                {
                    let (success, running) = match command {
                        PortalManagerCommand::Bind {
                            process_id,
                            authorization,
                        } => (
                            self.transport
                                .bind_client(message.sender_process_id, process_id, authorization)
                                .is_ok(),
                            true,
                        ),
                        PortalManagerCommand::Unbind { process_id } => {
                            let success = self
                                .transport
                                .unbind_client(message.sender_process_id, process_id)
                                .is_ok();
                            if success {
                                self.remove_client(process_id);
                            }
                            (success, true)
                        }
                        PortalManagerCommand::Shutdown => {
                            self.cancel_all();
                            self.brokers.shutdown();
                            (true, false)
                        }
                    };
                    let mut reply = [0; 24];
                    reply[..4].copy_from_slice(b"NSPA");
                    reply[4] = 1;
                    reply[5] = u8::from(success);
                    put(&mut reply, 8, self.startup.manager_generation());
                    put(&mut reply, 16, sequence);
                    self.manager
                        .send(&reply)
                        .map_err(|_| PortalServiceError::ControlTransport)?;
                    if !running {
                        return Ok(false);
                    }
                }
            }
            Ok(_) => {}
            Err(error) if error == ipc::Error::TRY_AGAIN => {}
            Err(_) => return Err(PortalServiceError::ManagerLost),
        }
        let now = platform::monotonic_time_ns().map_err(|_| PortalServiceError::Clock)?;
        // Tickets are ingested first; callers wait for the compositor's gesture acknowledgement.
        if let Ok(Some(ticket)) = self.transport.try_receive_gesture(now) {
            let mut ack = [0; 16];
            ack[..4].copy_from_slice(b"NSGA");
            put(&mut ack, 8, ticket.id());
            self.compositor
                .send(&ack)
                .map_err(|_| PortalServiceError::CompositorLost)?;
        }
        if let Ok(Some(request)) = self.transport.try_receive_request(now) {
            self.begin(request)?;
        }
        match self.compositor.try_receive(&mut bytes) {
            Ok(message)
                if message.capability.is_none()
                    && message.sender_process_id
                        == self.startup.trusted_compositor_process_id() =>
            {
                if let Some(envelope) =
                    decode_input(message.sender_process_id, &bytes[..message.bytes])
                {
                    let id = envelope.session_id();
                    if let Ok(Some(outcome)) = self
                        .picker_ui
                        .handle_authenticated_input(&mut self.input, envelope)
                    {
                        self.handle_outcome(id, outcome)?;
                    }
                }
            }
            Ok(_) => {}
            Err(error) if error == ipc::Error::TRY_AGAIN => {}
            Err(_) => return Err(PortalServiceError::CompositorLost),
        }
        for index in 0..self.pending.len() {
            // Reply authority is exact SEND and cannot inspect WAIT-only peer signals.
            // Manager unbind handles process exit; the deadline bounds abandoned live requests.
            if self.pending[index]
                .as_ref()
                .is_some_and(|p| now >= p.deadline_ns)
            {
                self.remove_pending(index, ApplicationPortalStatus::Cancelled);
            }
        }
        for slot in &mut self.owners {
            if let Some(owner) = *slot {
                let _ = self.brokers.forward_one(owner.id);
                if !self.brokers.contains(owner.id) {
                    *slot = None;
                }
            }
        }
        Ok(true)
    }

    fn begin(
        &mut self,
        request: PendingApplicationPortalRequest,
    ) -> Result<(), PortalServiceError> {
        let Some(slot) = self.pending.iter().position(Option::is_none) else {
            let _ = request.reply_terminal(ApplicationPortalStatus::Unavailable);
            return Ok(());
        };
        let Ok(id) = self
            .picker_ui
            .create_session(request.admission(), self.root)
        else {
            let _ = request.reply_terminal(ApplicationPortalStatus::Unavailable);
            return Ok(());
        };
        let deadline_ns = platform::monotonic_time_ns()
            .map_err(|_| PortalServiceError::Clock)?
            .saturating_add(300_000_000_000);
        self.pending[slot] = Some(PendingPicker {
            id,
            request,
            deadline_ns,
        });
        // The integration protocol allocates a distinct trusted surface identity per picker.
        self.picker_ui
            .bind_surface(&mut self.input, id, id, 640, 480)
            .map_err(|_| PortalServiceError::ControlTransport)?;
        self.load_page(id)?;
        self.snapshot(id)
    }

    fn load_page(&mut self, id: u64) -> Result<(), PortalServiceError> {
        let session = self
            .picker_ui
            .session_mut(id)
            .expect("owned pending picker");
        self.browser
            .load_next_page(session.picker_mut())
            .map_err(|_| PortalServiceError::ProviderLost)?;
        Ok(())
    }

    fn snapshot(&self, id: u64) -> Result<(), PortalServiceError> {
        let session = self.picker_ui.session(id).expect("owned picker");
        let picker = session.picker();
        let mut bytes = [0; SNAPSHOT_BYTES];
        bytes[..4].copy_from_slice(b"NSPU");
        bytes[4] = 1;
        bytes[5] = u8::from(picker.page_complete());
        put(&mut bytes, 8, id);
        put(&mut bytes, 16, picker.admission().client_process_id());
        put(&mut bytes, 24, picker.admission().request().request_id());
        put(&mut bytes, 32, session.selected_index() as u64);
        put(&mut bytes, 40, picker.entries().len() as u64);
        if let Some(entry) = picker.entries().nth(session.selected_index()) {
            bytes[6] = entry.name().len() as u8;
            bytes[7] = entry.resource().kind() as u8;
            bytes[64..64 + entry.name().len()].copy_from_slice(entry.name());
        }
        self.compositor
            .send(&bytes)
            .map_err(|_| PortalServiceError::CompositorLost)
    }

    fn handle_outcome(
        &mut self,
        id: u64,
        outcome: PortalPickerOutcome,
    ) -> Result<(), PortalServiceError> {
        match outcome {
            PortalPickerOutcome::Updated => self.snapshot(id),
            PortalPickerOutcome::PageRequired { .. } => {
                self.load_page(id)?;
                self.snapshot(id)
            }
            PortalPickerOutcome::Cancelled => {
                if let Some(index) = self
                    .pending
                    .iter()
                    .position(|p| p.as_ref().is_some_and(|p| p.id == id))
                {
                    self.remove_pending(index, ApplicationPortalStatus::Cancelled);
                }
                Ok(())
            }
            PortalPickerOutcome::Selected { resource, .. } => self.complete(id, resource),
        }
    }

    fn complete(
        &mut self,
        id: u64,
        resource: ApplicationResourceIdentity,
    ) -> Result<(), PortalServiceError> {
        let index = self
            .pending
            .iter()
            .position(|p| p.as_ref().is_some_and(|p| p.id == id))
            .expect("owned picker");
        let Some(broker_slot) = self.owners.iter().position(Option::is_none) else {
            self.remove_pending(index, ApplicationPortalStatus::Unavailable);
            return Ok(());
        };
        let pending = self.pending[index].take().expect("owned request");
        self.picker_ui.remove_session(id);
        self.input.unbind_session(id);
        let owner = pending.request.admission().client_process_id();
        let session = filesystem::connect_writable_service(self.filesystem.as_raw(), 1)
            .map_err(|_| PortalServiceError::ProviderLost)?;
        let admission = pending.request.admission();
        let existing = self.store.records().any(|record| {
            record.active()
                && record.subject() == admission.subject()
                && record.resource() == resource
        });
        let prepared = if existing {
            PreparedApplicationSelection::authorize(&mut self.store, admission, resource)
        } else {
            PreparedApplicationSelection::issue(&mut self.store, admission, resource)
        };
        let selection = match prepared {
            Ok(selection) => selection,
            Err(_) => {
                let _ = session.disconnect(2);
                let _ = pending
                    .request
                    .reply_terminal(ApplicationPortalStatus::Unavailable);
                return Ok(());
            }
        };
        let completed = match pending.request.complete_durable_selection(
            selection,
            &mut self.persistence,
            Some(self.commit),
        ) {
            Ok(completed) => completed,
            Err(error) => {
                let _ = session.disconnect(2);
                return if error.requires_fail_stop() {
                    Err(PortalServiceError::PersistenceAfterTransfer)
                } else {
                    Ok(())
                };
            }
        };
        self.commit = completed.commit();
        let forwarder = ApplicationResourceForwarder::restore(
            completed.into_broker(),
            session,
            self.root.filesystem_uuid(),
            2,
        )
        .map_err(|_| PortalServiceError::ProviderLost)?;
        let broker_id = self
            .brokers
            .register(forwarder)
            .map_err(|_| PortalServiceError::ControlTransport)?;
        self.owners[broker_slot] = Some(LiveBroker {
            owner,
            id: broker_id,
        });
        Ok(())
    }

    fn remove_pending(&mut self, index: usize, status: ApplicationPortalStatus) {
        if let Some(pending) = self.pending[index].take() {
            self.picker_ui.remove_session(pending.id);
            self.input.unbind_session(pending.id);
            let _ = pending.request.reply_terminal(status);
        }
    }

    fn remove_client(&mut self, process: u64) {
        for index in 0..self.pending.len() {
            if self.pending[index]
                .as_ref()
                .is_some_and(|p| p.request.admission().client_process_id() == process)
            {
                self.remove_pending(index, ApplicationPortalStatus::Cancelled);
            }
        }
        for slot in &mut self.owners {
            if slot.is_some_and(|owner| owner.owner == process)
                && let Some(owner) = slot.take()
            {
                self.brokers.remove(owner.id);
            }
        }
    }

    fn cancel_all(&mut self) {
        for index in 0..self.pending.len() {
            self.remove_pending(index, ApplicationPortalStatus::Cancelled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_wire_is_canonical_and_replay_checked_by_router() {
        let mut router = PortalPickerInputRouter::new(42).unwrap();
        router.bind(1, 2, 640, 480).unwrap();
        let bytes = encode_input(1, 2, 1, PortalKey::Down);
        assert!(
            router
                .translate(decode_input(41, &bytes).unwrap(), 0, 3)
                .is_err()
        );
        assert!(
            router
                .translate(decode_input(42, &bytes).unwrap(), 0, 3)
                .is_ok()
        );
        assert!(
            router
                .translate(decode_input(42, &bytes).unwrap(), 0, 3)
                .is_err()
        );
        let mut malformed = bytes;
        malformed[39] = 1;
        assert!(decode_input(42, &malformed).is_none());
        assert!(decode_input(42, &bytes[..39]).is_none());
    }
}
