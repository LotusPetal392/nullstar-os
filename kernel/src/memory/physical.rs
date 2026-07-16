use bootloader_api::info::{MemoryRegion, MemoryRegionKind, MemoryRegions};
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, OffsetPageTable, PageSize, PageTable, PhysFrame, Size4KiB,
    },
};

pub const FRAME_SIZE: u64 = Size4KiB::SIZE;

/// Provides access to the active level 4 page table through the bootloader's
/// physical-memory mapping.
///
/// # Safety
///
/// The caller must guarantee that `physical_memory_offset` is the offset of a
/// complete physical-memory mapping and that this function is called at most
/// once for the active page-table hierarchy.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) }
}

/// Returns a mutable reference to the currently active level 4 page table.
///
/// # Safety
///
/// The supplied offset must map every physical address to the corresponding
/// virtual address. The caller must also ensure that no other mutable reference
/// to the active level 4 table exists.
unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let (level_4_table_frame, _) = Cr3::read();
    let physical_address = level_4_table_frame.start_address();
    let virtual_address = physical_memory_offset + physical_address.as_u64();
    let page_table_pointer: *mut PageTable = virtual_address.as_mut_ptr();

    unsafe { &mut *page_table_pointer }
}

/// Allocates unique 4 KiB frames from regions the bootloader marks as usable.
pub struct BootInfoFrameAllocator {
    memory_regions: &'static [MemoryRegion],
    next_region: usize,
    next_frame_address: u64,
    allocated_frames: u64,
    usable_frames: u64,
}

impl BootInfoFrameAllocator {
    /// Creates a frame allocator over the supplied bootloader memory map.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that the memory map remains valid for the
    /// lifetime of the kernel and that no other allocator can hand out frames
    /// from the same usable regions.
    pub unsafe fn init(memory_regions: &'static MemoryRegions) -> Self {
        let memory_regions: &'static [MemoryRegion] = memory_regions;
        let usable_frames = memory_regions.iter().map(usable_frame_count).sum();

        Self {
            memory_regions,
            next_region: 0,
            next_frame_address: 0,
            allocated_frames: 0,
            usable_frames,
        }
    }

    pub fn usable_frame_count(&self) -> u64 {
        self.usable_frames
    }

    pub fn allocated_frame_count(&self) -> u64 {
        self.allocated_frames
    }

    pub fn remaining_frame_count(&self) -> u64 {
        self.usable_frames.saturating_sub(self.allocated_frames)
    }

    fn next_usable_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        loop {
            let region = self.memory_regions.get(self.next_region)?;

            let Some((region_start, region_end)) = usable_frame_bounds(region) else {
                self.advance_region();
                continue;
            };

            if self.next_frame_address < region_start {
                self.next_frame_address = region_start;
            }

            if self.next_frame_address < region_end {
                let frame_address = self.next_frame_address;
                self.next_frame_address = frame_address.saturating_add(FRAME_SIZE);
                self.allocated_frames = self.allocated_frames.saturating_add(1);

                return Some(PhysFrame::containing_address(PhysAddr::new(frame_address)));
            }

            self.advance_region();
        }
    }

    fn advance_region(&mut self) {
        self.next_region = self.next_region.saturating_add(1);
        self.next_frame_address = 0;
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.next_usable_frame()
    }
}

fn usable_frame_count(region: &MemoryRegion) -> u64 {
    usable_frame_bounds(region)
        .map(|(start, end)| (end - start) / FRAME_SIZE)
        .unwrap_or(0)
}

fn usable_frame_bounds(region: &MemoryRegion) -> Option<(u64, u64)> {
    if region.kind != MemoryRegionKind::Usable {
        return None;
    }

    let start = align_up(region.start)?;
    let end = align_down(region.end);

    (start < end).then_some((start, end))
}

fn align_up(address: u64) -> Option<u64> {
    address
        .checked_add(FRAME_SIZE - 1)
        .map(|value| value & !(FRAME_SIZE - 1))
}

const fn align_down(address: u64) -> u64 {
    address & !(FRAME_SIZE - 1)
}
