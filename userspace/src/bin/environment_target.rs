#![no_std]
#![no_main]

use userspace::{
    args::Args,
    environment::Environment,
    managed_startup::{ManagedToolStartMode, managed_tool_start_mode},
    syscall,
};

userspace::managed_tool_entry!(rust_main);
userspace::panic_handler!();

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let environment = unsafe { Environment::from_stack(initial_stack) };
    match arguments.get(1) {
        Some(b"child") | Some(b"parent") => {
            if managed_tool_start_mode() != ManagedToolStartMode::Managed
                || environment.find(b"BASE") != Some(&b"changed"[..])
                || environment.find(b"ADDED") != Some(&b"ready"[..])
                || environment.find(b"REMOVE").is_some()
                || environment.len() != 2
            {
                syscall::exit(20);
            }
            match arguments.get(1) {
                Some(b"child") => syscall::exit(31),
                Some(b"parent") => syscall::exit(32),
                _ => syscall::exit(21),
            }
        }
        Some(b"shell") => {
            if managed_tool_start_mode() != ManagedToolStartMode::Managed
                || environment.find(b"SHELL_VALUE") != Some(&b"expanded"[..])
                || environment.len() != 1
            {
                syscall::exit(22);
            }
            syscall::exit(0)
        }
        _ => syscall::exit(21),
    }
}
