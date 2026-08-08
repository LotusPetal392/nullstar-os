//! Public userspace syscall facade.
//!
//! Supported launches are constructed from generic process and descriptor
//! primitives. Pipelines use a close-on-exec pipe as a launch barrier so every
//! child remains blocked until the parent has finished descriptor and
//! process-group setup. Bundled userspace no longer calls the legacy atomic
//! spawn syscall; syscall 7 remains a kernel ABI compatibility entry point.

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

#[derive(Debug)]
pub struct LaunchBarrier {
    pair: PipePair,
    reader_open: bool,
    writer_open: bool,
}

impl LaunchBarrier {
    pub fn new() -> Result<Self> {
        let pair = pipe_pair()?;
        if set_descriptor_flags(pair.reader, DescriptorFlags::CLOSE_ON_EXEC).is_err()
            || set_descriptor_flags(pair.writer, DescriptorFlags::CLOSE_ON_EXEC).is_err()
        {
            let _ = close(pair.writer);
            let _ = close(pair.reader);
            return Err(Errno::IO);
        }
        Ok(Self {
            pair,
            reader_open: true,
            writer_open: true,
        })
    }

    pub fn release_in_place(&mut self) -> Result<()> {
        let writer_result = if self.writer_open {
            close(self.pair.writer).inspect(|()| self.writer_open = false)
        } else {
            Ok(())
        };
        let reader_result = if self.reader_open {
            close(self.pair.reader).inspect(|()| self.reader_open = false)
        } else {
            Ok(())
        };
        writer_result.and(reader_result)
    }

    pub fn release(mut self) -> Result<()> {
        self.release_in_place()
    }
}

impl Drop for LaunchBarrier {
    fn drop(&mut self) {
        let _ = self.release_in_place();
    }
}

#[derive(Clone, Copy)]
struct LaunchDescriptors {
    stdin: Option<FileDescriptor>,
    stdout: Option<FileDescriptor>,
    stderr: Option<FileDescriptor>,
}

impl LaunchDescriptors {
    const fn new(
        stdin: Option<FileDescriptor>,
        stdout: Option<FileDescriptor>,
        stderr: Option<FileDescriptor>,
    ) -> Self {
        Self {
            stdin,
            stdout,
            stderr,
        }
    }

    const fn is_empty(self) -> bool {
        self.stdin.is_none() && self.stdout.is_none() && self.stderr.is_none()
    }
}

pub fn spawn_command(
    command: &[u8],
    flags: SpawnFlags,
    stdin_descriptor: Option<FileDescriptor>,
    stdout_descriptor: Option<FileDescriptor>,
    stderr_descriptor: Option<FileDescriptor>,
    process_group: Option<ProcessGroupId>,
) -> Result<ProcessId> {
    spawn_command_inner(
        command,
        flags,
        LaunchDescriptors::new(stdin_descriptor, stdout_descriptor, stderr_descriptor),
        process_group,
        None,
    )
}

pub fn spawn_command_with_barrier(
    command: &[u8],
    flags: SpawnFlags,
    stdin_descriptor: Option<FileDescriptor>,
    stdout_descriptor: Option<FileDescriptor>,
    stderr_descriptor: Option<FileDescriptor>,
    process_group: Option<ProcessGroupId>,
    barrier: &LaunchBarrier,
) -> Result<ProcessId> {
    spawn_command_inner(
        command,
        flags,
        LaunchDescriptors::new(stdin_descriptor, stdout_descriptor, stderr_descriptor),
        process_group,
        Some(barrier.pair),
    )
}

fn spawn_command_inner(
    command: &[u8],
    flags: SpawnFlags,
    descriptors: LaunchDescriptors,
    process_group: Option<ProcessGroupId>,
    barrier: Option<PipePair>,
) -> Result<ProcessId> {
    if !generic_spawn_supported(
        command,
        flags,
        descriptors,
        process_group,
        barrier.is_some(),
    ) {
        return Err(Errno::INVALID_ARGUMENT);
    }

    let foreground = flags.contains(SpawnFlags::FOREGROUND);
    let child_process_id = fork()?;
    if child_process_id == 0 {
        launch_child(command, foreground, descriptors, process_group, barrier)
    }

    let target_group = process_group.unwrap_or(child_process_id);
    if platform::set_process_group(child_process_id, target_group).is_err() {
        terminate_and_reap(child_process_id);
        return Err(Errno::IO);
    }
    Ok(child_process_id)
}

fn generic_spawn_supported(
    command: &[u8],
    flags: SpawnFlags,
    descriptors: LaunchDescriptors,
    process_group: Option<ProcessGroupId>,
    barrier: bool,
) -> bool {
    let new_group = flags.contains(SpawnFlags::NEW_PROCESS_GROUP);
    let join_group = flags.contains(SpawnFlags::JOIN_PROCESS_GROUP);
    let descriptors_requested = flags.contains(SpawnFlags::USE_DESCRIPTORS);
    let group_request_valid = if new_group == join_group {
        false
    } else if new_group {
        process_group.is_none()
    } else {
        process_group.is_some_and(|group| group != 0)
    };

    !command.is_empty()
        && group_request_valid
        && (!flags.contains(SpawnFlags::FOREGROUND) || new_group)
        && (!join_group || barrier)
        && descriptors_requested != descriptors.is_empty()
}

fn launch_child(
    command: &[u8],
    foreground: bool,
    descriptors: LaunchDescriptors,
    process_group: Option<ProcessGroupId>,
    barrier: Option<PipePair>,
) -> ! {
    let process_id = match getpid() {
        Ok(process_id) => process_id,
        Err(_) => launch_failure(),
    };
    let expected_group = process_group.unwrap_or(process_id);

    if let Some(pair) = barrier
        && close(pair.writer).is_err()
    {
        launch_failure();
    }
    if !install_descriptors(descriptors) {
        launch_failure();
    }
    close_descriptor_sources(descriptors);

    if let Some(pair) = barrier {
        wait_for_barrier(pair.reader);
        if platform::get_process_group(0).ok() != Some(expected_group) {
            launch_failure();
        }
    } else if !wait_for_group(expected_group) {
        launch_failure();
    }

    if foreground && foreground_process_group(expected_group).is_err() {
        launch_failure();
    }
    if execve(command).is_err() {
        launch_failure();
    }
    exit(127)
}

fn install_descriptors(descriptors: LaunchDescriptors) -> bool {
    install_descriptor(descriptors.stdin, STDIN)
        && install_descriptor(descriptors.stdout, STDOUT)
        && install_descriptor(descriptors.stderr, STDERR)
}

fn install_descriptor(source: Option<FileDescriptor>, target: FileDescriptor) -> bool {
    source.is_none_or(|source| platform::dup2(source, target).is_ok())
}

fn close_descriptor_sources(descriptors: LaunchDescriptors) {
    let sources = [descriptors.stdin, descriptors.stdout, descriptors.stderr];
    for (index, source) in sources.iter().enumerate() {
        let Some(source) = *source else {
            continue;
        };
        if source < 3 || sources[..index].contains(&Some(source)) {
            continue;
        }
        if close(source).is_err() {
            launch_failure();
        }
    }
}

fn wait_for_group(expected_group: ProcessGroupId) -> bool {
    for _ in 0..GROUP_SETUP_YIELDS {
        if platform::get_process_group(0).ok() == Some(expected_group) {
            return true;
        }
        if yield_now().is_err() {
            break;
        }
    }
    false
}

fn wait_for_barrier(reader: FileDescriptor) {
    let mut byte = [0_u8; 1];
    loop {
        match read(reader, &mut byte) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error == Errno::INTERRUPTED => {}
            Err(_) => launch_failure(),
        }
    }
    if close(reader).is_err() {
        launch_failure();
    }
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

#[cfg(test)]
mod tests {
    use super::{LaunchDescriptors, generic_spawn_supported};
    use crate::syscall_legacy::{STDOUT, SpawnFlags};

    #[test]
    fn exec_launcher_uses_generic_new_group_path() {
        assert!(generic_spawn_supported(
            b"exec runtime-probe manual-argv",
            SpawnFlags::FOREGROUND | SpawnFlags::USE_DESCRIPTORS | SpawnFlags::NEW_PROCESS_GROUP,
            LaunchDescriptors::new(None, Some(STDOUT), None),
            None,
            false,
        ));
    }

    #[test]
    fn joined_group_requires_a_barrier() {
        let flags = SpawnFlags::USE_DESCRIPTORS | SpawnFlags::JOIN_PROCESS_GROUP;
        let descriptors = LaunchDescriptors::new(None, Some(STDOUT), None);
        assert!(!generic_spawn_supported(
            b"cat",
            flags,
            descriptors,
            Some(7),
            false,
        ));
        assert!(generic_spawn_supported(
            b"cat",
            flags,
            descriptors,
            Some(7),
            true,
        ));
    }

    #[test]
    fn descriptor_flag_must_match_descriptor_arguments() {
        assert!(!generic_spawn_supported(
            b"cat",
            SpawnFlags::NEW_PROCESS_GROUP,
            LaunchDescriptors::new(None, Some(STDOUT), None),
            None,
            false,
        ));
        assert!(!generic_spawn_supported(
            b"cat",
            SpawnFlags::USE_DESCRIPTORS | SpawnFlags::NEW_PROCESS_GROUP,
            LaunchDescriptors::new(None, None, None),
            None,
            false,
        ));
    }
}
