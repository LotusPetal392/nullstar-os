//! Canonical file-portal messages and trusted user-gesture admission.
//!
//! This module is policy, not a standalone security boundary. A portal service must pass the
//! kernel-stamped sender process ID to registration and admission, keep the registry private, and
//! configure its trusted gesture issuer from authenticated startup authority.

use crate::{
    application_identity::{ApplicationProfile, AuthorizedApplication},
    application_permission::{
        ApplicationGrantAuthorization, ApplicationGrantRights, ApplicationGrantScope,
        ApplicationGrantSubject, ApplicationResourceKind,
    },
};

pub const MAX_PORTAL_GESTURES: usize = 64;
pub const MAX_TRUSTED_GESTURE_LIFETIME_NS: u64 = 5_000_000_000;
pub const PORTAL_REQUEST_BYTES: usize = 64;
pub const PORTAL_RESPONSE_BYTES: usize = 64;
pub const PORTAL_GESTURE_TICKET_BYTES: usize = 96;
pub const PORTAL_PROTOCOL_VERSION: u16 = 1;

const REQUEST_MAGIC: [u8; 4] = *b"NSPR";
const RESPONSE_MAGIC: [u8; 4] = *b"NSPS";
const GESTURE_MAGIC: [u8; 4] = *b"NSGT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ApplicationPortalOperation {
    OpenFile = 1,
    SaveFile = 2,
    SelectDirectory = 3,
}

impl ApplicationPortalOperation {
    const fn from_raw(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::OpenFile),
            2 => Some(Self::SaveFile),
            3 => Some(Self::SelectDirectory),
            _ => None,
        }
    }

    pub const fn resource_kind(self) -> ApplicationResourceKind {
        match self {
            Self::OpenFile | Self::SaveFile => ApplicationResourceKind::File,
            Self::SelectDirectory => ApplicationResourceKind::Directory,
        }
    }

    const fn accepts_rights(self, rights: ApplicationGrantRights) -> bool {
        if !rights.valid_for(self.resource_kind()) {
            return false;
        }
        match self {
            Self::OpenFile => rights.contains(ApplicationGrantRights::READ),
            Self::SaveFile => rights.contains(ApplicationGrantRights::WRITE),
            Self::SelectDirectory => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationPortalRequest {
    request_id: u64,
    ticket_id: u64,
    parent_surface: u64,
    operation: ApplicationPortalOperation,
    rights: ApplicationGrantRights,
    scope: ApplicationGrantScope,
}

impl ApplicationPortalRequest {
    pub const fn new(
        request_id: u64,
        ticket_id: u64,
        parent_surface: u64,
        operation: ApplicationPortalOperation,
        rights: ApplicationGrantRights,
        scope: ApplicationGrantScope,
    ) -> Option<Self> {
        if request_id == 0
            || ticket_id == 0
            || parent_surface == 0
            || !operation.accepts_rights(rights)
        {
            return None;
        }
        Some(Self {
            request_id,
            ticket_id,
            parent_surface,
            operation,
            rights,
            scope,
        })
    }

    pub const fn request_id(self) -> u64 {
        self.request_id
    }

    pub const fn ticket_id(self) -> u64 {
        self.ticket_id
    }

    pub const fn parent_surface(self) -> u64 {
        self.parent_surface
    }

    pub const fn operation(self) -> ApplicationPortalOperation {
        self.operation
    }

    pub const fn rights(self) -> ApplicationGrantRights {
        self.rights
    }

    pub const fn scope(self) -> ApplicationGrantScope {
        self.scope
    }

    pub fn encode(self) -> [u8; PORTAL_REQUEST_BYTES] {
        let mut bytes = [0_u8; PORTAL_REQUEST_BYTES];
        bytes[0..4].copy_from_slice(&REQUEST_MAGIC);
        bytes[4..6].copy_from_slice(&PORTAL_PROTOCOL_VERSION.to_le_bytes());
        bytes[6] = self.operation as u8;
        bytes[7] = self.scope as u8;
        write_u64(&mut bytes, 8, self.request_id);
        write_u64(&mut bytes, 16, self.ticket_id);
        write_u64(&mut bytes, 24, self.parent_surface);
        bytes[32..34].copy_from_slice(&self.rights.bits().to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PortalRequestDecodeError> {
        if bytes.len() != PORTAL_REQUEST_BYTES {
            return Err(PortalRequestDecodeError::Length);
        }
        if bytes[0..4] != REQUEST_MAGIC {
            return Err(PortalRequestDecodeError::Magic);
        }
        if read_u16(bytes, 4) != PORTAL_PROTOCOL_VERSION {
            return Err(PortalRequestDecodeError::Version);
        }
        if bytes[34..].iter().any(|byte| *byte != 0) {
            return Err(PortalRequestDecodeError::Reserved);
        }
        let operation = ApplicationPortalOperation::from_raw(bytes[6])
            .ok_or(PortalRequestDecodeError::Operation)?;
        let scope = grant_scope_from_raw(bytes[7]).ok_or(PortalRequestDecodeError::Scope)?;
        let rights = ApplicationGrantRights::from_bits(read_u16(bytes, 32))
            .ok_or(PortalRequestDecodeError::Rights)?;
        Self::new(
            read_u64(bytes, 8),
            read_u64(bytes, 16),
            read_u64(bytes, 24),
            operation,
            rights,
            scope,
        )
        .ok_or(PortalRequestDecodeError::Canonical)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalRequestDecodeError {
    Length,
    Magic,
    Version,
    Operation,
    Scope,
    Rights,
    Reserved,
    Canonical,
}

/// A short-lived ticket emitted by the trusted desktop gesture authority.
///
/// The integer ID is not authority on its own. The portal admits the ticket only after receiving
/// this complete record from the configured issuer over authenticated IPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedUserGestureTicket {
    id: u64,
    target_process_id: u64,
    user: u64,
    session: u64,
    application: u64,
    installation: u64,
    surface: u64,
    seat: u64,
    event_sequence: u64,
    issued_at_ns: u64,
    expires_at_ns: u64,
}

impl TrustedUserGestureTicket {
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        id: u64,
        target_process_id: u64,
        user: u64,
        session: u64,
        application: u64,
        installation: u64,
        surface: u64,
        seat: u64,
        event_sequence: u64,
        issued_at_ns: u64,
        expires_at_ns: u64,
    ) -> Option<Self> {
        let lifetime = match expires_at_ns.checked_sub(issued_at_ns) {
            Some(lifetime) => lifetime,
            None => return None,
        };
        if id == 0
            || target_process_id == 0
            || user == 0
            || session == 0
            || application == 0
            || installation == 0
            || surface == 0
            || seat == 0
            || event_sequence == 0
            || lifetime == 0
            || lifetime > MAX_TRUSTED_GESTURE_LIFETIME_NS
        {
            return None;
        }
        Some(Self {
            id,
            target_process_id,
            user,
            session,
            application,
            installation,
            surface,
            seat,
            event_sequence,
            issued_at_ns,
            expires_at_ns,
        })
    }

    pub const fn id(self) -> u64 {
        self.id
    }

    pub const fn target_process_id(self) -> u64 {
        self.target_process_id
    }

    pub const fn surface(self) -> u64 {
        self.surface
    }

    pub const fn issued_at_ns(self) -> u64 {
        self.issued_at_ns
    }

    pub const fn expires_at_ns(self) -> u64 {
        self.expires_at_ns
    }

    pub fn encode(self) -> [u8; PORTAL_GESTURE_TICKET_BYTES] {
        let mut bytes = [0_u8; PORTAL_GESTURE_TICKET_BYTES];
        bytes[0..4].copy_from_slice(&GESTURE_MAGIC);
        bytes[4..6].copy_from_slice(&PORTAL_PROTOCOL_VERSION.to_le_bytes());
        for (index, value) in [
            self.id,
            self.target_process_id,
            self.user,
            self.session,
            self.application,
            self.installation,
            self.surface,
            self.seat,
            self.event_sequence,
            self.issued_at_ns,
            self.expires_at_ns,
        ]
        .into_iter()
        .enumerate()
        {
            write_u64(&mut bytes, 8 + index * 8, value);
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PortalGestureDecodeError> {
        if bytes.len() != PORTAL_GESTURE_TICKET_BYTES {
            return Err(PortalGestureDecodeError::Length);
        }
        if bytes[0..4] != GESTURE_MAGIC {
            return Err(PortalGestureDecodeError::Magic);
        }
        if read_u16(bytes, 4) != PORTAL_PROTOCOL_VERSION {
            return Err(PortalGestureDecodeError::Version);
        }
        if bytes[6] != 0 || bytes[7] != 0 {
            return Err(PortalGestureDecodeError::Reserved);
        }
        Self::new(
            read_u64(bytes, 8),
            read_u64(bytes, 16),
            read_u64(bytes, 24),
            read_u64(bytes, 32),
            read_u64(bytes, 40),
            read_u64(bytes, 48),
            read_u64(bytes, 56),
            read_u64(bytes, 64),
            read_u64(bytes, 72),
            read_u64(bytes, 80),
            read_u64(bytes, 88),
        )
        .ok_or(PortalGestureDecodeError::Canonical)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalGestureDecodeError {
    Length,
    Magic,
    Version,
    Reserved,
    Canonical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustedGestureState {
    Available,
    Consumed { transaction_id: u64 },
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedGestureRecord {
    ticket: TrustedUserGestureTicket,
    state: TrustedGestureState,
}

impl TrustedGestureRecord {
    pub const fn ticket(self) -> TrustedUserGestureTicket {
        self.ticket
    }

    pub const fn state(self) -> TrustedGestureState {
        self.state
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalTicketRegistrationError {
    UnauthorizedIssuer,
    NotYetValid,
    Expired,
    DuplicateTicket,
    DuplicateGesture,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalAdmissionError {
    InvalidClient,
    InvalidAuthorization,
    TransientPersistence,
    UnknownTicket,
    TicketReplayed,
    TicketNotYetValid,
    TicketExpired,
    ProcessMismatch,
    ApplicationMismatch,
    SessionMismatch,
    SurfaceMismatch,
    TransactionIdExhausted,
}

/// Opaque proof that one canonical portal request consumed one matching trusted gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmittedPortalRequest {
    transaction_id: u64,
    client_process_id: u64,
    authorization: AuthorizedApplication,
    subject: ApplicationGrantSubject,
    request: ApplicationPortalRequest,
}

impl AdmittedPortalRequest {
    pub const fn transaction_id(self) -> u64 {
        self.transaction_id
    }

    pub const fn client_process_id(self) -> u64 {
        self.client_process_id
    }

    pub const fn authorization(self) -> AuthorizedApplication {
        self.authorization
    }

    pub const fn subject(self) -> ApplicationGrantSubject {
        self.subject
    }

    pub const fn request(self) -> ApplicationPortalRequest {
        self.request
    }
}

/// Fixed-capacity trusted-gesture registry owned by the portal service.
pub struct ApplicationPortalAdmission {
    trusted_issuer_process_id: u64,
    records: [Option<TrustedGestureRecord>; MAX_PORTAL_GESTURES],
    next_transaction_id: u64,
}

impl ApplicationPortalAdmission {
    pub const fn new(trusted_issuer_process_id: u64) -> Option<Self> {
        if trusted_issuer_process_id == 0 {
            return None;
        }
        Some(Self {
            trusted_issuer_process_id,
            records: [None; MAX_PORTAL_GESTURES],
            next_transaction_id: 1,
        })
    }

    pub fn register_ticket(
        &mut self,
        authenticated_sender_process_id: u64,
        now_ns: u64,
        ticket: TrustedUserGestureTicket,
    ) -> Result<(), PortalTicketRegistrationError> {
        if authenticated_sender_process_id != self.trusted_issuer_process_id {
            return Err(PortalTicketRegistrationError::UnauthorizedIssuer);
        }
        if now_ns < ticket.issued_at_ns {
            return Err(PortalTicketRegistrationError::NotYetValid);
        }
        if now_ns > ticket.expires_at_ns {
            return Err(PortalTicketRegistrationError::Expired);
        }
        if self
            .records
            .iter()
            .flatten()
            .any(|record| record.ticket.id == ticket.id)
        {
            return Err(PortalTicketRegistrationError::DuplicateTicket);
        }
        if self.records.iter().flatten().any(|record| {
            record.ticket.seat == ticket.seat
                && record.ticket.event_sequence == ticket.event_sequence
        }) {
            return Err(PortalTicketRegistrationError::DuplicateGesture);
        }
        let Some(slot) = self.records.iter().position(Option::is_none) else {
            return Err(PortalTicketRegistrationError::Full);
        };
        self.records[slot] = Some(TrustedGestureRecord {
            ticket,
            state: TrustedGestureState::Available,
        });
        Ok(())
    }

    pub fn admit_request(
        &mut self,
        authenticated_client_process_id: u64,
        now_ns: u64,
        authorization: AuthorizedApplication,
        request: ApplicationPortalRequest,
    ) -> Result<AdmittedPortalRequest, PortalAdmissionError> {
        if authenticated_client_process_id == 0 {
            return Err(PortalAdmissionError::InvalidClient);
        }
        let subject = ApplicationGrantSubject::from_authorization(authorization)
            .ok_or(PortalAdmissionError::InvalidAuthorization)?;
        if authorization.profile() != ApplicationProfile::Desktop {
            return Err(PortalAdmissionError::InvalidAuthorization);
        }
        if request.scope == ApplicationGrantScope::Persistent && subject.is_transient() {
            return Err(PortalAdmissionError::TransientPersistence);
        }
        let Some(index) = self
            .records
            .iter()
            .position(|record| record.is_some_and(|record| record.ticket.id == request.ticket_id))
        else {
            return Err(PortalAdmissionError::UnknownTicket);
        };
        let record = self.records[index].expect("matched portal ticket exists");
        if record.state != TrustedGestureState::Available {
            return Err(PortalAdmissionError::TicketReplayed);
        }
        if now_ns < record.ticket.issued_at_ns {
            return Err(PortalAdmissionError::TicketNotYetValid);
        }
        if now_ns > record.ticket.expires_at_ns {
            self.records[index] = Some(TrustedGestureRecord {
                state: TrustedGestureState::Expired,
                ..record
            });
            return Err(PortalAdmissionError::TicketExpired);
        }
        if record.ticket.target_process_id != authenticated_client_process_id {
            return Err(PortalAdmissionError::ProcessMismatch);
        }
        let identity = authorization.identity();
        let principal = authorization.principal();
        let provenance = authorization.provenance();
        if record.ticket.application != principal.application
            || record.ticket.installation != provenance.installation
        {
            return Err(PortalAdmissionError::ApplicationMismatch);
        }
        if record.ticket.user != identity.user || record.ticket.session != identity.session {
            return Err(PortalAdmissionError::SessionMismatch);
        }
        if record.ticket.surface != request.parent_surface {
            return Err(PortalAdmissionError::SurfaceMismatch);
        }
        let transaction_id = self.next_transaction_id;
        let next_transaction_id = transaction_id
            .checked_add(1)
            .ok_or(PortalAdmissionError::TransactionIdExhausted)?;
        self.records[index] = Some(TrustedGestureRecord {
            state: TrustedGestureState::Consumed { transaction_id },
            ..record
        });
        self.next_transaction_id = next_transaction_id;
        Ok(AdmittedPortalRequest {
            transaction_id,
            client_process_id: authenticated_client_process_id,
            authorization,
            subject,
            request,
        })
    }

    pub const fn next_transaction_id(&self) -> u64 {
        self.next_transaction_id
    }

    pub fn records(&self) -> impl Iterator<Item = TrustedGestureRecord> + '_ {
        self.records.iter().flatten().copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ApplicationPortalStatus {
    Selected = 1,
    Cancelled = 2,
    Denied = 3,
    InvalidRequest = 4,
    Unavailable = 5,
}

impl ApplicationPortalStatus {
    const fn from_raw(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Selected),
            2 => Some(Self::Cancelled),
            3 => Some(Self::Denied),
            4 => Some(Self::InvalidRequest),
            5 => Some(Self::Unavailable),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationPortalResponse {
    status: ApplicationPortalStatus,
    request_id: u64,
    transaction_id: u64,
    grant_id: u64,
    grant_revision: u64,
    resource_kind: Option<ApplicationResourceKind>,
    rights: ApplicationGrantRights,
    scope: Option<ApplicationGrantScope>,
}

impl ApplicationPortalResponse {
    pub fn selected(
        admission: AdmittedPortalRequest,
        grant: ApplicationGrantAuthorization,
    ) -> Result<Self, PortalSelectionError> {
        let request = admission.request;
        if grant.subject() != admission.subject {
            return Err(PortalSelectionError::SubjectMismatch);
        }
        if grant.resource().kind() != request.operation.resource_kind() {
            return Err(PortalSelectionError::ResourceKindMismatch);
        }
        if grant.rights() != request.rights {
            return Err(PortalSelectionError::RightsMismatch);
        }
        if grant.scope() != request.scope {
            return Err(PortalSelectionError::ScopeMismatch);
        }
        Ok(Self {
            status: ApplicationPortalStatus::Selected,
            request_id: request.request_id,
            transaction_id: admission.transaction_id,
            grant_id: grant.grant_id(),
            grant_revision: grant.grant_revision(),
            resource_kind: Some(grant.resource().kind()),
            rights: grant.rights(),
            scope: Some(grant.scope()),
        })
    }

    pub const fn terminal(request_id: u64, status: ApplicationPortalStatus) -> Option<Self> {
        if request_id == 0 || matches!(status, ApplicationPortalStatus::Selected) {
            return None;
        }
        Some(Self {
            status,
            request_id,
            transaction_id: 0,
            grant_id: 0,
            grant_revision: 0,
            resource_kind: None,
            rights: ApplicationGrantRights::EMPTY,
            scope: None,
        })
    }

    pub const fn status(self) -> ApplicationPortalStatus {
        self.status
    }

    pub const fn request_id(self) -> u64 {
        self.request_id
    }

    pub const fn transaction_id(self) -> Option<u64> {
        if self.transaction_id == 0 {
            None
        } else {
            Some(self.transaction_id)
        }
    }

    pub const fn grant_id(self) -> Option<u64> {
        if self.grant_id == 0 {
            None
        } else {
            Some(self.grant_id)
        }
    }

    pub fn encode(self) -> [u8; PORTAL_RESPONSE_BYTES] {
        assert!(
            self.canonical(),
            "cannot encode a noncanonical portal response"
        );
        let mut bytes = [0_u8; PORTAL_RESPONSE_BYTES];
        bytes[0..4].copy_from_slice(&RESPONSE_MAGIC);
        bytes[4..6].copy_from_slice(&PORTAL_PROTOCOL_VERSION.to_le_bytes());
        bytes[6] = self.status as u8;
        bytes[7] = self.resource_kind.map_or(0, |kind| kind as u8);
        write_u64(&mut bytes, 8, self.request_id);
        write_u64(&mut bytes, 16, self.transaction_id);
        write_u64(&mut bytes, 24, self.grant_id);
        write_u64(&mut bytes, 32, self.grant_revision);
        bytes[40..42].copy_from_slice(&self.rights.bits().to_le_bytes());
        bytes[42] = self.scope.map_or(0, |scope| scope as u8);
        bytes[43] = u8::from(self.status == ApplicationPortalStatus::Selected);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PortalResponseDecodeError> {
        if bytes.len() != PORTAL_RESPONSE_BYTES {
            return Err(PortalResponseDecodeError::Length);
        }
        if bytes[0..4] != RESPONSE_MAGIC {
            return Err(PortalResponseDecodeError::Magic);
        }
        if read_u16(bytes, 4) != PORTAL_PROTOCOL_VERSION {
            return Err(PortalResponseDecodeError::Version);
        }
        if bytes[44..].iter().any(|byte| *byte != 0) {
            return Err(PortalResponseDecodeError::Reserved);
        }
        let status =
            ApplicationPortalStatus::from_raw(bytes[6]).ok_or(PortalResponseDecodeError::Status)?;
        let resource_kind = match bytes[7] {
            0 => None,
            1 => Some(ApplicationResourceKind::File),
            2 => Some(ApplicationResourceKind::Directory),
            _ => return Err(PortalResponseDecodeError::ResourceKind),
        };
        let rights = ApplicationGrantRights::from_bits(read_u16(bytes, 40))
            .ok_or(PortalResponseDecodeError::Rights)?;
        let scope = match bytes[42] {
            0 => None,
            raw => Some(grant_scope_from_raw(raw).ok_or(PortalResponseDecodeError::Scope)?),
        };
        let response = Self {
            status,
            request_id: read_u64(bytes, 8),
            transaction_id: read_u64(bytes, 16),
            grant_id: read_u64(bytes, 24),
            grant_revision: read_u64(bytes, 32),
            resource_kind,
            rights,
            scope,
        };
        if bytes[43] != u8::from(status == ApplicationPortalStatus::Selected)
            || !response.canonical()
        {
            return Err(PortalResponseDecodeError::Canonical);
        }
        Ok(response)
    }

    pub const fn validate_envelope(
        self,
        transferred_capabilities: usize,
    ) -> Result<(), PortalResponseEnvelopeError> {
        let expected = if matches!(self.status, ApplicationPortalStatus::Selected) {
            1
        } else {
            0
        };
        if transferred_capabilities == expected {
            Ok(())
        } else {
            Err(PortalResponseEnvelopeError::CapabilityCount {
                expected,
                actual: transferred_capabilities,
            })
        }
    }

    const fn canonical(self) -> bool {
        if self.request_id == 0 {
            return false;
        }
        if matches!(self.status, ApplicationPortalStatus::Selected) {
            let Some(kind) = self.resource_kind else {
                return false;
            };
            self.transaction_id != 0
                && self.grant_id != 0
                && self.grant_revision != 0
                && self.rights.valid_for(kind)
                && self.scope.is_some()
        } else {
            self.transaction_id == 0
                && self.grant_id == 0
                && self.grant_revision == 0
                && self.resource_kind.is_none()
                && self.rights.bits() == 0
                && self.scope.is_none()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalSelectionError {
    SubjectMismatch,
    ResourceKindMismatch,
    RightsMismatch,
    ScopeMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalResponseDecodeError {
    Length,
    Magic,
    Version,
    Status,
    ResourceKind,
    Rights,
    Scope,
    Reserved,
    Canonical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalResponseEnvelopeError {
    CapabilityCount { expected: usize, actual: usize },
}

const fn grant_scope_from_raw(value: u8) -> Option<ApplicationGrantScope> {
    match value {
        1 => Some(ApplicationGrantScope::Once),
        2 => Some(ApplicationGrantScope::Session),
        3 => Some(ApplicationGrantScope::Persistent),
        _ => None,
    }
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application_identity::{
            ApplicationInstallScope, ApplicationInstallation, ApplicationLaunchSelection,
            ApplicationProfileSet, ApplicationTrustClass, InstalledApplicationComponent,
            PackageVerification, authorize_application_launch,
        },
        application_permission::{ApplicationPermissionStore, ApplicationResourceIdentity},
    };

    const ISSUER_PROCESS: u64 = 401;
    const CLIENT_PROCESS: u64 = 501;
    const SURFACE: u64 = 601;

    fn authorization_with(
        application: u64,
        installation: u64,
        session: u64,
        trust_class: ApplicationTrustClass,
        scope: ApplicationInstallScope,
    ) -> AuthorizedApplication {
        let components = [InstalledApplicationComponent::new(
            21,
            b"/application",
            ApplicationProfileSet::DESKTOP,
            true,
        )];
        authorize_application_launch(
            PackageVerification {
                package: 11,
                package_generation: 12,
                application,
                publisher: 14,
                signing_lineage: 15,
                trust_class,
                system_application: false,
                components: &components,
            },
            ApplicationInstallation {
                installation,
                package: 11,
                package_generation: 12,
                application,
                publisher: 14,
                signing_lineage: 15,
                trust_class,
                scope,
                owner_user: 17,
                system_application: false,
            },
            ApplicationLaunchSelection {
                component: 21,
                user: 17,
                session,
                profile: ApplicationProfile::Desktop,
            },
        )
        .unwrap()
    }

    fn authorization() -> AuthorizedApplication {
        authorization_with(
            13,
            16,
            18,
            ApplicationTrustClass::Repository,
            ApplicationInstallScope::User,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn ticket(
        id: u64,
        target_process_id: u64,
        authorization: AuthorizedApplication,
        surface: u64,
        seat: u64,
        event_sequence: u64,
        issued_at_ns: u64,
        expires_at_ns: u64,
    ) -> TrustedUserGestureTicket {
        TrustedUserGestureTicket::new(
            id,
            target_process_id,
            authorization.identity().user,
            authorization.identity().session,
            authorization.principal().application,
            authorization.provenance().installation,
            surface,
            seat,
            event_sequence,
            issued_at_ns,
            expires_at_ns,
        )
        .unwrap()
    }

    fn open_request(ticket_id: u64, scope: ApplicationGrantScope) -> ApplicationPortalRequest {
        ApplicationPortalRequest::new(
            701,
            ticket_id,
            SURFACE,
            ApplicationPortalOperation::OpenFile,
            ApplicationGrantRights::READ,
            scope,
        )
        .unwrap()
    }

    fn admitted_request(
        scope: ApplicationGrantScope,
    ) -> (AuthorizedApplication, AdmittedPortalRequest) {
        let authorization = authorization();
        let ticket = ticket(801, CLIENT_PROCESS, authorization, SURFACE, 1, 1, 100, 200);
        let mut registry = ApplicationPortalAdmission::new(ISSUER_PROCESS).unwrap();
        registry
            .register_ticket(ISSUER_PROCESS, 100, ticket)
            .unwrap();
        let admitted = registry
            .admit_request(
                CLIENT_PROCESS,
                101,
                authorization,
                open_request(ticket.id(), scope),
            )
            .unwrap();
        (authorization, admitted)
    }

    #[test]
    fn requests_have_a_fixed_canonical_encoding_and_operation_specific_rights() {
        let request = open_request(801, ApplicationGrantScope::Persistent);
        let encoded = request.encode();
        let golden = [
            0x4e, 0x53, 0x50, 0x52, 0x01, 0x00, 0x01, 0x03, 0xbd, 0x02, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x21, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x59, 0x02, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(encoded, golden);
        assert_eq!(ApplicationPortalRequest::decode(&encoded), Ok(request));

        assert!(
            ApplicationPortalRequest::new(
                1,
                2,
                3,
                ApplicationPortalOperation::OpenFile,
                ApplicationGrantRights::WRITE,
                ApplicationGrantScope::Once,
            )
            .is_none()
        );
        assert!(
            ApplicationPortalRequest::new(
                1,
                2,
                3,
                ApplicationPortalOperation::SaveFile,
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Once,
            )
            .is_none()
        );
        assert!(
            ApplicationPortalRequest::new(
                1,
                2,
                3,
                ApplicationPortalOperation::SelectDirectory,
                ApplicationGrantRights::ENUMERATE | ApplicationGrantRights::CREATE,
                ApplicationGrantScope::Session,
            )
            .is_some()
        );

        let mut reserved = encoded;
        reserved[63] = 1;
        assert_eq!(
            ApplicationPortalRequest::decode(&reserved),
            Err(PortalRequestDecodeError::Reserved)
        );
        let mut invalid_rights = encoded;
        invalid_rights[32] = ApplicationGrantRights::WRITE.bits() as u8;
        assert_eq!(
            ApplicationPortalRequest::decode(&invalid_rights),
            Err(PortalRequestDecodeError::Canonical)
        );
    }

    #[test]
    fn gesture_tickets_are_bounded_canonical_and_round_trip() {
        let ticket = ticket(
            801,
            CLIENT_PROCESS,
            authorization(),
            SURFACE,
            2,
            3,
            100,
            200,
        );
        let encoded = ticket.encode();
        assert_eq!(encoded.len(), PORTAL_GESTURE_TICKET_BYTES);
        assert_eq!(&encoded[0..8], b"NSGT\x01\x00\x00\x00");
        assert_eq!(TrustedUserGestureTicket::decode(&encoded), Ok(ticket));

        assert!(TrustedUserGestureTicket::new(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 10).is_none());
        assert!(
            TrustedUserGestureTicket::new(
                1,
                2,
                3,
                4,
                5,
                6,
                7,
                8,
                9,
                10,
                10 + MAX_TRUSTED_GESTURE_LIFETIME_NS + 1,
            )
            .is_none()
        );
        let mut reserved = encoded;
        reserved[6] = 1;
        assert_eq!(
            TrustedUserGestureTicket::decode(&reserved),
            Err(PortalGestureDecodeError::Reserved)
        );
    }

    #[test]
    fn registration_requires_the_trusted_issuer_and_unique_event_provenance() {
        let authorization = authorization();
        let first = ticket(801, CLIENT_PROCESS, authorization, SURFACE, 2, 3, 100, 200);
        let mut registry = ApplicationPortalAdmission::new(ISSUER_PROCESS).unwrap();
        assert_eq!(
            registry.register_ticket(999, 100, first),
            Err(PortalTicketRegistrationError::UnauthorizedIssuer)
        );
        assert_eq!(
            registry.register_ticket(ISSUER_PROCESS, 99, first),
            Err(PortalTicketRegistrationError::NotYetValid)
        );
        registry
            .register_ticket(ISSUER_PROCESS, 100, first)
            .unwrap();
        assert_eq!(
            registry.register_ticket(ISSUER_PROCESS, 100, first),
            Err(PortalTicketRegistrationError::DuplicateTicket)
        );
        let cloned_event = ticket(802, CLIENT_PROCESS, authorization, SURFACE, 2, 3, 100, 200);
        assert_eq!(
            registry.register_ticket(ISSUER_PROCESS, 100, cloned_event),
            Err(PortalTicketRegistrationError::DuplicateGesture)
        );
    }

    #[test]
    fn admission_binds_process_application_session_and_surface_then_consumes_once() {
        let authorization = authorization();
        let ticket = ticket(801, CLIENT_PROCESS, authorization, SURFACE, 2, 3, 100, 200);
        let request = open_request(ticket.id(), ApplicationGrantScope::Persistent);

        for (client, authorized, surface, expected) in [
            (
                CLIENT_PROCESS + 1,
                authorization,
                SURFACE,
                PortalAdmissionError::ProcessMismatch,
            ),
            (
                CLIENT_PROCESS,
                authorization_with(
                    99,
                    16,
                    18,
                    ApplicationTrustClass::Repository,
                    ApplicationInstallScope::User,
                ),
                SURFACE,
                PortalAdmissionError::ApplicationMismatch,
            ),
            (
                CLIENT_PROCESS,
                authorization_with(
                    13,
                    16,
                    99,
                    ApplicationTrustClass::Repository,
                    ApplicationInstallScope::User,
                ),
                SURFACE,
                PortalAdmissionError::SessionMismatch,
            ),
            (
                CLIENT_PROCESS,
                authorization,
                SURFACE + 1,
                PortalAdmissionError::SurfaceMismatch,
            ),
        ] {
            let mut registry = ApplicationPortalAdmission::new(ISSUER_PROCESS).unwrap();
            registry
                .register_ticket(ISSUER_PROCESS, 100, ticket)
                .unwrap();
            let attempted = ApplicationPortalRequest::new(
                request.request_id(),
                request.ticket_id(),
                surface,
                request.operation(),
                request.rights(),
                request.scope(),
            )
            .unwrap();
            assert_eq!(
                registry.admit_request(client, 101, authorized, attempted),
                Err(expected)
            );
            assert_eq!(
                registry.records().next().unwrap().state(),
                TrustedGestureState::Available
            );
        }

        let mut registry = ApplicationPortalAdmission::new(ISSUER_PROCESS).unwrap();
        registry
            .register_ticket(ISSUER_PROCESS, 100, ticket)
            .unwrap();
        let admitted = registry
            .admit_request(CLIENT_PROCESS, 101, authorization, request)
            .unwrap();
        assert_eq!(admitted.transaction_id(), 1);
        assert_eq!(admitted.client_process_id(), CLIENT_PROCESS);
        assert_eq!(admitted.request(), request);
        assert_eq!(
            registry.admit_request(CLIENT_PROCESS, 102, authorization, request),
            Err(PortalAdmissionError::TicketReplayed)
        );
        assert_eq!(
            registry.records().next().unwrap().state(),
            TrustedGestureState::Consumed { transaction_id: 1 }
        );
    }

    #[test]
    fn expired_tickets_become_tombstones_and_cannot_be_replayed() {
        let authorization = authorization();
        let ticket = ticket(801, CLIENT_PROCESS, authorization, SURFACE, 2, 3, 100, 110);
        let request = open_request(ticket.id(), ApplicationGrantScope::Once);
        let mut registry = ApplicationPortalAdmission::new(ISSUER_PROCESS).unwrap();
        registry
            .register_ticket(ISSUER_PROCESS, 100, ticket)
            .unwrap();
        assert_eq!(
            registry.admit_request(CLIENT_PROCESS, 111, authorization, request),
            Err(PortalAdmissionError::TicketExpired)
        );
        assert_eq!(
            registry.records().next().unwrap().state(),
            TrustedGestureState::Expired
        );
        assert_eq!(
            registry.admit_request(CLIENT_PROCESS, 109, authorization, request),
            Err(PortalAdmissionError::TicketReplayed)
        );
    }

    #[test]
    fn persistent_admission_rejects_transient_installations_without_consuming() {
        let transient = authorization_with(
            13,
            16,
            18,
            ApplicationTrustClass::Transient,
            ApplicationInstallScope::Transient,
        );
        let ticket = ticket(801, CLIENT_PROCESS, transient, SURFACE, 2, 3, 100, 200);
        let mut registry = ApplicationPortalAdmission::new(ISSUER_PROCESS).unwrap();
        registry
            .register_ticket(ISSUER_PROCESS, 100, ticket)
            .unwrap();
        assert_eq!(
            registry.admit_request(
                CLIENT_PROCESS,
                101,
                transient,
                open_request(ticket.id(), ApplicationGrantScope::Persistent),
            ),
            Err(PortalAdmissionError::TransientPersistence)
        );
        assert_eq!(
            registry.records().next().unwrap().state(),
            TrustedGestureState::Available
        );
        assert!(
            registry
                .admit_request(
                    CLIENT_PROCESS,
                    101,
                    transient,
                    open_request(ticket.id(), ApplicationGrantScope::Session),
                )
                .is_ok()
        );
    }

    #[test]
    fn registry_capacity_and_transaction_exhaustion_fail_without_overwrite() {
        let authorization = authorization();
        let mut registry = ApplicationPortalAdmission::new(ISSUER_PROCESS).unwrap();
        for index in 0..MAX_PORTAL_GESTURES as u64 {
            registry
                .register_ticket(
                    ISSUER_PROCESS,
                    100,
                    ticket(
                        1_000 + index,
                        CLIENT_PROCESS,
                        authorization,
                        SURFACE,
                        1,
                        1_000 + index,
                        100,
                        200,
                    ),
                )
                .unwrap();
        }
        assert_eq!(registry.records().count(), MAX_PORTAL_GESTURES);
        assert_eq!(
            registry.register_ticket(
                ISSUER_PROCESS,
                100,
                ticket(
                    2_000,
                    CLIENT_PROCESS,
                    authorization,
                    SURFACE,
                    1,
                    2_000,
                    100,
                    200,
                ),
            ),
            Err(PortalTicketRegistrationError::Full)
        );

        let mut exhausted = ApplicationPortalAdmission::new(ISSUER_PROCESS).unwrap();
        let ticket = ticket(801, CLIENT_PROCESS, authorization, SURFACE, 2, 3, 100, 200);
        exhausted
            .register_ticket(ISSUER_PROCESS, 100, ticket)
            .unwrap();
        exhausted.next_transaction_id = u64::MAX;
        assert_eq!(
            exhausted.admit_request(
                CLIENT_PROCESS,
                101,
                authorization,
                open_request(ticket.id(), ApplicationGrantScope::Once),
            ),
            Err(PortalAdmissionError::TransactionIdExhausted)
        );
        assert_eq!(
            exhausted.records().next().unwrap().state(),
            TrustedGestureState::Available
        );
    }

    #[test]
    fn selected_responses_bind_admission_to_grant_and_capability_cardinality() {
        let (authorization, admitted) = admitted_request(ApplicationGrantScope::Persistent);
        let resource =
            ApplicationResourceIdentity::new([0x5a; 16], 901, 1, ApplicationResourceKind::File)
                .unwrap();
        let mut store = ApplicationPermissionStore::new();
        store
            .issue(
                authorization,
                resource,
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Persistent,
            )
            .unwrap();
        let grant = store
            .authorize(authorization, resource, ApplicationGrantRights::READ)
            .unwrap();
        let response = ApplicationPortalResponse::selected(admitted, grant).unwrap();
        let encoded = response.encode();
        assert_eq!(&encoded[0..8], b"NSPS\x01\x00\x01\x01");
        assert_eq!(ApplicationPortalResponse::decode(&encoded), Ok(response));
        assert_eq!(response.status(), ApplicationPortalStatus::Selected);
        assert_eq!(response.transaction_id(), Some(1));
        assert_eq!(response.grant_id(), Some(1));
        assert_eq!(response.validate_envelope(1), Ok(()));
        assert_eq!(
            response.validate_envelope(0),
            Err(PortalResponseEnvelopeError::CapabilityCount {
                expected: 1,
                actual: 0,
            })
        );

        let terminal =
            ApplicationPortalResponse::terminal(701, ApplicationPortalStatus::Cancelled).unwrap();
        assert_eq!(
            ApplicationPortalResponse::decode(&terminal.encode()),
            Ok(terminal)
        );
        assert_eq!(terminal.validate_envelope(0), Ok(()));
        assert!(
            ApplicationPortalResponse::terminal(701, ApplicationPortalStatus::Selected).is_none()
        );

        let mut leaked_payload = terminal.encode();
        leaked_payload[24] = 1;
        assert_eq!(
            ApplicationPortalResponse::decode(&leaked_payload),
            Err(PortalResponseDecodeError::Canonical)
        );
    }

    #[test]
    fn selected_response_rejects_cross_subject_kind_rights_and_scope_mixups() {
        let (authorization, admitted) = admitted_request(ApplicationGrantScope::Persistent);
        let mut store = ApplicationPermissionStore::new();
        let directory = ApplicationResourceIdentity::new(
            [0x5a; 16],
            901,
            1,
            ApplicationResourceKind::Directory,
        )
        .unwrap();
        store
            .issue(
                authorization,
                directory,
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Persistent,
            )
            .unwrap();
        let directory_grant = store
            .authorize(authorization, directory, ApplicationGrantRights::READ)
            .unwrap();
        assert_eq!(
            ApplicationPortalResponse::selected(admitted, directory_grant),
            Err(PortalSelectionError::ResourceKindMismatch)
        );

        let file =
            ApplicationResourceIdentity::new([0x5a; 16], 902, 1, ApplicationResourceKind::File)
                .unwrap();
        let other = authorization_with(
            99,
            99,
            18,
            ApplicationTrustClass::Repository,
            ApplicationInstallScope::User,
        );
        let mut other_store = ApplicationPermissionStore::new();
        other_store
            .issue(
                other,
                file,
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Persistent,
            )
            .unwrap();
        let other_grant = other_store
            .authorize(other, file, ApplicationGrantRights::READ)
            .unwrap();
        assert_eq!(
            ApplicationPortalResponse::selected(admitted, other_grant),
            Err(PortalSelectionError::SubjectMismatch)
        );

        let mut scope_store = ApplicationPermissionStore::new();
        scope_store
            .issue(
                authorization,
                file,
                ApplicationGrantRights::READ | ApplicationGrantRights::WRITE,
                ApplicationGrantScope::Session,
            )
            .unwrap();
        let reduced_grant = scope_store
            .authorize(authorization, file, ApplicationGrantRights::READ)
            .unwrap();
        assert_eq!(
            ApplicationPortalResponse::selected(admitted, reduced_grant),
            Err(PortalSelectionError::ScopeMismatch)
        );
    }
}
