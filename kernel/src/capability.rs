//! Bounded capability authority and resource-security model.
//!
//! This layer is independent of the current userspace ABI registry.  It makes
//! object type, rights attenuation, generation-checked handles, transfer, and
//! revocation explicit so individual kernel services can migrate to one common
//! authority model without weakening existing compatibility paths.

use alloc::vec::Vec;
use core::num::NonZeroU64;

use crate::{
    object::{ObjectId, ObjectType, Rights},
    process_model::ProcessId,
};

pub const MAX_CAPABILITY_PROCESSES: usize = 64;
pub const MAX_CAPABILITY_SLOTS: usize = 128;
pub const MAX_CAPABILITY_OBJECTS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CapabilityHandle {
    slot: u16,
    generation: u32,
}

impl CapabilityHandle {
    pub const fn from_raw(raw: u64) -> Option<Self> {
        let slot = (raw & 0xffff) as u16;
        let generation = (raw >> 16) as u32;
        if generation == 0 {
            None
        } else {
            Some(Self { slot, generation })
        }
    }

    pub const fn raw(self) -> u64 {
        (self.generation as u64) << 16 | self.slot as u64
    }

    pub const fn slot(self) -> usize {
        self.slot as usize
    }

    pub const fn generation(self) -> u32 {
        self.generation
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityMetadata {
    pub handle: CapabilityHandle,
    pub object: ObjectId,
    pub object_type: ObjectType,
    pub rights: Rights,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectSnapshot {
    pub object: ObjectId,
    pub object_type: ObjectType,
    pub revoked: bool,
    pub handle_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityError {
    ProcessLimit,
    ObjectLimit,
    HandleLimit,
    ObjectIdExhausted,
    InvalidProcess,
    InvalidHandle,
    StaleHandle,
    ObjectNotFound,
    Revoked,
    TypeMismatch,
    RightsMissing,
    RightsEscalation,
    InvalidRights,
}

#[derive(Clone, Copy)]
struct CapabilityEntry {
    object: ObjectId,
    object_type: ObjectType,
    rights: Rights,
}

struct HandleSlot {
    generation: u32,
    entry: Option<CapabilityEntry>,
}

struct ProcessCapabilities {
    process: ProcessId,
    slots: Vec<HandleSlot>,
}

struct CapabilityObject {
    object: ObjectId,
    object_type: ObjectType,
    revoked: bool,
}

/// Bounded registry of process authority over kernel objects.
pub struct CapabilityRegistry {
    next_object_id: u64,
    processes: Vec<ProcessCapabilities>,
    objects: Vec<CapabilityObject>,
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CapabilityRegistry {
    pub const fn new() -> Self {
        Self {
            next_object_id: 1,
            processes: Vec::new(),
            objects: Vec::new(),
        }
    }

    pub fn process_count(&self) -> usize {
        self.processes.len()
    }

    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    pub fn ensure_process(&mut self, process: ProcessId) -> Result<(), CapabilityError> {
        if self.process_index(process).is_some() {
            return Ok(());
        }
        if self.processes.len() >= MAX_CAPABILITY_PROCESSES {
            return Err(CapabilityError::ProcessLimit);
        }
        self.processes.push(ProcessCapabilities {
            process,
            slots: Vec::new(),
        });
        Ok(())
    }

    pub fn remove_process(&mut self, process: ProcessId) -> Result<(), CapabilityError> {
        let index = self
            .process_index(process)
            .ok_or(CapabilityError::InvalidProcess)?;
        self.processes.swap_remove(index);
        self.collect_revoked_objects();
        Ok(())
    }

    pub fn create_object(
        &mut self,
        process: ProcessId,
        object_type: ObjectType,
        rights: Rights,
    ) -> Result<CapabilityMetadata, CapabilityError> {
        self.validate_rights(object_type, rights)?;
        self.ensure_process(process)?;
        if self.objects.len() >= MAX_CAPABILITY_OBJECTS {
            return Err(CapabilityError::ObjectLimit);
        }
        let object = ObjectId::new(
            NonZeroU64::new(self.next_object_id).ok_or(CapabilityError::ObjectIdExhausted)?,
        );
        self.next_object_id = self
            .next_object_id
            .checked_add(1)
            .ok_or(CapabilityError::ObjectIdExhausted)?;
        self.objects.push(CapabilityObject {
            object,
            object_type,
            revoked: false,
        });
        match self.install(process, object, object_type, rights) {
            Ok(handle) => Ok(handle),
            Err(error) => {
                self.objects.pop();
                Err(error)
            }
        }
    }

    pub fn duplicate(
        &mut self,
        process: ProcessId,
        source: CapabilityHandle,
        requested: Rights,
    ) -> Result<CapabilityMetadata, CapabilityError> {
        let source_metadata = self.lookup(process, source)?;
        if !source_metadata.rights.contains(Rights::DUPLICATE) {
            return Err(CapabilityError::RightsMissing);
        }
        let rights = source_metadata
            .rights
            .reduce_to(requested)
            .map_err(|_| CapabilityError::RightsEscalation)?;
        self.install(
            process,
            source_metadata.object,
            source_metadata.object_type,
            rights,
        )
    }

    /// Move an attenuated capability to another process atomically with
    /// respect to target capacity: a failed install leaves the source intact.
    pub fn transfer(
        &mut self,
        source_process: ProcessId,
        source: CapabilityHandle,
        target_process: ProcessId,
        requested: Rights,
    ) -> Result<CapabilityMetadata, CapabilityError> {
        let source_metadata = self.lookup(source_process, source)?;
        if !source_metadata.rights.contains(Rights::TRANSFER) {
            return Err(CapabilityError::RightsMissing);
        }
        let rights = source_metadata
            .rights
            .reduce_to(requested)
            .map_err(|_| CapabilityError::RightsEscalation)?;
        let transferred = self.install(
            target_process,
            source_metadata.object,
            source_metadata.object_type,
            rights,
        )?;
        self.close(source_process, source)?;
        Ok(transferred)
    }

    pub fn reduce(
        &mut self,
        process: ProcessId,
        handle: CapabilityHandle,
        requested: Rights,
    ) -> Result<CapabilityMetadata, CapabilityError> {
        let index = self.validate_handle(process, handle)?;
        let entry = self.processes[index.0].slots[index.1]
            .entry
            .as_mut()
            .ok_or(CapabilityError::InvalidHandle)?;
        entry.rights = entry
            .rights
            .reduce_to(requested)
            .map_err(|_| CapabilityError::RightsEscalation)?;
        Ok(CapabilityMetadata {
            handle,
            object: entry.object,
            object_type: entry.object_type,
            rights: entry.rights,
        })
    }

    pub fn lookup(
        &self,
        process: ProcessId,
        handle: CapabilityHandle,
    ) -> Result<CapabilityMetadata, CapabilityError> {
        let (process_index, slot_index) = self.validate_handle(process, handle)?;
        let entry = self.processes[process_index].slots[slot_index]
            .entry
            .ok_or(CapabilityError::InvalidHandle)?;
        let object = self.object(entry.object)?;
        if object.revoked {
            return Err(CapabilityError::Revoked);
        }
        if object.object_type != entry.object_type {
            return Err(CapabilityError::TypeMismatch);
        }
        Ok(CapabilityMetadata {
            handle,
            object: entry.object,
            object_type: entry.object_type,
            rights: entry.rights,
        })
    }

    pub fn check(
        &self,
        process: ProcessId,
        handle: CapabilityHandle,
        expected_type: ObjectType,
        required: Rights,
    ) -> Result<CapabilityMetadata, CapabilityError> {
        let metadata = self.lookup(process, handle)?;
        if metadata.object_type != expected_type {
            return Err(CapabilityError::TypeMismatch);
        }
        if !metadata.rights.contains(required) {
            return Err(CapabilityError::RightsMissing);
        }
        Ok(metadata)
    }

    pub fn close(
        &mut self,
        process: ProcessId,
        handle: CapabilityHandle,
    ) -> Result<(), CapabilityError> {
        let (process_index, slot_index) = self.validate_handle(process, handle)?;
        let slot = &mut self.processes[process_index].slots[slot_index];
        if slot.entry.take().is_none() {
            return Err(CapabilityError::InvalidHandle);
        }
        slot.generation = slot.generation.checked_add(1).unwrap_or(0);
        self.collect_revoked_objects();
        Ok(())
    }

    pub fn revoke(&mut self, object: ObjectId) -> Result<(), CapabilityError> {
        let record = self.object_mut(object)?;
        record.revoked = true;
        Ok(())
    }

    pub fn object_snapshot(&self, object: ObjectId) -> Result<ObjectSnapshot, CapabilityError> {
        let record = self.object(object)?;
        Ok(ObjectSnapshot {
            object,
            object_type: record.object_type,
            revoked: record.revoked,
            handle_count: self
                .processes
                .iter()
                .flat_map(|process| process.slots.iter())
                .filter(|slot| slot.entry.is_some_and(|entry| entry.object == object))
                .count(),
        })
    }

    pub const fn allowed_rights(object_type: ObjectType) -> Rights {
        match object_type {
            ObjectType::Process | ObjectType::Thread => Rights::INSPECT
                .union(Rights::DUPLICATE)
                .union(Rights::TRANSFER)
                .union(Rights::WAIT)
                .union(Rights::SIGNAL)
                .union(Rights::MANAGE),
            ObjectType::AddressSpace => Rights::INSPECT
                .union(Rights::DUPLICATE)
                .union(Rights::TRANSFER)
                .union(Rights::MAP)
                .union(Rights::MANAGE),
            ObjectType::Job => Rights::BASIC.union(Rights::SIGNAL).union(Rights::MANAGE),
            ObjectType::Channel => Rights::BASIC
                .union(Rights::READ)
                .union(Rights::WRITE)
                .union(Rights::SIGNAL),
            ObjectType::Notification | ObjectType::Event => Rights::BASIC.union(Rights::SIGNAL),
            ObjectType::SharedMemory => Rights::BASIC
                .union(Rights::READ)
                .union(Rights::WRITE)
                .union(Rights::MAP),
            ObjectType::Timer => Rights::BASIC.union(Rights::SET_PROPERTY),
            ObjectType::EventPort => Rights::BASIC
                .union(Rights::SIGNAL)
                .union(Rights::SET_PROPERTY),
            ObjectType::Device => Rights::BASIC
                .union(Rights::READ)
                .union(Rights::WRITE)
                .union(Rights::MAP)
                .union(Rights::MANAGE),
        }
    }

    fn install(
        &mut self,
        process: ProcessId,
        object: ObjectId,
        object_type: ObjectType,
        rights: Rights,
    ) -> Result<CapabilityMetadata, CapabilityError> {
        self.validate_rights(object_type, rights)?;
        let object_record = self.object(object)?;
        if object_record.revoked {
            return Err(CapabilityError::Revoked);
        }
        if object_record.object_type != object_type {
            return Err(CapabilityError::TypeMismatch);
        }
        self.ensure_process(process)?;
        let process_index = self
            .process_index(process)
            .ok_or(CapabilityError::InvalidProcess)?;
        let table = &mut self.processes[process_index];
        let (slot_index, generation) = if let Some((index, slot)) = table
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.entry.is_none() && slot.generation != 0)
        {
            (index, slot.generation)
        } else {
            if table.slots.len() >= MAX_CAPABILITY_SLOTS {
                return Err(CapabilityError::HandleLimit);
            }
            table.slots.push(HandleSlot {
                generation: 1,
                entry: None,
            });
            (table.slots.len() - 1, 1)
        };
        let slot = &mut table.slots[slot_index];
        slot.entry = Some(CapabilityEntry {
            object,
            object_type,
            rights,
        });
        let handle = CapabilityHandle {
            slot: slot_index as u16,
            generation,
        };
        Ok(CapabilityMetadata {
            handle,
            object,
            object_type,
            rights,
        })
    }

    fn validate_rights(
        &self,
        object_type: ObjectType,
        rights: Rights,
    ) -> Result<(), CapabilityError> {
        if rights == Rights::NONE || !Self::allowed_rights(object_type).contains(rights) {
            Err(CapabilityError::InvalidRights)
        } else {
            Ok(())
        }
    }

    fn validate_handle(
        &self,
        process: ProcessId,
        handle: CapabilityHandle,
    ) -> Result<(usize, usize), CapabilityError> {
        let process_index = self
            .process_index(process)
            .ok_or(CapabilityError::InvalidProcess)?;
        let slot = self.processes[process_index]
            .slots
            .get(handle.slot())
            .ok_or(CapabilityError::InvalidHandle)?;
        if slot.generation != handle.generation() {
            return Err(CapabilityError::StaleHandle);
        }
        Ok((process_index, handle.slot()))
    }

    fn process_index(&self, process: ProcessId) -> Option<usize> {
        self.processes
            .iter()
            .position(|candidate| candidate.process == process)
    }

    fn object(&self, object: ObjectId) -> Result<&CapabilityObject, CapabilityError> {
        self.objects
            .iter()
            .find(|candidate| candidate.object == object)
            .ok_or(CapabilityError::ObjectNotFound)
    }

    fn object_mut(&mut self, object: ObjectId) -> Result<&mut CapabilityObject, CapabilityError> {
        self.objects
            .iter_mut()
            .find(|candidate| candidate.object == object)
            .ok_or(CapabilityError::ObjectNotFound)
    }

    fn collect_revoked_objects(&mut self) {
        self.objects.retain(|object| {
            self.processes
                .iter()
                .flat_map(|process| process.slots.iter())
                .any(|slot| {
                    slot.entry
                        .is_some_and(|entry| entry.object == object.object)
                })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(raw: u64) -> ProcessId {
        ProcessId::from_raw(raw).unwrap()
    }

    fn channel_rights() -> Rights {
        Rights::BASIC.union(Rights::READ).union(Rights::WRITE)
    }

    #[test]
    fn create_lookup_and_type_rights_are_authoritative() {
        let mut registry = CapabilityRegistry::new();
        let metadata = registry
            .create_object(process(1), ObjectType::Channel, channel_rights())
            .unwrap();
        assert_eq!(registry.process_count(), 1);
        assert_eq!(registry.object_count(), 1);
        assert_eq!(registry.lookup(process(1), metadata.handle), Ok(metadata));
        assert_eq!(
            registry.check(
                process(1),
                metadata.handle,
                ObjectType::Notification,
                Rights::WAIT
            ),
            Err(CapabilityError::TypeMismatch)
        );
        assert_eq!(
            registry.check(
                process(1),
                metadata.handle,
                ObjectType::Channel,
                Rights::MANAGE
            ),
            Err(CapabilityError::RightsMissing)
        );
    }

    #[test]
    fn duplicate_and_reduce_only_attenuate_authority() {
        let mut registry = CapabilityRegistry::new();
        let original = registry
            .create_object(
                process(1),
                ObjectType::SharedMemory,
                channel_rights().union(Rights::MAP),
            )
            .unwrap();
        let reduced = registry
            .duplicate(process(1), original.handle, Rights::READ)
            .unwrap();
        assert!(!reduced.rights.contains(Rights::WRITE));
        assert_eq!(
            registry.duplicate(process(1), reduced.handle, Rights::WRITE),
            Err(CapabilityError::RightsMissing)
        );
        assert_eq!(
            registry.reduce(process(1), reduced.handle, Rights::WRITE),
            Err(CapabilityError::RightsEscalation)
        );
    }

    #[test]
    fn transfer_moves_an_attenuated_capability_and_preserves_source_on_failure() {
        let mut registry = CapabilityRegistry::new();
        let source = registry
            .create_object(process(1), ObjectType::Channel, channel_rights())
            .unwrap();
        let transferred = registry
            .transfer(
                process(1),
                source.handle,
                process(2),
                Rights::BASIC.union(Rights::READ),
            )
            .unwrap();
        assert_eq!(
            registry.lookup(process(1), source.handle),
            Err(CapabilityError::StaleHandle)
        );
        assert_eq!(
            registry.lookup(process(2), transferred.handle),
            Ok(transferred)
        );
        assert_eq!(
            registry.transfer(
                process(2),
                transferred.handle,
                process(2),
                Rights::BASIC.union(Rights::READ).union(Rights::WRITE),
            ),
            Err(CapabilityError::RightsEscalation)
        );
    }

    #[test]
    fn close_reuses_slot_with_a_new_generation_and_revocation_invalidates_all_handles() {
        let mut registry = CapabilityRegistry::new();
        let first = registry
            .create_object(process(1), ObjectType::Notification, Rights::BASIC)
            .unwrap();
        registry.close(process(1), first.handle).unwrap();
        let second = registry
            .create_object(process(1), ObjectType::Event, Rights::BASIC)
            .unwrap();
        assert_eq!(first.handle.slot(), second.handle.slot());
        assert_ne!(first.handle.generation(), second.handle.generation());
        assert_eq!(
            registry.lookup(process(1), first.handle),
            Err(CapabilityError::StaleHandle)
        );

        let duplicate = registry
            .duplicate(process(1), second.handle, Rights::BASIC)
            .unwrap();
        registry.revoke(second.object).unwrap();
        assert_eq!(
            registry.lookup(process(1), second.handle),
            Err(CapabilityError::Revoked)
        );
        assert_eq!(
            registry.lookup(process(1), duplicate.handle),
            Err(CapabilityError::Revoked)
        );
        registry.close(process(1), second.handle).unwrap();
        registry.close(process(1), duplicate.handle).unwrap();
        assert_eq!(registry.object_count(), 0);
    }

    #[test]
    fn exhausted_generation_is_never_reused() {
        let mut registry = CapabilityRegistry::new();
        let original = registry
            .create_object(process(1), ObjectType::Channel, channel_rights())
            .unwrap();
        let process_index = registry.process_index(process(1)).unwrap();
        let slot_index = original.handle.slot();
        registry.processes[process_index].slots[slot_index].generation = u32::MAX;
        let maximal = CapabilityHandle {
            slot: slot_index as u16,
            generation: u32::MAX,
        };

        registry.close(process(1), maximal).unwrap();
        assert_eq!(
            registry.processes[process_index].slots[slot_index].generation,
            0
        );
        assert_eq!(
            registry.lookup(process(1), maximal),
            Err(CapabilityError::StaleHandle)
        );

        let replacement = registry
            .create_object(process(1), ObjectType::Event, Rights::BASIC)
            .unwrap();
        assert_ne!(replacement.handle.slot(), slot_index);
    }

    #[test]
    fn invalid_rights_and_object_limits_are_rejected() {
        let mut registry = CapabilityRegistry::new();
        assert_eq!(
            registry.create_object(process(1), ObjectType::Timer, Rights::WRITE),
            Err(CapabilityError::InvalidRights)
        );
        let handle = CapabilityHandle::from_raw(1_u64 << 16).unwrap();
        assert_eq!(
            registry.lookup(process(1), handle),
            Err(CapabilityError::InvalidProcess)
        );
    }
}
