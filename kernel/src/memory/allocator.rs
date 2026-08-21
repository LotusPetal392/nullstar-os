use core::{
    alloc::{GlobalAlloc, Layout},
    mem, ptr,
};

use spin::Mutex;
use x86_64::{
    VirtAddr,
    instructions::interrupts,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTableFlags, Size4KiB, mapper::MapToError,
    },
};

use crate::{
    arch::x86_64::ap_trampoline::ApTrampoline,
    memory::{BootInfoFrameAllocator, physical_memory_offset},
    serial_println,
};

pub const HEAP_START: usize = 0x_4444_4444_0000;
// Leave room for the cached framebuffer, filesystem buffers, and kernel services.
pub const HEAP_SIZE: usize = 32 * 1024 * 1024;
pub const HEAP_PAGE_COUNT: usize = HEAP_SIZE / 4096;

#[global_allocator]
static ALLOCATOR: LockedAllocator = LockedAllocator::new();

/// Maps the virtual heap and initializes the global linked-list allocator.
pub fn init_heap(
    mapper: &mut OffsetPageTable<'static>,
    frame_allocator: &mut BootInfoFrameAllocator,
) -> Result<(), MapToError<Size4KiB>> {
    let heap_start = VirtAddr::new(HEAP_START as u64);
    let heap_end = heap_start + (HEAP_SIZE - 1) as u64;
    let start_page: Page<Size4KiB> = Page::containing_address(heap_start);
    let end_page: Page<Size4KiB> = Page::containing_address(heap_end);

    for page in Page::range_inclusive(start_page, end_page) {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

        unsafe {
            mapper.map_to(page, frame, flags, frame_allocator)?.flush();
        }
    }

    unsafe {
        ALLOCATOR.init(HEAP_START, HEAP_SIZE);
    }

    match (
        frame_allocator.reserved_low_frame(),
        physical_memory_offset(),
    ) {
        (Some(frame), Some(offset)) => {
            match ApTrampoline::install_global(frame, offset, mapper, frame_allocator) {
                Ok(trampoline) => serial_println!(
                    "SMP AP trampoline installed: physical={:#x}, vector={:#x}",
                    trampoline.physical_address(),
                    trampoline.startup_vector()
                ),
                Err(error) => {
                    serial_println!("SMP AP trampoline installation failed: {error:?}")
                }
            }
        }
        _ => serial_println!("SMP AP trampoline unavailable: no reserved low-memory frame"),
    }

    Ok(())
}

struct LockedAllocator {
    inner: Mutex<LinkedListAllocator>,
}

impl LockedAllocator {
    const fn new() -> Self {
        Self {
            inner: Mutex::new(LinkedListAllocator::new()),
        }
    }

    unsafe fn init(&self, heap_start: usize, heap_size: usize) {
        unsafe {
            self.inner.lock().init(heap_start, heap_size);
        }
    }
}

unsafe impl GlobalAlloc for LockedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        interrupts::without_interrupts(|| {
            let (size, align) = size_align(layout);
            let mut allocator = self.inner.lock();

            let Some((region, allocation_start)) = allocator.find_region(size, align) else {
                return ptr::null_mut();
            };

            let allocation_end = allocation_start
                .checked_add(size)
                .expect("heap allocation address overflow");
            let region_start = region.start_addr();
            let excess_size = region.end_addr() - allocation_end;
            let prefix_size = allocation_start - region_start;

            if prefix_size > 0 {
                // SAFETY: `find_region` removed this region from the free list,
                // and the prefix is aligned, large enough for a list node, and
                // disjoint from the allocation returned to the caller.
                unsafe {
                    allocator.add_free_region(region_start, prefix_size);
                }
            }

            if excess_size > 0 {
                // SAFETY: this suffix was part of the selected free region and
                // starts after the allocation, so no live allocation aliases it.
                unsafe {
                    allocator.add_free_region(allocation_end, excess_size);
                }
            }

            allocation_start as *mut u8
        })
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        interrupts::without_interrupts(|| {
            let (size, _) = size_align(layout);

            // SAFETY: `GlobalAlloc` requires callers to pass the same live
            // allocation and layout returned by `alloc`; the allocator lock
            // serializes the resulting free-list mutation.
            unsafe {
                self.inner.lock().add_free_region(pointer as usize, size);
            }
        });
    }
}

struct LinkedListAllocator {
    head: ListNode,
}

impl LinkedListAllocator {
    const fn new() -> Self {
        Self {
            head: ListNode::new(0),
        }
    }

    unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        unsafe {
            self.add_free_region(heap_start, heap_size);
        }
    }

    unsafe fn add_free_region(&mut self, address: usize, size: usize) {
        assert_eq!(align_up(address, mem::align_of::<ListNode>()), address);
        assert!(size >= mem::size_of::<ListNode>());
        let end = address
            .checked_add(size)
            .expect("free-region address overflow");

        let mut current = &mut self.head;
        while current
            .next
            .as_ref()
            .is_some_and(|next| next.start_addr() < address)
        {
            current = current
                .next
                .as_mut()
                .expect("free-list successor disappeared");
        }

        if current.size != 0 {
            assert!(current.end_addr() <= address, "free regions overlap");
        }
        if let Some(next) = current.next.as_ref() {
            assert!(end <= next.start_addr(), "free regions overlap");
        }

        let mut node = ListNode::new(size);
        node.next = current.next.take();

        let node_pointer = address as *mut ListNode;
        // SAFETY: the caller guarantees that this aligned region is valid,
        // unallocated heap memory. The overlap checks above ensure that no
        // existing free-list node aliases it.
        unsafe {
            node_pointer.write(node);
            current.next = Some(&mut *node_pointer);
        }

        {
            let inserted = current
                .next
                .as_mut()
                .expect("new free-list node disappeared");
            Self::merge_adjacent(inserted);
        }
        if current.size != 0 {
            Self::merge_adjacent(current);
        }
    }

    fn merge_adjacent(region: &mut ListNode) {
        while region
            .next
            .as_ref()
            .is_some_and(|next| region.end_addr() == next.start_addr())
        {
            let next = region
                .next
                .take()
                .expect("adjacent free-list node disappeared");
            region.size = region
                .size
                .checked_add(next.size)
                .expect("coalesced free-region size overflow");
            region.next = next.next.take();
        }
    }

    fn find_region(&mut self, size: usize, align: usize) -> Option<(&'static mut ListNode, usize)> {
        let mut current = &mut self.head;

        while let Some(ref mut region) = current.next {
            if let Ok(allocation_start) = Self::allocation_start(region, size, align) {
                let next = region.next.take();
                let selected = current
                    .next
                    .take()
                    .expect("the selected free-list region disappeared");
                current.next = next;
                return Some((selected, allocation_start));
            }

            current = current
                .next
                .as_mut()
                .expect("the current free-list region disappeared");
        }

        None
    }

    fn allocation_start(region: &ListNode, size: usize, align: usize) -> Result<usize, ()> {
        let region_start = region.start_addr();
        let mut allocation_start = align_up(region_start, align);
        let prefix_size = allocation_start.checked_sub(region_start).ok_or(())?;
        if prefix_size > 0 && prefix_size < mem::size_of::<ListNode>() {
            let minimum_start = region_start
                .checked_add(mem::size_of::<ListNode>())
                .ok_or(())?;
            allocation_start = align_up(minimum_start, align);
        }
        let allocation_end = allocation_start.checked_add(size).ok_or(())?;

        if allocation_end > region.end_addr() {
            return Err(());
        }

        let excess_size = region.end_addr() - allocation_end;
        if excess_size > 0 && excess_size < mem::size_of::<ListNode>() {
            return Err(());
        }

        let prefix_size = allocation_start - region_start;
        if prefix_size > 0 && prefix_size < mem::size_of::<ListNode>() {
            return Err(());
        }

        Ok(allocation_start)
    }
}

struct ListNode {
    size: usize,
    next: Option<&'static mut ListNode>,
}

impl ListNode {
    const fn new(size: usize) -> Self {
        Self { size, next: None }
    }

    fn start_addr(&self) -> usize {
        self as *const Self as usize
    }

    fn end_addr(&self) -> usize {
        self.start_addr() + self.size
    }
}

fn size_align(layout: Layout) -> (usize, usize) {
    let layout = layout
        .align_to(mem::align_of::<ListNode>())
        .expect("failed to adjust allocation alignment")
        .pad_to_align();
    let size = layout.size().max(mem::size_of::<ListNode>());

    (size, layout.align())
}

const fn align_up(address: usize, align: usize) -> usize {
    (address + align - 1) & !(align - 1)
}
