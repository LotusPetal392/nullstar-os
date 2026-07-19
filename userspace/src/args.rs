use core::{marker::PhantomData, slice};

use crate::abi::limits;

/// View of the kernel-built `argc`/`argv` block at process entry.
#[derive(Clone, Copy)]
pub struct Args<'a> {
    stack_pointer: *const usize,
    count: usize,
    marker: PhantomData<&'a [u8]>,
}

impl<'a> Args<'a> {
    /// Creates an argument view from the untouched userspace entry stack.
    ///
    /// # Safety
    ///
    /// `stack_pointer` must identify the initial stack layout constructed by the
    /// GalacticOS process loader and remain mapped for the lifetime `'a`.
    pub unsafe fn from_stack(stack_pointer: *const usize) -> Self {
        let count = unsafe { stack_pointer.read() }.min(limits::MAX_ARGUMENTS);
        Self {
            stack_pointer,
            count,
            marker: PhantomData,
        }
    }

    pub const fn len(self) -> usize {
        self.count
    }

    pub const fn is_empty(self) -> bool {
        self.count == 0
    }

    pub fn get(self, index: usize) -> Option<&'a [u8]> {
        if index >= self.count {
            return None;
        }

        let pointer = unsafe {
            self.stack_pointer
                .add(index.saturating_add(1))
                .cast::<*const u8>()
                .read()
        };
        if pointer.is_null() {
            return None;
        }

        let mut length = 0usize;
        while length < limits::MAX_ARGUMENT_BYTES {
            if unsafe { pointer.add(length).read() } == 0 {
                return Some(unsafe { slice::from_raw_parts(pointer, length) });
            }
            length = length.saturating_add(1);
        }
        None
    }

    pub fn iter(self) -> ArgIter<'a> {
        ArgIter {
            arguments: self,
            index: 0,
        }
    }
}

pub struct ArgIter<'a> {
    arguments: Args<'a>,
    index: usize,
}

impl<'a> Iterator for ArgIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let argument = self.arguments.get(self.index)?;
        self.index = self.index.saturating_add(1);
        Some(argument)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.arguments.len().saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for ArgIter<'_> {}
