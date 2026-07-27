//! Phase 2 inode and inline-extent encoding.

use crate::{Error, crc32c};

pub const INODE_BYTES: usize = 256;
pub const INODE_CHECKSUM_OFFSET: usize = 252;
pub const INLINE_EXTENT_COUNT: usize = 4;
pub const EXTENT_BYTES: usize = 24;
/// Byte offset of `parent_inode`, used as the next pointer for Phase 3 orphans.
pub const INODE_PARENT_OR_ORPHAN_NEXT_OFFSET: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InodeValidationMode {
    /// Phase 2 semantics: every allocated inode must have at least one link.
    Strict,
    /// A Phase 3 regular-file orphan: link count is zero and `parent_inode` is
    /// the next orphan inode number (zero terminates the list).
    Phase3Orphan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum NodeKind {
    Free = 0,
    Regular = 1,
    Directory = 2,
    Symlink = 3,
}

impl NodeKind {
    fn decode(value: u16) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Free),
            1 => Ok(Self::Regular),
            2 => Ok(Self::Directory),
            3 => Ok(Self::Symlink),
            _ => Err(Error::InvalidNodeKind(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timestamp {
    pub seconds: u64,
    pub nanoseconds: u32,
}

impl Timestamp {
    pub fn validate(self) -> Result<(), Error> {
        if self.nanoseconds >= 1_000_000_000 {
            Err(Error::InvalidTimestamp)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Extent {
    pub logical_first_block: u64,
    pub physical_first_block: u64,
    pub length_blocks: u32,
    pub flags: u32,
}

impl Extent {
    pub fn logical_end(self) -> Result<u64, Error> {
        self.logical_first_block
            .checked_add(u64::from(self.length_blocks))
            .ok_or(Error::ArithmeticOverflow)
    }
    pub fn physical_end(self) -> Result<u64, Error> {
        self.physical_first_block
            .checked_add(u64::from(self.length_blocks))
            .ok_or(Error::ArithmeticOverflow)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inode {
    pub kind: NodeKind,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub link_count: u32,
    pub flags: u32,
    pub generation: u64,
    pub size: u64,
    pub allocated_blocks: u64,
    pub parent_inode: u64,
    pub accessed: Timestamp,
    pub modified: Timestamp,
    pub changed: Timestamp,
    pub created: Timestamp,
    pub directory_entry_count: u64,
    pub extent_count: u16,
    pub extents: [Extent; INLINE_EXTENT_COUNT],
}

impl Default for Inode {
    fn default() -> Self {
        Self {
            kind: NodeKind::Free,
            mode: 0,
            uid: 0,
            gid: 0,
            link_count: 0,
            flags: 0,
            generation: 0,
            size: 0,
            allocated_blocks: 0,
            parent_inode: 0,
            accessed: Timestamp::default(),
            modified: Timestamp::default(),
            changed: Timestamp::default(),
            created: Timestamp::default(),
            directory_entry_count: 0,
            extent_count: 0,
            extents: [Extent::default(); INLINE_EXTENT_COUNT],
        }
    }
}

impl Inode {
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_with_mode(InodeValidationMode::Strict)
    }

    pub fn validate_with_mode(&self, validation: InodeValidationMode) -> Result<(), Error> {
        if self.kind == NodeKind::Free {
            return if validation == InodeValidationMode::Strict && self == &Self::default() {
                Ok(())
            } else {
                Err(Error::InvalidFreeInode)
            };
        }
        let links_valid = match validation {
            InodeValidationMode::Strict => self.link_count != 0,
            InodeValidationMode::Phase3Orphan => {
                self.kind == NodeKind::Regular && self.link_count == 0
            }
        };
        if self.mode & !0x0fff != 0 || !links_valid || self.generation == 0 || self.flags != 0 {
            return Err(Error::InvalidInode);
        }
        self.accessed.validate()?;
        self.modified.validate()?;
        self.changed.validate()?;
        self.created.validate()?;
        if usize::from(self.extent_count) > INLINE_EXTENT_COUNT {
            return Err(Error::TooManyExtents);
        }
        if self.kind != NodeKind::Directory && self.directory_entry_count != 0 {
            return Err(Error::InvalidInode);
        }
        if self.kind == NodeKind::Directory
            && (!self.size.is_multiple_of(4096) || self.parent_inode == 0)
        {
            return Err(Error::InvalidInode);
        }
        if self.kind == NodeKind::Symlink && (self.size != 0 || self.extent_count != 0) {
            return Err(Error::UnsupportedInlineSymlink);
        }
        let mut allocated = 0_u64;
        let mut previous_end = 0_u64;
        for (index, extent) in self.extents.iter().enumerate() {
            if index < usize::from(self.extent_count) {
                if extent.length_blocks == 0
                    || extent.flags != 0
                    || (index != 0 && extent.logical_first_block < previous_end)
                {
                    return Err(Error::InvalidExtent);
                }
                previous_end = extent.logical_end()?;
                let _ = extent.physical_end()?;
                allocated = allocated
                    .checked_add(u64::from(extent.length_blocks))
                    .ok_or(Error::ArithmeticOverflow)?;
                let logical_limit = self
                    .size
                    .checked_add(4095)
                    .ok_or(Error::ArithmeticOverflow)?
                    / 4096;
                if previous_end > logical_limit {
                    return Err(Error::InvalidExtent);
                }
            } else if *extent != Extent::default() {
                return Err(Error::InvalidExtent);
            }
        }
        if allocated != self.allocated_blocks {
            return Err(Error::InvalidExtent);
        }
        if self.kind == NodeKind::Directory {
            let blocks = self.size / 4096;
            let mut next = 0;
            for extent in &self.extents[..usize::from(self.extent_count)] {
                if extent.logical_first_block != next {
                    return Err(Error::InvalidExtent);
                }
                next = extent.logical_end()?;
            }
            if next != blocks {
                return Err(Error::InvalidExtent);
            }
        }
        Ok(())
    }

    /// Return the next orphan inode number after validating Phase 3 orphan semantics.
    pub fn orphan_next(&self) -> Result<u64, Error> {
        self.validate_with_mode(InodeValidationMode::Phase3Orphan)?;
        Ok(self.parent_inode)
    }

    pub fn encode(&self) -> Result<[u8; INODE_BYTES], Error> {
        self.encode_with_mode(InodeValidationMode::Strict)
    }

    pub fn encode_with_mode(
        &self,
        validation: InodeValidationMode,
    ) -> Result<[u8; INODE_BYTES], Error> {
        self.validate_with_mode(validation)?;
        if self.kind == NodeKind::Free {
            return Ok([0; INODE_BYTES]);
        }
        let mut out = [0; INODE_BYTES];
        put_u16(&mut out, 0, 1);
        put_u16(&mut out, 2, INODE_BYTES as u16);
        put_u16(&mut out, 4, self.kind as u16);
        put_u16(&mut out, 6, self.mode);
        put_u32(&mut out, 8, self.uid);
        put_u32(&mut out, 12, self.gid);
        put_u32(&mut out, 16, self.link_count);
        put_u32(&mut out, 20, self.flags);
        put_u64(&mut out, 24, self.generation);
        put_u64(&mut out, 32, self.size);
        put_u64(&mut out, 40, self.allocated_blocks);
        put_u64(&mut out, 48, self.parent_inode);
        put_timestamp(&mut out, 56, self.accessed);
        put_timestamp(&mut out, 72, self.modified);
        put_timestamp(&mut out, 88, self.changed);
        put_timestamp(&mut out, 104, self.created);
        put_u16(&mut out, 116, self.extent_count);
        put_u64(&mut out, 120, self.directory_entry_count);
        for (index, extent) in self.extents.iter().enumerate() {
            let o = 128 + index * EXTENT_BYTES;
            put_u64(&mut out, o, extent.logical_first_block);
            put_u64(&mut out, o + 8, extent.physical_first_block);
            put_u32(&mut out, o + 16, extent.length_blocks);
            put_u32(&mut out, o + 20, extent.flags);
        }
        let checksum = inode_checksum(&out);
        put_u32(&mut out, INODE_CHECKSUM_OFFSET, checksum);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        Self::decode_with_mode(bytes, InodeValidationMode::Strict)
    }

    pub fn decode_with_mode(bytes: &[u8], validation: InodeValidationMode) -> Result<Self, Error> {
        if bytes.len() < INODE_BYTES {
            return Err(Error::TruncatedPhase2Record {
                expected: INODE_BYTES,
                actual: bytes.len(),
            });
        }
        let b = &bytes[..INODE_BYTES];
        if b.iter().all(|byte| *byte == 0) {
            let inode = Self::default();
            inode.validate_with_mode(validation)?;
            return Ok(inode);
        }
        let stored = get_u32(b, INODE_CHECKSUM_OFFSET);
        let actual = inode_checksum(b);
        if stored != actual {
            return Err(Error::Phase2ChecksumMismatch {
                expected: stored,
                actual,
            });
        }
        if get_u16(b, 0) != 1
            || get_u16(b, 2) != INODE_BYTES as u16
            || b[68..72]
                .iter()
                .chain(&b[84..88])
                .chain(&b[100..104])
                .chain(&b[118..120])
                .chain(&b[224..252])
                .any(|byte| *byte != 0)
        {
            return Err(Error::NonZeroPhase2ReservedBytes);
        }
        let mut inode = Self {
            kind: NodeKind::decode(get_u16(b, 4))?,
            mode: get_u16(b, 6),
            uid: get_u32(b, 8),
            gid: get_u32(b, 12),
            link_count: get_u32(b, 16),
            flags: get_u32(b, 20),
            generation: get_u64(b, 24),
            size: get_u64(b, 32),
            allocated_blocks: get_u64(b, 40),
            parent_inode: get_u64(b, 48),
            accessed: get_timestamp(b, 56),
            modified: get_timestamp(b, 72),
            changed: get_timestamp(b, 88),
            created: get_timestamp(b, 104),
            extent_count: get_u16(b, 116),
            directory_entry_count: get_u64(b, 120),
            extents: [Extent::default(); INLINE_EXTENT_COUNT],
        };
        for index in 0..INLINE_EXTENT_COUNT {
            let o = 128 + index * EXTENT_BYTES;
            inode.extents[index] = Extent {
                logical_first_block: get_u64(b, o),
                physical_first_block: get_u64(b, o + 8),
                length_blocks: get_u32(b, o + 16),
                flags: get_u32(b, o + 20),
            };
        }
        inode.validate_with_mode(validation)?;
        Ok(inode)
    }
}

fn inode_checksum(bytes: &[u8]) -> u32 {
    let mut b = [0; INODE_BYTES];
    b.copy_from_slice(&bytes[..INODE_BYTES]);
    b[INODE_CHECKSUM_OFFSET..].fill(0);
    crc32c(&b)
}
fn put_timestamp(b: &mut [u8], o: usize, t: Timestamp) {
    put_u64(b, o, t.seconds);
    put_u32(b, o + 8, t.nanoseconds);
}
fn get_timestamp(b: &[u8], o: usize) -> Timestamp {
    Timestamp {
        seconds: get_u64(b, o),
        nanoseconds: get_u32(b, o + 8),
    }
}
fn get_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn get_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn get_u64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([
        b[o],
        b[o + 1],
        b[o + 2],
        b[o + 3],
        b[o + 4],
        b[o + 5],
        b[o + 6],
        b[o + 7],
    ])
}
fn put_u16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regular_inode() -> Inode {
        let mut inode = Inode {
            kind: NodeKind::Regular,
            mode: 0o644,
            uid: 10,
            gid: 20,
            link_count: 1,
            generation: 7,
            size: 3 * 4096,
            allocated_blocks: 2,
            ..Inode::default()
        };
        inode.extent_count = 2;
        inode.extents[0] = Extent {
            logical_first_block: 0,
            physical_first_block: 36,
            length_blocks: 1,
            flags: 0,
        };
        inode.extents[1] = Extent {
            logical_first_block: 2,
            physical_first_block: 40,
            length_blocks: 1,
            flags: 0,
        };
        inode
    }

    #[test]
    fn inode_round_trip_and_offsets_are_stable() {
        let inode = regular_inode();
        let bytes = inode.encode().unwrap();
        assert_eq!(get_u16(&bytes, 2), 256);
        assert_eq!(get_u16(&bytes, 116), 2);
        assert_eq!(get_u64(&bytes, 128), 0);
        assert_eq!(get_u64(&bytes, 136), 36);
        assert_ne!(get_u32(&bytes, 252), 0);
        assert_eq!(Inode::decode(&bytes).unwrap(), inode);
        assert_eq!(Inode::decode(&[0; INODE_BYTES]).unwrap(), Inode::default());
    }

    #[test]
    fn checksum_corruption_and_extent_invariants_are_rejected() {
        let mut bytes = regular_inode().encode().unwrap();
        bytes[32] ^= 1;
        assert!(matches!(
            Inode::decode(&bytes),
            Err(Error::Phase2ChecksumMismatch { .. })
        ));
        let mut inode = regular_inode();
        inode.extents[1].logical_first_block = 0;
        assert_eq!(inode.validate(), Err(Error::InvalidExtent));
        inode = regular_inode();
        inode.extent_count = 5;
        assert_eq!(inode.validate(), Err(Error::TooManyExtents));
        inode = regular_inode();
        inode.accessed.nanoseconds = 1_000_000_000;
        assert_eq!(inode.validate(), Err(Error::InvalidTimestamp));
    }

    #[test]
    fn phase3_orphan_mode_is_explicit_and_uses_parent_field_as_next() {
        let mut orphan = regular_inode();
        orphan.link_count = 0;
        orphan.parent_inode = 99;

        assert_eq!(orphan.validate(), Err(Error::InvalidInode));
        assert_eq!(orphan.orphan_next().unwrap(), 99);
        assert_eq!(INODE_PARENT_OR_ORPHAN_NEXT_OFFSET, 48);
        let bytes = orphan
            .encode_with_mode(InodeValidationMode::Phase3Orphan)
            .unwrap();
        assert_eq!(get_u32(&bytes, 16), 0);
        assert_eq!(get_u64(&bytes, INODE_PARENT_OR_ORPHAN_NEXT_OFFSET), 99);
        assert_eq!(Inode::decode(&bytes), Err(Error::InvalidInode));
        assert_eq!(
            Inode::decode_with_mode(&bytes, InodeValidationMode::Phase3Orphan).unwrap(),
            orphan
        );

        let mut linked = orphan.clone();
        linked.link_count = 1;
        assert_eq!(
            linked.validate_with_mode(InodeValidationMode::Phase3Orphan),
            Err(Error::InvalidInode)
        );
        let mut directory = orphan;
        directory.kind = NodeKind::Directory;
        assert_eq!(
            directory.validate_with_mode(InodeValidationMode::Phase3Orphan),
            Err(Error::InvalidInode)
        );
        assert_eq!(
            Inode::decode_with_mode(&[0; INODE_BYTES], InodeValidationMode::Phase3Orphan),
            Err(Error::InvalidFreeInode)
        );
    }

    #[test]
    fn symlink_kind_is_recognized_but_storage_is_reserved() {
        let inode = Inode {
            kind: NodeKind::Symlink,
            mode: 0o777,
            link_count: 1,
            generation: 1,
            size: 1,
            ..Inode::default()
        };
        assert_eq!(inode.validate(), Err(Error::UnsupportedInlineSymlink));
    }

    #[test]
    fn arbitrary_records_do_not_panic() {
        for byte in 0_u8..=255 {
            let input = [byte; INODE_BYTES];
            let _ = Inode::decode(&input);
        }
    }
}
