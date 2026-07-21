/// A bounded, resettable bump heap backed by process-local `.bss` or stack memory.
///
/// Allocations are monotonically carved from the backing array. The whole heap can
/// be reset once all returned references are no longer used. Mutable access to the
/// heap statically guarantees that live allocations cannot overlap a reset.
pub struct BumpHeap<const BYTES: usize> {
    storage: [u8; BYTES],
    cursor: usize,
}

impl<const BYTES: usize> BumpHeap<BYTES> {
    pub const fn new() -> Self {
        Self {
            storage: [0; BYTES],
            cursor: 0,
        }
    }

    pub const fn capacity(&self) -> usize {
        BYTES
    }

    pub fn used(&self) -> usize {
        self.cursor
    }

    pub fn remaining(&self) -> usize {
        BYTES.saturating_sub(self.used())
    }

    pub fn allocate(&mut self, length: usize, alignment: usize) -> Option<&mut [u8]> {
        if alignment == 0 || !alignment.is_power_of_two() {
            return None;
        }

        let mask = alignment.saturating_sub(1);
        let base = self.storage.as_ptr() as usize;
        let current = base.checked_add(self.cursor)?;
        let aligned = current.checked_add(mask)? & !mask;
        let start = aligned.checked_sub(base)?;
        let end = start.checked_add(length)?;
        if end > BYTES {
            return None;
        }
        self.cursor = end;
        Some(&mut self.storage[start..end])
    }

    pub fn copy_bytes(&mut self, bytes: &[u8], alignment: usize) -> Option<&mut [u8]> {
        let destination = self.allocate(bytes.len(), alignment)?;
        destination.copy_from_slice(bytes);
        Some(destination)
    }

    pub fn reset(&mut self) {
        self.cursor = 0;
    }
}

impl<const BYTES: usize> Default for BumpHeap<BYTES> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::BumpHeap;

    #[test]
    fn allocations_are_aligned_and_disjoint() {
        let mut heap = BumpHeap::<128>::new();
        let first_end = {
            let first = heap.allocate(7, 1).expect("first allocation");
            first.fill(0x11);
            first.as_ptr() as usize + first.len()
        };
        let second = heap.allocate(16, 16).expect("aligned allocation");

        assert_eq!((second.as_ptr() as usize) % 16, 0);
        assert!(second.as_ptr() as usize >= first_end);
        assert!(second.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn exhaustion_and_reset_are_bounded() {
        let mut heap = BumpHeap::<32>::new();
        assert!(heap.allocate(32, 1).is_some());
        assert!(heap.allocate(1, 1).is_none());
        assert_eq!(heap.remaining(), 0);

        heap.reset();
        assert_eq!(heap.used(), 0);
        assert!(heap.copy_bytes(b"reset", 1).is_some());
    }
}
