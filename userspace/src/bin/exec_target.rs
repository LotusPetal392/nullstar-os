#![no_std]
#![no_main]

use userspace::{
    args::Args,
    managed_startup::{ManagedToolStartMode, managed_tool_start_mode},
    syscall::{self, Errno, STDOUT},
};

userspace::managed_tool_entry!(rust_main);
userspace::panic_handler!();

const SUCCESS: &[u8] = b"transactional exec target passed\n";

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if managed_tool_start_mode() != ManagedToolStartMode::Managed
        || arguments.len() != 3
        || arguments.get(0) != Some(b"/exec-target")
        || arguments.get(1) != Some(b"alpha")
        || arguments.get(2) != Some(b"beta")
    {
        syscall::exit(64);
    }

    if syscall::getpid().is_err() {
        syscall::exit(1);
    }
    for _ in 0..8 {
        if syscall::yield_now().is_err() {
            syscall::exit(1);
        }
    }
    if syscall::write_all(3, b"target-after-exec\n").is_err() {
        syscall::exit(1);
    }

    match syscall::write_all(4, b"must-not-be-written\n") {
        Err(error) if error == Errno::BAD_FILE_DESCRIPTOR => {}
        _ => syscall::exit(2),
    }

    if syscall::close(3).is_err() || syscall::write_all(STDOUT, SUCCESS).is_err() {
        syscall::exit(3);
    }
    syscall::exit(23)
}
