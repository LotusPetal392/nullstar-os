//! Allocation-free native transport for capability-separated `NSVC` service control.
//!
//! Client operations borrow an exact-`SEND` observation or mutation grant and own only a private
//! reply receiver. Server operations own an exact-`RECEIVE` ingress and each accepted request owns
//! the transferred reply sender until it is answered or dropped.

use service_control::{
    CorrelationError, DecodeError, Operation, RequestId, SERVICE_CONTROL_WIRE_BYTES,
    ServiceControlMessage, ServiceControlRequest, ServiceControlResponse,
};

use crate::{
    ipc::{
        self, CapabilityHandle, CapabilityInfo, ObjectKind, ReceivedCapability, Rights, Transfer,
    },
    service_route::EndpointShapeError,
};

pub const LOGGING_SERVICE_ID: service_control::ServiceId =
    match service_control::ServiceId::from_bytes([
        0x7c, 0xbd, 0x3f, 0x65, 0x50, 0xa6, 0x4c, 0x30, 0xb1, 0x95, 0x9f, 0xbe, 0xd6, 0x33, 0xda,
        0x43,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("logging service ID must be a canonical UUIDv4"),
    };

pub const NULLFS_SERVICE_ID: service_control::ServiceId =
    match service_control::ServiceId::from_bytes([
        0xf8, 0x42, 0xdb, 0x55, 0x55, 0xc1, 0x4d, 0xc7, 0xa2, 0xdd, 0x88, 0xf9, 0xe0, 0x78, 0x56,
        0x66,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("NullFS service ID must be a canonical UUIDv4"),
    };

pub const TMPFS_SERVICE_ID: service_control::ServiceId =
    match service_control::ServiceId::from_bytes([
        0x52, 0x2f, 0xe4, 0x4e, 0x98, 0xd1, 0x46, 0x29, 0x88, 0xe6, 0x8b, 0x47, 0x4a, 0x68, 0x1a,
        0xbc,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("tmpfs service ID must be a canonical UUIDv4"),
    };

pub const VFS_SERVICE_ID: service_control::ServiceId =
    match service_control::ServiceId::from_bytes([
        0x56, 0xc2, 0x30, 0xa6, 0xe1, 0x8f, 0x46, 0x35, 0x92, 0x64, 0x93, 0x07, 0x08, 0x51, 0x5b,
        0x3c,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("VFS service ID must be a canonical UUIDv4"),
    };

pub const AHCI_SERVICE_ID: service_control::ServiceId =
    match service_control::ServiceId::from_bytes([
        0x1a, 0x2b, 0x3c, 0x4d, 0x5e, 0x6f, 0x70, 0x81, 0x92, 0xa3, 0xb4, 0xc5, 0xd6, 0xe7, 0xf8,
        0x09,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("AHCI service ID must be a canonical UUIDv4"),
    };

pub const CONSOLE_SERVICE_ID: service_control::ServiceId =
    match service_control::ServiceId::from_bytes([
        0x2b, 0x3c, 0x4d, 0x5e, 0x6f, 0x70, 0x81, 0x92, 0xa3, 0xb4, 0xc5, 0xd6, 0xe7, 0xf8, 0x09,
        0x1a,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("Console service ID must be a canonical UUIDv4"),
    };

pub const SERIAL_SERVICE_ID: service_control::ServiceId =
    match service_control::ServiceId::from_bytes([
        0x3c, 0x4d, 0x5e, 0x6f, 0x70, 0x81, 0x92, 0xa3, 0xb4, 0xc5, 0xd6, 0xe7, 0xf8, 0x09, 0x1a,
        0x2b,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("Serial service ID must be a canonical UUIDv4"),
    };

pub const KEYBOARD_SERVICE_ID: service_control::ServiceId =
    match service_control::ServiceId::from_bytes([
        0x4d, 0x5e, 0x6f, 0x70, 0x81, 0x92, 0xa3, 0xb4, 0xc5, 0xd6, 0xe7, 0xf8, 0x09, 0x1a, 0x2b,
        0x3c,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("Keyboard service ID must be a canonical UUIDv4"),
    };

const KNOWN_SERVICES: [(&[u8], service_control::ServiceId); 8] = [
    (b"logging", LOGGING_SERVICE_ID),
    (b"nullfs", NULLFS_SERVICE_ID),
    (b"tmpfs", TMPFS_SERVICE_ID),
    (b"vfs", VFS_SERVICE_ID),
    (b"ahci", AHCI_SERVICE_ID),
    (b"console", CONSOLE_SERVICE_ID),
    (b"serial", SERIAL_SERVICE_ID),
    (b"keyboard", KEYBOARD_SERVICE_ID),
];

/// Maps a stable command name to its committed service ID.
pub fn service_id(name: &[u8]) -> Option<service_control::ServiceId> {
    KNOWN_SERVICES
        .iter()
        .find_map(|(known_name, id)| (*known_name == name).then_some(*id))
}

/// Maps a committed service ID to its stable command name.
pub fn service_name(service: service_control::ServiceId) -> Option<&'static [u8]> {
    KNOWN_SERVICES
        .iter()
        .find_map(|(name, known_id)| (*known_id == service).then_some(*name))
}

/// Failure while creating and sending one authorized control request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeginError {
    MutationNotAllowed { operation: Operation },
    InspectGrant(ipc::Error),
    InvalidGrant(EndpointShapeError),
    CreateReplyEndpoint(ipc::Error),
    InspectReplyEndpoint(ipc::Error),
    InvalidReplyEndpoint(EndpointShapeError),
    DuplicateReplyReceiver(ipc::Error),
    InspectReplyReceiver(ipc::Error),
    InvalidReplyReceiver(EndpointShapeError),
    Send(ipc::Error),
    CloseReplySource(ipc::Error),
}

impl BeginError {
    /// Returns whether the request was atomically enqueued before this local failure occurred.
    pub const fn request_was_sent(self) -> bool {
        matches!(self, Self::CloseReplySource(_))
    }
}

/// Terminal failure while receiving and validating a control response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompleteError {
    Receive(ipc::Error),
    Decode(DecodeError),
    ZeroServerProcessId,
    UnexpectedCapability,
    Correlation(CorrelationError),
}

/// One validated response and the kernel-stamped identity of its server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlReply {
    server_process_id: u64,
    response: ServiceControlResponse,
}

impl ControlReply {
    pub const fn server_process_id(self) -> u64 {
        self.server_process_id
    }

    pub const fn response(self) -> ServiceControlResponse {
        self.response
    }
}

/// An in-progress service-control exchange.
///
/// The authority grant passed to [`Self::begin`] or [`Self::begin_mutation`] remains caller-owned.
/// This value owns only the private exact-`RECEIVE` reply endpoint and closes it on every terminal
/// path.
pub struct ControlExchange {
    request: ServiceControlMessage,
    reply_receive: Option<CapabilityHandle>,
    reply_object_id: u64,
}

impl ControlExchange {
    /// Sends one canonical 64-byte request with one transferred exact-`SEND` reply capability.
    pub fn begin(
        observation_grant: CapabilityHandle,
        request_id: RequestId,
        request: ServiceControlRequest,
    ) -> Result<Self, BeginError> {
        ensure_observation(request)
            .map_err(|operation| BeginError::MutationNotAllowed { operation })?;
        Self::begin_authorized(observation_grant, request_id, request)
    }

    /// Sends one canonical mutation request through a separately delegated exact-`SEND` grant.
    pub fn begin_mutation(
        mutation_grant: CapabilityHandle,
        request_id: RequestId,
        request: ServiceControlRequest,
    ) -> Result<Self, BeginError> {
        ensure_mutation(request)
            .map_err(|operation| BeginError::MutationNotAllowed { operation })?;
        Self::begin_authorized(mutation_grant, request_id, request)
    }

    fn begin_authorized(
        authority_grant: CapabilityHandle,
        request_id: RequestId,
        request: ServiceControlRequest,
    ) -> Result<Self, BeginError> {
        let grant_info = ipc::info(authority_grant).map_err(BeginError::InspectGrant)?;
        validate_endpoint(grant_info, Rights::SEND, false).map_err(BeginError::InvalidGrant)?;

        let reply_source = ipc::endpoint_create().map_err(BeginError::CreateReplyEndpoint)?;
        let source_info = match ipc::info(reply_source) {
            Ok(info) => info,
            Err(error) => {
                close_quietly(reply_source);
                return Err(BeginError::InspectReplyEndpoint(error));
            }
        };
        if let Err(error) = validate_endpoint(source_info, Rights::ENDPOINT, true) {
            close_quietly(reply_source);
            return Err(BeginError::InvalidReplyEndpoint(error));
        }
        if source_info.object_id == grant_info.object_id {
            close_quietly(reply_source);
            return Err(BeginError::InvalidReplyEndpoint(
                EndpointShapeError::SameObject {
                    object_id: source_info.object_id,
                },
            ));
        }

        let reply_receive = match ipc::duplicate(reply_source, Rights::RECEIVE) {
            Ok(handle) => handle,
            Err(error) => {
                close_quietly(reply_source);
                return Err(BeginError::DuplicateReplyReceiver(error));
            }
        };
        let receive_info = match ipc::info(reply_receive) {
            Ok(info) => info,
            Err(error) => {
                close_quietly(reply_receive);
                close_quietly(reply_source);
                return Err(BeginError::InspectReplyReceiver(error));
            }
        };
        if let Err(error) = validate_endpoint(receive_info, Rights::RECEIVE, true) {
            close_quietly(reply_receive);
            close_quietly(reply_source);
            return Err(BeginError::InvalidReplyReceiver(error));
        }
        if receive_info.object_id != source_info.object_id {
            close_quietly(reply_receive);
            close_quietly(reply_source);
            return Err(BeginError::InvalidReplyReceiver(
                EndpointShapeError::ObjectMismatch {
                    expected: source_info.object_id,
                    actual: receive_info.object_id,
                },
            ));
        }

        let request = ServiceControlMessage::request(request_id, request);
        if let Err(error) = ipc::send(
            authority_grant,
            &request.encode(),
            Some(Transfer {
                handle: reply_source,
                rights: Rights::SEND,
            }),
        ) {
            close_quietly(reply_receive);
            close_quietly(reply_source);
            return Err(BeginError::Send(error));
        }
        if let Err(error) = ipc::close(reply_source) {
            close_quietly(reply_receive);
            return Err(BeginError::CloseReplySource(error));
        }

        Ok(Self {
            request,
            reply_receive: Some(reply_receive),
            reply_object_id: receive_info.object_id,
        })
    }

    pub const fn request(&self) -> ServiceControlMessage {
        self.request
    }

    pub const fn reply_object_id(&self) -> u64 {
        self.reply_object_id
    }

    /// Attempts one non-blocking receive. Any result other than `Ok(None)` is terminal.
    pub fn try_complete(&mut self) -> Result<Option<ControlReply>, CompleteError> {
        let Some(reply_receive) = self.reply_receive else {
            return Err(CompleteError::Receive(ipc::Error::BAD_FILE_DESCRIPTOR));
        };
        let mut bytes = [0_u8; SERVICE_CONTROL_WIRE_BYTES];
        let received = match ipc::try_receive(reply_receive, &mut bytes) {
            Ok(received) => received,
            Err(error) if error == ipc::Error::TRY_AGAIN => return Ok(None),
            Err(error) => {
                self.close_receiver();
                return Err(CompleteError::Receive(error));
            }
        };
        let result = validate_client_response(
            self.request,
            &bytes[..received.bytes],
            received.sender_process_id,
            received.capability,
        );
        self.close_receiver();
        result.map(Some)
    }

    fn close_receiver(&mut self) {
        if let Some(handle) = self.reply_receive.take() {
            close_quietly(handle);
        }
    }
}

impl Drop for ControlExchange {
    fn drop(&mut self) {
        self.close_receiver();
    }
}

fn validate_client_response(
    request: ServiceControlMessage,
    bytes: &[u8],
    sender_process_id: u64,
    capability: Option<ReceivedCapability>,
) -> Result<ControlReply, CompleteError> {
    let result =
        validate_client_response_envelope(request, bytes, sender_process_id, capability.is_some());
    close_received(capability);
    result
}

fn validate_client_response_envelope(
    request: ServiceControlMessage,
    bytes: &[u8],
    sender_process_id: u64,
    has_capability: bool,
) -> Result<ControlReply, CompleteError> {
    let message = ServiceControlMessage::decode(bytes).map_err(CompleteError::Decode)?;
    if sender_process_id == 0 {
        return Err(CompleteError::ZeroServerProcessId);
    }
    if has_capability {
        return Err(CompleteError::UnexpectedCapability);
    }
    message
        .validate_response_to(&request)
        .map_err(CompleteError::Correlation)?;
    let ServiceControlMessage::Response { response, .. } = message else {
        unreachable!("response correlation rejects request messages")
    };
    Ok(ControlReply {
        server_process_id: sender_process_id,
        response,
    })
}

/// Failure while binding a service-control ingress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindError {
    Inspect(ipc::Error),
    Invalid(EndpointShapeError),
}

/// Failure while receiving or validating one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngressError {
    Receive(ipc::Error),
    Decode(DecodeError),
    ResponseNotAllowed,
    ZeroCallerProcessId,
    MissingReplyCapability,
    InvalidTransferredRights { actual: Rights },
    InspectReply(ipc::Error),
    InvalidReply(EndpointShapeError),
}

/// A service-control ingress owning one exact-`RECEIVE` endpoint.
pub struct ControlIngress {
    receive: Option<CapabilityHandle>,
    object_id: u64,
}

impl ControlIngress {
    pub fn bind(receive: CapabilityHandle) -> Result<Self, BindError> {
        let info = ipc::info(receive).map_err(BindError::Inspect)?;
        validate_endpoint(info, Rights::RECEIVE, false).map_err(BindError::Invalid)?;
        Ok(Self {
            receive: Some(receive),
            object_id: info.object_id,
        })
    }

    pub const fn object_id(&self) -> u64 {
        self.object_id
    }

    /// Attempts one non-blocking receive and consumes malformed oversized packets safely.
    pub fn try_accept(&mut self) -> Result<Option<PendingControlRequest>, IngressError> {
        let Some(receive) = self.receive else {
            return Err(IngressError::Receive(ipc::Error::BAD_FILE_DESCRIPTOR));
        };
        let mut bytes = [0_u8; crate::abi::limits::MAX_IPC_MESSAGE_BYTES];
        let received = match ipc::try_receive(receive, &mut bytes) {
            Ok(received) => received,
            Err(error) if error == ipc::Error::TRY_AGAIN => return Ok(None),
            Err(error) => return Err(IngressError::Receive(error)),
        };
        validate_ingress_request(
            self.object_id,
            &bytes[..received.bytes],
            received.sender_process_id,
            received.capability,
        )
        .map(Some)
    }
}

impl Drop for ControlIngress {
    fn drop(&mut self) {
        if let Some(handle) = self.receive.take() {
            close_quietly(handle);
        }
    }
}

fn validate_request_envelope(
    bytes: &[u8],
    sender_process_id: u64,
    capability_rights: Option<Rights>,
) -> Result<(RequestId, ServiceControlRequest), IngressError> {
    let message = ServiceControlMessage::decode(bytes).map_err(IngressError::Decode)?;
    let ServiceControlMessage::Request {
        request_id,
        request,
    } = message
    else {
        return Err(IngressError::ResponseNotAllowed);
    };
    if sender_process_id == 0 {
        return Err(IngressError::ZeroCallerProcessId);
    }
    match capability_rights {
        None => Err(IngressError::MissingReplyCapability),
        Some(Rights::SEND) => Ok((request_id, request)),
        Some(actual) => Err(IngressError::InvalidTransferredRights { actual }),
    }
}

fn validate_ingress_request(
    ingress_object_id: u64,
    bytes: &[u8],
    sender_process_id: u64,
    capability: Option<ReceivedCapability>,
) -> Result<PendingControlRequest, IngressError> {
    let (request_id, request) = match validate_request_envelope(
        bytes,
        sender_process_id,
        capability.map(|received| received.rights),
    ) {
        Ok(request) => request,
        Err(error) => {
            close_received(capability);
            return Err(error);
        }
    };
    let reply = capability.expect("valid request envelope requires a reply capability");
    let info = match ipc::info(reply.handle) {
        Ok(info) => info,
        Err(error) => {
            close_quietly(reply.handle);
            return Err(IngressError::InspectReply(error));
        }
    };
    if let Err(error) = validate_endpoint(info, Rights::SEND, true) {
        close_quietly(reply.handle);
        return Err(IngressError::InvalidReply(error));
    }
    if info.object_id == ingress_object_id {
        close_quietly(reply.handle);
        return Err(IngressError::InvalidReply(EndpointShapeError::SameObject {
            object_id: info.object_id,
        }));
    }

    Ok(PendingControlRequest {
        request_id,
        request,
        caller_process_id: sender_process_id,
        reply: Some(reply.handle),
    })
}

/// Failure while issuing a correlated response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyError {
    Correlation(CorrelationError),
    Send(ipc::Error),
    Close(ipc::Error),
}

/// An accepted request owning its exact-`SEND` private reply endpoint.
pub struct PendingControlRequest {
    request_id: RequestId,
    request: ServiceControlRequest,
    caller_process_id: u64,
    reply: Option<CapabilityHandle>,
}

impl PendingControlRequest {
    pub const fn request_id(&self) -> RequestId {
        self.request_id
    }

    pub const fn request(&self) -> ServiceControlRequest {
        self.request
    }

    pub const fn caller_process_id(&self) -> u64 {
        self.caller_process_id
    }

    /// Sends exactly one correlated response without a capability and closes the reply endpoint.
    pub fn reply(mut self, response: ServiceControlResponse) -> Result<(), ReplyError> {
        let request = ServiceControlMessage::request(self.request_id, self.request);
        let response = ServiceControlMessage::response(self.request_id, response);
        response
            .validate_response_to(&request)
            .map_err(ReplyError::Correlation)?;

        let handle = self
            .reply
            .take()
            .expect("pending request owns its reply endpoint until consumed");
        let send_result = ipc::send(handle, &response.encode(), None);
        let close_result = ipc::close(handle);
        match (send_result, close_result) {
            (Err(error), _) => Err(ReplyError::Send(error)),
            (Ok(()), Err(error)) => Err(ReplyError::Close(error)),
            (Ok(()), Ok(())) => Ok(()),
        }
    }
}

impl Drop for PendingControlRequest {
    fn drop(&mut self) {
        if let Some(handle) = self.reply.take() {
            close_quietly(handle);
        }
    }
}

fn ensure_observation(request: ServiceControlRequest) -> Result<(), Operation> {
    match request {
        ServiceControlRequest::List { .. } | ServiceControlRequest::Status { .. } => Ok(()),
        ServiceControlRequest::Start { .. }
        | ServiceControlRequest::Stop { .. }
        | ServiceControlRequest::Restart { .. } => Err(request.operation()),
    }
}

fn ensure_mutation(request: ServiceControlRequest) -> Result<(), Operation> {
    match request {
        ServiceControlRequest::Start { .. }
        | ServiceControlRequest::Stop { .. }
        | ServiceControlRequest::Restart { .. } => Ok(()),
        ServiceControlRequest::List { .. } | ServiceControlRequest::Status { .. } => {
            Err(request.operation())
        }
    }
}

fn validate_endpoint(
    info: CapabilityInfo,
    expected_rights: Rights,
    require_empty: bool,
) -> Result<(), EndpointShapeError> {
    if info.object_id == 0 {
        return Err(EndpointShapeError::ZeroObjectId);
    }
    if info.kind != ObjectKind::Endpoint {
        return Err(EndpointShapeError::WrongKind { actual: info.kind });
    }
    if info.rights != expected_rights {
        return Err(EndpointShapeError::WrongRights {
            expected: expected_rights,
            actual: info.rights,
        });
    }
    if require_empty && info.size != 0 {
        return Err(EndpointShapeError::NonEmptyQueue {
            messages: info.size,
        });
    }
    Ok(())
}

fn close_received(capability: Option<ReceivedCapability>) {
    if let Some(capability) = capability {
        close_quietly(capability.handle);
    }
}

fn close_quietly(handle: CapabilityHandle) {
    let _ = ipc::close(handle);
}

#[cfg(test)]
mod tests {
    use service_control::{
        DesiredState, ListResponse, ObservedState, ProviderGeneration, ServiceId, ServiceRecord,
        TargetResponse,
    };

    use super::*;

    const UUID: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];

    fn service() -> ServiceId {
        ServiceId::from_bytes(UUID).unwrap()
    }

    fn request_id() -> RequestId {
        RequestId::new(7).unwrap()
    }

    fn endpoint(object_id: u64, rights: Rights, size: u64) -> CapabilityInfo {
        CapabilityInfo {
            object_id,
            kind: ObjectKind::Endpoint,
            rights,
            size,
        }
    }

    fn status_request() -> ServiceControlMessage {
        ServiceControlMessage::request(
            request_id(),
            ServiceControlRequest::Status { service: service() },
        )
    }

    fn status_response() -> ServiceControlMessage {
        let record = ServiceRecord::new(
            service(),
            Some(ProviderGeneration::new(3).unwrap()),
            ObservedState::Ready,
            DesiredState::Running,
        )
        .unwrap();
        ServiceControlMessage::response(
            request_id(),
            ServiceControlResponse::status(TargetResponse::success(record)),
        )
    }

    #[test]
    fn committed_service_ids_are_uuid_v4_and_names_map_both_ways() {
        for (name, service) in KNOWN_SERVICES {
            assert_eq!(service.as_bytes()[6] >> 4, 4);
            assert_eq!(service.as_bytes()[8] & 0xc0, 0x80);
            assert_eq!(service_id(name), Some(service));
            assert_eq!(service_name(service), Some(name));
        }
        assert_eq!(service_id(b"unknown"), None);
        assert_eq!(LOGGING_SERVICE_ID, nswp_logging::LOGGING_SERVICE_ID);
    }

    #[test]
    fn endpoint_shapes_require_exact_directional_rights_and_empty_replies() {
        assert_eq!(
            validate_endpoint(endpoint(1, Rights::SEND, 9), Rights::SEND, false),
            Ok(())
        );
        assert_eq!(
            validate_endpoint(endpoint(2, Rights::RECEIVE, 9), Rights::RECEIVE, false),
            Ok(())
        );
        assert_eq!(
            validate_endpoint(
                endpoint(3, Rights::SEND | Rights::TRANSFER, 0),
                Rights::SEND,
                true
            ),
            Err(EndpointShapeError::WrongRights {
                expected: Rights::SEND,
                actual: Rights::SEND | Rights::TRANSFER,
            })
        );
        assert_eq!(
            validate_endpoint(endpoint(3, Rights::SEND, 1), Rights::SEND, true),
            Err(EndpointShapeError::NonEmptyQueue { messages: 1 })
        );
        assert_eq!(
            validate_endpoint(endpoint(0, Rights::SEND, 0), Rights::SEND, true),
            Err(EndpointShapeError::ZeroObjectId)
        );
    }

    #[test]
    fn response_envelope_requires_exact_codec_identity_no_capability_and_correlation() {
        let request = status_request();
        let response = status_response();
        let accepted =
            validate_client_response_envelope(request, &response.encode(), 42, false).unwrap();
        assert_eq!(accepted.server_process_id(), 42);
        assert_eq!(accepted.response().operation(), Operation::Status);

        assert_eq!(
            validate_client_response_envelope(request, &response.encode(), 0, false),
            Err(CompleteError::ZeroServerProcessId)
        );
        assert_eq!(
            validate_client_response_envelope(request, &response.encode(), 42, true),
            Err(CompleteError::UnexpectedCapability)
        );
        assert_eq!(
            validate_client_response_envelope(request, &response.encode()[..63], 42, false),
            Err(CompleteError::Decode(DecodeError::InvalidLength))
        );

        let wrong_id = ServiceControlMessage::response(
            RequestId::new(8).unwrap(),
            ServiceControlResponse::status(TargetResponse::failure(
                service(),
                service_control::ServiceControlFailure::NotFound,
            )),
        );
        assert_eq!(
            validate_client_response_envelope(request, &wrong_id.encode(), 42, false),
            Err(CompleteError::Correlation(
                CorrelationError::RequestIdMismatch
            ))
        );
    }

    #[test]
    fn ingress_envelope_requires_observation_request_caller_and_one_exact_send_reply() {
        let request = status_request().encode();
        assert_eq!(
            validate_request_envelope(&request, 9, Some(Rights::SEND)),
            Ok((
                request_id(),
                ServiceControlRequest::Status { service: service() }
            ))
        );
        assert_eq!(
            validate_request_envelope(&request, 0, Some(Rights::SEND)),
            Err(IngressError::ZeroCallerProcessId)
        );
        assert_eq!(
            validate_request_envelope(&request, 9, None),
            Err(IngressError::MissingReplyCapability)
        );
        assert_eq!(
            validate_request_envelope(&request, 9, Some(Rights::SEND | Rights::TRANSFER)),
            Err(IngressError::InvalidTransferredRights {
                actual: Rights::SEND | Rights::TRANSFER,
            })
        );
        assert_eq!(
            validate_request_envelope(&status_response().encode(), 9, Some(Rights::SEND)),
            Err(IngressError::ResponseNotAllowed)
        );
    }

    #[test]
    fn observation_client_rejects_mutations_but_server_admits_them_for_policy_denial() {
        for request in [
            ServiceControlRequest::Start { service: service() },
            ServiceControlRequest::Stop { service: service() },
            ServiceControlRequest::Restart { service: service() },
        ] {
            assert_eq!(ensure_observation(request), Err(request.operation()));
            assert_eq!(ensure_mutation(request), Ok(()));
            let message = ServiceControlMessage::request(request_id(), request).encode();
            assert_eq!(
                validate_request_envelope(&message, 9, Some(Rights::SEND)),
                Ok((request_id(), request))
            );
        }
        for request in [
            ServiceControlRequest::List { cursor: 0 },
            ServiceControlRequest::Status { service: service() },
        ] {
            assert_eq!(ensure_observation(request), Ok(()));
            assert_eq!(ensure_mutation(request), Err(request.operation()));
        }
    }

    #[test]
    fn begin_errors_identify_the_post_send_cleanup_boundary() {
        assert!(!BeginError::Send(ipc::Error::TRY_AGAIN).request_was_sent());
        assert!(BeginError::CloseReplySource(ipc::Error::BAD_FILE_DESCRIPTOR).request_was_sent());
    }

    #[test]
    fn reply_correlation_covers_list_cursor_and_status_target() {
        let list_request =
            ServiceControlMessage::request(request_id(), ServiceControlRequest::List { cursor: 4 });
        let wrong_cursor = ServiceControlMessage::response(
            request_id(),
            ServiceControlResponse::list(ListResponse::end(5)),
        );
        assert_eq!(
            wrong_cursor.validate_response_to(&list_request),
            Err(CorrelationError::ListCursorMismatch)
        );

        let other = ServiceId::from_bytes([
            0x10, 0x21, 0x32, 0x43, 0x54, 0x65, 0x47, 0x87, 0x98, 0xa9, 0xba, 0xcb, 0xdc, 0xed,
            0xfe, 0x0f,
        ])
        .unwrap();
        let wrong_target = ServiceControlMessage::response(
            request_id(),
            ServiceControlResponse::status(TargetResponse::failure(
                other,
                service_control::ServiceControlFailure::NotFound,
            )),
        );
        assert_eq!(
            wrong_target.validate_response_to(&status_request()),
            Err(CorrelationError::TargetServiceMismatch)
        );
    }
}
