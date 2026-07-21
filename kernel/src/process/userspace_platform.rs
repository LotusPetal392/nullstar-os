// Additional syscall entry and handlers for the documented userspace platform
// ABI. This file is included after `userspace.rs` so it deliberately shares the
// process manager's private invariants instead of duplicating them.

const PLATFORM_PAGE_SIZE: u64 = Size4KiB::SIZE;
const PLATFORM_WORKING_DIRECTORY_NAME: &str = "PWD";
const PLATFORM_WORKING_DIRECTORY_PREFIX: &str = "PWD=";

global_asm!(
    r#"
    .section .text
    .p2align 4
    .global galactic_platform_syscall_interrupt_entry
    .type galactic_platform_syscall_interrupt_entry,@function
galactic_platform_syscall_interrupt_entry:
    cld
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, rsp
    and rsp, -16
    call galactic_platform_syscall_dispatch
    mov rsp, rax

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    iretq
.size galactic_platform_syscall_interrupt_entry, .-galactic_platform_syscall_interrupt_entry
"#
);

unsafe extern "C" {
    fn galactic_platform_syscall_interrupt_entry();
}

pub fn platform_syscall_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(galactic_platform_syscall_interrupt_entry as *const () as usize as u64)
}

#[unsafe(no_mangle)]
pub extern "C" fn galactic_platform_syscall_dispatch(current_stack_pointer: usize) -> usize {
    let registers_pointer = current_stack_pointer as *mut SavedRegisters;
    let syscall_number = unsafe { (*registers_pointer).rax };

    if !platform_syscall_number(syscall_number)
        && !platform_reserved_environment_call(syscall_number, registers_pointer)
    {
        return galactic_syscall_dispatch(current_stack_pointer);
    }

    let Some(process_id) = scheduler::current_process_id() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };

    {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            unsafe { (*registers_pointer).rax = error_return(ERR_NO_PROCESS) };
            return current_stack_pointer;
        };
        process.syscall_count = process.syscall_count.saturating_add(1);
    }

    let registers = unsafe { &mut *registers_pointer };
    registers.rax = match syscall_number {
        abi::syscall::SYSTEM_INFO => {
            platform_system_info(process_id, registers.rdi, registers.rsi)
        }
        abi::syscall::STAT => platform_stat(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
        ),
        abi::syscall::FSTAT => {
            platform_fstat(process_id, registers.rdi, registers.rsi, registers.rdx)
        }
        abi::syscall::READ_DIRECTORY => platform_read_directory(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
            registers.r8,
        ),
        abi::syscall::CHDIR => platform_chdir(process_id, registers.rdi, registers.rsi),
        abi::syscall::GETCWD => platform_getcwd(process_id, registers.rdi, registers.rsi),
        abi::syscall::DUP => platform_dup(process_id, registers.rdi),
        abi::syscall::DUP2 => platform_dup2(process_id, registers.rdi, registers.rsi),
        abi::syscall::GETPPID => platform_getppid(process_id),
        abi::syscall::KILL => platform_kill(process_id, registers.rdi, registers.rsi),
        abi::syscall::ENVIRONMENT_SET | abi::syscall::ENVIRONMENT_UNSET => {
            error_return(ERR_INVALID_ARGUMENT)
        }
        _ => error_return(ERR_NOT_IMPLEMENTED),
    };
    current_stack_pointer
}

fn platform_syscall_number(number: u64) -> bool {
    matches!(
        number,
        abi::syscall::SYSTEM_INFO
            | abi::syscall::STAT
            | abi::syscall::FSTAT
            | abi::syscall::READ_DIRECTORY
            | abi::syscall::CHDIR
            | abi::syscall::GETCWD
            | abi::syscall::DUP
            | abi::syscall::DUP2
            | abi::syscall::GETPPID
            | abi::syscall::KILL
    )
}

fn platform_reserved_environment_call(
    syscall_number: u64,
    registers_pointer: *const SavedRegisters,
) -> bool {
    if !matches!(
        syscall_number,
        abi::syscall::ENVIRONMENT_SET | abi::syscall::ENVIRONMENT_UNSET
    ) {
        return false;
    }
    let Some(process_id) = scheduler::current_process_id() else {
        return false;
    };
    let registers = unsafe { &*registers_pointer };
    user_text(
        process_id,
        registers.rdi,
        registers.rsi,
        MAX_ENVIRONMENT_NAME_BYTES,
    )
    .is_ok_and(|name| name == PLATFORM_WORKING_DIRECTORY_NAME)
}

fn platform_system_info(process_id: u64, address: u64, length: u64) -> u64 {
    let info = abi::SystemInfo {
        abi_major: abi::ABI_VERSION_MAJOR,
        abi_minor: abi::ABI_VERSION_MINOR,
        capabilities: abi::capability::PLATFORM_V1,
        page_size: PLATFORM_PAGE_SIZE,
        maximum_open_files: MAX_OPEN_FILES as u64,
        maximum_path_bytes: abi::limits::MAX_PATH_BYTES as u64,
        maximum_directory_entries: abi::limits::MAX_DIRECTORY_ENTRIES_PER_CALL as u64,
        init_process_id: INIT_PROCESS_ID,
    };
    platform_write_value(process_id, address, length, info)
}

fn platform_stat(
    process_id: u64,
    path_address: u64,
    path_length: u64,
    stat_address: u64,
    stat_length: u64,
) -> u64 {
    let path = match platform_user_path(process_id, path_address, path_length) {
        Ok(path) => path,
        Err(error) => return error_return(error),
    };
    let metadata = match vfs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => return error_return(platform_vfs_errno(&error)),
    };
    platform_write_value(
        process_id,
        stat_address,
        stat_length,
        platform_stat_from_metadata(&metadata),
    )
}

fn platform_fstat(
    process_id: u64,
    descriptor: u64,
    stat_address: u64,
    stat_length: u64,
) -> u64 {
    let source = {
        let manager = PROCESS_MANAGER.lock();
        let Some(process) = manager
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
        else {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        };
        match platform_descriptor_source(process, descriptor) {
            Some(source) => source,
            None => return error_return(ERR_BAD_FILE_DESCRIPTOR),
        }
    };

    let stat = match source {
        PlatformDescriptorSource::TerminalRead | PlatformDescriptorSource::TerminalWrite => {
            abi::file::Stat {
                kind: abi::file::KIND_TERMINAL,
                size: 0,
                flags: 0,
            }
        }
        PlatformDescriptorSource::Pipe(_, _) => abi::file::Stat {
            kind: abi::file::KIND_PIPE,
            size: 0,
            flags: 0,
        },
        PlatformDescriptorSource::File(handle) => {
            let path = handle.lock().path.clone();
            let metadata = match vfs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => return error_return(platform_vfs_errno(&error)),
            };
            platform_stat_from_metadata(&metadata)
        }
    };
    platform_write_value(process_id, stat_address, stat_length, stat)
}

fn platform_read_directory(
    process_id: u64,
    path_address: u64,
    path_length: u64,
    start_index: u64,
    records_address: u64,
    capacity: u64,
) -> u64 {
    let capacity = match usize::try_from(capacity) {
        Ok(capacity) if capacity <= abi::limits::MAX_DIRECTORY_ENTRIES_PER_CALL => capacity,
        _ => return error_return(ERR_INVALID_ARGUMENT),
    };
    if capacity == 0 {
        return 0;
    }
    let byte_length = match capacity.checked_mul(size_of::<abi::file::DirectoryEntry>()) {
        Some(length) => length,
        None => return error_return(ERR_ARGUMENT_TOO_LARGE),
    };
    if !user_range_allows(process_id, records_address, byte_length, true) {
        return error_return(ERR_BAD_ADDRESS);
    }
    let start_index = match usize::try_from(start_index) {
        Ok(index) => index,
        Err(_) => return error_return(ERR_INVALID_ARGUMENT),
    };
    let path = match platform_user_path(process_id, path_address, path_length) {
        Ok(path) => path,
        Err(error) => return error_return(error),
    };
    let entries = match vfs::read_directory(&path) {
        Ok(entries) => entries,
        Err(error) => return error_return(platform_vfs_errno(&error)),
    };

    let mut written = 0usize;
    for entry in entries.iter().skip(start_index).take(capacity) {
        let record = match platform_directory_record(entry) {
            Ok(record) => record,
            Err(error) => return error_return(error),
        };
        let destination =
            (records_address as *mut abi::file::DirectoryEntry).wrapping_add(written);
        unsafe { ptr::write_unaligned(destination, record) };
        written = written.saturating_add(1);
    }
    written as u64
}

fn platform_chdir(process_id: u64, path_address: u64, path_length: u64) -> u64 {
    let path = match platform_user_path(process_id, path_address, path_length) {
        Ok(path) => path,
        Err(error) => return error_return(error),
    };
    let metadata = match vfs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => return error_return(platform_vfs_errno(&error)),
    };
    if !metadata.is_directory() {
        return error_return(abi::errno::NOT_DIRECTORY);
    }
    match platform_set_working_directory(process_id, &metadata.path) {
        Ok(()) => 0,
        Err(error) => error_return(error),
    }
}

fn platform_getcwd(process_id: u64, address: u64, capacity: u64) -> u64 {
    let directory = platform_working_directory(process_id);
    let required = match directory.len().checked_add(1) {
        Some(required) => required,
        None => return error_return(abi::errno::RANGE),
    };
    let capacity = match usize::try_from(capacity) {
        Ok(capacity) => capacity,
        Err(_) => return error_return(abi::errno::RANGE),
    };
    if capacity < required {
        return error_return(abi::errno::RANGE);
    }
    if !user_range_allows(process_id, address, required, true) {
        return error_return(ERR_BAD_ADDRESS);
    }
    unsafe {
        ptr::copy_nonoverlapping(directory.as_ptr(), address as *mut u8, directory.len());
        (address as *mut u8)
            .wrapping_add(directory.len())
            .write(0);
    }
    directory.len() as u64
}

fn platform_dup(process_id: u64, descriptor: u64) -> u64 {
    let (source, new_descriptor) = {
        let manager = PROCESS_MANAGER.lock();
        let Some(process) = manager
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
        else {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        };
        let Some(source) = platform_descriptor_source(process, descriptor) else {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        };
        if matches!(
            &source,
            PlatformDescriptorSource::TerminalRead | PlatformDescriptorSource::TerminalWrite
        ) {
            return error_return(ERR_NOT_IMPLEMENTED);
        }
        if descriptor_count(process) >= MAX_OPEN_FILES {
            return error_return(ERR_TOO_MANY_OPEN_FILES);
        }
        let Some(new_descriptor) = allocate_descriptor(process) else {
            return error_return(ERR_TOO_MANY_OPEN_FILES);
        };
        (source, new_descriptor)
    };

    match platform_install_descriptor(process_id, source, new_descriptor, false) {
        Ok(()) => new_descriptor,
        Err(error) => error_return(error),
    }
}

fn platform_dup2(process_id: u64, descriptor: u64, target_descriptor: u64) -> u64 {
    let source = {
        let manager = PROCESS_MANAGER.lock();
        let Some(process) = manager
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
        else {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        };
        let Some(source) = platform_descriptor_source(process, descriptor) else {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        };
        source
    };
    if descriptor == target_descriptor {
        return target_descriptor;
    }

    let result = if target_descriptor < 3 {
        platform_install_stream(process_id, source, target_descriptor)
    } else if target_descriptor < 3 + MAX_OPEN_FILES as u64 {
        platform_install_descriptor(process_id, source, target_descriptor, true)
    } else {
        Err(ERR_BAD_FILE_DESCRIPTOR)
    };
    match result {
        Ok(()) => target_descriptor,
        Err(error) => error_return(error),
    }
}

fn platform_getppid(process_id: u64) -> u64 {
    PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == process_id)
        .and_then(|process| process.parent_process_id)
        .unwrap_or(KERNEL_REAPER_PROCESS_ID)
}

fn platform_kill(process_id: u64, target_process_id: u64, signal: u64) -> u64 {
    if !signal_is_supported(signal) {
        return error_return(ERR_INVALID_ARGUMENT);
    }
    let (authorized, process_group_id) = {
        let manager = PROCESS_MANAGER.lock();
        let Some(target) = manager
            .processes
            .iter()
            .find(|process| process.process_id == target_process_id && process.is_live())
        else {
            return error_return(ERR_NO_PROCESS);
        };
        (
            target.parent_process_id == Some(process_id),
            target.process_group_id,
        )
    };
    if !authorized {
        return error_return(abi::errno::PERMISSION);
    }

    let delivery = deliver_signal_to_process(target_process_id, signal);
    if !delivery.accepted {
        return error_return(ERR_NO_PROCESS);
    }
    {
        let mut manager = PROCESS_MANAGER.lock();
        if let Some(sender) = manager.process_mut(process_id) {
            sender.signal_sent_count = sender.signal_sent_count.saturating_add(1);
        }
        manager.signals_sent = manager.signals_sent.saturating_add(1);
    }
    if delivery.stopped {
        restore_group_terminal(process_group_id);
    }
    0
}

fn platform_user_path(process_id: u64, address: u64, length: u64) -> Result<String, i64> {
    let path = user_text(process_id, address, length, abi::limits::MAX_PATH_BYTES)?;
    if path.starts_with('/') {
        return Ok(path);
    }

    let directory = platform_working_directory(process_id);
    let separator = usize::from(directory != "/");
    let required = directory
        .len()
        .checked_add(separator)
        .and_then(|length| length.checked_add(path.len()))
        .ok_or(abi::errno::NAME_TOO_LONG)?;
    if required > abi::limits::MAX_PATH_BYTES {
        return Err(abi::errno::NAME_TOO_LONG);
    }

    let mut resolved = String::with_capacity(required);
    resolved.push_str(&directory);
    if separator != 0 {
        resolved.push('/');
    }
    resolved.push_str(&path);
    Ok(resolved)
}

fn platform_working_directory(process_id: u64) -> String {
    let candidate = {
        let manager = PROCESS_MANAGER.lock();
        manager
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
            .and_then(|process| {
                process.environment.iter().find_map(|entry| {
                    entry
                        .strip_prefix(PLATFORM_WORKING_DIRECTORY_PREFIX)
                        .map(String::from)
                })
            })
    };
    let Some(candidate) = candidate else {
        return String::from("/");
    };
    if !candidate.starts_with('/') {
        return String::from("/");
    }
    match vfs::metadata(&candidate) {
        Ok(metadata) if metadata.is_directory() => metadata.path,
        _ => String::from("/"),
    }
}

fn platform_set_working_directory(process_id: u64, directory: &str) -> Result<(), i64> {
    let mut entry = String::from(PLATFORM_WORKING_DIRECTORY_PREFIX);
    entry.push_str(directory);
    let entry_bytes = entry
        .len()
        .checked_add(1)
        .ok_or(ERR_ARGUMENT_TOO_LARGE)?;

    let mut manager = PROCESS_MANAGER.lock();
    let changed = {
        let process = manager.process_mut(process_id).ok_or(ERR_NO_PROCESS)?;
        let existing = process.environment.iter().position(|candidate| {
            environment_name(candidate) == Some(PLATFORM_WORKING_DIRECTORY_NAME)
        });
        if existing.is_none() && process.environment.len() >= MAX_ENVIRONMENT_VARIABLES {
            return Err(ERR_NO_SPACE);
        }
        let current_bytes =
            environment_serialized_bytes(&process.environment).ok_or(ERR_ARGUMENT_TOO_LARGE)?;
        let previous_bytes = existing
            .map(|index| process.environment[index].len().saturating_add(1))
            .unwrap_or(0);
        let total_bytes = current_bytes
            .saturating_sub(previous_bytes)
            .saturating_add(entry_bytes);
        if total_bytes > MAX_ENVIRONMENT_BYTES {
            return Err(ERR_ARGUMENT_TOO_LARGE);
        }
        match existing {
            Some(index) if process.environment[index] == entry => false,
            Some(index) => {
                process.environment[index] = entry;
                true
            }
            None => {
                process.environment.push(entry);
                true
            }
        }
    };
    if changed {
        let process = manager
            .process_mut(process_id)
            .expect("working-directory process disappeared during update");
        process.environment_change_count = process.environment_change_count.saturating_add(1);
        manager.environment_changes = manager.environment_changes.saturating_add(1);
    }
    Ok(())
}

fn platform_write_value<T: Copy>(
    process_id: u64,
    address: u64,
    length: u64,
    value: T,
) -> u64 {
    let required = size_of::<T>();
    let length = match usize::try_from(length) {
        Ok(length) => length,
        Err(_) => return error_return(abi::errno::RANGE),
    };
    if length < required {
        return error_return(abi::errno::RANGE);
    }
    if !user_range_allows(process_id, address, required, true) {
        return error_return(ERR_BAD_ADDRESS);
    }
    unsafe { ptr::write_unaligned(address as *mut T, value) };
    0
}

fn platform_stat_from_metadata(metadata: &vfs::Metadata) -> abi::file::Stat {
    abi::file::Stat {
        kind: if metadata.is_directory() {
            abi::file::KIND_DIRECTORY
        } else {
            abi::file::KIND_FILE
        },
        size: metadata.size,
        flags: platform_metadata_flags(
            metadata.read_only,
            metadata.hidden,
            metadata.system,
        ),
    }
}

fn platform_directory_record(
    entry: &vfs::DirectoryEntry,
) -> Result<abi::file::DirectoryEntry, i64> {
    let name = entry.name.as_bytes();
    if name.len() > abi::file::MAX_DIRECTORY_ENTRY_NAME_BYTES {
        return Err(abi::errno::NAME_TOO_LONG);
    }
    let mut record = abi::file::DirectoryEntry {
        kind: if entry.is_directory() {
            abi::file::KIND_DIRECTORY
        } else {
            abi::file::KIND_FILE
        },
        size: entry.size,
        flags: platform_metadata_flags(entry.read_only, entry.hidden, entry.system),
        name_length: name.len() as u64,
        name: [0; abi::file::DIRECTORY_ENTRY_NAME_CAPACITY],
    };
    record.name[..name.len()].copy_from_slice(name);
    Ok(record)
}

fn platform_metadata_flags(read_only: bool, hidden: bool, system: bool) -> u64 {
    u64::from(read_only) * abi::file::FLAG_READ_ONLY
        | u64::from(hidden) * abi::file::FLAG_HIDDEN
        | u64::from(system) * abi::file::FLAG_SYSTEM
}

#[derive(Clone)]
enum PlatformDescriptorSource {
    TerminalRead,
    TerminalWrite,
    File(OpenFileHandle),
    Pipe(PipeId, PipeDirection),
}

fn platform_descriptor_source(
    process: &Process,
    descriptor: u64,
) -> Option<PlatformDescriptorSource> {
    match descriptor {
        0 => match &process.stdin_target {
            Some(StreamTarget::File(handle)) => {
                Some(PlatformDescriptorSource::File(handle.clone()))
            }
            Some(StreamTarget::Pipe(pipe_id)) => {
                Some(PlatformDescriptorSource::Pipe(*pipe_id, PipeDirection::Reader))
            }
            None => Some(PlatformDescriptorSource::TerminalRead),
        },
        1 => match &process.stdout_target {
            Some(StreamTarget::File(handle)) => {
                Some(PlatformDescriptorSource::File(handle.clone()))
            }
            Some(StreamTarget::Pipe(pipe_id)) => {
                Some(PlatformDescriptorSource::Pipe(*pipe_id, PipeDirection::Writer))
            }
            None => Some(PlatformDescriptorSource::TerminalWrite),
        },
        2 => match &process.stderr_target {
            Some(StreamTarget::File(handle)) => {
                Some(PlatformDescriptorSource::File(handle.clone()))
            }
            Some(StreamTarget::Pipe(pipe_id)) => {
                Some(PlatformDescriptorSource::Pipe(*pipe_id, PipeDirection::Writer))
            }
            None => Some(PlatformDescriptorSource::TerminalWrite),
        },
        descriptor => {
            if let Some(pipe) = process
                .pipe_descriptors
                .iter()
                .find(|pipe| pipe.descriptor == descriptor)
            {
                return Some(PlatformDescriptorSource::Pipe(
                    pipe.pipe_id,
                    pipe.direction,
                ));
            }
            process
                .open_files
                .iter()
                .find(|file| file.descriptor == descriptor)
                .map(|file| PlatformDescriptorSource::File(file.handle.clone()))
        }
    }
}

fn platform_install_descriptor(
    process_id: u64,
    source: PlatformDescriptorSource,
    descriptor: u64,
    replace: bool,
) -> Result<(), i64> {
    if descriptor < 3 || descriptor >= 3 + MAX_OPEN_FILES as u64 {
        return Err(ERR_BAD_FILE_DESCRIPTOR);
    }
    if matches!(
        &source,
        PlatformDescriptorSource::TerminalRead | PlatformDescriptorSource::TerminalWrite
    ) {
        return Err(ERR_NOT_IMPLEMENTED);
    }
    platform_retain_source(&source)?;

    if replace {
        let in_use = PROCESS_MANAGER
            .lock()
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
            .is_some_and(|process| descriptor_in_use(process, descriptor));
        if in_use && (syscall_close(process_id, descriptor) as i64) < 0 {
            platform_release_source(&source);
            return Err(ERR_IO);
        }
    }

    let inserted = {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            platform_release_source(&source);
            return Err(ERR_BAD_FILE_DESCRIPTOR);
        };
        if descriptor_in_use(process, descriptor) || descriptor_count(process) >= MAX_OPEN_FILES {
            false
        } else {
            match source.clone() {
                PlatformDescriptorSource::File(handle) => process.open_files.push(OpenFile {
                    descriptor,
                    handle,
                    close_on_exec: false,
                }),
                PlatformDescriptorSource::Pipe(pipe_id, direction) => {
                    process.pipe_descriptors.push(PipeDescriptor {
                        descriptor,
                        pipe_id,
                        direction,
                        close_on_exec: false,
                    });
                }
                PlatformDescriptorSource::TerminalRead
                | PlatformDescriptorSource::TerminalWrite => unreachable!(),
            }
            true
        }
    };
    if !inserted {
        platform_release_source(&source);
        return Err(ERR_TOO_MANY_OPEN_FILES);
    }
    Ok(())
}

fn platform_install_stream(
    process_id: u64,
    source: PlatformDescriptorSource,
    descriptor: u64,
) -> Result<(), i64> {
    let access = if descriptor == 0 {
        StreamAccess::Read
    } else {
        StreamAccess::Write
    };
    let replacement = platform_stream_target(&source, access)?;
    retain_stream_target(&replacement, access).map_err(|_| ERR_IO)?;

    let previous = {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            release_stream_target(replacement, access);
            return Err(ERR_BAD_FILE_DESCRIPTOR);
        };
        match descriptor {
            0 => core::mem::replace(&mut process.stdin_target, replacement),
            1 => core::mem::replace(&mut process.stdout_target, replacement),
            2 => core::mem::replace(&mut process.stderr_target, replacement),
            _ => {
                release_stream_target(replacement, access);
                return Err(ERR_BAD_FILE_DESCRIPTOR);
            }
        }
    };
    release_stream_target(previous, access);
    Ok(())
}

fn platform_stream_target(
    source: &PlatformDescriptorSource,
    access: StreamAccess,
) -> Result<Option<StreamTarget>, i64> {
    match (source, access) {
        (PlatformDescriptorSource::TerminalRead, StreamAccess::Read)
        | (PlatformDescriptorSource::TerminalWrite, StreamAccess::Write) => Ok(None),
        (PlatformDescriptorSource::File(handle), access) => {
            let allowed = {
                let file = handle.lock();
                match access {
                    StreamAccess::Read => file.readable,
                    StreamAccess::Write => file.writable,
                }
            };
            if allowed {
                Ok(Some(StreamTarget::File(handle.clone())))
            } else {
                Err(ERR_BAD_FILE_DESCRIPTOR)
            }
        }
        (PlatformDescriptorSource::Pipe(pipe_id, PipeDirection::Reader), StreamAccess::Read)
        | (PlatformDescriptorSource::Pipe(pipe_id, PipeDirection::Writer), StreamAccess::Write) => {
            Ok(Some(StreamTarget::Pipe(*pipe_id)))
        }
        _ => Err(ERR_BAD_FILE_DESCRIPTOR),
    }
}

fn platform_retain_source(source: &PlatformDescriptorSource) -> Result<(), i64> {
    match source {
        PlatformDescriptorSource::Pipe(pipe_id, PipeDirection::Reader) => {
            pipe::retain_reader(*pipe_id).map_err(|_| ERR_IO)
        }
        PlatformDescriptorSource::Pipe(pipe_id, PipeDirection::Writer) => {
            pipe::retain_writer(*pipe_id).map_err(|_| ERR_IO)
        }
        _ => Ok(()),
    }
}

fn platform_release_source(source: &PlatformDescriptorSource) {
    match source {
        PlatformDescriptorSource::Pipe(pipe_id, PipeDirection::Reader) => {
            let _ = pipe::close_reader(*pipe_id);
        }
        PlatformDescriptorSource::Pipe(pipe_id, PipeDirection::Writer) => {
            let _ = pipe::close_writer(*pipe_id);
        }
        _ => {}
    }
}

fn platform_vfs_errno(error: &vfs::Error) -> i64 {
    match error {
        vfs::Error::NotFound => ERR_NO_ENTRY,
        vfs::Error::NotDirectory => abi::errno::NOT_DIRECTORY,
        vfs::Error::IsDirectory => ERR_IS_DIRECTORY,
        vfs::Error::ReadOnly => ERR_READ_ONLY,
        vfs::Error::NoSpace | vfs::Error::FileTooLarge | vfs::Error::TooManyFiles => ERR_NO_SPACE,
        vfs::Error::PathTooLong | vfs::Error::NameTooLong => abi::errno::NAME_TOO_LONG,
        vfs::Error::InvalidPath
        | vfs::Error::TooManyPathComponents
        | vfs::Error::InvalidOpenOptions => ERR_INVALID_ARGUMENT,
        _ => ERR_IO,
    }
}
