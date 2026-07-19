#![no_std]
#![no_main]

use userspace::{
    args::Args,
    syscall::{self, STDERR, STDOUT},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const USAGE: &[u8] = b"usage: /cat <path>\n";

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let Some(path) = arguments.get(1) else {
        let _ = syscall::write_all(STDERR, USAGE);
        syscall::exit(64);
    };

    let descriptor = match syscall::open(path, syscall::OpenFlags::READ) {
        Ok(descriptor) => descriptor,
        Err(_) => syscall::exit(1),
    };
    let mut buffer = [0_u8; 1024];
    loop {
        let count = match syscall::read(descriptor, &mut buffer) {
            Ok(count) => count,
            Err(_) => {
                let _ = syscall::close(descriptor);
                syscall::exit(1);
            }
        };
        if count == 0 {
            break;
        }
        if syscall::write_all(STDOUT, &buffer[..count]).is_err() {
            let _ = syscall::close(descriptor);
            syscall::exit(1);
        }
    }

    if syscall::close(descriptor).is_err() {
        syscall::exit(1);
    }
    syscall::exit(0)
}
