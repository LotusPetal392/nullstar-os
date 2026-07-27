//! Phase 2 allocation-group descriptor table encoding.

use crate::{BLOCK_SIZE, Error, Superblock, crc32c};

pub const ALLOCATION_GROUP_TABLE_MAGIC: [u8; 8] = *b"NFSAGDT\0";
pub const ALLOCATION_GROUP_TABLE_HEADER_BYTES: usize = 64;
pub const ALLOCATION_GROUP_DESCRIPTOR_BYTES: usize = 96;
pub const ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK: usize = 42;
pub const ALLOCATION_GROUP_TABLE_CHECKSUM_OFFSET: usize = 60;
pub const PHASE2_INODES_PER_GROUP: u32 = 256;
pub const PHASE2_INODE_TABLE_BLOCKS: u32 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AllocationGroupDescriptor {
    pub group_index: u32,
    pub flags: u32,
    pub group_start_block: u64,
    pub group_block_count: u64,
    pub block_bitmap_block: u64,
    pub inode_bitmap_block: u64,
    pub inode_table_first_block: u64,
    pub inode_table_block_count: u32,
    pub inodes_in_group: u32,
    pub first_data_block: u64,
    pub data_end_block: u64,
    pub root_inode_index: Option<u32>,
}

impl AllocationGroupDescriptor {
    pub fn validate(&self, superblock: &Superblock) -> Result<(), Error> {
        if self.flags != 0 || self.group_index >= superblock.allocation_group_count {
            return Err(Error::InvalidAllocationGroupDescriptor);
        }
        let expected_start = u64::from(self.group_index)
            .checked_mul(superblock.allocation_group_blocks)
            .ok_or(Error::ArithmeticOverflow)?;
        let expected_count = superblock
            .capacity_blocks
            .checked_sub(expected_start)
            .ok_or(Error::InvalidAllocationGroupDescriptor)?
            .min(superblock.allocation_group_blocks);
        if self.group_start_block != expected_start
            || self.group_block_count != expected_count
            || self.inode_table_block_count != PHASE2_INODE_TABLE_BLOCKS
            || self.inodes_in_group != PHASE2_INODES_PER_GROUP
            || self.first_data_block > self.data_end_block
            || self.data_end_block != expected_start + expected_count
        {
            return Err(Error::InvalidAllocationGroupDescriptor);
        }
        let group_end = expected_start
            .checked_add(expected_count)
            .ok_or(Error::ArithmeticOverflow)?;
        let inode_table_end = self
            .inode_table_first_block
            .checked_add(u64::from(self.inode_table_block_count))
            .ok_or(Error::ArithmeticOverflow)?;
        let metadata_floor = expected_start.max(superblock.first_allocatable_block);
        if self.block_bitmap_block < metadata_floor
            || self.block_bitmap_block >= group_end
            || self.inode_bitmap_block < metadata_floor
            || self.inode_bitmap_block >= group_end
            || self.inode_table_first_block < metadata_floor
            || inode_table_end > group_end
            || self.first_data_block < inode_table_end
            || self.block_bitmap_block == self.inode_bitmap_block
            || (self.block_bitmap_block >= self.inode_table_first_block
                && self.block_bitmap_block < inode_table_end)
            || (self.inode_bitmap_block >= self.inode_table_first_block
                && self.inode_bitmap_block < inode_table_end)
            || self
                .root_inode_index
                .is_some_and(|index| index >= PHASE2_INODES_PER_GROUP)
        {
            return Err(Error::InvalidAllocationGroupDescriptor);
        }
        Ok(())
    }

    fn encode_into(&self, output: &mut [u8]) {
        put_u32(output, 0, self.group_index);
        put_u32(output, 4, self.flags);
        put_u64(output, 8, self.group_start_block);
        put_u64(output, 16, self.group_block_count);
        put_u64(output, 24, self.block_bitmap_block);
        put_u64(output, 32, self.inode_bitmap_block);
        put_u64(output, 40, self.inode_table_first_block);
        put_u32(output, 48, self.inode_table_block_count);
        put_u32(output, 52, self.inodes_in_group);
        put_u64(output, 56, self.first_data_block);
        put_u64(output, 64, self.data_end_block);
        put_u32(output, 72, self.root_inode_index.unwrap_or(u32::MAX));
    }

    fn decode_from(input: &[u8]) -> Result<Self, Error> {
        if input[76..96].iter().any(|byte| *byte != 0) {
            return Err(Error::NonZeroPhase2ReservedBytes);
        }
        let root = get_u32(input, 72);
        Ok(Self {
            group_index: get_u32(input, 0),
            flags: get_u32(input, 4),
            group_start_block: get_u64(input, 8),
            group_block_count: get_u64(input, 16),
            block_bitmap_block: get_u64(input, 24),
            inode_bitmap_block: get_u64(input, 32),
            inode_table_first_block: get_u64(input, 40),
            inode_table_block_count: get_u32(input, 48),
            inodes_in_group: get_u32(input, 52),
            first_data_block: get_u64(input, 56),
            data_end_block: get_u64(input, 64),
            root_inode_index: (root != u32::MAX).then_some(root),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocationGroupTable {
    pub first_descriptor_index: u32,
    pub total_descriptor_count: u32,
    pub table_block_index: u32,
    pub table_block_count: u32,
    pub physical_block: u64,
    pub descriptor_count: u16,
    pub descriptors: [AllocationGroupDescriptor; ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK],
}

impl AllocationGroupTable {
    pub fn new(
        first_descriptor_index: u32,
        total_descriptor_count: u32,
        table_block_index: u32,
        table_block_count: u32,
        physical_block: u64,
    ) -> Self {
        Self {
            first_descriptor_index,
            total_descriptor_count,
            table_block_index,
            table_block_count,
            physical_block,
            descriptor_count: 0,
            descriptors: [AllocationGroupDescriptor::default();
                ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK],
        }
    }

    pub fn push(&mut self, descriptor: AllocationGroupDescriptor) -> Result<(), Error> {
        let index = usize::from(self.descriptor_count);
        if index >= ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK {
            return Err(Error::TooManyAllocationGroupDescriptors);
        }
        self.descriptors[index] = descriptor;
        self.descriptor_count += 1;
        Ok(())
    }

    pub fn validate(&self, superblock: &Superblock) -> Result<(), Error> {
        let count = usize::from(self.descriptor_count);
        if !superblock.phase2_enabled()
            || count > ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK
            || self.total_descriptor_count != superblock.allocation_group_count
            || self.table_block_count != superblock.descriptor_reservation_blocks
            || self.table_block_index >= self.table_block_count
            || self.physical_block
                != superblock
                    .first_descriptor_block
                    .checked_add(u64::from(self.table_block_index))
                    .ok_or(Error::ArithmeticOverflow)?
            || self.first_descriptor_index
                != self
                    .table_block_index
                    .saturating_mul(ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK as u32)
            || u64::from(self.first_descriptor_index) + count as u64
                > u64::from(self.total_descriptor_count)
        {
            return Err(Error::InvalidAllocationGroupTable);
        }
        let expected_count = (self.total_descriptor_count - self.first_descriptor_index)
            .min(ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK as u32)
            as usize;
        if count != expected_count {
            return Err(Error::InvalidAllocationGroupTable);
        }
        for (offset, descriptor) in self.descriptors[..count].iter().enumerate() {
            if descriptor.group_index != self.first_descriptor_index + offset as u32 {
                return Err(Error::InvalidAllocationGroupTable);
            }
            descriptor.validate(superblock)?;
        }
        if self.descriptors[count..]
            .iter()
            .any(|descriptor| *descriptor != AllocationGroupDescriptor::default())
        {
            return Err(Error::InvalidAllocationGroupTable);
        }
        Ok(())
    }

    pub fn encode(&self, superblock: &Superblock) -> Result<[u8; BLOCK_SIZE], Error> {
        self.validate(superblock)?;
        let mut output = [0; BLOCK_SIZE];
        output[..8].copy_from_slice(&ALLOCATION_GROUP_TABLE_MAGIC);
        put_u16(&mut output, 8, 1);
        put_u16(&mut output, 10, ALLOCATION_GROUP_TABLE_HEADER_BYTES as u16);
        put_u16(&mut output, 12, ALLOCATION_GROUP_DESCRIPTOR_BYTES as u16);
        put_u16(&mut output, 14, self.descriptor_count);
        put_u32(&mut output, 16, self.first_descriptor_index);
        put_u32(&mut output, 20, self.total_descriptor_count);
        put_u32(&mut output, 24, self.table_block_index);
        put_u32(&mut output, 28, self.table_block_count);
        put_u64(&mut output, 32, self.physical_block);
        for index in 0..usize::from(self.descriptor_count) {
            let start =
                ALLOCATION_GROUP_TABLE_HEADER_BYTES + index * ALLOCATION_GROUP_DESCRIPTOR_BYTES;
            self.descriptors[index]
                .encode_into(&mut output[start..start + ALLOCATION_GROUP_DESCRIPTOR_BYTES]);
        }
        let checksum = table_checksum(&output);
        put_u32(
            &mut output,
            ALLOCATION_GROUP_TABLE_CHECKSUM_OFFSET,
            checksum,
        );
        Ok(output)
    }

    pub fn decode(bytes: &[u8], superblock: &Superblock) -> Result<Self, Error> {
        if bytes.len() < BLOCK_SIZE {
            return Err(Error::TruncatedPhase2Record {
                expected: BLOCK_SIZE,
                actual: bytes.len(),
            });
        }
        let input = &bytes[..BLOCK_SIZE];
        if input[..8] != ALLOCATION_GROUP_TABLE_MAGIC {
            return Err(Error::InvalidAllocationGroupTable);
        }
        let stored = get_u32(input, ALLOCATION_GROUP_TABLE_CHECKSUM_OFFSET);
        let actual = table_checksum(input);
        if stored != actual {
            return Err(Error::Phase2ChecksumMismatch {
                expected: stored,
                actual,
            });
        }
        if get_u16(input, 8) != 1
            || get_u16(input, 10) != ALLOCATION_GROUP_TABLE_HEADER_BYTES as u16
            || get_u16(input, 12) != ALLOCATION_GROUP_DESCRIPTOR_BYTES as u16
            || input[40..60].iter().any(|byte| *byte != 0)
        {
            return Err(Error::InvalidAllocationGroupTable);
        }
        let descriptor_count = get_u16(input, 14);
        if usize::from(descriptor_count) > ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK {
            return Err(Error::TooManyAllocationGroupDescriptors);
        }
        let mut table = Self::new(
            get_u32(input, 16),
            get_u32(input, 20),
            get_u32(input, 24),
            get_u32(input, 28),
            get_u64(input, 32),
        );
        for index in 0..usize::from(descriptor_count) {
            let start =
                ALLOCATION_GROUP_TABLE_HEADER_BYTES + index * ALLOCATION_GROUP_DESCRIPTOR_BYTES;
            table.push(AllocationGroupDescriptor::decode_from(
                &input[start..start + ALLOCATION_GROUP_DESCRIPTOR_BYTES],
            )?)?;
        }
        let used_end = ALLOCATION_GROUP_TABLE_HEADER_BYTES
            + usize::from(descriptor_count) * ALLOCATION_GROUP_DESCRIPTOR_BYTES;
        if input[used_end..].iter().enumerate().any(|(offset, byte)| {
            let absolute = used_end + offset;
            *byte != 0
                && !(ALLOCATION_GROUP_TABLE_CHECKSUM_OFFSET
                    ..ALLOCATION_GROUP_TABLE_CHECKSUM_OFFSET + 4)
                    .contains(&absolute)
        }) {
            return Err(Error::NonZeroPhase2ReservedBytes);
        }
        table.validate(superblock)?;
        Ok(table)
    }
}

fn table_checksum(bytes: &[u8]) -> u32 {
    let mut input = [0; BLOCK_SIZE];
    input.copy_from_slice(&bytes[..BLOCK_SIZE]);
    input[ALLOCATION_GROUP_TABLE_CHECKSUM_OFFSET..ALLOCATION_GROUP_TABLE_CHECKSUM_OFFSET + 4]
        .fill(0);
    crc32c(&input)
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
    use crate::FormatOptions;

    const UUID: [u8; 16] = [1; 16];

    fn superblock() -> Superblock {
        Superblock::format_phase2(FormatOptions::new(4096 * 4096, UUID, "phase2")).unwrap()
    }

    fn descriptor() -> AllocationGroupDescriptor {
        AllocationGroupDescriptor {
            group_index: 0,
            flags: 0,
            group_start_block: 0,
            group_block_count: 4096,
            block_bitmap_block: 18,
            inode_bitmap_block: 19,
            inode_table_first_block: 20,
            inode_table_block_count: 16,
            inodes_in_group: 256,
            first_data_block: 36,
            data_end_block: 4096,
            root_inode_index: Some(0),
        }
    }

    #[test]
    fn table_round_trip_has_exact_layout_and_checksum() {
        let sb = superblock();
        let mut table = AllocationGroupTable::new(0, 1, 0, 1, 17);
        table.push(descriptor()).unwrap();
        let bytes = table.encode(&sb).unwrap();
        assert_eq!(&bytes[..8], &ALLOCATION_GROUP_TABLE_MAGIC);
        assert_eq!(get_u16(&bytes, 10), 64);
        assert_eq!(get_u16(&bytes, 12), 96);
        assert_ne!(get_u32(&bytes, 60), 0);
        assert_eq!(AllocationGroupTable::decode(&bytes, &sb).unwrap(), table);
    }

    #[test]
    fn corruption_and_bad_geometry_are_rejected() {
        let sb = superblock();
        let mut table = AllocationGroupTable::new(0, 1, 0, 1, 17);
        let mut desc = descriptor();
        table.push(desc).unwrap();
        let mut bytes = table.encode(&sb).unwrap();
        bytes[70] ^= 1;
        assert!(matches!(
            AllocationGroupTable::decode(&bytes, &sb),
            Err(Error::Phase2ChecksumMismatch { .. })
        ));
        desc.first_data_block = 35;
        assert_eq!(
            desc.validate(&sb),
            Err(Error::InvalidAllocationGroupDescriptor)
        );
    }

    #[test]
    fn arbitrary_blocks_do_not_panic() {
        let sb = superblock();
        for byte in 0_u8..=255 {
            let input = [byte; BLOCK_SIZE];
            let _ = AllocationGroupTable::decode(&input, &sb);
        }
    }
}
