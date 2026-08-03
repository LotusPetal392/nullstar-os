use crate::{ProviderGeneration, RoleId, RouteKey, ServiceId, ServiceIdError};

pub const SERVICE_ROUTE_MAGIC: [u8; 4] = *b"NSRT";
pub const SERVICE_ROUTE_VERSION: u16 = 1;
pub const SERVICE_ROUTE_WIRE_BYTES: usize = 40;

const KIND_REQUEST: u8 = 1;
const KIND_ACCEPTED: u8 = 2;
const KIND_FAILURE: u8 = 3;
const STATUS_OK: u8 = 0;

/// Canonical failure statuses carried by an `NSRT` failure message.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteFailure {
    Unauthorized = 1,
    Unavailable = 2,
    IssuerCapacity = 3,
}

impl RouteFailure {
    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Unauthorized),
            2 => Some(Self::Unavailable),
            3 => Some(Self::IssuerCapacity),
            _ => None,
        }
    }
}

/// A canonical `NSRT` v1 request or response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteMessage {
    Request {
        key: RouteKey,
    },
    Accepted {
        key: RouteKey,
        generation: ProviderGeneration,
    },
    Failure {
        key: RouteKey,
        failure: RouteFailure,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    InvalidLength,
    InvalidMagic,
    UnsupportedVersion,
    UnknownKind,
    UnknownStatus,
    NonzeroReserved,
    InvalidServiceId(ServiceIdError),
    ZeroRole,
    StatusNotAllowed,
    StatusRequired,
    GenerationNotAllowed,
    GenerationRequired,
}

impl RouteMessage {
    pub const fn key(self) -> RouteKey {
        match self {
            Self::Request { key } | Self::Accepted { key, .. } | Self::Failure { key, .. } => key,
        }
    }

    /// Encodes the message into the exact 40-byte `NSRT` v1 representation.
    ///
    /// Integer fields use little-endian order. UUID bytes retain RFC/network order. Bytes 28..32
    /// are reserved and always zero.
    pub fn encode(self) -> [u8; SERVICE_ROUTE_WIRE_BYTES] {
        let (kind, status, generation) = match self {
            Self::Request { .. } => (KIND_REQUEST, STATUS_OK, 0),
            Self::Accepted { generation, .. } => (KIND_ACCEPTED, STATUS_OK, generation.get()),
            Self::Failure { failure, .. } => (KIND_FAILURE, failure as u8, 0),
        };
        let key = self.key();
        let mut output = [0; SERVICE_ROUTE_WIRE_BYTES];
        output[0..4].copy_from_slice(&SERVICE_ROUTE_MAGIC);
        output[4..6].copy_from_slice(&SERVICE_ROUTE_VERSION.to_le_bytes());
        output[6] = kind;
        output[7] = status;
        output[8..24].copy_from_slice(key.service().as_bytes());
        output[24..28].copy_from_slice(&key.role().get().to_le_bytes());
        output[32..40].copy_from_slice(&generation.to_le_bytes());
        output
    }

    /// Decodes one exact canonical 40-byte `NSRT` v1 message.
    pub fn decode(input: &[u8]) -> Result<Self, DecodeError> {
        if input.len() != SERVICE_ROUTE_WIRE_BYTES {
            return Err(DecodeError::InvalidLength);
        }
        if input[0..4] != SERVICE_ROUTE_MAGIC {
            return Err(DecodeError::InvalidMagic);
        }
        if u16::from_le_bytes(input[4..6].try_into().expect("fixed version field"))
            != SERVICE_ROUTE_VERSION
        {
            return Err(DecodeError::UnsupportedVersion);
        }
        let kind = input[6];
        if !matches!(kind, KIND_REQUEST | KIND_ACCEPTED | KIND_FAILURE) {
            return Err(DecodeError::UnknownKind);
        }
        let status = input[7];
        if status != STATUS_OK && RouteFailure::from_wire(status).is_none() {
            return Err(DecodeError::UnknownStatus);
        }
        if input[28..32].iter().any(|byte| *byte != 0) {
            return Err(DecodeError::NonzeroReserved);
        }

        let mut service_bytes = [0; 16];
        service_bytes.copy_from_slice(&input[8..24]);
        let service =
            ServiceId::from_bytes(service_bytes).map_err(DecodeError::InvalidServiceId)?;
        let role = RoleId::new(u32::from_le_bytes(
            input[24..28].try_into().expect("fixed role field"),
        ))
        .ok_or(DecodeError::ZeroRole)?;
        let generation =
            u64::from_le_bytes(input[32..40].try_into().expect("fixed generation field"));
        let key = RouteKey::new(service, role);

        match kind {
            KIND_REQUEST => {
                if status != STATUS_OK {
                    return Err(DecodeError::StatusNotAllowed);
                }
                if generation != 0 {
                    return Err(DecodeError::GenerationNotAllowed);
                }
                Ok(Self::Request { key })
            }
            KIND_ACCEPTED => {
                if status != STATUS_OK {
                    return Err(DecodeError::StatusNotAllowed);
                }
                let generation =
                    ProviderGeneration::new(generation).ok_or(DecodeError::GenerationRequired)?;
                Ok(Self::Accepted { key, generation })
            }
            KIND_FAILURE => {
                let failure = RouteFailure::from_wire(status).ok_or(DecodeError::StatusRequired)?;
                if generation != 0 {
                    return Err(DecodeError::GenerationNotAllowed);
                }
                Ok(Self::Failure { key, failure })
            }
            _ => unreachable!(),
        }
    }
}
