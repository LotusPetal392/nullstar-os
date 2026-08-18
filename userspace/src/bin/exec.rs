#![no_std]
#![no_main]

use userspace::{
    abi::limits,
    args::Args,
    environment::Environment,
    managed_startup::ManagedToolCommand,
    syscall::{self, STDERR},
};

userspace::managed_tool_entry!(rust_main);
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

    let inherited_environment = unsafe { Environment::from_stack(initial_stack) };
    let mut environment = [(&[][..], &[][..]); limits::MAX_ENVIRONMENT_VARIABLES];
    let mut environment_count = 0usize;
    for entry in inherited_environment.iter() {
        let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
            let _ = syscall::write_all(STDERR, FAILURE);
            syscall::exit(7);
        };
        environment[environment_count] = (&entry[..separator], &entry[separator + 1..]);
        environment_count += 1;
    }
    if syscall::exec_managed_command(ManagedToolCommand::new(
        &command[..length],
        &environment[..environment_count],
    ))
    .is_err()
    {
        let _ = syscall::write_all(STDERR, FAILURE);
        syscall::exit(126);
    }
    syscall::exit(127)
}
