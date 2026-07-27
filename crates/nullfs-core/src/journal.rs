use alloc::vec::Vec;

use nullfs_blockdev::BlockDevice;
use nullfs_format::{
    BLOCK_SIZE, FilesystemState, JournalControl, JournalState, JournalTag, PHASE3_MAX_UPDATES,
    next_generation, next_transaction_id,
};

use crate::{Error, Filesystem, RuntimeMountMode};

#[derive(Clone)]
struct StagedBlock {
    target: u64,
    image: [u8; BLOCK_SIZE],
}

/// A bounded collection of complete, unique home-block after-images.
///
/// Transactions are currently exposed only inside `nullfs-core`; namespace mutation APIs will
/// build on this primitive in a later phase.
pub(crate) struct Transaction {
    id: u64,
    pub(crate) state: FilesystemState,
    updates: Vec<StagedBlock>,
}

impl Transaction {
    #[allow(dead_code)]
    pub(crate) const fn new(id: u64, state: FilesystemState) -> Self {
        Self {
            id,
            state,
            updates: Vec::new(),
        }
    }

    pub(crate) fn stage(&mut self, target: u64, image: &[u8; BLOCK_SIZE]) -> Result<(), Error> {
        if let Some(existing) = self
            .updates
            .iter_mut()
            .find(|update| update.target == target)
        {
            existing.image = *image;
            return Ok(());
        }
        // One journal entry is reserved for the coherent filesystem-state update.
        if self.updates.len() >= PHASE3_MAX_UPDATES - 1 {
            return Err(Error::TransactionTooLarge);
        }
        self.updates.push(StagedBlock {
            target,
            image: *image,
        });
        Ok(())
    }

    pub(crate) fn staged(&self, target: u64) -> Option<&[u8; BLOCK_SIZE]> {
        self.updates
            .iter()
            .find(|update| update.target == target)
            .map(|update| &update.image)
    }
}

impl<D: BlockDevice> Filesystem<D> {
    #[allow(dead_code)]
    pub(crate) fn begin_transaction(&mut self) -> Result<(), Error> {
        self.ensure_writable()?;
        if self.transaction.is_none() {
            let state = self.state.ok_or(Error::CorruptVolume)?;
            self.transaction = Some(Transaction::new(state.next_transaction_id, state));
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn stage_block(
        &mut self,
        target: u64,
        image: &[u8; BLOCK_SIZE],
    ) -> Result<(), Error> {
        self.ensure_home_target(target)?;
        self.begin_transaction()?;
        self.transaction
            .as_mut()
            .ok_or(Error::CorruptVolume)?
            .stage(target, image)
    }

    pub(crate) fn commit_transaction(&mut self) -> Result<(), Error> {
        self.ensure_writable()?;
        let Some(mut transaction) = self.transaction.take() else {
            return Ok(());
        };
        let old_state = self.state.ok_or(Error::CorruptVolume)?;
        let mut new_state = transaction.state;
        new_state.generation = next_generation(old_state.generation)?;
        new_state.next_transaction_id = next_transaction_id(transaction.id)?;
        let state_image = new_state.encode()?;
        if transaction
            .staged(self.superblock.filesystem_state_block)
            .is_some()
        {
            return Err(Error::ProtectedBlock);
        }
        transaction.stage(self.superblock.filesystem_state_block, &state_image)?;

        let result = self.commit_updates(&transaction, new_state);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn commit_updates(
        &mut self,
        transaction: &Transaction,
        new_state: FilesystemState,
    ) -> Result<(), Error> {
        let mut tags = Vec::with_capacity(transaction.updates.len());
        for (index, update) in transaction.updates.iter().enumerate() {
            self.ensure_home_target_or_state(update.target)?;
            let tag = JournalTag::new(transaction.id, index as u32, update.target, &update.image)?;
            self.device.write_blocks(
                self.superblock.journal_first_block + 2 + index as u64,
                &tag.encode()?,
            )?;
            self.device.write_blocks(
                self.superblock.journal_first_block + 2 + PHASE3_MAX_UPDATES as u64 + index as u64,
                &update.image,
            )?;
            tags.push(tag);
        }
        self.device.flush()?;

        let committed_generation = next_generation(self.journal_generation)?;
        let committed = JournalControl::committed(committed_generation, transaction.id, &tags)?;
        let older_slot = self.older_control_slot;
        self.device.write_blocks(
            self.superblock.journal_first_block + u64::from(older_slot),
            &committed.encode()?,
        )?;
        self.device.flush()?;

        for update in &transaction.updates {
            self.device.write_blocks(update.target, &update.image)?;
        }
        self.device.flush()?;

        let empty_generation = next_generation(committed_generation)?;
        let empty = JournalControl::empty(empty_generation);
        let newer_slot = 1 - older_slot;
        self.device.write_blocks(
            self.superblock.journal_first_block + u64::from(newer_slot),
            &empty.encode()?,
        )?;
        self.device.flush()?;

        self.state = Some(new_state);
        self.journal_generation = empty_generation;
        self.older_control_slot = older_slot;
        Ok(())
    }

    pub(crate) fn recover_journal(&mut self) -> Result<(), Error> {
        let (control, slot) = self.select_control()?;
        self.journal_generation = control.generation;
        self.older_control_slot = 1 - slot;
        if control.state == JournalState::Empty {
            return Ok(());
        }

        let count = control.update_count as usize;
        let mut tags = Vec::with_capacity(count);
        let mut images = Vec::with_capacity(count);
        for index in 0..count {
            let mut bytes = [0; BLOCK_SIZE];
            self.device.read_blocks(
                self.superblock.journal_first_block + 2 + index as u64,
                &mut bytes,
            )?;
            let tag = JournalTag::decode(&bytes)?;
            self.ensure_home_target_or_state(tag.target_home_block)?;
            if tags
                .iter()
                .any(|prior: &JournalTag| prior.target_home_block == tag.target_home_block)
            {
                return Err(Error::CorruptJournal);
            }
            self.device.read_blocks(
                self.superblock.journal_first_block + 2 + PHASE3_MAX_UPDATES as u64 + index as u64,
                &mut bytes,
            )?;
            if !tag.image_matches(&bytes) {
                return Err(Error::CorruptJournal);
            }
            tags.push(tag);
            images.push(bytes);
        }
        control.validate_tags(&tags)?;
        let state_target = self.superblock.filesystem_state_block;
        let mut state_images = tags
            .iter()
            .zip(&images)
            .filter(|(tag, _)| tag.target_home_block == state_target);
        let (_, state_image) = state_images.next().ok_or(Error::CorruptJournal)?;
        if state_images.next().is_some() {
            return Err(Error::CorruptJournal);
        }
        let recovered_state = FilesystemState::decode(state_image)?;
        let mut old_state_image = [0; BLOCK_SIZE];
        self.device
            .read_blocks(state_target, &mut old_state_image)?;
        let old_state = FilesystemState::decode(&old_state_image)?;
        if recovered_state.next_transaction_id != next_transaction_id(control.transaction_id)?
            || recovered_state.generation < old_state.generation
        {
            return Err(Error::CorruptJournal);
        }
        if self.mount_mode == RuntimeMountMode::ReadOnly {
            self.recovery_overlay = tags
                .iter()
                .zip(&images)
                .map(|(tag, image)| (tag.target_home_block, *image))
                .collect();
            self.state = Some(recovered_state);
            return Ok(());
        }
        for (tag, image) in tags.iter().zip(&images) {
            self.device.write_blocks(tag.target_home_block, image)?;
        }
        self.device.flush()?;

        let empty_generation = next_generation(control.generation)?;
        let empty = JournalControl::empty(empty_generation);
        let publish_slot = 1 - slot;
        self.device.write_blocks(
            self.superblock.journal_first_block + u64::from(publish_slot),
            &empty.encode()?,
        )?;
        self.device.flush()?;
        self.journal_generation = empty_generation;
        self.older_control_slot = slot;
        Ok(())
    }

    fn select_control(&mut self) -> Result<(JournalControl, u8), Error> {
        let mut decoded = [None, None];
        for slot in 0..2_u8 {
            let mut bytes = [0; BLOCK_SIZE];
            self.device.read_blocks(
                self.superblock.journal_first_block + u64::from(slot),
                &mut bytes,
            )?;
            decoded[slot as usize] = JournalControl::decode(&bytes).ok();
        }
        match decoded {
            [None, None] => Err(Error::CorruptJournal),
            [Some(control), None] => Ok((control, 0)),
            [None, Some(control)] => Ok((control, 1)),
            [Some(left), Some(right)] => {
                if left.generation == right.generation && left != right {
                    Err(Error::CorruptJournal)
                } else if left.generation >= right.generation {
                    Ok((left, 0))
                } else {
                    Ok((right, 1))
                }
            }
        }
    }

    pub(crate) fn ensure_writable(&self) -> Result<(), Error> {
        if self.poisoned {
            Err(Error::Poisoned)
        } else if self.mount_mode != RuntimeMountMode::ReadWrite {
            Err(Error::ReadOnly)
        } else {
            Ok(())
        }
    }

    fn ensure_home_target(&self, target: u64) -> Result<(), Error> {
        if target < self.superblock.first_allocatable_block
            || target >= self.superblock.capacity_blocks
        {
            Err(Error::ProtectedBlock)
        } else {
            Ok(())
        }
    }

    fn ensure_home_target_or_state(&self, target: u64) -> Result<(), Error> {
        if target == self.superblock.filesystem_state_block {
            Ok(())
        } else {
            self.ensure_home_target(target)
        }
    }
}
