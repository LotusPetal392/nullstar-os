#![no_std]
#![no_main]

use userspace::{
    args::Args,
    managed_startup::{ManagedToolStartMode, managed_tool_start_mode},
    syscall::{self, STDOUT},
};

userspace::managed_tool_entry!(rust_main);
userspace::panic_handler!();

const MESSAGE: &[u8] = b"Hello through a blocking NullStar OS pipe.\n";

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if arguments.get(1) == Some(&b"managed"[..])
        && managed_tool_start_mode() != ManagedToolStartMode::Managed
    {
        syscall::exit(2);
    }
    for _ in 0..32 {
        if syscall::yield_now().is_err() {
            syscall::exit(1);
        }
    }
    if syscall::write_all(STDOUT, MESSAGE).is_err() {
        syscall::exit(1);
    }
    syscall::exit(0)
}
