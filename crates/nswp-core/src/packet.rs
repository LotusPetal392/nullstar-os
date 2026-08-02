use crate::{DecodeError, EncodeError, Header};

pub const PROTOCOL_ERROR_BODY_BYTES: usize = 24;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketKind {
    NegotiateRequest = 1,
    NegotiateResponse = 2,
    Request = 3,
    Response = 4,
    OneWay = 5,
    Event = 6,
    Cancel = 7,
    ProtocolError = 8,
}

impl TryFrom<u8> for PacketKind {
    type Error = DecodeError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::NegotiateRequest),
            2 => Ok(Self::NegotiateResponse),
            3 => Ok(Self::Request),
            4 => Ok(Self::Response),
            5 => Ok(Self::OneWay),
            6 => Ok(Self::Event),
            7 => Ok(Self::Cancel),
            8 => Ok(Self::ProtocolError),
            _ => Err(DecodeError::UnknownPacketKind),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeaderFlags(u8);

impl HeaderFlags {
    pub const NONE: Self = Self(0);
    pub const TRACE_SAMPLED: Self = Self(1);
    pub const KNOWN_BITS: u8 = Self::TRACE_SAMPLED.0;

    pub const fn from_bits(bits: u8) -> Result<Self, DecodeError> {
        if bits & !Self::KNOWN_BITS != 0 {
            Err(DecodeError::UnknownFlags)
        } else {
            Ok(Self(bits))
        }
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransportStatus {
    #[default]
    Ok = 0,
    Canceled = 1,
    TimedOut = 2,
    Overloaded = 3,
    ResourceExhausted = 4,
    Unavailable = 5,
    AccessDenied = 6,
    BadState = 7,
    NotSupported = 8,
    Internal = 9,
}

impl TryFrom<u32> for TransportStatus {
    type Error = DecodeError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::Canceled),
            2 => Ok(Self::TimedOut),
            3 => Ok(Self::Overloaded),
            4 => Ok(Self::ResourceExhausted),
            5 => Ok(Self::Unavailable),
            6 => Ok(Self::AccessDenied),
            7 => Ok(Self::BadState),
            8 => Ok(Self::NotSupported),
            9 => Ok(Self::Internal),
            _ => Err(DecodeError::UnknownTransportStatus),
        }
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolErrorCode {
    InvalidHeader = 1,
    UnsupportedWireVersion = 2,
    WrongProtocol = 3,
    WrongProtocolVersion = 4,
    UnexpectedPacketKind = 5,
    UnknownOrdinal = 6,
    InvalidBody = 7,
    NoncanonicalBody = 8,
    HandleCountMismatch = 9,
    WrongHandleType = 10,
    InsufficientHandleRights = 11,
    ExcessHandleRights = 12,
    DuplicateTransaction = 13,
    InvalidTransaction = 14,
    LimitExceeded = 15,
    ReservedValueUsed = 16,
    FieldUnavailable = 17,
    InternalRuntimeError = 18,
}

impl TryFrom<u32> for ProtocolErrorCode {
    type Error = DecodeError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::InvalidHeader),
            2 => Ok(Self::UnsupportedWireVersion),
            3 => Ok(Self::WrongProtocol),
            4 => Ok(Self::WrongProtocolVersion),
            5 => Ok(Self::UnexpectedPacketKind),
            6 => Ok(Self::UnknownOrdinal),
            7 => Ok(Self::InvalidBody),
            8 => Ok(Self::NoncanonicalBody),
            9 => Ok(Self::HandleCountMismatch),
            10 => Ok(Self::WrongHandleType),
            11 => Ok(Self::InsufficientHandleRights),
            12 => Ok(Self::ExcessHandleRights),
            13 => Ok(Self::DuplicateTransaction),
            14 => Ok(Self::InvalidTransaction),
            15 => Ok(Self::LimitExceeded),
            16 => Ok(Self::ReservedValueUsed),
            17 => Ok(Self::FieldUnavailable),
            18 => Ok(Self::InternalRuntimeError),
            _ => Err(DecodeError::InvalidProtocolError),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolErrorRecord {
    pub code: ProtocolErrorCode,
    pub detail: u32,
    pub related_transaction_id: u64,
    pub related_ordinal: u32,
}

impl ProtocolErrorRecord {
    pub fn encode(
        &self,
        header: &Header,
        output: &mut [u8; PROTOCOL_ERROR_BODY_BYTES],
    ) -> Result<(), EncodeError> {
        validate_protocol_error_header(self, header).map_err(EncodeError::InvalidHeader)?;
        output.fill(0);
        output[0..4].copy_from_slice(&(self.code as u32).to_le_bytes());
        output[4..8].copy_from_slice(&self.detail.to_le_bytes());
        output[8..16].copy_from_slice(&self.related_transaction_id.to_le_bytes());
        output[16..20].copy_from_slice(&self.related_ordinal.to_le_bytes());
        Ok(())
    }

    pub fn decode(
        header: &Header,
        input: &[u8; PROTOCOL_ERROR_BODY_BYTES],
    ) -> Result<Self, DecodeError> {
        if input[20..24] != [0; 4] {
            return Err(DecodeError::ReservedValueUsed);
        }
        let record = Self {
            code: ProtocolErrorCode::try_from(u32::from_le_bytes(input[0..4].try_into().unwrap()))?,
            detail: u32::from_le_bytes(input[4..8].try_into().unwrap()),
            related_transaction_id: u64::from_le_bytes(input[8..16].try_into().unwrap()),
            related_ordinal: u32::from_le_bytes(input[16..20].try_into().unwrap()),
        };
        validate_protocol_error_header(&record, header)?;
        Ok(record)
    }
}

fn validate_protocol_error_header(
    record: &ProtocolErrorRecord,
    header: &Header,
) -> Result<(), DecodeError> {
    if header.kind == PacketKind::ProtocolError
        && header.body_bytes == PROTOCOL_ERROR_BODY_BYTES as u32
        && header.transaction_id == record.related_transaction_id
        && header.ordinal == record.related_ordinal
    {
        Ok(())
    } else {
        Err(DecodeError::InvalidProtocolError)
    }
}
