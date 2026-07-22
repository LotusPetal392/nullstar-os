//! Public userspace syscall facade.
//!
//! Simple new-process-group launches are constructed from generic process and
//! descriptor primitives. Descriptor-bearing and joined pipeline stages retain
//! the legacy atomic spawn call until the shell gains a launch barrier.

use crate::{abi::signal, platform};

pub use crate::syscall_legacy::{
    ChildStatus, DescriptorFlags, Errno, FileDescriptor, OpenFlags, PipePair, ProcessGroupId,
    ProcessId, Result, STDERR, STDIN, STDOUT, SeekFrom, SignalAction, SignalActionFlags,
    SignalFrame, SignalHandler, SignalMask, SignalMaskHow, SpawnFlags, close, current_signal_mask,
    environment_set, environment_unset, execve, exit, foreground_process_group, fork, getpid, open,
    pipe_pair, query_signal_action, read, seek, set_descriptor_flags, signal_action, signal_mask,
    signal_process_group, try_wait_child, wait_child, write, write_all, yield_now,
};

const GROUP_SETUP_YIELDS: usize = 256;
const CHILD_LAUNCH_FAILURE: &[u8] = b"userspace launch failed\n";

pub fn spawn_command(
    command: &[u8],
    flags: SpawnFlags,
    stdin_descriptor: Option<FileDescriptor>,
    stdout_descriptor: Option<FileDescriptor>,
    stderr_descriptor: Option<FileDescriptor>,
    process_group: Option<ProcessGroupId>,
) -> Result<ProcessId> {
    if !generic_spawn_supported(
        command,
        flags,
        stdin_descriptor,
        stdout_descriptor,
        stderr_descriptor,
        process_group,
    ) {
        return crate::syscall_legacy::spawn_command(
            command,
            flags,
            stdin_descriptor,
            stdout_descriptor,
            stderr_descriptor,
            process_group,
        );
    }

    let foreground = flags.contains(SpawnFlags::FOREGROUND);
    let child_process_id = fork()?;
    if child_process_id == 0 {
        launch_child(command, foreground)
    }

    if platform::set_process_group(child_process_id, child_process_id).is_err() {
        terminate_and_reap(child_process_id);
        return Err(Errno::IO);
    }
    Ok(child_process_id)
}

fn generic_spawn_supported(
    command: &[u8],
    flags: SpawnFlags,
    stdin_descriptor: Option<FileDescriptor>,
    stdout_descriptor: Option<FileDescriptor>,
    stderr_descriptor: Option<FileDescriptor>,
    process_group: Option<ProcessGroupId>,
) -> bool {
    !command.is_empty()
        && flags.contains(SpawnFlags::NEW_PROCESS_GROUP)
        && !flags.contains(SpawnFlags::USE_DESCRIPTORS)
        && !flags.contains(SpawnFlags::JOIN_PROCESS_GROUP)
        && stdin_descriptor.is_none()
        && stdout_descriptor.is_none()
        && stderr_descriptor.is_none()
        && process_group.is_none()
        && !compatibility_launcher(command)
}

fn compatibility_launcher(command: &[u8]) -> bool {
    let token_end = command
        .iter()
        .position(|byte| byte.is_ascii_whitespace())
        .unwrap_or(command.len());
    let program = &command[..token_end];
    program == b"exec" || program.ends_with(b"/exec")
}

fn launch_child(command: &[u8], foreground: bool) -> ! {
    let process_id = match getpid() {
        Ok(process_id) => process_id,
        Err(_) => launch_failure(),
    };

    let mut grouped = false;
    for _ in 0..GROUP_SETUP_YIELDS {
        if platform::get_process_group(0).ok() == Some(process_id) {
            grouped = true;
            break;
        }
        if yield_now().is_err() {
            break;
        }
    }
    if !grouped {
        launch_failure();
    }
    if foreground && foreground_process_group(process_id).is_err() {
        launch_failure();
    }
    if execve(command).is_err() {
        launch_failure();
    }
    exit(127)
}

fn launch_failure() -> ! {
    let _ = write_all(STDERR, CHILD_LAUNCH_FAILURE);
    exit(126)
}

fn terminate_and_reap(process_id: ProcessId) {
    let _ = platform::kill(process_id, signal::TERMINATE);
    loop {
        match wait_child(process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Err(error) if error == Errno::INTERRUPTED => {}
            _ => break,
        }
    }
}
