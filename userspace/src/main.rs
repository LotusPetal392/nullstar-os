#![no_std]
#![no_main]

use userspace::{
    abi::INIT_PROCESS_ID,
    supervisor::{ShellStatusDisposition, shell_status_disposition},
    syscall::{self, ProcessId, STDERR, STDOUT, SpawnFlags},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const INIT_READY: &[u8] = b"userspace init ready: pid=1\n";
const SHELL_COMMAND: &[u8] = b"/ush";
const SHELL_LAUNCHED: &[u8] = b"userspace init launched /ush\n";
const SHELL_RESTARTING: &[u8] = b"userspace init: /ush exited; restarting\n";
const WRONG_PROCESS_ID: &[u8] = b"userspace init: expected process id 1\n";
const SHELL_SPAWN_FAILED: &[u8] = b"userspace init: failed to launch /ush\n";
const SHELL_WAIT_FAILED: &[u8] = b"userspace init: failed while waiting for /ush\n";
const SHELL_FOREGROUND_FAILED: &[u8] =
    b"userspace init: failed to restore /ush to the foreground\n";

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    if syscall::getpid() != Ok(INIT_PROCESS_ID) {
        fail(WRONG_PROCESS_ID);
    }
    if syscall::write_all(STDOUT, INIT_READY).is_err() {
        syscall::exit(1);
    }

    loop {
        let shell_process_id = match syscall::spawn_command(
            SHELL_COMMAND,
            SpawnFlags::FOREGROUND | SpawnFlags::NEW_PROCESS_GROUP,
            None,
            None,
            None,
            None,
        ) {
            Ok(process_id) => process_id,
            Err(_) => fail(SHELL_SPAWN_FAILED),
        };
        let _ = syscall::write_all(STDOUT, SHELL_LAUNCHED);

        supervise_shell(shell_process_id);
        let _ = syscall::write_all(STDOUT, SHELL_RESTARTING);
    }
}

fn supervise_shell(shell_process_id: ProcessId) {
    loop {
        let status = match syscall::wait_child(shell_process_id) {
            Ok(status) => status,
            Err(error) if error == syscall::Errno::INTERRUPTED => continue,
            Err(_) => fail(SHELL_WAIT_FAILED),
        };

        match shell_status_disposition(status.raw()) {
            ShellStatusDisposition::WaitForNextEvent => {}
            ShellStatusDisposition::RestoreForeground => {
                if syscall::foreground_process_group(shell_process_id).is_err() {
                    fail(SHELL_FOREGROUND_FAILED);
                }
            }
            ShellStatusDisposition::RestartShell => return,
        }
    }
}

fn fail(message: &[u8]) -> ! {
    let _ = syscall::write_all(STDERR, message);
    syscall::exit(1)
}
