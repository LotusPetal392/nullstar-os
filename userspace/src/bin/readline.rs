#![no_std]
#![no_main]

use userspace::syscall::{self, STDERR, STDIN, STDOUT};

userspace::entry!(rust_main);
userspace::panic_handler!();

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    if syscall::write_all(STDOUT, b"readline> ").is_err() {
        syscall::exit(1);
    }

    let mut buffer = [0_u8; 256];
    let count = match syscall::read(STDIN, &mut buffer) {
        Ok(count) => count,
        Err(_) => syscall::exit(1),
    };
    if syscall::write_all(STDOUT, b"terminal: ").is_err()
        || syscall::write_all(STDOUT, &buffer[..count]).is_err()
    {
        let _ = syscall::write_all(STDERR, b"readline: write failed\n");
        syscall::exit(1);
    }
    syscall::exit(0)
}
