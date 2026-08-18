#![no_std]
#![no_main]

use userspace::syscall::{self, STDIN, STDOUT};

userspace::managed_tool_entry!(rust_main);
userspace::panic_handler!();

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let mut buffer = [0_u8; 256];
    loop {
        let count = match syscall::read(STDIN, &mut buffer) {
            Ok(count) => count,
            Err(_) => syscall::exit(1),
        };
        if count == 0 {
            syscall::exit(0);
        }
        for byte in &mut buffer[..count] {
            if byte.is_ascii_lowercase() {
                *byte = byte.to_ascii_uppercase();
            }
        }
        if syscall::write_all(STDOUT, &buffer[..count]).is_err() {
            syscall::exit(1);
        }
    }
}
