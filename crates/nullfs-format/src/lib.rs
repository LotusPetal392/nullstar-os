#![no_std]

//! Shared, representation-independent NullFS on-disk format definitions.

mod allocation_group;
mod checksum;
mod directory;
pub mod endian;
pub mod features;
mod inode;
mod phase3;
mod superblock;

use core::fmt;

pub use allocation_group::{
    ALLOCATION_GROUP_DESCRIPTOR_BYTES, ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK,
    ALLOCATION_GROUP_TABLE_CHECKSUM_OFFSET, ALLOCATION_GROUP_TABLE_HEADER_BYTES,
    ALLOCATION_GROUP_TABLE_MAGIC, AllocationGroupDescriptor, AllocationGroupTable,
    PHASE2_INODE_TABLE_BLOCKS, PHASE2_INODES_PER_GROUP,
};
pub use checksum::crc32c;
pub use directory::{
    DIRECTORY_BLOCK_MAGIC, DIRECTORY_CHECKSUM_OFFSET, DIRECTORY_ENTRIES_PER_BLOCK,
    DIRECTORY_ENTRY_BYTES, DIRECTORY_HEADER_BYTES, DIRECTORY_NAME_CAPACITY, DirectoryBlock,
    DirectoryEntry,
};
pub use features::{Features, INCOMPAT_PHASE2_CORE, INCOMPAT_PHASE3_WRITABLE_REDO};
pub use inode::{
    EXTENT_BYTES, Extent, INLINE_EXTENT_COUNT, INODE_BYTES, INODE_CHECKSUM_OFFSET,
    INODE_PARENT_OR_ORPHAN_NEXT_OFFSET, Inode, InodeValidationMode, NodeKind, Timestamp,
};
pub use phase3::{
    FILESYSTEM_STATE_CHECKSUM_OFFSET, FILESYSTEM_STATE_FREE_BLOCK_COUNT_OFFSET,
    FILESYSTEM_STATE_FREE_INODE_COUNT_OFFSET, FILESYSTEM_STATE_MAGIC,
    FILESYSTEM_STATE_ORPHAN_HEAD_OFFSET, FilesystemState, INITIAL_GENERATION,
    INITIAL_TRANSACTION_ID, JOURNAL_CHECKSUM_OFFSET, JOURNAL_CONTROL_BLOCKS, JOURNAL_CONTROL_MAGIC,
    JOURNAL_HEADER_BYTES, JOURNAL_IMAGE_BLOCKS, JOURNAL_TAG_BLOCKS, JOURNAL_TAG_MAGIC,
    JournalControl, JournalState, JournalTag, PHASE3_BACKUP_SUPERBLOCK_BLOCK,
    PHASE3_FILESYSTEM_STATE_BLOCK, PHASE3_FIRST_ALLOCATABLE_BLOCK, PHASE3_JOURNAL_BLOCK_COUNT,
    PHASE3_JOURNAL_FIRST_BLOCK, PHASE3_MAX_UPDATES, PHASE3_MINIMUM_VOLUME_BLOCKS, bitmap_set,
    bitmap_test, next_generation, next_transaction_id, validate_bitmap_tail,
};
pub use superblock::{FormatOptions, Superblock, VolumeState};

pub const MAGIC: [u8; 8] = *b"NULLFS\0\0";
pub const FORMAT_MAJOR: u16 = 1;
pub const FORMAT_MINOR: u16 = 2;
pub const PHASE1_FORMAT_MINOR: u16 = 0;
pub const PHASE2_FORMAT_MINOR: u16 = 1;
pub const PHASE3_FORMAT_MINOR: u16 = 2;
pub const BLOCK_SIZE: usize = 4096;
pub const BLOCK_SIZE_U64: u64 = BLOCK_SIZE as u64;
pub const RESERVED_BOOT_BYTES: u64 = 64 * 1024;
pub const SUPERBLOCK_BLOCK: u64 = RESERVED_BOOT_BYTES / BLOCK_SIZE_U64;
pub const SUPERBLOCK_OFFSET: u64 = RESERVED_BOOT_BYTES;
pub const FIRST_DESCRIPTOR_BLOCK: u64 = SUPERBLOCK_BLOCK + 1;
pub const SUPERBLOCK_HEADER_BYTES: u32 = 256;
pub const SUPERBLOCK_CHECKSUM_OFFSET: usize = BLOCK_SIZE - 4;
pub const LABEL_CAPACITY: usize = 64;
pub const DEFAULT_ALLOCATION_GROUP_BLOCKS: u64 = 8192;
pub const DEFAULT_DESCRIPTOR_RESERVATION_BLOCKS: u32 = 1;
pub const MINIMUM_VOLUME_BLOCKS: u64 = FIRST_DESCRIPTOR_BLOCK + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    TruncatedSuperblock { actual: usize },
    InvalidMagic,
    ChecksumMismatch { expected: u32, actual: u32 },
    UnsupportedMajorVersion(u16),
    UnsupportedMinorVersion(u16),
    InvalidHeaderSize(u32),
    InvalidBlockSize(u32),
    InvalidState(u32),
    InvalidUuid,
    InvalidLabel,
    NonZeroReservedBytes,
    UnsupportedIncompatibleFeatures(u64),
    ReadOnlyFeaturesRequired(u64),
    DirtyVolume,
    InvalidCapacity,
    InvalidAllocationGroupSize,
    InvalidAllocationGroupCount,
    InvalidDescriptorReservation,
    InvalidDescriptorStart,
    InvalidFirstAllocatableBlock,
    DeviceTooSmall,
    ArithmeticOverflow,
    LabelTooLong,
    LabelContainsNul,
    TruncatedPhase2Record { expected: usize, actual: usize },
    Phase2ChecksumMismatch { expected: u32, actual: u32 },
    NonZeroPhase2ReservedBytes,
    InvalidAllocationGroupTable,
    InvalidAllocationGroupDescriptor,
    TooManyAllocationGroupDescriptors,
    InvalidNodeKind(u16),
    InvalidTimestamp,
    InvalidInode,
    InvalidFreeInode,
    TooManyExtents,
    InvalidExtent,
    UnsupportedInlineSymlink,
    InvalidDirectoryBlock,
    InvalidDirectoryEntry,
    InvalidDirectoryName,
    DuplicateDirectoryName,
    InvalidPhase3Geometry,
    TruncatedPhase3Block { actual: usize },
    Phase3ChecksumMismatch { expected: u32, actual: u32 },
    InvalidJournalState(u32),
    InvalidJournalControl,
    InvalidJournalTag,
    JournalTagsMismatch,
    InvalidFilesystemState,
    IdentifierExhausted,
    BitmapOutOfBounds,
    NonCanonicalBitmapTail,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedSuperblock { actual } => {
                write!(
                    formatter,
                    "superblock is truncated: {actual} of {BLOCK_SIZE} bytes"
                )
            }
            Self::InvalidMagic => formatter.write_str("NullFS magic is invalid"),
            Self::ChecksumMismatch { expected, actual } => write!(
                formatter,
                "superblock checksum mismatch: stored={expected:#010x}, calculated={actual:#010x}"
            ),
            Self::UnsupportedMajorVersion(version) => {
                write!(formatter, "unsupported NullFS major version {version}")
            }
            Self::UnsupportedMinorVersion(version) => {
                write!(formatter, "unsupported NullFS minor version {version}")
            }
            Self::InvalidHeaderSize(size) => write!(formatter, "invalid header size {size}"),
            Self::InvalidBlockSize(size) => write!(formatter, "invalid block size {size}"),
            Self::InvalidState(state) => write!(formatter, "invalid volume state {state}"),
            Self::InvalidUuid => formatter.write_str("filesystem UUID may not be all zero"),
            Self::InvalidLabel => formatter.write_str("filesystem label is not canonical UTF-8"),
            Self::NonZeroReservedBytes => {
                formatter.write_str("reserved superblock bytes are nonzero")
            }
            Self::UnsupportedIncompatibleFeatures(features) => write!(
                formatter,
                "unsupported incompatible feature bits {features:#018x}"
            ),
            Self::ReadOnlyFeaturesRequired(features) => write!(
                formatter,
                "unknown read-only-compatible feature bits require read-only access: {features:#018x}"
            ),
            Self::DirtyVolume => {
                formatter.write_str("dirty volume is not writable without recovery")
            }
            Self::InvalidCapacity => formatter.write_str("invalid filesystem capacity"),
            Self::InvalidAllocationGroupSize => {
                formatter.write_str("invalid allocation-group size")
            }
            Self::InvalidAllocationGroupCount => {
                formatter.write_str("invalid allocation-group count")
            }
            Self::InvalidDescriptorReservation => {
                formatter.write_str("invalid descriptor reservation")
            }
            Self::InvalidDescriptorStart => formatter.write_str("invalid descriptor start block"),
            Self::InvalidFirstAllocatableBlock => {
                formatter.write_str("invalid first allocatable block")
            }
            Self::DeviceTooSmall => formatter.write_str("device is too small for NullFS"),
            Self::ArithmeticOverflow => formatter.write_str("filesystem geometry overflowed"),
            Self::LabelTooLong => formatter.write_str("filesystem label exceeds 64 bytes"),
            Self::LabelContainsNul => formatter.write_str("filesystem label contains NUL"),
            Self::TruncatedPhase2Record { expected, actual } => write!(
                formatter,
                "Phase 2 record is truncated: {actual} of {expected} bytes"
            ),
            Self::Phase2ChecksumMismatch { expected, actual } => write!(
                formatter,
                "Phase 2 checksum mismatch: stored={expected:#010x}, calculated={actual:#010x}"
            ),
            Self::NonZeroPhase2ReservedBytes => {
                formatter.write_str("reserved Phase 2 bytes are nonzero")
            }
            Self::InvalidAllocationGroupTable => {
                formatter.write_str("invalid allocation-group descriptor table")
            }
            Self::InvalidAllocationGroupDescriptor => {
                formatter.write_str("invalid allocation-group descriptor")
            }
            Self::TooManyAllocationGroupDescriptors => {
                formatter.write_str("too many allocation-group descriptors in one block")
            }
            Self::InvalidNodeKind(kind) => write!(formatter, "invalid inode kind {kind}"),
            Self::InvalidTimestamp => formatter.write_str("invalid inode timestamp"),
            Self::InvalidInode => formatter.write_str("invalid inode"),
            Self::InvalidFreeInode => formatter.write_str("free inode is not canonically zero"),
            Self::TooManyExtents => formatter.write_str("inode has too many inline extents"),
            Self::InvalidExtent => formatter.write_str("invalid inline extent layout"),
            Self::UnsupportedInlineSymlink => {
                formatter.write_str("inline symlink storage is not defined")
            }
            Self::InvalidDirectoryBlock => formatter.write_str("invalid directory block"),
            Self::InvalidDirectoryEntry => formatter.write_str("invalid directory entry"),
            Self::InvalidDirectoryName => formatter.write_str("invalid directory name"),
            Self::DuplicateDirectoryName => formatter.write_str("duplicate directory name"),
            Self::InvalidPhase3Geometry => formatter.write_str("invalid Phase 3 geometry"),
            Self::TruncatedPhase3Block { actual } => write!(
                formatter,
                "Phase 3 block is truncated: {actual} of {BLOCK_SIZE} bytes"
            ),
            Self::Phase3ChecksumMismatch { expected, actual } => write!(
                formatter,
                "Phase 3 checksum mismatch: stored={expected:#010x}, calculated={actual:#010x}"
            ),
            Self::InvalidJournalState(state) => write!(formatter, "invalid journal state {state}"),
            Self::InvalidJournalControl => formatter.write_str("invalid journal control block"),
            Self::InvalidJournalTag => formatter.write_str("invalid journal tag block"),
            Self::JournalTagsMismatch => formatter.write_str("journal tags do not match control"),
            Self::InvalidFilesystemState => formatter.write_str("invalid filesystem-state block"),
            Self::IdentifierExhausted => formatter.write_str("persistent identifier exhausted"),
            Self::BitmapOutOfBounds => formatter.write_str("bitmap bit is out of bounds"),
            Self::NonCanonicalBitmapTail => {
                formatter.write_str("bitmap tail is not canonically zero")
            }
        }
    }
}

pub fn complete_blocks(device_bytes: u64) -> u64 {
    device_bytes / BLOCK_SIZE_U64
}
