#![no_std]
#![no_main]

use userspace::{
    args::Args,
    logctl,
    syscall::{self, STDERR, STDOUT},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const OBSERVER_ROUTE_HANDLE: u64 = 1;
const COMPLETE_MARKER: &[u8] = b"logctl: show complete\n";
const USAGE: &[u8] = b"usage: logctl show\n";
const SHOW_FAILED: &[u8] = b"logctl: history query failed\n";

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if arguments.len() != 2 || arguments.get(1) != Some(b"show") {
        fail(USAGE, 2);
    }
    if logctl::show(OBSERVER_ROUTE_HANDLE).is_err()
        || syscall::write_all(STDOUT, COMPLETE_MARKER).is_err()
    {
        fail(SHOW_FAILED, 3);
    }
    syscall::exit(0)
}

fn fail(message: &[u8], code: u64) -> ! {
    let _ = syscall::write_all(STDERR, message);
    syscall::exit(code)
}
