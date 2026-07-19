#![no_std]
#![no_main]

use userspace::syscall::{self, STDOUT};

userspace::entry!(rust_main);
userspace::panic_handler!();

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    if syscall::write_all(STDOUT, b"signal probe running\n").is_err() {
        syscall::exit(1);
    }
    loop {
        if syscall::yield_now().is_err() {
            syscall::exit(1);
        }
    }
}
