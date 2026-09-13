//! Capability transport for the application file portal.
//!
//! The portal keeps application requests and trusted compositor gestures on separate ingress
//! objects. Kernel-stamped sender IDs select manager-installed application bindings and authenticate
//! gesture tickets; payload process IDs never substitute for that transport identity.

use crate::{
    abi::limits,
    application_identity::{ApplicationProfile, AuthorizedApplication},
    application_permission::ApplicationGrantSubject,
    application_permission_persistence::{
        ApplicationPermissionCommit, ApplicationPermissionPersistence,
    },
    application_portal::{
        AdmittedPortalRequest, ApplicationPortalAdmission, ApplicationPortalRequest,
        ApplicationPortalStatus, PORTAL_GESTURE_TICKET_BYTES, PORTAL_REQUEST_BYTES,
        PortalAdmissionError, PortalRequestDecodeError, PortalTicketRegistrationError,
        TrustedUserGestureTicket,
    },
    application_resource::ApplicationResourceBroker,
    application_selection::{
        ApplicationSelectionCompletionError, ApplicationSelectionDurableCompletionError,
        DurableApplicationSelection, PreparedApplicationSelection,
    },
    handle::{
        BorrowedHandle, Endpoint, OwnedHandle, ReceivedCapability, ReceivedMessage, SendMoveError,
    },
    ipc::{self, ObjectKind, Rights},
};

pub const MAX_APPLICATION_PORTAL_CLIENTS: usize = 64;
pub const APPLICATION_PORTAL_INGRESS_RIGHTS: Rights = Rights::RECEIVE.union(Rights::WAIT);
pub const APPLICATION_PORTAL_CLIENT_RIGHTS: Rights = Rights::SEND;
pub const APPLICATION_PORTAL_CLIENT_SOURCE_RIGHTS: Rights = Rights::SEND
    .union(Rights::DUPLICATE)
    .union(Rights::TRANSFER);
pub const APPLICATION_PORTAL_GESTURE_SOURCE_RIGHTS: Rights = Rights::SEND.union(Rights::TRANSFER);
pub const APPLICATION_PORTAL_REPLY_RIGHTS: Rights = Rights::SEND;
pub const APPLICATION_PORTAL_REPLY_SOURCE_RIGHTS: Rights = Rights::SEND.union(Rights::TRANSFER);
pub const APPLICATION_PORTAL_REPLY_RECEIVER_RIGHTS: Rights = Rights::RECEIVE.union(Rights::WAIT);

const _: () = assert!(PORTAL_REQUEST_BYTES <= limits::MAX_IPC_MESSAGE_BYTES);
const _: () = assert!(PORTAL_GESTURE_TICKET_BYTES <= limits::MAX_IPC_MESSAGE_BYTES);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPortalTransportCreateError {
    InvalidApplicationManager,
    InvalidGestureIssuer,
    Endpoint(ipc::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPortalClientBindingError {
    UnauthorizedManager,
    InvalidClientProcess,
    InvalidAuthorization,
    AlreadyBound,
    NotBound,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPortalGestureReceiveError {
    Receive(ipc::Error),
    UnexpectedCapability,
    Decode(crate::application_portal::PortalGestureDecodeError),
    Registration(PortalTicketRegistrationError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPortalRequestReceiveError {
    Receive(ipc::Error),
    ZeroSenderProcessId,
    Decode(PortalRequestDecodeError),
    UnauthorizedClient,
    MissingReplyEndpoint,
    InvalidReplyRights { actual: Rights },
    InspectReply(ipc::Error),
    InvalidReplyKind,
    NonEmptyReplyEndpoint,
    ReplyAliasesIngress,
    Admission(PortalAdmissionError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPortalTerminalReplyError {
    SelectedIsNotTerminal,
    Transfer(ipc::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApplicationPortalClientBinding {
    process_id: u64,
    authorization: AuthorizedApplication,
}

/// Manager-held source from which exact send-only application endpoints are issued.
pub struct ApplicationPortalClientSource {
    endpoint: OwnedHandle<Endpoint>,
}

impl ApplicationPortalClientSource {
    pub fn issue_client(&self) -> ipc::Result<ApplicationPortalClientEndpoint> {
        Ok(ApplicationPortalClientEndpoint {
            endpoint: self.endpoint.duplicate(APPLICATION_PORTAL_CLIENT_RIGHTS)?,
        })
    }

    pub const fn endpoint(&self) -> &OwnedHandle<Endpoint> {
        &self.endpoint
    }

    pub fn into_endpoint(self) -> OwnedHandle<Endpoint> {
        self.endpoint
    }
}

/// Exact send-only endpoint installed in one authorized desktop application.
pub struct ApplicationPortalClientEndpoint {
    endpoint: OwnedHandle<Endpoint>,
}

impl ApplicationPortalClientEndpoint {
    pub const fn endpoint(&self) -> &OwnedHandle<Endpoint> {
        &self.endpoint
    }

    pub fn into_endpoint(self) -> OwnedHandle<Endpoint> {
        self.endpoint
    }

    /// Move-sends a canonical request with a reply source reduced to exact `SEND` in transit.
    pub fn send_request(
        &self,
        request: ApplicationPortalRequest,
        reply: ApplicationPortalReplySource,
    ) -> Result<(), SendMoveError<Endpoint>> {
        self.endpoint.send_move(
            &request.encode(),
            reply.into_endpoint(),
            APPLICATION_PORTAL_REPLY_RIGHTS,
        )
    }
}

/// Application-owned receive side of one portal request's reply channel.
pub struct ApplicationPortalReplyReceiver {
    endpoint: OwnedHandle<Endpoint>,
}

impl ApplicationPortalReplyReceiver {
    pub fn mint() -> ipc::Result<(Self, ApplicationPortalReplySource)> {
        let (mut receiver, mut source) = OwnedHandle::<Endpoint>::create_pair()?;
        receiver.replace_rights(APPLICATION_PORTAL_REPLY_RECEIVER_RIGHTS)?;
        source.replace_rights(APPLICATION_PORTAL_REPLY_SOURCE_RIGHTS)?;
        Ok((
            Self { endpoint: receiver },
            ApplicationPortalReplySource { endpoint: source },
        ))
    }

    pub const fn endpoint(&self) -> &OwnedHandle<Endpoint> {
        &self.endpoint
    }

    pub fn try_receive(&self, output: &mut [u8]) -> ipc::Result<ReceivedMessage> {
        self.endpoint.try_receive(output)
    }

    pub fn into_endpoint(self) -> OwnedHandle<Endpoint> {
        self.endpoint
    }
}

/// Transfer-capable source that becomes exact send-only authority at the portal.
pub struct ApplicationPortalReplySource {
    endpoint: OwnedHandle<Endpoint>,
}

impl ApplicationPortalReplySource {
    pub const fn endpoint(&self) -> &OwnedHandle<Endpoint> {
        &self.endpoint
    }

    pub fn into_endpoint(self) -> OwnedHandle<Endpoint> {
        self.endpoint
    }
}

/// Transferable compositor endpoint for canonical trusted-gesture tickets.
pub struct ApplicationPortalGestureSource {
    endpoint: OwnedHandle<Endpoint>,
}

impl ApplicationPortalGestureSource {
    pub const fn endpoint(&self) -> &OwnedHandle<Endpoint> {
        &self.endpoint
    }

    pub fn send_ticket(&self, ticket: TrustedUserGestureTicket) -> ipc::Result<()> {
        self.endpoint.send(&ticket.encode())
    }

    pub fn into_endpoint(self) -> OwnedHandle<Endpoint> {
        self.endpoint
    }
}

/// One admitted request and the exact send-only endpoint on which it must finish.
pub struct PendingApplicationPortalRequest {
    admission: AdmittedPortalRequest,
    reply: OwnedHandle<Endpoint>,
}

impl PendingApplicationPortalRequest {
    pub const fn admission(&self) -> AdmittedPortalRequest {
        self.admission
    }

    pub const fn request(&self) -> ApplicationPortalRequest {
        self.admission.request()
    }

    pub fn reply_endpoint(&self) -> BorrowedHandle<'_, Endpoint> {
        self.reply.borrow()
    }

    /// Sends a capability-free terminal response and consumes the reply authority.
    pub fn reply_terminal(
        self,
        status: ApplicationPortalStatus,
    ) -> Result<(), ApplicationPortalTerminalReplyError> {
        let Some(response) = crate::application_portal::ApplicationPortalResponse::terminal(
            self.request().request_id(),
            status,
        ) else {
            return Err(ApplicationPortalTerminalReplyError::SelectedIsNotTerminal);
        };
        self.reply
            .send(&response.encode())
            .map_err(ApplicationPortalTerminalReplyError::Transfer)
    }

    /// Completes a prepared selection on this request's reply channel.
    pub fn complete_selection<'a>(
        self,
        selection: PreparedApplicationSelection<'a>,
    ) -> Result<ApplicationResourceBroker, ApplicationSelectionCompletionError> {
        selection.complete(self.reply.borrow())
    }

    /// Completes a selection and returns its broker only after the permission snapshot is durable.
    pub fn complete_durable_selection<'a, B: ApplicationPermissionPersistence>(
        self,
        selection: PreparedApplicationSelection<'a>,
        backend: &mut B,
        previous: Option<ApplicationPermissionCommit>,
    ) -> Result<DurableApplicationSelection, ApplicationSelectionDurableCompletionError<B::Error>>
    {
        selection.complete_durable(self.reply.borrow(), backend, previous)
    }
}

/// Portal-owned transport state. Startup authority supplies both trusted process IDs.
pub struct ApplicationPortalTransport {
    application_manager_process_id: u64,
    request_ingress: OwnedHandle<Endpoint>,
    request_ingress_object_id: u64,
    gesture_ingress: OwnedHandle<Endpoint>,
    admission: ApplicationPortalAdmission,
    clients: [Option<ApplicationPortalClientBinding>; MAX_APPLICATION_PORTAL_CLIENTS],
}

impl ApplicationPortalTransport {
    pub fn mint(
        application_manager_process_id: u64,
        trusted_gesture_issuer_process_id: u64,
    ) -> Result<
        (
            Self,
            ApplicationPortalClientSource,
            ApplicationPortalGestureSource,
        ),
        ApplicationPortalTransportCreateError,
    > {
        if application_manager_process_id == 0 {
            return Err(ApplicationPortalTransportCreateError::InvalidApplicationManager);
        }
        let Some(admission) = ApplicationPortalAdmission::new(trusted_gesture_issuer_process_id)
        else {
            return Err(ApplicationPortalTransportCreateError::InvalidGestureIssuer);
        };

        let (mut request_ingress, mut client_source) = OwnedHandle::<Endpoint>::create_pair()
            .map_err(ApplicationPortalTransportCreateError::Endpoint)?;
        request_ingress
            .replace_rights(APPLICATION_PORTAL_INGRESS_RIGHTS)
            .map_err(ApplicationPortalTransportCreateError::Endpoint)?;
        client_source
            .replace_rights(APPLICATION_PORTAL_CLIENT_SOURCE_RIGHTS)
            .map_err(ApplicationPortalTransportCreateError::Endpoint)?;
        let request_ingress_object_id = request_ingress
            .info()
            .map_err(ApplicationPortalTransportCreateError::Endpoint)?
            .object_id;

        let (mut gesture_ingress, mut gesture_source) = OwnedHandle::<Endpoint>::create_pair()
            .map_err(ApplicationPortalTransportCreateError::Endpoint)?;
        gesture_ingress
            .replace_rights(APPLICATION_PORTAL_INGRESS_RIGHTS)
            .map_err(ApplicationPortalTransportCreateError::Endpoint)?;
        gesture_source
            .replace_rights(APPLICATION_PORTAL_GESTURE_SOURCE_RIGHTS)
            .map_err(ApplicationPortalTransportCreateError::Endpoint)?;

        Ok((
            Self {
                application_manager_process_id,
                request_ingress,
                request_ingress_object_id,
                gesture_ingress,
                admission,
                clients: [None; MAX_APPLICATION_PORTAL_CLIENTS],
            },
            ApplicationPortalClientSource {
                endpoint: client_source,
            },
            ApplicationPortalGestureSource {
                endpoint: gesture_source,
            },
        ))
    }

    pub const fn application_manager_process_id(&self) -> u64 {
        self.application_manager_process_id
    }

    pub const fn request_ingress(&self) -> &OwnedHandle<Endpoint> {
        &self.request_ingress
    }

    pub const fn gesture_ingress(&self) -> &OwnedHandle<Endpoint> {
        &self.gesture_ingress
    }

    pub fn client_authorization(&self, process_id: u64) -> Option<AuthorizedApplication> {
        self.clients
            .iter()
            .flatten()
            .find(|binding| binding.process_id == process_id)
            .map(|binding| binding.authorization)
    }

    /// Installs one process binding only when invoked for the kernel-authenticated manager.
    pub fn bind_client(
        &mut self,
        authenticated_manager_process_id: u64,
        process_id: u64,
        authorization: AuthorizedApplication,
    ) -> Result<(), ApplicationPortalClientBindingError> {
        if authenticated_manager_process_id != self.application_manager_process_id {
            return Err(ApplicationPortalClientBindingError::UnauthorizedManager);
        }
        if process_id == 0 {
            return Err(ApplicationPortalClientBindingError::InvalidClientProcess);
        }
        if authorization.profile() != ApplicationProfile::Desktop
            || ApplicationGrantSubject::from_authorization(authorization).is_none()
        {
            return Err(ApplicationPortalClientBindingError::InvalidAuthorization);
        }
        if self.client_authorization(process_id).is_some() {
            return Err(ApplicationPortalClientBindingError::AlreadyBound);
        }
        let Some(slot) = self.clients.iter_mut().find(|slot| slot.is_none()) else {
            return Err(ApplicationPortalClientBindingError::Full);
        };
        *slot = Some(ApplicationPortalClientBinding {
            process_id,
            authorization,
        });
        Ok(())
    }

    pub fn unbind_client(
        &mut self,
        authenticated_manager_process_id: u64,
        process_id: u64,
    ) -> Result<AuthorizedApplication, ApplicationPortalClientBindingError> {
        if authenticated_manager_process_id != self.application_manager_process_id {
            return Err(ApplicationPortalClientBindingError::UnauthorizedManager);
        }
        let Some(slot) = self
            .clients
            .iter_mut()
            .find(|slot| slot.is_some_and(|binding| binding.process_id == process_id))
        else {
            return Err(ApplicationPortalClientBindingError::NotBound);
        };
        Ok(slot
            .take()
            .expect("matched portal client binding")
            .authorization)
    }

    /// Consumes at most one compositor message, preserving `TRY_AGAIN` as an empty poll.
    pub fn try_receive_gesture(
        &mut self,
        now_ns: u64,
    ) -> Result<Option<TrustedUserGestureTicket>, ApplicationPortalGestureReceiveError> {
        let mut bytes = [0_u8; limits::MAX_IPC_MESSAGE_BYTES];
        let message = match self.gesture_ingress.try_receive(&mut bytes) {
            Ok(message) => message,
            Err(error) if error == ipc::Error::TRY_AGAIN => return Ok(None),
            Err(error) => return Err(ApplicationPortalGestureReceiveError::Receive(error)),
        };
        if message.capability.is_some() {
            return Err(ApplicationPortalGestureReceiveError::UnexpectedCapability);
        }
        let ticket = TrustedUserGestureTicket::decode(&bytes[..message.bytes])
            .map_err(ApplicationPortalGestureReceiveError::Decode)?;
        self.admission
            .register_ticket(message.sender_process_id, now_ns, ticket)
            .map_err(ApplicationPortalGestureReceiveError::Registration)?;
        Ok(Some(ticket))
    }

    /// Consumes at most one application request and adopts its validated reply endpoint.
    pub fn try_receive_request(
        &mut self,
        now_ns: u64,
    ) -> Result<Option<PendingApplicationPortalRequest>, ApplicationPortalRequestReceiveError> {
        let mut bytes = [0_u8; limits::MAX_IPC_MESSAGE_BYTES];
        let message = match self.request_ingress.try_receive(&mut bytes) {
            Ok(message) => message,
            Err(error) if error == ipc::Error::TRY_AGAIN => return Ok(None),
            Err(error) => return Err(ApplicationPortalRequestReceiveError::Receive(error)),
        };
        if message.sender_process_id == 0 {
            return Err(ApplicationPortalRequestReceiveError::ZeroSenderProcessId);
        }
        let request = ApplicationPortalRequest::decode(&bytes[..message.bytes])
            .map_err(ApplicationPortalRequestReceiveError::Decode)?;
        let Some(authorization) = self.client_authorization(message.sender_process_id) else {
            return Err(ApplicationPortalRequestReceiveError::UnauthorizedClient);
        };
        let reply = validate_reply_endpoint(self.request_ingress_object_id, message.capability)?;
        let admission = self
            .admission
            .admit_request(message.sender_process_id, now_ns, authorization, request)
            .map_err(ApplicationPortalRequestReceiveError::Admission)?;
        Ok(Some(PendingApplicationPortalRequest { admission, reply }))
    }
}

fn validate_reply_endpoint(
    request_ingress_object_id: u64,
    capability: Option<ReceivedCapability>,
) -> Result<OwnedHandle<Endpoint>, ApplicationPortalRequestReceiveError> {
    let Some(capability) = capability else {
        return Err(ApplicationPortalRequestReceiveError::MissingReplyEndpoint);
    };
    if capability.rights != APPLICATION_PORTAL_REPLY_RIGHTS {
        return Err(ApplicationPortalRequestReceiveError::InvalidReplyRights {
            actual: capability.rights,
        });
    }
    let info = capability
        .handle
        .info()
        .map_err(ApplicationPortalRequestReceiveError::InspectReply)?;
    if info.kind != ObjectKind::Endpoint {
        return Err(ApplicationPortalRequestReceiveError::InvalidReplyKind);
    }
    if info.rights != APPLICATION_PORTAL_REPLY_RIGHTS {
        return Err(ApplicationPortalRequestReceiveError::InvalidReplyRights {
            actual: info.rights,
        });
    }
    if info.size != 0 {
        return Err(ApplicationPortalRequestReceiveError::NonEmptyReplyEndpoint);
    }
    if info.object_id == request_ingress_object_id {
        return Err(ApplicationPortalRequestReceiveError::ReplyAliasesIngress);
    }
    capability
        .handle
        .try_cast::<Endpoint>()
        .map_err(|(error, _)| ApplicationPortalRequestReceiveError::InspectReply(error))
}
