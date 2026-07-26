// Filesystem discovery and per-process working-directory services.

fn platform_stat(
    process_id: u64,
    path_address: u64,
    path_length: u64,
    stat_address: u64,
    stat_length: u64,
    current_stack_pointer: usize,
) -> ControlOutcome {
    let path = match platform_user_path(process_id, path_address, path_length) {
        Ok(path) => path,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    if vfs_route_ready() {
        return vfs_route_stat(
            process_id,
            &path,
            stat_address,
            stat_length,
            current_stack_pointer,
        );
    }
    if tmpfs_proxy_state().is_some() {
        match tmpfs_proxy_path(&path) {
            Ok(Some(_)) => {
                return tmpfs_proxy_stat(
                    process_id,
                    &path,
                    stat_address,
                    stat_length,
                    current_stack_pointer,
                );
            }
            Ok(None) => {}
            Err(error) => return ControlOutcome::Ready(error_return(error)),
        }
    }
    let metadata = match vfs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => return ControlOutcome::Ready(error_return(platform_vfs_errno(&error))),
    };
    ControlOutcome::Ready(platform_write_value(
        process_id,
        stat_address,
        stat_length,
        platform_stat_from_metadata(&metadata),
    ))
}

fn platform_read_directory(
    process_id: u64,
    path_address: u64,
    path_length: u64,
    start_index: u64,
    records_address: u64,
    capacity: u64,
    current_stack_pointer: usize,
) -> ControlOutcome {
    let capacity = match usize::try_from(capacity) {
        Ok(capacity) if capacity <= abi::limits::MAX_DIRECTORY_ENTRIES_PER_CALL => capacity,
        _ => return ControlOutcome::Ready(error_return(ERR_INVALID_ARGUMENT)),
    };
    if capacity == 0 {
        return ControlOutcome::Ready(0);
    }
    let byte_length = match capacity.checked_mul(size_of::<abi::file::DirectoryEntry>()) {
        Some(length) => length,
        None => return ControlOutcome::Ready(error_return(ERR_ARGUMENT_TOO_LARGE)),
    };
    if !user_range_allows(process_id, records_address, byte_length, true) {
        return ControlOutcome::Ready(error_return(ERR_BAD_ADDRESS));
    }
    let start_index = match usize::try_from(start_index) {
        Ok(index) => index,
        Err(_) => return ControlOutcome::Ready(error_return(ERR_INVALID_ARGUMENT)),
    };
    let path = match platform_user_path(process_id, path_address, path_length) {
        Ok(path) => path,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    if vfs_route_ready() {
        return vfs_route_read_directory(
            process_id,
            &path,
            start_index,
            records_address,
            capacity,
            current_stack_pointer,
        );
    }
    if tmpfs_proxy_state().is_some() {
        match tmpfs_proxy_path(&path) {
            Ok(Some(TmpfsProxyPath::Directory)) => {
                return tmpfs_proxy_read_directory(
                    process_id,
                    &path,
                    start_index,
                    records_address,
                    capacity,
                    current_stack_pointer,
                );
            }
            Ok(Some(TmpfsProxyPath::File(_))) => {
                return ControlOutcome::Ready(error_return(abi::errno::NOT_DIRECTORY));
            }
            Ok(None) => {}
            Err(error) => return ControlOutcome::Ready(error_return(error)),
        }
    }
    let entries = match vfs::read_directory(&path) {
        Ok(entries) => entries,
        Err(error) => return ControlOutcome::Ready(error_return(platform_vfs_errno(&error))),
    };

    let mut written = 0usize;
    for entry in entries.iter().skip(start_index).take(capacity) {
        let record = match platform_directory_record(entry) {
            Ok(record) => record,
            Err(error) => return ControlOutcome::Ready(error_return(error)),
        };
        let destination = (records_address as *mut abi::file::DirectoryEntry).wrapping_add(written);
        unsafe { ptr::write_unaligned(destination, record) };
        written = written.saturating_add(1);
    }
    ControlOutcome::Ready(written as u64)
}

fn platform_chdir(
    process_id: u64,
    path_address: u64,
    path_length: u64,
    current_stack_pointer: usize,
) -> ControlOutcome {
    let path = match platform_user_path(process_id, path_address, path_length) {
        Ok(path) => path,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    if vfs_route_ready() {
        return vfs_route_chdir(process_id, &path, current_stack_pointer);
    }
    let metadata = match vfs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return ControlOutcome::Ready(error_return(platform_vfs_errno(&error)));
        }
    };
    if !metadata.is_directory() {
        return ControlOutcome::Ready(error_return(abi::errno::NOT_DIRECTORY));
    }
    ControlOutcome::Ready(match platform_set_working_directory(process_id, &metadata.path) {
        Ok(()) => 0,
        Err(error) => error_return(error),
    })
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
        (address as *mut u8).wrapping_add(directory.len()).write(0);
    }
    directory.len() as u64
}

fn platform_user_path(process_id: u64, address: u64, length: u64) -> Result<String, i64> {
    let path = user_text(process_id, address, length, abi::limits::MAX_PATH_BYTES)?;
    let candidate = if path.starts_with('/') {
        path
    } else {
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
        candidate.push_str(&path);
        candidate
    };

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
        });
    let Some(required) = required else {
        return Err(abi::errno::NAME_TOO_LONG);
    };
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
    if vfs_route_ready() && vfs_is_declared_namespace_directory(&candidate) {
        return candidate;
    }
    match vfs::metadata(&candidate) {
        Ok(metadata) if metadata.is_directory() => metadata.path,
        _ => String::from("/"),
    }
}

fn platform_set_working_directory(process_id: u64, directory: &str) -> Result<(), i64> {
    let mut entry = String::from(PLATFORM_WORKING_DIRECTORY_PREFIX);
    entry.push_str(directory);
    let entry_bytes = entry.len().checked_add(1).ok_or(ERR_ARGUMENT_TOO_LARGE)?;

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

fn platform_write_value<T: Copy>(process_id: u64, address: u64, length: u64, value: T) -> u64 {
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
        flags: platform_metadata_flags(metadata.read_only, metadata.hidden, metadata.system),
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
    let mut flags = 0;
    if read_only {
        flags |= abi::file::FLAG_READ_ONLY;
    }
    if hidden {
        flags |= abi::file::FLAG_HIDDEN;
    }
    if system {
        flags |= abi::file::FLAG_SYSTEM;
    }
    flags
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
