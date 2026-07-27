//! Phase 2 fixed-record linear directory blocks.

use crate::{BLOCK_SIZE, Error, NodeKind, crc32c};
use core::str;

pub const DIRECTORY_BLOCK_MAGIC: [u8; 8] = *b"NFSDIR\0\0";
pub const DIRECTORY_HEADER_BYTES: usize = 128;
pub const DIRECTORY_ENTRY_BYTES: usize = 128;
pub const DIRECTORY_ENTRIES_PER_BLOCK: usize = 31;
pub const DIRECTORY_NAME_CAPACITY: usize = 96;
pub const DIRECTORY_CHECKSUM_OFFSET: usize = 124;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub inode: u64,
    pub generation: u64,
    pub kind: NodeKind,
    name: [u8; DIRECTORY_NAME_CAPACITY],
    name_length: u8,
}

impl Default for DirectoryEntry {
    fn default() -> Self {
        Self {
            inode: 0,
            generation: 0,
            kind: NodeKind::Free,
            name: [0; DIRECTORY_NAME_CAPACITY],
            name_length: 0,
        }
    }
}

impl DirectoryEntry {
    pub fn new(inode: u64, generation: u64, kind: NodeKind, name: &str) -> Result<Self, Error> {
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes.len() > DIRECTORY_NAME_CAPACITY {
            return Err(Error::InvalidDirectoryName);
        }
        if bytes.contains(&0) || bytes.contains(&b'/') || name == "." || name == ".." {
            return Err(Error::InvalidDirectoryName);
        }
        if inode == 0 || generation == 0 || kind == NodeKind::Free {
            return Err(Error::InvalidDirectoryEntry);
        }
        let mut stored = [0; DIRECTORY_NAME_CAPACITY];
        stored[..bytes.len()].copy_from_slice(bytes);
        Ok(Self {
            inode,
            generation,
            kind,
            name: stored,
            name_length: bytes.len() as u8,
        })
    }
    pub fn name(&self) -> &str {
        str::from_utf8(&self.name[..usize::from(self.name_length)])
            .expect("validated directory name")
    }
    pub fn is_unused(&self) -> bool {
        self.inode == 0
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.is_unused() {
            return if self == &Self::default() {
                Ok(())
            } else {
                Err(Error::InvalidDirectoryEntry)
            };
        }
        if self.generation == 0 || self.kind == NodeKind::Free {
            return Err(Error::InvalidDirectoryEntry);
        }
        let length = usize::from(self.name_length);
        if length == 0
            || length > DIRECTORY_NAME_CAPACITY
            || self.name[length..].iter().any(|byte| *byte != 0)
        {
            return Err(Error::InvalidDirectoryName);
        }
        let name = str::from_utf8(&self.name[..length]).map_err(|_| Error::InvalidDirectoryName)?;
        if name.as_bytes().contains(&0)
            || name.as_bytes().contains(&b'/')
            || name == "."
            || name == ".."
        {
            return Err(Error::InvalidDirectoryName);
        }
        Ok(())
    }
    fn encode_into(&self, out: &mut [u8]) -> Result<(), Error> {
        self.validate()?;
        if self.is_unused() {
            return Ok(());
        }
        put_u64(out, 0, self.inode);
        put_u64(out, 8, self.generation);
        out[16] = self.kind as u16 as u8;
        out[17] = self.name_length;
        out[24..120].copy_from_slice(&self.name);
        Ok(())
    }
    fn decode_from(b: &[u8]) -> Result<Self, Error> {
        if b.iter().all(|byte| *byte == 0) {
            return Ok(Self::default());
        }
        if b[18..24].iter().any(|byte| *byte != 0) || b[120..128].iter().any(|byte| *byte != 0) {
            return Err(Error::NonZeroPhase2ReservedBytes);
        }
        let kind = match b[16] {
            1 => NodeKind::Regular,
            2 => NodeKind::Directory,
            3 => NodeKind::Symlink,
            value => return Err(Error::InvalidNodeKind(u16::from(value))),
        };
        let length = b[17];
        let mut name = [0; DIRECTORY_NAME_CAPACITY];
        name.copy_from_slice(&b[24..120]);
        let entry = Self {
            inode: get_u64(b, 0),
            generation: get_u64(b, 8),
            kind,
            name,
            name_length: length,
        };
        entry.validate()?;
        Ok(entry)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryBlock {
    pub owner_inode: u64,
    pub logical_block_index: u64,
    pub entries: [DirectoryEntry; DIRECTORY_ENTRIES_PER_BLOCK],
}

impl DirectoryBlock {
    pub fn new(owner_inode: u64, logical_block_index: u64) -> Result<Self, Error> {
        if owner_inode == 0 {
            return Err(Error::InvalidDirectoryBlock);
        }
        Ok(Self {
            owner_inode,
            logical_block_index,
            entries: [DirectoryEntry::default(); DIRECTORY_ENTRIES_PER_BLOCK],
        })
    }
    pub fn occupied_count(&self) -> u16 {
        self.entries
            .iter()
            .filter(|entry| !entry.is_unused())
            .count() as u16
    }
    pub fn cookie(&self, slot: usize) -> Result<u64, Error> {
        if slot >= DIRECTORY_ENTRIES_PER_BLOCK {
            return Err(Error::InvalidDirectoryEntry);
        }
        self.logical_block_index
            .checked_mul(DIRECTORY_ENTRIES_PER_BLOCK as u64)
            .and_then(|v| v.checked_add(slot as u64 + 1))
            .ok_or(Error::ArithmeticOverflow)
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.owner_inode == 0 {
            return Err(Error::InvalidDirectoryBlock);
        }
        for entry in &self.entries {
            entry.validate()?;
        }
        for left in 0..self.entries.len() {
            if self.entries[left].is_unused() {
                continue;
            }
            for right in left + 1..self.entries.len() {
                if !self.entries[right].is_unused()
                    && self.entries[left].name() == self.entries[right].name()
                {
                    return Err(Error::DuplicateDirectoryName);
                }
            }
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<[u8; BLOCK_SIZE], Error> {
        self.validate()?;
        let mut out = [0; BLOCK_SIZE];
        out[..8].copy_from_slice(&DIRECTORY_BLOCK_MAGIC);
        put_u16(&mut out, 8, 1);
        put_u16(&mut out, 10, DIRECTORY_HEADER_BYTES as u16);
        put_u16(&mut out, 12, DIRECTORY_ENTRY_BYTES as u16);
        put_u16(&mut out, 14, DIRECTORY_ENTRIES_PER_BLOCK as u16);
        put_u64(&mut out, 16, self.owner_inode);
        put_u64(&mut out, 24, self.logical_block_index);
        put_u32(&mut out, 32, u32::from(self.occupied_count()));
        for (index, entry) in self.entries.iter().enumerate() {
            let o = DIRECTORY_HEADER_BYTES + index * DIRECTORY_ENTRY_BYTES;
            entry.encode_into(&mut out[o..o + DIRECTORY_ENTRY_BYTES])?;
        }
        let checksum = directory_checksum(&out);
        put_u32(&mut out, DIRECTORY_CHECKSUM_OFFSET, checksum);
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < BLOCK_SIZE {
            return Err(Error::TruncatedPhase2Record {
                expected: BLOCK_SIZE,
                actual: bytes.len(),
            });
        }
        let b = &bytes[..BLOCK_SIZE];
        if b[..8] != DIRECTORY_BLOCK_MAGIC {
            return Err(Error::InvalidDirectoryBlock);
        }
        let stored = get_u32(b, DIRECTORY_CHECKSUM_OFFSET);
        let actual = directory_checksum(b);
        if stored != actual {
            return Err(Error::Phase2ChecksumMismatch {
                expected: stored,
                actual,
            });
        }
        if get_u16(b, 8) != 1
            || get_u16(b, 10) != DIRECTORY_HEADER_BYTES as u16
            || get_u16(b, 12) != DIRECTORY_ENTRY_BYTES as u16
            || get_u16(b, 14) != DIRECTORY_ENTRIES_PER_BLOCK as u16
            || get_u32(b, 36) != 0
            || b[40..124].iter().any(|byte| *byte != 0)
        {
            return Err(Error::InvalidDirectoryBlock);
        }
        let mut block = Self::new(get_u64(b, 16), get_u64(b, 24))?;
        for index in 0..DIRECTORY_ENTRIES_PER_BLOCK {
            let o = DIRECTORY_HEADER_BYTES + index * DIRECTORY_ENTRY_BYTES;
            block.entries[index] = DirectoryEntry::decode_from(&b[o..o + DIRECTORY_ENTRY_BYTES])?;
        }
        if get_u32(b, 32) != u32::from(block.occupied_count()) {
            return Err(Error::InvalidDirectoryBlock);
        }
        block.validate()?;
        Ok(block)
    }
}

fn directory_checksum(bytes: &[u8]) -> u32 {
    let mut b = [0; BLOCK_SIZE];
    b.copy_from_slice(&bytes[..BLOCK_SIZE]);
    b[DIRECTORY_CHECKSUM_OFFSET..DIRECTORY_CHECKSUM_OFFSET + 4].fill(0);
    crc32c(&b)
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
    fn directory_round_trip_and_cookies_are_stable() {
        let mut block = DirectoryBlock::new(1, 2).unwrap();
        block.entries[0] = DirectoryEntry::new(2, 9, NodeKind::Regular, "hello.txt").unwrap();
        block.entries[3] = DirectoryEntry::new(3, 10, NodeKind::Directory, "子").unwrap();
        let bytes = block.encode().unwrap();
        assert_eq!(&bytes[..8], &DIRECTORY_BLOCK_MAGIC);
        assert_eq!(get_u16(&bytes, 10), 128);
        assert_eq!(get_u16(&bytes, 14), 31);
        assert_ne!(get_u32(&bytes, 124), 0);
        assert_eq!(block.cookie(0).unwrap(), 63);
        assert_eq!(DirectoryBlock::decode(&bytes).unwrap(), block);
    }

    #[test]
    fn names_and_duplicates_are_rejected() {
        assert_eq!(
            DirectoryEntry::new(1, 1, NodeKind::Regular, ""),
            Err(Error::InvalidDirectoryName)
        );
        assert_eq!(
            DirectoryEntry::new(1, 1, NodeKind::Regular, "a/b"),
            Err(Error::InvalidDirectoryName)
        );
        assert_eq!(
            DirectoryEntry::new(1, 1, NodeKind::Regular, ".."),
            Err(Error::InvalidDirectoryName)
        );
        let long = "a".repeat(97);
        assert_eq!(
            DirectoryEntry::new(1, 1, NodeKind::Regular, &long),
            Err(Error::InvalidDirectoryName)
        );
        let mut block = DirectoryBlock::new(1, 0).unwrap();
        block.entries[0] = DirectoryEntry::new(2, 1, NodeKind::Regular, "same").unwrap();
        block.entries[1] = DirectoryEntry::new(3, 1, NodeKind::Regular, "same").unwrap();
        assert_eq!(block.validate(), Err(Error::DuplicateDirectoryName));
    }

    #[test]
    fn corruption_is_rejected() {
        let block = DirectoryBlock::new(1, 0).unwrap();
        let mut bytes = block.encode().unwrap();
        bytes[200] ^= 1;
        assert!(matches!(
            DirectoryBlock::decode(&bytes),
            Err(Error::Phase2ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn arbitrary_blocks_do_not_panic() {
        for byte in 0_u8..=255 {
            let input = [byte; BLOCK_SIZE];
            let _ = DirectoryBlock::decode(&input);
        }
    }
}
