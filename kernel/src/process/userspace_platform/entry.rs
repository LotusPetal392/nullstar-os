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
    let cwd_aware_legacy_call = platform_cwd_aware_legacy_call(syscall_number);

    if !platform_syscall_number(syscall_number)
        && !platform_reserved_environment_call(syscall_number, registers_pointer)
        && !cwd_aware_legacy_call
    {
        return galactic_syscall_dispatch(current_stack_pointer);
    }

    let Some(process_id) = scheduler::current_process_id() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };

    if cwd_aware_legacy_call {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
            return current_stack_pointer;
        };
        process.syscall_count = process.syscall_count.saturating_add(1);
    }

    // Keep the existing exact smoke-test counter stable: legacy calls continue
    // to be counted, while platform ABI calls are deliberately excluded until
    // versioned syscall metrics are added.
    let registers = unsafe { &mut *registers_pointer };
    if syscall_number == abi::syscall::SPAWN_COMMAND {
        return match platform_spawn_command(
            process_id,
            SpawnSyscallArgs {
                address: registers.rdi,
                length: registers.rsi,
                flags: registers.rdx,
                stdin_descriptor: registers.r10,
                stdout_descriptor: registers.r8,
                stderr_descriptor: registers.r9,
                process_group_argument: registers.rbx,
            },
            current_stack_pointer,
        ) {
            ControlOutcome::Ready(result) => {
                registers.rax = result;
                current_stack_pointer
            }
            ControlOutcome::Blocked => scheduler::block_current(current_stack_pointer),
        };
    }
    if syscall_number == abi::syscall::EXECVE {
        return match platform_execve(
            process_id,
            registers.rdi,
            registers.rsi,
            current_stack_pointer,
        ) {
            ControlOutcome::Ready(result) => {
                registers.rax = result;
                current_stack_pointer
            }
            ControlOutcome::Blocked => scheduler::block_current(current_stack_pointer),
        };
    }
    if syscall_number == abi::syscall::OPEN {
        return match platform_open(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            current_stack_pointer,
        ) {
            ControlOutcome::Ready(result) => {
                registers.rax = result;
                current_stack_pointer
            }
            ControlOutcome::Blocked => scheduler::block_current(current_stack_pointer),
        };
    }
    if syscall_number == abi::syscall::STAT {
        return match platform_stat(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
            current_stack_pointer,
        ) {
            ControlOutcome::Ready(result) => {
                registers.rax = result;
                current_stack_pointer
            }
            ControlOutcome::Blocked => scheduler::block_current(current_stack_pointer),
        };
    }
    if syscall_number == abi::syscall::READ_DIRECTORY {
        return match platform_read_directory(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
            registers.r8,
            current_stack_pointer,
        ) {
            ControlOutcome::Ready(result) => {
                registers.rax = result;
                current_stack_pointer
            }
            ControlOutcome::Blocked => scheduler::block_current(current_stack_pointer),
        };
    }

    registers.rax = match syscall_number {
        abi::syscall::SYSTEM_INFO => platform_system_info(process_id, registers.rdi, registers.rsi),
        abi::syscall::FSTAT => {
            platform_fstat(process_id, registers.rdi, registers.rsi, registers.rdx)
        }
        abi::syscall::CHDIR => platform_chdir(process_id, registers.rdi, registers.rsi),
        abi::syscall::GETCWD => platform_getcwd(process_id, registers.rdi, registers.rsi),
        abi::syscall::REGISTER_TMPFS_SERVICE => {
            platform_register_tmpfs_service(process_id, registers.rdi, registers.rsi)
        }
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

fn platform_cwd_aware_legacy_call(number: u64) -> bool {
    matches!(
        number,
        abi::syscall::OPEN | abi::syscall::SPAWN_COMMAND | abi::syscall::EXECVE
    )
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
            | abi::syscall::REGISTER_TMPFS_SERVICE
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

fn platform_open(
    process_id: u64,
    address: u64,
    length: u64,
    flags: u64,
    current_stack_pointer: usize,
) -> ControlOutcome {
    let (options, close_on_exec) = match decode_open_options(flags) {
        Ok(decoded) => decoded,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    let path = match platform_user_path(process_id, address, length) {
        Ok(path) => path,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    let descriptor = {
        let manager = PROCESS_MANAGER.lock();
        let Some(process) = manager
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
        else {
            return ControlOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
        };
        if descriptor_count(process) >= MAX_OPEN_FILES {
            return ControlOutcome::Ready(error_return(ERR_TOO_MANY_OPEN_FILES));
        }
        let Some(descriptor) = allocate_descriptor(process) else {
            return ControlOutcome::Ready(error_return(ERR_TOO_MANY_OPEN_FILES));
        };
        descriptor
    };
    if tmpfs_proxy_state().is_some() {
        match tmpfs_proxy_path(&path) {
            Ok(Some(TmpfsProxyPath::Directory)) => {
                return ControlOutcome::Ready(error_return(ERR_IS_DIRECTORY));
            }
            Ok(Some(TmpfsProxyPath::File(_))) => {
                return tmpfs_proxy_open(
                    process_id,
                    &path,
                    options,
                    close_on_exec,
                    descriptor,
                    current_stack_pointer,
                );
            }
            Ok(None) => {}
            Err(error) => return ControlOutcome::Ready(error_return(error)),
        }
    }
    let metadata = match vfs::open(&path, options) {
        Ok(metadata) => metadata,
        Err(error) => return ControlOutcome::Ready(error_return(vfs_errno(&error))),
    };
    let offset = if options.append { metadata.size } else { 0 };
    let handle = Arc::new(Mutex::new(OpenFileState {
        path: metadata.path,
        offset,
        readable: options.read,
        writable: options.write,
        append: options.append,
        size: metadata.size,
        backend: OpenFileBackend::Vfs,
    }));
    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return ControlOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
    };
    if descriptor_in_use(process, descriptor) {
        return ControlOutcome::Ready(error_return(ERR_TOO_MANY_OPEN_FILES));
    }
    process.open_files.push(OpenFile {
        descriptor,
        handle,
        close_on_exec,
    });
    process.open_count = process.open_count.saturating_add(1);
    ControlOutcome::Ready(descriptor)
}

fn platform_register_tmpfs_service(process_id: u64, handle: u64, generation: u64) -> u64 {
    let (entry, endpoint) = {
        let registry = CAPABILITY_REGISTRY.lock();
        let entry = registry.entry(process_id, handle);
        (entry.map(|entry| (entry.object.kind, entry.rights)), entry.map(|entry| entry.object))
    };
    let generation = match crate::tmpfs_abi::validate_registration(
        process_id,
        INIT_PROCESS_ID,
        generation,
        entry,
        abi::capability::KIND_ENDPOINT,
        abi::capability::RIGHT_SEND,
    ) {
        Ok(generation) => generation,
        Err(crate::tmpfs_abi::RegistrationError::Permission) => {
            return error_return(abi::errno::PERMISSION);
        }
        Err(crate::tmpfs_abi::RegistrationError::BadHandle) => {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        }
        Err(crate::tmpfs_abi::RegistrationError::MissingSendRight) => {
            return error_return(abi::errno::PERMISSION);
        }
        Err(
            crate::tmpfs_abi::RegistrationError::InvalidGeneration
            | crate::tmpfs_abi::RegistrationError::WrongObjectKind,
        ) => return error_return(ERR_INVALID_ARGUMENT),
    };
    let Some(endpoint) = endpoint else {
        return error_return(ERR_BAD_FILE_DESCRIPTOR);
    };

    let previous = {
        let mut state = TMPFS_PROXY.lock();
        let previous = state.request_endpoint;
        state.request_endpoint = Some(endpoint);
        state.generation = generation;
        previous
    };
    if previous != Some(endpoint) {
        if let Some(previous) = previous {
            kernel_capability_root_remove(previous);
        }
        kernel_capability_root_add(endpoint);
    }
    0
}

fn platform_spawn_command(
    process_id: u64,
    arguments: SpawnSyscallArgs,
    current_stack_pointer: usize,
) -> ControlOutcome {
    let SpawnSyscallArgs {
        address,
        length,
        flags,
        stdin_descriptor,
        stdout_descriptor,
        stderr_descriptor,
        process_group_argument,
    } = arguments;
    if flags & !abi::spawn::ALLOWED_FLAGS != 0 {
        return ControlOutcome::Ready(error_return(ERR_INVALID_ARGUMENT));
    }
    let new_process_group = flags & SPAWN_NEW_PROCESS_GROUP != 0;
    let join_process_group = flags & SPAWN_JOIN_PROCESS_GROUP != 0;
    if new_process_group && join_process_group {
        return ControlOutcome::Ready(error_return(ERR_INVALID_ARGUMENT));
    }
    let process_group_id = if join_process_group {
        if process_group_argument == DEFAULT_PROCESS_GROUP {
            return ControlOutcome::Ready(error_return(ERR_INVALID_ARGUMENT));
        }
        Some(process_group_argument)
    } else {
        None
    };
    let command = match user_text(process_id, address, length, MAX_COMMAND_BYTES) {
        Ok(command) => command,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    let (path, arguments) = match platform_parse_command_line(process_id, &command) {
        Ok(command) => command,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    let use_descriptors = flags & SPAWN_USE_DESCRIPTORS != 0;
    let stdin_descriptor =
        (use_descriptors && stdin_descriptor != DEFAULT_DESCRIPTOR).then_some(stdin_descriptor);
    let stdout_descriptor =
        (use_descriptors && stdout_descriptor != DEFAULT_DESCRIPTOR).then_some(stdout_descriptor);
    let stderr_descriptor =
        (use_descriptors && stderr_descriptor != DEFAULT_DESCRIPTOR).then_some(stderr_descriptor);

    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return ControlOutcome::Ready(error_return(ERR_NO_CHILD));
    };
    if resolve_stream_descriptor(process, stdin_descriptor, StreamAccess::Read).is_err()
        || resolve_stream_descriptor(process, stdout_descriptor, StreamAccess::Write).is_err()
        || resolve_stream_descriptor(process, stderr_descriptor, StreamAccess::Write).is_err()
    {
        return ControlOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
    }
    if process.pending_child_spawn.is_some()
        || process.pending_child_wait.is_some()
        || process.pending_exec.is_some()
        || process.pending_fork.is_some()
        || process.pending_tmpfs_proxy.is_some()
    {
        return ControlOutcome::Ready(error_return(ERR_IO));
    }
    process.pending_child_spawn = Some(PendingChildSpawn {
        path,
        arguments,
        foreground: flags & SPAWN_FOREGROUND != 0,
        stdin_descriptor,
        stdout_descriptor,
        stderr_descriptor,
        new_process_group,
        process_group_id,
        stack_pointer: current_stack_pointer,
    });
    process.state = ProcessState::Blocked;
    ControlOutcome::Blocked
}

fn platform_execve(
    process_id: u64,
    address: u64,
    length: u64,
    current_stack_pointer: usize,
) -> ControlOutcome {
    let command = match user_text(process_id, address, length, MAX_COMMAND_BYTES) {
        Ok(command) => command,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    let (path, arguments) = match platform_parse_command_line(process_id, &command) {
        Ok(command) => command,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };

    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return ControlOutcome::Ready(error_return(ERR_NO_PROCESS));
    };
    if process.pending_terminal_read.is_some()
        || process.pending_pipe_read.is_some()
        || process.pending_pipe_write.is_some()
        || process.pending_tmpfs_proxy.is_some()
        || process.pending_child_spawn.is_some()
        || process.pending_child_wait.is_some()
        || process.pending_exec.is_some()
        || process.pending_fork.is_some()
    {
        return ControlOutcome::Ready(error_return(ERR_IO));
    }
    process.pending_exec = Some(PendingExec {
        path,
        arguments,
        stack_pointer: current_stack_pointer,
    });
    process.state = ProcessState::Blocked;
    ControlOutcome::Blocked
}

fn platform_parse_command_line(
    process_id: u64,
    command: &str,
) -> Result<(String, Vec<String>), i64> {
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return Err(ERR_INVALID_ARGUMENT);
    };
    let path = if program.starts_with('/') {
        String::from(program)
    } else if program.contains('/') {
        platform_resolve_path(process_id, program)?
    } else {
        let mut path = String::from("/");
        path.push_str(program);
        path
    };
    let mut arguments = Vec::new();
    let mut argument_bytes = path.len().saturating_add(1);
    for argument in words {
        if arguments.len().saturating_add(1) >= MAX_ARGUMENTS {
            return Err(ERR_ARGUMENT_TOO_LARGE);
        }
        argument_bytes = argument_bytes.saturating_add(argument.len().saturating_add(1));
        if argument_bytes > MAX_ARGUMENT_BYTES {
            return Err(ERR_ARGUMENT_TOO_LARGE);
        }
        arguments.push(String::from(argument));
    }
    Ok((path, arguments))
}

fn platform_resolve_path(process_id: u64, path: &str) -> Result<String, i64> {
    let directory = platform_working_directory(process_id);
    let separator = usize::from(directory != "/");
    let capacity = directory
        .len()
        .checked_add(separator)
        .and_then(|length| length.checked_add(path.len()))
        .ok_or(abi::errno::NAME_TOO_LONG)?;
    let mut candidate = String::with_capacity(capacity);
    candidate.push_str(&directory);
    if separator != 0 {
        candidate.push('/');
    }
    candidate.push_str(path);

    let mut components = Vec::new();
    for component in candidate.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            component => {
                if components.len() >= vfs::MAX_PATH_COMPONENTS {
                    return Err(ERR_INVALID_ARGUMENT);
                }
                components.push(component);
            }
        }
    }

    let required = components
        .iter()
        .enumerate()
        .try_fold(1usize, |length, (index, component)| {
            length
                .checked_add(usize::from(index != 0))
                .and_then(|length| length.checked_add(component.len()))
        })
        .ok_or(abi::errno::NAME_TOO_LONG)?;
    if required > abi::limits::MAX_PATH_BYTES {
        return Err(abi::errno::NAME_TOO_LONG);
    }

    let mut resolved = String::with_capacity(required);
    resolved.push('/');
    for component in components {
        if resolved.len() > 1 {
            resolved.push('/');
        }
        resolved.push_str(component);
    }
    Ok(resolved)
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
