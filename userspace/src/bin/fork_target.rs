#![no_std]
#![no_main]

use userspace::{
    args::Args,
    managed_startup::{ManagedToolStartMode, managed_tool_start_mode},
    syscall,
};

userspace::managed_tool_entry!(rust_main);
userspace::panic_handler!();

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if managed_tool_start_mode() != ManagedToolStartMode::Managed
        || arguments.len() != 2
        || arguments.get(1) != Some(b"inherited")
    {
        syscall::exit(12);
    }
    if syscall::getpid().is_err() || syscall::write_all(3, b"target-after-exec\n").is_err() {
        syscall::exit(13);
    }
    syscall::exit(17)
}
