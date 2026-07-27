#![no_std]

//! Deterministic fresh-image construction for NullFS tests and host tooling.

extern crate alloc;

use alloc::{string::String, vec, vec::Vec};
use core::fmt;

use nullfs_blockdev::{BlockDevice, BlockDeviceError, checked_block_range};
use nullfs_format::{
    ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK, AllocationGroupDescriptor, AllocationGroupTable,
    BLOCK_SIZE, BLOCK_SIZE_U64, DIRECTORY_ENTRIES_PER_BLOCK, DirectoryBlock, DirectoryEntry,
    Error as FormatError, Extent, FIRST_DESCRIPTOR_BLOCK, FilesystemState, FormatOptions,
    INLINE_EXTENT_COUNT, INODE_BYTES, Inode, JournalControl, NodeKind, PHASE2_INODE_TABLE_BLOCKS,
    PHASE2_INODES_PER_GROUP, SUPERBLOCK_BLOCK, Superblock, bitmap_set, bitmap_test,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Device(BlockDeviceError),
    Format(FormatError),
    DeviceTooSmall,
    TooManyGroups,
    TooManyInodes,
    TooManyExtents,
    NoSpace,
    InvalidParent,
    DuplicateName,
    InvalidSparseSegment,
    ArithmeticOverflow,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(error) => write!(formatter, "block device: {error}"),
            Self::Format(error) => write!(formatter, "format: {error}"),
            Self::DeviceTooSmall => formatter.write_str("device is too small for Phase 2 metadata"),
            Self::TooManyGroups => {
                formatter.write_str("descriptor reservation cannot hold all groups")
            }
            Self::TooManyInodes => formatter.write_str("Phase 2 inode tables are full"),
            Self::TooManyExtents => formatter.write_str("file exceeds the inline extent limit"),
            Self::NoSpace => formatter.write_str("NullFS image has no suitable data extent"),
            Self::InvalidParent => formatter.write_str("parent is not an allocated directory"),
            Self::DuplicateName => formatter.write_str("directory already contains that name"),
            Self::InvalidSparseSegment => {
                formatter.write_str("invalid or overlapping sparse segment")
            }
            Self::ArithmeticOverflow => formatter.write_str("image geometry overflowed"),
        }
    }
}

impl From<BlockDeviceError> for Error {
    fn from(value: BlockDeviceError) -> Self {
        Self::Device(value)
    }
}

impl From<FormatError> for Error {
    fn from(value: FormatError) -> Self {
        Self::Format(value)
    }
}

/// Persistence behavior applied when a configured flush operation fails.
///
/// Selections operate on complete blocks dirtied since the last successful
/// flush or restart. `torn_prefix_bytes` limits every selected block to a
/// durable prefix; `None` persists each selected block completely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PersistencePolicy {
    /// Preserve the default all-or-nothing barrier behavior: persist nothing.
    #[default]
    Atomic,
    /// Persist the first `blocks` distinct blocks in first-dirtied order.
    DirtyBlockPrefix {
        blocks: usize,
        torn_prefix_bytes: Option<usize>,
    },
    /// Persist dirty blocks whose physical numbers occur in `blocks`.
    DirtyBlockSet {
        blocks: Vec<u64>,
        torn_prefix_bytes: Option<usize>,
    },
    /// Persist the first `writes` block-write records in forward or reverse order.
    ///
    /// Reverse order can expose older data when an epoch writes a block more
    /// than once. A multi-block write contributes one record per block.
    WritePrefix {
        writes: usize,
        reverse: bool,
        torn_prefix_bytes: Option<usize>,
    },
}

/// One complete block write recorded in the current flush epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochBlockWrite {
    pub block: u64,
    pub bytes: Vec<u8>,
}

/// Deterministic volatile/durable block-device model for crash-recovery tests.
///
/// Reads and writes use the volatile image. Successful [`BlockDevice::flush`]
/// calls are atomic barriers. On a configured failed flush, the opt-in
/// [`PersistencePolicy`] determines which dirty data reaches durable storage.
/// A configured operation fault leaves the device crashed and returns
/// [`BlockDeviceError::Io`]. While crashed, reads, writes, and flushes fail until
/// [`Self::restart`] discards volatile state and begins a new flush epoch.
pub struct CrashBlockDevice<D> {
    inner: D,
    block_size: usize,
    block_count: u64,
    volatile: Vec<u8>,
    durable: Vec<u8>,
    operation_count: u64,
    fail_at_operation: Option<u64>,
    torn_write_prefix: Option<usize>,
    persistence_policy: PersistencePolicy,
    dirty_blocks: Vec<u64>,
    epoch_writes: Vec<EpochBlockWrite>,
    last_persisted_blocks: Vec<u64>,
    crashed: bool,
}

impl<D: BlockDevice> CrashBlockDevice<D> {
    /// Snapshots the current contents of `inner` as both volatile and durable.
    pub fn new(mut inner: D) -> Result<Self, BlockDeviceError> {
        let block_size = inner.block_size();
        if block_size == 0 {
            return Err(BlockDeviceError::InvalidBlockSize);
        }
        let length = usize::try_from(inner.block_count())
            .ok()
            .and_then(|count| count.checked_mul(block_size))
            .ok_or(BlockDeviceError::ArithmeticOverflow)?;
        let block_count = inner.block_count();
        let mut durable = vec![0; length];
        inner.read_blocks(0, &mut durable)?;
        Ok(Self {
            inner,
            block_size,
            block_count,
            volatile: durable.clone(),
            durable,
            operation_count: 0,
            fail_at_operation: None,
            torn_write_prefix: None,
            persistence_policy: PersistencePolicy::Atomic,
            dirty_blocks: Vec::new(),
            epoch_writes: Vec::new(),
            last_persisted_blocks: Vec::new(),
            crashed: false,
        })
    }

    /// Configures the zero-based write/flush operation that will fail.
    pub fn fail_at_operation(&mut self, operation: Option<u64>) {
        self.fail_at_operation = operation;
    }

    /// Persists this many bytes from the first block of a faulted write.
    ///
    /// `None` (the default) fails before changing durable state. Values larger
    /// than one block are clamped to one block and to the write buffer length.
    pub fn set_torn_write_prefix(&mut self, bytes: Option<usize>) {
        self.torn_write_prefix = bytes;
    }

    /// Configures persistence on failed flushes. The default is `Atomic`.
    pub fn set_persistence_policy(&mut self, policy: PersistencePolicy) {
        self.persistence_policy = policy;
    }

    pub const fn operation_count(&self) -> u64 {
        self.operation_count
    }

    pub const fn is_crashed(&self) -> bool {
        self.crashed
    }

    pub const fn configured_failure(&self) -> Option<u64> {
        self.fail_at_operation
    }

    pub const fn persistence_policy(&self) -> &PersistencePolicy {
        &self.persistence_policy
    }

    /// Distinct blocks in first-dirtied order for the current flush epoch.
    pub fn dirty_blocks(&self) -> &[u64] {
        &self.dirty_blocks
    }

    /// Complete block-write records in issue order for the current flush epoch.
    pub fn epoch_writes(&self) -> &[EpochBlockWrite] {
        &self.epoch_writes
    }

    /// Blocks changed by the most recent durable transition.
    pub fn last_persisted_blocks(&self) -> &[u64] {
        &self.last_persisted_blocks
    }

    pub fn volatile_bytes(&self) -> &[u8] {
        &self.volatile
    }

    pub fn durable_bytes(&self) -> &[u8] {
        &self.durable
    }

    /// Simulates power-on by replacing volatile contents with durable contents.
    pub fn restart(&mut self) {
        self.volatile.copy_from_slice(&self.durable);
        self.dirty_blocks.clear();
        self.epoch_writes.clear();
        self.crashed = false;
    }

    /// Returns the wrapped device after forcing it to match the durable shadow.
    pub fn into_inner(mut self) -> Result<D, BlockDeviceError> {
        self.inner.write_blocks(0, &self.durable)?;
        self.inner.flush()?;
        Ok(self.inner)
    }

    fn persist_torn_prefix(
        &mut self,
        start: usize,
        buffer: &[u8],
        length: usize,
    ) -> Result<(), BlockDeviceError> {
        let block_start = start / self.block_size * self.block_size;
        let block_number = u64::try_from(block_start / self.block_size)
            .map_err(|_| BlockDeviceError::ArithmeticOverflow)?;
        let mut block = self.durable[block_start..block_start + self.block_size].to_vec();
        block[..length].copy_from_slice(&buffer[..length]);
        self.inner.write_blocks(block_number, &block)?;
        self.inner.flush()?;
        self.durable[block_start..block_start + self.block_size].copy_from_slice(&block);
        self.volatile[block_start..block_start + self.block_size].copy_from_slice(&block);
        self.last_persisted_blocks.clear();
        self.last_persisted_blocks.push(block_number);
        Ok(())
    }

    fn failed_flush_transitions(&self) -> (Vec<EpochBlockWrite>, Option<usize>) {
        match &self.persistence_policy {
            PersistencePolicy::Atomic => (Vec::new(), None),
            PersistencePolicy::DirtyBlockPrefix {
                blocks,
                torn_prefix_bytes,
            } => (
                self.dirty_blocks
                    .iter()
                    .take(*blocks)
                    .map(|block| self.current_block(*block))
                    .collect(),
                *torn_prefix_bytes,
            ),
            PersistencePolicy::DirtyBlockSet {
                blocks,
                torn_prefix_bytes,
            } => (
                blocks
                    .iter()
                    .filter(|block| self.dirty_blocks.contains(block))
                    .map(|block| self.current_block(*block))
                    .collect(),
                *torn_prefix_bytes,
            ),
            PersistencePolicy::WritePrefix {
                writes,
                reverse,
                torn_prefix_bytes,
            } => {
                let selected = if *reverse {
                    self.epoch_writes
                        .iter()
                        .rev()
                        .take(*writes)
                        .cloned()
                        .collect()
                } else {
                    self.epoch_writes.iter().take(*writes).cloned().collect()
                };
                (selected, *torn_prefix_bytes)
            }
        }
    }

    fn current_block(&self, block: u64) -> EpochBlockWrite {
        let start = block as usize * self.block_size;
        EpochBlockWrite {
            block,
            bytes: self.volatile[start..start + self.block_size].to_vec(),
        }
    }

    fn persist_failed_flush(&mut self) -> Result<(), BlockDeviceError> {
        let (transitions, torn_prefix) = self.failed_flush_transitions();
        let mut next_durable = self.durable.clone();
        self.last_persisted_blocks.clear();
        for transition in transitions {
            let start = usize::try_from(transition.block)
                .ok()
                .and_then(|block| block.checked_mul(self.block_size))
                .ok_or(BlockDeviceError::ArithmeticOverflow)?;
            let length = torn_prefix.unwrap_or(self.block_size).min(self.block_size);
            next_durable[start..start + length].copy_from_slice(&transition.bytes[..length]);
            if !self.last_persisted_blocks.contains(&transition.block) {
                self.last_persisted_blocks.push(transition.block);
            }
        }
        if !self.last_persisted_blocks.is_empty() {
            self.inner.write_blocks(0, &next_durable)?;
            self.inner.flush()?;
        }
        self.durable = next_durable;
        Ok(())
    }

    fn operation_fails(&mut self) -> bool {
        let operation = self.operation_count;
        self.operation_count = self.operation_count.saturating_add(1);
        if self.fail_at_operation == Some(operation) {
            self.crashed = true;
            true
        } else {
            false
        }
    }
}

impl<D: BlockDevice> BlockDevice for CrashBlockDevice<D> {
    fn block_size(&self) -> usize {
        self.block_size
    }

    fn block_count(&self) -> u64 {
        self.block_count
    }

    fn read_blocks(&mut self, first_block: u64, buffer: &mut [u8]) -> Result<(), BlockDeviceError> {
        if self.crashed {
            return Err(BlockDeviceError::Io);
        }
        let range =
            checked_block_range(self.block_size, self.block_count, first_block, buffer.len())?;
        buffer.copy_from_slice(&self.volatile[range]);
        Ok(())
    }

    fn write_blocks(&mut self, first_block: u64, buffer: &[u8]) -> Result<(), BlockDeviceError> {
        let range =
            checked_block_range(self.block_size, self.block_count, first_block, buffer.len())?;
        if self.crashed {
            return Err(BlockDeviceError::Io);
        }
        if self.operation_fails() {
            if let Some(prefix) = self.torn_write_prefix {
                let length = prefix.min(self.block_size).min(buffer.len());
                self.persist_torn_prefix(range.start, buffer, length)?;
            }
            return Err(BlockDeviceError::Io);
        }
        self.volatile[range].copy_from_slice(buffer);
        for (offset, bytes) in buffer.chunks(self.block_size).enumerate() {
            let block = first_block
                .checked_add(offset as u64)
                .ok_or(BlockDeviceError::ArithmeticOverflow)?;
            if !self.dirty_blocks.contains(&block) {
                self.dirty_blocks.push(block);
            }
            self.epoch_writes.push(EpochBlockWrite {
                block,
                bytes: bytes.to_vec(),
            });
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BlockDeviceError> {
        if self.crashed {
            return Err(BlockDeviceError::Io);
        }
        if self.operation_fails() {
            self.persist_failed_flush()?;
            return Err(BlockDeviceError::Io);
        }
        self.inner.write_blocks(0, &self.volatile)?;
        self.inner.flush()?;
        self.durable.copy_from_slice(&self.volatile);
        self.last_persisted_blocks.clone_from(&self.dirty_blocks);
        self.dirty_blocks.clear();
        self.epoch_writes.clear();
        Ok(())
    }
}

#[derive(Clone)]
struct PendingDirectoryEntry {
    name: String,
    inode: u64,
}

#[derive(Clone)]
enum PendingContent {
    Directory(Vec<PendingDirectoryEntry>),
    File { segments: Vec<(u64, Vec<u8>)> },
}

#[derive(Clone)]
struct PendingNode {
    inode: Inode,
    content: PendingContent,
}

pub struct ImageBuilder<D> {
    device: D,
    superblock: Superblock,
    groups: Vec<AllocationGroupDescriptor>,
    next_data_block: Vec<u64>,
    nodes: Vec<PendingNode>,
}

impl<D: BlockDevice> ImageBuilder<D> {
    pub fn new(device: D, uuid: [u8; 16], label: &str) -> Result<Self, Error> {
        if device.block_size() != BLOCK_SIZE {
            return Err(BlockDeviceError::InvalidBlockSize.into());
        }
        let device_bytes = device
            .block_count()
            .checked_mul(BLOCK_SIZE_U64)
            .ok_or(Error::ArithmeticOverflow)?;
        let superblock = Superblock::format_phase3(FormatOptions::new(device_bytes, uuid, label))?;
        let maximum_groups = usize::try_from(superblock.descriptor_reservation_blocks)
            .map_err(|_| Error::TooManyGroups)?
            .checked_mul(ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK)
            .ok_or(Error::ArithmeticOverflow)?;
        if superblock.allocation_group_count as usize > maximum_groups {
            return Err(Error::TooManyGroups);
        }
        let mut groups = Vec::with_capacity(superblock.allocation_group_count as usize);
        let mut next_data_block = Vec::with_capacity(superblock.allocation_group_count as usize);
        for group_index in 0..superblock.allocation_group_count {
            let group_start = u64::from(group_index)
                .checked_mul(superblock.allocation_group_blocks)
                .ok_or(Error::ArithmeticOverflow)?;
            let group_count =
                (superblock.capacity_blocks - group_start).min(superblock.allocation_group_blocks);
            let group_end = group_start
                .checked_add(group_count)
                .ok_or(Error::ArithmeticOverflow)?;
            let metadata_start = group_start.max(superblock.first_allocatable_block);
            let block_bitmap_block = metadata_start;
            let inode_bitmap_block = metadata_start
                .checked_add(1)
                .ok_or(Error::ArithmeticOverflow)?;
            let inode_table_first_block = metadata_start
                .checked_add(2)
                .ok_or(Error::ArithmeticOverflow)?;
            let first_data_block = inode_table_first_block
                .checked_add(u64::from(PHASE2_INODE_TABLE_BLOCKS))
                .ok_or(Error::ArithmeticOverflow)?;
            if first_data_block >= group_end {
                return Err(Error::DeviceTooSmall);
            }
            let descriptor = AllocationGroupDescriptor {
                group_index,
                flags: 0,
                group_start_block: group_start,
                group_block_count: group_count,
                block_bitmap_block,
                inode_bitmap_block,
                inode_table_first_block,
                inode_table_block_count: PHASE2_INODE_TABLE_BLOCKS,
                inodes_in_group: PHASE2_INODES_PER_GROUP,
                first_data_block,
                data_end_block: group_end,
                root_inode_index: (group_index == 0).then_some(0),
            };
            descriptor.validate(&superblock)?;
            next_data_block.push(first_data_block);
            groups.push(descriptor);
        }
        let root = PendingNode {
            inode: Inode {
                kind: NodeKind::Directory,
                mode: 0o755,
                link_count: 1,
                generation: 1,
                parent_inode: 1,
                ..Inode::default()
            },
            content: PendingContent::Directory(Vec::new()),
        };
        Ok(Self {
            device,
            superblock,
            groups,
            next_data_block,
            nodes: vec![root],
        })
    }

    pub const fn superblock(&self) -> &Superblock {
        &self.superblock
    }

    pub fn create_directory(&mut self, parent: u64, name: &str, mode: u16) -> Result<u64, Error> {
        let inode_number = self.allocate_inode()?;
        let inode = Inode {
            kind: NodeKind::Directory,
            mode,
            link_count: 1,
            generation: inode_number,
            parent_inode: parent,
            ..Inode::default()
        };
        self.insert_node(parent, name, inode, PendingContent::Directory(Vec::new()))?;
        Ok(inode_number)
    }

    pub fn create_file(
        &mut self,
        parent: u64,
        name: &str,
        bytes: &[u8],
        mode: u16,
    ) -> Result<u64, Error> {
        self.create_sparse_file(parent, name, bytes.len() as u64, &[(0, bytes)], mode)
    }

    pub fn create_sparse_file(
        &mut self,
        parent: u64,
        name: &str,
        size: u64,
        segments: &[(u64, &[u8])],
        mode: u16,
    ) -> Result<u64, Error> {
        if segments.len() > INLINE_EXTENT_COUNT {
            return Err(Error::TooManyExtents);
        }
        let mut copied = Vec::with_capacity(segments.len());
        let mut previous_end = 0_u64;
        for (index, (offset, bytes)) in segments.iter().enumerate() {
            let end = offset
                .checked_add(bytes.len() as u64)
                .ok_or(Error::ArithmeticOverflow)?;
            if *offset % BLOCK_SIZE_U64 != 0
                || bytes.is_empty()
                || end > size
                || (index != 0 && *offset < previous_end)
            {
                return Err(Error::InvalidSparseSegment);
            }
            previous_end = end;
            copied.push((*offset, bytes.to_vec()));
        }
        let inode_number = self.allocate_inode()?;
        let inode = Inode {
            kind: NodeKind::Regular,
            mode,
            link_count: 1,
            generation: inode_number,
            size,
            parent_inode: parent,
            ..Inode::default()
        };
        self.insert_node(
            parent,
            name,
            inode,
            PendingContent::File { segments: copied },
        )?;
        Ok(inode_number)
    }

    pub fn finish(mut self) -> Result<D, Error> {
        self.allocate_contents()?;
        self.write_metadata()?;
        self.device.flush()?;
        Ok(self.device)
    }

    fn allocate_inode(&self) -> Result<u64, Error> {
        let next = self
            .nodes
            .len()
            .checked_add(1)
            .ok_or(Error::ArithmeticOverflow)?;
        let capacity = self
            .groups
            .len()
            .checked_mul(PHASE2_INODES_PER_GROUP as usize)
            .ok_or(Error::ArithmeticOverflow)?;
        if next > capacity {
            return Err(Error::TooManyInodes);
        }
        Ok(next as u64)
    }

    fn insert_node(
        &mut self,
        parent: u64,
        name: &str,
        inode: Inode,
        content: PendingContent,
    ) -> Result<(), Error> {
        let child_inode = self
            .nodes
            .len()
            .checked_add(1)
            .ok_or(Error::ArithmeticOverflow)? as u64;
        let parent_index = usize::try_from(parent.checked_sub(1).ok_or(Error::InvalidParent)?)
            .map_err(|_| Error::InvalidParent)?;
        let Some(parent_node) = self.nodes.get_mut(parent_index) else {
            return Err(Error::InvalidParent);
        };
        let PendingContent::Directory(entries) = &mut parent_node.content else {
            return Err(Error::InvalidParent);
        };
        if entries.iter().any(|entry| entry.name == name) {
            return Err(Error::DuplicateName);
        }
        DirectoryEntry::new(child_inode, inode.generation, inode.kind, name)?;
        entries.push(PendingDirectoryEntry {
            name: String::from(name),
            inode: child_inode,
        });
        self.nodes.push(PendingNode { inode, content });
        Ok(())
    }

    fn allocate_contents(&mut self) -> Result<(), Error> {
        for node_index in 0..self.nodes.len() {
            let inode_number = node_index as u64 + 1;
            let content = self.nodes[node_index].content.clone();
            match content {
                PendingContent::File { segments } => {
                    let mut extent_count = 0usize;
                    let mut allocated_blocks = 0_u64;
                    for (offset, bytes) in segments {
                        let blocks = bytes
                            .len()
                            .checked_add(BLOCK_SIZE - 1)
                            .ok_or(Error::ArithmeticOverflow)?
                            / BLOCK_SIZE;
                        let physical = self.allocate_blocks(blocks as u32)?;
                        let logical = offset / BLOCK_SIZE_U64;
                        self.nodes[node_index].inode.extents[extent_count] = Extent {
                            logical_first_block: logical,
                            physical_first_block: physical,
                            length_blocks: blocks as u32,
                            flags: 0,
                        };
                        extent_count += 1;
                        allocated_blocks = allocated_blocks
                            .checked_add(blocks as u64)
                            .ok_or(Error::ArithmeticOverflow)?;
                        self.write_file_segment(physical, &bytes)?;
                    }
                    self.nodes[node_index].inode.extent_count = extent_count as u16;
                    self.nodes[node_index].inode.allocated_blocks = allocated_blocks;
                }
                PendingContent::Directory(entries) => {
                    let block_count = entries.len().div_ceil(DIRECTORY_ENTRIES_PER_BLOCK).max(1);
                    let physical = self.allocate_blocks(block_count as u32)?;
                    self.nodes[node_index].inode.size = (block_count * BLOCK_SIZE) as u64;
                    self.nodes[node_index].inode.allocated_blocks = block_count as u64;
                    self.nodes[node_index].inode.directory_entry_count = entries.len() as u64;
                    self.nodes[node_index].inode.extent_count = 1;
                    self.nodes[node_index].inode.extents[0] = Extent {
                        logical_first_block: 0,
                        physical_first_block: physical,
                        length_blocks: block_count as u32,
                        flags: 0,
                    };
                    for logical in 0..block_count {
                        let mut directory = DirectoryBlock::new(inode_number, logical as u64)?;
                        let start = logical * DIRECTORY_ENTRIES_PER_BLOCK;
                        for (slot, pending) in entries
                            .iter()
                            .skip(start)
                            .take(DIRECTORY_ENTRIES_PER_BLOCK)
                            .enumerate()
                        {
                            let target = &self.nodes[pending.inode as usize - 1].inode;
                            directory.entries[slot] = DirectoryEntry::new(
                                pending.inode,
                                target.generation,
                                target.kind,
                                &pending.name,
                            )?;
                        }
                        self.device
                            .write_blocks(physical + logical as u64, &directory.encode()?)?;
                    }
                }
            }
            self.nodes[node_index].inode.validate()?;
        }
        Ok(())
    }

    fn allocate_blocks(&mut self, count: u32) -> Result<u64, Error> {
        if count == 0 {
            return Err(Error::NoSpace);
        }
        for (index, group) in self.groups.iter().enumerate() {
            let start = self.next_data_block[index];
            let end = start
                .checked_add(u64::from(count))
                .ok_or(Error::ArithmeticOverflow)?;
            if end <= group.data_end_block {
                self.next_data_block[index] = end;
                return Ok(start);
            }
        }
        Err(Error::NoSpace)
    }

    fn write_file_segment(&mut self, physical: u64, bytes: &[u8]) -> Result<(), Error> {
        for (index, chunk) in bytes.chunks(BLOCK_SIZE).enumerate() {
            let mut block = [0; BLOCK_SIZE];
            block[..chunk.len()].copy_from_slice(chunk);
            self.device.write_blocks(physical + index as u64, &block)?;
        }
        Ok(())
    }

    fn write_metadata(&mut self) -> Result<(), Error> {
        let zero = [0; BLOCK_SIZE];
        let mut free_block_count = 0_u64;
        let mut free_inode_count = 0_u64;
        for block in 0..self.superblock.first_allocatable_block {
            self.device.write_blocks(block, &zero)?;
        }
        for (group_index, group) in self.groups.iter().enumerate() {
            let mut block_bitmap = [0; BLOCK_SIZE];
            let mut inode_bitmap = [0; BLOCK_SIZE];

            for physical in group.group_start_block..group.first_data_block {
                bitmap_set(
                    &mut block_bitmap,
                    usize::try_from(physical - group.group_start_block)
                        .map_err(|_| Error::ArithmeticOverflow)?,
                    true,
                )?;
            }
            for node in &self.nodes {
                for extent in node
                    .inode
                    .extents
                    .iter()
                    .take(node.inode.extent_count as usize)
                {
                    let extent_end = extent
                        .physical_first_block
                        .checked_add(u64::from(extent.length_blocks))
                        .ok_or(Error::ArithmeticOverflow)?;
                    let start = extent.physical_first_block.max(group.first_data_block);
                    let end = extent_end.min(group.data_end_block);
                    for physical in start..end {
                        bitmap_set(
                            &mut block_bitmap,
                            usize::try_from(physical - group.group_start_block)
                                .map_err(|_| Error::ArithmeticOverflow)?,
                            true,
                        )?;
                    }
                }
            }
            let first_inode = group_index
                .checked_mul(PHASE2_INODES_PER_GROUP as usize)
                .ok_or(Error::ArithmeticOverflow)?;
            let allocated_inodes = self.nodes.len().saturating_sub(first_inode).min(
                usize::try_from(group.inodes_in_group).map_err(|_| Error::ArithmeticOverflow)?,
            );
            for local_inode in 0..allocated_inodes {
                bitmap_set(&mut inode_bitmap, local_inode, true)?;
            }

            for physical in group.first_data_block..group.data_end_block {
                let local = usize::try_from(physical - group.group_start_block)
                    .map_err(|_| Error::ArithmeticOverflow)?;
                if bitmap_test(&block_bitmap, local) == Some(false) {
                    free_block_count = free_block_count
                        .checked_add(1)
                        .ok_or(Error::ArithmeticOverflow)?;
                }
            }
            for local_inode in
                0..usize::try_from(group.inodes_in_group).map_err(|_| Error::ArithmeticOverflow)?
            {
                if bitmap_test(&inode_bitmap, local_inode) == Some(false) {
                    free_inode_count = free_inode_count
                        .checked_add(1)
                        .ok_or(Error::ArithmeticOverflow)?;
                }
            }

            self.device
                .write_blocks(group.block_bitmap_block, &block_bitmap)?;
            self.device
                .write_blocks(group.inode_bitmap_block, &inode_bitmap)?;
            for offset in 0..group.inode_table_block_count {
                self.device
                    .write_blocks(group.inode_table_first_block + u64::from(offset), &zero)?;
            }
        }

        for (group_index, group) in self.groups.iter().enumerate() {
            for table_block in 0..group.inode_table_block_count as usize {
                let mut block = [0; BLOCK_SIZE];
                for slot in 0..BLOCK_SIZE / INODE_BYTES {
                    let local_inode = table_block * (BLOCK_SIZE / INODE_BYTES) + slot;
                    let global_index = group_index * PHASE2_INODES_PER_GROUP as usize + local_inode;
                    let Some(node) = self.nodes.get(global_index) else {
                        continue;
                    };
                    let encoded = node.inode.encode()?;
                    let start = slot * INODE_BYTES;
                    block[start..start + INODE_BYTES].copy_from_slice(&encoded);
                }
                self.device
                    .write_blocks(group.inode_table_first_block + table_block as u64, &block)?;
            }
        }

        let table_count = self.superblock.descriptor_reservation_blocks;
        for table_index in 0..table_count {
            let first = table_index as usize * ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK;
            let mut table = AllocationGroupTable::new(
                first as u32,
                self.groups.len() as u32,
                table_index,
                table_count,
                FIRST_DESCRIPTOR_BLOCK + u64::from(table_index),
            );
            for descriptor in self
                .groups
                .iter()
                .skip(first)
                .take(ALLOCATION_GROUP_DESCRIPTORS_PER_BLOCK)
            {
                table.push(*descriptor)?;
            }
            self.device
                .write_blocks(table.physical_block, &table.encode(&self.superblock)?)?;
        }
        let state = FilesystemState {
            free_block_count,
            free_inode_count,
            orphan_head: 0,
            ..FilesystemState::initial()
        }
        .encode()?;
        self.device
            .write_blocks(self.superblock.filesystem_state_block, &state)?;
        for control_index in 0..2_u64 {
            let control = JournalControl::empty(control_index + 1).encode()?;
            self.device.write_blocks(
                self.superblock.journal_first_block + control_index,
                &control,
            )?;
        }

        let encoded_superblock = self.superblock.encode()?;
        self.device
            .write_blocks(SUPERBLOCK_BLOCK, &encoded_superblock)?;
        self.device
            .write_blocks(self.superblock.backup_superblock_block, &encoded_superblock)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nullfs_blockdev::MemoryBlockDevice;
    use nullfs_format::{
        JournalState, MountMode, PHASE3_JOURNAL_FIRST_BLOCK, bitmap_test, validate_bitmap_tail,
    };

    const IMAGE_BLOCKS: u64 = 4096;

    #[test]
    fn crash_discards_unflushed_writes() {
        let inner = MemoryBlockDevice::new(512, 2).expect("device");
        let mut device = CrashBlockDevice::new(inner).expect("crash device");
        device.write_blocks(0, &[0x5a; 512]).expect("write");
        assert_eq!(device.operation_count(), 1);
        assert_ne!(device.volatile_bytes(), device.durable_bytes());

        device.restart();

        let mut block = [0xff; 512];
        device.read_blocks(0, &mut block).expect("read");
        assert_eq!(block, [0; 512]);
        assert_eq!(device.volatile_bytes(), device.durable_bytes());
    }

    #[test]
    fn crash_preserves_flushed_writes() {
        let inner = MemoryBlockDevice::new(512, 2).expect("device");
        let mut device = CrashBlockDevice::new(inner).expect("crash device");
        device.write_blocks(1, &[0xa5; 512]).expect("write");
        device.flush().expect("flush");
        assert_eq!(device.operation_count(), 2);

        device.restart();

        let mut block = [0; 512];
        device.read_blocks(1, &mut block).expect("read");
        assert_eq!(block, [0xa5; 512]);
    }

    #[test]
    fn configured_failure_and_torn_write_are_deterministic() {
        let inner = MemoryBlockDevice::new(512, 2).expect("device");
        let mut device = CrashBlockDevice::new(inner).expect("crash device");
        device.fail_at_operation(Some(1));
        device.set_torn_write_prefix(Some(17));
        device.write_blocks(0, &[1; 512]).expect("operation zero");
        assert_eq!(device.write_blocks(1, &[2; 512]), Err(BlockDeviceError::Io));
        assert!(device.is_crashed());
        assert_eq!(device.operation_count(), 2);
        let mut unreadable = [0; 512];
        assert_eq!(
            device.read_blocks(0, &mut unreadable),
            Err(BlockDeviceError::Io)
        );

        device.restart();

        let mut block = [0; 512];
        device.read_blocks(1, &mut block).expect("read torn block");
        assert_eq!(&block[..17], &[2; 17]);
        assert_eq!(&block[17..], &[0; 512 - 17]);
        let inner = device.into_inner().expect("synchronized inner");
        assert_eq!(&inner.bytes()[512..512 + 17], &[2; 17]);
        assert_eq!(&inner.bytes()[512 + 17..], &[0; 512 - 17]);
    }

    #[test]
    fn failed_flush_is_a_barrier_that_persists_nothing() {
        let inner = MemoryBlockDevice::new(512, 1).expect("device");
        let mut device = CrashBlockDevice::new(inner).expect("crash device");
        device.write_blocks(0, &[1; 512]).expect("first write");
        device.flush().expect("first barrier");
        device.write_blocks(0, &[2; 512]).expect("second write");
        device.fail_at_operation(Some(3));
        assert_eq!(device.flush(), Err(BlockDeviceError::Io));
        assert_eq!(
            device.read_blocks(0, &mut [0; 512]),
            Err(BlockDeviceError::Io)
        );

        device.restart();

        let mut block = [0; 512];
        device
            .read_blocks(0, &mut block)
            .expect("read after restart");
        assert_eq!(block, [1; 512]);
        let inner = device.into_inner().expect("synchronized inner");
        assert_eq!(inner.bytes(), &[1; 512]);
    }

    #[test]
    fn failed_flush_can_persist_a_dirty_block_prefix() {
        let inner = MemoryBlockDevice::new(512, 4).expect("device");
        let mut device = CrashBlockDevice::new(inner).expect("crash device");
        device.write_blocks(2, &[2; 512]).expect("write block 2");
        device.write_blocks(0, &[1; 512]).expect("write block 0");
        device.write_blocks(1, &[3; 512]).expect("write block 1");
        assert_eq!(device.dirty_blocks(), &[2, 0, 1]);
        assert_eq!(device.epoch_writes().len(), 3);
        device.set_persistence_policy(PersistencePolicy::DirtyBlockPrefix {
            blocks: 2,
            torn_prefix_bytes: None,
        });
        device.fail_at_operation(Some(3));

        assert_eq!(device.flush(), Err(BlockDeviceError::Io));
        assert_eq!(device.last_persisted_blocks(), &[2, 0]);
        device.restart();

        let mut image = [0; 4 * 512];
        device.read_blocks(0, &mut image).expect("restart image");
        assert_eq!(&image[..512], &[1; 512]);
        assert_eq!(&image[512..2 * 512], &[0; 512]);
        assert_eq!(&image[2 * 512..3 * 512], &[2; 512]);
    }

    #[test]
    fn failed_flush_can_persist_an_order_independent_dirty_set() {
        let inner = MemoryBlockDevice::new(512, 4).expect("device");
        let mut device = CrashBlockDevice::new(inner).expect("crash device");
        let mut image = [0; 4 * 512];
        for (block, bytes) in image.chunks_mut(512).enumerate() {
            bytes.fill(block as u8 + 1);
        }
        device.write_blocks(0, &image).expect("multi-block write");
        device.set_persistence_policy(PersistencePolicy::DirtyBlockSet {
            blocks: vec![3, 1],
            torn_prefix_bytes: None,
        });
        device.fail_at_operation(Some(1));

        assert_eq!(device.flush(), Err(BlockDeviceError::Io));
        assert_eq!(device.last_persisted_blocks(), &[3, 1]);
        device.restart();

        let mut durable = [0; 4 * 512];
        device.read_blocks(0, &mut durable).expect("restart image");
        assert_eq!(&durable[..512], &[0; 512]);
        assert_eq!(&durable[512..2 * 512], &[2; 512]);
        assert_eq!(&durable[2 * 512..3 * 512], &[0; 512]);
        assert_eq!(&durable[3 * 512..], &[4; 512]);
    }

    #[test]
    fn failed_flush_can_persist_write_records_in_reverse_order() {
        let inner = MemoryBlockDevice::new(512, 1).expect("device");
        let mut device = CrashBlockDevice::new(inner).expect("crash device");
        device.write_blocks(0, &[1; 512]).expect("older write");
        device.write_blocks(0, &[2; 512]).expect("newer write");
        assert_eq!(
            device
                .epoch_writes()
                .iter()
                .map(|write| write.block)
                .collect::<Vec<_>>(),
            vec![0, 0]
        );
        device.set_persistence_policy(PersistencePolicy::WritePrefix {
            writes: 2,
            reverse: true,
            torn_prefix_bytes: None,
        });
        device.fail_at_operation(Some(2));

        assert_eq!(device.flush(), Err(BlockDeviceError::Io));
        device.restart();

        let mut block = [0; 512];
        device.read_blocks(0, &mut block).expect("restart block");
        assert_eq!(block, [1; 512]);
    }

    #[test]
    fn failed_flush_can_tear_every_selected_block() {
        let inner = MemoryBlockDevice::new(512, 3).expect("device");
        let mut device = CrashBlockDevice::new(inner).expect("crash device");
        device.write_blocks(0, &[7; 3 * 512]).expect("write image");
        device.set_persistence_policy(PersistencePolicy::DirtyBlockSet {
            blocks: vec![0, 2],
            torn_prefix_bytes: Some(19),
        });
        device.fail_at_operation(Some(1));

        assert_eq!(device.flush(), Err(BlockDeviceError::Io));
        assert_eq!(device.last_persisted_blocks(), &[0, 2]);
        device.restart();

        let mut image = [0; 3 * 512];
        device.read_blocks(0, &mut image).expect("restart image");
        for block in [0, 2] {
            let start = block * 512;
            assert_eq!(&image[start..start + 19], &[7; 19]);
            assert_eq!(&image[start + 19..start + 512], &[0; 512 - 19]);
        }
        assert_eq!(&image[512..2 * 512], &[0; 512]);
        let inner = device.into_inner().expect("synchronized inner");
        assert_eq!(inner.bytes(), &image);
    }

    fn block(device: &MemoryBlockDevice, physical: u64) -> &[u8] {
        let start = usize::try_from(physical).unwrap() * BLOCK_SIZE;
        &device.bytes()[start..start + BLOCK_SIZE]
    }

    #[test]
    fn populated_image_has_canonical_phase3_metadata_and_allocations() {
        let device = MemoryBlockDevice::new(BLOCK_SIZE, IMAGE_BLOCKS).expect("device");
        let mut builder = ImageBuilder::new(device, [7; 16], "phase3").expect("builder");
        let directory = builder
            .create_directory(1, "etc", 0o755)
            .expect("directory");
        builder
            .create_file(directory, "payload", &vec![0x5a; BLOCK_SIZE + 1], 0o644)
            .expect("file");
        let groups = builder.groups.clone();
        let superblock = builder.superblock.clone();
        let device = builder.finish().expect("finish");

        let primary = block(&device, SUPERBLOCK_BLOCK);
        let backup = block(&device, superblock.backup_superblock_block);
        assert_eq!(primary, backup);
        let decoded = Superblock::decode(
            primary,
            Some(IMAGE_BLOCKS * BLOCK_SIZE_U64),
            MountMode::ReadWrite,
        )
        .expect("clean phase 3 superblock");
        assert_eq!(decoded, superblock);
        assert!(decoded.phase3_enabled());

        let state = FilesystemState::decode(block(&device, superblock.filesystem_state_block))
            .expect("filesystem state");
        let expected_free_blocks = groups
            .iter()
            .map(|group| {
                (group.first_data_block..group.data_end_block)
                    .filter(|physical| {
                        bitmap_test(
                            block(&device, group.block_bitmap_block),
                            usize::try_from(*physical - group.group_start_block).unwrap(),
                        ) == Some(false)
                    })
                    .count() as u64
            })
            .sum();
        let expected_free_inodes = groups
            .iter()
            .map(|group| {
                (0..group.inodes_in_group as usize)
                    .filter(|local| {
                        bitmap_test(block(&device, group.inode_bitmap_block), *local) == Some(false)
                    })
                    .count() as u64
            })
            .sum();
        assert_eq!(state.free_block_count, expected_free_blocks);
        assert_eq!(state.free_inode_count, expected_free_inodes);
        assert_eq!(state.orphan_head, 0);
        assert_eq!(state.free_block_count, 3924);
        assert_eq!(state.free_inode_count, 253);
        for index in 0..2_u64 {
            let control =
                JournalControl::decode(block(&device, superblock.journal_first_block + index))
                    .expect("journal control");
            assert_eq!(control.state, JournalState::Empty);
            assert_eq!(control.generation, index + 1);
        }
        for physical in PHASE3_JOURNAL_FIRST_BLOCK + 2..superblock.first_allocatable_block {
            assert_eq!(block(&device, physical), &[0; BLOCK_SIZE]);
        }

        let group = &groups[0];
        assert_eq!(group.block_bitmap_block, 150);
        let block_bitmap = block(&device, group.block_bitmap_block);
        let inode_bitmap = block(&device, group.inode_bitmap_block);
        validate_bitmap_tail(block_bitmap, group.group_block_count as usize)
            .expect("canonical block bitmap tail");
        validate_bitmap_tail(inode_bitmap, group.inodes_in_group as usize)
            .expect("canonical inode bitmap tail");

        for physical in group.group_start_block..group.first_data_block {
            assert_eq!(
                bitmap_test(block_bitmap, (physical - group.group_start_block) as usize),
                Some(true),
                "metadata block {physical}"
            );
        }
        for local_inode in 0..group.inodes_in_group as usize {
            assert_eq!(
                bitmap_test(inode_bitmap, local_inode),
                Some(local_inode < 3)
            );
        }

        // Root and child directories consume one block each; the populated file consumes two.
        for physical in group.first_data_block..group.first_data_block + 4 {
            assert_eq!(
                bitmap_test(block_bitmap, (physical - group.group_start_block) as usize),
                Some(true),
                "owned data block {physical}"
            );
        }
        assert_eq!(
            bitmap_test(
                block_bitmap,
                (group.first_data_block + 4 - group.group_start_block) as usize,
            ),
            Some(false)
        );
    }
}
