//! Allocation-free native service-route transport and publication adapter.
//!
//! The adapter deliberately exposes non-blocking, one-message operations. Callers can fold
//! [`RouteResolution::try_complete`] and [`RouteIngress::try_accept`] into their own bounded pump.

use service_route::{
    Authorizer, DecodeError, ProviderGeneration, PublishError, PublishedRoute, RouteFailure,
    RouteKey, RouteMessage, RouteTable, SERVICE_GENERATION_WIRE_BYTES, SERVICE_ROUTE_WIRE_BYTES,
    ServiceGenerationDecodeError, ServiceGenerationHandoff, WithdrawError,
};

use crate::ipc::{
    self, CapabilityHandle, CapabilityInfo, ObjectKind, ReceivedCapability, Rights, Transfer,
};

const PROVIDER_SOURCE_RIGHTS: Rights = match Rights::from_bits(
    Rights::SEND.bits() | Rights::DUPLICATE.bits() | Rights::TRANSFER.bits(),
) {
    Some(rights) => rights,
    None => panic!("provider source rights must be valid"),
};
const DISPOSABLE_PROVIDER_RIGHTS: Rights =
    match Rights::from_bits(Rights::SEND.bits() | Rights::TRANSFER.bits()) {
        Some(rights) => rights,
        None => panic!("disposable provider rights must be valid"),
    };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationHandoffSendError {
    Inspect(ipc::Error),
    InvalidEndpoint(EndpointShapeError),
    Send(ipc::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationHandoffReceiveError {
    InvalidExpectedSender,
    Inspect(ipc::Error),
    InvalidEndpoint(EndpointShapeError),
    Receive(ipc::Error),
    UnexpectedSender { expected: u64, actual: u64 },
    UnexpectedCapability,
    Decode(ServiceGenerationDecodeError),
    Close(ipc::Error),
}

/// Queues one canonical generation handoff on an empty full-rights endpoint.
///
/// The source remains caller-owned. Sending before the child is released ensures the child cannot
/// observe an uninitialized generation slot.
pub fn queue_service_generation(
    source: CapabilityHandle,
    generation: ProviderGeneration,
) -> Result<(), GenerationHandoffSendError> {
    let info = ipc::info(source).map_err(GenerationHandoffSendError::Inspect)?;
    validate_endpoint(info, Rights::ENDPOINT, true)
        .map_err(GenerationHandoffSendError::InvalidEndpoint)?;
    ipc::send(
        source,
        &ServiceGenerationHandoff::new(generation).encode(),
        None,
    )
    .map_err(GenerationHandoffSendError::Send)
}

/// Receives one generation from an exact-`RECEIVE` bootstrap endpoint and closes the handle.
///
/// The handle is consumed on every return path. The sender identity is kernel-stamped and must
/// match the expected generation authority.
pub fn receive_service_generation(
    receive: CapabilityHandle,
    expected_sender: u64,
) -> Result<ProviderGeneration, GenerationHandoffReceiveError> {
    if expected_sender == 0 {
        close_quietly(receive);
        return Err(GenerationHandoffReceiveError::InvalidExpectedSender);
    }
    let result = receive_service_generation_inner(receive, expected_sender);
    let close_result = ipc::close(receive);
    match (result, close_result) {
        (Err(error), _) => Err(error),
        (Ok(generation), Ok(())) => Ok(generation),
        (Ok(_), Err(error)) => Err(GenerationHandoffReceiveError::Close(error)),
    }
}

fn receive_service_generation_inner(
    receive: CapabilityHandle,
    expected_sender: u64,
) -> Result<ProviderGeneration, GenerationHandoffReceiveError> {
    let info = ipc::info(receive).map_err(GenerationHandoffReceiveError::Inspect)?;
    validate_endpoint(info, Rights::RECEIVE, false)
        .map_err(GenerationHandoffReceiveError::InvalidEndpoint)?;
    let mut bytes = [0_u8; SERVICE_GENERATION_WIRE_BYTES];
    let message =
        ipc::receive(receive, &mut bytes).map_err(GenerationHandoffReceiveError::Receive)?;
    let has_capability = message.capability.is_some();
    close_received(message.capability);
    validate_service_generation_envelope(
        &bytes[..message.bytes],
        message.sender_process_id,
        expected_sender,
        has_capability,
    )
}

fn validate_service_generation_envelope(
    bytes: &[u8],
    sender_process_id: u64,
    expected_sender: u64,
    has_capability: bool,
) -> Result<ProviderGeneration, GenerationHandoffReceiveError> {
    if has_capability {
        return Err(GenerationHandoffReceiveError::UnexpectedCapability);
    }
    if sender_process_id != expected_sender {
        return Err(GenerationHandoffReceiveError::UnexpectedSender {
            expected: expected_sender,
            actual: sender_process_id,
        });
    }
    ServiceGenerationHandoff::decode(bytes)
        .map(ServiceGenerationHandoff::generation)
        .map_err(GenerationHandoffReceiveError::Decode)
}

/// Why a capability does not have the exact endpoint shape required by service routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointShapeError {
    ZeroObjectId,
    WrongKind { actual: ObjectKind },
    WrongRights { expected: Rights, actual: Rights },
    NonEmptyQueue { messages: u64 },
    ObjectMismatch { expected: u64, actual: u64 },
    SameObject { object_id: u64 },
}

/// Failure while creating and sending a route request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootstrapError {
    InspectRouteGrant(ipc::Error),
    InvalidRouteGrant(EndpointShapeError),
    CreateReplyEndpoint(ipc::Error),
    InspectReplyEndpoint(ipc::Error),
    InvalidReplyEndpoint(EndpointShapeError),
    DuplicateReplyReceiver(ipc::Error),
    InspectReplyReceiver(ipc::Error),
    InvalidReplyReceiver(EndpointShapeError),
    SendRequest(ipc::Error),
    CloseReplySource(ipc::Error),
}

/// A terminal failure while validating a broker response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveError {
    Receive(ipc::Error),
    Decode(DecodeError),
    ZeroBrokerProcessId,
    UnexpectedResponse,
    KeyMismatch {
        expected: RouteKey,
        actual: RouteKey,
    },
    MissingProviderCapability,
    UnexpectedCapability,
    InspectProvider(ipc::Error),
    InvalidProvider(EndpointShapeError),
    BrokerFailure(RouteFailure),
}

/// An in-progress client-side route resolution.
///
/// The route-grant handle passed to [`RouteResolution::begin`] is borrowed by value but remains
/// caller-owned and open. This object owns only its private exact-`RECEIVE` reply endpoint.
pub struct RouteResolution {
    key: RouteKey,
    reply_receive: Option<CapabilityHandle>,
    reply_object_id: u64,
}

impl RouteResolution {
    /// Validates an exact-`SEND` route grant, creates a private reply endpoint, and sends one
    /// canonical request with an exact-`SEND` attachment.
    pub fn begin(route_grant: CapabilityHandle, key: RouteKey) -> Result<Self, BootstrapError> {
        let grant_info = ipc::info(route_grant).map_err(BootstrapError::InspectRouteGrant)?;
        validate_endpoint(grant_info, Rights::SEND, false)
            .map_err(BootstrapError::InvalidRouteGrant)?;

        let reply_source = ipc::endpoint_create().map_err(BootstrapError::CreateReplyEndpoint)?;
        let source_info = match ipc::info(reply_source) {
            Ok(info) => info,
            Err(error) => {
                close_quietly(reply_source);
                return Err(BootstrapError::InspectReplyEndpoint(error));
            }
        };
        if let Err(error) = validate_endpoint(source_info, Rights::ENDPOINT, true) {
            close_quietly(reply_source);
            return Err(BootstrapError::InvalidReplyEndpoint(error));
        }

        let reply_receive = match ipc::duplicate(reply_source, Rights::RECEIVE) {
            Ok(handle) => handle,
            Err(error) => {
                close_quietly(reply_source);
                return Err(BootstrapError::DuplicateReplyReceiver(error));
            }
        };
        let receive_info = match ipc::info(reply_receive) {
            Ok(info) => info,
            Err(error) => {
                close_quietly(reply_receive);
                close_quietly(reply_source);
                return Err(BootstrapError::InspectReplyReceiver(error));
            }
        };
        if let Err(error) = validate_endpoint(receive_info, Rights::RECEIVE, true) {
            close_quietly(reply_receive);
            close_quietly(reply_source);
            return Err(BootstrapError::InvalidReplyReceiver(error));
        }
        if receive_info.object_id != source_info.object_id {
            close_quietly(reply_receive);
            close_quietly(reply_source);
            return Err(BootstrapError::InvalidReplyReceiver(
                EndpointShapeError::ObjectMismatch {
                    expected: source_info.object_id,
                    actual: receive_info.object_id,
                },
            ));
        }

        let request = RouteMessage::Request { key }.encode();
        if let Err(error) = ipc::send(
            route_grant,
            &request,
            Some(Transfer {
                handle: reply_source,
                rights: Rights::SEND,
            }),
        ) {
            close_quietly(reply_receive);
            close_quietly(reply_source);
            return Err(BootstrapError::SendRequest(error));
        }
        if let Err(error) = ipc::close(reply_source) {
            close_quietly(reply_receive);
            return Err(BootstrapError::CloseReplySource(error));
        }

        Ok(Self {
            key,
            reply_receive: Some(reply_receive),
            reply_object_id: receive_info.object_id,
        })
    }

    pub const fn key(&self) -> RouteKey {
        self.key
    }

    pub const fn reply_object_id(&self) -> u64 {
        self.reply_object_id
    }

    /// Attempts exactly one non-blocking receive.
    ///
    /// `Ok(None)` means no response is queued. Every other result is terminal and closes the
    /// private reply receiver. Any unexpected attached capability is also closed.
    pub fn try_complete(&mut self) -> Result<Option<ResolvedRoute>, ResolveError> {
        let Some(reply_receive) = self.reply_receive else {
            return Err(ResolveError::Receive(ipc::Error::BAD_FILE_DESCRIPTOR));
        };
        let mut bytes = [0_u8; SERVICE_ROUTE_WIRE_BYTES];
        let received = match ipc::try_receive(reply_receive, &mut bytes) {
            Ok(received) => received,
            Err(error) if error == ipc::Error::TRY_AGAIN => return Ok(None),
            Err(error) => {
                self.close_receiver();
                return Err(ResolveError::Receive(error));
            }
        };

        let result = validate_client_response(
            self.key,
            self.reply_object_id,
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

impl Drop for RouteResolution {
    fn drop(&mut self) {
        self.close_receiver();
    }
}

/// An accepted provider route. The exact-`SEND` provider ingress handle is owned by this value.
pub struct ResolvedRoute {
    key: RouteKey,
    generation: ProviderGeneration,
    broker_process_id: u64,
    object_id: u64,
    handle: Option<CapabilityHandle>,
}

impl ResolvedRoute {
    pub const fn key(&self) -> RouteKey {
        self.key
    }

    pub const fn generation(&self) -> ProviderGeneration {
        self.generation
    }

    pub const fn broker_process_id(&self) -> u64 {
        self.broker_process_id
    }

    pub const fn object_id(&self) -> u64 {
        self.object_id
    }

    /// Transfers ownership of the exact-`SEND` provider ingress handle to the caller.
    pub fn into_handle(mut self) -> CapabilityHandle {
        self.handle
            .take()
            .expect("resolved route handle is present until consumed")
    }
}

impl Drop for ResolvedRoute {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            close_quietly(handle);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponseDisposition {
    Accepted(ProviderGeneration),
    Failure(RouteFailure),
}

fn validate_response_envelope(
    expected_key: RouteKey,
    bytes: &[u8],
    sender_process_id: u64,
    capability_rights: Option<Rights>,
) -> Result<ResponseDisposition, ResolveError> {
    let message = RouteMessage::decode(bytes).map_err(ResolveError::Decode)?;
    if sender_process_id == 0 {
        return Err(ResolveError::ZeroBrokerProcessId);
    }
    if message.key() != expected_key {
        return Err(ResolveError::KeyMismatch {
            expected: expected_key,
            actual: message.key(),
        });
    }

    match message {
        RouteMessage::Accepted { generation, .. } => match capability_rights {
            None => Err(ResolveError::MissingProviderCapability),
            Some(Rights::SEND) => Ok(ResponseDisposition::Accepted(generation)),
            Some(actual) => Err(ResolveError::InvalidProvider(
                EndpointShapeError::WrongRights {
                    expected: Rights::SEND,
                    actual,
                },
            )),
        },
        RouteMessage::Failure { failure, .. } => {
            if capability_rights.is_some() {
                Err(ResolveError::UnexpectedCapability)
            } else {
                Ok(ResponseDisposition::Failure(failure))
            }
        }
        RouteMessage::Request { .. } => Err(ResolveError::UnexpectedResponse),
    }
}

fn validate_client_response(
    expected_key: RouteKey,
    reply_object_id: u64,
    bytes: &[u8],
    sender_process_id: u64,
    capability: Option<ReceivedCapability>,
) -> Result<ResolvedRoute, ResolveError> {
    let disposition = match validate_response_envelope(
        expected_key,
        bytes,
        sender_process_id,
        capability.map(|received| received.rights),
    ) {
        Ok(disposition) => disposition,
        Err(error) => {
            close_received(capability);
            return Err(error);
        }
    };
    let ResponseDisposition::Accepted(generation) = disposition else {
        let ResponseDisposition::Failure(failure) = disposition else {
            unreachable!()
        };
        return Err(ResolveError::BrokerFailure(failure));
    };
    let received = capability.expect("accepted response envelope requires a capability");
    let info = match ipc::info(received.handle) {
        Ok(info) => info,
        Err(error) => {
            close_quietly(received.handle);
            return Err(ResolveError::InspectProvider(error));
        }
    };
    if let Err(error) = validate_endpoint(info, Rights::SEND, false) {
        close_quietly(received.handle);
        return Err(ResolveError::InvalidProvider(error));
    }
    if info.object_id == reply_object_id {
        close_quietly(received.handle);
        return Err(ResolveError::InvalidProvider(
            EndpointShapeError::SameObject {
                object_id: info.object_id,
            },
        ));
    }
    Ok(ResolvedRoute {
        key: expected_key,
        generation,
        broker_process_id: sender_process_id,
        object_id: info.object_id,
        handle: Some(received.handle),
    })
}

/// Failure while binding a broker ingress endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindError {
    Inspect(ipc::Error),
    Invalid(EndpointShapeError),
}

/// Failure while receiving or validating one route request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngressError {
    Receive(ipc::Error),
    Decode(DecodeError),
    UnexpectedMessage,
    WrongGrantedKey {
        granted: RouteKey,
        requested: RouteKey,
    },
    ZeroSenderProcessId,
    MissingReplyCapability,
    InvalidTransferredRights {
        actual: Rights,
    },
    InspectReply(ipc::Error),
    InvalidReply(EndpointShapeError),
}

/// A broker ingress bound to exactly one granted route key and one exact-`RECEIVE` endpoint.
pub struct RouteIngress {
    granted_key: RouteKey,
    receive: Option<CapabilityHandle>,
    object_id: u64,
}

impl RouteIngress {
    pub fn bind(receive: CapabilityHandle, granted_key: RouteKey) -> Result<Self, BindError> {
        let info = ipc::info(receive).map_err(BindError::Inspect)?;
        validate_endpoint(info, Rights::RECEIVE, false).map_err(BindError::Invalid)?;
        Ok(Self {
            granted_key,
            receive: Some(receive),
            object_id: info.object_id,
        })
    }

    pub const fn granted_key(&self) -> RouteKey {
        self.granted_key
    }

    pub const fn object_id(&self) -> u64 {
        self.object_id
    }

    /// Attempts exactly one non-blocking request receive.
    pub fn try_accept(&mut self) -> Result<Option<PendingRouteRequest>, IngressError> {
        let Some(receive) = self.receive else {
            return Err(IngressError::Receive(ipc::Error::BAD_FILE_DESCRIPTOR));
        };
        // Receive at the ABI maximum so an oversized malformed request is consumed rather than
        // remaining at the head of the shared ingress queue after a `RANGE` result.
        let mut bytes = [0_u8; crate::abi::limits::MAX_IPC_MESSAGE_BYTES];
        let received = match ipc::try_receive(receive, &mut bytes) {
            Ok(received) => received,
            Err(error) if error == ipc::Error::TRY_AGAIN => return Ok(None),
            Err(error) => return Err(IngressError::Receive(error)),
        };
        validate_ingress_request(
            self.granted_key,
            self.object_id,
            &bytes[..received.bytes],
            received.sender_process_id,
            received.capability,
        )
        .map(Some)
    }

    pub fn into_handle(mut self) -> CapabilityHandle {
        self.receive
            .take()
            .expect("route ingress handle is present until consumed")
    }
}

impl Drop for RouteIngress {
    fn drop(&mut self) {
        if let Some(handle) = self.receive.take() {
            close_quietly(handle);
        }
    }
}

fn validate_request_envelope(
    granted_key: RouteKey,
    bytes: &[u8],
    sender_process_id: u64,
    capability_rights: Option<Rights>,
) -> Result<(), IngressError> {
    let message = RouteMessage::decode(bytes).map_err(IngressError::Decode)?;
    let requested = match message {
        RouteMessage::Request { key } => key,
        RouteMessage::Accepted { .. } | RouteMessage::Failure { .. } => {
            return Err(IngressError::UnexpectedMessage);
        }
    };
    if requested != granted_key {
        return Err(IngressError::WrongGrantedKey {
            granted: granted_key,
            requested,
        });
    }
    if sender_process_id == 0 {
        return Err(IngressError::ZeroSenderProcessId);
    }
    match capability_rights {
        None => Err(IngressError::MissingReplyCapability),
        Some(Rights::SEND) => Ok(()),
        Some(actual) => Err(IngressError::InvalidTransferredRights { actual }),
    }
}

fn validate_ingress_request(
    granted_key: RouteKey,
    ingress_object_id: u64,
    bytes: &[u8],
    sender_process_id: u64,
    capability: Option<ReceivedCapability>,
) -> Result<PendingRouteRequest, IngressError> {
    if let Err(error) = validate_request_envelope(
        granted_key,
        bytes,
        sender_process_id,
        capability.map(|received| received.rights),
    ) {
        close_received(capability);
        return Err(error);
    }
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

    Ok(PendingRouteRequest {
        key: granted_key,
        sender_process_id,
        reply_object_id: info.object_id,
        reply: Some(reply.handle),
    })
}

/// An authorization denial. The canonical unauthorized reply has already been attempted exactly
/// once, and the reply handle has been closed.
#[derive(Debug, PartialEq, Eq)]
pub struct AuthorizationError<E> {
    pub source: E,
    pub reply_error: Option<ipc::Error>,
}

/// A received request that has not yet crossed the authorization boundary.
pub struct PendingRouteRequest {
    key: RouteKey,
    sender_process_id: u64,
    reply_object_id: u64,
    reply: Option<CapabilityHandle>,
}

impl PendingRouteRequest {
    pub const fn key(&self) -> RouteKey {
        self.key
    }

    pub const fn sender_process_id(&self) -> u64 {
        self.sender_process_id
    }

    pub const fn reply_object_id(&self) -> u64 {
        self.reply_object_id
    }

    /// Authorizes the ingress-granted key before any route-table lookup can occur.
    ///
    /// A denial sends `RouteFailure::Unauthorized` without a capability and consumes the request.
    pub fn authorize<A>(
        mut self,
        authorizer: &mut A,
    ) -> Result<AuthorizedRouteRequest, AuthorizationError<A::Error>>
    where
        A: Authorizer<u64>,
    {
        match authorizer.authorize(&self.sender_process_id, self.key) {
            Ok(()) => Ok(AuthorizedRouteRequest {
                key: self.key,
                sender_process_id: self.sender_process_id,
                reply_object_id: self.reply_object_id,
                reply: self.reply.take(),
            }),
            Err(source) => {
                let reply_error = self
                    .reply_message(RouteMessage::Failure {
                        key: self.key,
                        failure: RouteFailure::Unauthorized,
                    })
                    .err();
                Err(AuthorizationError {
                    source,
                    reply_error,
                })
            }
        }
    }

    fn reply_message(&mut self, message: RouteMessage) -> ipc::Result<()> {
        send_reply_once(&mut self.reply, &message.encode(), None)
    }
}

impl Drop for PendingRouteRequest {
    fn drop(&mut self) {
        if let Some(handle) = self.reply.take() {
            close_quietly(handle);
        }
    }
}

/// The successful result of replying to an authorized request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteReply {
    Accepted { generation: ProviderGeneration },
    Failure(RouteFailure),
}

/// A local failure while issuing or sending a route reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyError {
    IssueProvider {
        source: ipc::Error,
        capacity_reply_error: Option<ipc::Error>,
    },
    Send {
        intended: RouteReply,
        source: ipc::Error,
    },
}

/// A request that is authorized and may therefore consult route availability.
pub struct AuthorizedRouteRequest {
    key: RouteKey,
    sender_process_id: u64,
    reply_object_id: u64,
    reply: Option<CapabilityHandle>,
}

impl AuthorizedRouteRequest {
    pub const fn key(&self) -> RouteKey {
        self.key
    }

    pub const fn sender_process_id(&self) -> u64 {
        self.sender_process_id
    }

    pub const fn reply_object_id(&self) -> u64 {
        self.reply_object_id
    }

    /// Resolves availability and replies exactly once without retaining a blocked reply handle.
    pub fn resolve<const N: usize>(
        mut self,
        routes: &NativeRouteTable<CapabilityHandle, N>,
    ) -> Result<RouteReply, ReplyError> {
        let Some(published) = routes.published(self.key) else {
            let intended = RouteReply::Failure(RouteFailure::Unavailable);
            return send_route_reply(
                &mut self.reply,
                RouteMessage::Failure {
                    key: self.key,
                    failure: RouteFailure::Unavailable,
                },
                None,
                intended,
            );
        };

        let temporary = match ipc::duplicate(*published.authority, DISPOSABLE_PROVIDER_RIGHTS) {
            Ok(handle) => handle,
            Err(source) => {
                let capacity_reply_error = send_reply_once(
                    &mut self.reply,
                    &RouteMessage::Failure {
                        key: self.key,
                        failure: RouteFailure::IssuerCapacity,
                    }
                    .encode(),
                    None,
                )
                .err();
                return Err(ReplyError::IssueProvider {
                    source,
                    capacity_reply_error,
                });
            }
        };

        let intended = RouteReply::Accepted {
            generation: published.generation,
        };
        send_route_reply(
            &mut self.reply,
            RouteMessage::Accepted {
                key: self.key,
                generation: published.generation,
            },
            Some(temporary),
            intended,
        )
    }
}

impl Drop for AuthorizedRouteRequest {
    fn drop(&mut self) {
        if let Some(handle) = self.reply.take() {
            close_quietly(handle);
        }
    }
}

fn send_route_reply(
    reply: &mut Option<CapabilityHandle>,
    message: RouteMessage,
    temporary: Option<CapabilityHandle>,
    intended: RouteReply,
) -> Result<RouteReply, ReplyError> {
    send_reply_once(reply, &message.encode(), temporary)
        .map(|()| intended)
        .map_err(|source| ReplyError::Send { intended, source })
}

fn send_reply_once(
    reply: &mut Option<CapabilityHandle>,
    bytes: &[u8; SERVICE_ROUTE_WIRE_BYTES],
    temporary: Option<CapabilityHandle>,
) -> ipc::Result<()> {
    let Some(reply_handle) = reply.take() else {
        if let Some(temporary) = temporary {
            close_quietly(temporary);
        }
        return Err(ipc::Error::BAD_FILE_DESCRIPTOR);
    };
    let transfer = temporary.map(|handle| Transfer {
        handle,
        rights: Rights::SEND,
    });
    let result = ipc::send(reply_handle, bytes, transfer);
    if let Some(temporary) = temporary {
        close_quietly(temporary);
    }
    close_quietly(reply_handle);
    result
}

/// Publication failure that preserves ownership of the submitted provider authority.
#[derive(Debug, PartialEq, Eq)]
pub enum NativePublishError<H> {
    Inspect {
        authority: H,
        source: ipc::Error,
    },
    InvalidAuthority {
        authority: H,
        source: EndpointShapeError,
    },
    Capacity {
        authority: H,
    },
    GenerationNotNewer {
        authority: H,
        current_generation: ProviderGeneration,
    },
}

impl<H> NativePublishError<H> {
    pub fn authority(&self) -> &H {
        match self {
            Self::Inspect { authority, .. }
            | Self::InvalidAuthority { authority, .. }
            | Self::Capacity { authority }
            | Self::GenerationNotNewer { authority, .. } => authority,
        }
    }

    pub fn into_authority(self) -> H {
        match self {
            Self::Inspect { authority, .. }
            | Self::InvalidAuthority { authority, .. }
            | Self::Capacity { authority }
            | Self::GenerationNotNewer { authority, .. } => authority,
        }
    }
}

/// Fixed-capacity native route publication table.
///
/// `service_route::RouteTable` remains the authority for strict generation ordering and permanent
/// tombstones. The wrapper tracks keys only so active owned handles can be closed on drop.
pub struct NativeRouteTable<H: Copy, const N: usize> {
    table: RouteTable<H, N>,
    keys: [Option<RouteKey>; N],
    close: fn(H),
}

impl<H: Copy, const N: usize> NativeRouteTable<H, N> {
    const fn with_closer(close: fn(H)) -> Self {
        Self {
            table: RouteTable::new(),
            keys: [None; N],
            close,
        }
    }

    pub const fn capacity(&self) -> usize {
        self.table.capacity()
    }

    pub const fn len(&self) -> usize {
        self.table.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    pub fn active_len(&self) -> usize {
        self.table.active_len()
    }

    pub fn generation(&self, key: RouteKey) -> Option<ProviderGeneration> {
        self.table.generation(key)
    }

    pub fn published(&self, key: RouteKey) -> Option<PublishedRoute<&H>> {
        self.table.get(key)
    }

    fn publish_prevalidated(
        &mut self,
        key: RouteKey,
        generation: ProviderGeneration,
        authority: H,
    ) -> Result<Option<H>, NativePublishError<H>> {
        let already_tracked = self.keys.contains(&Some(key));
        match self.table.publish(key, generation, authority) {
            Ok(displaced) => {
                if !already_tracked {
                    let slot = self
                        .keys
                        .iter_mut()
                        .find(|slot| slot.is_none())
                        .expect("core table and cleanup-key capacity diverged");
                    *slot = Some(key);
                }
                Ok(displaced)
            }
            Err(PublishError::Capacity { authority }) => {
                Err(NativePublishError::Capacity { authority })
            }
            Err(PublishError::GenerationNotNewer {
                authority,
                current_generation,
            }) => Err(NativePublishError::GenerationNotNewer {
                authority,
                current_generation,
            }),
        }
    }

    pub fn withdraw(
        &mut self,
        key: RouteKey,
        generation: ProviderGeneration,
    ) -> Result<H, WithdrawError> {
        self.table.withdraw(key, generation)
    }
}

impl<const N: usize> NativeRouteTable<CapabilityHandle, N> {
    pub const fn new() -> Self {
        Self::with_closer(close_native_handle)
    }

    /// Publishes an owned endpoint source with exactly `SEND | DUPLICATE | TRANSFER` rights.
    ///
    /// On success, any displaced authority is returned to the caller. On failure, the submitted
    /// authority remains owned by the error. Issuance never transfers this stable source directly.
    pub fn publish(
        &mut self,
        key: RouteKey,
        generation: ProviderGeneration,
        authority: CapabilityHandle,
    ) -> Result<Option<CapabilityHandle>, NativePublishError<CapabilityHandle>> {
        let info = ipc::info(authority)
            .map_err(|source| NativePublishError::Inspect { authority, source })?;
        validate_endpoint(info, PROVIDER_SOURCE_RIGHTS, false)
            .map_err(|source| NativePublishError::InvalidAuthority { authority, source })?;
        self.publish_prevalidated(key, generation, authority)
    }
}

impl<const N: usize> Default for NativeRouteTable<CapabilityHandle, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: Copy, const N: usize> Drop for NativeRouteTable<H, N> {
    fn drop(&mut self) {
        for key in self.keys.iter().flatten().copied() {
            let Some(published) = self.table.get(key) else {
                continue;
            };
            let generation = published.generation;
            if let Ok(authority) = self.table.withdraw(key, generation) {
                (self.close)(authority);
            }
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

fn close_native_handle(handle: CapabilityHandle) {
    close_quietly(handle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use service_route::{RoleId, ServiceId};

    const UUID: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];

    fn key(role: u32) -> RouteKey {
        RouteKey::new(
            ServiceId::from_bytes(UUID).unwrap(),
            RoleId::new(role).unwrap(),
        )
    }

    fn generation(value: u64) -> ProviderGeneration {
        ProviderGeneration::new(value).unwrap()
    }

    fn endpoint(object_id: u64, rights: Rights, size: u64) -> CapabilityInfo {
        CapabilityInfo {
            object_id,
            kind: ObjectKind::Endpoint,
            rights,
            size,
        }
    }

    #[test]
    fn generation_handoff_endpoints_have_exact_directional_rights() {
        assert_eq!(
            validate_endpoint(endpoint(1, Rights::ENDPOINT, 0), Rights::ENDPOINT, true),
            Ok(())
        );
        assert_eq!(
            validate_endpoint(endpoint(1, Rights::RECEIVE, 1), Rights::RECEIVE, false),
            Ok(())
        );
        assert_eq!(
            validate_endpoint(endpoint(1, Rights::SEND, 1), Rights::RECEIVE, false),
            Err(EndpointShapeError::WrongRights {
                expected: Rights::RECEIVE,
                actual: Rights::SEND,
            })
        );
    }

    #[test]
    fn generation_handoff_envelope_pins_sender_capability_count_and_codec() {
        let bytes = ServiceGenerationHandoff::new(generation(7)).encode();
        assert_eq!(
            validate_service_generation_envelope(&bytes, 1, 1, false),
            Ok(generation(7))
        );
        assert_eq!(
            validate_service_generation_envelope(&bytes, 2, 1, false),
            Err(GenerationHandoffReceiveError::UnexpectedSender {
                expected: 1,
                actual: 2,
            })
        );
        assert_eq!(
            validate_service_generation_envelope(&bytes, 1, 1, true),
            Err(GenerationHandoffReceiveError::UnexpectedCapability)
        );
        let mut malformed = bytes;
        malformed[6] = 1;
        assert_eq!(
            validate_service_generation_envelope(&malformed, 1, 1, false),
            Err(GenerationHandoffReceiveError::Decode(
                ServiceGenerationDecodeError::NonzeroReserved,
            ))
        );
    }

    #[test]
    fn route_grants_require_exact_send_and_a_nonzero_endpoint_object() {
        assert_eq!(
            validate_endpoint(endpoint(1, Rights::SEND, 4), Rights::SEND, false),
            Ok(())
        );
        assert_eq!(
            validate_endpoint(
                endpoint(1, Rights::SEND | Rights::TRANSFER, 0),
                Rights::SEND,
                false
            ),
            Err(EndpointShapeError::WrongRights {
                expected: Rights::SEND,
                actual: Rights::SEND | Rights::TRANSFER,
            })
        );
        assert_eq!(
            validate_endpoint(endpoint(0, Rights::SEND, 0), Rights::SEND, false),
            Err(EndpointShapeError::ZeroObjectId)
        );
    }

    #[test]
    fn reply_endpoints_require_an_empty_queue_when_inspectable() {
        assert_eq!(
            validate_endpoint(endpoint(2, Rights::SEND, 0), Rights::SEND, true),
            Ok(())
        );
        assert_eq!(
            validate_endpoint(endpoint(2, Rights::SEND, 1), Rights::SEND, true),
            Err(EndpointShapeError::NonEmptyQueue { messages: 1 })
        );
    }

    #[test]
    fn provider_publication_shape_is_exact_and_excludes_receive() {
        assert_eq!(
            validate_endpoint(
                endpoint(3, PROVIDER_SOURCE_RIGHTS, 7),
                PROVIDER_SOURCE_RIGHTS,
                false
            ),
            Ok(())
        );
        assert_eq!(
            validate_endpoint(
                endpoint(3, Rights::ENDPOINT, 0),
                PROVIDER_SOURCE_RIGHTS,
                false
            ),
            Err(EndpointShapeError::WrongRights {
                expected: PROVIDER_SOURCE_RIGHTS,
                actual: Rights::ENDPOINT,
            })
        );
        assert_eq!(DISPOSABLE_PROVIDER_RIGHTS, Rights::SEND | Rights::TRANSFER);
    }

    #[test]
    fn non_endpoints_are_rejected_even_with_matching_rights() {
        let info = CapabilityInfo {
            object_id: 4,
            kind: ObjectKind::Notification,
            rights: Rights::SEND,
            size: 0,
        };
        assert_eq!(
            validate_endpoint(info, Rights::SEND, false),
            Err(EndpointShapeError::WrongKind {
                actual: ObjectKind::Notification,
            })
        );
    }

    #[test]
    fn core_table_rules_drive_native_generation_and_tombstones() {
        let mut routes = NativeRouteTable::<u64, 1>::with_closer(|_| {});
        assert_eq!(
            routes.publish_prevalidated(key(1), generation(2), 20),
            Ok(None)
        );
        assert_eq!(
            routes.publish_prevalidated(key(1), generation(2), 21),
            Err(NativePublishError::GenerationNotNewer {
                authority: 21,
                current_generation: generation(2),
            })
        );
        assert_eq!(routes.withdraw(key(1), generation(2)), Ok(20));
        assert_eq!(routes.generation(key(1)), Some(generation(2)));
        assert_eq!(
            routes.publish_prevalidated(key(2), generation(1), 30),
            Err(NativePublishError::Capacity { authority: 30 })
        );
        assert_eq!(
            routes.publish_prevalidated(key(1), generation(3), 31),
            Ok(None)
        );
        assert_eq!(routes.withdraw(key(1), generation(3)), Ok(31));
    }

    #[test]
    fn replacement_returns_the_displaced_owned_authority() {
        let mut routes = NativeRouteTable::<u64, 1>::with_closer(|_| {});
        assert_eq!(
            routes.publish_prevalidated(key(1), generation(1), 10),
            Ok(None)
        );
        assert_eq!(
            routes.publish_prevalidated(key(1), generation(2), 20),
            Ok(Some(10))
        );
        assert_eq!(routes.active_len(), 1);
        assert_eq!(routes.withdraw(key(1), generation(2)), Ok(20));
    }

    #[test]
    fn zero_capacity_native_table_preserves_submitted_ownership() {
        let mut routes = NativeRouteTable::<u64, 0>::with_closer(|_| {});
        let error = routes
            .publish_prevalidated(key(1), generation(1), 9)
            .unwrap_err();
        assert_eq!(error.authority(), &9);
        assert_eq!(error.into_authority(), 9);
        assert!(routes.is_empty());
    }

    #[test]
    fn request_and_response_wire_shapes_remain_exactly_forty_bytes() {
        assert_eq!(SERVICE_ROUTE_WIRE_BYTES, 40);
        assert_eq!(RouteMessage::Request { key: key(1) }.encode().len(), 40);
        assert_eq!(
            RouteMessage::Accepted {
                key: key(1),
                generation: generation(1),
            }
            .encode()
            .len(),
            40
        );
    }

    #[test]
    fn accepted_and_failure_messages_echo_the_requested_key() {
        let accepted = RouteMessage::Accepted {
            key: key(1),
            generation: generation(8),
        };
        let failure = RouteMessage::Failure {
            key: key(1),
            failure: RouteFailure::Unavailable,
        };
        assert_eq!(
            RouteMessage::decode(&accepted.encode()).unwrap().key(),
            key(1)
        );
        assert_eq!(
            RouteMessage::decode(&failure.encode()).unwrap().key(),
            key(1)
        );
        assert_ne!(accepted.key(), key(2));
    }

    #[test]
    fn decode_enforces_nonzero_accepted_generation() {
        let mut accepted = RouteMessage::Accepted {
            key: key(1),
            generation: generation(1),
        }
        .encode();
        accepted[32..40].fill(0);
        assert_eq!(
            RouteMessage::decode(&accepted),
            Err(DecodeError::GenerationRequired)
        );
    }

    #[test]
    fn reply_outcomes_distinguish_acceptance_from_all_canonical_failures() {
        assert_ne!(
            RouteReply::Accepted {
                generation: generation(1)
            },
            RouteReply::Failure(RouteFailure::Unauthorized)
        );
        for failure in [
            RouteFailure::Unauthorized,
            RouteFailure::Unavailable,
            RouteFailure::IssuerCapacity,
        ] {
            let message = RouteMessage::Failure {
                key: key(1),
                failure,
            };
            assert_eq!(RouteMessage::decode(&message.encode()), Ok(message));
        }
    }

    #[test]
    fn client_response_envelope_requires_pid_echo_and_exact_capability_cardinality() {
        let accepted = RouteMessage::Accepted {
            key: key(1),
            generation: generation(5),
        }
        .encode();
        assert_eq!(
            validate_response_envelope(key(1), &accepted, 7, Some(Rights::SEND)),
            Ok(ResponseDisposition::Accepted(generation(5)))
        );
        assert_eq!(
            validate_response_envelope(key(1), &accepted, 0, Some(Rights::SEND)),
            Err(ResolveError::ZeroBrokerProcessId)
        );
        assert_eq!(
            validate_response_envelope(key(2), &accepted, 7, Some(Rights::SEND)),
            Err(ResolveError::KeyMismatch {
                expected: key(2),
                actual: key(1),
            })
        );
        assert_eq!(
            validate_response_envelope(key(1), &accepted, 7, None),
            Err(ResolveError::MissingProviderCapability)
        );
        assert_eq!(
            validate_response_envelope(key(1), &accepted, 7, Some(Rights::SEND | Rights::TRANSFER),),
            Err(ResolveError::InvalidProvider(
                EndpointShapeError::WrongRights {
                    expected: Rights::SEND,
                    actual: Rights::SEND | Rights::TRANSFER,
                }
            ))
        );
    }

    #[test]
    fn failure_responses_forbid_capabilities_and_requests_are_not_responses() {
        let failure = RouteMessage::Failure {
            key: key(1),
            failure: RouteFailure::Unavailable,
        }
        .encode();
        assert_eq!(
            validate_response_envelope(key(1), &failure, 9, None),
            Ok(ResponseDisposition::Failure(RouteFailure::Unavailable))
        );
        assert_eq!(
            validate_response_envelope(key(1), &failure, 9, Some(Rights::SEND)),
            Err(ResolveError::UnexpectedCapability)
        );
        assert_eq!(
            validate_response_envelope(
                key(1),
                &RouteMessage::Request { key: key(1) }.encode(),
                9,
                None,
            ),
            Err(ResolveError::UnexpectedResponse)
        );
    }

    #[test]
    fn ingress_envelope_requires_granted_key_sender_and_exact_send_reply() {
        let request = RouteMessage::Request { key: key(1) }.encode();
        assert_eq!(
            validate_request_envelope(key(1), &request, 11, Some(Rights::SEND)),
            Ok(())
        );
        assert_eq!(
            validate_request_envelope(key(2), &request, 11, Some(Rights::SEND)),
            Err(IngressError::WrongGrantedKey {
                granted: key(2),
                requested: key(1),
            })
        );
        assert_eq!(
            validate_request_envelope(key(1), &request, 0, Some(Rights::SEND)),
            Err(IngressError::ZeroSenderProcessId)
        );
        assert_eq!(
            validate_request_envelope(key(1), &request, 11, None),
            Err(IngressError::MissingReplyCapability)
        );
        assert_eq!(
            validate_request_envelope(key(1), &request, 11, Some(Rights::SEND | Rights::TRANSFER),),
            Err(IngressError::InvalidTransferredRights {
                actual: Rights::SEND | Rights::TRANSFER,
            })
        );
    }

    #[test]
    fn ingress_rejects_response_kinds_and_malformed_wire_before_availability() {
        let accepted = RouteMessage::Accepted {
            key: key(1),
            generation: generation(1),
        }
        .encode();
        assert_eq!(
            validate_request_envelope(key(1), &accepted, 11, Some(Rights::SEND)),
            Err(IngressError::UnexpectedMessage)
        );
        assert_eq!(
            validate_request_envelope(key(1), &accepted[..39], 11, Some(Rights::SEND)),
            Err(IngressError::Decode(DecodeError::InvalidLength))
        );
    }

    #[test]
    fn publication_errors_return_authority_for_every_failure_class() {
        let inspect = NativePublishError::Inspect {
            authority: 1_u64,
            source: ipc::Error::IO,
        };
        let invalid = NativePublishError::InvalidAuthority {
            authority: 2_u64,
            source: EndpointShapeError::ZeroObjectId,
        };
        let capacity = NativePublishError::Capacity { authority: 3_u64 };
        let stale = NativePublishError::GenerationNotNewer {
            authority: 4_u64,
            current_generation: generation(7),
        };
        assert_eq!(inspect.into_authority(), 1);
        assert_eq!(invalid.into_authority(), 2);
        assert_eq!(capacity.into_authority(), 3);
        assert_eq!(stale.into_authority(), 4);
    }

    #[test]
    fn malformed_lengths_are_rejected_by_the_core_codec() {
        let request = RouteMessage::Request { key: key(1) }.encode();
        assert_eq!(
            RouteMessage::decode(&request[..SERVICE_ROUTE_WIRE_BYTES - 1]),
            Err(DecodeError::InvalidLength)
        );
    }
}
