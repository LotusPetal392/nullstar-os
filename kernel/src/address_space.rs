//! Bounded ownership and mapping model for isolated virtual address spaces.
//!
//! The architecture-specific userspace loader owns page-table construction.
//! This module owns the invariants that loader, fork, and fault paths share:
//! page-aligned user ranges, unique mappings per address space, frame
//! references, permissions, and copy-on-write transitions.

use alloc::vec::Vec;

use crate::process_model::ProcessId;

pub const PAGE_SIZE: u64 = 4096;
pub const USER_VIRTUAL_START: u64 = PAGE_SIZE;
pub const USER_VIRTUAL_END: u64 = 0x0000_8000_0000_0000;
pub const MAX_ADDRESS_SPACES: usize = 64;
pub const MAX_MAPPINGS_PER_SPACE: usize = 512;
pub const MAX_PHYSICAL_FRAMES: usize = 2048;

const MAX_FRAME_NUMBER: u64 = u64::MAX / PAGE_SIZE;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AddressSpaceId(u64);

impl AddressSpaceId {
    pub const fn from_raw(raw: u64) -> Option<Self> {
        if raw == 0 { None } else { Some(Self(raw)) }
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct VirtualPage(u64);

impl VirtualPage {
    pub const fn from_address(address: u64) -> Option<Self> {
        if address < USER_VIRTUAL_START
            || address >= USER_VIRTUAL_END
            || !address.is_multiple_of(PAGE_SIZE)
        {
            return None;
        }
        Some(Self(address / PAGE_SIZE))
    }

    pub const fn from_raw(raw: u64) -> Option<Self> {
        if raw == 0 || raw >= USER_VIRTUAL_END / PAGE_SIZE {
            None
        } else {
            Some(Self(raw))
        }
    }

    pub const fn address(self) -> u64 {
        self.0 * PAGE_SIZE
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PhysicalFrame(u64);

impl PhysicalFrame {
    pub const fn from_address(address: u64) -> Option<Self> {
        if address == 0 || !address.is_multiple_of(PAGE_SIZE) {
            return None;
        }
        Self::from_raw(address / PAGE_SIZE)
    }

    pub const fn from_raw(raw: u64) -> Option<Self> {
        if raw == 0 || raw > MAX_FRAME_NUMBER {
            None
        } else {
            Some(Self(raw))
        }
    }

    pub const fn address(self) -> u64 {
        self.0 * PAGE_SIZE
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PagePermissions(u8);

impl PagePermissions {
    const READ: u8 = 1 << 0;
    const WRITE: u8 = 1 << 1;
    const EXECUTE: u8 = 1 << 2;

    pub const fn new(
        readable: bool,
        writable: bool,
        executable: bool,
    ) -> Result<Self, PermissionError> {
        if writable && executable {
            return Err(PermissionError::WritableExecutable);
        }
        if !readable && !writable && !executable {
            return Err(PermissionError::NoAccess);
        }
        let mut bits = 0;
        if readable {
            bits |= Self::READ;
        }
        if writable {
            bits |= Self::WRITE;
        }
        if executable {
            bits |= Self::EXECUTE;
        }
        Ok(Self(bits))
    }

    pub const fn read_only() -> Self {
        Self(Self::READ)
    }

    pub const fn writable() -> Self {
        Self(Self::READ | Self::WRITE)
    }

    pub const fn executable() -> Self {
        Self(Self::READ | Self::EXECUTE)
    }

    pub const fn readable(self) -> bool {
        self.0 & Self::READ != 0
    }

    pub const fn writable_flag(self) -> bool {
        self.0 & Self::WRITE != 0
    }

    pub const fn executable_flag(self) -> bool {
        self.0 & Self::EXECUTE != 0
    }

    const fn with_writable(self, writable: bool) -> Self {
        if writable {
            Self(self.0 | Self::WRITE)
        } else {
            Self(self.0 & !Self::WRITE)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermissionError {
    NoAccess,
    WritableExecutable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mapping {
    pub page: VirtualPage,
    pub frame: PhysicalFrame,
    pub permissions: PagePermissions,
    pub copy_on_write: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AddressSpaceSnapshot {
    pub id: AddressSpaceId,
    pub owner: ProcessId,
    pub generation: u64,
    pub mapping_count: usize,
    pub copy_on_write_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CowResolution {
    pub space: AddressSpaceId,
    pub page: VirtualPage,
    pub old_frame: PhysicalFrame,
    pub new_frame: PhysicalFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameSnapshot {
    pub frame: PhysicalFrame,
    pub references: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryError {
    AddressSpaceLimit,
    FrameLimit,
    MappingLimit,
    InvalidAddressSpace,
    InvalidVirtualAddress,
    InvalidPhysicalFrame,
    Permission(PermissionError),
    AlreadyMapped,
    NotMapped,
    FrameInUse,
    FrameConflict,
    CowRequired,
}

struct AddressSpace {
    id: AddressSpaceId,
    owner: ProcessId,
    generation: u64,
    mappings: Vec<Mapping>,
}

struct FrameRecord {
    frame: PhysicalFrame,
    references: usize,
}

/// Bounded address-space registry and physical-frame reference tracker.
pub struct AddressSpaceTable {
    next_space_id: u64,
    next_frame_number: u64,
    spaces: Vec<AddressSpace>,
    frames: Vec<FrameRecord>,
}

impl Default for AddressSpaceTable {
    fn default() -> Self {
        Self::new()
    }
}

impl AddressSpaceTable {
    pub const fn new() -> Self {
        Self {
            next_space_id: 1,
            next_frame_number: 1,
            spaces: Vec::new(),
            frames: Vec::new(),
        }
    }

    pub fn address_space_count(&self) -> usize {
        self.spaces.len()
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub fn create(&mut self, owner: ProcessId) -> Result<AddressSpaceId, MemoryError> {
        if self.spaces.len() >= MAX_ADDRESS_SPACES {
            return Err(MemoryError::AddressSpaceLimit);
        }
        let id =
            AddressSpaceId::from_raw(self.next_space_id).ok_or(MemoryError::AddressSpaceLimit)?;
        self.next_space_id = self
            .next_space_id
            .checked_add(1)
            .ok_or(MemoryError::AddressSpaceLimit)?;
        self.spaces.push(AddressSpace {
            id,
            owner,
            generation: 0,
            mappings: Vec::new(),
        });
        Ok(id)
    }

    pub fn destroy(&mut self, space: AddressSpaceId) -> Result<AddressSpaceSnapshot, MemoryError> {
        let index = self.space_index(space)?;
        let removed = self.spaces.swap_remove(index);
        for mapping in &removed.mappings {
            self.release_reference(mapping.frame);
        }
        Ok(Self::snapshot_of(&removed))
    }

    pub fn allocate_frame(&mut self) -> Result<PhysicalFrame, MemoryError> {
        if self.frames.len() >= MAX_PHYSICAL_FRAMES {
            return Err(MemoryError::FrameLimit);
        }
        let frame =
            PhysicalFrame::from_raw(self.next_frame_number).ok_or(MemoryError::FrameLimit)?;
        self.next_frame_number = self
            .next_frame_number
            .checked_add(1)
            .ok_or(MemoryError::FrameLimit)?;
        self.frames.push(FrameRecord {
            frame,
            references: 0,
        });
        Ok(frame)
    }

    pub fn release_frame(&mut self, frame: PhysicalFrame) -> Result<(), MemoryError> {
        let index = self.frame_index(frame)?;
        if self.frames[index].references != 0 {
            return Err(MemoryError::FrameInUse);
        }
        self.frames.swap_remove(index);
        Ok(())
    }

    pub fn frame_snapshot(&self, frame: PhysicalFrame) -> Result<FrameSnapshot, MemoryError> {
        let record = self
            .frames
            .iter()
            .find(|record| record.frame == frame)
            .ok_or(MemoryError::InvalidPhysicalFrame)?;
        Ok(FrameSnapshot {
            frame,
            references: record.references,
        })
    }

    pub fn map_new(
        &mut self,
        space: AddressSpaceId,
        page: VirtualPage,
        permissions: PagePermissions,
    ) -> Result<Mapping, MemoryError> {
        let frame = self.allocate_frame()?;
        match self.map(space, page, frame, permissions) {
            Ok(mapping) => Ok(mapping),
            Err(error) => {
                let _ = self.release_frame(frame);
                Err(error)
            }
        }
    }

    pub fn map(
        &mut self,
        space: AddressSpaceId,
        page: VirtualPage,
        frame: PhysicalFrame,
        permissions: PagePermissions,
    ) -> Result<Mapping, MemoryError> {
        self.validate_permissions(permissions)?;
        let space_index = self.space_index(space)?;
        if self.spaces[space_index]
            .mappings
            .iter()
            .any(|mapping| mapping.page == page)
        {
            return Err(MemoryError::AlreadyMapped);
        }
        if self.spaces[space_index].mappings.len() >= MAX_MAPPINGS_PER_SPACE {
            return Err(MemoryError::MappingLimit);
        }
        self.retain_frame(frame)?;
        let mapping = Mapping {
            page,
            frame,
            permissions,
            copy_on_write: false,
        };
        self.spaces[space_index].mappings.push(mapping);
        self.bump_generation(space_index);
        Ok(mapping)
    }

    pub fn unmap(
        &mut self,
        space: AddressSpaceId,
        page: VirtualPage,
    ) -> Result<Mapping, MemoryError> {
        let space_index = self.space_index(space)?;
        let mapping_index = self.mapping_index(space_index, page)?;
        let mapping = self.spaces[space_index].mappings.swap_remove(mapping_index);
        self.release_reference(mapping.frame);
        self.bump_generation(space_index);
        Ok(mapping)
    }

    pub fn protect(
        &mut self,
        space: AddressSpaceId,
        page: VirtualPage,
        permissions: PagePermissions,
    ) -> Result<Mapping, MemoryError> {
        self.validate_permissions(permissions)?;
        let space_index = self.space_index(space)?;
        let mapping_index = self.mapping_index(space_index, page)?;
        if self.spaces[space_index].mappings[mapping_index].copy_on_write
            && permissions.writable_flag()
        {
            return Err(MemoryError::CowRequired);
        }
        self.spaces[space_index].mappings[mapping_index].permissions = permissions;
        self.bump_generation(space_index);
        Ok(self.spaces[space_index].mappings[mapping_index])
    }

    pub fn mapping(
        &self,
        space: AddressSpaceId,
        page: VirtualPage,
    ) -> Result<Mapping, MemoryError> {
        let space_index = self.space_index(space)?;
        let mapping_index = self.mapping_index(space_index, page)?;
        Ok(self.spaces[space_index].mappings[mapping_index])
    }

    pub fn mappings(&self, space: AddressSpaceId) -> Result<Vec<Mapping>, MemoryError> {
        Ok(self.spaces[self.space_index(space)?].mappings.clone())
    }

    pub fn snapshot(&self, space: AddressSpaceId) -> Result<AddressSpaceSnapshot, MemoryError> {
        Ok(Self::snapshot_of(&self.spaces[self.space_index(space)?]))
    }

    /// Clone an address space with writable mappings converted to copy-on-write.
    pub fn clone_cow(
        &mut self,
        parent: AddressSpaceId,
        child_owner: ProcessId,
    ) -> Result<AddressSpaceId, MemoryError> {
        let parent_index = self.space_index(parent)?;
        let parent_mappings = self.spaces[parent_index].mappings.clone();
        let child = self.create(child_owner)?;
        let child_index = self.space_index(child)?;

        for mapping in &parent_mappings {
            self.retain_frame(mapping.frame)?;
            let mut child_mapping = *mapping;
            if mapping.permissions.writable_flag() {
                child_mapping.permissions = mapping.permissions.with_writable(false);
                child_mapping.copy_on_write = true;
            }
            self.spaces[child_index].mappings.push(child_mapping);
        }
        for mapping in &mut self.spaces[parent_index].mappings {
            if mapping.permissions.writable_flag() {
                mapping.permissions = mapping.permissions.with_writable(false);
                mapping.copy_on_write = true;
            }
        }
        self.bump_generation(parent_index);
        self.bump_generation(child_index);
        Ok(child)
    }

    pub fn resolve_cow_fault(
        &mut self,
        space: AddressSpaceId,
        page: VirtualPage,
        new_frame: PhysicalFrame,
    ) -> Result<CowResolution, MemoryError> {
        let space_index = self.space_index(space)?;
        let mapping_index = self.mapping_index(space_index, page)?;
        let old_frame = self.spaces[space_index].mappings[mapping_index].frame;
        if !self.spaces[space_index].mappings[mapping_index].copy_on_write {
            return Err(MemoryError::CowRequired);
        }
        if old_frame == new_frame {
            return Err(MemoryError::FrameConflict);
        }
        self.retain_frame(new_frame)?;
        self.release_reference(old_frame);
        let mapping = &mut self.spaces[space_index].mappings[mapping_index];
        mapping.frame = new_frame;
        mapping.permissions = mapping.permissions.with_writable(true);
        mapping.copy_on_write = false;
        self.bump_generation(space_index);
        Ok(CowResolution {
            space,
            page,
            old_frame,
            new_frame,
        })
    }

    fn validate_permissions(&self, permissions: PagePermissions) -> Result<(), MemoryError> {
        PagePermissions::new(
            permissions.readable(),
            permissions.writable_flag(),
            permissions.executable_flag(),
        )
        .map(|_| ())
        .map_err(MemoryError::Permission)
    }

    fn space_index(&self, space: AddressSpaceId) -> Result<usize, MemoryError> {
        self.spaces
            .iter()
            .position(|candidate| candidate.id == space)
            .ok_or(MemoryError::InvalidAddressSpace)
    }

    fn frame_index(&self, frame: PhysicalFrame) -> Result<usize, MemoryError> {
        self.frames
            .iter()
            .position(|record| record.frame == frame)
            .ok_or(MemoryError::InvalidPhysicalFrame)
    }

    fn mapping_index(&self, space_index: usize, page: VirtualPage) -> Result<usize, MemoryError> {
        self.spaces[space_index]
            .mappings
            .iter()
            .position(|mapping| mapping.page == page)
            .ok_or(MemoryError::NotMapped)
    }

    fn retain_frame(&mut self, frame: PhysicalFrame) -> Result<(), MemoryError> {
        if let Some(record) = self.frames.iter_mut().find(|record| record.frame == frame) {
            record.references = record.references.saturating_add(1);
            return Ok(());
        }
        if self.frames.len() >= MAX_PHYSICAL_FRAMES {
            return Err(MemoryError::FrameLimit);
        }
        self.frames.push(FrameRecord {
            frame,
            references: 1,
        });
        Ok(())
    }

    fn release_reference(&mut self, frame: PhysicalFrame) {
        let Some(index) = self.frames.iter().position(|record| record.frame == frame) else {
            return;
        };
        if self.frames[index].references > 1 {
            self.frames[index].references -= 1;
        } else {
            self.frames.swap_remove(index);
        }
    }

    fn bump_generation(&mut self, space_index: usize) {
        self.spaces[space_index].generation = self.spaces[space_index].generation.saturating_add(1);
    }

    fn snapshot_of(space: &AddressSpace) -> AddressSpaceSnapshot {
        AddressSpaceSnapshot {
            id: space.id,
            owner: space.owner,
            generation: space.generation,
            mapping_count: space.mappings.len(),
            copy_on_write_count: space
                .mappings
                .iter()
                .filter(|mapping| mapping.copy_on_write)
                .count(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(raw: u64) -> ProcessId {
        ProcessId::from_raw(raw).unwrap()
    }

    fn page(raw: u64) -> VirtualPage {
        VirtualPage::from_address(raw * PAGE_SIZE).unwrap()
    }

    #[test]
    fn page_and_frame_constructors_reject_unaligned_or_noncanonical_values() {
        assert_eq!(VirtualPage::from_address(0), None);
        assert_eq!(VirtualPage::from_address(PAGE_SIZE + 1), None);
        assert_eq!(VirtualPage::from_address(USER_VIRTUAL_END), None);
        assert_eq!(VirtualPage::from_raw(1).unwrap().address(), PAGE_SIZE);
        assert_eq!(PhysicalFrame::from_address(0), None);
        assert_eq!(PhysicalFrame::from_address(PAGE_SIZE + 1), None);
        assert_eq!(PhysicalFrame::from_raw(3).unwrap().address(), 3 * PAGE_SIZE);
    }

    #[test]
    fn mappings_are_isolated_and_reference_frames_are_reclaimed() {
        let mut table = AddressSpaceTable::new();
        let first = table.create(owner(1)).unwrap();
        let second = table.create(owner(2)).unwrap();
        let frame = table.allocate_frame().unwrap();
        let virtual_page = page(1);

        table
            .map(first, virtual_page, frame, PagePermissions::writable())
            .unwrap();
        assert_eq!(
            table.mapping(second, virtual_page),
            Err(MemoryError::NotMapped)
        );
        assert_eq!(table.frame_snapshot(frame).unwrap().references, 1);
        table.unmap(first, virtual_page).unwrap();
        assert_eq!(
            table.frame_snapshot(frame),
            Err(MemoryError::InvalidPhysicalFrame)
        );
        assert_eq!(table.frame_count(), 0);
    }

    #[test]
    fn duplicate_mappings_and_wx_permissions_are_rejected_without_leaks() {
        let mut table = AddressSpaceTable::new();
        let space = table.create(owner(1)).unwrap();
        let permissions = PagePermissions::new(true, true, true).unwrap_err();
        assert_eq!(permissions, PermissionError::WritableExecutable);
        let writable = PagePermissions::writable();
        table.map_new(space, page(1), writable).unwrap();
        assert_eq!(
            table.map_new(space, page(1), writable),
            Err(MemoryError::AlreadyMapped)
        );
        assert_eq!(table.frame_count(), 1);
    }

    #[test]
    fn clone_cow_shares_writable_pages_until_fault_resolution() {
        let mut table = AddressSpaceTable::new();
        let parent = table.create(owner(1)).unwrap();
        let child = owner(2);
        let virtual_page = page(2);
        let frame = table
            .map_new(parent, virtual_page, PagePermissions::writable())
            .unwrap()
            .frame;
        let child_space = table.clone_cow(parent, child).unwrap();

        assert!(table.mapping(parent, virtual_page).unwrap().copy_on_write);
        assert!(
            table
                .mapping(child_space, virtual_page)
                .unwrap()
                .copy_on_write
        );
        assert_eq!(table.frame_snapshot(frame).unwrap().references, 2);
        assert_eq!(
            table.protect(parent, virtual_page, PagePermissions::writable()),
            Err(MemoryError::CowRequired)
        );

        let replacement = table.allocate_frame().unwrap();
        let resolution = table
            .resolve_cow_fault(child_space, virtual_page, replacement)
            .unwrap();
        assert_eq!(resolution.old_frame, frame);
        assert_eq!(
            table
                .mapping(child_space, virtual_page)
                .unwrap()
                .permissions,
            PagePermissions::writable()
        );
        assert_eq!(table.frame_snapshot(frame).unwrap().references, 1);
        assert_eq!(table.frame_snapshot(replacement).unwrap().references, 1);
    }

    #[test]
    fn destroying_a_space_releases_all_mapping_references() {
        let mut table = AddressSpaceTable::new();
        let space = table.create(owner(7)).unwrap();
        table
            .map_new(space, page(3), PagePermissions::read_only())
            .unwrap();
        table
            .map_new(space, page(4), PagePermissions::executable())
            .unwrap();
        let snapshot = table.destroy(space).unwrap();
        assert_eq!(snapshot.mapping_count, 2);
        assert_eq!(table.address_space_count(), 0);
        assert_eq!(table.frame_count(), 0);
    }
}
