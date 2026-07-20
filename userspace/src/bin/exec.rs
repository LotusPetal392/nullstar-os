#![no_std]
#![no_main]

use userspace::{
    abi::limits,
    args::Args,
    syscall::{self, STDERR},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const USAGE: &[u8] = b"usage: /exec <program> [arguments...]\n";
const FAILURE: &[u8] = b"exec: image replacement failed\n";

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if arguments.len() < 2 {
        let _ = syscall::write_all(STDERR, USAGE);
        syscall::exit(64);
    }

    let mut command = [0_u8; limits::MAX_COMMAND_BYTES];
    let mut length = 0usize;
    for (index, argument) in arguments.iter().skip(1).enumerate() {
        if index != 0 {
            if length == command.len() {
                let _ = syscall::write_all(STDERR, FAILURE);
                syscall::exit(7);
            }
            command[length] = b' ';
            length += 1;
        }
        let Some(end) = length.checked_add(argument.len()) else {
            let _ = syscall::write_all(STDERR, FAILURE);
            syscall::exit(7);
        };
        if end > command.len() || argument.iter().any(|byte| byte.is_ascii_whitespace()) {
            let _ = syscall::write_all(STDERR, FAILURE);
            syscall::exit(7);
        }
        command[length..end].copy_from_slice(argument);
        length = end;
    }

    if syscall::execve(&command[..length]).is_err() {
        let _ = syscall::write_all(STDERR, FAILURE);
        syscall::exit(126);
    }
    syscall::exit(127)
}
