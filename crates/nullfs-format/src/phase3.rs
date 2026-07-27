//! Phase 3 writable redo-journal and filesystem-state encodings.
//!
//! The journal is fixed at 130 contiguous blocks: two redundant controls,
//! followed by 64 tag blocks and the corresponding 64 full-block images.

use crate::{BLOCK_SIZE, Error, crc32c};

pub const PHASE3_MAX_UPDATES: usize = 64;
pub const JOURNAL_CONTROL_BLOCKS: u32 = 2;
pub const JOURNAL_TAG_BLOCKS: u32 = PHASE3_MAX_UPDATES as u32;
pub const JOURNAL_IMAGE_BLOCKS: u32 = PHASE3_MAX_UPDATES as u32;
pub const PHASE3_JOURNAL_BLOCK_COUNT: u32 =
    JOURNAL_CONTROL_BLOCKS + JOURNAL_TAG_BLOCKS + JOURNAL_IMAGE_BLOCKS;

pub const PHASE3_BACKUP_SUPERBLOCK_BLOCK: u64 = 18;
pub const PHASE3_FILESYSTEM_STATE_BLOCK: u64 = 19;
pub const PHASE3_JOURNAL_FIRST_BLOCK: u64 = 20;
pub const PHASE3_FIRST_ALLOCATABLE_BLOCK: u64 =
    PHASE3_JOURNAL_FIRST_BLOCK + PHASE3_JOURNAL_BLOCK_COUNT as u64;
pub const PHASE3_MINIMUM_VOLUME_BLOCKS: u64 = PHASE3_FIRST_ALLOCATABLE_BLOCK + 1;

pub const JOURNAL_CONTROL_MAGIC: [u8; 8] = *b"NFSJCTL\0";
pub const JOURNAL_TAG_MAGIC: [u8; 8] = *b"NFSJTAG\0";
pub const FILESYSTEM_STATE_MAGIC: [u8; 8] = *b"NFSSTAT\0";
pub const JOURNAL_HEADER_BYTES: u16 = 64;
pub const JOURNAL_CHECKSUM_OFFSET: usize = 60;
pub const FILESYSTEM_STATE_ORPHAN_HEAD_OFFSET: usize = 32;
pub const FILESYSTEM_STATE_FREE_BLOCK_COUNT_OFFSET: usize = 40;
pub const FILESYSTEM_STATE_FREE_INODE_COUNT_OFFSET: usize = 48;
pub const FILESYSTEM_STATE_CHECKSUM_OFFSET: usize = 60;
pub const INITIAL_GENERATION: u64 = 1;
pub const INITIAL_TRANSACTION_ID: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum JournalState {
    Empty = 0,
    Committed = 1,
}

impl JournalState {
    fn decode(value: u32) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Empty),
            1 => Ok(Self::Committed),
            _ => Err(Error::InvalidJournalState(value)),
        }
    }
}

/// Advance a persistent identifier. Zero is permanently reserved.
pub fn next_generation(current: u64) -> Result<u64, Error> {
    current.checked_add(1).ok_or(Error::IdentifierExhausted)
}

/// Advance a transaction identifier. Zero is permanently reserved.
pub fn next_transaction_id(current: u64) -> Result<u64, Error> {
    current.checked_add(1).ok_or(Error::IdentifierExhausted)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalTag {
    pub transaction_id: u64,
    pub target_home_block: u64,
    pub image_checksum: u32,
    pub update_index: u32,
}

impl JournalTag {
    pub fn new(
        transaction_id: u64,
        update_index: u32,
        target_home_block: u64,
        image: &[u8],
    ) -> Result<Self, Error> {
        if image.len() != BLOCK_SIZE {
            return Err(Error::InvalidJournalTag);
        }
        let tag = Self {
            transaction_id,
            target_home_block,
            image_checksum: crc32c(image),
            update_index,
        };
        tag.validate()?;
        Ok(tag)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.transaction_id == 0
            || self.target_home_block == 0
            || self.update_index >= PHASE3_MAX_UPDATES as u32
        {
            return Err(Error::InvalidJournalTag);
        }
        Ok(())
    }

    pub fn image_matches(&self, image: &[u8]) -> bool {
        image.len() == BLOCK_SIZE && crc32c(image) == self.image_checksum
    }

    pub fn encode(&self) -> Result<[u8; BLOCK_SIZE], Error> {
        self.validate()?;
        let mut out = [0; BLOCK_SIZE];
        out[..8].copy_from_slice(&JOURNAL_TAG_MAGIC);
        put_u16(&mut out, 8, 1);
        put_u16(&mut out, 10, JOURNAL_HEADER_BYTES);
        put_u64(&mut out, 16, self.transaction_id);
        put_u64(&mut out, 24, self.target_home_block);
        put_u32(&mut out, 32, self.image_checksum);
        put_u32(&mut out, 36, self.update_index);
        put_checksum(&mut out, JOURNAL_CHECKSUM_OFFSET);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let b = checked_block(bytes)?;
        verify_checksum(b, JOURNAL_CHECKSUM_OFFSET)?;
        if b[..8] != JOURNAL_TAG_MAGIC
            || get_u16(b, 8) != 1
            || get_u16(b, 10) != JOURNAL_HEADER_BYTES
            || b[12..16].iter().any(|v| *v != 0)
            || b[40..60].iter().any(|v| *v != 0)
            || b[64..].iter().any(|v| *v != 0)
        {
            return Err(Error::InvalidJournalTag);
        }
        let tag = Self {
            transaction_id: get_u64(b, 16),
            target_home_block: get_u64(b, 24),
            image_checksum: get_u32(b, 32),
            update_index: get_u32(b, 36),
        };
        tag.validate()?;
        Ok(tag)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalControl {
    pub generation: u64,
    pub transaction_id: u64,
    pub state: JournalState,
    pub update_count: u32,
    pub tags_digest: u32,
}

impl JournalControl {
    pub const fn empty(generation: u64) -> Self {
        Self {
            generation,
            transaction_id: 0,
            state: JournalState::Empty,
            update_count: 0,
            tags_digest: 0,
        }
    }

    pub fn committed(
        generation: u64,
        transaction_id: u64,
        tags: &[JournalTag],
    ) -> Result<Self, Error> {
        if generation == 0
            || transaction_id == 0
            || tags.is_empty()
            || tags.len() > PHASE3_MAX_UPDATES
        {
            return Err(Error::InvalidJournalControl);
        }
        validate_tag_sequence(transaction_id, tags)?;
        Ok(Self {
            generation,
            transaction_id,
            state: JournalState::Committed,
            update_count: tags.len() as u32,
            tags_digest: tags_digest(tags),
        })
    }

    pub fn validate(&self) -> Result<(), Error> {
        let canonical = self.generation != 0
            && match self.state {
                JournalState::Empty => {
                    self.transaction_id == 0 && self.update_count == 0 && self.tags_digest == 0
                }
                JournalState::Committed => {
                    self.transaction_id != 0
                        && self.update_count != 0
                        && self.update_count <= PHASE3_MAX_UPDATES as u32
                }
            };
        if canonical {
            Ok(())
        } else {
            Err(Error::InvalidJournalControl)
        }
    }

    /// Rejects missing, reordered, stale, or mixed transaction tags.
    pub fn validate_tags(&self, tags: &[JournalTag]) -> Result<(), Error> {
        self.validate()?;
        if self.state == JournalState::Empty {
            return if tags.is_empty() {
                Ok(())
            } else {
                Err(Error::JournalTagsMismatch)
            };
        }
        if tags.len() != self.update_count as usize {
            return Err(Error::JournalTagsMismatch);
        }
        validate_tag_sequence(self.transaction_id, tags)?;
        if tags_digest(tags) != self.tags_digest {
            return Err(Error::JournalTagsMismatch);
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<[u8; BLOCK_SIZE], Error> {
        self.validate()?;
        let mut out = [0; BLOCK_SIZE];
        out[..8].copy_from_slice(&JOURNAL_CONTROL_MAGIC);
        put_u16(&mut out, 8, 1);
        put_u16(&mut out, 10, JOURNAL_HEADER_BYTES);
        put_u64(&mut out, 16, self.generation);
        put_u64(&mut out, 24, self.transaction_id);
        put_u32(&mut out, 32, self.state as u32);
        put_u32(&mut out, 36, self.update_count);
        put_u32(&mut out, 40, self.tags_digest);
        put_checksum(&mut out, JOURNAL_CHECKSUM_OFFSET);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let b = checked_block(bytes)?;
        verify_checksum(b, JOURNAL_CHECKSUM_OFFSET)?;
        if b[..8] != JOURNAL_CONTROL_MAGIC
            || get_u16(b, 8) != 1
            || get_u16(b, 10) != JOURNAL_HEADER_BYTES
            || b[12..16].iter().any(|v| *v != 0)
            || b[44..60].iter().any(|v| *v != 0)
            || b[64..].iter().any(|v| *v != 0)
        {
            return Err(Error::InvalidJournalControl);
        }
        let control = Self {
            generation: get_u64(b, 16),
            transaction_id: get_u64(b, 24),
            state: JournalState::decode(get_u32(b, 32))?,
            update_count: get_u32(b, 36),
            tags_digest: get_u32(b, 40),
        };
        control.validate()?;
        Ok(control)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilesystemState {
    pub generation: u64,
    pub next_transaction_id: u64,
    /// First inode in the persistent orphan list, or zero when the list is empty.
    pub orphan_head: u64,
    pub free_block_count: u64,
    pub free_inode_count: u64,
}

impl FilesystemState {
    pub const fn initial() -> Self {
        Self {
            generation: INITIAL_GENERATION,
            next_transaction_id: INITIAL_TRANSACTION_ID,
            orphan_head: 0,
            free_block_count: 0,
            free_inode_count: 0,
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.generation == 0 || self.next_transaction_id == 0 {
            Err(Error::InvalidFilesystemState)
        } else {
            Ok(())
        }
    }
    pub fn encode(&self) -> Result<[u8; BLOCK_SIZE], Error> {
        self.validate()?;
        let mut out = [0; BLOCK_SIZE];
        out[..8].copy_from_slice(&FILESYSTEM_STATE_MAGIC);
        put_u16(&mut out, 8, 1);
        put_u16(&mut out, 10, JOURNAL_HEADER_BYTES);
        put_u64(&mut out, 16, self.generation);
        put_u64(&mut out, 24, self.next_transaction_id);
        put_u64(
            &mut out,
            FILESYSTEM_STATE_ORPHAN_HEAD_OFFSET,
            self.orphan_head,
        );
        put_u64(
            &mut out,
            FILESYSTEM_STATE_FREE_BLOCK_COUNT_OFFSET,
            self.free_block_count,
        );
        put_u64(
            &mut out,
            FILESYSTEM_STATE_FREE_INODE_COUNT_OFFSET,
            self.free_inode_count,
        );
        put_checksum(&mut out, FILESYSTEM_STATE_CHECKSUM_OFFSET);
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let b = checked_block(bytes)?;
        verify_checksum(b, FILESYSTEM_STATE_CHECKSUM_OFFSET)?;
        if b[..8] != FILESYSTEM_STATE_MAGIC
            || get_u16(b, 8) != 1
            || get_u16(b, 10) != JOURNAL_HEADER_BYTES
            || b[12..16]
                .iter()
                .chain(&b[56..60])
                .chain(&b[64..])
                .any(|v| *v != 0)
        {
            return Err(Error::InvalidFilesystemState);
        }
        let state = Self {
            generation: get_u64(b, 16),
            next_transaction_id: get_u64(b, 24),
            orphan_head: get_u64(b, FILESYSTEM_STATE_ORPHAN_HEAD_OFFSET),
            free_block_count: get_u64(b, FILESYSTEM_STATE_FREE_BLOCK_COUNT_OFFSET),
            free_inode_count: get_u64(b, FILESYSTEM_STATE_FREE_INODE_COUNT_OFFSET),
        };
        state.validate()?;
        Ok(state)
    }
}

pub fn bitmap_test(bitmap: &[u8], bit: usize) -> Option<bool> {
    bitmap.get(bit / 8).map(|byte| byte & (1 << (bit % 8)) != 0)
}
pub fn bitmap_set(bitmap: &mut [u8], bit: usize, value: bool) -> Result<(), Error> {
    let byte = bitmap.get_mut(bit / 8).ok_or(Error::BitmapOutOfBounds)?;
    let mask = 1 << (bit % 8);
    if value {
        *byte |= mask;
    } else {
        *byte &= !mask;
    }
    Ok(())
}
/// Unused high bits in the final byte and all following bytes must be zero.
pub fn validate_bitmap_tail(bitmap: &[u8], valid_bits: usize) -> Result<(), Error> {
    if valid_bits > bitmap.len().saturating_mul(8) {
        return Err(Error::BitmapOutOfBounds);
    }
    let full = valid_bits / 8;
    let remainder = valid_bits % 8;
    let tail_start = full + usize::from(remainder != 0);
    if remainder != 0 && bitmap[full] & !((1_u8 << remainder) - 1) != 0 {
        return Err(Error::NonCanonicalBitmapTail);
    }
    if bitmap[tail_start..].iter().any(|byte| *byte != 0) {
        return Err(Error::NonCanonicalBitmapTail);
    }
    Ok(())
}

fn validate_tag_sequence(transaction_id: u64, tags: &[JournalTag]) -> Result<(), Error> {
    for (index, tag) in tags.iter().enumerate() {
        tag.validate()?;
        if tag.transaction_id != transaction_id || tag.update_index != index as u32 {
            return Err(Error::JournalTagsMismatch);
        }
        if tags[..index]
            .iter()
            .any(|prior| prior.target_home_block == tag.target_home_block)
        {
            return Err(Error::JournalTagsMismatch);
        }
    }
    Ok(())
}
fn tags_digest(tags: &[JournalTag]) -> u32 {
    let mut canonical = [0_u8; PHASE3_MAX_UPDATES * 24];
    for (index, tag) in tags.iter().enumerate() {
        let o = index * 24;
        canonical[o..o + 8].copy_from_slice(&tag.transaction_id.to_le_bytes());
        canonical[o + 8..o + 16].copy_from_slice(&tag.target_home_block.to_le_bytes());
        canonical[o + 16..o + 20].copy_from_slice(&tag.image_checksum.to_le_bytes());
        canonical[o + 20..o + 24].copy_from_slice(&tag.update_index.to_le_bytes());
    }
    crc32c(&canonical[..tags.len() * 24])
}
fn checked_block(bytes: &[u8]) -> Result<&[u8], Error> {
    if bytes.len() < BLOCK_SIZE {
        Err(Error::TruncatedPhase3Block {
            actual: bytes.len(),
        })
    } else {
        Ok(&bytes[..BLOCK_SIZE])
    }
}
fn put_checksum(block: &mut [u8; BLOCK_SIZE], offset: usize) {
    let checksum = crc32c(block);
    put_u32(block, offset, checksum);
}
fn verify_checksum(block: &[u8], offset: usize) -> Result<(), Error> {
    let stored = get_u32(block, offset);
    let mut copy = [0; BLOCK_SIZE];
    copy.copy_from_slice(block);
    copy[offset..offset + 4].fill(0);
    let actual = crc32c(&copy);
    if stored == actual {
        Ok(())
    } else {
        Err(Error::Phase3ChecksumMismatch {
            expected: stored,
            actual,
        })
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
    #[test]
    fn exact_layout_round_trips_and_rejects_mixed_tags() {
        let image = [0x5a; BLOCK_SIZE];
        let tag = JournalTag::new(7, 0, 222, &image).unwrap();
        let bytes = tag.encode().unwrap();
        assert_eq!(&bytes[..8], &JOURNAL_TAG_MAGIC);
        assert_eq!(get_u64(&bytes, 16), 7);
        assert_eq!(get_u64(&bytes, 24), 222);
        assert_eq!(JournalTag::decode(&bytes).unwrap(), tag);
        assert!(tag.image_matches(&image));
        let control = JournalControl::committed(3, 7, &[tag]).unwrap();
        let encoded = control.encode().unwrap();
        assert_eq!(JournalControl::decode(&encoded).unwrap(), control);
        let stale = JournalTag {
            transaction_id: 8,
            ..tag
        };
        assert_eq!(
            control.validate_tags(&[stale]),
            Err(Error::JournalTagsMismatch)
        );
    }
    #[test]
    fn state_corruption_and_noncanonical_bytes_are_rejected() {
        let state = FilesystemState {
            generation: 7,
            next_transaction_id: 11,
            orphan_head: 23,
            free_block_count: 101,
            free_inode_count: 47,
        };
        let mut bytes = state.encode().unwrap();
        assert_eq!(get_u64(&bytes, FILESYSTEM_STATE_ORPHAN_HEAD_OFFSET), 23);
        assert_eq!(
            get_u64(&bytes, FILESYSTEM_STATE_FREE_BLOCK_COUNT_OFFSET),
            101
        );
        assert_eq!(
            get_u64(&bytes, FILESYSTEM_STATE_FREE_INODE_COUNT_OFFSET),
            47
        );
        assert_eq!(&bytes[56..60], &[0; 4]);
        assert_eq!(&bytes[64..], &[0; BLOCK_SIZE - 64]);
        assert_eq!(FilesystemState::decode(&bytes).unwrap(), state);
        bytes[20] ^= 1;
        assert!(matches!(
            FilesystemState::decode(&bytes),
            Err(Error::Phase3ChecksumMismatch { .. })
        ));
        let mut bytes = state.encode().unwrap();
        bytes[56] = 1;
        bytes[FILESYSTEM_STATE_CHECKSUM_OFFSET..FILESYSTEM_STATE_CHECKSUM_OFFSET + 4].fill(0);
        put_checksum(&mut bytes, FILESYSTEM_STATE_CHECKSUM_OFFSET);
        assert_eq!(
            FilesystemState::decode(&bytes),
            Err(Error::InvalidFilesystemState)
        );
        let mut bytes = state.encode().unwrap();
        bytes[100] = 1;
        bytes[FILESYSTEM_STATE_CHECKSUM_OFFSET..FILESYSTEM_STATE_CHECKSUM_OFFSET + 4].fill(0);
        put_checksum(&mut bytes, FILESYSTEM_STATE_CHECKSUM_OFFSET);
        assert_eq!(
            FilesystemState::decode(&bytes),
            Err(Error::InvalidFilesystemState)
        );
    }
    #[test]
    fn bitmap_tail_is_canonical() {
        let mut map = [0_u8; 2];
        bitmap_set(&mut map, 8, true).unwrap();
        assert!(bitmap_test(&map, 8).unwrap());
        validate_bitmap_tail(&map, 9).unwrap();
        map[1] |= 0x80;
        assert_eq!(
            validate_bitmap_tail(&map, 9),
            Err(Error::NonCanonicalBitmapTail)
        );
    }
    #[test]
    fn arbitrary_input_never_panics() {
        for value in 0_u8..=255 {
            let block = [value; BLOCK_SIZE];
            let _ = JournalTag::decode(&block);
            let _ = JournalControl::decode(&block);
            let _ = FilesystemState::decode(&block);
        }
    }
}
