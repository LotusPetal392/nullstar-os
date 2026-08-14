#![no_std]
#![no_main]

use userspace::{
    abi::{capability, file, limits, signal},
    args::Args,
    blocking_ipc,
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

    let Ok(replacement_source) = ipc::duplicate(endpoint, Rights::ENDPOINT) else {
        return false;
    };
    if ipc::replace(replacement_source, Rights::EMPTY).err() != Some(ipc::Error::PERMISSION) {
        return false;
    }
    let Ok(send_only) = ipc::replace(replacement_source, Rights::SEND) else {
        return false;
    };
    let Ok(send_only_info) = ipc::info(send_only) else {
        return false;
    };
    let mut denied_buffer = [0_u8; 1];
    if send_only_info.object_id != endpoint_info.object_id
        || send_only_info.kind != ObjectKind::Endpoint
        || send_only_info.rights != Rights::SEND
        || ipc::try_receive(send_only, &mut denied_buffer).err() != Some(ipc::Error::PERMISSION)
    {
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
        && endpoint_move_transfer_probe(current_process)
}

fn endpoint_move_transfer_probe(current_process: u64) -> bool {
    const MOVED_MESSAGE: &[u8] = b"move-capability";

    let Ok(endpoint) = ipc::endpoint_create() else {
        return false;
    };
    let Ok(notification) = ipc::notification_create() else {
        return false;
    };
    let Ok(source_info) = ipc::info(notification) else {
        return false;
    };
    for _ in 0..limits::MAX_ENDPOINT_MESSAGES {
        if ipc::send(endpoint, &[], None).is_err() {
            return false;
        }
    }
    let transfer = Transfer {
        handle: notification,
        rights: Rights::NOTIFICATION,
    };
    if ipc::send_move(endpoint, MOVED_MESSAGE, transfer).err() != Some(ipc::Error::TRY_AGAIN)
        || !ipc::info(notification).is_ok_and(|info| info == source_info)
    {
        return false;
    }

    let mut buffer = [0_u8; MOVED_MESSAGE.len()];
    for _ in 0..limits::MAX_ENDPOINT_MESSAGES {
        let Ok(message) = ipc::try_receive(endpoint, &mut buffer) else {
            return false;
        };
        if message.bytes != 0 || message.capability.is_some() {
            return false;
        }
    }
    if ipc::send_move(endpoint, MOVED_MESSAGE, transfer).is_err()
        || ipc::info(notification).err() != Some(ipc::Error::BAD_FILE_DESCRIPTOR)
    {
        return false;
    }
    let Ok(message) = ipc::try_receive(endpoint, &mut buffer) else {
        return false;
    };
    let Some(received) = message.capability else {
        return false;
    };
    let moved = message.sender_process_id == current_process
        && message.bytes == MOVED_MESSAGE.len()
        && buffer.as_slice() == MOVED_MESSAGE
        && received.rights == Rights::NOTIFICATION
        && ipc::info(received.handle).is_ok_and(|info| {
            info.object_id == source_info.object_id
                && info.kind == ObjectKind::Notification
                && info.rights == Rights::NOTIFICATION
        })
        && ipc::notification_signal(received.handle, 1).ok() == Some(1)
        && ipc::notification_try_wait(received.handle).ok() == Some(0);

    ipc::close(received.handle).is_ok()
        && ipc::close(endpoint).is_ok()
        && moved
        && endpoint_move_waiter_probe(current_process)
}

fn endpoint_move_waiter_probe(current_process: u64) -> bool {
    const ENDPOINT_HANDLE: ipc::CapabilityHandle = 1;
    const MESSAGE: &[u8] = b"move-wakes-waiter";

    let Ok(endpoint) = ipc::endpoint_create() else {
        return false;
    };
    let Ok(notification) = ipc::notification_create() else {
        let _ = ipc::close(endpoint);
        return false;
    };
    let Ok(barrier) = syscall::pipe_pair() else {
        let _ = ipc::close(notification);
        let _ = ipc::close(endpoint);
        return false;
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        let _ = ipc::close(notification);
        let _ = ipc::close(endpoint);
        return false;
    };
    if child == 0 {
        let _ = syscall::close(barrier.reader);
        if !ipc::wait_for_handle(ENDPOINT_HANDLE)
            .is_ok_and(|info| info.kind == ObjectKind::Endpoint && info.rights == Rights::RECEIVE)
            || syscall::write_all(barrier.writer, &[1]).is_err()
            || syscall::close(barrier.writer).is_err()
        {
            syscall::exit(70);
        }
        let mut bytes = [0_u8; MESSAGE.len()];
        let Ok(message) = blocking_ipc::receive(ENDPOINT_HANDLE, &mut bytes) else {
            syscall::exit(71);
        };
        let Some(received) = message.capability else {
            syscall::exit(72);
        };
        let valid = message.sender_process_id == current_process
            && message.bytes == MESSAGE.len()
            && bytes.as_slice() == MESSAGE
            && received.rights == Rights::WAIT
            && ipc::info(received.handle).is_ok_and(|info| {
                info.kind == ObjectKind::Notification && info.rights == Rights::WAIT
            });
        let closed = ipc::close(received.handle).is_ok() && ipc::close(ENDPOINT_HANDLE).is_ok();
        syscall::exit(if valid && closed { 0 } else { 73 });
    }

    let setup = syscall::close(barrier.writer).is_ok()
        && ipc::grant_child(child, endpoint, Rights::RECEIVE, ENDPOINT_HANDLE).ok()
            == Some(ENDPOINT_HANDLE);
    let mut ready = [0_u8; 1];
    let synchronized = setup
        && syscall::read(barrier.reader, &mut ready).ok() == Some(1)
        && ready[0] == 1
        && syscall::close(barrier.reader).is_ok();
    if synchronized {
        for _ in 0..4 {
            let _ = syscall::yield_now();
        }
    }
    let sent = synchronized
        && ipc::send_move(
            endpoint,
            MESSAGE,
            Transfer {
                handle: notification,
                rights: Rights::WAIT,
            },
        )
        .is_ok()
        && ipc::info(notification).err() == Some(ipc::Error::BAD_FILE_DESCRIPTOR);

    let mut child_succeeded = false;
    if sent {
        for _ in 0..JOB_WAIT_YIELDS {
            match syscall::try_wait_child(child) {
                Ok(status) => {
                    child_succeeded = status.success();
                    break;
                }
                Err(error) if error == syscall::Errno::TRY_AGAIN => {
                    let _ = syscall::yield_now();
                }
                Err(_) => break,
            }
        }
    }
    if !child_succeeded {
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
    }
    if !sent {
        let _ = ipc::close(notification);
    }
    let _ = syscall::close(barrier.reader);
    let _ = syscall::close(barrier.writer);
    ipc::close(endpoint).is_ok() && sent && child_succeeded
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
        || ipc::job_get_process_limit(job).ok() != Some(limits::MAX_JOB_PROCESSES)
        || ipc::job_get_process_limit(wait_only).ok() != Some(limits::MAX_JOB_PROCESSES)
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

    let hierarchy_verified = terminated && hierarchical_job_probe(job, wait_only);
    close_job_handles(job, wait_only, hierarchy_verified)
}

fn hierarchical_job_probe(
    parent: ipc::CapabilityHandle,
    parent_wait: ipc::CapabilityHandle,
) -> bool {
    let Ok(manage_only) = ipc::duplicate(parent, Rights::MANAGE) else {
        return false;
    };
    let query_rights_verified = ipc::job_get_process_limit(manage_only).err()
        == Some(ipc::Error::PERMISSION)
        && ipc::close(manage_only).is_ok();
    if ipc::job_retire(parent).err() != Some(ipc::Error::INVALID_ARGUMENT)
        || ipc::job_retire(parent_wait).err() != Some(ipc::Error::PERMISSION)
        || ipc::job_create_child(parent_wait).err() != Some(ipc::Error::PERMISSION)
        || !query_rights_verified
    {
        return false;
    }
    let Ok(child_job) = ipc::job_create_child(parent) else {
        return false;
    };
    let Ok(grandchild_job) = ipc::job_create_child(child_job) else {
        let _ = ipc::close(child_job);
        return false;
    };
    if ipc::job_retire(child_job).err() != Some(ipc::Error::TRY_AGAIN) {
        return close_hierarchy_handles(child_job, grandchild_job, false);
    }
    let hierarchy_shape = ipc::info(child_job).is_ok_and(|child| {
        ipc::info(grandchild_job).is_ok_and(|grandchild| {
            child.kind == ObjectKind::Job
                && child.rights == Rights::JOB
                && child.size == 0
                && grandchild.kind == ObjectKind::Job
                && grandchild.rights == Rights::JOB
                && grandchild.size == 0
                && child.object_id != grandchild.object_id
        })
    });
    if !hierarchy_shape || ipc::job_try_wait(parent_wait).err() != Some(ipc::Error::NO_CHILD) {
        return close_hierarchy_handles(child_job, grandchild_job, false);
    }

    let Ok(barrier) = syscall::pipe_pair() else {
        return close_hierarchy_handles(child_job, grandchild_job, false);
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        return close_hierarchy_handles(child_job, grandchild_job, false);
    };
    if child == 0 {
        let _ = syscall::close(barrier.writer);
        let mut byte = [0_u8; 1];
        if syscall::read(barrier.reader, &mut byte).ok() != Some(0)
            || syscall::close(barrier.reader).is_err()
        {
            syscall::exit(124);
        }
        match syscall::fork() {
            Ok(0) => syscall::exit(44),
            Ok(descendant) => {
                if syscall::wait_child(descendant)
                    .ok()
                    .map(|status| status.raw())
                    != Some(44)
                {
                    syscall::exit(125);
                }
            }
            Err(_) => syscall::exit(126),
        }
        syscall::exit(24);
    }

    let reader_closed = syscall::close(barrier.reader).is_ok();
    let assigned = ipc::job_assign(grandchild_job, child).ok() == Some(child);
    let subtree_visible = [parent, child_job, grandchild_job]
        .iter()
        .all(|job| ipc::info(*job).is_ok_and(|info| info.size == 1));
    let barrier_released = syscall::close(barrier.writer).is_ok();
    if !reader_closed || !assigned || !subtree_visible || !barrier_released {
        let _ = ipc::job_terminate(parent);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_hierarchy_handles(child_job, grandchild_job, false);
    }

    let Some(first) = bounded_job_wait(parent_wait) else {
        let _ = ipc::job_terminate(parent);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_hierarchy_handles(child_job, grandchild_job, false);
    };
    let Some(second) = bounded_job_wait(parent_wait) else {
        let _ = ipc::job_terminate(parent);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_hierarchy_handles(child_job, grandchild_job, false);
    };
    let descendant_observed = [first, second]
        .iter()
        .any(|exit| exit.process_id != child && exit.status.raw() == 44);
    let child_observed = [first, second]
        .iter()
        .any(|exit| exit.process_id == child && exit.status.raw() == 24);
    if !descendant_observed
        || !child_observed
        || syscall::wait_child(child).ok().map(|status| status.raw()) != Some(24)
        || ipc::job_try_wait(grandchild_job).err() != Some(ipc::Error::NO_CHILD)
        || ![parent, child_job, grandchild_job]
            .iter()
            .all(|job| ipc::info(*job).is_ok_and(|info| info.size == 0))
    {
        return close_hierarchy_handles(child_job, grandchild_job, false);
    }

    let Ok(termination_barrier) = syscall::pipe_pair() else {
        return close_hierarchy_handles(child_job, grandchild_job, false);
    };
    let Ok(terminated_child) = syscall::fork() else {
        let _ = syscall::close(termination_barrier.reader);
        let _ = syscall::close(termination_barrier.writer);
        return close_hierarchy_handles(child_job, grandchild_job, false);
    };
    if terminated_child == 0 {
        let _ = syscall::close(termination_barrier.writer);
        let mut byte = [0_u8; 1];
        let _ = syscall::read(termination_barrier.reader, &mut byte);
        syscall::exit(127);
    }
    let termination_reader_closed = syscall::close(termination_barrier.reader).is_ok();
    let termination_assigned =
        ipc::job_assign(child_job, terminated_child).ok() == Some(terminated_child);
    let termination_count = ipc::job_terminate(parent).ok();
    if termination_count != Some(1) {
        let _ = platform::kill(terminated_child, signal::KILL);
    }
    let terminated_exit = termination_assigned
        .then(|| bounded_job_wait(parent_wait))
        .flatten();
    let waited_status = syscall::wait_child(terminated_child).ok();
    let _ = syscall::close(termination_barrier.writer);
    let terminated = termination_reader_closed
        && termination_assigned
        && termination_count == Some(1)
        && terminated_exit.is_some_and(|exit| {
            exit.process_id == terminated_child && exit.status.signal() == Some(signal::KILL)
        })
        && waited_status.is_some_and(|status| status.signal() == Some(signal::KILL))
        && ipc::job_try_wait(parent_wait).err() == Some(ipc::Error::NO_CHILD);

    let process_limit_verified = terminated && job_process_limit_probe(parent, parent_wait);
    let retired = retire_and_close_hierarchy(child_job, grandchild_job, process_limit_verified);
    retired && job_reclamation_probe(parent)
}

fn job_process_limit_probe(
    parent: ipc::CapabilityHandle,
    parent_wait: ipc::CapabilityHandle,
) -> bool {
    if ipc::job_set_process_limit(parent_wait, 1).err() != Some(ipc::Error::PERMISSION) {
        return false;
    }
    let Ok(limited_job) = ipc::job_create_child(parent) else {
        return false;
    };
    if ipc::job_set_process_limit(limited_job, 1).ok() != Some(1) {
        let _ = ipc::close(limited_job);
        return false;
    }
    let Ok(leaf_job) = ipc::job_create_child(limited_job) else {
        let _ = ipc::close(limited_job);
        return false;
    };
    if ipc::job_get_process_limit(limited_job).ok() != Some(1)
        || ipc::job_get_process_limit(leaf_job).ok() != Some(1)
        || ipc::job_retire(limited_job).err() != Some(ipc::Error::TRY_AGAIN)
        || ipc::job_set_process_limit(leaf_job, 2).err() != Some(ipc::Error::PERMISSION)
    {
        return close_hierarchy_handles(limited_job, leaf_job, false);
    }

    let Ok(barrier) = syscall::pipe_pair() else {
        return close_hierarchy_handles(limited_job, leaf_job, false);
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        return close_hierarchy_handles(limited_job, leaf_job, false);
    };
    if child == 0 {
        let _ = syscall::close(barrier.writer);
        let mut byte = [0_u8; 1];
        if syscall::read(barrier.reader, &mut byte).ok() != Some(0)
            || syscall::close(barrier.reader).is_err()
        {
            syscall::exit(128);
        }
        match syscall::fork() {
            Err(error) if error == syscall::Errno::NO_SPACE => syscall::exit(46),
            Ok(0) => syscall::exit(129),
            Ok(descendant) => {
                let _ = platform::kill(descendant, signal::KILL);
                let _ = syscall::wait_child(descendant);
                syscall::exit(130);
            }
            Err(_) => syscall::exit(131),
        }
    }

    let reader_closed = syscall::close(barrier.reader).is_ok();
    let assigned = ipc::job_assign(leaf_job, child).ok() == Some(child);
    let tightened_below_usage = ipc::job_set_process_limit(limited_job, 0).ok() == Some(0);
    let relaxation_denied =
        ipc::job_set_process_limit(limited_job, 1).err() == Some(ipc::Error::PERMISSION);
    let subtree_visible = [parent, limited_job, leaf_job]
        .iter()
        .all(|job| ipc::info(*job).is_ok_and(|info| info.size == 1));
    let barrier_released = syscall::close(barrier.writer).is_ok();
    if !reader_closed
        || !assigned
        || !tightened_below_usage
        || !relaxation_denied
        || !subtree_visible
        || !barrier_released
    {
        let _ = ipc::job_terminate(parent);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_hierarchy_handles(limited_job, leaf_job, false);
    }

    let exit = bounded_job_wait(parent_wait);
    let waited_status = syscall::wait_child(child).ok();
    let denied = exit.is_some_and(|exit| exit.process_id == child && exit.status.raw() == 46)
        && waited_status.is_some_and(|status| status.raw() == 46)
        && ipc::job_try_wait(parent_wait).err() == Some(ipc::Error::NO_CHILD)
        && [parent, limited_job, leaf_job]
            .iter()
            .all(|job| ipc::info(*job).is_ok_and(|info| info.size == 0));

    let limits_visible = ipc::job_get_process_limit(limited_job).ok() == Some(0)
        && ipc::job_get_process_limit(leaf_job).ok() == Some(1);

    retire_and_close_hierarchy(limited_job, leaf_job, denied && limits_visible)
}

fn retire_and_close_hierarchy(
    parent: ipc::CapabilityHandle,
    child: ipc::CapabilityHandle,
    result: bool,
) -> bool {
    if !result || ipc::job_retire(child).is_err() {
        return close_hierarchy_handles(parent, child, false);
    }
    let child_is_inert = ipc::job_retire(child).err() == Some(ipc::Error::PERMISSION)
        && ipc::job_create_child(child).err() == Some(ipc::Error::PERMISSION)
        && ipc::job_set_process_limit(child, 0).err() == Some(ipc::Error::PERMISSION)
        && ipc::job_get_process_limit(child).is_ok()
        && ipc::job_try_wait(child).err() == Some(ipc::Error::NO_CHILD);
    let parent_retired = child_is_inert && ipc::job_retire(parent).is_ok();
    close_hierarchy_handles(parent, child, parent_retired)
}

fn job_reclamation_probe(parent: ipc::CapabilityHandle) -> bool {
    for _ in 0..=limits::MAX_JOB_OBJECTS {
        let Ok(child) = ipc::job_create_child(parent) else {
            return false;
        };
        if ipc::job_retire(child).is_err() || ipc::close(child).is_err() {
            return false;
        }
    }
    ipc::info(parent).is_ok_and(|info| info.size == 0)
}

fn close_hierarchy_handles(
    child: ipc::CapabilityHandle,
    grandchild: ipc::CapabilityHandle,
    result: bool,
) -> bool {
    let grandchild_closed = ipc::close(grandchild).is_ok();
    let child_closed = ipc::close(child).is_ok();
    grandchild_closed && child_closed && result
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
