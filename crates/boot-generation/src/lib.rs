#![no_std]

//! Canonical, target-independent boot-generation selection records.
//!
//! The record uses CRC32C to detect accidental corruption and malformed or partial writes. It
//! does not guarantee torn-write recovery or authenticate boot artifacts; the selected generation's
//! manifest remains the trust boundary.

use core::num::NonZeroU64;

use nullfs_format::crc32c;

pub const SELECTION_MAGIC: [u8; 8] = *b"NSBSEL\0\0";
pub const SELECTION_FORMAT_MAJOR: u16 = 1;
pub const SELECTION_FORMAT_MINOR: u16 = 0;
pub const SELECTION_RECORD_BYTES: usize = 64;
pub const SELECTION_CHECKSUM_OFFSET: usize = 60;

const HEADER_BYTES: u16 = SELECTION_RECORD_BYTES as u16;
const RESERVED_START: usize = 52;
const RESERVED_END: usize = SELECTION_CHECKSUM_OFFSET;

/// A nonzero persistent boot-generation identity.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GenerationId(NonZeroU64);

impl GenerationId {
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// A nonzero monotonic revision of the mutable selection record.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SelectionSequence(NonZeroU64);

impl SelectionSequence {
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }

    pub const fn checked_next(self) -> Option<Self> {
        match self.get().checked_add(1) {
            Some(value) => Self::new(value),
            None => None,
        }
    }
}

/// One of the two retained firmware-readable artifact slots.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Slot {
    Zero = 0,
    One = 1,
}

impl Slot {
    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Zero),
            1 => Some(Self::One),
            _ => None,
        }
    }
}

/// Lifecycle state associated with a selected or retained generation.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Health {
    Pending = 1,
    Healthy = 2,
    Failed = 3,
}

impl Health {
    const fn from_wire(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Pending),
            2 => Some(Self::Healthy),
            3 => Some(Self::Failed),
            _ => None,
        }
    }
}

/// One generation retained by the canonical store and firmware mirror.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetainedGeneration {
    id: GenerationId,
    slot: Slot,
    health: Health,
    artifact_crc32c: u32,
}

impl RetainedGeneration {
    pub const fn new(id: GenerationId, slot: Slot, health: Health, artifact_crc32c: u32) -> Self {
        Self {
            id,
            slot,
            health,
            artifact_crc32c,
        }
    }

    pub const fn id(self) -> GenerationId {
        self.id
    }

    pub const fn slot(self) -> Slot {
        self.slot
    }

    pub const fn health(self) -> Health {
        self.health
    }

    pub const fn artifact_crc32c(self) -> u32 {
        self.artifact_crc32c
    }
}

/// A canonical selected generation and optional retained predecessor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    sequence: SelectionSequence,
    selected: RetainedGeneration,
    previous: Option<RetainedGeneration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionError {
    DuplicateGeneration,
    DuplicateSlot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    InvalidLength,
    InvalidMagic,
    UnsupportedMajorVersion(u16),
    UnsupportedMinorVersion(u16),
    InvalidHeaderSize(u16),
    NonzeroFlags,
    ChecksumMismatch { stored: u32, calculated: u32 },
    ZeroSequence,
    ZeroSelectedGeneration,
    UnknownSelectedSlot(u8),
    UnknownSelectedHealth(u8),
    PreviousGenerationRequired,
    UnexpectedPreviousGenerationData,
    UnknownPreviousSlot(u8),
    UnknownPreviousHealth(u8),
    NonzeroReserved,
    InvalidSelection(SelectionError),
}

impl Selection {
    pub const fn new(
        sequence: SelectionSequence,
        selected: RetainedGeneration,
        previous: Option<RetainedGeneration>,
    ) -> Result<Self, SelectionError> {
        if let Some(previous) = previous {
            if selected.id().get() == previous.id().get() {
                return Err(SelectionError::DuplicateGeneration);
            }
            if selected.slot() as u8 == previous.slot() as u8 {
                return Err(SelectionError::DuplicateSlot);
            }
        }
        Ok(Self {
            sequence,
            selected,
            previous,
        })
    }

    pub const fn sequence(self) -> SelectionSequence {
        self.sequence
    }

    pub const fn selected(self) -> RetainedGeneration {
        self.selected
    }

    pub const fn previous(self) -> Option<RetainedGeneration> {
        self.previous
    }

    /// Encodes the one canonical little-endian 64-byte representation.
    pub fn encode(self) -> [u8; SELECTION_RECORD_BYTES] {
        let mut output = [0; SELECTION_RECORD_BYTES];
        output[0..8].copy_from_slice(&SELECTION_MAGIC);
        write_u16(&mut output, 8, SELECTION_FORMAT_MAJOR);
        write_u16(&mut output, 10, SELECTION_FORMAT_MINOR);
        write_u16(&mut output, 12, HEADER_BYTES);
        write_u64(&mut output, 16, self.sequence.get());
        write_u64(&mut output, 24, self.selected.id().get());
        write_u32(&mut output, 40, self.selected.artifact_crc32c());
        output[48] = self.selected.slot() as u8;
        output[50] = self.selected.health() as u8;

        if let Some(previous) = self.previous {
            write_u64(&mut output, 32, previous.id().get());
            write_u32(&mut output, 44, previous.artifact_crc32c());
            output[49] = previous.slot() as u8;
            output[51] = previous.health() as u8;
        }

        let checksum = crc32c(&output);
        write_u32(&mut output, SELECTION_CHECKSUM_OFFSET, checksum);
        output
    }

    /// Decodes and validates one exact canonical 64-byte selection record.
    pub fn decode(input: &[u8]) -> Result<Self, DecodeError> {
        if input.len() != SELECTION_RECORD_BYTES {
            return Err(DecodeError::InvalidLength);
        }
        if input[0..8] != SELECTION_MAGIC {
            return Err(DecodeError::InvalidMagic);
        }
        let major = read_u16(input, 8);
        if major != SELECTION_FORMAT_MAJOR {
            return Err(DecodeError::UnsupportedMajorVersion(major));
        }
        let minor = read_u16(input, 10);
        if minor != SELECTION_FORMAT_MINOR {
            return Err(DecodeError::UnsupportedMinorVersion(minor));
        }
        let header_size = read_u16(input, 12);
        if header_size != HEADER_BYTES {
            return Err(DecodeError::InvalidHeaderSize(header_size));
        }
        if read_u16(input, 14) != 0 {
            return Err(DecodeError::NonzeroFlags);
        }

        let stored = read_u32(input, SELECTION_CHECKSUM_OFFSET);
        let mut checksummed = [0; SELECTION_RECORD_BYTES];
        checksummed.copy_from_slice(input);
        checksummed[SELECTION_CHECKSUM_OFFSET..SELECTION_RECORD_BYTES].fill(0);
        let calculated = crc32c(&checksummed);
        if stored != calculated {
            return Err(DecodeError::ChecksumMismatch { stored, calculated });
        }

        let sequence =
            SelectionSequence::new(read_u64(input, 16)).ok_or(DecodeError::ZeroSequence)?;
        let selected_id =
            GenerationId::new(read_u64(input, 24)).ok_or(DecodeError::ZeroSelectedGeneration)?;
        let selected_slot =
            Slot::from_wire(input[48]).ok_or(DecodeError::UnknownSelectedSlot(input[48]))?;
        let selected_health =
            Health::from_wire(input[50]).ok_or(DecodeError::UnknownSelectedHealth(input[50]))?;
        let selected = RetainedGeneration::new(
            selected_id,
            selected_slot,
            selected_health,
            read_u32(input, 40),
        );

        let previous_id = read_u64(input, 32);
        let previous_crc = read_u32(input, 44);
        let previous_slot = input[49];
        let previous_health = input[51];
        let previous = if previous_id == 0 {
            if previous_crc != 0 || previous_slot != 0 || previous_health != 0 {
                return Err(DecodeError::UnexpectedPreviousGenerationData);
            }
            None
        } else {
            if previous_health == 0 {
                return Err(DecodeError::PreviousGenerationRequired);
            }
            let id =
                GenerationId::new(previous_id).ok_or(DecodeError::PreviousGenerationRequired)?;
            let slot = Slot::from_wire(previous_slot)
                .ok_or(DecodeError::UnknownPreviousSlot(previous_slot))?;
            let health = Health::from_wire(previous_health)
                .ok_or(DecodeError::UnknownPreviousHealth(previous_health))?;
            Some(RetainedGeneration::new(id, slot, health, previous_crc))
        };

        if input[RESERVED_START..RESERVED_END]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(DecodeError::NonzeroReserved);
        }

        Self::new(sequence, selected, previous).map_err(DecodeError::InvalidSelection)
    }
}

fn read_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([input[offset], input[offset + 1]])
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
    ])
}

fn read_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
        input[offset + 4],
        input[offset + 5],
        input[offset + 6],
        input[offset + 7],
    ])
}

fn write_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(output: &mut [u8], offset: usize, value: u64) {
    output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
