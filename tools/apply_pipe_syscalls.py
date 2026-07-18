from pipe_patch_common import replace_once

# Pipe-aware write dispatch and implementation.
replace_once(
    "kernel/src/process/userspace.rs",
    '''        SYSCALL_WRITE => {
            let result = syscall_write(process_id, registers.rdi, registers.rsi, registers.rdx);
            registers.rax = result;
            current_stack_pointer
        }
''',
    '''        SYSCALL_WRITE => match syscall_write(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            current_stack_pointer,
        ) {
            WriteOutcome::Ready(result) => {
                registers.rax = result;
                current_stack_pointer
            }
            WriteOutcome::Blocked => scheduler::block_current(current_stack_pointer),
        },
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''fn syscall_write(process_id: u64, file_descriptor: u64, address: u64, length: u64) -> u64 {
    if file_descriptor != 1 && file_descriptor != 2 {
        return error_return(ERR_BAD_FILE_DESCRIPTOR);
    }
    let Ok(length) = usize::try_from(length) else {
        return error_return(ERR_ARGUMENT_TOO_LARGE);
    };
    if length > MAX_SYSCALL_WRITE_BYTES {
        return error_return(ERR_ARGUMENT_TOO_LARGE);
    }

    let readable = PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == process_id)
        .map(|process| {
            process
                .ranges
                .iter()
                .any(|range| range.readable && range.contains(address, length))
        })
        .unwrap_or(false);
    if !readable {
        return error_return(ERR_BAD_ADDRESS);
    }

    let bytes = unsafe { slice::from_raw_parts(address as *const u8, length) };
    if let Ok(text) = str::from_utf8(bytes) {
        crate::print!("{text}");
        crate::serial_print!("{text}");
    } else {
        for byte in bytes.iter().copied() {
            let character = match byte {
                b'\\n' | b'\\r' | b'\\t' => char::from(byte),
                0x20..=0x7e => char::from(byte),
                _ => '.',
            };
            crate::print!("{character}");
            crate::serial_print!("{character}");
        }
    }

    let mut manager = PROCESS_MANAGER.lock();
    if let Some(process) = manager.process_mut(process_id) {
        process.write_count = process.write_count.saturating_add(1);
        process.bytes_written = process.bytes_written.saturating_add(length as u64);
    }
    length as u64
}
''',
    '''enum WriteOutcome {
    Ready(u64),
    Blocked,
}

fn syscall_write(
    process_id: u64,
    file_descriptor: u64,
    address: u64,
    length: u64,
    current_stack_pointer: usize,
) -> WriteOutcome {
    if file_descriptor != 1 && file_descriptor != 2 {
        return WriteOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
    }
    let Ok(length) = usize::try_from(length) else {
        return WriteOutcome::Ready(error_return(ERR_ARGUMENT_TOO_LARGE));
    };
    if length > MAX_SYSCALL_WRITE_BYTES {
        return WriteOutcome::Ready(error_return(ERR_ARGUMENT_TOO_LARGE));
    }
    if length == 0 {
        return WriteOutcome::Ready(0);
    }

    let (readable, stdout_pipe) = PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == process_id)
        .map(|process| {
            (
                process
                    .ranges
                    .iter()
                    .any(|range| range.readable && range.contains(address, length)),
                process.stdout_pipe,
            )
        })
        .unwrap_or((false, None));
    if !readable {
        return WriteOutcome::Ready(error_return(ERR_BAD_ADDRESS));
    }

    let bytes = unsafe { slice::from_raw_parts(address as *const u8, length) };
    if file_descriptor == 1 {
        if let Some(pipe_id) = stdout_pipe {
            return match pipe::write(pipe_id, bytes) {
                Ok(pipe::WriteOutcome::Written(count)) => {
                    let mut manager = PROCESS_MANAGER.lock();
                    if let Some(process) = manager.process_mut(process_id) {
                        process.write_count = process.write_count.saturating_add(1);
                        process.bytes_written = process.bytes_written.saturating_add(count as u64);
                        process.pipe_write_count = process.pipe_write_count.saturating_add(1);
                        process.pipe_bytes_written =
                            process.pipe_bytes_written.saturating_add(count as u64);
                    }
                    WriteOutcome::Ready(count as u64)
                }
                Ok(pipe::WriteOutcome::Full) => {
                    let mut manager = PROCESS_MANAGER.lock();
                    let Some(process) = manager.process_mut(process_id) else {
                        return WriteOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
                    };
                    process.pending_pipe_write = Some(PendingPipeWrite {
                        pipe_id,
                        address,
                        length,
                        stack_pointer: current_stack_pointer,
                    });
                    process.state = ProcessState::Blocked;
                    process.blocked_pipe_write_count =
                        process.blocked_pipe_write_count.saturating_add(1);
                    drop(manager);
                    let _ = pipe::note_blocked_write(pipe_id);
                    WriteOutcome::Blocked
                }
                Ok(pipe::WriteOutcome::NoReaders) => {
                    WriteOutcome::Ready(error_return(ERR_BROKEN_PIPE))
                }
                Err(_) => WriteOutcome::Ready(error_return(ERR_IO)),
            };
        }
    }

    if let Ok(text) = str::from_utf8(bytes) {
        crate::print!("{text}");
        crate::serial_print!("{text}");
    } else {
        for byte in bytes.iter().copied() {
            let character = match byte {
                b'\\n' | b'\\r' | b'\\t' => char::from(byte),
                0x20..=0x7e => char::from(byte),
                _ => '.',
            };
            crate::print!("{character}");
            crate::serial_print!("{character}");
        }
    }

    let mut manager = PROCESS_MANAGER.lock();
    if let Some(process) = manager.process_mut(process_id) {
        process.write_count = process.write_count.saturating_add(1);
        process.bytes_written = process.bytes_written.saturating_add(length as u64);
    }
    WriteOutcome::Ready(length as u64)
}
''',
)

# Pipe-aware stdin read.
replace_once(
    "kernel/src/process/userspace.rs",
    '''    if descriptor == 0 {
        return syscall_terminal_read(process_id, address, length, current_stack_pointer);
    }
''',
    '''    if descriptor == 0 {
        let stdin_pipe = PROCESS_MANAGER
            .lock()
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
            .and_then(|process| process.stdin_pipe);
        if let Some(pipe_id) = stdin_pipe {
            return syscall_pipe_read(
                process_id,
                pipe_id,
                address,
                length,
                current_stack_pointer,
            );
        }
        return syscall_terminal_read(process_id, address, length, current_stack_pointer);
    }
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''fn syscall_terminal_read(
''',
    '''fn syscall_pipe_read(
    process_id: u64,
    pipe_id: PipeId,
    address: u64,
    length: usize,
    current_stack_pointer: usize,
) -> ReadOutcome {
    match pipe::read(pipe_id, length) {
        Ok(pipe::ReadOutcome::Data(bytes)) => {
            unsafe {
                ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len());
            }
            let mut manager = PROCESS_MANAGER.lock();
            if let Some(process) = manager.process_mut(process_id) {
                process.pipe_read_count = process.pipe_read_count.saturating_add(1);
                process.pipe_bytes_read =
                    process.pipe_bytes_read.saturating_add(bytes.len() as u64);
            }
            ReadOutcome::Ready(bytes.len() as u64)
        }
        Ok(pipe::ReadOutcome::EndOfFile) => {
            let mut manager = PROCESS_MANAGER.lock();
            if let Some(process) = manager.process_mut(process_id) {
                process.pipe_read_count = process.pipe_read_count.saturating_add(1);
            }
            ReadOutcome::Ready(0)
        }
        Ok(pipe::ReadOutcome::Empty) => {
            let mut manager = PROCESS_MANAGER.lock();
            let Some(process) = manager.process_mut(process_id) else {
                return ReadOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
            };
            process.pending_pipe_read = Some(PendingPipeRead {
                pipe_id,
                address,
                length,
                stack_pointer: current_stack_pointer,
            });
            process.state = ProcessState::Blocked;
            process.blocked_pipe_read_count =
                process.blocked_pipe_read_count.saturating_add(1);
            drop(manager);
            let _ = pipe::note_blocked_read(pipe_id);
            ReadOutcome::Blocked
        }
        Err(_) => ReadOutcome::Ready(error_return(ERR_IO)),
    }
}

fn syscall_terminal_read(
''',
)

# Service pending pipe reads and writes from the kernel address space.
replace_once(
    "kernel/src/process/userspace.rs",
    '''fn syscall_close(process_id: u64, descriptor: u64) -> u64 {
''',
    '''fn service_pipe_waiters(physical_memory_offset: VirtAddr) -> Result<usize, Error> {
    let pending_reads: Vec<(u64, PendingPipeRead)> = cpu_interrupts::without_interrupts(|| {
        PROCESS_MANAGER
            .lock()
            .processes
            .iter()
            .filter_map(|process| {
                process
                    .pending_pipe_read
                    .map(|pending| (process.process_id, pending))
            })
            .collect()
    });
    let pending_writes: Vec<(u64, PendingPipeWrite)> = cpu_interrupts::without_interrupts(|| {
        PROCESS_MANAGER
            .lock()
            .processes
            .iter()
            .filter_map(|process| {
                process
                    .pending_pipe_write
                    .map(|pending| (process.process_id, pending))
            })
            .collect()
    });

    let mut wakeups = 0usize;
    for (process_id, pending) in pending_reads {
        let result = match pipe::read(pending.pipe_id, pending.length) {
            Ok(pipe::ReadOutcome::Data(bytes)) => Some(bytes),
            Ok(pipe::ReadOutcome::EndOfFile) => Some(Vec::new()),
            Ok(pipe::ReadOutcome::Empty) => None,
            Err(_) => {
                complete_pipe_read_error(process_id, pending, ERR_IO)?;
                wakeups = wakeups.saturating_add(1);
                continue;
            }
        };
        let Some(bytes) = result else {
            continue;
        };
        cpu_interrupts::without_interrupts(|| -> Result<(), Error> {
            let mut manager = PROCESS_MANAGER.lock();
            let process = manager
                .process_mut(process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            if !bytes.is_empty() {
                write_user_bytes(
                    pending.address,
                    &bytes,
                    physical_memory_offset,
                    &process.pages,
                )?;
            }
            let registers = unsafe { &mut *(pending.stack_pointer as *mut SavedRegisters) };
            registers.rax = bytes.len() as u64;
            process.pending_pipe_read = None;
            process.state = ProcessState::Runnable;
            process.pipe_read_count = process.pipe_read_count.saturating_add(1);
            process.pipe_bytes_read =
                process.pipe_bytes_read.saturating_add(bytes.len() as u64);
            drop(manager);
            if !scheduler::wake_process(process_id) {
                return Err(Error::ProcessNotFound(process_id));
            }
            let _ = pipe::note_reader_wakeup(pending.pipe_id);
            Ok(())
        })?;
        wakeups = wakeups.saturating_add(1);
    }

    for (process_id, pending) in pending_writes {
        let bytes = cpu_interrupts::without_interrupts(|| -> Result<Vec<u8>, Error> {
            let manager = PROCESS_MANAGER.lock();
            let process = manager
                .processes
                .iter()
                .find(|process| process.process_id == process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            read_user_bytes(
                pending.address,
                pending.length,
                physical_memory_offset,
                &process.pages,
            )
        })?;
        let result = match pipe::write(pending.pipe_id, &bytes) {
            Ok(pipe::WriteOutcome::Written(count)) => Ok(count as u64),
            Ok(pipe::WriteOutcome::Full) => continue,
            Ok(pipe::WriteOutcome::NoReaders) => Err(ERR_BROKEN_PIPE),
            Err(_) => Err(ERR_IO),
        };
        cpu_interrupts::without_interrupts(|| -> Result<(), Error> {
            let mut manager = PROCESS_MANAGER.lock();
            let process = manager
                .process_mut(process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            let registers = unsafe { &mut *(pending.stack_pointer as *mut SavedRegisters) };
            match result {
                Ok(count) => {
                    registers.rax = count;
                    process.write_count = process.write_count.saturating_add(1);
                    process.bytes_written = process.bytes_written.saturating_add(count);
                    process.pipe_write_count = process.pipe_write_count.saturating_add(1);
                    process.pipe_bytes_written = process.pipe_bytes_written.saturating_add(count);
                }
                Err(error) => registers.rax = error_return(error),
            }
            process.pending_pipe_write = None;
            process.state = ProcessState::Runnable;
            drop(manager);
            if !scheduler::wake_process(process_id) {
                return Err(Error::ProcessNotFound(process_id));
            }
            let _ = pipe::note_writer_wakeup(pending.pipe_id);
            Ok(())
        })?;
        wakeups = wakeups.saturating_add(1);
    }

    Ok(wakeups)
}

fn complete_pipe_read_error(
    process_id: u64,
    pending: PendingPipeRead,
    error: i64,
) -> Result<(), Error> {
    cpu_interrupts::without_interrupts(|| -> Result<(), Error> {
        let mut manager = PROCESS_MANAGER.lock();
        let process = manager
            .process_mut(process_id)
            .ok_or(Error::ProcessNotFound(process_id))?;
        let registers = unsafe { &mut *(pending.stack_pointer as *mut SavedRegisters) };
        registers.rax = error_return(error);
        process.pending_pipe_read = None;
        process.state = ProcessState::Runnable;
        drop(manager);
        if !scheduler::wake_process(process_id) {
            return Err(Error::ProcessNotFound(process_id));
        }
        let _ = pipe::note_reader_wakeup(pending.pipe_id);
        Ok(())
    })
}

fn syscall_close(process_id: u64, descriptor: u64) -> u64 {
''',
)

# Physical-copy helper for a blocked writer whose address space is not active.
replace_once(
    "kernel/src/process/userspace.rs",
    '''fn map_range(
''',
    '''fn read_user_bytes(
    mut virtual_address: u64,
    mut length: usize,
    physical_memory_offset: VirtAddr,
    pages: &[UserPage],
) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::with_capacity(length);
    while length > 0 {
        let page_address = align_down(virtual_address);
        let page = pages
            .iter()
            .find(|page| page.virtual_address == page_address)
            .ok_or(Error::InvalidUserRange)?;
        let within_page =
            usize::try_from(virtual_address - page_address).map_err(|_| Error::AddressOverflow)?;
        let chunk = length.min(Size4KiB::SIZE as usize - within_page);
        let source_address = physical_memory_offset
            .as_u64()
            .checked_add(page.frame.start_address().as_u64())
            .and_then(|address| address.checked_add(within_page as u64))
            .ok_or(Error::AddressOverflow)?;
        let source = unsafe { slice::from_raw_parts(source_address as *const u8, chunk) };
        bytes.extend_from_slice(source);
        virtual_address = virtual_address
            .checked_add(chunk as u64)
            .ok_or(Error::AddressOverflow)?;
        length -= chunk;
    }
    Ok(bytes)
}

fn map_range(
''',
)
