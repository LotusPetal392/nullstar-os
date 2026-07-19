use core::{
    cell::{Cell, UnsafeCell},
    mem::MaybeUninit,
    slice,
};

/// A bounded, resettable bump heap backed by process-local `.bss` or stack memory.
///
/// Allocations are monotonically carved from the backing array. The whole heap can
/// be reset once all returned references are no longer used. GalacticOS processes
/// are single-threaded, so this type deliberately does not implement `Sync`.
pub struct BumpHeap<const BYTES: usize> {
    storage: UnsafeCell<[MaybeUninit<u8>; BYTES]>,
    cursor: Cell<usize>,
}

impl<const BYTES: usize> BumpHeap<BYTES> {
    pub const fn new() -> Self {
        Self {
            storage: UnsafeCell::new([MaybeUninit::uninit(); BYTES]),
            cursor: Cell::new(0),
        }
    }

    pub const fn capacity(&self) -> usize {
        BYTES
    }

    pub fn used(&self) -> usize {
        self.cursor.get()
    }

    pub fn remaining(&self) -> usize {
        BYTES.saturating_sub(self.used())
    }

    pub fn allocate(&self, length: usize, alignment: usize) -> Option<&mut [u8]> {
        if alignment == 0 || !alignment.is_power_of_two() {
            return None;
        }

        let mask = alignment.saturating_sub(1);
        let base = unsafe { (*self.storage.get()).as_mut_ptr().cast::<u8>() } as usize;
        let current = base.checked_add(self.cursor.get())?;
        let aligned = current.checked_add(mask)? & !mask;
        let start = aligned.checked_sub(base)?;
        let end = start.checked_add(length)?;
        if end > BYTES {
            return None;
        }
        self.cursor.set(end);

        let pointer = unsafe { (*self.storage.get()).as_mut_ptr().add(start).cast::<u8>() };
        Some(unsafe { slice::from_raw_parts_mut(pointer, length) })
    }

    pub fn copy_bytes(&self, bytes: &[u8], alignment: usize) -> Option<&mut [u8]> {
        let destination = self.allocate(bytes.len(), alignment)?;
        destination.copy_from_slice(bytes);
        Some(destination)
    }

    pub fn reset(&mut self) {
        self.cursor.set(0);
    }
}

impl<const BYTES: usize> Default for BumpHeap<BYTES> {
    fn default() -> Self {
        Self::new()
    }
}
