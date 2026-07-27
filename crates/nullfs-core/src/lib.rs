#![no_std]

//! Validated NullFS semantic operations and Phase 3 redo-journal foundation.

extern crate alloc;

mod journal;
mod mutation;

use journal::Transaction;

use alloc::{string::String, vec, vec::Vec};
use core::{fmt, str};

use nullfs_blockdev::{BlockDevice, BlockDeviceError};
use nullfs_format::{
    AllocationGroupDescriptor, AllocationGroupTable, BLOCK_SIZE, BLOCK_SIZE_U64,
    DIRECTORY_ENTRIES_PER_BLOCK, DirectoryBlock, Error as FormatError, FilesystemState,
    INODE_BYTES, Inode, InodeValidationMode, MountMode, NodeKind, PHASE2_INODES_PER_GROUP,
    SUPERBLOCK_BLOCK, Superblock, VolumeState, bitmap_test, validate_bitmap_tail,
};

pub const ROOT_INODE: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NodeId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeAttributes {
    pub node: NodeId,
    pub generation: u64,
    pub kind: NodeKind,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub link_count: u32,
    pub size: u64,
    pub allocated_blocks: u64,
    pub accessed: nullfs_format::Timestamp,
    pub modified: nullfs_format::Timestamp,
    pub changed: nullfs_format::Timestamp,
    pub created: nullfs_format::Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryRecord {
    pub node: NodeId,
    pub generation: u64,
    pub kind: NodeKind,
    pub name: String,
    pub next_cookie: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilesystemStatistics {
    pub total_data_blocks: u64,
    pub free_data_blocks: u64,
    pub total_inodes: u64,
    pub free_inodes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Device(BlockDeviceError),
    Format(FormatError),
    Phase2Required,
    InvalidNode,
    InvalidName,
    NotFound,
    NotDirectory,
    IsDirectory,
    UnsupportedNodeKind,
    InvalidCookie,
    CorruptVolume,
    ArithmeticOverflow,
    Phase3Required,
    RedundantSuperblocksDisagree,
    CorruptJournal,
    ProtectedBlock,
    TransactionTooLarge,
    ReadOnly,
    Poisoned,
    AlreadyExists,
    NoSpace,
    ExtentLimit,
    DirectoryNotEmpty,
    DirectoryCycle,
    TransactionInProgress,
    InvalidHandle,
    RecoveryRequired,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(error) => write!(formatter, "block device: {error}"),
            Self::Format(error) => write!(formatter, "format: {error}"),
            Self::Phase2Required => formatter.write_str("NullFS Phase 2 format is required"),
            Self::InvalidNode => formatter.write_str("invalid or free inode"),
            Self::InvalidName => formatter.write_str("invalid path component"),
            Self::NotFound => formatter.write_str("directory entry was not found"),
            Self::NotDirectory => formatter.write_str("node is not a directory"),
            Self::IsDirectory => formatter.write_str("node is a directory"),
            Self::UnsupportedNodeKind => formatter.write_str("node kind is not supported"),
            Self::InvalidCookie => formatter.write_str("directory cookie is invalid"),
            Self::CorruptVolume => formatter.write_str("NullFS metadata is inconsistent"),
            Self::ArithmeticOverflow => formatter.write_str("filesystem arithmetic overflowed"),
            Self::Phase3Required => formatter.write_str("NullFS Phase 3 format is required"),
            Self::RedundantSuperblocksDisagree => {
                formatter.write_str("valid redundant superblocks disagree")
            }
            Self::CorruptJournal => formatter.write_str("NullFS journal is corrupt"),
            Self::ProtectedBlock => {
                formatter.write_str("transaction targets a protected bootstrap block")
            }
            Self::TransactionTooLarge => {
                formatter.write_str("transaction exceeds 64 complete block images")
            }
            Self::ReadOnly => formatter.write_str("filesystem is mounted read-only"),
            Self::Poisoned => formatter.write_str("filesystem is poisoned after a failed write"),
            Self::AlreadyExists => formatter.write_str("directory entry already exists"),
            Self::NoSpace => formatter.write_str("filesystem has no free inode or data block"),
            Self::ExtentLimit => formatter.write_str("operation exceeds the four inline extents"),
            Self::DirectoryNotEmpty => formatter.write_str("directory is not empty"),
            Self::DirectoryCycle => formatter.write_str("rename would create a directory cycle"),
            Self::TransactionInProgress => {
                formatter.write_str("another transaction is already staged")
            }
            Self::InvalidHandle => formatter.write_str("invalid or stale open handle"),
            Self::RecoveryRequired => formatter.write_str("writable recovery is required"),
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

pub struct MountFailure<D> {
    pub error: Error,
    pub device: D,
}

impl<D> fmt::Debug for MountFailure<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MountFailure")
            .field("error", &self.error)
            .field("device", &"<preserved>")
            .finish()
    }
}

impl<D> MountFailure<D> {
    pub fn into_parts(self) -> (Error, D) {
        (self.error, self.device)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMountMode {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenHandle {
    pub id: u64,
    pub node: NodeId,
    pub generation: u64,
    pub kind: NodeKind,
}

#[derive(Clone, Copy)]
struct OpenRecord {
    handle: OpenHandle,
}

pub struct Filesystem<D> {
    device: D,
    superblock: Superblock,
    groups: Vec<AllocationGroupDescriptor>,
    mount_mode: RuntimeMountMode,
    state: Option<FilesystemState>,
    journal_generation: u64,
    older_control_slot: u8,
    poisoned: bool,
    transaction: Option<Transaction>,
    open_handles: Vec<OpenRecord>,
    next_handle_id: u64,
    recovery_overlay: Vec<(u64, [u8; BLOCK_SIZE])>,
}

impl<D: BlockDevice> Filesystem<D> {
    pub fn mount(device: D) -> Result<Self, Error> {
        Self::try_mount(device).map_err(|failure| failure.error)
    }

    pub fn mount_read_write(device: D) -> Result<Self, Error> {
        Self::try_mount_read_write(device).map_err(|failure| failure.error)
    }

    pub fn try_mount(device: D) -> Result<Self, MountFailure<D>> {
        Self::try_mount_with_mode(device, RuntimeMountMode::ReadOnly)
    }

    pub fn try_mount_read_write(device: D) -> Result<Self, MountFailure<D>> {
        Self::try_mount_with_mode(device, RuntimeMountMode::ReadWrite)
    }

    fn try_mount_with_mode(
        mut device: D,
        mount_mode: RuntimeMountMode,
    ) -> Result<Self, MountFailure<D>> {
        let (superblock, groups) = match discover_mount_metadata(&mut device, mount_mode) {
            Ok(metadata) => metadata,
            Err(error) => return Err(MountFailure { error, device }),
        };
        let mut filesystem = Self {
            device,
            superblock,
            groups,
            mount_mode,
            state: None,
            journal_generation: 0,
            older_control_slot: 0,
            poisoned: false,
            transaction: None,
            open_handles: Vec::new(),
            next_handle_id: 1,
            recovery_overlay: Vec::new(),
        };
        match filesystem.finish_mount() {
            Ok(()) => Ok(filesystem),
            Err(error) => Err(MountFailure {
                error,
                device: filesystem.device,
            }),
        }
    }

    fn finish_mount(&mut self) -> Result<(), Error> {
        if self.superblock.phase3_enabled() {
            self.recover_journal()?;
            if self.state.is_none() {
                self.read_state()?;
            }
            if self.mount_mode == RuntimeMountMode::ReadWrite {
                self.reclaim_orphans()?;
            } else if self.state.ok_or(Error::CorruptVolume)?.orphan_head != 0 {
                return Err(Error::RecoveryRequired);
            }
        }
        self.validate_volume()?;
        if self.superblock.phase3_enabled() {
            self.initialize_free_counts()?;
        }
        if self.mount_mode == RuntimeMountMode::ReadWrite {
            self.write_superblocks(VolumeState::Dirty)?;
        }
        Ok(())
    }

    pub const fn superblock(&self) -> &Superblock {
        &self.superblock
    }

    pub const fn mount_mode(&self) -> RuntimeMountMode {
        self.mount_mode
    }

    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    pub fn sync(&mut self) -> Result<(), Error> {
        self.ensure_writable()?;
        self.commit_transaction()?;
        self.device.flush().map_err(|error| {
            self.poisoned = true;
            Error::Device(error)
        })
    }

    pub fn try_unmount(&mut self) -> Result<(), Error> {
        if self.mount_mode == RuntimeMountMode::ReadWrite {
            if self.poisoned {
                self.poisoned = false;
                let recovery = (|| {
                    self.transaction = None;
                    self.recover_journal()?;
                    self.read_state()?;
                    self.reclaim_orphans()?;
                    self.validate_volume()
                })();
                if let Err(error) = recovery {
                    self.poisoned = true;
                    return Err(error);
                }
            }
            self.sync()?;
            self.write_superblocks(VolumeState::Clean)?;
        }
        Ok(())
    }

    pub fn unmount(mut self) -> Result<D, Error> {
        self.try_unmount()?;
        Ok(self.device)
    }

    pub fn close(self) -> Result<D, Error> {
        self.unmount()
    }

    pub const fn root(&self) -> NodeId {
        NodeId(ROOT_INODE)
    }

    pub fn statistics(&self) -> Result<FilesystemStatistics, Error> {
        let state = self.state.ok_or(Error::CorruptVolume)?;
        let total_data_blocks = self.groups.iter().try_fold(0_u64, |total, group| {
            total
                .checked_add(group.data_end_block - group.first_data_block)
                .ok_or(Error::ArithmeticOverflow)
        })?;
        let total_inodes = u64::try_from(self.groups.len())
            .map_err(|_| Error::ArithmeticOverflow)?
            .checked_mul(u64::from(PHASE2_INODES_PER_GROUP))
            .ok_or(Error::ArithmeticOverflow)?;
        Ok(FilesystemStatistics {
            total_data_blocks,
            free_data_blocks: state.free_block_count,
            total_inodes,
            free_inodes: state.free_inode_count,
        })
    }

    pub fn into_device(self) -> D {
        self.device
    }

    pub fn attributes(&mut self, node: NodeId) -> Result<NodeAttributes, Error> {
        let inode = self.read_allocated_inode(node)?;
        Ok(NodeAttributes {
            node,
            generation: inode.generation,
            kind: inode.kind,
            mode: inode.mode,
            uid: inode.uid,
            gid: inode.gid,
            link_count: inode.link_count,
            size: inode.size,
            allocated_blocks: inode.allocated_blocks,
            accessed: inode.accessed,
            modified: inode.modified,
            changed: inode.changed,
            created: inode.created,
        })
    }

    pub fn lookup(&mut self, directory: NodeId, name: &[u8]) -> Result<NodeId, Error> {
        let name = str::from_utf8(name).map_err(|_| Error::InvalidName)?;
        if name.is_empty() || name.as_bytes().contains(&0) || name.as_bytes().contains(&b'/') {
            return Err(Error::InvalidName);
        }
        let inode = self.read_allocated_inode(directory)?;
        if inode.kind != NodeKind::Directory {
            return Err(Error::NotDirectory);
        }
        if name == "." {
            return Ok(directory);
        }
        if name == ".." {
            return Ok(NodeId(inode.parent_inode));
        }
        for logical_block in 0..inode.size / BLOCK_SIZE_U64 {
            let block = self.read_directory_block(directory, &inode, logical_block)?;
            for entry in block.entries.iter().filter(|entry| !entry.is_unused()) {
                if entry.name() == name {
                    return Ok(NodeId(entry.inode));
                }
            }
        }
        Err(Error::NotFound)
    }

    pub fn lookup_path(&mut self, start: NodeId, path: &str) -> Result<NodeId, Error> {
        let mut node = if path.starts_with('/') {
            self.root()
        } else {
            start
        };
        for component in path.split('/') {
            if component.is_empty() || component == "." {
                continue;
            }
            node = self.lookup(node, component.as_bytes())?;
        }
        Ok(node)
    }

    pub fn read(&mut self, node: NodeId, offset: u64, output: &mut [u8]) -> Result<usize, Error> {
        let inode = self.read_allocated_inode(node)?;
        match inode.kind {
            NodeKind::Directory => return Err(Error::IsDirectory),
            NodeKind::Regular => {}
            NodeKind::Symlink => return Err(Error::UnsupportedNodeKind),
            NodeKind::Free => return Err(Error::InvalidNode),
        }
        if offset >= inode.size || output.is_empty() {
            return Ok(0);
        }
        let available = inode.size - offset;
        let count = output
            .len()
            .min(usize::try_from(available).unwrap_or(usize::MAX));
        output[..count].fill(0);
        let mut completed = 0usize;
        let mut block_bytes = [0; BLOCK_SIZE];
        while completed < count {
            let file_offset = offset
                .checked_add(completed as u64)
                .ok_or(Error::ArithmeticOverflow)?;
            let logical_block = file_offset / BLOCK_SIZE_U64;
            let within_block = usize::try_from(file_offset % BLOCK_SIZE_U64)
                .map_err(|_| Error::ArithmeticOverflow)?;
            let chunk = (BLOCK_SIZE - within_block).min(count - completed);
            if let Some(physical_block) = physical_block(&inode, logical_block)? {
                self.read_block(physical_block, &mut block_bytes)?;
                output[completed..completed + chunk]
                    .copy_from_slice(&block_bytes[within_block..within_block + chunk]);
            }
            completed += chunk;
        }
        Ok(count)
    }

    pub fn read_directory(
        &mut self,
        node: NodeId,
        cookie: u64,
        maximum: usize,
    ) -> Result<Vec<DirectoryRecord>, Error> {
        let inode = self.read_allocated_inode(node)?;
        if inode.kind != NodeKind::Directory {
            return Err(Error::NotDirectory);
        }
        let maximum_cookie = 2_u64
            .checked_add((inode.size / BLOCK_SIZE_U64) * DIRECTORY_ENTRIES_PER_BLOCK as u64)
            .ok_or(Error::ArithmeticOverflow)?;
        if cookie > maximum_cookie {
            return Err(Error::InvalidCookie);
        }
        let mut records = Vec::with_capacity(maximum.min(16));
        if cookie < 1 && records.len() < maximum {
            records.push(DirectoryRecord {
                node,
                generation: inode.generation,
                kind: NodeKind::Directory,
                name: String::from("."),
                next_cookie: 1,
            });
        }
        if cookie < 2 && records.len() < maximum {
            let parent = self.read_allocated_inode(NodeId(inode.parent_inode))?;
            records.push(DirectoryRecord {
                node: NodeId(inode.parent_inode),
                generation: parent.generation,
                kind: NodeKind::Directory,
                name: String::from(".."),
                next_cookie: 2,
            });
        }
        for logical_block in 0..inode.size / BLOCK_SIZE_U64 {
            if records.len() >= maximum {
                break;
            }
            let block = self.read_directory_block(node, &inode, logical_block)?;
            for (slot, entry) in block.entries.iter().enumerate() {
                let next_cookie = 2_u64
                    .checked_add(block.cookie(slot)?)
                    .ok_or(Error::ArithmeticOverflow)?;
                if next_cookie <= cookie || entry.is_unused() {
                    continue;
                }
                records.push(DirectoryRecord {
                    node: NodeId(entry.inode),
                    generation: entry.generation,
                    kind: entry.kind,
                    name: String::from(entry.name()),
                    next_cookie,
                });
                if records.len() >= maximum {
                    break;
                }
            }
        }
        Ok(records)
    }

    pub(crate) fn read_allocated_inode(&mut self, node: NodeId) -> Result<Inode, Error> {
        let inode = self.read_inode(node)?;
        if inode.kind == NodeKind::Free {
            Err(Error::InvalidNode)
        } else {
            Ok(inode)
        }
    }

    fn read_inode(&mut self, node: NodeId) -> Result<Inode, Error> {
        let (descriptor, index) = self.inode_location(node)?;
        let byte_offset = index
            .checked_mul(INODE_BYTES)
            .ok_or(Error::ArithmeticOverflow)?;
        let block_offset = byte_offset / BLOCK_SIZE;
        let within_block = byte_offset % BLOCK_SIZE;
        let physical_block = descriptor
            .inode_table_first_block
            .checked_add(block_offset as u64)
            .ok_or(Error::ArithmeticOverflow)?;
        let mut block = [0; BLOCK_SIZE];
        self.read_block(physical_block, &mut block)?;
        let bytes = &block[within_block..within_block + INODE_BYTES];
        match Inode::decode(bytes) {
            Ok(inode) => Ok(inode),
            Err(strict_error) if self.superblock.phase3_enabled() => {
                Inode::decode_with_mode(bytes, InodeValidationMode::Phase3Orphan)
                    .map_err(|_| strict_error.into())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn inode_location(
        &self,
        node: NodeId,
    ) -> Result<(AllocationGroupDescriptor, usize), Error> {
        if node.0 == 0 {
            return Err(Error::InvalidNode);
        }
        let zero_based = node.0 - 1;
        let group_index = zero_based / u64::from(PHASE2_INODES_PER_GROUP);
        let inode_index = zero_based % u64::from(PHASE2_INODES_PER_GROUP);
        let descriptor = self
            .groups
            .get(usize::try_from(group_index).map_err(|_| Error::InvalidNode)?)
            .copied()
            .ok_or(Error::InvalidNode)?;
        Ok((
            descriptor,
            usize::try_from(inode_index).map_err(|_| Error::InvalidNode)?,
        ))
    }

    pub(crate) fn read_directory_block(
        &mut self,
        node: NodeId,
        inode: &Inode,
        logical_block: u64,
    ) -> Result<DirectoryBlock, Error> {
        let physical = physical_block(inode, logical_block)?.ok_or(Error::CorruptVolume)?;
        let mut bytes = [0; BLOCK_SIZE];
        self.read_block(physical, &mut bytes)?;
        let block = DirectoryBlock::decode(&bytes)?;
        if block.owner_inode != node.0 || block.logical_block_index != logical_block {
            return Err(Error::CorruptVolume);
        }
        Ok(block)
    }

    fn validate_volume(&mut self) -> Result<(), Error> {
        let total_inodes = self
            .groups
            .len()
            .checked_mul(PHASE2_INODES_PER_GROUP as usize)
            .ok_or(Error::ArithmeticOverflow)?;
        let mut allocated = vec![false; total_inodes + 1];
        let mut referenced = vec![false; total_inodes + 1];
        let mut owned_blocks = Vec::<u64>::new();

        for descriptor in self.groups.clone() {
            let mut bitmap = [0; BLOCK_SIZE];
            self.read_block(descriptor.block_bitmap_block, &mut bitmap)?;
            if self.superblock.phase3_enabled() {
                validate_bitmap_tail(&bitmap, descriptor.group_block_count as usize)?;
                for physical in descriptor.group_start_block..descriptor.first_data_block {
                    let bit = usize::try_from(physical - descriptor.group_start_block)
                        .map_err(|_| Error::ArithmeticOverflow)?;
                    if bitmap_test(&bitmap, bit) != Some(true) {
                        return Err(Error::CorruptVolume);
                    }
                }
            } else if bitmap.iter().any(|byte| *byte != 0) {
                return Err(Error::CorruptVolume);
            }
            self.read_block(descriptor.inode_bitmap_block, &mut bitmap)?;
            if self.superblock.phase3_enabled() {
                validate_bitmap_tail(&bitmap, descriptor.inodes_in_group as usize)?;
            } else if bitmap.iter().any(|byte| *byte != 0) {
                return Err(Error::CorruptVolume);
            }
        }

        for inode_number in 1..=total_inodes as u64 {
            let inode = self.read_inode(NodeId(inode_number))?;
            if inode.kind == NodeKind::Free {
                continue;
            }
            allocated[inode_number as usize] = true;
            if self.superblock.phase3_enabled() {
                let (descriptor, local) = self.inode_location(NodeId(inode_number))?;
                let mut bitmap = [0; BLOCK_SIZE];
                self.read_block(descriptor.inode_bitmap_block, &mut bitmap)?;
                if bitmap_test(&bitmap, local) != Some(true) {
                    return Err(Error::CorruptVolume);
                }
            }
            for extent in &inode.extents[..usize::from(inode.extent_count)] {
                for block in extent.physical_first_block..extent.physical_end()? {
                    if !self.block_is_data(block) || owned_blocks.contains(&block) {
                        return Err(Error::CorruptVolume);
                    }
                    if self.superblock.phase3_enabled() && !self.block_bitmap_value(block)? {
                        return Err(Error::CorruptVolume);
                    }
                    owned_blocks.push(block);
                }
            }
        }
        if self.superblock.phase3_enabled() {
            for inode_number in 1..=total_inodes as u64 {
                let (descriptor, local) = self.inode_location(NodeId(inode_number))?;
                let mut bitmap = [0; BLOCK_SIZE];
                self.read_block(descriptor.inode_bitmap_block, &mut bitmap)?;
                if bitmap_test(&bitmap, local) != Some(allocated[inode_number as usize]) {
                    return Err(Error::CorruptVolume);
                }
            }
            for descriptor in self.groups.clone() {
                let mut bitmap = [0; BLOCK_SIZE];
                self.read_block(descriptor.block_bitmap_block, &mut bitmap)?;
                for physical in descriptor.first_data_block..descriptor.data_end_block {
                    let bit = usize::try_from(physical - descriptor.group_start_block)
                        .map_err(|_| Error::ArithmeticOverflow)?;
                    let marked = bitmap_test(&bitmap, bit).ok_or(Error::CorruptVolume)?;
                    if marked != owned_blocks.contains(&physical) {
                        return Err(Error::CorruptVolume);
                    }
                }
            }
        }
        if self.superblock.phase3_enabled() {
            let state = self.state.ok_or(Error::CorruptVolume)?;
            let allocated_inodes = allocated.iter().skip(1).filter(|value| **value).count() as u64;
            let total_inodes = total_inodes as u64;
            let total_data_blocks = self.groups.iter().try_fold(0_u64, |total, group| {
                total
                    .checked_add(group.data_end_block - group.first_data_block)
                    .ok_or(Error::ArithmeticOverflow)
            })?;
            let expected_inodes = total_inodes - allocated_inodes;
            let expected_blocks = total_data_blocks - owned_blocks.len() as u64;
            if (state.free_inode_count != 0 || state.free_block_count != 0)
                && (state.free_inode_count != expected_inodes
                    || state.free_block_count != expected_blocks)
            {
                return Err(Error::CorruptVolume);
            }
        }
        if !allocated.get(ROOT_INODE as usize).copied().unwrap_or(false) {
            return Err(Error::CorruptVolume);
        }
        let root = self.read_allocated_inode(self.root())?;
        if root.kind != NodeKind::Directory || root.parent_inode != ROOT_INODE {
            return Err(Error::CorruptVolume);
        }
        referenced[ROOT_INODE as usize] = true;

        for inode_number in 1..=total_inodes as u64 {
            if !allocated[inode_number as usize] {
                continue;
            }
            let inode = self.read_allocated_inode(NodeId(inode_number))?;
            if inode.kind != NodeKind::Directory {
                continue;
            }
            self.validate_parent_chain(NodeId(inode_number), total_inodes)?;
            let mut names = Vec::<String>::new();
            let mut entry_count = 0_u64;
            for logical in 0..inode.size / BLOCK_SIZE_U64 {
                let block = self.read_directory_block(NodeId(inode_number), &inode, logical)?;
                for entry in block.entries.iter().filter(|entry| !entry.is_unused()) {
                    let target_index =
                        usize::try_from(entry.inode).map_err(|_| Error::CorruptVolume)?;
                    if target_index > total_inodes || !allocated[target_index] {
                        return Err(Error::CorruptVolume);
                    }
                    let target = self.read_allocated_inode(NodeId(entry.inode))?;
                    if target.generation != entry.generation || target.kind != entry.kind {
                        return Err(Error::CorruptVolume);
                    }
                    if names.iter().any(|name| name == entry.name()) {
                        return Err(Error::CorruptVolume);
                    }
                    names.push(String::from(entry.name()));
                    referenced[target_index] = true;
                    entry_count = entry_count
                        .checked_add(1)
                        .ok_or(Error::ArithmeticOverflow)?;
                }
            }
            if entry_count != inode.directory_entry_count {
                return Err(Error::CorruptVolume);
            }
        }
        if allocated
            .iter()
            .zip(&referenced)
            .enumerate()
            .skip(1)
            .any(|(_, (allocated, referenced))| *allocated && !*referenced)
        {
            return Err(Error::CorruptVolume);
        }
        Ok(())
    }

    fn validate_parent_chain(&mut self, node: NodeId, total_inodes: usize) -> Result<(), Error> {
        let mut cursor = node;
        for _ in 0..=total_inodes {
            let inode = self.read_allocated_inode(cursor)?;
            if inode.kind != NodeKind::Directory {
                return Err(Error::CorruptVolume);
            }
            if cursor == self.root() {
                return (inode.parent_inode == ROOT_INODE)
                    .then_some(())
                    .ok_or(Error::CorruptVolume);
            }
            cursor = NodeId(inode.parent_inode);
        }
        Err(Error::CorruptVolume)
    }

    fn block_is_data(&self, block: u64) -> bool {
        self.groups
            .iter()
            .any(|group| block >= group.first_data_block && block < group.data_end_block)
    }

    fn block_bitmap_value(&mut self, physical: u64) -> Result<bool, Error> {
        let group = self
            .groups
            .iter()
            .find(|group| {
                physical >= group.group_start_block
                    && physical < group.group_start_block + group.group_block_count
            })
            .copied()
            .ok_or(Error::CorruptVolume)?;
        let mut bitmap = [0; BLOCK_SIZE];
        self.read_block(group.block_bitmap_block, &mut bitmap)?;
        let bit = usize::try_from(physical - group.group_start_block)
            .map_err(|_| Error::ArithmeticOverflow)?;
        bitmap_test(&bitmap, bit).ok_or(Error::CorruptVolume)
    }

    pub(crate) fn read_block(
        &mut self,
        physical: u64,
        output: &mut [u8; BLOCK_SIZE],
    ) -> Result<(), Error> {
        if let Some(staged) = self.transaction.as_ref().and_then(|tx| tx.staged(physical)) {
            *output = *staged;
            Ok(())
        } else if let Some((_, image)) = self
            .recovery_overlay
            .iter()
            .find(|(target, _)| *target == physical)
        {
            *output = *image;
            Ok(())
        } else {
            self.device
                .read_blocks(physical, output)
                .map_err(Into::into)
        }
    }

    fn initialize_free_counts(&mut self) -> Result<(), Error> {
        let state = self.state.ok_or(Error::CorruptVolume)?;
        if state.free_block_count != 0 || state.free_inode_count != 0 {
            return Ok(());
        }
        let mut free_blocks = 0_u64;
        let mut free_inodes = 0_u64;
        for group in self.groups.clone() {
            let mut bitmap = [0; BLOCK_SIZE];
            self.read_block(group.block_bitmap_block, &mut bitmap)?;
            for physical in group.first_data_block..group.data_end_block {
                let bit = usize::try_from(physical - group.group_start_block)
                    .map_err(|_| Error::ArithmeticOverflow)?;
                free_blocks += u64::from(bitmap_test(&bitmap, bit) == Some(false));
            }
            self.read_block(group.inode_bitmap_block, &mut bitmap)?;
            for local in 0..group.inodes_in_group as usize {
                free_inodes += u64::from(bitmap_test(&bitmap, local) == Some(false));
            }
        }
        if self.mount_mode == RuntimeMountMode::ReadWrite {
            self.begin_transaction()?;
            let pending = &mut self.transaction.as_mut().ok_or(Error::CorruptVolume)?.state;
            pending.free_block_count = free_blocks;
            pending.free_inode_count = free_inodes;
            self.commit_transaction()?;
        } else {
            let current = self.state.as_mut().ok_or(Error::CorruptVolume)?;
            current.free_block_count = free_blocks;
            current.free_inode_count = free_inodes;
        }
        Ok(())
    }

    fn read_state(&mut self) -> Result<(), Error> {
        let mut block = [0; BLOCK_SIZE];
        self.device
            .read_blocks(self.superblock.filesystem_state_block, &mut block)?;
        self.state = Some(FilesystemState::decode(&block)?);
        Ok(())
    }

    fn write_superblocks(&mut self, state: VolumeState) -> Result<(), Error> {
        self.superblock.state = state;
        let encoded = self.superblock.encode()?;
        let result = (|| {
            self.device
                .write_blocks(self.superblock.backup_superblock_block, &encoded)?;
            self.device.flush()?;
            self.device.write_blocks(SUPERBLOCK_BLOCK, &encoded)?;
            self.device.flush()?;
            Ok::<(), Error>(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
}

fn discover_mount_metadata<D: BlockDevice>(
    device: &mut D,
    mount_mode: RuntimeMountMode,
) -> Result<(Superblock, Vec<AllocationGroupDescriptor>), Error> {
    if device.block_size() != BLOCK_SIZE {
        return Err(Error::Device(BlockDeviceError::InvalidBlockSize));
    }
    let device_bytes = device
        .block_count()
        .checked_mul(BLOCK_SIZE_U64)
        .ok_or(Error::ArithmeticOverflow)?;
    let mut block = [0; BLOCK_SIZE];
    device.read_blocks(SUPERBLOCK_BLOCK, &mut block)?;
    let primary = Superblock::decode(&block, Some(device_bytes), MountMode::ReadOnly).ok();
    device.read_blocks(nullfs_format::PHASE3_BACKUP_SUPERBLOCK_BLOCK, &mut block)?;
    let backup = Superblock::decode(&block, Some(device_bytes), MountMode::ReadOnly).ok();
    let superblock = match (&primary, &backup) {
        (Some(primary), _) if !primary.phase3_enabled() => primary.clone(),
        _ => select_superblock(primary, backup)?,
    };
    if !superblock.phase2_enabled() {
        return Err(Error::Phase2Required);
    }
    if mount_mode == RuntimeMountMode::ReadWrite && !superblock.phase3_enabled() {
        return Err(Error::Phase3Required);
    }
    let mut groups = Vec::with_capacity(superblock.allocation_group_count as usize);
    for table_index in 0..superblock.descriptor_reservation_blocks {
        device.read_blocks(
            superblock.first_descriptor_block + u64::from(table_index),
            &mut block,
        )?;
        let table = AllocationGroupTable::decode(&block, &superblock)?;
        groups.extend_from_slice(&table.descriptors[..usize::from(table.descriptor_count)]);
    }
    if groups.len() != superblock.allocation_group_count as usize {
        return Err(Error::CorruptVolume);
    }
    Ok((superblock, groups))
}

fn select_superblock(
    primary: Option<Superblock>,
    backup: Option<Superblock>,
) -> Result<Superblock, Error> {
    match (primary, backup) {
        (Some(primary), Some(backup)) => {
            if primary == backup {
                return Ok(primary);
            }
            let mut normalized_primary = primary.clone();
            let mut normalized_backup = backup.clone();
            normalized_primary.state = VolumeState::Clean;
            normalized_backup.state = VolumeState::Clean;
            if normalized_primary == normalized_backup {
                let mut selected = primary;
                if backup.state == VolumeState::Dirty {
                    selected.state = VolumeState::Dirty;
                }
                Ok(selected)
            } else {
                Err(Error::RedundantSuperblocksDisagree)
            }
        }
        (Some(primary), None) if primary.phase3_enabled() => Ok(primary),
        (None, Some(backup)) if backup.phase3_enabled() => Ok(backup),
        _ => Err(Error::CorruptVolume),
    }
}

fn physical_block(inode: &Inode, logical_block: u64) -> Result<Option<u64>, Error> {
    for extent in &inode.extents[..usize::from(inode.extent_count)] {
        let end = extent.logical_end()?;
        if logical_block >= extent.logical_first_block && logical_block < end {
            let relative = logical_block - extent.logical_first_block;
            return extent
                .physical_first_block
                .checked_add(relative)
                .map(Some)
                .ok_or(Error::ArithmeticOverflow);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use nullfs_blockdev::MemoryBlockDevice;
    use nullfs_format::{JournalControl, JournalTag, PHASE3_MAX_UPDATES};
    use nullfs_testkit::{CrashBlockDevice, ImageBuilder, PersistencePolicy};

    fn image() -> MemoryBlockDevice {
        let device = MemoryBlockDevice::new(BLOCK_SIZE, 4096).expect("device");
        let mut builder = ImageBuilder::new(device, [7; 16], "test").expect("builder");
        let etc = builder.create_directory(1, "etc", 0o755).expect("etc");
        builder
            .create_file(etc, "name", b"NullStar\n", 0o644)
            .expect("file");
        builder
            .create_sparse_file(
                1,
                "sparse",
                3 * BLOCK_SIZE_U64,
                &[(0, b"first"), (2 * BLOCK_SIZE_U64, b"third")],
                0o644,
            )
            .expect("sparse");
        builder.finish().expect("finish")
    }

    fn filesystem() -> Filesystem<MemoryBlockDevice> {
        Filesystem::mount(image()).expect("mount")
    }

    #[test]
    fn traverses_and_reads_files() {
        let mut filesystem = filesystem();
        let file = filesystem
            .lookup_path(filesystem.root(), "/etc/name")
            .expect("lookup");
        let mut bytes = [0; 32];
        let count = filesystem.read(file, 0, &mut bytes).expect("read");
        assert_eq!(&bytes[..count], b"NullStar\n");
        assert_eq!(filesystem.attributes(file).expect("attributes").size, 9);
    }

    #[test]
    fn sparse_holes_read_as_zero() {
        let mut filesystem = filesystem();
        let file = filesystem
            .lookup(filesystem.root(), b"sparse")
            .expect("lookup");
        let mut bytes = vec![0xff; BLOCK_SIZE * 3];
        let count = filesystem.read(file, 0, &mut bytes).expect("read");
        assert_eq!(count, bytes.len());
        assert_eq!(&bytes[..5], b"first");
        assert!(
            bytes[BLOCK_SIZE..BLOCK_SIZE * 2]
                .iter()
                .all(|byte| *byte == 0)
        );
        assert_eq!(&bytes[BLOCK_SIZE * 2..BLOCK_SIZE * 2 + 5], b"third");
    }

    #[test]
    fn writable_mount_is_dirty_until_clean_unmount_and_staged_data_remounts() {
        let mut filesystem = Filesystem::mount_read_write(image()).expect("writable mount");
        assert_eq!(filesystem.superblock().state, VolumeState::Dirty);
        let file = filesystem
            .lookup_path(filesystem.root(), "/etc/name")
            .expect("file");
        let inode = filesystem.read_allocated_inode(file).expect("inode");
        let target = inode.extents[0].physical_first_block;
        let mut replacement = [0; BLOCK_SIZE];
        replacement[..8].copy_from_slice(b"Phase 3\n");
        filesystem.stage_block(target, &replacement).expect("stage");
        let mut staged = [0; 8];
        assert_eq!(
            filesystem.read(file, 0, &mut staged).expect("staged read"),
            8
        );
        assert_eq!(&staged, b"Phase 3\n");
        let device = filesystem.unmount().expect("clean unmount");

        let mut remounted = Filesystem::mount_read_write(device).expect("remount");
        let file = remounted
            .lookup_path(remounted.root(), "/etc/name")
            .expect("file");
        let mut persisted = [0; 8];
        remounted.read(file, 0, &mut persisted).expect("read");
        assert_eq!(&persisted, b"Phase 3\n");
        let device = remounted.unmount().expect("unmount");
        let clean = Filesystem::mount(device).expect("clean read-only mount");
        assert_eq!(clean.superblock().state, VolumeState::Clean);
    }

    #[test]
    fn committed_journal_is_replayed_on_writable_mount() {
        let mut device = image();
        let mounted = Filesystem::mount(device.clone()).expect("inspect");
        let superblock = mounted.superblock.clone();
        let target = mounted.groups[0].first_data_block + 2;
        drop(mounted);
        let image = [0x5a; BLOCK_SIZE];
        let tag = JournalTag::new(9, 0, target, &image).expect("tag");
        let state = FilesystemState {
            generation: 2,
            next_transaction_id: 10,
            ..FilesystemState::initial()
        };
        let state_image = state.encode().unwrap();
        let state_tag =
            JournalTag::new(9, 1, superblock.filesystem_state_block, &state_image).unwrap();
        device
            .write_blocks(superblock.journal_first_block + 2, &tag.encode().unwrap())
            .unwrap();
        device
            .write_blocks(
                superblock.journal_first_block + 3,
                &state_tag.encode().unwrap(),
            )
            .unwrap();
        device
            .write_blocks(
                superblock.journal_first_block + 2 + PHASE3_MAX_UPDATES as u64,
                &image,
            )
            .unwrap();
        device
            .write_blocks(
                superblock.journal_first_block + 3 + PHASE3_MAX_UPDATES as u64,
                &state_image,
            )
            .unwrap();
        let control = JournalControl::committed(3, 9, &[tag, state_tag]).unwrap();
        device
            .write_blocks(superblock.journal_first_block, &control.encode().unwrap())
            .unwrap();

        let filesystem = Filesystem::mount_read_write(device).expect("recover");
        let device = filesystem.unmount().expect("unmount");
        let start = target as usize * BLOCK_SIZE;
        assert_eq!(&device.bytes()[start..start + BLOCK_SIZE], &image);
    }

    fn remount_rw(device: MemoryBlockDevice) -> Filesystem<MemoryBlockDevice> {
        Filesystem::mount_read_write(device).expect("remount writable")
    }

    #[test]
    fn mutations_survive_remount_and_reuse_inode_with_new_generation() {
        let mut fs = remount_rw(image());
        let file = fs.create(fs.root(), b"new", 0o644).expect("create");
        fs.write(file, 2 * BLOCK_SIZE_U64 + 3, b"sparse")
            .expect("sparse write");
        let device = fs.unmount().expect("unmount");
        let mut fs = remount_rw(device);
        let file = fs.lookup(fs.root(), b"new").expect("lookup");
        let generation = fs.attributes(file).unwrap().generation;
        let mut data = vec![0xff; 2 * BLOCK_SIZE + 9];
        fs.read(file, 0, &mut data).unwrap();
        assert!(data[..2 * BLOCK_SIZE + 3].iter().all(|byte| *byte == 0));
        assert_eq!(&data[2 * BLOCK_SIZE + 3..], b"sparse");
        fs.unlink(fs.root(), b"new").expect("unlink");
        let reused = fs.create(fs.root(), b"reused", 0o600).expect("reuse");
        assert_eq!(reused, file);
        assert_ne!(fs.attributes(reused).unwrap().generation, generation);
        let device = fs.unmount().unwrap();
        assert!(Filesystem::mount(device).is_ok());
    }

    #[test]
    fn directory_growth_holes_truncate_and_extent_rollback() {
        let mut fs = remount_rw(image());
        let directory = fs.mkdir(fs.root(), b"many", 0o755).unwrap();
        for index in 0..35 {
            let name = alloc::format!("entry-{index:02}");
            fs.create(directory, name.as_bytes(), 0o644).unwrap();
        }
        fs.unlink(directory, b"entry-05").unwrap();
        let replacement = fs.create(directory, b"replacement", 0o644).unwrap();
        let records = fs.read_directory(directory, 0, 64).unwrap();
        assert_eq!(records.len(), 2 + 35);

        fs.write(replacement, 0, &[0x7a; BLOCK_SIZE]).unwrap();
        fs.truncate(replacement, 7).unwrap();
        fs.truncate(replacement, BLOCK_SIZE_U64).unwrap();
        let mut tail = [0xff; 32];
        fs.read(replacement, 0, &mut tail).unwrap();
        assert_eq!(&tail[..7], &[0x7a; 7]);
        assert!(tail[7..].iter().all(|byte| *byte == 0));

        let fragmented = fs.create(fs.root(), b"fragmented", 0o644).unwrap();
        for logical in [0_u64, 2, 4, 6] {
            fs.write(fragmented, logical * BLOCK_SIZE_U64, b"x")
                .unwrap();
        }
        assert_eq!(
            fs.write(fragmented, 8 * BLOCK_SIZE_U64, b"x"),
            Err(Error::ExtentLimit)
        );
        assert_eq!(fs.attributes(fragmented).unwrap().allocated_blocks, 4);
        let device = fs.unmount().unwrap();
        assert!(Filesystem::mount(device).is_ok());
    }

    #[test]
    fn rename_replacement_cross_directory_and_cycle_checks() {
        let mut fs = remount_rw(image());
        let left = fs.mkdir(fs.root(), b"left", 0o755).unwrap();
        let right = fs.mkdir(fs.root(), b"right", 0o755).unwrap();
        let child = fs.mkdir(left, b"child", 0o755).unwrap();
        let source = fs.create(left, b"source", 0o644).unwrap();
        fs.create(right, b"target", 0o644).unwrap();
        fs.rename(left, b"source", right, b"target").unwrap();
        assert_eq!(fs.lookup(right, b"target").unwrap(), source);
        assert_eq!(fs.lookup(left, b"source"), Err(Error::NotFound));
        fs.rename(right, b"target", right, b"renamed").unwrap();
        assert_eq!(fs.lookup(right, b"renamed").unwrap(), source);
        assert_eq!(
            fs.rename(fs.root(), b"left", child, b"cycle"),
            Err(Error::DirectoryCycle)
        );
        assert_eq!(fs.rmdir(fs.root(), b"left"), Err(Error::DirectoryNotEmpty));
        fs.rmdir(left, b"child").unwrap();
        let device = fs.unmount().unwrap();
        assert!(Filesystem::mount(device).is_ok());
    }

    #[derive(Clone, Copy)]
    enum PersistenceMutation {
        Create,
        Write { node: NodeId },
        Rename { source: NodeId },
    }

    fn persistence_policies() -> Vec<PersistencePolicy> {
        let selected: Vec<u64> = (SUPERBLOCK_BLOCK..180).collect();
        let mut policies = vec![
            PersistencePolicy::DirtyBlockPrefix {
                blocks: 1,
                torn_prefix_bytes: None,
            },
            PersistencePolicy::DirtyBlockPrefix {
                blocks: 64,
                torn_prefix_bytes: None,
            },
            PersistencePolicy::DirtyBlockSet {
                blocks: selected,
                torn_prefix_bytes: None,
            },
            PersistencePolicy::WritePrefix {
                writes: 3,
                reverse: false,
                torn_prefix_bytes: None,
            },
            PersistencePolicy::WritePrefix {
                writes: 3,
                reverse: true,
                torn_prefix_bytes: None,
            },
        ];
        for bytes in [16, 63, 128, BLOCK_SIZE - 4] {
            policies.push(PersistencePolicy::DirtyBlockPrefix {
                blocks: 64,
                torn_prefix_bytes: Some(bytes),
            });
        }
        policies
    }

    fn persistence_fixture(mutation: PersistenceMutation) -> MemoryBlockDevice {
        match mutation {
            PersistenceMutation::Create => initialized_clean_image(),
            PersistenceMutation::Write { .. } => write_crash_image().0,
            PersistenceMutation::Rename { .. } => rename_crash_image(),
        }
    }

    fn perform_persistence_mutation(
        fs: &mut Filesystem<CrashBlockDevice<MemoryBlockDevice>>,
        mutation: PersistenceMutation,
    ) -> Result<(), Error> {
        match mutation {
            PersistenceMutation::Create => {
                fs.create(fs.root(), b"persist-create", 0o644).map(|_| ())
            }
            PersistenceMutation::Write { node } => {
                fs.write(node, 7, b"persistent-write").map(|_| ())
            }
            PersistenceMutation::Rename { .. } => {
                let left = fs.lookup(fs.root(), b"crash-left")?;
                let right = fs.lookup(fs.root(), b"crash-right")?;
                fs.rename(left, b"before", right, b"after")
            }
        }
    }

    fn assert_persistence_semantics(
        fs: &mut Filesystem<CrashBlockDevice<MemoryBlockDevice>>,
        mutation: PersistenceMutation,
    ) -> bool {
        match mutation {
            PersistenceMutation::Create => match fs.lookup(fs.root(), b"persist-create") {
                Ok(node) => {
                    let attributes = fs.attributes(node).unwrap();
                    assert_eq!(attributes.kind, NodeKind::Regular);
                    assert_eq!(attributes.size, 0);
                    true
                }
                Err(Error::NotFound) => false,
                result => panic!("mixed create state: {result:?}"),
            },
            PersistenceMutation::Write { node } => {
                let attributes = fs.attributes(node).unwrap();
                assert!(attributes.size == 0 || attributes.size == 23);
                if attributes.size != 0 {
                    let mut bytes = [0xff; 23];
                    fs.read(node, 0, &mut bytes).unwrap();
                    assert_eq!(&bytes[..7], &[0; 7]);
                    assert_eq!(&bytes[7..], b"persistent-write");
                }
                attributes.size != 0
            }
            PersistenceMutation::Rename { source } => {
                let left = fs.lookup(fs.root(), b"crash-left").unwrap();
                let right = fs.lookup(fs.root(), b"crash-right").unwrap();
                let old = fs.lookup(left, b"before");
                let new = fs.lookup(right, b"after");
                assert!(
                    (old == Ok(source) && new == Err(Error::NotFound))
                        || (old == Err(Error::NotFound) && new == Ok(source)),
                    "mixed rename state: old={old:?} new={new:?}"
                );
                new == Ok(source)
            }
        }
    }

    #[test]
    fn failed_flush_persistence_matrix_never_exposes_mixed_mutations() {
        let write_node = write_crash_image().1;
        let rename_baseline = rename_crash_image();
        let mut inspect = Filesystem::mount(rename_baseline.clone()).unwrap();
        let left = inspect.lookup(inspect.root(), b"crash-left").unwrap();
        let rename_source = inspect.lookup(left, b"before").unwrap();
        drop(inspect);
        let mutations = [
            PersistenceMutation::Create,
            PersistenceMutation::Write { node: write_node },
            PersistenceMutation::Rename {
                source: rename_source,
            },
        ];
        let flush_offsets = [8_u64, 10, 15, 17];
        let policies = persistence_policies();
        let mut cases = 0usize;
        let mut old = 0usize;
        let mut new = 0usize;
        let mut rejected = 0usize;

        for mutation in mutations {
            let baseline = match mutation {
                PersistenceMutation::Rename { .. } => rename_baseline.clone(),
                _ => persistence_fixture(mutation),
            };
            for flush_offset in flush_offsets {
                for policy in policies.iter().cloned() {
                    cases += 1;
                    let torn = !matches!(
                        policy,
                        PersistencePolicy::Atomic
                            | PersistencePolicy::DirtyBlockPrefix {
                                torn_prefix_bytes: None,
                                ..
                            }
                            | PersistencePolicy::DirtyBlockSet {
                                torn_prefix_bytes: None,
                                ..
                            }
                            | PersistencePolicy::WritePrefix {
                                torn_prefix_bytes: None,
                                ..
                            }
                    );
                    let crash = CrashBlockDevice::new(baseline.clone()).unwrap();
                    let mut fs = Filesystem::mount_read_write(crash).unwrap();
                    fs.device.set_persistence_policy(policy);
                    fs.device
                        .fail_at_operation(Some(fs.device.operation_count() + flush_offset));
                    assert!(perform_persistence_mutation(&mut fs, mutation).is_err());
                    let mut crash = fs.into_device();
                    crash.restart();
                    crash.fail_at_operation(None);
                    crash.set_persistence_policy(PersistencePolicy::Atomic);
                    match Filesystem::try_mount_read_write(crash) {
                        Ok(mut recovered) => {
                            if assert_persistence_semantics(&mut recovered, mutation) {
                                new += 1;
                            } else {
                                old += 1;
                            }
                            recovered.unmount().unwrap();
                        }
                        Err(failure) => {
                            rejected += 1;
                            assert!(torn, "non-torn policy rejected: {:?}", failure.error);
                            assert!(matches!(
                                failure.error,
                                Error::CorruptJournal
                                    | Error::CorruptVolume
                                    | Error::Format(_)
                                    | Error::RedundantSuperblocksDisagree
                            ));
                        }
                    }
                }
            }
        }
        assert_eq!(cases, 108);
        assert_eq!((old, new, rejected), (33, 72, 3));
    }

    #[test]
    fn failed_flush_dirty_superblock_persistence_matrix_is_recoverable_or_rejected() {
        let baseline = initialized_clean_image();
        let mut cases = 0usize;
        let mut recovered_cases = 0usize;
        let mut rejected_cases = 0usize;
        for flush_offset in [1_u64, 3] {
            for policy in persistence_policies() {
                cases += 1;
                let torn = matches!(
                    policy,
                    PersistencePolicy::DirtyBlockPrefix {
                        torn_prefix_bytes: Some(_),
                        ..
                    } | PersistencePolicy::DirtyBlockSet {
                        torn_prefix_bytes: Some(_),
                        ..
                    } | PersistencePolicy::WritePrefix {
                        torn_prefix_bytes: Some(_),
                        ..
                    }
                );
                let mut crash = CrashBlockDevice::new(baseline.clone()).unwrap();
                crash.set_persistence_policy(policy);
                crash.fail_at_operation(Some(flush_offset));
                let failure = Filesystem::try_mount_read_write(crash).err().unwrap();
                let mut crash = failure.device;
                crash.restart();
                crash.fail_at_operation(None);
                crash.set_persistence_policy(PersistencePolicy::Atomic);
                match Filesystem::try_mount_read_write(crash) {
                    Ok(recovered) => {
                        recovered_cases += 1;
                        let device = recovered.unmount().unwrap();
                        assert_eq!(
                            Filesystem::mount(device).unwrap().superblock().state,
                            VolumeState::Clean
                        );
                    }
                    Err(failure) => {
                        rejected_cases += 1;
                        assert!(torn);
                        assert!(matches!(
                            failure.error,
                            Error::CorruptVolume
                                | Error::Format(_)
                                | Error::RedundantSuperblocksDisagree
                        ));
                    }
                }
            }
        }
        assert_eq!(cases, 18);
        assert_eq!((recovered_cases, rejected_cases), (18, 0));
    }

    fn create_crash_boundary_count() -> u64 {
        let crash = CrashBlockDevice::new(image()).unwrap();
        let mut fs = Filesystem::mount_read_write(crash).unwrap();
        let start = fs.device.operation_count();
        fs.create(fs.root(), b"atomic", 0o644).unwrap();
        fs.device.operation_count() - start
    }

    #[test]
    fn create_is_old_or_new_at_every_crash_boundary() {
        let boundaries = create_crash_boundary_count();
        assert_eq!(boundaries, 18);
        for boundary in 0..boundaries {
            let crash = CrashBlockDevice::new(image()).unwrap();
            let mut fs = Filesystem::mount_read_write(crash).unwrap();
            let fail_at = fs.device.operation_count() + boundary;
            fs.device.fail_at_operation(Some(fail_at));
            assert!(fs.create(fs.root(), b"atomic", 0o644).is_err());
            let mut crash = fs.into_device();
            assert!(crash.is_crashed());
            crash.restart();
            crash.fail_at_operation(None);
            let mut recovered = Filesystem::mount_read_write(crash).unwrap();
            match recovered.lookup(recovered.root(), b"atomic") {
                Ok(node) => {
                    let attributes = recovered.attributes(node).unwrap();
                    assert_eq!(attributes.kind, NodeKind::Regular);
                    assert_eq!(attributes.size, 0);
                }
                Err(Error::NotFound) => {}
                result => panic!("boundary {boundary}: unexpected create state {result:?}"),
            }
            recovered.unmount().unwrap();
        }
    }

    fn write_crash_image() -> (MemoryBlockDevice, NodeId) {
        let mut fs = Filesystem::mount_read_write(image()).unwrap();
        let node = fs.create(fs.root(), b"crash-write", 0o644).unwrap();
        let device = fs.unmount().unwrap();
        (device, node)
    }

    #[test]
    fn write_is_old_or_fully_new_at_every_single_chunk_crash_boundary() {
        let (baseline, node) = write_crash_image();
        let payload = vec![0xa6; 3 * BLOCK_SIZE + 17];
        let crash = CrashBlockDevice::new(baseline.clone()).unwrap();
        let mut fs = Filesystem::mount_read_write(crash).unwrap();
        let start = fs.device.operation_count();
        assert_eq!(fs.write(node, 11, &payload).unwrap(), payload.len());
        let boundaries = fs.device.operation_count() - start;
        assert_eq!(boundaries, 27);

        for boundary in 0..boundaries {
            let crash = CrashBlockDevice::new(baseline.clone()).unwrap();
            let mut fs = Filesystem::mount_read_write(crash).unwrap();
            let fail_at = fs.device.operation_count() + boundary;
            fs.device.fail_at_operation(Some(fail_at));
            assert!(fs.write(node, 11, &payload).is_err());
            let mut crash = fs.into_device();
            crash.restart();
            crash.fail_at_operation(None);
            let mut recovered = Filesystem::mount_read_write(crash).unwrap();
            let attributes = recovered.attributes(node).unwrap();
            assert!(attributes.size == 0 || attributes.size == payload.len() as u64 + 11);
            if attributes.size != 0 {
                let mut bytes = vec![0xff; attributes.size as usize];
                recovered.read(node, 0, &mut bytes).unwrap();
                assert!(bytes[..11].iter().all(|byte| *byte == 0));
                assert_eq!(&bytes[11..], &payload);
            }
            recovered.unmount().unwrap();
        }
    }

    fn rename_crash_image() -> MemoryBlockDevice {
        let mut fs = Filesystem::mount_read_write(image()).unwrap();
        let left = fs.mkdir(fs.root(), b"crash-left", 0o755).unwrap();
        fs.mkdir(fs.root(), b"crash-right", 0o755).unwrap();
        fs.create(left, b"before", 0o644).unwrap();
        fs.unmount().unwrap()
    }

    #[test]
    fn cross_directory_rename_is_old_or_new_at_every_crash_boundary() {
        let baseline = rename_crash_image();
        let crash = CrashBlockDevice::new(baseline.clone()).unwrap();
        let mut fs = Filesystem::mount_read_write(crash).unwrap();
        let left = fs.lookup(fs.root(), b"crash-left").unwrap();
        let right = fs.lookup(fs.root(), b"crash-right").unwrap();
        let source = fs.lookup(left, b"before").unwrap();
        let start = fs.device.operation_count();
        fs.rename(left, b"before", right, b"after").unwrap();
        let boundaries = fs.device.operation_count() - start;
        assert_eq!(boundaries, 18);

        for boundary in 0..boundaries {
            let crash = CrashBlockDevice::new(baseline.clone()).unwrap();
            let mut fs = Filesystem::mount_read_write(crash).unwrap();
            let left = fs.lookup(fs.root(), b"crash-left").unwrap();
            let right = fs.lookup(fs.root(), b"crash-right").unwrap();
            let fail_at = fs.device.operation_count() + boundary;
            fs.device.fail_at_operation(Some(fail_at));
            assert!(fs.rename(left, b"before", right, b"after").is_err());
            let mut crash = fs.into_device();
            crash.restart();
            crash.fail_at_operation(None);
            let mut recovered = Filesystem::mount_read_write(crash).unwrap();
            let left = recovered.lookup(recovered.root(), b"crash-left").unwrap();
            let right = recovered.lookup(recovered.root(), b"crash-right").unwrap();
            let old = recovered.lookup(left, b"before");
            let new = recovered.lookup(right, b"after");
            assert!(
                (old == Ok(source) && new == Err(Error::NotFound))
                    || (old == Err(Error::NotFound) && new == Ok(source)),
                "boundary {boundary}: old={old:?} new={new:?}"
            );
            recovered.unmount().unwrap();
        }
    }

    #[test]
    fn counters_track_allocations_and_frees_transactionally() {
        let mut fs = Filesystem::mount_read_write(image()).unwrap();
        let initial = fs.state.unwrap();
        let node = fs.create(fs.root(), b"counted", 0o644).unwrap();
        assert_eq!(
            fs.state.unwrap().free_inode_count,
            initial.free_inode_count - 1
        );
        fs.write(node, 0, &[1; BLOCK_SIZE + 1]).unwrap();
        assert_eq!(
            fs.state.unwrap().free_block_count,
            initial.free_block_count - 2
        );
        fs.unlink(fs.root(), b"counted").unwrap();
        assert_eq!(fs.state.unwrap().free_inode_count, initial.free_inode_count);
        assert_eq!(fs.state.unwrap().free_block_count, initial.free_block_count);
        let device = fs.unmount().unwrap();
        assert!(Filesystem::mount(device).is_ok());
    }

    #[test]
    fn unlink_open_preserves_handle_until_final_close() {
        let mut fs = Filesystem::mount_read_write(image()).unwrap();
        let node = fs.create(fs.root(), b"open-file", 0o644).unwrap();
        fs.write(node, 0, b"before").unwrap();
        let handle = fs.open_node(node).unwrap();
        fs.unlink(fs.root(), b"open-file").unwrap();
        assert_eq!(fs.lookup(fs.root(), b"open-file"), Err(Error::NotFound));
        let mut bytes = [0; 6];
        fs.read_handle(handle, 0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"before");
        fs.write_handle(handle, 0, b"after!").unwrap();
        fs.close_node(handle).unwrap();
        assert_eq!(fs.validate_handle(handle), Err(Error::InvalidHandle));
        let reused = fs.create(fs.root(), b"reused-open", 0o644).unwrap();
        assert_eq!(reused, node);
        assert_ne!(fs.attributes(reused).unwrap().generation, handle.generation);
        let device = fs.unmount().unwrap();
        assert!(Filesystem::mount(device).is_ok());
    }

    #[test]
    fn mount_reclaims_orphans_and_rejects_orphan_cycles() {
        let mut fs = Filesystem::mount_read_write(image()).unwrap();
        let node = fs.create(fs.root(), b"restart-orphan", 0o644).unwrap();
        fs.write(node, 0, b"payload").unwrap();
        let _handle = fs.open_node(node).unwrap();
        fs.unlink(fs.root(), b"restart-orphan").unwrap();
        let device = fs.unmount().unwrap();
        let mut recovered = Filesystem::mount_read_write(device).unwrap();
        let reused = recovered
            .create(recovered.root(), b"after-recovery", 0o644)
            .unwrap();
        assert_eq!(reused, node);
        let device = recovered.unmount().unwrap();

        let mut fs = Filesystem::mount_read_write(device).unwrap();
        let orphan = fs.create(fs.root(), b"bad-orphan", 0o644).unwrap();
        let _handle = fs.open_node(orphan).unwrap();
        fs.unlink(fs.root(), b"bad-orphan").unwrap();
        let mut inode = fs.read_allocated_inode(orphan).unwrap();
        inode.parent_inode = orphan.0;
        let (group, local) = fs.inode_location(orphan).unwrap();
        let byte = local * INODE_BYTES;
        let physical = group.inode_table_first_block + (byte / BLOCK_SIZE) as u64;
        let within = byte % BLOCK_SIZE;
        let mut block = [0; BLOCK_SIZE];
        fs.read_block(physical, &mut block).unwrap();
        block[within..within + INODE_BYTES].copy_from_slice(
            &inode
                .encode_with_mode(InodeValidationMode::Phase3Orphan)
                .unwrap(),
        );
        fs.begin_transaction().unwrap();
        fs.stage_block(physical, &block).unwrap();
        fs.commit_transaction().unwrap();
        let device = fs.unmount().unwrap();
        assert!(matches!(
            Filesystem::mount_read_write(device),
            Err(Error::CorruptVolume)
        ));
    }

    #[test]
    fn read_only_mount_uses_committed_after_image_overlay_and_state_is_mandatory() {
        let mut device = image();
        let mut mounted = Filesystem::mount(device.clone()).unwrap();
        let sb = mounted.superblock.clone();
        let node = mounted.lookup_path(mounted.root(), "/etc/name").unwrap();
        let target = mounted.read_allocated_inode(node).unwrap().extents[0].physical_first_block;
        let mut after = [0; BLOCK_SIZE];
        after[..7].copy_from_slice(b"overlay");
        let data_tag = JournalTag::new(1, 0, target, &after).unwrap();
        let old = mounted.state.unwrap();
        let next = FilesystemState {
            generation: old.generation + 1,
            next_transaction_id: 2,
            ..old
        };
        let state_image = next.encode().unwrap();
        let state_tag = JournalTag::new(1, 1, sb.filesystem_state_block, &state_image).unwrap();
        drop(mounted);
        for (index, (tag, image)) in [(data_tag, after), (state_tag, state_image)]
            .iter()
            .enumerate()
        {
            device
                .write_blocks(
                    sb.journal_first_block + 2 + index as u64,
                    &tag.encode().unwrap(),
                )
                .unwrap();
            device
                .write_blocks(
                    sb.journal_first_block + 2 + PHASE3_MAX_UPDATES as u64 + index as u64,
                    image,
                )
                .unwrap();
        }
        let control = JournalControl::committed(3, 1, &[data_tag, state_tag]).unwrap();
        device
            .write_blocks(sb.journal_first_block, &control.encode().unwrap())
            .unwrap();
        let mut read_only = Filesystem::mount(device.clone()).unwrap();
        let mut bytes = [0; 7];
        read_only.read(node, 0, &mut bytes).unwrap();
        assert_eq!(&bytes, b"overlay");

        let missing = JournalControl::committed(4, 1, &[data_tag]).unwrap();
        device
            .write_blocks(sb.journal_first_block + 1, &missing.encode().unwrap())
            .unwrap();
        assert!(matches!(
            Filesystem::mount_read_write(device),
            Err(Error::CorruptJournal)
        ));
    }

    #[test]
    fn superblocks_differing_only_in_state_prefer_dirty_and_are_repaired() {
        let mut device = image();
        let mut block = [0; BLOCK_SIZE];
        device.read_blocks(SUPERBLOCK_BLOCK, &mut block).unwrap();
        let mut sb = Superblock::decode(
            &block,
            Some(device.block_count() * BLOCK_SIZE_U64),
            MountMode::ReadOnly,
        )
        .unwrap();
        sb.state = VolumeState::Dirty;
        device
            .write_blocks(sb.backup_superblock_block, &sb.encode().unwrap())
            .unwrap();
        let read_only = Filesystem::mount(device).unwrap();
        assert_eq!(read_only.superblock().state, VolumeState::Dirty);
        let device = read_only.into_device();
        let writable = Filesystem::mount_read_write(device).unwrap();
        let device = writable.unmount().unwrap();
        let clean = Filesystem::mount(device).unwrap();
        assert_eq!(clean.superblock().state, VolumeState::Clean);
    }

    fn initialized_clean_image() -> MemoryBlockDevice {
        Filesystem::mount_read_write(image())
            .unwrap()
            .unmount()
            .unwrap()
    }

    fn counter_initialization_image() -> MemoryBlockDevice {
        let mut device = image();
        device
            .write_blocks(
                nullfs_format::PHASE3_FILESYSTEM_STATE_BLOCK,
                &FilesystemState::initial().encode().unwrap(),
            )
            .unwrap();
        device.flush().unwrap();
        device
    }

    fn orphan_mount_image() -> (MemoryBlockDevice, NodeId) {
        let mut fs = Filesystem::mount_read_write(image()).unwrap();
        let node = fs.create(fs.root(), b"mount-orphan", 0o644).unwrap();
        let _handle = fs.open_node(node).unwrap();
        fs.unlink(fs.root(), b"mount-orphan").unwrap();
        (fs.unmount().unwrap(), node)
    }

    fn journal_replay_mount_image() -> (MemoryBlockDevice, NodeId) {
        let mut setup = Filesystem::mount_read_write(image()).unwrap();
        let node = setup.create(setup.root(), b"mount-replay", 0o644).unwrap();
        let baseline = setup.unmount().unwrap();
        let crash = CrashBlockDevice::new(baseline).unwrap();
        let mut fs = Filesystem::mount_read_write(crash).unwrap();
        // One data block, one allocation bitmap, one inode-table block, and state:
        // eight tag/image writes, payload flush, committed-control write/flush, then home writes.
        fs.device
            .fail_at_operation(Some(fs.device.operation_count() + 11));
        assert!(fs.write(node, 0, b"replayed").is_err());
        let mut crash = fs.into_device();
        crash.restart();
        (crash.into_inner().unwrap(), node)
    }

    fn assert_all_mount_boundaries_recover(
        baseline: MemoryBlockDevice,
        verify: impl Fn(&mut Filesystem<CrashBlockDevice<MemoryBlockDevice>>),
    ) -> u64 {
        let crash = CrashBlockDevice::new(baseline.clone()).unwrap();
        let mounted = Filesystem::try_mount_read_write(crash).unwrap();
        let boundaries = mounted.device.operation_count();
        mounted.unmount().unwrap();
        assert!(boundaries > 0);
        for boundary in 0..boundaries {
            let mut crash = CrashBlockDevice::new(baseline.clone()).unwrap();
            crash.fail_at_operation(Some(boundary));
            let failure = Filesystem::try_mount_read_write(crash)
                .err()
                .expect("injected mount failure");
            assert_eq!(failure.error, Error::Device(BlockDeviceError::Io));
            let mut crash = failure.device;
            crash.restart();
            crash.fail_at_operation(None);
            let mut recovered = Filesystem::try_mount_read_write(crash).unwrap();
            verify(&mut recovered);
            recovered.unmount().unwrap();
        }
        boundaries
    }

    #[test]
    fn ownership_preserving_mount_recovers_every_dirty_publication_boundary() {
        let clean = initialized_clean_image();
        assert_eq!(
            assert_all_mount_boundaries_recover(clean.clone(), |_| {}),
            4
        );

        let mut stale = clean;
        let mut block = [0; BLOCK_SIZE];
        stale.read_blocks(SUPERBLOCK_BLOCK, &mut block).unwrap();
        let mut sb = Superblock::decode(
            &block,
            Some(stale.block_count() * BLOCK_SIZE_U64),
            MountMode::ReadOnly,
        )
        .unwrap();
        sb.state = VolumeState::Dirty;
        stale
            .write_blocks(sb.backup_superblock_block, &sb.encode().unwrap())
            .unwrap();
        assert_eq!(assert_all_mount_boundaries_recover(stale, |_| {}), 4);
    }

    #[test]
    fn ownership_preserving_mount_recovers_counter_orphan_and_journal_boundaries() {
        assert_eq!(
            assert_all_mount_boundaries_recover(counter_initialization_image(), |_| {}),
            13
        );

        let (orphan_image, orphan) = orphan_mount_image();
        assert_eq!(
            assert_all_mount_boundaries_recover(orphan_image, |fs| {
                assert!(matches!(fs.attributes(orphan), Err(Error::InvalidNode)));
            }),
            19
        );

        let (journal_image, node) = journal_replay_mount_image();
        assert_eq!(
            assert_all_mount_boundaries_recover(journal_image, |fs| {
                let mut bytes = [0; 8];
                fs.read(node, 0, &mut bytes).unwrap();
                assert_eq!(&bytes, b"replayed");
            }),
            11
        );
    }

    #[test]
    fn try_unmount_can_retry_every_failure_boundary_after_restart() {
        let baseline = initialized_clean_image();
        let crash = CrashBlockDevice::new(baseline.clone()).unwrap();
        let mut mounted = Filesystem::mount_read_write(crash).unwrap();
        let start = mounted.device.operation_count();
        mounted.try_unmount().unwrap();
        let boundaries = mounted.device.operation_count() - start;
        assert_eq!(boundaries, 5);

        for boundary in 0..boundaries {
            let crash = CrashBlockDevice::new(baseline.clone()).unwrap();
            let mut fs = Filesystem::mount_read_write(crash).unwrap();
            let fail_at = fs.device.operation_count() + boundary;
            fs.device.fail_at_operation(Some(fail_at));
            assert!(fs.try_unmount().is_err());
            fs.device.restart();
            fs.device.fail_at_operation(None);
            fs.try_unmount().expect("retry unmount");
            let crash = fs.into_device();
            assert_eq!(
                Filesystem::mount(crash).unwrap().superblock().state,
                VolumeState::Clean
            );
        }
    }

    #[test]
    fn crash_before_final_close_is_reclaimed_on_restart() {
        let crash = CrashBlockDevice::new(image()).unwrap();
        let mut fs = Filesystem::mount_read_write(crash).unwrap();
        let node = fs.create(fs.root(), b"close-crash", 0o644).unwrap();
        fs.write(node, 0, b"durable orphan data").unwrap();
        let handle = fs.open_node(node).unwrap();
        fs.unlink(fs.root(), b"close-crash").unwrap();
        let fail_at = fs.device.operation_count();
        fs.device.fail_at_operation(Some(fail_at));
        assert!(fs.close_node(handle).is_err());
        let mut crash = fs.into_device();
        crash.restart();
        crash.fail_at_operation(None);
        let mut recovered = Filesystem::mount_read_write(crash).unwrap();
        let reused = recovered
            .create(recovered.root(), b"close-reused", 0o644)
            .unwrap();
        assert_eq!(reused, node);
        recovered.unmount().unwrap();
    }

    #[test]
    fn unaligned_write_crosses_allocation_group_with_budgeted_chunks() {
        let device = MemoryBlockDevice::new(BLOCK_SIZE, 9000).unwrap();
        let builder = ImageBuilder::new(device, [9; 16], "groups").unwrap();
        let mut fs = Filesystem::mount_read_write(builder.finish().unwrap()).unwrap();
        let node = fs.create(fs.root(), b"cross-group", 0o644).unwrap();
        let first = fs.groups[0].first_data_block + 1;
        let bytes_to_boundary = (fs.groups[0].data_end_block - first) as usize * BLOCK_SIZE;
        let payload = vec![0x3c; bytes_to_boundary];
        assert_eq!(fs.write(node, 3, &payload).unwrap(), payload.len());
        assert!(
            fs.attributes(node).unwrap().allocated_blocks > (bytes_to_boundary / BLOCK_SIZE) as u64
        );
        let device = fs.unmount().unwrap();
        let mut fs = Filesystem::mount(device).unwrap();
        let mut edge = [0; 8];
        fs.read(node, bytes_to_boundary as u64 - 1, &mut edge)
            .unwrap();
        assert_eq!(&edge[..4], &[0x3c; 4]);
    }

    #[derive(Clone)]
    struct ModelNode {
        directory: bool,
        parent: u64,
        children: BTreeMap<String, u64>,
        data: Vec<u8>,
    }

    struct Model {
        nodes: BTreeMap<u64, ModelNode>,
        serial: u64,
    }

    impl Model {
        fn new() -> Self {
            let mut nodes = BTreeMap::new();
            nodes.insert(
                ROOT_INODE,
                ModelNode {
                    directory: true,
                    parent: ROOT_INODE,
                    children: BTreeMap::new(),
                    data: Vec::new(),
                },
            );
            Self { nodes, serial: 0 }
        }

        fn name(&mut self, prefix: &str) -> String {
            self.serial += 1;
            alloc::format!("{prefix}-{}", self.serial)
        }

        fn ids_where(&self, predicate: impl Fn(&ModelNode) -> bool) -> Vec<u64> {
            self.nodes
                .iter()
                .filter_map(|(id, node)| predicate(node).then_some(*id))
                .collect()
        }

        fn child_location(&self, node: u64) -> Option<(u64, String)> {
            let parent = self.nodes.get(&node)?.parent;
            self.nodes
                .get(&parent)?
                .children
                .iter()
                .find_map(|(name, child)| (*child == node).then(|| (parent, name.clone())))
        }

        fn is_descendant(&self, ancestor: u64, mut node: u64) -> bool {
            for _ in 0..=self.nodes.len() {
                if node == ancestor {
                    return true;
                }
                if node == ROOT_INODE {
                    return false;
                }
                let Some(current) = self.nodes.get(&node) else {
                    return false;
                };
                node = current.parent;
            }
            true
        }
    }

    struct Prng(u64);

    impl Prng {
        fn next(&mut self) -> u64 {
            let mut value = self.0;
            value ^= value << 13;
            value ^= value >> 7;
            value ^= value << 17;
            self.0 = value;
            value
        }

        fn index(&mut self, length: usize) -> Option<usize> {
            (length != 0).then(|| self.next() as usize % length)
        }
    }

    fn randomized_image(seed: u64) -> MemoryBlockDevice {
        let device = MemoryBlockDevice::new(BLOCK_SIZE, 4096).unwrap();
        ImageBuilder::new(
            device,
            seed.to_le_bytes().repeat(2).try_into().unwrap(),
            "model",
        )
        .unwrap()
        .finish()
        .unwrap()
    }

    fn assert_model_tree(fs: &mut Filesystem<MemoryBlockDevice>, model: &Model, node: u64) {
        let expected = model.nodes.get(&node).expect("model node");
        let attributes = fs.attributes(NodeId(node)).expect("attributes");
        if expected.directory {
            assert_eq!(attributes.kind, NodeKind::Directory);
            let records = fs.read_directory(NodeId(node), 0, 1024).expect("listing");
            let actual: BTreeMap<String, u64> = records
                .into_iter()
                .filter(|record| record.name != "." && record.name != "..")
                .map(|record| (record.name, record.node.0))
                .collect();
            assert_eq!(actual, expected.children, "directory inode {node}");
            for child in expected.children.values() {
                assert_model_tree(fs, model, *child);
            }
        } else {
            assert_eq!(attributes.kind, NodeKind::Regular);
            assert_eq!(attributes.size, expected.data.len() as u64);
            let mut actual = vec![0; expected.data.len()];
            assert_eq!(fs.read(NodeId(node), 0, &mut actual).unwrap(), actual.len());
            assert_eq!(actual, expected.data, "file inode {node}");
        }
    }

    fn remount_and_compare(
        fs: Filesystem<MemoryBlockDevice>,
        model: &Model,
    ) -> Filesystem<MemoryBlockDevice> {
        let device = fs.unmount().expect("model clean unmount");
        let mut fs = Filesystem::mount_read_write(device).expect("model remount");
        assert_model_tree(&mut fs, model, ROOT_INODE);
        fs
    }

    #[test]
    fn deterministic_randomized_namespace_and_data_model() {
        for seed in [
            0x1234_5678_9abc_def1,
            0x5eed_f00d_cafe_babe,
            0xd1ce_ba5e_1020_3040,
        ] {
            let mut random = Prng(seed);
            let mut model = Model::new();
            let mut fs = Filesystem::mount_read_write(randomized_image(seed)).unwrap();

            for step in 0..220 {
                let directories = model.ids_where(|node| node.directory);
                let files = model.ids_where(|node| !node.directory);
                match random.next() % 10 {
                    0 if model.nodes.len() < 70 => {
                        let parent = directories[random.index(directories.len()).unwrap()];
                        let name = model.name("file");
                        match fs.create(NodeId(parent), name.as_bytes(), 0o644) {
                            Ok(node) => {
                                model
                                    .nodes
                                    .get_mut(&parent)
                                    .unwrap()
                                    .children
                                    .insert(name, node.0);
                                model.nodes.insert(
                                    node.0,
                                    ModelNode {
                                        directory: false,
                                        parent,
                                        children: BTreeMap::new(),
                                        data: Vec::new(),
                                    },
                                );
                            }
                            Err(Error::NoSpace | Error::TransactionTooLarge) => {}
                            result => panic!("seed {seed:#x} step {step} create: {result:?}"),
                        }
                    }
                    1 if model.nodes.len() < 70 => {
                        let parent = directories[random.index(directories.len()).unwrap()];
                        let name = model.name("dir");
                        match fs.mkdir(NodeId(parent), name.as_bytes(), 0o755) {
                            Ok(node) => {
                                model
                                    .nodes
                                    .get_mut(&parent)
                                    .unwrap()
                                    .children
                                    .insert(name, node.0);
                                model.nodes.insert(
                                    node.0,
                                    ModelNode {
                                        directory: true,
                                        parent,
                                        children: BTreeMap::new(),
                                        data: Vec::new(),
                                    },
                                );
                            }
                            Err(
                                Error::NoSpace | Error::ExtentLimit | Error::TransactionTooLarge,
                            ) => {}
                            result => panic!("seed {seed:#x} step {step} mkdir: {result:?}"),
                        }
                    }
                    2 if !files.is_empty() => {
                        let node = files[random.index(files.len()).unwrap()];
                        let old_len = model.nodes[&node].data.len();
                        let offset = match random.next() % 4 {
                            0 => 0,
                            1 => old_len as u64,
                            2 => random.next() % (old_len as u64 + 1),
                            _ => old_len as u64 + random.next() % (BLOCK_SIZE_U64 * 2),
                        };
                        let length = (random.next() as usize % 97) + 1;
                        let bytes = vec![(random.next() & 0xff) as u8; length];
                        match fs.write(NodeId(node), offset, &bytes) {
                            Ok(written) => {
                                let data = &mut model.nodes.get_mut(&node).unwrap().data;
                                let end = offset as usize + written;
                                data.resize(data.len().max(end), 0);
                                data[offset as usize..end].copy_from_slice(&bytes[..written]);
                            }
                            Err(
                                Error::ExtentLimit | Error::TransactionTooLarge | Error::NoSpace,
                            ) => {}
                            result => panic!("seed {seed:#x} step {step} write: {result:?}"),
                        }
                    }
                    3 if !files.is_empty() => {
                        let node = files[random.index(files.len()).unwrap()];
                        let old_len = model.nodes[&node].data.len();
                        let size = random.next() as usize % (old_len + BLOCK_SIZE * 2 + 1);
                        fs.truncate(NodeId(node), size as u64).unwrap();
                        model.nodes.get_mut(&node).unwrap().data.resize(size, 0);
                    }
                    4 if !files.is_empty() => {
                        let node = files[random.index(files.len()).unwrap()];
                        let (parent, name) = model.child_location(node).unwrap();
                        fs.unlink(NodeId(parent), name.as_bytes()).unwrap();
                        model.nodes.get_mut(&parent).unwrap().children.remove(&name);
                        model.nodes.remove(&node);
                    }
                    5 => {
                        let empty = model.ids_where(|node| {
                            node.directory && node.children.is_empty() && node.parent != ROOT_INODE
                        });
                        if let Some(index) = random.index(empty.len()) {
                            let node = empty[index];
                            let (parent, name) = model.child_location(node).unwrap();
                            fs.rmdir(NodeId(parent), name.as_bytes()).unwrap();
                            model.nodes.get_mut(&parent).unwrap().children.remove(&name);
                            model.nodes.remove(&node);
                        }
                    }
                    6 if model.nodes.len() > 1 => {
                        let movable: Vec<u64> = model
                            .nodes
                            .keys()
                            .copied()
                            .filter(|id| *id != ROOT_INODE)
                            .collect();
                        let node = movable[random.index(movable.len()).unwrap()];
                        let candidates: Vec<u64> = directories
                            .iter()
                            .copied()
                            .filter(|parent| !model.is_descendant(node, *parent))
                            .collect();
                        if let Some(index) = random.index(candidates.len()) {
                            let new_parent = candidates[index];
                            let (old_parent, old_name) = model.child_location(node).unwrap();
                            let new_name = model.name("moved");
                            fs.rename(
                                NodeId(old_parent),
                                old_name.as_bytes(),
                                NodeId(new_parent),
                                new_name.as_bytes(),
                            )
                            .unwrap();
                            model
                                .nodes
                                .get_mut(&old_parent)
                                .unwrap()
                                .children
                                .remove(&old_name);
                            model
                                .nodes
                                .get_mut(&new_parent)
                                .unwrap()
                                .children
                                .insert(new_name, node);
                            model.nodes.get_mut(&node).unwrap().parent = new_parent;
                        }
                    }
                    7 => {
                        let node_ids: Vec<u64> = model.nodes.keys().copied().collect();
                        let node = node_ids[random.index(node_ids.len()).unwrap()];
                        assert_model_tree(&mut fs, &model, node);
                    }
                    8 => {
                        let parent = directories[random.index(directories.len()).unwrap()];
                        assert_eq!(
                            fs.unlink(NodeId(parent), b"definitely-missing"),
                            Err(Error::NotFound)
                        );
                    }
                    _ => {
                        let parent = directories[random.index(directories.len()).unwrap()];
                        if let Some(name) = model.nodes[&parent].children.keys().next() {
                            assert_eq!(
                                fs.create(NodeId(parent), name.as_bytes(), 0o644),
                                Err(Error::AlreadyExists)
                            );
                        }
                    }
                }
                if step % 25 == 24 {
                    fs = remount_and_compare(fs, &model);
                }
            }
            fs = remount_and_compare(fs, &model);
            fs.unmount().unwrap();
        }
    }

    #[test]
    fn directory_cookies_resume_deterministically() {
        let mut filesystem = filesystem();
        let first = filesystem
            .read_directory(filesystem.root(), 0, 3)
            .expect("first page");
        assert_eq!(first[0].name, ".");
        assert_eq!(first[1].name, "..");
        let cookie = first.last().expect("entry").next_cookie;
        let second = filesystem
            .read_directory(filesystem.root(), cookie, 8)
            .expect("second page");
        assert!(second.iter().all(|entry| entry.next_cookie > cookie));
    }
}
