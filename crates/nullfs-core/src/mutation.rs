use alloc::vec::Vec;
use core::str;

use nullfs_blockdev::BlockDevice;
use nullfs_format::{
    BLOCK_SIZE, BLOCK_SIZE_U64, DirectoryBlock, DirectoryEntry, Extent, INLINE_EXTENT_COUNT,
    INODE_BYTES, Inode, InodeValidationMode, NodeKind, bitmap_set, bitmap_test,
};

use crate::{Error, Filesystem, NodeId, OpenHandle, OpenRecord, physical_block};

impl<D: BlockDevice> Filesystem<D> {
    pub fn open_node(&mut self, node: NodeId) -> Result<OpenHandle, Error> {
        let inode = self.read_allocated_inode(node)?;
        if inode.link_count == 0 {
            return Err(Error::InvalidNode);
        }
        let handle = OpenHandle {
            id: self.next_handle_id,
            node,
            generation: inode.generation,
            kind: inode.kind,
        };
        self.next_handle_id = self
            .next_handle_id
            .checked_add(1)
            .ok_or(Error::ArithmeticOverflow)?;
        self.open_handles.push(OpenRecord { handle });
        Ok(handle)
    }

    pub fn validate_handle(&mut self, handle: OpenHandle) -> Result<NodeId, Error> {
        let record = self
            .open_handles
            .iter()
            .find(|record| record.handle.id == handle.id)
            .copied()
            .ok_or(Error::InvalidHandle)?;
        if record.handle != handle {
            return Err(Error::InvalidHandle);
        }
        let inode = self.read_allocated_inode(handle.node)?;
        if inode.generation != handle.generation || inode.kind != handle.kind {
            return Err(Error::InvalidHandle);
        }
        Ok(handle.node)
    }

    pub fn read_handle(
        &mut self,
        handle: OpenHandle,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, Error> {
        let node = self.validate_handle(handle)?;
        self.read(node, offset, output)
    }

    pub fn write_handle(
        &mut self,
        handle: OpenHandle,
        offset: u64,
        input: &[u8],
    ) -> Result<usize, Error> {
        let node = self.validate_handle(handle)?;
        self.write(node, offset, input)
    }

    pub fn close_node(&mut self, handle: OpenHandle) -> Result<(), Error> {
        let position = self
            .open_handles
            .iter()
            .position(|record| record.handle.id == handle.id)
            .ok_or(Error::InvalidHandle)?;
        if self.open_handles[position].handle != handle {
            return Err(Error::InvalidHandle);
        }
        let record = self.open_handles.swap_remove(position);
        if self.open_handles.iter().any(|record| {
            record.handle.node == handle.node && record.handle.generation == handle.generation
        }) {
            return Ok(());
        }
        let result = (|| {
            let inode = self.read_allocated_inode(handle.node)?;
            if inode.generation == handle.generation && inode.link_count == 0 {
                self.reclaim_one_orphan(handle.node, &inode)?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.open_handles.push(record);
        }
        result
    }
    pub fn create(&mut self, parent: NodeId, name: &[u8], mode: u16) -> Result<NodeId, Error> {
        self.create_node(parent, name, mode, NodeKind::Regular)
    }

    pub fn create_directory(
        &mut self,
        parent: NodeId,
        name: &[u8],
        mode: u16,
    ) -> Result<NodeId, Error> {
        self.create_node(parent, name, mode, NodeKind::Directory)
    }

    pub fn mkdir(&mut self, parent: NodeId, name: &[u8], mode: u16) -> Result<NodeId, Error> {
        self.create_directory(parent, name, mode)
    }

    fn create_node(
        &mut self,
        parent: NodeId,
        name: &[u8],
        mode: u16,
        kind: NodeKind,
    ) -> Result<NodeId, Error> {
        self.start_mutation()?;
        let result = (|| {
            let name = valid_name(name)?;
            let mut parent_inode = self.read_allocated_inode(parent)?;
            if parent_inode.kind != NodeKind::Directory {
                return Err(Error::NotDirectory);
            }
            if self.find_entry(parent, &parent_inode, name)?.is_some() {
                return Err(Error::AlreadyExists);
            }
            let node = self.allocate_inode()?;
            let generation = self.next_generation_value()?;
            let mut inode = Inode {
                kind,
                mode,
                link_count: 1,
                generation,
                parent_inode: parent.0,
                ..Inode::default()
            };
            if kind == NodeKind::Directory {
                let physical = self.allocate_data_block()?;
                let directory = DirectoryBlock::new(node.0, 0)?;
                self.stage_block(physical, &directory.encode()?)?;
                inode.size = BLOCK_SIZE_U64;
                inode.allocated_blocks = 1;
                inode.extent_count = 1;
                inode.extents[0] = Extent {
                    logical_first_block: 0,
                    physical_first_block: physical,
                    length_blocks: 1,
                    flags: 0,
                };
            }
            self.write_inode(node, &inode)?;
            self.insert_entry(parent, &mut parent_inode, name, node, &inode)?;
            self.write_inode(parent, &parent_inode)?;
            self.commit_transaction()?;
            Ok(node)
        })();
        self.finish_mutation(result)
    }

    pub fn write(&mut self, node: NodeId, offset: u64, input: &[u8]) -> Result<usize, Error> {
        self.ensure_writable()?;
        if input.is_empty() {
            return Ok(0);
        }
        let mut committed = 0usize;
        while committed < input.len() {
            let chunk_offset = offset
                .checked_add(committed as u64)
                .ok_or(Error::ArithmeticOverflow)?;
            let update_budget = usize::try_from(self.superblock.journal_max_updates)
                .map_err(|_| Error::ArithmeticOverflow)?;
            // Reserve state, inode-table, and two allocation-bitmap images (a chunk may
            // cross one allocation-group boundary).
            let data_budget = update_budget
                .checked_sub(4)
                .ok_or(Error::TransactionTooLarge)?;
            let first_capacity = data_budget
                .checked_mul(BLOCK_SIZE)
                .and_then(|bytes| bytes.checked_sub((chunk_offset % BLOCK_SIZE_U64) as usize))
                .ok_or(Error::TransactionTooLarge)?;
            let chunk = (input.len() - committed).min(first_capacity);
            self.start_mutation()?;
            let result = self.write_chunk(node, chunk_offset, &input[committed..committed + chunk]);
            match result {
                Ok(count) => committed += count,
                Err(error) => {
                    self.transaction = None;
                    return if committed == 0 {
                        Err(error)
                    } else {
                        Ok(committed)
                    };
                }
            }
        }
        Ok(committed)
    }

    fn write_chunk(&mut self, node: NodeId, offset: u64, input: &[u8]) -> Result<usize, Error> {
        let mut inode = self.read_allocated_inode(node)?;
        if inode.kind == NodeKind::Directory {
            return Err(Error::IsDirectory);
        }
        if inode.kind != NodeKind::Regular {
            return Err(Error::UnsupportedNodeKind);
        }
        let end = offset
            .checked_add(input.len() as u64)
            .ok_or(Error::ArithmeticOverflow)?;
        let first = offset / BLOCK_SIZE_U64;
        let last = (end - 1) / BLOCK_SIZE_U64;
        let mut consumed = 0usize;
        for logical in first..=last {
            let within = if logical == first {
                (offset % BLOCK_SIZE_U64) as usize
            } else {
                0
            };
            let count = (BLOCK_SIZE - within).min(input.len() - consumed);
            let mut image = [0; BLOCK_SIZE];
            let physical = if let Some(physical) = physical_block(&inode, logical)? {
                self.read_block(physical, &mut image)?;
                physical
            } else {
                let physical = self.allocate_data_block()?;
                add_extent(&mut inode, logical, physical)?;
                physical
            };
            image[within..within + count].copy_from_slice(&input[consumed..consumed + count]);
            self.stage_block(physical, &image)?;
            consumed += count;
        }
        inode.size = inode.size.max(end);
        self.write_inode(node, &inode)?;
        self.commit_transaction()?;
        Ok(consumed)
    }

    pub fn truncate(&mut self, node: NodeId, size: u64) -> Result<(), Error> {
        self.start_mutation()?;
        let result = (|| {
            let mut inode = self.read_allocated_inode(node)?;
            if inode.kind == NodeKind::Directory {
                return Err(Error::IsDirectory);
            }
            if inode.kind != NodeKind::Regular {
                return Err(Error::UnsupportedNodeKind);
            }
            if size < inode.size {
                let retained_blocks = size.div_ceil(BLOCK_SIZE_U64);
                let mut retained = [Extent::default(); INLINE_EXTENT_COUNT];
                let mut retained_count = 0usize;
                let extents = inode.extents;
                for extent in &extents[..inode.extent_count as usize] {
                    for relative in 0..u64::from(extent.length_blocks) {
                        let logical = extent.logical_first_block + relative;
                        let physical = extent.physical_first_block + relative;
                        if logical < retained_blocks {
                            add_extent_raw(&mut retained, &mut retained_count, logical, physical)?;
                        } else {
                            self.free_data_block(physical)?;
                        }
                    }
                }
                inode.extents = retained;
                inode.extent_count = retained_count as u16;
                inode.allocated_blocks = retained[..retained_count]
                    .iter()
                    .map(|extent| u64::from(extent.length_blocks))
                    .sum();
                if !size.is_multiple_of(BLOCK_SIZE_U64) {
                    let logical = size / BLOCK_SIZE_U64;
                    if let Some(physical) = physical_block(&inode, logical)? {
                        let mut image = [0; BLOCK_SIZE];
                        self.read_block(physical, &mut image)?;
                        image[(size % BLOCK_SIZE_U64) as usize..].fill(0);
                        self.stage_block(physical, &image)?;
                    }
                }
            }
            inode.size = size;
            self.write_inode(node, &inode)?;
            self.commit_transaction()
        })();
        self.finish_mutation(result)
    }

    pub fn unlink(&mut self, parent: NodeId, name: &[u8]) -> Result<(), Error> {
        self.remove(parent, name, false)
    }

    pub fn rmdir(&mut self, parent: NodeId, name: &[u8]) -> Result<(), Error> {
        self.remove(parent, name, true)
    }

    fn remove(&mut self, parent: NodeId, name: &[u8], directory: bool) -> Result<(), Error> {
        self.start_mutation()?;
        let result = (|| {
            let name = valid_name(name)?;
            let mut parent_inode = self.read_allocated_inode(parent)?;
            let (logical, slot, entry) = self
                .find_entry(parent, &parent_inode, name)?
                .ok_or(Error::NotFound)?;
            let inode = self.read_allocated_inode(NodeId(entry.inode))?;
            if directory {
                if inode.kind != NodeKind::Directory {
                    return Err(Error::NotDirectory);
                }
                if inode.directory_entry_count != 0 {
                    return Err(Error::DirectoryNotEmpty);
                }
            } else if inode.kind == NodeKind::Directory {
                return Err(Error::IsDirectory);
            }
            self.clear_entry(parent, &mut parent_inode, logical, slot)?;
            let node = NodeId(entry.inode);
            let open = self.open_handles.iter().any(|record| {
                record.handle.node == node && record.handle.generation == inode.generation
            });
            if open && inode.kind == NodeKind::Regular {
                let mut orphan = inode.clone();
                orphan.link_count = 0;
                orphan.parent_inode = self
                    .transaction
                    .as_ref()
                    .ok_or(Error::CorruptVolume)?
                    .state
                    .orphan_head;
                self.write_inode(node, &orphan)?;
                self.transaction
                    .as_mut()
                    .ok_or(Error::CorruptVolume)?
                    .state
                    .orphan_head = node.0;
            } else {
                self.free_inode_and_blocks(node, &inode)?;
            }
            self.write_inode(parent, &parent_inode)?;
            self.commit_transaction()
        })();
        self.finish_mutation(result)
    }

    #[allow(clippy::collapsible_if)]
    pub fn rename(
        &mut self,
        old_parent: NodeId,
        old_name: &[u8],
        new_parent: NodeId,
        new_name: &[u8],
    ) -> Result<(), Error> {
        self.start_mutation()?;
        let result = (|| {
            let old_name = valid_name(old_name)?;
            let new_name = valid_name(new_name)?;
            let mut old_dir = self.read_allocated_inode(old_parent)?;
            let (old_logical, old_slot, source_entry) = self
                .find_entry(old_parent, &old_dir, old_name)?
                .ok_or(Error::NotFound)?;
            let source_node = NodeId(source_entry.inode);
            let mut source = self.read_allocated_inode(source_node)?;
            if source.kind == NodeKind::Directory {
                self.prevent_cycle(source_node, new_parent)?;
            }
            if old_parent == new_parent && old_name == new_name {
                self.transaction = None;
                return Ok(());
            }
            if old_parent == new_parent {
                if let Some((logical, slot, destination)) =
                    self.find_entry(old_parent, &old_dir, new_name)?
                {
                    if destination.inode == source_node.0 {
                        return Err(Error::AlreadyExists);
                    }
                    if destination.inode != source_node.0 {
                        let target = self.read_allocated_inode(NodeId(destination.inode))?;
                        if source.kind == NodeKind::Directory && target.kind != NodeKind::Directory
                        {
                            return Err(Error::NotDirectory);
                        }
                        if source.kind != NodeKind::Directory && target.kind == NodeKind::Directory
                        {
                            return Err(Error::IsDirectory);
                        }
                        if target.kind == NodeKind::Directory && target.directory_entry_count != 0 {
                            return Err(Error::DirectoryNotEmpty);
                        }
                        self.clear_entry(old_parent, &mut old_dir, logical, slot)?;
                        self.free_inode_and_blocks(NodeId(destination.inode), &target)?;
                    }
                }
                self.clear_entry(old_parent, &mut old_dir, old_logical, old_slot)?;
                self.insert_entry(old_parent, &mut old_dir, new_name, source_node, &source)?;
                self.write_inode(old_parent, &old_dir)?;
                self.commit_transaction()?;
                return Ok(());
            }
            let mut new_dir = self.read_allocated_inode(new_parent)?;
            if new_dir.kind != NodeKind::Directory {
                return Err(Error::NotDirectory);
            }
            if let Some((logical, slot, destination)) =
                self.find_entry(new_parent, &new_dir, new_name)?
            {
                if destination.inode == source_node.0 {
                    return Err(Error::AlreadyExists);
                } else {
                    let target = self.read_allocated_inode(NodeId(destination.inode))?;
                    if source.kind == NodeKind::Directory && target.kind != NodeKind::Directory {
                        return Err(Error::NotDirectory);
                    }
                    if source.kind != NodeKind::Directory && target.kind == NodeKind::Directory {
                        return Err(Error::IsDirectory);
                    }
                    if target.kind == NodeKind::Directory && target.directory_entry_count != 0 {
                        return Err(Error::DirectoryNotEmpty);
                    }
                    self.clear_entry(new_parent, &mut new_dir, logical, slot)?;
                    self.free_inode_and_blocks(NodeId(destination.inode), &target)?;
                }
            }
            self.clear_entry(old_parent, &mut old_dir, old_logical, old_slot)?;
            self.insert_entry(new_parent, &mut new_dir, new_name, source_node, &source)?;
            if source.kind == NodeKind::Directory && old_parent != new_parent {
                source.parent_inode = new_parent.0;
                self.write_inode(source_node, &source)?;
            }
            self.write_inode(old_parent, &old_dir)?;
            self.write_inode(new_parent, &new_dir)?;
            self.commit_transaction()
        })();
        self.finish_mutation(result)
    }

    fn start_mutation(&mut self) -> Result<(), Error> {
        self.ensure_writable()?;
        if self.transaction.is_some() {
            return Err(Error::TransactionInProgress);
        }
        self.begin_transaction()
    }

    fn finish_mutation<T>(&mut self, result: Result<T, Error>) -> Result<T, Error> {
        if result.is_err() {
            self.transaction = None;
        }
        result
    }

    fn next_generation_value(&self) -> Result<u64, Error> {
        self.state
            .ok_or(Error::CorruptVolume)?
            .generation
            .checked_add(1)
            .ok_or(Error::ArithmeticOverflow)
    }

    fn allocate_inode(&mut self) -> Result<NodeId, Error> {
        for (group_index, group) in self.groups.clone().iter().enumerate() {
            let mut bitmap = [0; BLOCK_SIZE];
            self.read_block(group.inode_bitmap_block, &mut bitmap)?;
            for local in 0..group.inodes_in_group as usize {
                if bitmap_test(&bitmap, local) == Some(false) {
                    bitmap_set(&mut bitmap, local, true)?;
                    self.stage_block(group.inode_bitmap_block, &bitmap)?;
                    let state = &mut self.transaction.as_mut().ok_or(Error::CorruptVolume)?.state;
                    state.free_inode_count = state
                        .free_inode_count
                        .checked_sub(1)
                        .ok_or(Error::CorruptVolume)?;
                    return Ok(NodeId(
                        (group_index * group.inodes_in_group as usize + local + 1) as u64,
                    ));
                }
            }
        }
        Err(Error::NoSpace)
    }

    fn allocate_data_block(&mut self) -> Result<u64, Error> {
        for group in self.groups.clone() {
            let mut bitmap = [0; BLOCK_SIZE];
            self.read_block(group.block_bitmap_block, &mut bitmap)?;
            for physical in group.first_data_block..group.data_end_block {
                let bit = (physical - group.group_start_block) as usize;
                if bitmap_test(&bitmap, bit) == Some(false) {
                    bitmap_set(&mut bitmap, bit, true)?;
                    self.stage_block(group.block_bitmap_block, &bitmap)?;
                    let state = &mut self.transaction.as_mut().ok_or(Error::CorruptVolume)?.state;
                    state.free_block_count = state
                        .free_block_count
                        .checked_sub(1)
                        .ok_or(Error::CorruptVolume)?;
                    return Ok(physical);
                }
            }
        }
        Err(Error::NoSpace)
    }

    fn free_data_block(&mut self, physical: u64) -> Result<(), Error> {
        let group = self
            .groups
            .iter()
            .find(|group| physical >= group.first_data_block && physical < group.data_end_block)
            .copied()
            .ok_or(Error::CorruptVolume)?;
        let mut bitmap = [0; BLOCK_SIZE];
        self.read_block(group.block_bitmap_block, &mut bitmap)?;
        bitmap_set(
            &mut bitmap,
            (physical - group.group_start_block) as usize,
            false,
        )?;
        self.stage_block(group.block_bitmap_block, &bitmap)?;
        let state = &mut self.transaction.as_mut().ok_or(Error::CorruptVolume)?.state;
        state.free_block_count = state
            .free_block_count
            .checked_add(1)
            .ok_or(Error::ArithmeticOverflow)?;
        Ok(())
    }

    fn write_inode(&mut self, node: NodeId, inode: &Inode) -> Result<(), Error> {
        let (group, local) = self.inode_location(node)?;
        let byte = local * INODE_BYTES;
        let physical = group.inode_table_first_block + (byte / BLOCK_SIZE) as u64;
        let within = byte % BLOCK_SIZE;
        let mut block = [0; BLOCK_SIZE];
        self.read_block(physical, &mut block)?;
        let encoded = if inode.link_count == 0 && inode.kind != NodeKind::Free {
            inode.encode_with_mode(InodeValidationMode::Phase3Orphan)?
        } else {
            inode.encode()?
        };
        block[within..within + INODE_BYTES].copy_from_slice(&encoded);
        self.stage_block(physical, &block)
    }

    fn find_entry(
        &mut self,
        directory: NodeId,
        inode: &Inode,
        name: &str,
    ) -> Result<Option<(u64, usize, DirectoryEntry)>, Error> {
        if inode.kind != NodeKind::Directory {
            return Err(Error::NotDirectory);
        }
        for logical in 0..inode.size / BLOCK_SIZE_U64 {
            let block = self.read_directory_block(directory, inode, logical)?;
            for (slot, entry) in block.entries.iter().enumerate() {
                if !entry.is_unused() && entry.name() == name {
                    return Ok(Some((logical, slot, *entry)));
                }
            }
        }
        Ok(None)
    }

    fn insert_entry(
        &mut self,
        directory: NodeId,
        inode: &mut Inode,
        name: &str,
        child: NodeId,
        child_inode: &Inode,
    ) -> Result<(), Error> {
        let entry = DirectoryEntry::new(child.0, child_inode.generation, child_inode.kind, name)?;
        for logical in 0..inode.size / BLOCK_SIZE_U64 {
            let mut block = self.read_directory_block(directory, inode, logical)?;
            if let Some(slot) = block.entries.iter().position(DirectoryEntry::is_unused) {
                block.entries[slot] = entry;
                let physical = physical_block(inode, logical)?.ok_or(Error::CorruptVolume)?;
                self.stage_block(physical, &block.encode()?)?;
                inode.directory_entry_count += 1;
                return Ok(());
            }
        }
        let logical = inode.size / BLOCK_SIZE_U64;
        let physical = self.allocate_data_block()?;
        add_extent(inode, logical, physical)?;
        inode.size += BLOCK_SIZE_U64;
        let mut block = DirectoryBlock::new(directory.0, logical)?;
        block.entries[0] = entry;
        self.stage_block(physical, &block.encode()?)?;
        inode.directory_entry_count += 1;
        Ok(())
    }

    fn clear_entry(
        &mut self,
        directory: NodeId,
        inode: &mut Inode,
        logical: u64,
        slot: usize,
    ) -> Result<(), Error> {
        let mut block = self.read_directory_block(directory, inode, logical)?;
        block.entries[slot] = DirectoryEntry::default();
        let physical = physical_block(inode, logical)?.ok_or(Error::CorruptVolume)?;
        self.stage_block(physical, &block.encode()?)?;
        inode.directory_entry_count = inode
            .directory_entry_count
            .checked_sub(1)
            .ok_or(Error::CorruptVolume)?;
        Ok(())
    }

    fn free_inode_and_blocks(&mut self, node: NodeId, inode: &Inode) -> Result<(), Error> {
        for extent in &inode.extents[..inode.extent_count as usize] {
            for physical in extent.physical_first_block..extent.physical_end()? {
                self.free_data_block(physical)?;
            }
        }
        self.write_inode(node, &Inode::default())?;
        let (group, local) = self.inode_location(node)?;
        let mut bitmap = [0; BLOCK_SIZE];
        self.read_block(group.inode_bitmap_block, &mut bitmap)?;
        bitmap_set(&mut bitmap, local, false)?;
        self.stage_block(group.inode_bitmap_block, &bitmap)?;
        let state = &mut self.transaction.as_mut().ok_or(Error::CorruptVolume)?.state;
        state.free_inode_count = state
            .free_inode_count
            .checked_add(1)
            .ok_or(Error::ArithmeticOverflow)?;
        Ok(())
    }

    pub(crate) fn reclaim_orphans(&mut self) -> Result<(), Error> {
        let mut seen = Vec::new();
        let mut cursor = self.state.ok_or(Error::CorruptVolume)?.orphan_head;
        while cursor != 0 {
            if seen.contains(&cursor) {
                return Err(Error::CorruptVolume);
            }
            seen.push(cursor);
            let inode = self.read_allocated_inode(NodeId(cursor))?;
            inode.validate_with_mode(InodeValidationMode::Phase3Orphan)?;
            cursor = inode.orphan_next()?;
        }
        for node in seen {
            let inode = self.read_allocated_inode(NodeId(node))?;
            self.reclaim_one_orphan(NodeId(node), &inode)?;
        }
        Ok(())
    }

    fn reclaim_one_orphan(&mut self, node: NodeId, inode: &Inode) -> Result<(), Error> {
        self.start_mutation()?;
        let result = (|| {
            let next = inode.orphan_next()?;
            let head = self
                .transaction
                .as_ref()
                .ok_or(Error::CorruptVolume)?
                .state
                .orphan_head;
            if head == node.0 {
                self.transaction
                    .as_mut()
                    .ok_or(Error::CorruptVolume)?
                    .state
                    .orphan_head = next;
            } else {
                let mut cursor = head;
                loop {
                    if cursor == 0 {
                        return Err(Error::CorruptVolume);
                    }
                    let mut prior = self.read_allocated_inode(NodeId(cursor))?;
                    let prior_next = prior.orphan_next()?;
                    if prior_next == node.0 {
                        prior.parent_inode = next;
                        self.write_inode(NodeId(cursor), &prior)?;
                        break;
                    }
                    cursor = prior_next;
                }
            }
            self.free_inode_and_blocks(node, inode)?;
            self.commit_transaction()
        })();
        self.finish_mutation(result)
    }

    fn prevent_cycle(&mut self, source: NodeId, destination_parent: NodeId) -> Result<(), Error> {
        let mut cursor = destination_parent;
        for _ in 0..=self.groups.len() * nullfs_format::PHASE2_INODES_PER_GROUP as usize {
            if cursor == source {
                return Err(Error::DirectoryCycle);
            }
            if cursor == self.root() {
                return Ok(());
            }
            let inode = self.read_allocated_inode(cursor)?;
            if inode.kind != NodeKind::Directory {
                return Err(Error::NotDirectory);
            }
            cursor = NodeId(inode.parent_inode);
        }
        Err(Error::CorruptVolume)
    }
}

fn valid_name(bytes: &[u8]) -> Result<&str, Error> {
    let name = str::from_utf8(bytes).map_err(|_| Error::InvalidName)?;
    DirectoryEntry::new(1, 1, NodeKind::Regular, name).map_err(|_| Error::InvalidName)?;
    Ok(name)
}

fn add_extent(inode: &mut Inode, logical: u64, physical: u64) -> Result<(), Error> {
    let mut extents = inode.extents;
    let mut count = inode.extent_count as usize;
    add_extent_raw(&mut extents, &mut count, logical, physical)?;
    inode.extents = extents;
    inode.extent_count = count as u16;
    inode.allocated_blocks += 1;
    Ok(())
}

#[allow(clippy::collapsible_if)]
fn add_extent_raw(
    extents: &mut [Extent; INLINE_EXTENT_COUNT],
    count: &mut usize,
    logical: u64,
    physical: u64,
) -> Result<(), Error> {
    let mut pending = Vec::with_capacity(*count + 1);
    pending.extend_from_slice(&extents[..*count]);
    pending.push(Extent {
        logical_first_block: logical,
        physical_first_block: physical,
        length_blocks: 1,
        flags: 0,
    });
    pending.sort_unstable_by_key(|extent| extent.logical_first_block);
    let mut merged = [Extent::default(); INLINE_EXTENT_COUNT];
    let mut merged_count = 0usize;
    for extent in pending {
        if let Some(last) = merged_count
            .checked_sub(1)
            .and_then(|index| merged.get_mut(index))
        {
            if last.logical_end()? == extent.logical_first_block
                && last.physical_end()? == extent.physical_first_block
            {
                last.length_blocks = last
                    .length_blocks
                    .checked_add(extent.length_blocks)
                    .ok_or(Error::ArithmeticOverflow)?;
                continue;
            }
        }
        if merged_count == INLINE_EXTENT_COUNT {
            return Err(Error::ExtentLimit);
        }
        merged[merged_count] = extent;
        merged_count += 1;
    }
    *extents = merged;
    *count = merged_count;
    Ok(())
}
