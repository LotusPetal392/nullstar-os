//! Rooted live-filesystem forwarding for application resource grants.
//!
//! Provider node IDs and attached application memory never cross this adapter. Each grant gets one
//! bounded broker-local node namespace, and application buffers are copied through broker-owned
//! mirrors attached to the provider session.

use core::{array, mem::size_of, slice};

use crate::{
    application_permission::{
        ApplicationGrantAuthorization, ApplicationGrantRecord, ApplicationGrantRevocation,
        ApplicationGrantRights, ApplicationGrantState, ApplicationResourceIdentity,
        ApplicationResourceKind, ApplicationResourceRestoreError, ApplicationResourceRestorer,
    },
    application_resource::{
        ApplicationResourceAccess, ApplicationResourceAuthorizationError, ApplicationResourceBroker,
    },
    filesystem::{self, Node, Session, protocol},
    handle::{Endpoint, OwnedHandle, ReceivedCapability, SharedMemory},
    ipc::{self, Rights, Transfer},
};

pub const MAX_APPLICATION_RESOURCE_NODES: usize = 64;
pub const MAX_APPLICATION_RESOURCE_BUFFERS: usize = 4;
pub const MAX_APPLICATION_RESOURCE_BUFFER_BYTES: usize = 4096;
pub const MAX_ACTIVE_APPLICATION_RESOURCE_FORWARDERS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationResourceForwarderId(u64);

impl ApplicationResourceForwarderId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Authoritative change that makes one or more live resource endpoints stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationResourceLifecycleEvent {
    GrantRevoked {
        grant_id: u64,
        revocation_revision: u64,
    },
    ApplicationSessionEnded {
        application_session: u64,
    },
    ProviderReplaced {
        filesystem_uuid: [u8; 16],
        retired_generation: u64,
    },
    ResourceRemoved(ApplicationResourceIdentity),
}

impl ApplicationResourceLifecycleEvent {
    /// Converts the committed result of `ApplicationPermissionStore::revoke` into a lifecycle
    /// event. Consuming a one-shot grant does not close the endpoint that consumption created.
    pub const fn grant_revoked(record: ApplicationGrantRecord) -> Option<Self> {
        match record.state() {
            ApplicationGrantState::Revoked(reason)
                if !matches!(reason, ApplicationGrantRevocation::Consumed) =>
            {
                Some(Self::GrantRevoked {
                    grant_id: record.id(),
                    revocation_revision: record.revision(),
                })
            }
            _ => None,
        }
    }

    pub const fn application_session_ended(application_session: u64) -> Option<Self> {
        if application_session == 0 {
            None
        } else {
            Some(Self::ApplicationSessionEnded {
                application_session,
            })
        }
    }

    pub fn provider_replaced(filesystem_uuid: [u8; 16], retired_generation: u64) -> Option<Self> {
        if filesystem_uuid == [0; 16] || retired_generation == 0 {
            None
        } else {
            Some(Self::ProviderReplaced {
                filesystem_uuid,
                retired_generation,
            })
        }
    }

    pub const fn resource_removed(resource: ApplicationResourceIdentity) -> Self {
        Self::ResourceRemoved(resource)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationResourceForwarderRegistrationError {
    Full,
    IdExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationResourceRegistryForwardError {
    UnknownForwarder,
    Forward(ApplicationResourceForwardError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationResourceTeardownReport {
    brokers_closed: usize,
    provider_disconnect_failures: usize,
}

impl ApplicationResourceTeardownReport {
    pub const fn brokers_closed(self) -> usize {
        self.brokers_closed
    }

    pub const fn provider_disconnect_failures(self) -> usize {
        self.provider_disconnect_failures
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationResourceForwardError {
    TryAgain,
    BrokerTransport,
    ReplyTransport,
    InvalidProviderRoot,
    ProviderIdentity(ApplicationResourceRestoreError),
    ProviderReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationResourceForwardOutcome {
    Replied,
    Disconnected,
    DroppedMalformedMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApplicationResourceForwarderBinding {
    grant_id: u64,
    grant_revision: u64,
    application_session: u64,
    resource: ApplicationResourceIdentity,
    provider_generation: u64,
}

impl ApplicationResourceForwarderBinding {
    const fn new(grant: ApplicationGrantAuthorization, provider_generation: u64) -> Self {
        Self {
            grant_id: grant.grant_id(),
            grant_revision: grant.grant_revision(),
            application_session: grant.application_session(),
            resource: grant.resource(),
            provider_generation,
        }
    }

    fn invalidated_by(self, event: ApplicationResourceLifecycleEvent) -> bool {
        match event {
            ApplicationResourceLifecycleEvent::GrantRevoked {
                grant_id,
                revocation_revision,
            } => self.grant_id == grant_id && self.grant_revision < revocation_revision,
            ApplicationResourceLifecycleEvent::ApplicationSessionEnded {
                application_session,
            } => self.application_session == application_session,
            ApplicationResourceLifecycleEvent::ProviderReplaced {
                filesystem_uuid,
                retired_generation,
            } => {
                self.resource.filesystem_uuid() == filesystem_uuid
                    && self.provider_generation == retired_generation
            }
            ApplicationResourceLifecycleEvent::ResourceRemoved(resource) => {
                self.resource == resource
            }
        }
    }
}

#[derive(Debug)]
struct ActiveApplicationResourceForwarder {
    id: ApplicationResourceForwarderId,
    binding: ApplicationResourceForwarderBinding,
    forwarder: ApplicationResourceForwarder,
}

/// Bounded owner of all application resource brokers in one broker process.
///
/// Lifecycle changes remove matching entries before attempting provider cleanup, so application
/// sends observe peer closure even when the retired provider no longer replies.
#[derive(Debug)]
pub struct ApplicationResourceForwarderRegistry {
    entries:
        [Option<ActiveApplicationResourceForwarder>; MAX_ACTIVE_APPLICATION_RESOURCE_FORWARDERS],
    next_id: u64,
    next_teardown_request_id: u64,
}

impl ApplicationResourceForwarderRegistry {
    pub fn new() -> Self {
        Self {
            entries: array::from_fn(|_| None),
            next_id: 1,
            next_teardown_request_id: 1,
        }
    }

    pub fn register(
        &mut self,
        forwarder: ApplicationResourceForwarder,
    ) -> Result<ApplicationResourceForwarderId, ApplicationResourceForwarderRegistrationError> {
        let Some(slot) = self.entries.iter().position(Option::is_none) else {
            let _ = self.close_forwarder(forwarder);
            return Err(ApplicationResourceForwarderRegistrationError::Full);
        };
        let Some(next_id) = self.next_id.checked_add(1) else {
            let _ = self.close_forwarder(forwarder);
            return Err(ApplicationResourceForwarderRegistrationError::IdExhausted);
        };
        let id = ApplicationResourceForwarderId(self.next_id);
        let binding = forwarder.binding();
        self.entries[slot] = Some(ActiveApplicationResourceForwarder {
            id,
            binding,
            forwarder,
        });
        self.next_id = next_id;
        Ok(id)
    }

    pub fn forward_one(
        &mut self,
        id: ApplicationResourceForwarderId,
    ) -> Result<ApplicationResourceForwardOutcome, ApplicationResourceRegistryForwardError> {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.as_ref().is_some_and(|entry| entry.id == id))
        else {
            return Err(ApplicationResourceRegistryForwardError::UnknownForwarder);
        };
        let result = self.entries[index]
            .as_mut()
            .expect("matched forwarder exists")
            .forwarder
            .forward_one();
        match result {
            Ok(ApplicationResourceForwardOutcome::Disconnected) => {
                self.entries[index] = None;
                Ok(ApplicationResourceForwardOutcome::Disconnected)
            }
            Ok(outcome) => Ok(outcome),
            Err(ApplicationResourceForwardError::TryAgain) => {
                Err(ApplicationResourceRegistryForwardError::Forward(
                    ApplicationResourceForwardError::TryAgain,
                ))
            }
            Err(error) => {
                let _ = self.close_entry(index);
                Err(ApplicationResourceRegistryForwardError::Forward(error))
            }
        }
    }

    pub fn invalidate(
        &mut self,
        event: ApplicationResourceLifecycleEvent,
    ) -> ApplicationResourceTeardownReport {
        let mut report = ApplicationResourceTeardownReport {
            brokers_closed: 0,
            provider_disconnect_failures: 0,
        };
        for index in 0..self.entries.len() {
            if self.entries[index]
                .as_ref()
                .is_some_and(|entry| entry.binding.invalidated_by(event))
            {
                report.brokers_closed += 1;
                if self.close_entry(index).is_err() {
                    report.provider_disconnect_failures += 1;
                }
            }
        }
        report
    }

    pub fn shutdown(&mut self) -> ApplicationResourceTeardownReport {
        let mut report = ApplicationResourceTeardownReport {
            brokers_closed: 0,
            provider_disconnect_failures: 0,
        };
        for index in 0..self.entries.len() {
            if self.entries[index].is_some() {
                report.brokers_closed += 1;
                if self.close_entry(index).is_err() {
                    report.provider_disconnect_failures += 1;
                }
            }
        }
        report
    }

    pub fn contains(&self, id: ApplicationResourceForwarderId) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.as_ref().is_some_and(|entry| entry.id == id))
    }

    pub fn len(&self) -> usize {
        self.entries.iter().flatten().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn close_entry(&mut self, index: usize) -> Result<(), filesystem::Error> {
        let entry = self.entries[index].take().expect("active entry exists");
        self.close_forwarder(entry.forwarder)
    }

    fn close_forwarder(
        &mut self,
        forwarder: ApplicationResourceForwarder,
    ) -> Result<(), filesystem::Error> {
        let provider_session = forwarder.close_broker();
        let request_id = self.next_teardown_request_id;
        self.next_teardown_request_id = request_id.wrapping_add(1).max(1);
        provider_session.disconnect(request_id)
    }
}

impl Default for ApplicationResourceForwarderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
struct NodeBinding {
    broker_id: u64,
    provider: Node,
    kind: u16,
    open_reference: bool,
}

#[derive(Debug)]
struct BufferBinding {
    id: u64,
    length: usize,
    client: OwnedHandle<SharedMemory>,
    provider: OwnedHandle<SharedMemory>,
}

/// One live adapter for one immutable grant authorization and one dedicated provider session.
#[derive(Debug)]
pub struct ApplicationResourceForwarder {
    broker: ApplicationResourceBroker,
    provider_session: Session,
    reply_endpoint: Option<OwnedHandle<Endpoint>>,
    client_session_id: u64,
    client_generation: u64,
    client_features: u64,
    connected: bool,
    disconnected: bool,
    next_node_id: u64,
    nodes: [Option<NodeBinding>; MAX_APPLICATION_RESOURCE_NODES],
    buffers: [Option<BufferBinding>; MAX_APPLICATION_RESOURCE_BUFFERS],
}

impl ApplicationResourceForwarder {
    /// Restores the exact resource identity carried by the broker grant and uses that node as the
    /// forwarding root. The expected UUID must come from trusted mount selection.
    pub fn restore(
        broker: ApplicationResourceBroker,
        provider_session: Session,
        expected_filesystem_uuid: [u8; 16],
        request_id: u64,
    ) -> Result<Self, ApplicationResourceForwardError> {
        let resource = broker.authority().grant().resource();
        let restorer = ApplicationResourceRestorer::new(provider_session, expected_filesystem_uuid)
            .ok_or(ApplicationResourceForwardError::InvalidProviderRoot)?;
        let provider_root = restorer
            .restore(request_id, resource)
            .map_err(ApplicationResourceForwardError::ProviderIdentity)?;
        Self::new(broker, provider_session, provider_root)
    }

    pub fn new(
        broker: ApplicationResourceBroker,
        provider_session: Session,
        provider_root: Node,
    ) -> Result<Self, ApplicationResourceForwardError> {
        provider_root
            .id_for(provider_session)
            .map_err(|_| ApplicationResourceForwardError::InvalidProviderRoot)?;
        let grant = broker.authority().grant();
        if has_mutation_access(grant.rights()) && !provider_session.is_writable() {
            return Err(ApplicationResourceForwardError::ProviderReadOnly);
        }
        let root_kind = match grant.resource().kind() {
            ApplicationResourceKind::File => protocol::node_kind::FILE,
            ApplicationResourceKind::Directory => protocol::node_kind::DIRECTORY,
        };
        let attributes = provider_session
            .attributes(u64::MAX, provider_root)
            .map_err(|_| ApplicationResourceForwardError::InvalidProviderRoot)?;
        if attributes.node_id != provider_root.id() || attributes.kind != root_kind {
            return Err(ApplicationResourceForwardError::InvalidProviderRoot);
        }
        let mut nodes = [None; MAX_APPLICATION_RESOURCE_NODES];
        nodes[0] = Some(NodeBinding {
            broker_id: protocol::ROOT_NODE_ID,
            provider: provider_root,
            kind: root_kind,
            open_reference: false,
        });
        Ok(Self {
            broker,
            provider_session,
            reply_endpoint: None,
            client_session_id: grant.grant_id(),
            client_generation: grant.grant_revision(),
            client_features: 0,
            connected: false,
            disconnected: false,
            next_node_id: protocol::ROOT_NODE_ID + 1,
            nodes,
            buffers: array::from_fn(|_| None),
        })
    }

    pub const fn connected(&self) -> bool {
        self.connected
    }

    pub const fn disconnected(&self) -> bool {
        self.disconnected
    }

    fn binding(&self) -> ApplicationResourceForwarderBinding {
        ApplicationResourceForwarderBinding::new(
            self.broker.authority().grant(),
            self.provider_session.generation(),
        )
    }

    /// Drops application-facing authority and broker-owned memory before returning the provider
    /// session for best-effort disconnect.
    fn close_broker(self) -> Session {
        let Self {
            broker,
            provider_session,
            reply_endpoint,
            buffers,
            ..
        } = self;
        drop(broker);
        drop(reply_endpoint);
        drop(buffers);
        provider_session
    }

    /// Receives, validates, forwards, rewrites, and replies to one queued application request.
    pub fn forward_one(
        &mut self,
    ) -> Result<ApplicationResourceForwardOutcome, ApplicationResourceForwardError> {
        let mut bytes = [0_u8; size_of::<protocol::Request>()];
        let message = self
            .broker
            .try_receive(&mut bytes)
            .map_err(|error| match error {
                ipc::Error::TRY_AGAIN => ApplicationResourceForwardError::TryAgain,
                _ => ApplicationResourceForwardError::BrokerTransport,
            })?;
        if message.bytes != bytes.len() {
            drop(message.capability);
            return Ok(ApplicationResourceForwardOutcome::DroppedMalformedMessage);
        }
        let request =
            unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const protocol::Request) };
        if request.operation == protocol::operation::CONNECT {
            return self.connect(request, message.capability);
        }
        let capability = message.capability;
        let mut dispatched = false;
        let mut reply = self.base_reply(&request);
        if !self.connected
            || self.disconnected
            || request.session_id != self.client_session_id
            || request.generation != self.client_generation
        {
            reply.status = protocol::status::STALE_SESSION;
        } else if !canonical_request(&request)
            || request.operation == protocol::operation::ATTACH_BUFFER && capability.is_none()
            || request.operation != protocol::operation::ATTACH_BUFFER && capability.is_some()
        {
            drop(capability);
            reply.status = protocol::status::INVALID;
        } else {
            dispatched = true;
            reply = self.dispatch(&request, capability);
        }
        self.send_reply(&reply)?;
        if request.operation == protocol::operation::DISCONNECT && dispatched {
            self.disconnected = true;
            self.connected = false;
            self.reply_endpoint = None;
            self.buffers = array::from_fn(|_| None);
            return Ok(ApplicationResourceForwardOutcome::Disconnected);
        }
        Ok(ApplicationResourceForwardOutcome::Replied)
    }

    fn connect(
        &mut self,
        request: protocol::Request,
        capability: Option<ReceivedCapability>,
    ) -> Result<ApplicationResourceForwardOutcome, ApplicationResourceForwardError> {
        let Some(capability) = capability else {
            return Ok(ApplicationResourceForwardOutcome::DroppedMalformedMessage);
        };
        if self.connected
            || self.disconnected
            || capability.rights != Rights::SEND
            || !canonical_request(&request)
            || self
                .broker
                .authorize_operation(request.operation, request.flags)
                .is_err()
        {
            drop(capability);
            return Ok(ApplicationResourceForwardOutcome::DroppedMalformedMessage);
        }
        let endpoint = match capability.handle.try_cast::<Endpoint>() {
            Ok(endpoint) => endpoint,
            Err(_) => return Ok(ApplicationResourceForwardOutcome::DroppedMalformedMessage),
        };
        self.client_features = if request.flags == protocol::connect_flags::WRITE {
            protocol::session_features::WRITE
        } else {
            0
        };
        let mut reply = self.base_reply(&request);
        reply.session_id = self.client_session_id;
        reply.generation = self.client_generation;
        reply.node_id = protocol::ROOT_NODE_ID;
        reply.node_kind = protocol::node_kind::DIRECTORY;
        reply.value = self.client_features;
        endpoint
            .send(bytes_of(&reply))
            .map_err(|_| ApplicationResourceForwardError::ReplyTransport)?;
        self.reply_endpoint = Some(endpoint);
        self.connected = true;
        Ok(ApplicationResourceForwardOutcome::Replied)
    }

    fn dispatch(
        &mut self,
        request: &protocol::Request,
        capability: Option<ReceivedCapability>,
    ) -> protocol::Reply {
        let access = match self
            .broker
            .authorize_operation(request.operation, request.flags)
        {
            Ok(access) => access,
            Err(error) => {
                drop(capability);
                return self.authorization_reply(request, error);
            }
        };
        if requires_writable_session(request)
            && self.client_features & protocol::session_features::WRITE == 0
        {
            drop(capability);
            return self.status_reply(request, protocol::status::PERMISSION);
        }
        match request.operation {
            protocol::operation::ATTACH_BUFFER => {
                self.attach_buffer(request, capability.expect("attachment shape was validated"))
            }
            protocol::operation::DETACH_BUFFER => self.detach_buffer(request),
            protocol::operation::CANCEL => {
                self.status_reply(request, protocol::status::NOT_SUPPORTED)
            }
            protocol::operation::RESOLVE_IDENTITY | protocol::operation::RESTORE_IDENTITY => {
                self.status_reply(request, protocol::status::NOT_SUPPORTED)
            }
            _ => self.forward_provider(request, access),
        }
    }

    fn attach_buffer(
        &mut self,
        request: &protocol::Request,
        capability: ReceivedCapability,
    ) -> protocol::Reply {
        if capability.rights != Rights::READ.union(Rights::WRITE) {
            return self.status_reply(request, protocol::status::INVALID);
        }
        let client = match capability.handle.try_cast::<SharedMemory>() {
            Ok(client) => client,
            Err(_) => return self.status_reply(request, protocol::status::INVALID),
        };
        let length = match usize::try_from(request.bulk.length) {
            Ok(length) => length,
            Err(_) => return self.status_reply(request, protocol::status::RANGE),
        };
        if !client.info().is_ok_and(|info| {
            info.rights == Rights::READ.union(Rights::WRITE) && info.size >= request.bulk.length
        }) {
            return self.status_reply(request, protocol::status::INVALID);
        }
        let slot = self
            .buffer_index(request.bulk.buffer_id)
            .or_else(|| self.buffers.iter().position(Option::is_none));
        let Some(slot) = slot else {
            return self.status_reply(request, protocol::status::NO_SPACE);
        };
        let provider = match OwnedHandle::<SharedMemory>::create(length) {
            Ok(provider) => provider,
            Err(_) => return self.status_reply(request, protocol::status::NO_SPACE),
        };
        let provider_request = self.provider_request(request);
        let provider_reply = match self.provider_session.exchange_protocol(
            &provider_request,
            Some(Transfer {
                handle: provider.as_raw(),
                rights: Rights::READ.union(Rights::WRITE),
            }),
            false,
        ) {
            Ok(reply) => reply,
            Err(_) => self.provider_error_reply(&provider_request, protocol::status::IO),
        };
        if provider_reply.status == protocol::status::OK {
            self.buffers[slot] = Some(BufferBinding {
                id: request.bulk.buffer_id,
                length,
                client,
                provider,
            });
        }
        self.client_reply(request, provider_reply)
    }

    fn detach_buffer(&mut self, request: &protocol::Request) -> protocol::Reply {
        let Some(index) = self.buffer_index(request.bulk.buffer_id) else {
            return self.status_reply(request, protocol::status::STALE_BUFFER);
        };
        let provider_request = self.provider_request(request);
        let reply = self.provider_exchange(&provider_request, false);
        if reply.status == protocol::status::OK {
            self.buffers[index] = None;
        }
        self.client_reply(request, reply)
    }

    fn forward_provider(
        &mut self,
        request: &protocol::Request,
        access: ApplicationResourceAccess,
    ) -> protocol::Reply {
        let mutation = matches!(
            access,
            ApplicationResourceAccess::Write
                | ApplicationResourceAccess::Create
                | ApplicationResourceAccess::Remove
                | ApplicationResourceAccess::Rename
                | ApplicationResourceAccess::Synchronize
        ) || request.operation == protocol::operation::CLOSE_NODE
            || request.operation == protocol::operation::DISCONNECT
            || request.operation == protocol::operation::OPEN
                && request.flags & protocol::request_flags::TRUNCATE != 0;
        if request.operation == protocol::operation::DISCONNECT {
            let status = match self.provider_session.disconnect(request.request_id) {
                Ok(()) => protocol::status::OK,
                Err(filesystem::Error::OutcomeUnknown) => protocol::status::OUTCOME_UNKNOWN,
                Err(filesystem::Error::Service(status)) => status,
                Err(_) => protocol::status::IO,
            };
            return self.status_reply(request, status);
        }
        let mut provider_request = self.provider_request(request);
        let primary = if request.node_id == protocol::INVALID_ID {
            None
        } else {
            match self.node_binding(request.node_id) {
                Some(binding) => {
                    provider_request.node_id = binding.provider.id();
                    Some(binding)
                }
                None => return self.status_reply(request, protocol::status::STALE_NODE),
            }
        };
        if request.secondary_node_id != protocol::INVALID_ID {
            let Some(binding) = self.node_binding(request.secondary_node_id) else {
                return self.status_reply(request, protocol::status::STALE_NODE);
            };
            if binding.kind != protocol::node_kind::DIRECTORY {
                return self.status_reply(request, protocol::status::NOT_DIRECTORY);
            }
            provider_request.secondary_node_id = binding.provider.id();
        }
        if requires_directory(request.operation)
            && primary.is_some_and(|binding| binding.kind != protocol::node_kind::DIRECTORY)
        {
            return self.status_reply(request, protocol::status::NOT_DIRECTORY);
        }
        if request.operation == protocol::operation::CLOSE_NODE
            && !primary.is_some_and(|binding| binding.open_reference)
        {
            return self.status_reply(request, protocol::status::STALE_NODE);
        }
        if returns_node(request.operation) && self.available_node_slots() == 0 {
            return self.status_reply(request, protocol::status::NO_SPACE);
        }
        if matches!(
            request.operation,
            protocol::operation::WRITE | protocol::operation::RENAME
        ) && let Err(status) = self.copy_client_to_provider(request.bulk)
        {
            return self.status_reply(request, status);
        }
        if request.operation == protocol::operation::RENAME
            && let Err(status) = self.validate_buffer_name(request.bulk)
        {
            return self.status_reply(request, status);
        }
        let provider_reply = self.provider_exchange(&provider_request, mutation);
        if provider_reply.status != protocol::status::OK {
            return self.client_reply(request, provider_reply);
        }
        if request.operation == protocol::operation::OPEN
            && primary.is_none_or(|binding| binding.kind != provider_reply.node_kind)
        {
            if let Some(provider) = Node::from_reply(self.provider_session, &provider_reply) {
                let _ = self
                    .provider_session
                    .close_node(request.request_id, provider);
            }
            return self.status_reply(request, protocol::status::IO);
        }
        match request.operation {
            protocol::operation::LOOKUP
            | protocol::operation::OPEN
            | protocol::operation::CREATE_FILE
            | protocol::operation::CREATE_DIRECTORY => self.node_reply(
                request,
                provider_reply,
                request.operation == protocol::operation::OPEN,
            ),
            protocol::operation::GET_ATTRIBUTES => self.attributes_reply(request, provider_reply),
            protocol::operation::READ => {
                if let Err(status) =
                    self.copy_provider_to_client(request.bulk, provider_reply.value)
                {
                    return self.status_reply(request, status);
                }
                self.client_reply(request, provider_reply)
            }
            protocol::operation::READ_DIRECTORY => self.directory_reply(request, provider_reply),
            protocol::operation::CLOSE_NODE => {
                self.remove_node(request.node_id);
                self.client_reply(request, provider_reply)
            }
            _ => self.client_reply(request, provider_reply),
        }
    }

    fn node_reply(
        &mut self,
        request: &protocol::Request,
        provider_reply: protocol::Reply,
        open_reference: bool,
    ) -> protocol::Reply {
        if provider_reply.node_kind == protocol::node_kind::SYMBOLIC_LINK {
            if open_reference
                && let Some(provider) = Node::from_reply(self.provider_session, &provider_reply)
            {
                let _ = self
                    .provider_session
                    .close_node(request.request_id, provider);
            }
            return self.status_reply(request, protocol::status::NOT_SUPPORTED);
        }
        let Some(provider) = Node::from_reply(self.provider_session, &provider_reply) else {
            return self.status_reply(request, provider_failure_status(request.operation));
        };
        let Some(broker_id) = self.insert_node(provider, provider_reply.node_kind, open_reference)
        else {
            if open_reference {
                let _ = self
                    .provider_session
                    .close_node(request.request_id, provider);
            }
            return self.status_reply(request, protocol::status::NO_SPACE);
        };
        let mut reply = self.client_reply(request, provider_reply);
        reply.node_id = broker_id;
        reply
    }

    fn attributes_reply(
        &self,
        request: &protocol::Request,
        provider_reply: protocol::Reply,
    ) -> protocol::Reply {
        let mut attributes = unsafe {
            core::ptr::read_unaligned(
                provider_reply.data.as_ptr() as *const protocol::NodeAttributes
            )
        };
        attributes.node_id = request.node_id;
        let mut reply = self.client_reply(request, provider_reply);
        reply.node_id = request.node_id;
        reply.data = [0; protocol::MAX_INLINE_DATA_BYTES];
        let bytes = bytes_of(&attributes);
        reply.data[..bytes.len()].copy_from_slice(bytes);
        reply
    }

    fn directory_reply(
        &mut self,
        request: &protocol::Request,
        provider_reply: protocol::Reply,
    ) -> protocol::Reply {
        let count = match usize::try_from(provider_reply.value) {
            Ok(count) => count,
            Err(_) => return self.status_reply(request, protocol::status::IO),
        };
        let byte_count = match count.checked_mul(size_of::<protocol::DirectoryEntry>()) {
            Some(bytes) if bytes <= MAX_APPLICATION_RESOURCE_BUFFER_BYTES => bytes,
            _ => return self.status_reply(request, protocol::status::IO),
        };
        let Some(index) = self.buffer_index(request.bulk.buffer_id) else {
            return self.status_reply(request, protocol::status::STALE_BUFFER);
        };
        let offset = match usize::try_from(request.bulk.offset) {
            Ok(offset) => offset,
            Err(_) => return self.status_reply(request, protocol::status::RANGE),
        };
        let mut bytes = [0_u8; MAX_APPLICATION_RESOURCE_BUFFER_BYTES];
        if self.buffers[index]
            .as_ref()
            .expect("buffer index remains occupied")
            .provider
            .read(offset, &mut bytes[..byte_count])
            .ok()
            != Some(byte_count)
        {
            return self.status_reply(request, protocol::status::IO);
        }
        if count > self.available_node_slots() {
            return self.status_reply(request, protocol::status::NO_SPACE);
        }
        for entry_bytes in
            bytes[..byte_count].chunks_exact_mut(size_of::<protocol::DirectoryEntry>())
        {
            let mut entry = unsafe {
                core::ptr::read_unaligned(entry_bytes.as_ptr() as *const protocol::DirectoryEntry)
            };
            if !canonical_directory_entry(&entry)
                || entry.kind == protocol::node_kind::SYMBOLIC_LINK
            {
                return self.status_reply(request, protocol::status::IO);
            }
            let Some(provider) = Node::from_id(self.provider_session, entry.node_id) else {
                return self.status_reply(request, protocol::status::IO);
            };
            let Some(broker_id) = self.insert_node(provider, entry.kind, false) else {
                return self.status_reply(request, protocol::status::NO_SPACE);
            };
            entry.node_id = broker_id;
            entry_bytes.copy_from_slice(bytes_of(&entry));
        }
        if self.buffers[index]
            .as_ref()
            .expect("buffer index remains occupied")
            .client
            .write(offset, &bytes[..byte_count])
            .ok()
            != Some(byte_count)
        {
            return self.status_reply(request, protocol::status::IO);
        }
        self.client_reply(request, provider_reply)
    }

    fn provider_request(&self, request: &protocol::Request) -> protocol::Request {
        let mut provider = *request;
        provider.session_id = self.provider_session.id();
        provider.generation = self.provider_session.generation();
        provider
    }

    fn provider_exchange(&self, request: &protocol::Request, mutation: bool) -> protocol::Reply {
        match self
            .provider_session
            .exchange_protocol(request, None, mutation)
        {
            Ok(reply) => reply,
            Err(filesystem::Error::OutcomeUnknown) => {
                self.provider_error_reply(request, protocol::status::OUTCOME_UNKNOWN)
            }
            Err(_) => self.provider_error_reply(request, protocol::status::IO),
        }
    }

    fn provider_error_reply(&self, request: &protocol::Request, status: i32) -> protocol::Reply {
        let mut reply = protocol::Reply::EMPTY;
        reply.operation = request.operation;
        reply.request_id = request.request_id;
        reply.session_id = request.session_id;
        reply.generation = request.generation;
        reply.status = status;
        reply
    }

    fn copy_client_to_provider(&self, bulk: protocol::BulkBuffer) -> Result<(), i32> {
        let (index, offset, length) = self.buffer_range(bulk)?;
        let binding = self.buffers[index]
            .as_ref()
            .expect("buffer index remains occupied");
        let mut bytes = [0_u8; MAX_APPLICATION_RESOURCE_BUFFER_BYTES];
        if binding.client.read(offset, &mut bytes[..length]).ok() != Some(length)
            || binding.provider.write(offset, &bytes[..length]).ok() != Some(length)
        {
            return Err(protocol::status::IO);
        }
        Ok(())
    }

    fn copy_provider_to_client(&self, bulk: protocol::BulkBuffer, count: u64) -> Result<(), i32> {
        if count > bulk.length {
            return Err(protocol::status::IO);
        }
        let narrowed = protocol::BulkBuffer {
            length: count,
            ..bulk
        };
        let (index, offset, length) = self.buffer_range(narrowed)?;
        let binding = self.buffers[index]
            .as_ref()
            .expect("buffer index remains occupied");
        let mut bytes = [0_u8; MAX_APPLICATION_RESOURCE_BUFFER_BYTES];
        if binding.provider.read(offset, &mut bytes[..length]).ok() != Some(length)
            || binding.client.write(offset, &bytes[..length]).ok() != Some(length)
        {
            return Err(protocol::status::IO);
        }
        Ok(())
    }

    fn validate_buffer_name(&self, bulk: protocol::BulkBuffer) -> Result<(), i32> {
        let (index, offset, length) = self.buffer_range(bulk)?;
        let mut name = [0_u8; protocol::MAX_NAME_BYTES];
        let binding = self.buffers[index]
            .as_ref()
            .expect("buffer index remains occupied");
        if binding.client.read(offset, &mut name[..length]).ok() != Some(length)
            || !valid_component(&name[..length])
        {
            return Err(protocol::status::INVALID);
        }
        Ok(())
    }

    fn buffer_range(&self, bulk: protocol::BulkBuffer) -> Result<(usize, usize, usize), i32> {
        let Some(index) = self.buffer_index(bulk.buffer_id) else {
            return Err(protocol::status::STALE_BUFFER);
        };
        let offset = usize::try_from(bulk.offset).map_err(|_| protocol::status::RANGE)?;
        let length = usize::try_from(bulk.length).map_err(|_| protocol::status::RANGE)?;
        let binding = self.buffers[index]
            .as_ref()
            .expect("buffer index remains occupied");
        if length > MAX_APPLICATION_RESOURCE_BUFFER_BYTES
            || offset
                .checked_add(length)
                .is_none_or(|end| end > binding.length)
        {
            return Err(protocol::status::RANGE);
        }
        Ok((index, offset, length))
    }

    fn buffer_index(&self, id: u64) -> Option<usize> {
        self.buffers
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|binding| binding.id == id))
    }

    fn node_binding(&self, broker_id: u64) -> Option<NodeBinding> {
        self.nodes
            .iter()
            .flatten()
            .copied()
            .find(|binding| binding.broker_id == broker_id)
    }

    fn available_node_slots(&self) -> usize {
        self.nodes.iter().filter(|slot| slot.is_none()).count()
    }

    fn insert_node(&mut self, provider: Node, kind: u16, open_reference: bool) -> Option<u64> {
        if !open_reference
            && let Some(binding) = self.nodes.iter().flatten().find(|binding| {
                !binding.open_reference && binding.provider == provider && binding.kind == kind
            })
        {
            return Some(binding.broker_id);
        }
        let slot = self.nodes.iter_mut().find(|slot| slot.is_none())?;
        let broker_id = self.next_node_id;
        self.next_node_id = self.next_node_id.checked_add(1)?;
        *slot = Some(NodeBinding {
            broker_id,
            provider,
            kind,
            open_reference,
        });
        Some(broker_id)
    }

    fn remove_node(&mut self, broker_id: u64) {
        if let Some(slot) = self.nodes.iter_mut().find(|slot| {
            slot.as_ref()
                .is_some_and(|binding| binding.broker_id == broker_id)
        }) {
            *slot = None;
        }
    }

    fn client_reply(
        &self,
        request: &protocol::Request,
        mut reply: protocol::Reply,
    ) -> protocol::Reply {
        reply.operation = request.operation;
        reply.request_id = request.request_id;
        reply.session_id = self.client_session_id;
        reply.generation = self.client_generation;
        reply
    }

    fn base_reply(&self, request: &protocol::Request) -> protocol::Reply {
        let mut reply = protocol::Reply::EMPTY;
        reply.operation = request.operation;
        reply.request_id = request.request_id;
        reply.session_id = request.session_id;
        reply.generation = request.generation;
        reply
    }

    fn status_reply(&self, request: &protocol::Request, status: i32) -> protocol::Reply {
        let mut reply = self.base_reply(request);
        reply.status = status;
        reply
    }

    fn authorization_reply(
        &self,
        request: &protocol::Request,
        error: ApplicationResourceAuthorizationError,
    ) -> protocol::Reply {
        let status = match error {
            ApplicationResourceAuthorizationError::InvalidFlags => protocol::status::INVALID,
            ApplicationResourceAuthorizationError::ResourceKindDenied => {
                protocol::status::NOT_DIRECTORY
            }
            ApplicationResourceAuthorizationError::RightsDenied => protocol::status::PERMISSION,
            ApplicationResourceAuthorizationError::UnsupportedOperation => {
                protocol::status::NOT_SUPPORTED
            }
        };
        self.status_reply(request, status)
    }

    fn send_reply(&self, reply: &protocol::Reply) -> Result<(), ApplicationResourceForwardError> {
        self.reply_endpoint
            .as_ref()
            .ok_or(ApplicationResourceForwardError::ReplyTransport)?
            .send(bytes_of(reply))
            .map_err(|_| ApplicationResourceForwardError::ReplyTransport)
    }
}

fn canonical_request(request: &protocol::Request) -> bool {
    if request.version != protocol::VERSION
        || request.request_id == protocol::INVALID_ID
        || request.reserved != [0; 3]
        || request.name_length as usize > protocol::MAX_NAME_BYTES
        || request.name[request.name_length as usize..]
            .iter()
            .any(|byte| *byte != 0)
    {
        return false;
    }
    let no_nodes = request.node_id == 0 && request.secondary_node_id == 0;
    let no_file_offset = request.file_offset == 0;
    let no_bulk = request.bulk == protocol::BulkBuffer::NONE;
    let no_name = request.name_length == 0;
    let one_node = request.node_id != 0 && request.secondary_node_id == 0;
    let valid_name = valid_component(&request.name[..request.name_length as usize]);
    let valid_bulk =
        request.bulk.buffer_id != 0 && request.bulk.length != 0 && request.bulk.end().is_some();
    match request.operation {
        protocol::operation::CONNECT => {
            request.session_id == 0
                && request.generation == 0
                && no_nodes
                && no_file_offset
                && no_bulk
                && no_name
        }
        protocol::operation::ATTACH_BUFFER => {
            no_nodes
                && no_file_offset
                && request.bulk.buffer_id != 0
                && request.bulk.offset == 0
                && request.bulk.length != 0
                && request.bulk.length <= MAX_APPLICATION_RESOURCE_BUFFER_BYTES as u64
                && no_name
        }
        protocol::operation::DETACH_BUFFER => {
            no_nodes
                && no_file_offset
                && request.bulk.buffer_id != 0
                && request.bulk.offset == 0
                && request.bulk.length == 0
                && no_name
        }
        protocol::operation::LOOKUP
        | protocol::operation::CREATE_FILE
        | protocol::operation::CREATE_DIRECTORY
        | protocol::operation::UNLINK
        | protocol::operation::RMDIR => one_node && no_file_offset && no_bulk && valid_name,
        protocol::operation::GET_ATTRIBUTES
        | protocol::operation::OPEN
        | protocol::operation::CLOSE_NODE => one_node && no_file_offset && no_bulk && no_name,
        protocol::operation::TRUNCATE => one_node && no_bulk && no_name,
        protocol::operation::READ | protocol::operation::WRITE => one_node && valid_bulk && no_name,
        protocol::operation::READ_DIRECTORY => one_node && valid_bulk && no_name,
        protocol::operation::RENAME => {
            request.node_id != 0
                && request.secondary_node_id != 0
                && no_file_offset
                && valid_name
                && valid_bulk
                && request.bulk.length <= protocol::MAX_NAME_BYTES as u64
        }
        protocol::operation::RESOLVE_IDENTITY => one_node && no_file_offset && no_bulk && no_name,
        protocol::operation::RESTORE_IDENTITY => {
            no_nodes && no_file_offset && no_bulk && canonical_inline_identity(request)
        }
        protocol::operation::CANCEL
        | protocol::operation::DISCONNECT
        | protocol::operation::SYNC => no_nodes && no_file_offset && no_bulk && no_name,
        _ => no_nodes && no_file_offset && no_bulk && no_name,
    }
}

fn canonical_inline_identity(request: &protocol::Request) -> bool {
    let identity_bytes = size_of::<protocol::StableNodeIdentity>();
    if usize::from(request.name_length) != identity_bytes
        || request.name[identity_bytes..].iter().any(|byte| *byte != 0)
    {
        return false;
    }
    let identity = unsafe {
        core::ptr::read_unaligned(request.name.as_ptr() as *const protocol::StableNodeIdentity)
    };
    identity.canonical()
}

fn canonical_directory_entry(entry: &protocol::DirectoryEntry) -> bool {
    entry.node_id != 0
        && entry.reserved == 0
        && usize::from(entry.name_length) <= protocol::MAX_NAME_BYTES
        && valid_component(&entry.name[..usize::from(entry.name_length)])
        && entry.name[usize::from(entry.name_length)..]
            .iter()
            .all(|byte| *byte == 0)
        && matches!(
            entry.kind,
            protocol::node_kind::FILE
                | protocol::node_kind::DIRECTORY
                | protocol::node_kind::SYMBOLIC_LINK
        )
}

fn valid_component(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.contains(&b'/') && !name.contains(&0)
}

fn requires_directory(operation: u16) -> bool {
    matches!(
        operation,
        protocol::operation::LOOKUP
            | protocol::operation::READ_DIRECTORY
            | protocol::operation::CREATE_FILE
            | protocol::operation::CREATE_DIRECTORY
            | protocol::operation::UNLINK
            | protocol::operation::RMDIR
            | protocol::operation::RENAME
    )
}

fn returns_node(operation: u16) -> bool {
    matches!(
        operation,
        protocol::operation::LOOKUP
            | protocol::operation::OPEN
            | protocol::operation::CREATE_FILE
            | protocol::operation::CREATE_DIRECTORY
    )
}

fn requires_writable_session(request: &protocol::Request) -> bool {
    matches!(
        request.operation,
        protocol::operation::WRITE
            | protocol::operation::CREATE_FILE
            | protocol::operation::CREATE_DIRECTORY
            | protocol::operation::UNLINK
            | protocol::operation::RENAME
            | protocol::operation::TRUNCATE
            | protocol::operation::RMDIR
            | protocol::operation::SYNC
    ) || request.operation == protocol::operation::OPEN
        && request.flags
            & (protocol::request_flags::WRITE
                | protocol::request_flags::APPEND
                | protocol::request_flags::TRUNCATE
                | protocol::request_flags::CREATE)
            != 0
}

fn provider_failure_status(operation: u16) -> i32 {
    if matches!(
        operation,
        protocol::operation::CREATE_FILE | protocol::operation::CREATE_DIRECTORY
    ) {
        protocol::status::OUTCOME_UNKNOWN
    } else {
        protocol::status::IO
    }
}

fn has_mutation_access(rights: ApplicationGrantRights) -> bool {
    rights.bits()
        & ApplicationGrantRights::WRITE
            .union(ApplicationGrantRights::CREATE)
            .union(ApplicationGrantRights::REMOVE)
            .bits()
        != 0
}

fn bytes_of<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(filesystem_uuid: [u8; 16], object_id: u64) -> ApplicationResourceIdentity {
        ApplicationResourceIdentity::new(
            filesystem_uuid,
            object_id,
            7,
            ApplicationResourceKind::File,
        )
        .unwrap()
    }

    fn request(operation: u16) -> protocol::Request {
        let mut request = protocol::Request::EMPTY;
        request.operation = operation;
        request.request_id = 1;
        request.session_id = 2;
        request.generation = 3;
        request
    }

    #[test]
    fn canonical_connect_has_no_provider_scoped_fields() {
        let mut connect = request(protocol::operation::CONNECT);
        connect.session_id = 0;
        connect.generation = 0;
        assert!(canonical_request(&connect));
        connect.node_id = 9;
        assert!(!canonical_request(&connect));
    }

    #[test]
    fn rooted_names_reject_path_navigation() {
        for name in [b".".as_slice(), b"..", b"a/b", b"a\0b", b""] {
            assert!(!valid_component(name));
        }
        assert!(valid_component(b"child"));
    }

    #[test]
    fn canonical_lookup_rejects_hidden_name_tail() {
        let mut lookup = request(protocol::operation::LOOKUP);
        lookup.node_id = 1;
        lookup.name_length = 5;
        lookup.name[..5].copy_from_slice(b"child");
        assert!(canonical_request(&lookup));
        lookup.name[5] = 1;
        assert!(!canonical_request(&lookup));
    }

    #[test]
    fn canonical_identity_restoration_requires_one_complete_stable_tuple() {
        let identity =
            protocol::StableNodeIdentity::new([0x71; 16], 3, 5, protocol::node_kind::FILE).unwrap();
        let mut restore = request(protocol::operation::RESTORE_IDENTITY);
        restore.name_length = size_of::<protocol::StableNodeIdentity>() as u16;
        restore.name[..size_of::<protocol::StableNodeIdentity>()]
            .copy_from_slice(bytes_of(&identity));
        assert!(canonical_request(&restore));
        restore.name[16..24].fill(0);
        assert!(!canonical_request(&restore));
    }

    #[test]
    fn canonical_bulk_is_bounded_and_nonzero() {
        let mut read = request(protocol::operation::READ);
        read.node_id = 1;
        read.bulk = protocol::BulkBuffer {
            buffer_id: 4,
            offset: 0,
            length: MAX_APPLICATION_RESOURCE_BUFFER_BYTES as u64,
        };
        assert!(canonical_request(&read));
        read.bulk.length = 0;
        assert!(!canonical_request(&read));
    }

    #[test]
    fn directory_entries_are_canonical_components() {
        let mut entry = protocol::DirectoryEntry::EMPTY;
        entry.node_id = 4;
        entry.kind = protocol::node_kind::FILE;
        entry.name_length = 4;
        entry.name[..4].copy_from_slice(b"file");
        assert!(canonical_directory_entry(&entry));
        entry.name[4] = 1;
        assert!(!canonical_directory_entry(&entry));
    }

    #[test]
    fn mutation_operations_require_writable_session_negotiation() {
        let mut write = request(protocol::operation::WRITE);
        write.node_id = 1;
        assert!(requires_writable_session(&write));

        let mut open = request(protocol::operation::OPEN);
        open.node_id = 1;
        open.flags = protocol::request_flags::READ;
        assert!(!requires_writable_session(&open));
        open.flags |= protocol::request_flags::WRITE;
        assert!(requires_writable_session(&open));

        let mut attributes = request(protocol::operation::GET_ATTRIBUTES);
        attributes.node_id = 1;
        assert!(!requires_writable_session(&attributes));
    }

    #[test]
    fn lifecycle_events_match_only_the_retired_live_authority() {
        let filesystem_uuid = [0x71; 16];
        let selected = resource(filesystem_uuid, 9);
        let binding = ApplicationResourceForwarderBinding {
            grant_id: 11,
            grant_revision: 13,
            application_session: 17,
            resource: selected,
            provider_generation: 19,
        };

        assert!(
            binding.invalidated_by(ApplicationResourceLifecycleEvent::GrantRevoked {
                grant_id: 11,
                revocation_revision: 14,
            })
        );
        assert!(
            !binding.invalidated_by(ApplicationResourceLifecycleEvent::GrantRevoked {
                grant_id: 11,
                revocation_revision: 13,
            })
        );
        assert!(binding.invalidated_by(
            ApplicationResourceLifecycleEvent::application_session_ended(17).unwrap()
        ));
        assert!(!binding.invalidated_by(
            ApplicationResourceLifecycleEvent::application_session_ended(18).unwrap()
        ));
        assert!(binding.invalidated_by(
            ApplicationResourceLifecycleEvent::provider_replaced(filesystem_uuid, 19).unwrap()
        ));
        assert!(!binding.invalidated_by(
            ApplicationResourceLifecycleEvent::provider_replaced(filesystem_uuid, 20).unwrap()
        ));
        assert!(
            binding.invalidated_by(ApplicationResourceLifecycleEvent::resource_removed(
                selected
            ))
        );
        assert!(
            !binding.invalidated_by(ApplicationResourceLifecycleEvent::resource_removed(
                resource(filesystem_uuid, 10)
            ))
        );
    }

    #[test]
    fn lifecycle_events_reject_ambiguous_zero_identifiers() {
        assert_eq!(
            ApplicationResourceLifecycleEvent::application_session_ended(0),
            None
        );
        assert_eq!(
            ApplicationResourceLifecycleEvent::provider_replaced([0; 16], 1),
            None
        );
        assert_eq!(
            ApplicationResourceLifecycleEvent::provider_replaced([1; 16], 0),
            None
        );
    }
}
