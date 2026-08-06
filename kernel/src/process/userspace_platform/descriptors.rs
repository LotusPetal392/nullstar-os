// File-descriptor metadata and duplication services.

fn platform_fstat(process_id: u64, descriptor: u64, stat_address: u64, stat_length: u64) -> u64 {
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
            let file = handle.lock();
            match file.backend {
                OpenFileBackend::TmpfsProxy {
                    generation,
                    session_id,
                    session_generation,
                    ..
                } => {
                    if !tmpfs_proxy_backend_is_current(generation, session_id, session_generation) {
                        return error_return(ERR_IO);
                    }
                    abi::file::Stat {
                        kind: abi::file::KIND_FILE,
                        size: file.size,
                        flags: 0,
                    }
                },
                OpenFileBackend::NullfsProxy {
                    generation,
                    session_id,
                    session_generation,
                    ..
                } => {
                    if !nullfs_proxy_backend_is_current(generation, session_id, session_generation)
                    {
                        return error_return(ERR_IO);
                    }
                    abi::file::Stat {
                        kind: abi::file::KIND_FILE,
                        size: open_file_size(&file),
                        flags: 0,
                    }
                }
                OpenFileBackend::Vfs => {
                    let metadata = match vfs::metadata(&file.path) {
                        Ok(metadata) => metadata,
                        Err(error) => return error_return(platform_vfs_errno(&error)),
                    };
                    platform_stat_from_metadata(&metadata)
                }
            }
        }
    };
    platform_write_value(process_id, stat_address, stat_length, stat)
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
            Some(StreamTarget::Pipe(pipe_id)) => Some(PlatformDescriptorSource::Pipe(
                *pipe_id,
                PipeDirection::Reader,
            )),
            None => Some(PlatformDescriptorSource::TerminalRead),
        },
        1 => match &process.stdout_target {
            Some(StreamTarget::File(handle)) => {
                Some(PlatformDescriptorSource::File(handle.clone()))
            }
            Some(StreamTarget::Pipe(pipe_id)) => Some(PlatformDescriptorSource::Pipe(
                *pipe_id,
                PipeDirection::Writer,
            )),
            None => Some(PlatformDescriptorSource::TerminalWrite),
        },
        2 => match &process.stderr_target {
            Some(StreamTarget::File(handle)) => {
                Some(PlatformDescriptorSource::File(handle.clone()))
            }
            Some(StreamTarget::Pipe(pipe_id)) => Some(PlatformDescriptorSource::Pipe(
                *pipe_id,
                PipeDirection::Writer,
            )),
            None => Some(PlatformDescriptorSource::TerminalWrite),
        },
        descriptor => {
            if let Some(pipe) = process
                .pipe_descriptors
                .iter()
                .find(|pipe| pipe.descriptor == descriptor)
            {
                return Some(PlatformDescriptorSource::Pipe(pipe.pipe_id, pipe.direction));
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
