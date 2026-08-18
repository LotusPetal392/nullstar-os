#![no_std]
#![no_main]

use userspace::{
    args::Args,
    platform::{self, DirectoryEntry},
    syscall::{self, STDERR, STDOUT},
};

userspace::managed_tool_entry!(rust_main);
userspace::panic_handler!();

const PAGE_ENTRIES: usize = 16;
const USAGE: &[u8] = b"usage: ls [directory]\n";
const READ_FAILURE: &[u8] = b"ls: unable to read directory\n";

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if arguments.len() > 2 {
        let _ = syscall::write_all(STDERR, USAGE);
        syscall::exit(2);
    }
    let path = arguments.get(1).unwrap_or(b".");
    let mut start_index = 0usize;
    let mut entries = [DirectoryEntry::EMPTY; PAGE_ENTRIES];

    loop {
        let count = match platform::read_directory(path, start_index, &mut entries) {
            Ok(count) => count,
            Err(_) => {
                let _ = syscall::write_all(STDERR, READ_FAILURE);
                syscall::exit(1);
            }
        };
        for entry in &entries[..count] {
            if syscall::write_all(STDOUT, entry.name()).is_err()
                || entry.is_directory() && syscall::write_all(STDOUT, b"/").is_err()
                || syscall::write_all(STDOUT, b"\n").is_err()
            {
                syscall::exit(1);
            }
        }
        start_index = start_index.saturating_add(count);
        if count < entries.len() {
            break;
        }
    }

    syscall::exit(0)
}
