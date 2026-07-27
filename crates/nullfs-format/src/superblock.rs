use core::str;

use crate::{
    BLOCK_SIZE, BLOCK_SIZE_U64, DEFAULT_ALLOCATION_GROUP_BLOCKS,
    DEFAULT_DESCRIPTOR_RESERVATION_BLOCKS, Error, FIRST_DESCRIPTOR_BLOCK, FORMAT_MAJOR,
    FORMAT_MINOR, Features, INCOMPAT_PHASE2_CORE, INCOMPAT_PHASE3_WRITABLE_REDO, LABEL_CAPACITY,
    MAGIC, MINIMUM_VOLUME_BLOCKS, MountMode, PHASE1_FORMAT_MINOR, PHASE2_FORMAT_MINOR,
    PHASE3_BACKUP_SUPERBLOCK_BLOCK, PHASE3_FILESYSTEM_STATE_BLOCK, PHASE3_FIRST_ALLOCATABLE_BLOCK,
    PHASE3_FORMAT_MINOR, PHASE3_JOURNAL_BLOCK_COUNT, PHASE3_JOURNAL_FIRST_BLOCK,
    PHASE3_MAX_UPDATES, PHASE3_MINIMUM_VOLUME_BLOCKS, SUPERBLOCK_CHECKSUM_OFFSET,
    SUPERBLOCK_HEADER_BYTES, allocation_group::ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK, crc32c,
};

const MAGIC_OFFSET: usize = 0;
const MAJOR_OFFSET: usize = 8;
const MINOR_OFFSET: usize = 10;
const HEADER_SIZE_OFFSET: usize = 12;
const BLOCK_SIZE_OFFSET: usize = 16;
const STATE_OFFSET: usize = 20;
const COMPATIBLE_FEATURES_OFFSET: usize = 24;
const READ_ONLY_FEATURES_OFFSET: usize = 32;
const INCOMPATIBLE_FEATURES_OFFSET: usize = 40;
const UUID_OFFSET: usize = 48;
const LABEL_OFFSET: usize = 64;
const CAPACITY_BLOCKS_OFFSET: usize = 128;
const ALLOCATION_GROUP_BLOCKS_OFFSET: usize = 136;
const ALLOCATION_GROUP_COUNT_OFFSET: usize = 144;
const DESCRIPTOR_RESERVATION_OFFSET: usize = 148;
const FIRST_DESCRIPTOR_BLOCK_OFFSET: usize = 152;
const FIRST_ALLOCATABLE_BLOCK_OFFSET: usize = 160;
const RESERVED_HEADER_OFFSET: usize = 168;
const BACKUP_SUPERBLOCK_BLOCK_OFFSET: usize = 168;
const FILESYSTEM_STATE_BLOCK_OFFSET: usize = 176;
const JOURNAL_FIRST_BLOCK_OFFSET: usize = 184;
const JOURNAL_BLOCK_COUNT_OFFSET: usize = 192;
const JOURNAL_MAX_UPDATES_OFFSET: usize = 196;
const PHASE3_GEOMETRY_END: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeState {
    Clean,
    Dirty,
}

impl VolumeState {
    const fn encode(self) -> u32 {
        match self {
            Self::Clean => 0,
            Self::Dirty => 1,
        }
    }

    fn decode(value: u32) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Clean),
            1 => Ok(Self::Dirty),
            _ => Err(Error::InvalidState(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatOptions<'a> {
    pub device_bytes: u64,
    pub uuid: [u8; 16],
    pub label: &'a str,
    pub allocation_group_blocks: u64,
    pub descriptor_reservation_blocks: u32,
}

impl<'a> FormatOptions<'a> {
    pub const fn new(device_bytes: u64, uuid: [u8; 16], label: &'a str) -> Self {
        Self {
            device_bytes,
            uuid,
            label,
            allocation_group_blocks: DEFAULT_ALLOCATION_GROUP_BLOCKS,
            descriptor_reservation_blocks: DEFAULT_DESCRIPTOR_RESERVATION_BLOCKS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superblock {
    pub format_major: u16,
    pub format_minor: u16,
    pub state: VolumeState,
    pub features: Features,
    pub filesystem_uuid: [u8; 16],
    label: [u8; LABEL_CAPACITY],
    label_length: u8,
    pub capacity_blocks: u64,
    pub allocation_group_blocks: u64,
    pub allocation_group_count: u32,
    pub descriptor_reservation_blocks: u32,
    pub first_descriptor_block: u64,
    pub first_allocatable_block: u64,
    pub backup_superblock_block: u64,
    pub filesystem_state_block: u64,
    pub journal_first_block: u64,
    pub journal_block_count: u32,
    pub journal_max_updates: u32,
}

impl Superblock {
    pub fn format(options: FormatOptions<'_>) -> Result<Self, Error> {
        if options.uuid == [0; 16] {
            return Err(Error::InvalidUuid);
        }
        let label_bytes = options.label.as_bytes();
        if label_bytes.len() > LABEL_CAPACITY {
            return Err(Error::LabelTooLong);
        }
        if label_bytes.contains(&0) {
            return Err(Error::LabelContainsNul);
        }
        let capacity_blocks = options.device_bytes / BLOCK_SIZE_U64;
        if capacity_blocks < MINIMUM_VOLUME_BLOCKS {
            return Err(Error::DeviceTooSmall);
        }
        if options.allocation_group_blocks == 0 {
            return Err(Error::InvalidAllocationGroupSize);
        }
        if options.descriptor_reservation_blocks == 0 {
            return Err(Error::InvalidDescriptorReservation);
        }
        let first_allocatable_block = FIRST_DESCRIPTOR_BLOCK
            .checked_add(u64::from(options.descriptor_reservation_blocks))
            .ok_or(Error::ArithmeticOverflow)?;
        if first_allocatable_block > capacity_blocks {
            return Err(Error::DeviceTooSmall);
        }
        let allocation_group_count = group_count(capacity_blocks, options.allocation_group_blocks)?;
        let mut label = [0; LABEL_CAPACITY];
        label[..label_bytes.len()].copy_from_slice(label_bytes);
        Ok(Self {
            format_major: FORMAT_MAJOR,
            format_minor: PHASE1_FORMAT_MINOR,
            state: VolumeState::Clean,
            features: Features::default(),
            filesystem_uuid: options.uuid,
            label,
            label_length: label_bytes.len() as u8,
            capacity_blocks,
            allocation_group_blocks: options.allocation_group_blocks,
            allocation_group_count,
            descriptor_reservation_blocks: options.descriptor_reservation_blocks,
            first_descriptor_block: FIRST_DESCRIPTOR_BLOCK,
            first_allocatable_block,
            backup_superblock_block: 0,
            filesystem_state_block: 0,
            journal_first_block: 0,
            journal_block_count: 0,
            journal_max_updates: 0,
        })
    }

    pub fn format_phase2(options: FormatOptions<'_>) -> Result<Self, Error> {
        let mut superblock = Self::format(options)?;
        superblock.format_minor = PHASE2_FORMAT_MINOR;
        superblock.features.incompatible |= INCOMPAT_PHASE2_CORE;
        superblock.validate(None, MountMode::ReadOnly)?;
        Ok(superblock)
    }

    pub fn format_phase3(options: FormatOptions<'_>) -> Result<Self, Error> {
        if options.descriptor_reservation_blocks != DEFAULT_DESCRIPTOR_RESERVATION_BLOCKS {
            return Err(Error::InvalidPhase3Geometry);
        }
        let mut superblock = Self::format_phase2(options)?;
        if superblock.capacity_blocks < PHASE3_MINIMUM_VOLUME_BLOCKS {
            return Err(Error::DeviceTooSmall);
        }
        superblock.format_minor = PHASE3_FORMAT_MINOR;
        superblock.features.incompatible |= INCOMPAT_PHASE3_WRITABLE_REDO;
        superblock.backup_superblock_block = PHASE3_BACKUP_SUPERBLOCK_BLOCK;
        superblock.filesystem_state_block = PHASE3_FILESYSTEM_STATE_BLOCK;
        superblock.journal_first_block = PHASE3_JOURNAL_FIRST_BLOCK;
        superblock.journal_block_count = PHASE3_JOURNAL_BLOCK_COUNT;
        superblock.journal_max_updates = PHASE3_MAX_UPDATES as u32;
        superblock.first_allocatable_block = PHASE3_FIRST_ALLOCATABLE_BLOCK;
        superblock.validate(None, MountMode::ReadOnly)?;
        Ok(superblock)
    }

    pub const fn phase2_enabled(&self) -> bool {
        self.format_minor >= PHASE2_FORMAT_MINOR
            && self.features.incompatible & INCOMPAT_PHASE2_CORE != 0
    }

    pub const fn phase3_enabled(&self) -> bool {
        self.format_minor == PHASE3_FORMAT_MINOR
            && self.features.incompatible & INCOMPAT_PHASE3_WRITABLE_REDO != 0
    }

    pub fn label(&self) -> &str {
        str::from_utf8(&self.label[..usize::from(self.label_length)])
            .expect("validated NullFS label became invalid")
    }

    pub fn encode(&self) -> Result<[u8; BLOCK_SIZE], Error> {
        self.validate(None, MountMode::ReadOnly)?;
        let mut bytes = [0; BLOCK_SIZE];
        bytes[MAGIC_OFFSET..MAGIC_OFFSET + MAGIC.len()].copy_from_slice(&MAGIC);
        put_u16(&mut bytes, MAJOR_OFFSET, self.format_major);
        put_u16(&mut bytes, MINOR_OFFSET, self.format_minor);
        put_u32(&mut bytes, HEADER_SIZE_OFFSET, SUPERBLOCK_HEADER_BYTES);
        put_u32(&mut bytes, BLOCK_SIZE_OFFSET, BLOCK_SIZE as u32);
        put_u32(&mut bytes, STATE_OFFSET, self.state.encode());
        put_u64(
            &mut bytes,
            COMPATIBLE_FEATURES_OFFSET,
            self.features.compatible,
        );
        put_u64(
            &mut bytes,
            READ_ONLY_FEATURES_OFFSET,
            self.features.read_only_compatible,
        );
        put_u64(
            &mut bytes,
            INCOMPATIBLE_FEATURES_OFFSET,
            self.features.incompatible,
        );
        bytes[UUID_OFFSET..UUID_OFFSET + self.filesystem_uuid.len()]
            .copy_from_slice(&self.filesystem_uuid);
        bytes[LABEL_OFFSET..LABEL_OFFSET + LABEL_CAPACITY].copy_from_slice(&self.label);
        put_u64(&mut bytes, CAPACITY_BLOCKS_OFFSET, self.capacity_blocks);
        put_u64(
            &mut bytes,
            ALLOCATION_GROUP_BLOCKS_OFFSET,
            self.allocation_group_blocks,
        );
        put_u32(
            &mut bytes,
            ALLOCATION_GROUP_COUNT_OFFSET,
            self.allocation_group_count,
        );
        put_u32(
            &mut bytes,
            DESCRIPTOR_RESERVATION_OFFSET,
            self.descriptor_reservation_blocks,
        );
        put_u64(
            &mut bytes,
            FIRST_DESCRIPTOR_BLOCK_OFFSET,
            self.first_descriptor_block,
        );
        put_u64(
            &mut bytes,
            FIRST_ALLOCATABLE_BLOCK_OFFSET,
            self.first_allocatable_block,
        );
        if self.phase3_enabled() {
            put_u64(
                &mut bytes,
                BACKUP_SUPERBLOCK_BLOCK_OFFSET,
                self.backup_superblock_block,
            );
            put_u64(
                &mut bytes,
                FILESYSTEM_STATE_BLOCK_OFFSET,
                self.filesystem_state_block,
            );
            put_u64(
                &mut bytes,
                JOURNAL_FIRST_BLOCK_OFFSET,
                self.journal_first_block,
            );
            put_u32(
                &mut bytes,
                JOURNAL_BLOCK_COUNT_OFFSET,
                self.journal_block_count,
            );
            put_u32(
                &mut bytes,
                JOURNAL_MAX_UPDATES_OFFSET,
                self.journal_max_updates,
            );
        }
        let checksum = superblock_checksum(&bytes);
        put_u32(&mut bytes, SUPERBLOCK_CHECKSUM_OFFSET, checksum);
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8], device_bytes: Option<u64>, mode: MountMode) -> Result<Self, Error> {
        if bytes.len() < BLOCK_SIZE {
            return Err(Error::TruncatedSuperblock {
                actual: bytes.len(),
            });
        }
        let bytes = &bytes[..BLOCK_SIZE];
        if bytes[MAGIC_OFFSET..MAGIC_OFFSET + MAGIC.len()] != MAGIC {
            return Err(Error::InvalidMagic);
        }
        let stored_checksum = get_u32(bytes, SUPERBLOCK_CHECKSUM_OFFSET);
        let calculated_checksum = superblock_checksum(bytes);
        if stored_checksum != calculated_checksum {
            return Err(Error::ChecksumMismatch {
                expected: stored_checksum,
                actual: calculated_checksum,
            });
        }
        let format_major = get_u16(bytes, MAJOR_OFFSET);
        if format_major != FORMAT_MAJOR {
            return Err(Error::UnsupportedMajorVersion(format_major));
        }
        let format_minor = get_u16(bytes, MINOR_OFFSET);
        if format_minor > FORMAT_MINOR {
            return Err(Error::UnsupportedMinorVersion(format_minor));
        }
        let header_size = get_u32(bytes, HEADER_SIZE_OFFSET);
        if header_size != SUPERBLOCK_HEADER_BYTES {
            return Err(Error::InvalidHeaderSize(header_size));
        }
        let block_size = get_u32(bytes, BLOCK_SIZE_OFFSET);
        if block_size != BLOCK_SIZE as u32 {
            return Err(Error::InvalidBlockSize(block_size));
        }
        let state = VolumeState::decode(get_u32(bytes, STATE_OFFSET))?;
        let features = Features {
            compatible: get_u64(bytes, COMPATIBLE_FEATURES_OFFSET),
            read_only_compatible: get_u64(bytes, READ_ONLY_FEATURES_OFFSET),
            incompatible: get_u64(bytes, INCOMPATIBLE_FEATURES_OFFSET),
        };
        let mut filesystem_uuid = [0; 16];
        filesystem_uuid.copy_from_slice(&bytes[UUID_OFFSET..UUID_OFFSET + 16]);
        if filesystem_uuid == [0; 16] {
            return Err(Error::InvalidUuid);
        }
        let label_bytes = &bytes[LABEL_OFFSET..LABEL_OFFSET + LABEL_CAPACITY];
        let label_length = label_bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(LABEL_CAPACITY);
        if label_bytes[label_length..].iter().any(|byte| *byte != 0)
            || str::from_utf8(&label_bytes[..label_length]).is_err()
        {
            return Err(Error::InvalidLabel);
        }
        let phase3_feature = features.incompatible & INCOMPAT_PHASE3_WRITABLE_REDO != 0;
        if format_minor == PHASE3_FORMAT_MINOR && phase3_feature {
            if bytes[PHASE3_GEOMETRY_END..SUPERBLOCK_CHECKSUM_OFFSET]
                .iter()
                .any(|byte| *byte != 0)
            {
                return Err(Error::NonZeroReservedBytes);
            }
        } else if bytes[RESERVED_HEADER_OFFSET..SUPERBLOCK_CHECKSUM_OFFSET]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(Error::NonZeroReservedBytes);
        }
        let mut label = [0; LABEL_CAPACITY];
        label.copy_from_slice(label_bytes);
        let superblock = Self {
            format_major,
            format_minor,
            state,
            features,
            filesystem_uuid,
            label,
            label_length: label_length as u8,
            capacity_blocks: get_u64(bytes, CAPACITY_BLOCKS_OFFSET),
            allocation_group_blocks: get_u64(bytes, ALLOCATION_GROUP_BLOCKS_OFFSET),
            allocation_group_count: get_u32(bytes, ALLOCATION_GROUP_COUNT_OFFSET),
            descriptor_reservation_blocks: get_u32(bytes, DESCRIPTOR_RESERVATION_OFFSET),
            first_descriptor_block: get_u64(bytes, FIRST_DESCRIPTOR_BLOCK_OFFSET),
            first_allocatable_block: get_u64(bytes, FIRST_ALLOCATABLE_BLOCK_OFFSET),
            backup_superblock_block: get_u64(bytes, BACKUP_SUPERBLOCK_BLOCK_OFFSET),
            filesystem_state_block: get_u64(bytes, FILESYSTEM_STATE_BLOCK_OFFSET),
            journal_first_block: get_u64(bytes, JOURNAL_FIRST_BLOCK_OFFSET),
            journal_block_count: get_u32(bytes, JOURNAL_BLOCK_COUNT_OFFSET),
            journal_max_updates: get_u32(bytes, JOURNAL_MAX_UPDATES_OFFSET),
        };
        superblock.validate(device_bytes, mode)?;
        Ok(superblock)
    }

    pub fn validate(&self, device_bytes: Option<u64>, mode: MountMode) -> Result<(), Error> {
        if self.format_major != FORMAT_MAJOR {
            return Err(Error::UnsupportedMajorVersion(self.format_major));
        }
        if self.format_minor > FORMAT_MINOR {
            return Err(Error::UnsupportedMinorVersion(self.format_minor));
        }
        if self.filesystem_uuid == [0; 16] {
            return Err(Error::InvalidUuid);
        }
        if self.format_minor < PHASE2_FORMAT_MINOR
            && self.features.incompatible & INCOMPAT_PHASE2_CORE != 0
        {
            return Err(Error::UnsupportedIncompatibleFeatures(INCOMPAT_PHASE2_CORE));
        }
        if self.format_minor < PHASE3_FORMAT_MINOR
            && self.features.incompatible & INCOMPAT_PHASE3_WRITABLE_REDO != 0
        {
            return Err(Error::UnsupportedIncompatibleFeatures(
                INCOMPAT_PHASE3_WRITABLE_REDO,
            ));
        }
        self.features.validate(mode)?;
        if mode == MountMode::ReadWrite && self.state == VolumeState::Dirty {
            return Err(Error::DirtyVolume);
        }
        if self.capacity_blocks < MINIMUM_VOLUME_BLOCKS {
            return Err(Error::InvalidCapacity);
        }
        if let Some(device_bytes) = device_bytes {
            let device_blocks = device_bytes / BLOCK_SIZE_U64;
            if self.capacity_blocks > device_blocks {
                return Err(Error::InvalidCapacity);
            }
        }
        if self.allocation_group_blocks == 0 {
            return Err(Error::InvalidAllocationGroupSize);
        }
        if self.allocation_group_count
            != group_count(self.capacity_blocks, self.allocation_group_blocks)?
        {
            return Err(Error::InvalidAllocationGroupCount);
        }
        if self.descriptor_reservation_blocks == 0 {
            return Err(Error::InvalidDescriptorReservation);
        }
        if self.format_minor >= PHASE2_FORMAT_MINOR && !self.phase2_enabled() {
            return Err(Error::InvalidAllocationGroupTable);
        }
        if self.format_minor >= PHASE3_FORMAT_MINOR && !self.phase3_enabled() {
            return Err(Error::InvalidPhase3Geometry);
        }
        if self.phase2_enabled() {
            let descriptor_capacity = u64::from(self.descriptor_reservation_blocks)
                .checked_mul(ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK as u64)
                .ok_or(Error::ArithmeticOverflow)?;
            if u64::from(self.allocation_group_count) > descriptor_capacity {
                return Err(Error::InvalidDescriptorReservation);
            }
        }
        if self.first_descriptor_block != FIRST_DESCRIPTOR_BLOCK {
            return Err(Error::InvalidDescriptorStart);
        }
        let descriptor_end = self
            .first_descriptor_block
            .checked_add(u64::from(self.descriptor_reservation_blocks))
            .ok_or(Error::ArithmeticOverflow)?;
        let expected_first_allocatable = if self.phase3_enabled() {
            if self.descriptor_reservation_blocks != DEFAULT_DESCRIPTOR_RESERVATION_BLOCKS
                || self.capacity_blocks < PHASE3_MINIMUM_VOLUME_BLOCKS
                || descriptor_end != PHASE3_BACKUP_SUPERBLOCK_BLOCK
                || self.backup_superblock_block != PHASE3_BACKUP_SUPERBLOCK_BLOCK
                || self.filesystem_state_block != PHASE3_FILESYSTEM_STATE_BLOCK
                || self.journal_first_block != PHASE3_JOURNAL_FIRST_BLOCK
                || self.journal_block_count != PHASE3_JOURNAL_BLOCK_COUNT
                || self.journal_max_updates != PHASE3_MAX_UPDATES as u32
            {
                return Err(Error::InvalidPhase3Geometry);
            }
            PHASE3_FIRST_ALLOCATABLE_BLOCK
        } else {
            if self.backup_superblock_block != 0
                || self.filesystem_state_block != 0
                || self.journal_first_block != 0
                || self.journal_block_count != 0
                || self.journal_max_updates != 0
            {
                return Err(Error::InvalidPhase3Geometry);
            }
            descriptor_end
        };
        if self.first_allocatable_block != expected_first_allocatable
            || self.first_allocatable_block > self.capacity_blocks
        {
            return Err(Error::InvalidFirstAllocatableBlock);
        }
        Ok(())
    }
}

fn group_count(capacity_blocks: u64, group_blocks: u64) -> Result<u32, Error> {
    if group_blocks == 0 {
        return Err(Error::InvalidAllocationGroupSize);
    }
    let rounded = capacity_blocks
        .checked_add(group_blocks - 1)
        .ok_or(Error::ArithmeticOverflow)?;
    u32::try_from(rounded / group_blocks).map_err(|_| Error::InvalidAllocationGroupCount)
}

fn superblock_checksum(bytes: &[u8]) -> u32 {
    let mut checksum_input = [0; BLOCK_SIZE];
    checksum_input.copy_from_slice(&bytes[..BLOCK_SIZE]);
    checksum_input[SUPERBLOCK_CHECKSUM_OFFSET..].fill(0);
    crc32c(&checksum_input)
}

fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
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

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SUPERBLOCK_OFFSET, complete_blocks};

    const UUID: [u8; 16] = [
        0x61, 0x3e, 0x42, 0x19, 0x88, 0x5d, 0x4a, 0xb7, 0x9f, 0x44, 0x31, 0xa2, 0x77, 0x10, 0xcd,
        0xef,
    ];
    const IMAGE_BYTES: u64 = 64 * 1024 * 1024;

    fn formatted() -> Superblock {
        Superblock::format(FormatOptions::new(IMAGE_BYTES, UUID, "Phase1"))
            .expect("valid format options")
    }

    #[test]
    fn phase3_format_has_exact_geometry_and_retains_phase2() {
        let superblock = Superblock::format_phase3(FormatOptions::new(IMAGE_BYTES, UUID, "Phase3"))
            .expect("phase 3 format");
        assert_eq!(superblock.format_minor, PHASE3_FORMAT_MINOR);
        assert!(superblock.phase2_enabled());
        assert!(superblock.phase3_enabled());
        assert_eq!(superblock.backup_superblock_block, 18);
        assert_eq!(superblock.filesystem_state_block, 19);
        assert_eq!(superblock.journal_first_block, 20);
        assert_eq!(superblock.journal_block_count, 130);
        assert_eq!(superblock.journal_max_updates, 64);
        assert_eq!(superblock.first_allocatable_block, 150);
        let bytes = superblock.encode().expect("encode");
        assert_eq!(get_u64(&bytes, BACKUP_SUPERBLOCK_BLOCK_OFFSET), 18);
        assert_eq!(get_u64(&bytes, FILESYSTEM_STATE_BLOCK_OFFSET), 19);
        assert_eq!(get_u64(&bytes, JOURNAL_FIRST_BLOCK_OFFSET), 20);
        assert_eq!(get_u32(&bytes, JOURNAL_BLOCK_COUNT_OFFSET), 130);
        assert_eq!(get_u32(&bytes, JOURNAL_MAX_UPDATES_OFFSET), 64);
        assert!(
            bytes[PHASE3_GEOMETRY_END..SUPERBLOCK_CHECKSUM_OFFSET]
                .iter()
                .all(|v| *v == 0)
        );
        assert_eq!(
            Superblock::decode(&bytes, Some(IMAGE_BYTES), MountMode::ReadWrite).unwrap(),
            superblock
        );
    }

    #[test]
    fn phase3_reserved_bytes_require_exact_version_feature_and_geometry() {
        let mut phase2 = Superblock::format_phase2(FormatOptions::new(IMAGE_BYTES, UUID, "Phase2"))
            .unwrap()
            .encode()
            .unwrap();
        put_u64(&mut phase2, BACKUP_SUPERBLOCK_BLOCK_OFFSET, 18);
        put_u32(&mut phase2, SUPERBLOCK_CHECKSUM_OFFSET, 0);
        let checksum = superblock_checksum(&phase2);
        put_u32(&mut phase2, SUPERBLOCK_CHECKSUM_OFFSET, checksum);
        assert_eq!(
            Superblock::decode(&phase2, None, MountMode::ReadOnly),
            Err(Error::NonZeroReservedBytes)
        );

        let mut phase3 = Superblock::format_phase3(FormatOptions::new(IMAGE_BYTES, UUID, "Phase3"))
            .unwrap()
            .encode()
            .unwrap();
        put_u32(&mut phase3, JOURNAL_BLOCK_COUNT_OFFSET, 129);
        put_u32(&mut phase3, SUPERBLOCK_CHECKSUM_OFFSET, 0);
        let checksum = superblock_checksum(&phase3);
        put_u32(&mut phase3, SUPERBLOCK_CHECKSUM_OFFSET, checksum);
        assert_eq!(
            Superblock::decode(&phase3, None, MountMode::ReadOnly),
            Err(Error::InvalidPhase3Geometry)
        );
    }

    #[test]
    fn phase2_format_sets_minor_and_feature() {
        let superblock = Superblock::format_phase2(FormatOptions::new(IMAGE_BYTES, UUID, "Phase2"))
            .expect("phase 2 format");
        assert_eq!(superblock.format_minor, PHASE2_FORMAT_MINOR);
        assert!(superblock.phase2_enabled());
        let bytes = superblock.encode().expect("encode");
        assert_eq!(
            Superblock::decode(&bytes, Some(IMAGE_BYTES), MountMode::ReadOnly).expect("decode"),
            superblock
        );
    }

    #[test]
    fn format_round_trip_is_deterministic() {
        let superblock = formatted();
        let first = superblock.encode().expect("encode");
        let second = superblock.encode().expect("encode");
        assert_eq!(first, second);
        assert_eq!(&first[..8], &MAGIC);
        assert_eq!(get_u32(&first, BLOCK_SIZE_OFFSET), BLOCK_SIZE as u32);
        assert_eq!(
            get_u64(&first, CAPACITY_BLOCKS_OFFSET),
            complete_blocks(IMAGE_BYTES)
        );
        assert_eq!(get_u64(&first, FIRST_DESCRIPTOR_BLOCK_OFFSET), 17);
        assert_eq!(get_u64(&first, FIRST_ALLOCATABLE_BLOCK_OFFSET), 18);
        assert!(
            first[RESERVED_HEADER_OFFSET..SUPERBLOCK_CHECKSUM_OFFSET]
                .iter()
                .all(|byte| *byte == 0)
        );
        let decoded =
            Superblock::decode(&first, Some(IMAGE_BYTES), MountMode::ReadWrite).expect("decode");
        assert_eq!(decoded, superblock);
        assert_eq!(decoded.label(), "Phase1");
        assert_eq!(SUPERBLOCK_OFFSET, 65_536);
    }

    #[test]
    fn corruption_is_detected() {
        let mut bytes = formatted().encode().expect("encode");
        bytes[CAPACITY_BLOCKS_OFFSET] ^= 1;
        assert!(matches!(
            Superblock::decode(&bytes, Some(IMAGE_BYTES), MountMode::ReadOnly),
            Err(Error::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn truncated_input_is_rejected() {
        assert_eq!(
            Superblock::decode(&[0; 128], None, MountMode::ReadOnly),
            Err(Error::TruncatedSuperblock { actual: 128 })
        );
    }

    #[test]
    fn invalid_block_size_is_rejected_after_rechecksumming() {
        let mut bytes = formatted().encode().expect("encode");
        put_u32(&mut bytes, BLOCK_SIZE_OFFSET, 2048);
        put_u32(&mut bytes, SUPERBLOCK_CHECKSUM_OFFSET, 0);
        let checksum = superblock_checksum(&bytes);
        put_u32(&mut bytes, SUPERBLOCK_CHECKSUM_OFFSET, checksum);
        assert_eq!(
            Superblock::decode(&bytes, Some(IMAGE_BYTES), MountMode::ReadOnly),
            Err(Error::InvalidBlockSize(2048))
        );
    }

    #[test]
    fn incompatible_features_are_rejected() {
        let mut bytes = formatted().encode().expect("encode");
        put_u64(&mut bytes, INCOMPATIBLE_FEATURES_OFFSET, 1);
        put_u32(&mut bytes, SUPERBLOCK_CHECKSUM_OFFSET, 0);
        let checksum = superblock_checksum(&bytes);
        put_u32(&mut bytes, SUPERBLOCK_CHECKSUM_OFFSET, checksum);
        assert_eq!(
            Superblock::decode(&bytes, Some(IMAGE_BYTES), MountMode::ReadOnly),
            Err(Error::UnsupportedIncompatibleFeatures(1))
        );
    }

    #[test]
    fn unknown_read_only_features_reject_writes_only() {
        let mut bytes = formatted().encode().expect("encode");
        put_u64(&mut bytes, READ_ONLY_FEATURES_OFFSET, 1);
        put_u32(&mut bytes, SUPERBLOCK_CHECKSUM_OFFSET, 0);
        let checksum = superblock_checksum(&bytes);
        put_u32(&mut bytes, SUPERBLOCK_CHECKSUM_OFFSET, checksum);
        assert!(Superblock::decode(&bytes, Some(IMAGE_BYTES), MountMode::ReadOnly).is_ok());
        assert_eq!(
            Superblock::decode(&bytes, Some(IMAGE_BYTES), MountMode::ReadWrite),
            Err(Error::ReadOnlyFeaturesRequired(1))
        );
    }

    #[test]
    fn truncated_device_is_rejected() {
        let bytes = formatted().encode().expect("encode");
        assert_eq!(
            Superblock::decode(&bytes, Some(IMAGE_BYTES / 2), MountMode::ReadOnly),
            Err(Error::InvalidCapacity)
        );
    }

    #[test]
    fn arbitrary_blocks_do_not_panic() {
        for seed in 0_u8..=255 {
            let mut bytes = [seed; BLOCK_SIZE];
            bytes[0] ^= seed.rotate_left(1);
            let _ = Superblock::decode(&bytes, Some(IMAGE_BYTES), MountMode::ReadOnly);
        }
    }
}
