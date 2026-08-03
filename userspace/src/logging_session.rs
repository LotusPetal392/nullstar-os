//! Allocation-free logging session bootstrap and per-session NSWP transports.
//!
//! A logging service exposes one shared ingress endpoint per [`SessionRole`]. Possession of a
//! role's ingress capability is the authority to request that role; roles are deliberately absent
//! from the bootstrap wire format. Each accepted session uses the shared ingress for client-to-
//! server traffic and a fresh private endpoint for server-to-client traffic.

use nswp_runtime::{MAX_PACKET_BYTES, PacketBuf, TryRecvError, TrySendError, TryTransport};

use crate::ipc::{
    self, CapabilityHandle, CapabilityInfo, ObjectKind, ReceivedCapability, Rights, Transfer,
};

pub const CONTROL_RECORD_BYTES: usize = 16;
pub const CONTROL_MAGIC: [u8; 4] = *b"NSLS";
pub const CONTROL_WIRE_VERSION: u8 = 1;
pub const MAX_LOGGING_SESSIONS: usize = 4;
pub const MAX_LOGGING_SESSIONS_PER_ROLE: usize = 3;

const INVALID_HANDLE: CapabilityHandle = crate::abi::capability::INVALID_HANDLE;
const CONTROL_KIND_CONNECT: u8 = 1;
const CONTROL_KIND_CONNECT_RESPONSE: u8 = 2;
const CONTROL_KIND_DISCONNECT: u8 = 3;

/// The authority represented by a shared logging ingress endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRole {
    Producer,
    Observer,
}

/// Status returned by the service for a connect request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum ConnectStatus {
    Accepted = 0,
    Unavailable = 1,
    CapacityExhausted = 2,
    Rejected = 3,
}

impl ConnectStatus {
    const fn from_raw(raw: u16) -> Option<Self> {
        match raw {
            0 => Some(Self::Accepted),
            1 => Some(Self::Unavailable),
            2 => Some(Self::CapacityExhausted),
            3 => Some(Self::Rejected),
            _ => None,
        }
    }
}

pub const fn admission_rejection(
    total_sessions: usize,
    role_sessions: usize,
    duplicate_owner: bool,
) -> Option<ConnectStatus> {
    if duplicate_owner {
        Some(ConnectStatus::Rejected)
    } else if total_sessions >= MAX_LOGGING_SESSIONS
        || role_sessions >= MAX_LOGGING_SESSIONS_PER_ROLE
    {
        Some(ConnectStatus::CapacityExhausted)
    } else {
        None
    }
}

/// A bootstrap record. These records are always exactly [`CONTROL_RECORD_BYTES`] bytes and are
/// intentionally distinct from NSWP packets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlRecord {
    Connect,
    ConnectResponse {
        status: ConnectStatus,
        service_generation: u64,
    },
    Disconnect {
        service_generation: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlEncodeError {
    MissingServiceGeneration,
    UnexpectedServiceGeneration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlDecodeError {
    InvalidLength { bytes: usize },
    InvalidMagic,
    UnsupportedVersion { version: u8 },
    UnknownKind { kind: u8 },
    UnknownStatus { status: u16 },
    UnexpectedStatus { status: u16 },
    MissingServiceGeneration,
    UnexpectedServiceGeneration,
}

impl ControlRecord {
    pub fn encode(self) -> Result<[u8; CONTROL_RECORD_BYTES], ControlEncodeError> {
        let (kind, status, generation) = match self {
            Self::Connect => (CONTROL_KIND_CONNECT, ConnectStatus::Accepted as u16, 0),
            Self::ConnectResponse {
                status,
                service_generation,
            } => {
                if status == ConnectStatus::Accepted && service_generation == 0 {
                    return Err(ControlEncodeError::MissingServiceGeneration);
                }
                if status != ConnectStatus::Accepted && service_generation != 0 {
                    return Err(ControlEncodeError::UnexpectedServiceGeneration);
                }
                (
                    CONTROL_KIND_CONNECT_RESPONSE,
                    status as u16,
                    service_generation,
                )
            }
            Self::Disconnect { service_generation } => {
                if service_generation == 0 {
                    return Err(ControlEncodeError::MissingServiceGeneration);
                }
                (
                    CONTROL_KIND_DISCONNECT,
                    ConnectStatus::Accepted as u16,
                    service_generation,
                )
            }
        };

        let mut output = [0_u8; CONTROL_RECORD_BYTES];
        output[..4].copy_from_slice(&CONTROL_MAGIC);
        output[4] = CONTROL_WIRE_VERSION;
        output[5] = kind;
        output[6..8].copy_from_slice(&status.to_le_bytes());
        output[8..16].copy_from_slice(&generation.to_le_bytes());
        Ok(output)
    }

    pub fn decode(input: &[u8]) -> Result<Self, ControlDecodeError> {
        if input.len() != CONTROL_RECORD_BYTES {
            return Err(ControlDecodeError::InvalidLength { bytes: input.len() });
        }
        if input[..4] != CONTROL_MAGIC {
            return Err(ControlDecodeError::InvalidMagic);
        }
        if input[4] != CONTROL_WIRE_VERSION {
            return Err(ControlDecodeError::UnsupportedVersion { version: input[4] });
        }

        let status = u16::from_le_bytes([input[6], input[7]]);
        let generation = u64::from_le_bytes([
            input[8], input[9], input[10], input[11], input[12], input[13], input[14], input[15],
        ]);
        match input[5] {
            CONTROL_KIND_CONNECT => {
                require_zero_status(status)?;
                if generation != 0 {
                    return Err(ControlDecodeError::UnexpectedServiceGeneration);
                }
                Ok(Self::Connect)
            }
            CONTROL_KIND_CONNECT_RESPONSE => {
                let status = ConnectStatus::from_raw(status)
                    .ok_or(ControlDecodeError::UnknownStatus { status })?;
                if status == ConnectStatus::Accepted && generation == 0 {
                    return Err(ControlDecodeError::MissingServiceGeneration);
                }
                if status != ConnectStatus::Accepted && generation != 0 {
                    return Err(ControlDecodeError::UnexpectedServiceGeneration);
                }
                Ok(Self::ConnectResponse {
                    status,
                    service_generation: generation,
                })
            }
            CONTROL_KIND_DISCONNECT => {
                require_zero_status(status)?;
                if generation == 0 {
                    return Err(ControlDecodeError::MissingServiceGeneration);
                }
                Ok(Self::Disconnect {
                    service_generation: generation,
                })
            }
            kind => Err(ControlDecodeError::UnknownKind { kind }),
        }
    }
}

fn require_zero_status(status: u16) -> Result<(), ControlDecodeError> {
    if status == ConnectStatus::Accepted as u16 {
        Ok(())
    } else {
        Err(ControlDecodeError::UnexpectedStatus { status })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientConnectError {
    Ipc(ipc::Error),
    YieldFailed,
    InvalidIngress,
    InvalidPrivateEndpoint,
    Closed,
    InvalidServiceProcessId,
    UnexpectedCapability,
    UnexpectedControlRecord,
    MalformedControl(ControlDecodeError),
    Rejected(ConnectStatus),
}

/// A nonblocking client-side session bootstrap.
///
/// `new` takes ownership of `ingress_handle` only when it succeeds. The resulting bootstrap owns
/// all of its handles and closes them on failure or drop.
pub struct ClientBootstrap {
    ingress_handle: CapabilityHandle,
    reply_source_handle: CapabilityHandle,
    receive_handle: CapabilityHandle,
    connect_sent: bool,
    closed: bool,
}

impl ClientBootstrap {
    pub fn new(ingress_handle: CapabilityHandle) -> Result<Self, ClientConnectError> {
        let ingress_info = ipc::info(ingress_handle).map_err(ClientConnectError::Ipc)?;
        if !exact_endpoint(ingress_info, Rights::SEND) {
            return Err(ClientConnectError::InvalidIngress);
        }

        let reply_source_handle = ipc::endpoint_create().map_err(ClientConnectError::Ipc)?;
        let receive_handle = match ipc::duplicate(reply_source_handle, Rights::RECEIVE) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = ipc::close(reply_source_handle);
                return Err(ClientConnectError::Ipc(error));
            }
        };
        let receive_is_exact =
            ipc::info(receive_handle).is_ok_and(|info| exact_endpoint(info, Rights::RECEIVE));
        if !receive_is_exact {
            let _ = ipc::close(receive_handle);
            let _ = ipc::close(reply_source_handle);
            return Err(ClientConnectError::InvalidPrivateEndpoint);
        }

        Ok(Self {
            ingress_handle,
            reply_source_handle,
            receive_handle,
            connect_sent: false,
            closed: false,
        })
    }

    /// Advances the bootstrap without blocking. `Ok(None)` means the ingress or reply endpoint is
    /// not ready yet. Fatal errors close all handles owned by this bootstrap.
    pub fn try_connect(&mut self) -> Result<Option<ClientTransport>, ClientConnectError> {
        if self.closed {
            return Err(ClientConnectError::Closed);
        }
        if !self.connect_sent {
            let record = ControlRecord::Connect
                .encode()
                .expect("the canonical connect record is always encodable");
            match ipc::send(
                self.ingress_handle,
                &record,
                Some(Transfer {
                    handle: self.reply_source_handle,
                    rights: Rights::SEND,
                }),
            ) {
                Ok(()) => {
                    self.connect_sent = true;
                    let _ = ipc::close(self.reply_source_handle);
                    self.reply_source_handle = INVALID_HANDLE;
                }
                Err(error) if error == ipc::Error::TRY_AGAIN => return Ok(None),
                Err(error) => {
                    self.close_local();
                    return Err(ClientConnectError::Ipc(error));
                }
            }
        }

        let mut bytes = [0_u8; CONTROL_RECORD_BYTES];
        let message = match ipc::try_receive(self.receive_handle, &mut bytes) {
            Ok(message) => message,
            Err(error) if error == ipc::Error::TRY_AGAIN => return Ok(None),
            Err(error) => {
                self.close_local();
                return Err(ClientConnectError::Ipc(error));
            }
        };
        if let Some(capability) = message.capability {
            let _ = ipc::close(capability.handle);
            self.close_local();
            return Err(ClientConnectError::UnexpectedCapability);
        }
        if message.sender_process_id == 0 {
            self.close_local();
            return Err(ClientConnectError::InvalidServiceProcessId);
        }
        let response = match ControlRecord::decode(&bytes[..message.bytes]) {
            Ok(response) => response,
            Err(error) => {
                self.close_local();
                return Err(ClientConnectError::MalformedControl(error));
            }
        };
        let ControlRecord::ConnectResponse {
            status,
            service_generation,
        } = response
        else {
            self.close_local();
            return Err(ClientConnectError::UnexpectedControlRecord);
        };
        if status != ConnectStatus::Accepted {
            self.close_local();
            return Err(ClientConnectError::Rejected(status));
        }

        let transport = ClientTransport {
            ingress_handle: self.take_ingress(),
            receive_handle: self.take_receive(),
            service_process_id: message.sender_process_id,
            service_generation,
            disconnect_pending: true,
            closed: false,
        };
        self.closed = true;
        Ok(Some(transport))
    }

    /// Completes a bootstrap, yielding cooperatively while either endpoint is not ready.
    pub fn connect(mut self) -> Result<ClientTransport, ClientConnectError> {
        loop {
            if let Some(transport) = self.try_connect()? {
                return Ok(transport);
            }
            crate::syscall::yield_now().map_err(|_| ClientConnectError::YieldFailed)?;
        }
    }

    fn take_ingress(&mut self) -> CapabilityHandle {
        let handle = self.ingress_handle;
        self.ingress_handle = INVALID_HANDLE;
        handle
    }

    fn take_receive(&mut self) -> CapabilityHandle {
        let handle = self.receive_handle;
        self.receive_handle = INVALID_HANDLE;
        handle
    }

    fn close_local(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        close_if_valid(&mut self.reply_source_handle);
        close_if_valid(&mut self.receive_handle);
        close_if_valid(&mut self.ingress_handle);
    }
}

impl Drop for ClientBootstrap {
    fn drop(&mut self) {
        self.close_local();
    }
}

/// A connected client transport. It owns exact `SEND` authority to one shared role ingress and
/// exact `RECEIVE` authority to its private reply endpoint.
pub struct ClientTransport {
    ingress_handle: CapabilityHandle,
    receive_handle: CapabilityHandle,
    service_process_id: u64,
    service_generation: u64,
    disconnect_pending: bool,
    closed: bool,
}

impl ClientTransport {
    pub const fn service_process_id(&self) -> u64 {
        self.service_process_id
    }

    pub const fn service_generation(&self) -> u64 {
        self.service_generation
    }

    /// Best-effort disconnect followed by local capability closure.
    pub fn disconnect(&mut self) {
        self.close_local();
    }

    fn close_local(&mut self) {
        if self.closed {
            return;
        }
        if let Some(record) =
            take_disconnect_record(&mut self.disconnect_pending, self.service_generation)
        {
            let _ = ipc::send(self.ingress_handle, &record, None);
        }
        self.closed = true;
        close_if_valid(&mut self.receive_handle);
        close_if_valid(&mut self.ingress_handle);
    }

    fn fail_send(&mut self) -> Result<(), TrySendError> {
        self.close_local();
        Err(TrySendError::PeerClosed)
    }

    fn fail_receive(&mut self) -> Result<usize, TryRecvError> {
        self.close_local();
        Err(TryRecvError::PeerClosed)
    }
}

impl TryTransport for ClientTransport {
    fn try_send(&mut self, packet: &[u8]) -> Result<(), TrySendError> {
        if self.closed || packet.len() > MAX_PACKET_BYTES {
            return self.fail_send();
        }
        match ipc::send(self.ingress_handle, packet, None) {
            Ok(()) => Ok(()),
            Err(error) if error == ipc::Error::TRY_AGAIN => Err(TrySendError::Full),
            Err(_) => self.fail_send(),
        }
    }

    fn try_recv(&mut self, output: &mut [u8]) -> Result<usize, TryRecvError> {
        if self.closed || output.len() > crate::abi::limits::MAX_IPC_MESSAGE_BYTES {
            return self.fail_receive();
        }
        let message = match ipc::try_receive(self.receive_handle, output) {
            Ok(message) => message,
            Err(error) if error == ipc::Error::TRY_AGAIN => return Err(TryRecvError::Empty),
            Err(error) if error == ipc::Error::RANGE => {
                return Err(TryRecvError::MessageTooLarge {
                    bytes: output.len().saturating_add(1),
                });
            }
            Err(_) => return self.fail_receive(),
        };
        if let Some(capability) = message.capability {
            let _ = ipc::close(capability.handle);
            return self.fail_receive();
        }
        if !sender_is_pinned(self.service_process_id, message.sender_process_id) {
            return self.fail_receive();
        }
        Ok(message.bytes)
    }

    fn close(&mut self) {
        self.close_local();
    }
}

impl Drop for ClientTransport {
    fn drop(&mut self) {
        self.close_local();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerIngressError {
    Ipc(ipc::Error),
    InvalidIngress,
    InvalidOwnerProcessId,
    MalformedControl(ControlDecodeError),
    UnexpectedControlRecord,
    MissingReplyCapability,
    UnexpectedCapability,
    InvalidReplyCapability,
}

/// The receiving side of one role-specific shared ingress endpoint.
pub struct ServerIngress {
    role: SessionRole,
    receive_handle: CapabilityHandle,
    object_id: u64,
    closed: bool,
}

impl ServerIngress {
    /// Takes ownership of `receive_handle` only on success.
    pub fn new(
        role: SessionRole,
        receive_handle: CapabilityHandle,
    ) -> Result<Self, ServerIngressError> {
        let info = ipc::info(receive_handle).map_err(ServerIngressError::Ipc)?;
        if !exact_endpoint(info, Rights::RECEIVE) {
            return Err(ServerIngressError::InvalidIngress);
        }
        Ok(Self {
            role,
            receive_handle,
            object_id: info.object_id,
            closed: false,
        })
    }

    pub const fn role(&self) -> SessionRole {
        self.role
    }

    /// Receives one control record or candidate NSWP packet without blocking.
    pub fn try_receive(&mut self) -> Result<Option<ServerIngressEvent>, ServerIngressError> {
        if self.closed {
            return Err(ServerIngressError::Ipc(ipc::Error::BAD_FILE_DESCRIPTOR));
        }
        let mut bytes = [0_u8; MAX_PACKET_BYTES];
        let message = match ipc::try_receive(self.receive_handle, &mut bytes) {
            Ok(message) => message,
            Err(error) if error == ipc::Error::TRY_AGAIN => return Ok(None),
            Err(error) => return Err(ServerIngressError::Ipc(error)),
        };
        if message.sender_process_id == 0 {
            close_received(message.capability);
            return Err(ServerIngressError::InvalidOwnerProcessId);
        }
        let payload = &bytes[..message.bytes];
        if payload.starts_with(&CONTROL_MAGIC) {
            let control = match ControlRecord::decode(payload) {
                Ok(control) => control,
                Err(error) => {
                    close_received(message.capability);
                    return Err(ServerIngressError::MalformedControl(error));
                }
            };
            return self.control_event(message.sender_process_id, message.capability, control);
        }

        if let Some(capability) = message.capability {
            let _ = ipc::close(capability.handle);
            return Err(ServerIngressError::UnexpectedCapability);
        }
        let packet = PacketBuf::from_slice(payload)
            .expect("the ingress buffer is exactly the runtime packet bound");
        Ok(Some(ServerIngressEvent::Packet(InboundPacket {
            role: self.role,
            owner_process_id: message.sender_process_id,
            packet,
        })))
    }

    fn control_event(
        &self,
        owner_process_id: u64,
        capability: Option<ReceivedCapability>,
        control: ControlRecord,
    ) -> Result<Option<ServerIngressEvent>, ServerIngressError> {
        match control {
            ControlRecord::Connect => {
                let capability = capability.ok_or(ServerIngressError::MissingReplyCapability)?;
                if !valid_reply_capability(capability, self.object_id) {
                    let _ = ipc::close(capability.handle);
                    return Err(ServerIngressError::InvalidReplyCapability);
                }
                Ok(Some(ServerIngressEvent::Connect(PendingConnect {
                    role: self.role,
                    owner_process_id,
                    reply_handle: capability.handle,
                })))
            }
            ControlRecord::Disconnect { service_generation } => {
                if let Some(capability) = capability {
                    let _ = ipc::close(capability.handle);
                    return Err(ServerIngressError::UnexpectedCapability);
                }
                Ok(Some(ServerIngressEvent::Disconnect(DisconnectRequest {
                    role: self.role,
                    owner_process_id,
                    service_generation,
                })))
            }
            ControlRecord::ConnectResponse { .. } => {
                close_received(capability);
                Err(ServerIngressError::UnexpectedControlRecord)
            }
        }
    }

    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        close_if_valid(&mut self.receive_handle);
    }
}

impl Drop for ServerIngress {
    fn drop(&mut self) {
        self.close();
    }
}

// Keeping the bounded packet inline is the allocation-free alternative to boxing this variant.
#[allow(clippy::large_enum_variant)]
pub enum ServerIngressEvent {
    Connect(PendingConnect),
    Disconnect(DisconnectRequest),
    Packet(InboundPacket),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptError {
    InvalidServiceGeneration,
    InvalidStatus,
    WouldBlock,
    Ipc(ipc::Error),
    Completed,
}

/// A validated connect request that owns the client's exact `SEND` reply authority.
pub struct PendingConnect {
    role: SessionRole,
    owner_process_id: u64,
    reply_handle: CapabilityHandle,
}

impl PendingConnect {
    pub const fn role(&self) -> SessionRole {
        self.role
    }

    pub const fn owner_process_id(&self) -> u64 {
        self.owner_process_id
    }

    /// Sends an accepted response. `WouldBlock` leaves this request intact for a retry.
    pub fn try_accept(&mut self, service_generation: u64) -> Result<ServerTransport, AcceptError> {
        if service_generation == 0 {
            return Err(AcceptError::InvalidServiceGeneration);
        }
        if self.reply_handle == INVALID_HANDLE {
            return Err(AcceptError::Completed);
        }
        let response = ControlRecord::ConnectResponse {
            status: ConnectStatus::Accepted,
            service_generation,
        }
        .encode()
        .expect("accepted responses with a nonzero generation are canonical");
        send_control(self.reply_handle, &response)?;

        let reply_handle = self.reply_handle;
        self.reply_handle = INVALID_HANDLE;
        Ok(ServerTransport {
            role: self.role,
            owner_process_id: self.owner_process_id,
            service_generation,
            reply_handle,
            pending: PendingInbound::new(),
            closed: false,
        })
    }

    /// Sends a rejected response and closes the reply authority. `WouldBlock` leaves this request
    /// intact for a retry.
    pub fn try_reject(&mut self, status: ConnectStatus) -> Result<(), AcceptError> {
        if status == ConnectStatus::Accepted {
            return Err(AcceptError::InvalidStatus);
        }
        if self.reply_handle == INVALID_HANDLE {
            return Err(AcceptError::Completed);
        }
        let response = ControlRecord::ConnectResponse {
            status,
            service_generation: 0,
        }
        .encode()
        .expect("rejected responses with a zero generation are canonical");
        send_control(self.reply_handle, &response)?;
        close_if_valid(&mut self.reply_handle);
        Ok(())
    }
}

impl Drop for PendingConnect {
    fn drop(&mut self) {
        close_if_valid(&mut self.reply_handle);
    }
}

fn send_control(
    endpoint: CapabilityHandle,
    record: &[u8; CONTROL_RECORD_BYTES],
) -> Result<(), AcceptError> {
    match ipc::send(endpoint, record, None) {
        Ok(()) => Ok(()),
        Err(error) if error == ipc::Error::TRY_AGAIN => Err(AcceptError::WouldBlock),
        Err(error) => Err(AcceptError::Ipc(error)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisconnectRequest {
    role: SessionRole,
    owner_process_id: u64,
    service_generation: u64,
}

impl DisconnectRequest {
    pub const fn role(self) -> SessionRole {
        self.role
    }

    pub const fn owner_process_id(self) -> u64 {
        self.owner_process_id
    }

    pub const fn service_generation(self) -> u64 {
        self.service_generation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InboundPacket {
    role: SessionRole,
    owner_process_id: u64,
    packet: PacketBuf,
}

impl InboundPacket {
    pub const fn role(&self) -> SessionRole {
        self.role
    }

    pub const fn owner_process_id(&self) -> u64 {
        self.owner_process_id
    }

    pub fn bytes(&self) -> &[u8] {
        self.packet.as_slice()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuePacketError {
    Closed,
    WrongRole,
    WrongOwner,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisconnectError {
    Closed,
    WrongRole,
    WrongOwner,
    WrongServiceGeneration,
}

/// The server half of one logging session. It owns exact `SEND` reply authority and one bounded
/// pending client-to-server NSWP packet.
pub struct ServerTransport {
    role: SessionRole,
    owner_process_id: u64,
    service_generation: u64,
    reply_handle: CapabilityHandle,
    pending: PendingInbound,
    closed: bool,
}

impl ServerTransport {
    pub const fn role(&self) -> SessionRole {
        self.role
    }

    pub const fn owner_process_id(&self) -> u64 {
        self.owner_process_id
    }

    pub const fn service_generation(&self) -> u64 {
        self.service_generation
    }

    pub const fn has_pending_packet(&self) -> bool {
        self.pending.has_packet()
    }

    /// Queues one ingress packet after checking the authoritative role and owner PID.
    pub fn try_queue_packet(&mut self, packet: &InboundPacket) -> Result<(), QueuePacketError> {
        if self.closed {
            return Err(QueuePacketError::Closed);
        }
        if packet.role != self.role {
            return Err(QueuePacketError::WrongRole);
        }
        if !sender_is_pinned(self.owner_process_id, packet.owner_process_id) {
            return Err(QueuePacketError::WrongOwner);
        }
        self.pending.push(packet.packet)
    }

    /// Validates a disconnect against role, owner PID, and service generation, then closes the
    /// private reply authority.
    pub fn handle_disconnect(&mut self, request: DisconnectRequest) -> Result<(), DisconnectError> {
        validate_disconnect(
            self.closed,
            self.role,
            self.owner_process_id,
            self.service_generation,
            request,
        )?;
        self.close_local();
        Ok(())
    }

    fn close_local(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.pending.clear();
        close_if_valid(&mut self.reply_handle);
    }

    fn fail_send(&mut self) -> Result<(), TrySendError> {
        self.close_local();
        Err(TrySendError::PeerClosed)
    }
}

impl TryTransport for ServerTransport {
    fn try_send(&mut self, packet: &[u8]) -> Result<(), TrySendError> {
        if self.closed || packet.len() > MAX_PACKET_BYTES {
            return self.fail_send();
        }
        match ipc::send(self.reply_handle, packet, None) {
            Ok(()) => Ok(()),
            Err(error) if error == ipc::Error::TRY_AGAIN => Err(TrySendError::Full),
            Err(_) => self.fail_send(),
        }
    }

    fn try_recv(&mut self, output: &mut [u8]) -> Result<usize, TryRecvError> {
        if self.closed {
            return Err(TryRecvError::PeerClosed);
        }
        self.pending.receive(output)
    }

    fn close(&mut self) {
        self.close_local();
    }
}

impl Drop for ServerTransport {
    fn drop(&mut self) {
        self.close_local();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingInbound {
    packet: Option<PacketBuf>,
}

impl PendingInbound {
    const fn new() -> Self {
        Self { packet: None }
    }

    const fn has_packet(&self) -> bool {
        self.packet.is_some()
    }

    fn push(&mut self, packet: PacketBuf) -> Result<(), QueuePacketError> {
        if self.packet.is_some() {
            return Err(QueuePacketError::Full);
        }
        self.packet = Some(packet);
        Ok(())
    }

    fn receive(&mut self, output: &mut [u8]) -> Result<usize, TryRecvError> {
        let packet = self.packet.as_ref().ok_or(TryRecvError::Empty)?;
        if output.len() < packet.len() {
            return Err(TryRecvError::MessageTooLarge {
                bytes: packet.len(),
            });
        }
        let bytes = packet.len();
        output[..bytes].copy_from_slice(packet.as_slice());
        self.packet = None;
        Ok(bytes)
    }

    fn clear(&mut self) {
        self.packet = None;
    }
}

fn exact_endpoint(info: CapabilityInfo, rights: Rights) -> bool {
    info.kind == ObjectKind::Endpoint && info.rights == rights
}

fn valid_reply_capability(capability: ReceivedCapability, ingress_object_id: u64) -> bool {
    capability.rights == Rights::SEND
        && ipc::info(capability.handle).is_ok_and(|info| {
            exact_endpoint(info, Rights::SEND) && info.object_id != ingress_object_id
        })
}

fn sender_is_pinned(expected: u64, actual: u64) -> bool {
    expected != 0 && actual == expected
}

fn validate_disconnect(
    closed: bool,
    role: SessionRole,
    owner_process_id: u64,
    service_generation: u64,
    request: DisconnectRequest,
) -> Result<(), DisconnectError> {
    if closed {
        return Err(DisconnectError::Closed);
    }
    if request.role != role {
        return Err(DisconnectError::WrongRole);
    }
    if !sender_is_pinned(owner_process_id, request.owner_process_id) {
        return Err(DisconnectError::WrongOwner);
    }
    if request.service_generation != service_generation {
        return Err(DisconnectError::WrongServiceGeneration);
    }
    Ok(())
}

fn take_disconnect_record(
    pending: &mut bool,
    service_generation: u64,
) -> Option<[u8; CONTROL_RECORD_BYTES]> {
    if !*pending {
        return None;
    }
    *pending = false;
    ControlRecord::Disconnect { service_generation }
        .encode()
        .ok()
}

fn close_received(capability: Option<ReceivedCapability>) {
    if let Some(capability) = capability {
        let _ = ipc::close(capability.handle);
    }
}

fn close_if_valid(handle: &mut CapabilityHandle) {
    if *handle != INVALID_HANDLE {
        let _ = ipc::close(*handle);
        *handle = INVALID_HANDLE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(record: ControlRecord) -> [u8; CONTROL_RECORD_BYTES] {
        record.encode().unwrap()
    }

    #[test]
    fn admission_bounds_reserve_capacity_for_both_roles() {
        assert_eq!(admission_rejection(0, 0, false), None);
        assert_eq!(
            admission_rejection(1, 1, true),
            Some(ConnectStatus::Rejected)
        );
        assert_eq!(
            admission_rejection(MAX_LOGGING_SESSIONS, 2, false),
            Some(ConnectStatus::CapacityExhausted)
        );
        assert_eq!(
            admission_rejection(3, MAX_LOGGING_SESSIONS_PER_ROLE, false),
            Some(ConnectStatus::CapacityExhausted)
        );
        assert_eq!(admission_rejection(3, 2, false), None);
    }

    #[test]
    fn control_records_have_a_distinct_fixed_canonical_prefix() {
        let connect = encoded(ControlRecord::Connect);
        assert_eq!(connect.len(), CONTROL_RECORD_BYTES);
        assert_eq!(&connect[..4], b"NSLS");
        assert_ne!(&connect[..4], b"NSWP");
        assert_eq!(connect[4], CONTROL_WIRE_VERSION);
        assert_eq!(ControlRecord::decode(&connect), Ok(ControlRecord::Connect));
    }

    #[test]
    fn all_canonical_control_records_round_trip() {
        let records = [
            ControlRecord::Connect,
            ControlRecord::ConnectResponse {
                status: ConnectStatus::Accepted,
                service_generation: 7,
            },
            ControlRecord::ConnectResponse {
                status: ConnectStatus::Unavailable,
                service_generation: 0,
            },
            ControlRecord::ConnectResponse {
                status: ConnectStatus::CapacityExhausted,
                service_generation: 0,
            },
            ControlRecord::ConnectResponse {
                status: ConnectStatus::Rejected,
                service_generation: 0,
            },
            ControlRecord::Disconnect {
                service_generation: u64::MAX,
            },
        ];
        for record in records {
            let bytes = encoded(record);
            assert_eq!(ControlRecord::decode(&bytes), Ok(record));
        }
    }

    #[test]
    fn codec_rejects_every_noncanonical_control_field() {
        let connect = encoded(ControlRecord::Connect);
        assert_eq!(
            ControlRecord::decode(&connect[..15]),
            Err(ControlDecodeError::InvalidLength { bytes: 15 })
        );

        let mut malformed = connect;
        malformed[0] ^= 0xff;
        assert_eq!(
            ControlRecord::decode(&malformed),
            Err(ControlDecodeError::InvalidMagic)
        );

        let mut malformed = connect;
        malformed[4] = CONTROL_WIRE_VERSION + 1;
        assert_eq!(
            ControlRecord::decode(&malformed),
            Err(ControlDecodeError::UnsupportedVersion { version: 2 })
        );

        let mut malformed = connect;
        malformed[5] = 0xff;
        assert_eq!(
            ControlRecord::decode(&malformed),
            Err(ControlDecodeError::UnknownKind { kind: 0xff })
        );

        let mut malformed = connect;
        malformed[6..8].copy_from_slice(&1_u16.to_le_bytes());
        assert_eq!(
            ControlRecord::decode(&malformed),
            Err(ControlDecodeError::UnexpectedStatus { status: 1 })
        );

        let mut malformed = connect;
        malformed[8..16].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(
            ControlRecord::decode(&malformed),
            Err(ControlDecodeError::UnexpectedServiceGeneration)
        );
    }

    #[test]
    fn response_codec_rejects_unknown_status_and_bad_generation_pairings() {
        let mut unknown = encoded(ControlRecord::ConnectResponse {
            status: ConnectStatus::Rejected,
            service_generation: 0,
        });
        unknown[6..8].copy_from_slice(&99_u16.to_le_bytes());
        assert_eq!(
            ControlRecord::decode(&unknown),
            Err(ControlDecodeError::UnknownStatus { status: 99 })
        );

        assert_eq!(
            ControlRecord::ConnectResponse {
                status: ConnectStatus::Accepted,
                service_generation: 0,
            }
            .encode(),
            Err(ControlEncodeError::MissingServiceGeneration)
        );
        assert_eq!(
            ControlRecord::ConnectResponse {
                status: ConnectStatus::Rejected,
                service_generation: 1,
            }
            .encode(),
            Err(ControlEncodeError::UnexpectedServiceGeneration)
        );

        let mut accepted_without_generation = encoded(ControlRecord::ConnectResponse {
            status: ConnectStatus::Accepted,
            service_generation: 1,
        });
        accepted_without_generation[8..16].fill(0);
        assert_eq!(
            ControlRecord::decode(&accepted_without_generation),
            Err(ControlDecodeError::MissingServiceGeneration)
        );

        let mut rejected_with_generation = encoded(ControlRecord::ConnectResponse {
            status: ConnectStatus::Rejected,
            service_generation: 0,
        });
        rejected_with_generation[8..16].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(
            ControlRecord::decode(&rejected_with_generation),
            Err(ControlDecodeError::UnexpectedServiceGeneration)
        );
    }

    #[test]
    fn disconnect_requires_zero_status_and_nonzero_generation() {
        assert_eq!(
            ControlRecord::Disconnect {
                service_generation: 0,
            }
            .encode(),
            Err(ControlEncodeError::MissingServiceGeneration)
        );

        let mut malformed = encoded(ControlRecord::Disconnect {
            service_generation: 9,
        });
        malformed[6..8].copy_from_slice(&2_u16.to_le_bytes());
        assert_eq!(
            ControlRecord::decode(&malformed),
            Err(ControlDecodeError::UnexpectedStatus { status: 2 })
        );
    }

    fn endpoint(object_id: u64, rights: Rights) -> CapabilityInfo {
        CapabilityInfo {
            object_id,
            kind: ObjectKind::Endpoint,
            rights,
            size: 0,
        }
    }

    #[test]
    fn session_endpoints_require_exact_rights() {
        assert!(exact_endpoint(endpoint(1, Rights::SEND), Rights::SEND));
        assert!(exact_endpoint(
            endpoint(2, Rights::RECEIVE),
            Rights::RECEIVE
        ));
        assert!(!exact_endpoint(
            endpoint(1, Rights::SEND | Rights::TRANSFER),
            Rights::SEND
        ));
        assert!(!exact_endpoint(
            endpoint(2, Rights::RECEIVE | Rights::DUPLICATE),
            Rights::RECEIVE
        ));
        assert!(!exact_endpoint(
            CapabilityInfo {
                kind: ObjectKind::Notification,
                ..endpoint(1, Rights::SEND)
            },
            Rights::SEND
        ));
    }

    #[test]
    fn sender_identity_must_be_nonzero_and_remain_pinned() {
        assert!(!sender_is_pinned(0, 0));
        assert!(!sender_is_pinned(0, 7));
        assert!(!sender_is_pinned(7, 0));
        assert!(sender_is_pinned(7, 7));
        assert!(!sender_is_pinned(7, 8));
    }

    #[test]
    fn pending_inbound_is_exactly_one_packet_and_preserves_on_short_output() {
        let first = PacketBuf::from_slice(b"first").unwrap();
        let second = PacketBuf::from_slice(b"second").unwrap();
        let mut pending = PendingInbound::new();
        assert_eq!(pending.receive(&mut [0_u8; 8]), Err(TryRecvError::Empty));
        assert_eq!(pending.push(first), Ok(()));
        assert!(pending.has_packet());
        assert_eq!(pending.push(second), Err(QueuePacketError::Full));

        let mut short = [0_u8; 4];
        assert_eq!(
            pending.receive(&mut short),
            Err(TryRecvError::MessageTooLarge { bytes: 5 })
        );
        assert!(pending.has_packet());

        let mut output = [0_u8; 8];
        assert_eq!(pending.receive(&mut output), Ok(5));
        assert_eq!(&output[..5], b"first");
        assert!(!pending.has_packet());
        assert_eq!(pending.receive(&mut output), Err(TryRecvError::Empty));
    }

    #[test]
    fn disconnect_validation_pins_role_owner_and_generation() {
        let request = DisconnectRequest {
            role: SessionRole::Producer,
            owner_process_id: 41,
            service_generation: 9,
        };
        assert_eq!(
            validate_disconnect(false, SessionRole::Producer, 41, 9, request),
            Ok(())
        );
        assert_eq!(
            validate_disconnect(true, SessionRole::Producer, 41, 9, request),
            Err(DisconnectError::Closed)
        );
        assert_eq!(
            validate_disconnect(false, SessionRole::Observer, 41, 9, request),
            Err(DisconnectError::WrongRole)
        );
        assert_eq!(
            validate_disconnect(false, SessionRole::Producer, 42, 9, request),
            Err(DisconnectError::WrongOwner)
        );
        assert_eq!(
            validate_disconnect(false, SessionRole::Producer, 41, 10, request),
            Err(DisconnectError::WrongServiceGeneration)
        );
    }

    #[test]
    fn client_disconnect_is_canonical_and_emitted_at_most_once() {
        let mut pending = true;
        let first = take_disconnect_record(&mut pending, 77).unwrap();
        assert_eq!(
            ControlRecord::decode(&first),
            Ok(ControlRecord::Disconnect {
                service_generation: 77
            })
        );
        assert_eq!(take_disconnect_record(&mut pending, 77), None);

        let mut invalid = true;
        assert_eq!(take_disconnect_record(&mut invalid, 0), None);
        assert!(!invalid);
    }
}
