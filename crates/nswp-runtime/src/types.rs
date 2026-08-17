use nswp_core::{
    AvailableFeature, BodyError, BoundProtocol, ConnectionLimits, FeatureRecord,
    FeatureSetValidator, MinorVersionProfile, ProtocolId, SelectedFeatures,
};

pub const MAX_BODY_BYTES: usize = 192;
pub const MAX_PACKET_BYTES: usize = nswp_core::NSWP_HEADER_BYTES + MAX_BODY_BYTES;
pub const MAX_OUTSTANDING: usize = 8;
pub const MAX_RECENTLY_CANCELED: usize = 8;
pub const MAX_NEGOTIATED_FEATURES: usize = 16;

pub const HANDLE_FREE_ENDPOINT_LIMITS: ConnectionLimits = ConnectionLimits {
    max_body_bytes: MAX_BODY_BYTES as u32,
    max_handles: 0,
    max_outstanding: MAX_OUTSTANDING as u16,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacketBuf {
    bytes: [u8; MAX_PACKET_BYTES],
    len: u16,
}

impl PacketBuf {
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_PACKET_BYTES],
            len: 0,
        }
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self, RuntimeError> {
        if bytes.len() > MAX_PACKET_BYTES {
            return Err(RuntimeError::PacketTooLarge { bytes: bytes.len() });
        }
        let mut packet = Self::new();
        packet.bytes[..bytes.len()].copy_from_slice(bytes);
        packet.len = bytes.len() as u16;
        Ok(packet)
    }

    pub const fn len(&self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len()]
    }

    pub fn as_mut_capacity(&mut self) -> &mut [u8; MAX_PACKET_BYTES] {
        &mut self.bytes
    }

    pub(crate) fn set_len(&mut self, len: usize) -> Result<(), RuntimeError> {
        if len > MAX_PACKET_BYTES {
            return Err(RuntimeError::PacketTooLarge { bytes: len });
        }
        self.len = len as u16;
        Ok(())
    }
}

impl Default for PacketBuf {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyBuf {
    bytes: [u8; MAX_BODY_BYTES],
    len: u8,
}

impl BodyBuf {
    pub const fn new() -> Self {
        Self {
            bytes: [0; MAX_BODY_BYTES],
            len: 0,
        }
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self, RuntimeError> {
        if bytes.len() > MAX_BODY_BYTES {
            return Err(RuntimeError::BodyTooLarge { bytes: bytes.len() });
        }
        let mut body = Self::new();
        body.bytes[..bytes.len()].copy_from_slice(bytes);
        body.len = bytes.len() as u8;
        Ok(body)
    }

    pub const fn len(&self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len()]
    }
}

impl Default for BodyBuf {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrySendError {
    Full,
    PeerClosed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TryRecvError {
    Empty,
    PeerClosed,
    MessageTooLarge { bytes: usize },
}

pub trait TryTransport {
    fn try_send(&mut self, packet: &[u8]) -> Result<(), TrySendError>;
    fn try_recv(&mut self, output: &mut [u8]) -> Result<usize, TryRecvError>;
    fn close(&mut self);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerContextId(u64);

impl PeerContextId {
    pub const UNSPECIFIED: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeadlinePolicy {
    Required { max_duration_ns: Option<u64> },
    Optional { max_duration_ns: Option<u64> },
    Forbidden,
}

pub type BodyValidator = fn(&[u8], &BoundProtocol<'_>) -> Result<(), BodyError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodKind {
    RequestResponse,
    OneWay,
}

/// Maximum disclosure permitted for one encoded message in diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessagePrivacy {
    /// Structural metadata and correlation identifiers may be retained.
    Public,
    /// Payload remains private to the owning user or application boundary.
    Private,
    /// Payload requires privileged diagnostic access.
    Sensitive,
    /// Payload never enters logs or traces; correlation is also suppressed.
    Secret,
    /// Payload may expose encoded size and timing, but never content.
    Opaque,
}

impl MessagePrivacy {
    pub const fn exposes_correlation(self) -> bool {
        matches!(self, Self::Public | Self::Private | Self::Sensitive)
    }
}

#[derive(Clone, Copy)]
pub struct MethodDescriptor {
    pub ordinal: u32,
    pub kind: MethodKind,
    pub deadline: DeadlinePolicy,
    pub request_privacy: MessagePrivacy,
    pub response_privacy: MessagePrivacy,
    pub validate_request: BodyValidator,
    pub validate_response: BodyValidator,
}

#[derive(Clone, Copy)]
pub struct ProtocolDescriptor<'a> {
    pub protocol_id: ProtocolId,
    pub major: u16,
    pub min_minor: u16,
    pub max_minor: u16,
    pub limits: ConnectionLimits,
    pub requested_features: &'a [FeatureRecord],
    pub available_features: &'a [AvailableFeature],
    pub versions: &'a [MinorVersionProfile],
    pub feature_set_fits: FeatureSetValidator,
    pub methods: &'a [MethodDescriptor],
}

impl ProtocolDescriptor<'_> {
    pub fn method(&self, ordinal: u32) -> Option<&MethodDescriptor> {
        self.methods.iter().find(|method| method.ordinal == ordinal)
    }

    pub fn server_profile(&self, service_generation: u64) -> nswp_core::ServerProfile<'_> {
        nswp_core::ServerProfile {
            protocol_id: self.protocol_id,
            protocol_major: self.major,
            server_limits: self.limits,
            transport_limits: HANDLE_FREE_ENDPOINT_LIMITS,
            protocol_limits: self.limits,
            service_generation,
            versions: self.versions,
            available_features: self.available_features,
            feature_set_fits: self.feature_set_fits,
        }
    }
}

pub fn no_features_fit(
    _minor: u16,
    features: &dyn SelectedFeatures,
    limits: ConnectionLimits,
) -> bool {
    !features.contains(u32::MAX)
        && limits.max_body_bytes != 0
        && limits.max_handles == 0
        && limits.max_outstanding != 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundState<const FEATURES: usize = MAX_NEGOTIATED_FEATURES> {
    protocol_id: ProtocolId,
    major: u16,
    minor: u16,
    limits: ConnectionLimits,
    service_generation: u64,
    feature_ids: [u32; FEATURES],
    feature_count: u8,
}

impl<const FEATURES: usize> BoundState<FEATURES> {
    pub fn from_parts(
        protocol_id: ProtocolId,
        major: u16,
        minor: u16,
        limits: ConnectionLimits,
        service_generation: u64,
        features: impl Iterator<Item = u32>,
    ) -> Result<Self, RuntimeError> {
        let mut state = Self {
            protocol_id,
            major,
            minor,
            limits,
            service_generation,
            feature_ids: [0; FEATURES],
            feature_count: 0,
        };
        for id in features {
            let slot = state
                .feature_ids
                .get_mut(usize::from(state.feature_count))
                .ok_or(RuntimeError::TooManyFeatures)?;
            *slot = id;
            state.feature_count += 1;
        }
        state.view().map_err(RuntimeError::Decode)?;
        Ok(state)
    }

    pub fn view(&self) -> Result<BoundProtocol<'_>, nswp_core::DecodeError> {
        BoundProtocol::new(
            self.protocol_id,
            self.major,
            self.minor,
            self.limits,
            self.service_generation,
            &self.feature_ids[..usize::from(self.feature_count)],
        )
    }

    pub const fn protocol_id(&self) -> ProtocolId {
        self.protocol_id
    }

    pub const fn major(&self) -> u16 {
        self.major
    }

    pub const fn minor(&self) -> u16 {
        self.minor
    }

    pub const fn limits(&self) -> ConnectionLimits {
        self.limits
    }

    pub const fn service_generation(&self) -> u64 {
        self.service_generation
    }

    pub fn features(&self) -> &[u32] {
        &self.feature_ids[..usize::from(self.feature_count)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionPhase {
    New,
    Negotiating,
    Bound,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    LocalClosed,
    PeerClosed,
    ProtocolError,
    NegotiationRejected(nswp_core::NegotiationStatus),
    ServiceGenerationReplaced,
    TransactionIdExhausted,
    RecentlyCanceledExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    WouldBlock,
    PeerClosed,
    Closed(CloseReason),
    InvalidState,
    PacketTooLarge { bytes: usize },
    BodyTooLarge { bytes: usize },
    TooManyFeatures,
    OutstandingLimit,
    RecentlyCanceledExhausted,
    UnknownMethod,
    WrongMethodKind,
    InvalidDeadline,
    UnknownTransaction,
    TransactionNotExecuting,
    Encode(nswp_core::EncodeError),
    Decode(nswp_core::DecodeError),
    Negotiation(nswp_core::NegotiationError),
    Body(BodyError),
}

impl From<nswp_core::EncodeError> for RuntimeError {
    fn from(value: nswp_core::EncodeError) -> Self {
        Self::Encode(value)
    }
}

impl From<nswp_core::DecodeError> for RuntimeError {
    fn from(value: nswp_core::DecodeError) -> Self {
        Self::Decode(value)
    }
}

impl From<nswp_core::NegotiationError> for RuntimeError {
    fn from(value: nswp_core::NegotiationError) -> Self {
        Self::Negotiation(value)
    }
}

impl From<BodyError> for RuntimeError {
    fn from(value: BodyError) -> Self {
        Self::Body(value)
    }
}

pub(crate) fn validate_deadline(
    policy: DeadlinePolicy,
    now_ns: u64,
    deadline_ns: u64,
) -> Result<(), RuntimeError> {
    match policy {
        DeadlinePolicy::Forbidden if deadline_ns != u64::MAX => Err(RuntimeError::InvalidDeadline),
        DeadlinePolicy::Required { .. } if deadline_ns == u64::MAX => {
            Err(RuntimeError::InvalidDeadline)
        }
        DeadlinePolicy::Required { max_duration_ns }
        | DeadlinePolicy::Optional { max_duration_ns }
            if deadline_ns != u64::MAX
                && max_duration_ns
                    .is_some_and(|maximum| deadline_ns > now_ns.saturating_add(maximum)) =>
        {
            Err(RuntimeError::InvalidDeadline)
        }
        _ => Ok(()),
    }
}
