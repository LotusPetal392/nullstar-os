#![no_std]
#![no_main]
use userspace::syscall::{self, STDERR, STDOUT};
userspace::entry!(rust_main);
userspace::panic_handler!();
extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    if syscall::write_all(STDOUT, b"stdout probe line\n").is_err()
        || syscall::write_all(STDERR, b"stderr probe line\n").is_err()
    {
        syscall::exit(1);
    }
    syscall::exit(0)
}
