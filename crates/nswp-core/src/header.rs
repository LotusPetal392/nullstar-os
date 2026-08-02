use crate::{DecodeError, EncodeError, HeaderFlags, PacketKind, TransportStatus};

pub const NSWP_HEADER_BYTES: usize = 64;
pub const NSWP_MAGIC: [u8; 4] = *b"NSWP";
pub const NSWP_WIRE_MAJOR: u8 = 1;
pub const NSWP_WIRE_MINOR: u8 = 0;

pub mod offset {
    pub const MAGIC: usize = 0x00;
    pub const HEADER_BYTES: usize = 0x04;
    pub const WIRE_MAJOR: usize = 0x06;
    pub const WIRE_MINOR: usize = 0x07;
    pub const KIND: usize = 0x08;
    pub const FLAGS: usize = 0x09;
    pub const RESERVED0: usize = 0x0a;
    pub const PROTOCOL_MAJOR: usize = 0x0c;
    pub const PROTOCOL_MINOR: usize = 0x0e;
    pub const ORDINAL: usize = 0x10;
    pub const BODY_BYTES: usize = 0x14;
    pub const HANDLE_COUNT: usize = 0x18;
    pub const RESERVED1: usize = 0x1a;
    pub const TRANSPORT_STATUS: usize = 0x1c;
    pub const TRANSACTION_ID: usize = 0x20;
    pub const DEADLINE_NS: usize = 0x28;
    pub const TRACE_ID: usize = 0x30;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub kind: PacketKind,
    pub flags: HeaderFlags,
    pub protocol_major: u16,
    pub protocol_minor: u16,
    pub ordinal: u32,
    pub body_bytes: u32,
    pub handle_count: u16,
    pub transport_status: TransportStatus,
    pub transaction_id: u64,
    pub deadline_ns: u64,
    pub trace_id: [u8; 16],
}

impl Header {
    pub fn encode(&self, output: &mut [u8; NSWP_HEADER_BYTES]) -> Result<(), EncodeError> {
        self.validate_intrinsic()
            .map_err(EncodeError::InvalidHeader)?;
        output.fill(0);
        output[offset::MAGIC..offset::MAGIC + 4].copy_from_slice(&NSWP_MAGIC);
        put_u16(output, offset::HEADER_BYTES, NSWP_HEADER_BYTES as u16);
        output[offset::WIRE_MAJOR] = NSWP_WIRE_MAJOR;
        output[offset::WIRE_MINOR] = NSWP_WIRE_MINOR;
        output[offset::KIND] = self.kind as u8;
        output[offset::FLAGS] = self.flags.bits();
        put_u16(output, offset::PROTOCOL_MAJOR, self.protocol_major);
        put_u16(output, offset::PROTOCOL_MINOR, self.protocol_minor);
        put_u32(output, offset::ORDINAL, self.ordinal);
        put_u32(output, offset::BODY_BYTES, self.body_bytes);
        put_u16(output, offset::HANDLE_COUNT, self.handle_count);
        put_u32(
            output,
            offset::TRANSPORT_STATUS,
            self.transport_status as u32,
        );
        put_u64(output, offset::TRANSACTION_ID, self.transaction_id);
        put_u64(output, offset::DEADLINE_NS, self.deadline_ns);
        output[offset::TRACE_ID..offset::TRACE_ID + 16].copy_from_slice(&self.trace_id);
        Ok(())
    }

    pub fn decode(input: &[u8; NSWP_HEADER_BYTES]) -> Result<Self, DecodeError> {
        if input[offset::MAGIC..offset::MAGIC + 4] != NSWP_MAGIC {
            return Err(DecodeError::InvalidMagic);
        }
        if get_u16(input, offset::HEADER_BYTES) != NSWP_HEADER_BYTES as u16 {
            return Err(DecodeError::InvalidHeaderSize);
        }
        if input[offset::WIRE_MAJOR] != NSWP_WIRE_MAJOR
            || input[offset::WIRE_MINOR] != NSWP_WIRE_MINOR
        {
            return Err(DecodeError::UnsupportedWireVersion);
        }
        if get_u16(input, offset::RESERVED0) != 0 || get_u16(input, offset::RESERVED1) != 0 {
            return Err(DecodeError::ReservedValueUsed);
        }

        let mut trace_id = [0_u8; 16];
        trace_id.copy_from_slice(&input[offset::TRACE_ID..offset::TRACE_ID + 16]);
        let header = Self {
            kind: PacketKind::try_from(input[offset::KIND])?,
            flags: HeaderFlags::from_bits(input[offset::FLAGS])?,
            protocol_major: get_u16(input, offset::PROTOCOL_MAJOR),
            protocol_minor: get_u16(input, offset::PROTOCOL_MINOR),
            ordinal: get_u32(input, offset::ORDINAL),
            body_bytes: get_u32(input, offset::BODY_BYTES),
            handle_count: get_u16(input, offset::HANDLE_COUNT),
            transport_status: TransportStatus::try_from(get_u32(input, offset::TRANSPORT_STATUS))?,
            transaction_id: get_u64(input, offset::TRANSACTION_ID),
            deadline_ns: get_u64(input, offset::DEADLINE_NS),
            trace_id,
        };
        header.validate_intrinsic()?;
        Ok(header)
    }

    pub fn decode_prefix(input: &[u8]) -> Result<Self, DecodeError> {
        let encoded = input
            .get(..NSWP_HEADER_BYTES)
            .ok_or(DecodeError::PacketTooShort)?;
        let mut header = [0_u8; NSWP_HEADER_BYTES];
        header.copy_from_slice(encoded);
        Self::decode(&header)
    }

    pub(crate) fn validate_intrinsic(&self) -> Result<(), DecodeError> {
        if !self.body_bytes.is_multiple_of(8) {
            return Err(DecodeError::BodyNotAligned);
        }
        if self.flags.contains(HeaderFlags::TRACE_SAMPLED) && self.trace_id == [0; 16] {
            return Err(DecodeError::InvalidPacketRelationship);
        }

        let infinite = u64::MAX;
        let valid = match self.kind {
            PacketKind::NegotiateRequest | PacketKind::NegotiateResponse => {
                self.protocol_major == 0
                    && self.protocol_minor == 0
                    && self.ordinal == 0
                    && self.handle_count == 0
                    && self.transport_status == TransportStatus::Ok
                    && self.transaction_id == 0
                    && self.deadline_ns == infinite
            }
            PacketKind::Request => {
                self.protocol_major != 0
                    && self.ordinal != 0
                    && self.transport_status == TransportStatus::Ok
                    && self.transaction_id != 0
            }
            PacketKind::Response => {
                self.protocol_major != 0
                    && self.ordinal != 0
                    && self.transaction_id != 0
                    && (self.transport_status == TransportStatus::Ok
                        || (self.body_bytes == 0 && self.handle_count == 0))
            }
            PacketKind::OneWay => {
                self.protocol_major != 0
                    && self.ordinal != 0
                    && self.transport_status == TransportStatus::Ok
                    && self.transaction_id == 0
            }
            PacketKind::Event => {
                self.protocol_major != 0
                    && self.ordinal != 0
                    && self.transport_status == TransportStatus::Ok
                    && self.transaction_id == 0
                    && self.deadline_ns == infinite
            }
            PacketKind::Cancel => {
                self.protocol_major != 0
                    && self.ordinal != 0
                    && self.body_bytes == 0
                    && self.handle_count == 0
                    && self.transport_status == TransportStatus::Ok
                    && self.transaction_id != 0
            }
            PacketKind::ProtocolError => {
                self.body_bytes == crate::PROTOCOL_ERROR_BODY_BYTES as u32
                    && self.handle_count == 0
                    && self.transport_status == TransportStatus::Ok
            }
        };
        if valid {
            Ok(())
        } else {
            Err(DecodeError::InvalidPacketRelationship)
        }
    }
}

fn get_u16(bytes: &[u8; NSWP_HEADER_BYTES], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn get_u32(bytes: &[u8; NSWP_HEADER_BYTES], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn get_u64(bytes: &[u8; NSWP_HEADER_BYTES], offset: usize) -> u64 {
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

fn put_u16(bytes: &mut [u8; NSWP_HEADER_BYTES], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8; NSWP_HEADER_BYTES], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8; NSWP_HEADER_BYTES], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
