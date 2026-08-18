#![no_std]
#![no_main]

use userspace::{
    abi::limits,
    platform,
    syscall::{self, STDOUT},
};

userspace::managed_tool_entry!(rust_main);
userspace::panic_handler!();

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let mut path = [0_u8; limits::MAX_PATH_BYTES + 1];
    let path = match platform::getcwd(&mut path) {
        Ok(path) => path,
        Err(_) => syscall::exit(1),
    };
    if syscall::write_all(STDOUT, path).is_err() || syscall::write_all(STDOUT, b"\n").is_err() {
        syscall::exit(1);
    }
    syscall::exit(0)
}
