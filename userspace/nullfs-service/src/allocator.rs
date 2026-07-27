use core::{
    alloc::{GlobalAlloc, Layout},
    mem, ptr,
};

use spin::Mutex;

const HEAP_BYTES: usize = 2 * 1024 * 1024;

#[repr(C, align(4096))]
struct HeapArena {
    bytes: [u8; HEAP_BYTES],
}

#[unsafe(link_section = ".bss.nullfs_heap")]
static mut HEAP_ARENA: HeapArena = HeapArena {
    bytes: [0; HEAP_BYTES],
};

#[global_allocator]
static ALLOCATOR: LockedAllocator = LockedAllocator::new();

pub fn init() {
    let heap_start = (&raw mut HEAP_ARENA).cast::<u8>() as usize;

    // SAFETY: the arena has static lifetime, is page-aligned, and is handed to
    // the allocator exactly once before the service performs any allocation.
    unsafe {
        ALLOCATOR.init(heap_start, HEAP_BYTES);
    }
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
        // SAFETY: the caller provides one exclusive, static backing arena.
        unsafe {
            self.inner.lock().init(heap_start, heap_size);
        }
    }
}

unsafe impl GlobalAlloc for LockedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let Some((size, align)) = size_align(layout) else {
            return ptr::null_mut();
        };
        let mut allocator = self.inner.lock();
        let Some((region, allocation_start)) = allocator.find_region(size, align) else {
            return ptr::null_mut();
        };

        let allocation_end = allocation_start + size;
        let region_start = region.start_addr();
        let prefix_size = allocation_start - region_start;
        let suffix_size = region.end_addr() - allocation_end;

        if prefix_size != 0 {
            // SAFETY: this prefix came from the removed free region and is
            // disjoint from the allocation returned to the caller.
            unsafe {
                allocator.add_free_region(region_start, prefix_size);
            }
        }
        if suffix_size != 0 {
            // SAFETY: this suffix came from the removed free region and starts
            // after the allocation returned to the caller.
            unsafe {
                allocator.add_free_region(allocation_end, suffix_size);
            }
        }

        allocation_start as *mut u8
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let Some((size, _)) = size_align(layout) else {
            return;
        };

        // SAFETY: `GlobalAlloc` requires the caller to return the same live
        // allocation and layout; the mutex serializes free-list mutation.
        unsafe {
            self.inner.lock().add_free_region(pointer as usize, size);
        }
    }
}

struct LinkedListAllocator {
    head: ListNode,
    initialized: bool,
}

impl LinkedListAllocator {
    const fn new() -> Self {
        Self {
            head: ListNode::new(0),
            initialized: false,
        }
    }

    unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        assert!(!self.initialized, "NullFS heap initialized twice");
        self.initialized = true;

        // SAFETY: the caller guarantees that the complete region is aligned,
        // writable, unallocated static storage.
        unsafe {
            self.add_free_region(heap_start, heap_size);
        }
    }

    unsafe fn add_free_region(&mut self, address: usize, size: usize) {
        assert_eq!(
            align_up(address, mem::align_of::<ListNode>()),
            Some(address)
        );
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

        // SAFETY: this aligned region is valid free arena storage, and the
        // overlap checks ensure that no existing free-list node aliases it.
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
        if !self.initialized {
            return None;
        }

        let mut current = &mut self.head;
        while let Some(ref mut region) = current.next {
            if let Some(allocation_start) = Self::allocation_start(region, size, align) {
                let next = region.next.take();
                let selected = current
                    .next
                    .take()
                    .expect("selected free-list region disappeared");
                current.next = next;
                return Some((selected, allocation_start));
            }

            current = current
                .next
                .as_mut()
                .expect("current free-list region disappeared");
        }
        None
    }

    fn allocation_start(region: &ListNode, size: usize, align: usize) -> Option<usize> {
        let region_start = region.start_addr();
        let mut allocation_start = align_up(region_start, align)?;
        let prefix_size = allocation_start.checked_sub(region_start)?;
        if prefix_size != 0 && prefix_size < mem::size_of::<ListNode>() {
            let minimum_start = region_start.checked_add(mem::size_of::<ListNode>())?;
            allocation_start = align_up(minimum_start, align)?;
        }
        let allocation_end = allocation_start.checked_add(size)?;
        if allocation_end > region.end_addr() {
            return None;
        }

        let prefix_size = allocation_start - region_start;
        let suffix_size = region.end_addr() - allocation_end;
        if (prefix_size != 0 && prefix_size < mem::size_of::<ListNode>())
            || (suffix_size != 0 && suffix_size < mem::size_of::<ListNode>())
        {
            return None;
        }
        Some(allocation_start)
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

fn size_align(layout: Layout) -> Option<(usize, usize)> {
    let layout = layout
        .align_to(mem::align_of::<ListNode>())
        .ok()?
        .pad_to_align();
    Some((
        layout.size().max(mem::size_of::<ListNode>()),
        layout.align(),
    ))
}

fn align_up(address: usize, align: usize) -> Option<usize> {
    address
        .checked_add(align - 1)
        .map(|value| value & !(align - 1))
}
