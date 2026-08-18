#![no_std]
#![no_main]

use userspace::syscall::{self, STDOUT};

userspace::managed_tool_entry!(rust_main);
userspace::panic_handler!();

// Keep the probe alive across prompt recovery without making the smoke test slow.
extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    for _ in 0..64 {
        if syscall::yield_now().is_err() {
            syscall::exit(1);
        }
    }
    if syscall::write_all(STDOUT, b"background job complete\n").is_err() {
        syscall::exit(1);
    }
    syscall::exit(0)
}
