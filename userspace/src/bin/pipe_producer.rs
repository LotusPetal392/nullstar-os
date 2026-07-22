#![no_std]
#![no_main]

use userspace::syscall::{self, STDOUT};

userspace::entry!(rust_main);
userspace::panic_handler!();

const MESSAGE: &[u8] = b"Hello through a blocking NullStar OS pipe.\n";

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
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
