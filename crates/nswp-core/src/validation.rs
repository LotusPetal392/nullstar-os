use crate::{DecodeError, Header, NSWP_HEADER_BYTES, PacketKind, ProtocolId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionLimits {
    pub max_body_bytes: u32,
    pub max_handles: u16,
    pub max_outstanding: u16,
}

impl ConnectionLimits {
    pub const DESKTOP: Self = Self {
        max_body_bytes: 65_472,
        max_handles: 64,
        max_outstanding: 4_096,
    };
    pub const ENDPOINT_PROTOTYPE: Self = Self {
        max_body_bytes: 192,
        max_handles: 1,
        max_outstanding: 8,
    };

    pub const fn validate(self) -> Result<Self, DecodeError> {
        if self.max_body_bytes == 0 || self.max_outstanding == 0 {
            Err(DecodeError::InvalidPacketRelationship)
        } else {
            Ok(self)
        }
    }

    pub const fn component_min(self, other: Self) -> Self {
        Self {
            max_body_bytes: if self.max_body_bytes < other.max_body_bytes {
                self.max_body_bytes
            } else {
                other.max_body_bytes
            },
            max_handles: if self.max_handles < other.max_handles {
                self.max_handles
            } else {
                other.max_handles
            },
            max_outstanding: if self.max_outstanding < other.max_outstanding {
                self.max_outstanding
            } else {
                other.max_outstanding
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundProtocol<'a> {
    protocol_id: ProtocolId,
    major: u16,
    minor: u16,
    limits: ConnectionLimits,
    service_generation: u64,
    features: &'a [u32],
}

impl<'a> BoundProtocol<'a> {
    pub fn new(
        protocol_id: ProtocolId,
        major: u16,
        minor: u16,
        limits: ConnectionLimits,
        service_generation: u64,
        features: &'a [u32],
    ) -> Result<Self, DecodeError> {
        limits.validate()?;
        if major == 0 || service_generation == 0 || !canonical_feature_ids(features) {
            return Err(DecodeError::InvalidPacketRelationship);
        }
        Ok(Self {
            protocol_id,
            major,
            minor,
            limits,
            service_generation,
            features,
        })
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

    pub const fn features(&self) -> &'a [u32] {
        self.features
    }

    pub fn supports_feature(&self, id: u32) -> bool {
        self.features.binary_search(&id).is_ok()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState<'a> {
    New,
    Negotiating,
    Bound(BoundProtocol<'a>),
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    ClientToServer,
    ServerToClient,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutstandingTransaction {
    pub transaction_id: u64,
    pub ordinal: u32,
    pub deadline_ns: u64,
    pub trace_id: [u8; 16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionState {
    NotApplicable,
    Available { outstanding_count: u16 },
    Outstanding(OutstandingTransaction),
    RecentlyCanceled(OutstandingTransaction),
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidationContext<'a, 'connection> {
    pub direction: Direction,
    pub connection: &'connection ConnectionState<'a>,
    pub transport_bytes: usize,
    pub attached_handles: usize,
    pub transaction: TransactionState,
}

impl Header {
    pub fn validate(
        &self,
        direction: Direction,
        connection: &ConnectionState<'_>,
    ) -> Result<(), DecodeError> {
        self.validate_intrinsic()?;
        validate_direction(self.kind, direction)?;
        validate_connection(self, connection)
    }

    pub fn validate_context(&self, context: &ValidationContext<'_, '_>) -> Result<(), DecodeError> {
        self.validate(context.direction, context.connection)?;
        let expected = NSWP_HEADER_BYTES
            .checked_add(self.body_bytes as usize)
            .ok_or(DecodeError::LengthMismatch)?;
        if context.transport_bytes != expected {
            return Err(DecodeError::LengthMismatch);
        }
        if context.attached_handles != usize::from(self.handle_count) {
            return Err(DecodeError::HandleCountMismatch);
        }
        let bound = match context.connection {
            ConnectionState::Bound(bound) => Some(bound),
            _ => None,
        };
        if let Some(bound) = bound {
            if self.body_bytes > bound.limits.max_body_bytes {
                return Err(DecodeError::BodyLimitExceeded);
            }
            if self.handle_count > bound.limits.max_handles {
                return Err(DecodeError::HandleLimitExceeded);
            }
        }
        validate_transaction(self, context.transaction, bound)
    }
}

fn validate_direction(kind: PacketKind, direction: Direction) -> Result<(), DecodeError> {
    let valid = match kind {
        PacketKind::NegotiateRequest
        | PacketKind::Request
        | PacketKind::OneWay
        | PacketKind::Cancel => direction == Direction::ClientToServer,
        PacketKind::NegotiateResponse | PacketKind::Response | PacketKind::Event => {
            direction == Direction::ServerToClient
        }
        PacketKind::ProtocolError => true,
    };
    if valid {
        Ok(())
    } else {
        Err(DecodeError::InvalidDirection)
    }
}

fn validate_connection(header: &Header, state: &ConnectionState<'_>) -> Result<(), DecodeError> {
    match state {
        ConnectionState::New => {
            if header.kind == PacketKind::NegotiateRequest
                || (header.kind == PacketKind::ProtocolError
                    && header.protocol_major == 0
                    && header.protocol_minor == 0)
            {
                Ok(())
            } else {
                Err(DecodeError::InvalidConnectionState)
            }
        }
        ConnectionState::Negotiating => {
            if header.kind == PacketKind::NegotiateResponse
                || (header.kind == PacketKind::ProtocolError
                    && header.protocol_major == 0
                    && header.protocol_minor == 0)
            {
                Ok(())
            } else {
                Err(DecodeError::InvalidConnectionState)
            }
        }
        ConnectionState::Bound(bound) => {
            if matches!(
                header.kind,
                PacketKind::NegotiateRequest | PacketKind::NegotiateResponse
            ) {
                return Err(DecodeError::InvalidConnectionState);
            }
            if header.protocol_major == bound.major && header.protocol_minor == bound.minor {
                Ok(())
            } else {
                Err(DecodeError::InvalidPacketRelationship)
            }
        }
        ConnectionState::Closed => Err(DecodeError::InvalidConnectionState),
    }
}

fn validate_transaction(
    header: &Header,
    transaction: TransactionState,
    bound: Option<&BoundProtocol<'_>>,
) -> Result<(), DecodeError> {
    match header.kind {
        PacketKind::Request => match transaction {
            TransactionState::Available { outstanding_count } => {
                let limit = bound
                    .map(|bound| bound.limits.max_outstanding)
                    .ok_or(DecodeError::InvalidConnectionState)?;
                if outstanding_count < limit {
                    Ok(())
                } else {
                    Err(DecodeError::TransactionReuse)
                }
            }
            TransactionState::Outstanding(_) | TransactionState::RecentlyCanceled(_) => {
                Err(DecodeError::TransactionReuse)
            }
            TransactionState::NotApplicable | TransactionState::Unknown => {
                Err(DecodeError::TransactionMismatch)
            }
        },
        PacketKind::Response => match transaction {
            TransactionState::Outstanding(expected)
            | TransactionState::RecentlyCanceled(expected) => match_transaction(header, expected),
            _ => Err(DecodeError::TransactionMismatch),
        },
        PacketKind::Cancel => match transaction {
            TransactionState::Outstanding(expected) => match_transaction(header, expected),
            TransactionState::Unknown => Ok(()),
            _ => Err(DecodeError::TransactionMismatch),
        },
        PacketKind::NegotiateRequest
        | PacketKind::NegotiateResponse
        | PacketKind::OneWay
        | PacketKind::Event
        | PacketKind::ProtocolError => {
            if transaction == TransactionState::NotApplicable {
                Ok(())
            } else {
                Err(DecodeError::TransactionMismatch)
            }
        }
    }
}

fn match_transaction(header: &Header, expected: OutstandingTransaction) -> Result<(), DecodeError> {
    if header.transaction_id == expected.transaction_id
        && header.ordinal == expected.ordinal
        && header.deadline_ns == expected.deadline_ns
        && header.trace_id == expected.trace_id
    {
        Ok(())
    } else {
        Err(DecodeError::TransactionMismatch)
    }
}

fn canonical_feature_ids(features: &[u32]) -> bool {
    let mut previous = None;
    for feature in features.iter().copied() {
        if feature == 0 || previous.is_some_and(|previous| feature <= previous) {
            return false;
        }
        previous = Some(feature);
    }
    true
}
