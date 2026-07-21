use core::{marker::PhantomData, slice};

use crate::abi::limits;

/// View of the kernel-built `envp` block at process entry.
#[derive(Clone, Copy)]
pub struct Environment<'a> {
    table: *const *const u8,
    count: usize,
    marker: PhantomData<&'a [u8]>,
}

impl<'a> Environment<'a> {
    /// Creates an environment view from the untouched userspace entry stack.
    ///
    /// # Safety
    ///
    /// `stack_pointer` must identify the initial stack layout constructed by the
    /// GalacticOS process loader and remain mapped for the lifetime `'a`.
    pub unsafe fn from_stack(stack_pointer: *const usize) -> Self {
        let argument_count = unsafe { stack_pointer.read() }.min(limits::MAX_ARGUMENTS);
        let table =
            unsafe { stack_pointer.add(argument_count.saturating_add(2)) }.cast::<*const u8>();
        let mut count = 0usize;
        while count < limits::MAX_ENVIRONMENT_VARIABLES {
            let pointer = unsafe { table.add(count).read() };
            if pointer.is_null() {
                break;
            }
            count = count.saturating_add(1);
        }
        Self {
            table,
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
        let pointer = unsafe { self.table.add(index).read() };
        if pointer.is_null() {
            return None;
        }
        let mut length = 0usize;
        while length < limits::MAX_ENVIRONMENT_BYTES {
            if unsafe { pointer.add(length).read() } == 0 {
                return Some(unsafe { slice::from_raw_parts(pointer, length) });
            }
            length = length.saturating_add(1);
        }
        None
    }

    pub fn find(self, name: &[u8]) -> Option<&'a [u8]> {
        self.iter().find_map(|entry| {
            let separator = entry.iter().position(|byte| *byte == b'=')?;
            (entry[..separator] == name[..]).then_some(&entry[separator.saturating_add(1)..])
        })
    }

    pub fn iter(self) -> EnvironmentIter<'a> {
        EnvironmentIter {
            environment: self,
            index: 0,
        }
    }
}

pub struct EnvironmentIter<'a> {
    environment: Environment<'a>,
    index: usize,
}

impl<'a> Iterator for EnvironmentIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.environment.get(self.index)?;
        self.index = self.index.saturating_add(1);
        Some(entry)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.environment.len().saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for EnvironmentIter<'_> {}
