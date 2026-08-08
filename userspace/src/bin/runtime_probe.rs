#![no_std]
#![no_main]

use userspace::{
    abi::{capability, file, signal},
    args::Args,
    heap::BumpHeap,
    ipc::{self, ObjectKind, Rights, Transfer},
    platform::{self, DirectoryEntry},
    syscall::{self, OpenFlags, STDERR, STDIN, STDOUT, SignalAction, SignalMask, SpawnFlags},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SUCCESS: &[u8] = b"userspace Rust runtime probe passed\n";
const DIRECTORY_PAGE: usize = 8;
const JOB_WAIT_YIELDS: usize = 4096;

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let Some(argument) = arguments.get(1) else {
        syscall::exit(64);
    };
    if arguments.len() != 2 || argument.is_empty() {
        syscall::exit(64);
    }

    let mut heap = BumpHeap::<4096>::new();
    let block_length = {
        let Some(block) = heap.allocate(257, 16) else {
            syscall::exit(1);
        };
        if !(block.as_ptr() as usize).is_multiple_of(16) {
            syscall::exit(1);
        }
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = (index & 0xff) as u8;
        }
        if block[0] != 0 || block[256] != 0 {
            syscall::exit(1);
        }
        block.len()
    };

    let copy_matches = {
        let Some(copy) = heap.copy_bytes(argument, 8) else {
            syscall::exit(1);
        };
        copy == argument
    };
    if !copy_matches || heap.used() <= block_length {
        syscall::exit(1);
    }

    heap.reset();
    if heap.used() != 0 || heap.remaining() != heap.capacity() {
        syscall::exit(1);
    }
    let process_id = match syscall::getpid() {
        Ok(process_id) => process_id,
        Err(_) => syscall::exit(1),
    };
    if !platform_probe(argument, process_id) {
        syscall::exit(1);
    }
    if syscall::write_all(STDOUT, SUCCESS).is_err() {
        syscall::exit(1);
    }
    syscall::exit(0)
}

fn platform_probe(argument: &[u8], process_id: u64) -> bool {
    let Ok(info) = platform::system_info() else {
        return false;
    };
    if info.abi_major != userspace::abi::ABI_VERSION_MAJOR
        || info.abi_minor != userspace::abi::ABI_VERSION_MINOR
        || info.capabilities & capability::PLATFORM_V1 != capability::PLATFORM_V1
        || info.capabilities & capability::PROTECTION_V1 != capability::PROTECTION_V1
        || info.capabilities & capability::PROCESS_GROUP_CONTROL == 0
        || info.page_size != 4096
        || info.maximum_open_files < 3
    {
        return false;
    }
    if !capability_probe(process_id) || !job_probe() {
        return false;
    }
    let Ok(process_group) = platform::get_process_group(0) else {
        return false;
    };
    if process_group == 0
        || platform::set_process_group(0, process_group).ok() != Some(process_group)
    {
        return false;
    }

    let Ok(hello_stat) = platform::stat(b"/hello.txt") else {
        return false;
    };
    if !hello_stat.is_file() || hello_stat.size < 2 {
        return false;
    }

    if !root_directory_has_expected_entries() {
        return false;
    }

    let mut cwd = [0_u8; 64];
    let Ok(initial_directory) = platform::getcwd(&mut cwd) else {
        return false;
    };
    if initial_directory != b"/" || platform::chdir(b"/tmp").is_err() {
        return false;
    }
    let Ok(tmp_directory) = platform::getcwd(&mut cwd) else {
        return false;
    };
    if tmp_directory != b"/tmp" {
        return false;
    }
    let Ok(relative_stat) = platform::stat(b".") else {
        return false;
    };
    if !relative_stat.is_directory() {
        return false;
    }
    let mut directory_page = [DirectoryEntry::EMPTY; 1];
    if platform::read_directory(b".", 0, &mut directory_page).is_err() {
        return false;
    }
    if supplementary_probes_enabled(argument) && (!relative_open_probe() || !relative_spawn_probe())
    {
        return false;
    }
    if syscall::environment_set(b"PWD", b"/").is_ok() {
        return false;
    }
    if platform::chdir(b"..").is_err() {
        return false;
    }
    let Ok(parent_directory) = platform::getcwd(&mut cwd) else {
        return false;
    };
    if parent_directory != b"/" {
        return false;
    }

    if !descriptor_probe()
        || (supplementary_probes_enabled(argument) && !ordinary_descriptor_probe())
        || platform::getppid().is_err()
        || platform::kill(u64::MAX, signal::TERMINATE).err() != Some(platform::Errno::NO_PROCESS)
        || SignalMask::from_bits(signal::bit(signal::KILL)) != Some(SignalMask::EMPTY)
        || syscall::signal_action(signal::KILL, Some(&SignalAction::IGNORE), None).err()
            != Some(syscall::Errno::INVALID_ARGUMENT)
    {
        return false;
    }
    true
}

fn supplementary_probes_enabled(argument: &[u8]) -> bool {
    argument != b"runtime-smoke" && argument != b"manual-argv"
}

fn capability_probe(current_process: u64) -> bool {
    const MESSAGE: &[u8] = b"phase-one-ipc";
    const SHARED_BYTES: &[u8] = b"shared capability memory";
    const SHARED_OFFSET: usize = 7;

    let Ok(endpoint) = ipc::endpoint_create() else {
        return false;
    };
    let Ok(endpoint_info) = ipc::info(endpoint) else {
        return false;
    };
    if endpoint_info.kind != ObjectKind::Endpoint
        || !endpoint_info
            .rights
            .contains(Rights::SEND | Rights::RECEIVE)
        || endpoint_info.size != 0
    {
        return false;
    }

    let Ok(send_only) = ipc::duplicate(endpoint, Rights::SEND) else {
        return false;
    };
    let mut denied_buffer = [0_u8; 1];
    if ipc::try_receive(send_only, &mut denied_buffer).err() != Some(ipc::Error::PERMISSION) {
        return false;
    }

    let Ok(notification) = ipc::notification_create() else {
        return false;
    };
    let Ok(shared_memory) = ipc::shared_memory_create(64) else {
        return false;
    };
    let Ok(read_only_memory) = ipc::duplicate(shared_memory, Rights::READ) else {
        return false;
    };
    if ipc::shared_memory_write(read_only_memory, 0, b"denied").err()
        != Some(ipc::Error::PERMISSION)
    {
        return false;
    }
    if ipc::shared_memory_write(shared_memory, SHARED_OFFSET, SHARED_BYTES).ok()
        != Some(SHARED_BYTES.len())
    {
        return false;
    }
    let mut shared_readback = [0_u8; SHARED_BYTES.len()];
    if ipc::shared_memory_read(read_only_memory, SHARED_OFFSET, &mut shared_readback).ok()
        != Some(SHARED_BYTES.len())
        || shared_readback.as_slice() != SHARED_BYTES
    {
        return false;
    }

    if ipc::send(
        endpoint,
        MESSAGE,
        Some(Transfer {
            handle: notification,
            rights: Rights::WAIT,
        }),
    )
    .is_err()
    {
        return false;
    }

    let mut message_buffer = [0_u8; 32];
    let Ok(message) = ipc::try_receive(endpoint, &mut message_buffer) else {
        return false;
    };
    let Some(received_capability) = message.capability else {
        return false;
    };
    if message.sender_process_id != current_process
        || message.bytes != MESSAGE.len()
        || &message_buffer[..message.bytes] != MESSAGE
        || received_capability.rights != Rights::WAIT
    {
        return false;
    }
    let Ok(received_info) = ipc::info(received_capability.handle) else {
        return false;
    };
    if received_info.kind != ObjectKind::Notification
        || received_info.rights != Rights::WAIT
        || ipc::notification_signal(received_capability.handle, 1).err()
            != Some(ipc::Error::PERMISSION)
    {
        return false;
    }

    if ipc::notification_signal(notification, 2).ok() != Some(2)
        || ipc::notification_try_wait(received_capability.handle).ok() != Some(1)
        || ipc::notification_try_wait(received_capability.handle).ok() != Some(0)
        || ipc::notification_try_wait(received_capability.handle).err()
            != Some(ipc::Error::TRY_AGAIN)
    {
        return false;
    }

    ipc::close(received_capability.handle).is_ok()
        && ipc::close(read_only_memory).is_ok()
        && ipc::close(shared_memory).is_ok()
        && ipc::close(notification).is_ok()
        && ipc::close(send_only).is_ok()
        && ipc::close(endpoint).is_ok()
}

fn job_probe() -> bool {
    let Ok(job) = ipc::job_create() else {
        return false;
    };
    let Ok(wait_only) = ipc::duplicate(job, Rights::WAIT) else {
        let _ = ipc::close(job);
        return false;
    };
    let Ok(info) = ipc::info(job) else {
        let _ = ipc::close(wait_only);
        let _ = ipc::close(job);
        return false;
    };
    if info.kind != ObjectKind::Job
        || info.rights != Rights::JOB
        || info.size != 0
        || ipc::job_try_wait(wait_only).err() != Some(ipc::Error::NO_CHILD)
    {
        let _ = ipc::close(wait_only);
        let _ = ipc::close(job);
        return false;
    }

    let Ok(barrier) = syscall::pipe_pair() else {
        return close_job_handles(job, wait_only, false);
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        return close_job_handles(job, wait_only, false);
    };
    if child == 0 {
        let _ = syscall::close(barrier.writer);
        let mut byte = [0_u8; 1];
        if syscall::read(barrier.reader, &mut byte).ok() != Some(0)
            || syscall::close(barrier.reader).is_err()
        {
            syscall::exit(120);
        }
        match syscall::fork() {
            Ok(0) => syscall::exit(42),
            Ok(descendant) => {
                if syscall::wait_child(descendant)
                    .ok()
                    .map(|status| status.raw())
                    != Some(42)
                {
                    syscall::exit(121);
                }
            }
            Err(_) => syscall::exit(122),
        }
        syscall::exit(23);
    }

    let reader_closed = syscall::close(barrier.reader).is_ok();
    let attenuated_denied = ipc::job_assign(wait_only, child).err() == Some(ipc::Error::PERMISSION);
    let assigned = ipc::job_assign(job, child).ok() == Some(child);
    let member_visible = ipc::info(job).is_ok_and(|info| info.size == 1);
    let barrier_released = syscall::close(barrier.writer).is_ok();
    let setup_ok =
        reader_closed && attenuated_denied && assigned && member_visible && barrier_released;
    if !setup_ok {
        let _ = ipc::job_terminate(job);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_job_handles(job, wait_only, false);
    }

    let Some(first) = bounded_job_wait(wait_only) else {
        let _ = ipc::job_terminate(job);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_job_handles(job, wait_only, false);
    };
    let Some(second) = bounded_job_wait(wait_only) else {
        let _ = ipc::job_terminate(job);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_job_handles(job, wait_only, false);
    };
    let descendant_observed = [first, second]
        .iter()
        .any(|exit| exit.process_id != child && exit.status.raw() == 42);
    let child_observed = [first, second]
        .iter()
        .any(|exit| exit.process_id == child && exit.status.raw() == 23);
    if !descendant_observed
        || !child_observed
        || syscall::wait_child(child).ok().map(|status| status.raw()) != Some(23)
        || ipc::job_try_wait(wait_only).err() != Some(ipc::Error::NO_CHILD)
        || !ipc::info(job).is_ok_and(|info| info.size == 0)
    {
        return close_job_handles(job, wait_only, false);
    }

    let Ok(termination_barrier) = syscall::pipe_pair() else {
        return close_job_handles(job, wait_only, false);
    };
    let Ok(terminated_child) = syscall::fork() else {
        let _ = syscall::close(termination_barrier.reader);
        let _ = syscall::close(termination_barrier.writer);
        return close_job_handles(job, wait_only, false);
    };
    if terminated_child == 0 {
        let _ = syscall::close(termination_barrier.writer);
        let mut byte = [0_u8; 1];
        let _ = syscall::read(termination_barrier.reader, &mut byte);
        syscall::exit(123);
    }
    let termination_reader_closed = syscall::close(termination_barrier.reader).is_ok();
    let termination_assigned =
        ipc::job_assign(job, terminated_child).ok() == Some(terminated_child);
    let attenuated_termination_denied =
        ipc::job_terminate(wait_only).err() == Some(ipc::Error::PERMISSION);
    let termination_count = ipc::job_terminate(job).ok();
    if termination_count != Some(1) {
        let _ = platform::kill(terminated_child, signal::KILL);
    }
    let terminated_exit = termination_assigned
        .then(|| bounded_job_wait(wait_only))
        .flatten();
    let waited_status = syscall::wait_child(terminated_child).ok();
    let _ = syscall::close(termination_barrier.writer);
    let terminated = termination_reader_closed
        && termination_assigned
        && attenuated_termination_denied
        && termination_count == Some(1)
        && terminated_exit.is_some_and(|exit| {
            exit.process_id == terminated_child && exit.status.signal() == Some(signal::KILL)
        })
        && waited_status.is_some_and(|status| status.signal() == Some(signal::KILL));

    close_job_handles(job, wait_only, terminated)
}

fn bounded_job_wait(handle: ipc::CapabilityHandle) -> Option<ipc::JobExit> {
    for _ in 0..JOB_WAIT_YIELDS {
        match ipc::job_try_wait(handle) {
            Ok(exit) => return Some(exit),
            Err(error) if error == ipc::Error::TRY_AGAIN => {
                if syscall::yield_now().is_err() {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    None
}

fn close_job_handles(
    job: ipc::CapabilityHandle,
    wait_only: ipc::CapabilityHandle,
    result: bool,
) -> bool {
    let wait_closed = ipc::close(wait_only).is_ok();
    let job_closed = ipc::close(job).is_ok();
    wait_closed && job_closed && result
}

fn relative_open_probe() -> bool {
    const CONTENTS: &[u8] = b"cwd-aware open\n";
    let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE;
    let Ok(descriptor) = syscall::open(b"cwd-open.txt", flags) else {
        return false;
    };
    let written = syscall::write_all(descriptor, CONTENTS).is_ok();
    let closed = syscall::close(descriptor).is_ok();
    written
        && closed
        && platform::stat(b"/tmp/cwd-open.txt")
            .is_ok_and(|stat| stat.is_file() && stat.size == CONTENTS.len() as u64)
}

fn relative_spawn_probe() -> bool {
    let Ok(process_id) = syscall::spawn_command(
        b"../pwd",
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
    ) else {
        return false;
    };
    syscall::wait_child(process_id).is_ok_and(|status| status.success())
}

fn root_directory_has_expected_entries() -> bool {
    let mut offset = 0usize;
    let mut found_hello = false;
    let mut found_tmp = false;
    loop {
        let mut entries = [DirectoryEntry::EMPTY; DIRECTORY_PAGE];
        let Ok(count) = platform::read_directory(b"/", offset, &mut entries) else {
            return false;
        };
        for entry in &entries[..count] {
            found_hello |= entry.is_file() && ascii_eq_ignore_case(entry.name(), b"hello.txt");
            found_tmp |=
                entry.kind == file::KIND_DIRECTORY && ascii_eq_ignore_case(entry.name(), b"tmp");
        }
        offset = offset.saturating_add(count);
        if count < entries.len() {
            break;
        }
        if offset > 128 {
            return false;
        }
    }
    found_hello && found_tmp
}

fn descriptor_probe() -> bool {
    let Ok(stdout_stat) = platform::fstat(STDOUT) else {
        return false;
    };
    if !matches!(stdout_stat.kind, file::KIND_TERMINAL | file::KIND_FILE) {
        return false;
    }

    match platform::dup(STDOUT) {
        Ok(duplicate) if stdout_stat.kind == file::KIND_FILE => {
            let duplicate_matches = platform::fstat(duplicate)
                .is_ok_and(|stat| stat.kind == stdout_stat.kind && stat.flags == stdout_stat.flags);
            if syscall::close(duplicate).is_err() || !duplicate_matches {
                return false;
            }
        }
        Err(error)
            if stdout_stat.kind == file::KIND_TERMINAL
                && error == platform::Errno::NOT_IMPLEMENTED => {}
        _ => return false,
    }

    if platform::dup2(STDOUT, STDOUT).ok() != Some(STDOUT)
        || platform::dup2(STDOUT, STDERR).ok() != Some(STDERR)
        || platform::dup2(STDOUT, STDIN).err() != Some(platform::Errno::BAD_FILE_DESCRIPTOR)
    {
        return false;
    }
    platform::fstat(STDERR)
        .is_ok_and(|stat| stat.kind == stdout_stat.kind && stat.flags == stdout_stat.flags)
}

fn ordinary_descriptor_probe() -> bool {
    let Ok(descriptor) = syscall::open(b"/hello.txt", OpenFlags::READ) else {
        return false;
    };
    let Ok(stat) = platform::fstat(descriptor) else {
        let _ = syscall::close(descriptor);
        return false;
    };
    if !stat.is_file() {
        let _ = syscall::close(descriptor);
        return false;
    }

    let Ok(duplicate) = platform::dup(descriptor) else {
        let _ = syscall::close(descriptor);
        return false;
    };
    let Ok(reference) = syscall::open(b"/hello.txt", OpenFlags::READ) else {
        let _ = syscall::close(duplicate);
        let _ = syscall::close(descriptor);
        return false;
    };

    let mut first = [0_u8; 1];
    let mut second = [0_u8; 1];
    let mut expected = [0_u8; 2];
    let shared_offset = syscall::read(descriptor, &mut first).ok() == Some(1)
        && syscall::read(duplicate, &mut second).ok() == Some(1)
        && syscall::read(reference, &mut expected).ok() == Some(2)
        && first[0] == expected[0]
        && second[0] == expected[1];

    const DUP2_TARGET: u64 = 15;
    let dup2_ok = platform::dup2(descriptor, DUP2_TARGET).ok() == Some(DUP2_TARGET)
        && platform::fstat(DUP2_TARGET).is_ok();

    let _ = syscall::close(DUP2_TARGET);
    let _ = syscall::close(reference);
    let _ = syscall::close(duplicate);
    let _ = syscall::close(descriptor);
    shared_offset && dup2_ok
}

fn ascii_eq_ignore_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}
