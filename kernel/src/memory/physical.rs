use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use bootloader_api::info::{MemoryRegion, MemoryRegionKind, MemoryRegions};
use x86_64::{
    PhysAddr, VirtAddr,
    registers::control::Cr3,
    structures::paging::{
        FrameAllocator, OffsetPageTable, PageSize, PageTable, PhysFrame, Size4KiB,
    },
};

pub const FRAME_SIZE: u64 = Size4KiB::SIZE;
const SMP_TRAMPOLINE_LIMIT: u64 = 0x10_0000;

static PHYSICAL_MEMORY_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Provides access to the active level 4 page table through the bootloader's
/// physical-memory mapping.
///
/// # Safety
///
/// The caller must guarantee that `physical_memory_offset` is the offset of a
/// complete physical-memory mapping and that this function is called at most
/// once for the active page-table hierarchy.
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    PHYSICAL_MEMORY_OFFSET.store(physical_memory_offset.as_u64(), Ordering::Release);
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) }
}

pub fn physical_memory_offset() -> Option<VirtAddr> {
    let offset = PHYSICAL_MEMORY_OFFSET.load(Ordering::Acquire);
    (offset != 0).then(|| VirtAddr::new(offset))
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
///
/// Returned process frames can be recycled after a task has stopped running and
/// its address space has been detached from CR3. The allocator remains single-
/// owner: callers must continue to use the one instance created during boot.
pub struct BootInfoFrameAllocator {
    memory_regions: &'static [MemoryRegion],
    next_region: usize,
    next_frame_address: u64,
    allocated_frames: u64,
    usable_frames: u64,
    recycled_frames: Vec<PhysFrame<Size4KiB>>,
    reserved_low_frame: Option<PhysFrame<Size4KiB>>,
    reclaimed_frames: u64,
    reused_frames: u64,
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

        let mut allocator = Self {
            memory_regions,
            next_region: 0,
            next_frame_address: 0,
            allocated_frames: 0,
            usable_frames,
            recycled_frames: Vec::new(),
            reserved_low_frame: None,
            reclaimed_frames: 0,
            reused_frames: 0,
        };
        let _ = allocator.reserve_frame_below(SMP_TRAMPOLINE_LIMIT);
        allocator
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

    pub fn recycled_frame_count(&self) -> usize {
        self.recycled_frames.len()
    }

    pub fn reserved_frame_count(&self) -> usize {
        usize::from(self.reserved_low_frame.is_some())
    }

    pub fn reserved_low_frame(&self) -> Option<PhysFrame<Size4KiB>> {
        self.reserved_low_frame
    }

    pub fn reclaimed_frame_count(&self) -> u64 {
        self.reclaimed_frames
    }

    pub fn reused_frame_count(&self) -> u64 {
        self.reused_frames
    }

    /// Reserve one usable frame below `exclusive_limit` before normal frame
    /// allocation begins. SMP uses this to hold a low-memory page for the AP
    /// startup trampoline without allowing the heap or a process mapping to
    /// recycle that page later.
    ///
    /// This path must remain allocation-free because it runs before the kernel
    /// heap is initialized.
    pub fn reserve_frame_below(&mut self, exclusive_limit: u64) -> Option<PhysFrame<Size4KiB>> {
        if self.allocated_frames != 0
            || !self.recycled_frames.is_empty()
            || self.reserved_low_frame.is_some()
        {
            return None;
        }

        let limit = align_down(exclusive_limit);
        if limit < FRAME_SIZE {
            return None;
        }

        let mut selected = None;
        for region in self.memory_regions {
            let Some((region_start, region_end)) = usable_frame_bounds(region) else {
                continue;
            };
            let region_end = region_end.min(limit);
            if region_start >= region_end {
                continue;
            }

            let address = region_end.saturating_sub(FRAME_SIZE);
            if address < FRAME_SIZE {
                continue;
            }
            let frame = PhysFrame::containing_address(PhysAddr::new(address));
            if selected
                .map(|current: PhysFrame<Size4KiB>| frame.start_address() > current.start_address())
                .unwrap_or(true)
            {
                selected = Some(frame);
            }
        }

        let frame = selected?;
        self.reserved_low_frame = Some(frame);
        self.allocated_frames = self.allocated_frames.saturating_add(1);
        Some(frame)
    }

    /// Returns a frame to the allocator after every mapping that references it
    /// has been removed and the frame is no longer reachable through an active
    /// CR3. Process reaping is the first caller of this interface.
    pub fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        debug_assert_ne!(
            self.reserved_low_frame,
            Some(frame),
            "reserved physical frame cannot be returned to the allocator"
        );
        debug_assert!(
            !self.recycled_frames.contains(&frame),
            "physical frame was returned to the allocator twice"
        );
        self.recycled_frames.push(frame);
        self.allocated_frames = self.allocated_frames.saturating_sub(1);
        self.reclaimed_frames = self.reclaimed_frames.saturating_add(1);
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
                let frame = PhysFrame::containing_address(PhysAddr::new(frame_address));
                if self.reserved_low_frame == Some(frame) {
                    continue;
                }
                return Some(frame);
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
        let frame = if let Some(frame) = self.recycled_frames.pop() {
            self.reused_frames = self.reused_frames.saturating_add(1);
            frame
        } else {
            self.next_usable_frame()?
        };
        self.allocated_frames = self.allocated_frames.saturating_add(1);
        Some(frame)
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
