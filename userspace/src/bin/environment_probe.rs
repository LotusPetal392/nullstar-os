#![no_std]
#![no_main]

use userspace::{environment::Environment, managed_startup::ManagedToolCommand, syscall};

userspace::entry!(rust_main);
userspace::panic_handler!();

const CHILD_COMMAND: &[u8] = b"/environment-target child";
const PARENT_COMMAND: &[u8] = b"/environment-target parent";
const EXEC_ENVIRONMENT: &[(&[u8], &[u8])] = &[(b"BASE", b"changed"), (b"ADDED", b"ready")];

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let environment = unsafe { Environment::from_stack(initial_stack) };
    if environment.find(b"BASE") != Some(&b"seed"[..])
        || environment.find(b"REMOVE") != Some(&b"gone"[..])
        || environment.find(b"ADDED").is_some()
    {
        syscall::exit(1);
    }
    if syscall::environment_set(b"BASE", b"changed").is_err() {
        syscall::exit(2);
    }
    if syscall::environment_set(b"ADDED", b"ready").is_err() {
        syscall::exit(3);
    }
    if syscall::environment_unset(b"REMOVE").is_err() {
        syscall::exit(4);
    }

    let child = match syscall::fork() {
        Ok(process_id) => process_id,
        Err(_) => syscall::exit(5),
    };
    if child == 0 {
        if syscall::exec_managed_command(ManagedToolCommand::new(CHILD_COMMAND, EXEC_ENVIRONMENT))
            .is_err()
        {
            syscall::exit(6);
        }
        syscall::exit(7);
    }
    match syscall::wait_child(child) {
        Ok(status) if status.raw() == 31 => {}
        _ => syscall::exit(8),
    }
    if syscall::exec_managed_command(ManagedToolCommand::new(PARENT_COMMAND, EXEC_ENVIRONMENT))
        .is_err()
    {
        syscall::exit(9);
    }
    syscall::exit(10)
}
