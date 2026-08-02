// Capability-authorized, bounded handoff from the kernel early-log ring.

mod kernel_early_log_abi {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/early_log_abi.rs"
    ));
}

fn open_kernel_early_log_reader(process_id: u64) -> u64 {
    if process_id != INIT_PROCESS_ID {
        return error_return(abi::errno::PERMISSION);
    }

    let mut registry = CAPABILITY_REGISTRY.lock();
    let object = match registry.create_object(
        abi::capability::KIND_KERNEL_EARLY_LOG_READER,
        CapabilityObjectData::KernelEarlyLogReader(KernelEarlyLogReaderObject),
    ) {
        Ok(object) => object,
        Err(error) => return error_return(error),
    };
    match registry.insert_entry(
        process_id,
        object,
        abi::capability::KERNEL_EARLY_LOG_READER_RIGHTS,
    ) {
        Ok(handle) => handle,
        Err(error) => {
            registry.collect_garbage();
            error_return(error)
        }
    }
}

fn read_kernel_early_log(
    process_id: u64,
    handle: u64,
    after_sequence: u64,
    output_address: u64,
    output_length: u64,
) -> u64 {
    if output_length != kernel_early_log_abi::READ_RESPONSE_BYTES as u64
        || !capability_user_range(
            process_id,
            output_address,
            kernel_early_log_abi::READ_RESPONSE_BYTES,
            true,
        )
    {
        return error_return(if output_length != kernel_early_log_abi::READ_RESPONSE_BYTES as u64 {
            abi::errno::INVALID_ARGUMENT
        } else {
            abi::errno::BAD_ADDRESS
        });
    }

    {
        let registry = CAPABILITY_REGISTRY.lock();
        let Some(entry) = registry.entry(process_id, handle) else {
            return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
        };
        if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_READ) {
            return error_return(error);
        }
        if entry.object.kind != abi::capability::KIND_KERNEL_EARLY_LOG_READER {
            return error_return(abi::errno::INVALID_ARGUMENT);
        }
        let Some(index) = registry.object_index(entry.object) else {
            return error_return(abi::errno::IO);
        };
        if !matches!(
            registry.objects[index].data,
            CapabilityObjectData::KernelEarlyLogReader(_)
        ) {
            return error_return(abi::errno::IO);
        }
    }

    let after = if after_sequence == 0 {
        None
    } else {
        match crate::early_log::EarlySequence::new(after_sequence) {
            Some(sequence) => Some(sequence),
            None => return error_return(abi::errno::INVALID_ARGUMENT),
        }
    };
    let read = match crate::early_log::try_read_kernel_early_log_after(after) {
        Ok(read) => read,
        Err(crate::early_log::SnapshotError::Busy) => {
            return error_return(abi::errno::TRY_AGAIN);
        }
        Err(crate::early_log::SnapshotError::Uninitialized) => {
            return error_return(abi::errno::IO);
        }
    };

    let mut response = kernel_early_log_abi::ReadResponse::EMPTY;
    response.submitted_records = read.stats.submitted;
    response.retained_records = read.stats.retained as u64;
    response.capacity_records = read.stats.capacity as u64;
    response.overwritten_records = read.stats.overwritten;
    response.dropped_records = read.stats.dropped;
    response.rejected_records = read.stats.rejected;
    response.busy_drops = read.busy_drops;
    response.oldest_sequence = read
        .stats
        .oldest_sequence
        .map(crate::early_log::EarlySequence::get)
        .unwrap_or(0);
    response.newest_sequence = read
        .stats
        .newest_sequence
        .map(crate::early_log::EarlySequence::get)
        .unwrap_or(0);
    if let crate::early_log::BootIdentity::Id(boot_id) = read.stats.boot_identity {
        response.flags |= kernel_early_log_abi::FLAG_BOOT_ID_PRESENT;
        response.boot_id = *boot_id.as_bytes();
    }

    if let Some(record) = read.record {
        response.flags |= kernel_early_log_abi::FLAG_RECORD_PRESENT;
        response.sequence = record.sequence().get();
        response.event_id = record.event_id().into_bytes();
        response.severity = record.severity() as u8;
        response.privacy = record.privacy() as u8;
        response.monotonic_time_ns = record.monotonic_time_ns();
        let source = record.source();
        if let Some(cpu_id) = source.cpu_id {
            response.flags |= kernel_early_log_abi::FLAG_CPU_ID_PRESENT;
            response.cpu_id = cpu_id;
        }
        if let Some(process_id) = source.process_id {
            response.flags |= kernel_early_log_abi::FLAG_PROCESS_ID_PRESENT;
            response.process_id = process_id.get();
        }
        if let Some(thread_id) = source.thread_id {
            response.flags |= kernel_early_log_abi::FLAG_THREAD_ID_PRESENT;
            response.thread_id = thread_id.get();
        }
        let subsystem = record.subsystem().as_bytes();
        let message = record.message().as_bytes();
        response.subsystem_len = subsystem.len() as u8;
        response.message_len = message.len() as u8;
        response.subsystem[..subsystem.len()].copy_from_slice(subsystem);
        response.message[..message.len()].copy_from_slice(message);
    }

    debug_assert_eq!(response.flags & !kernel_early_log_abi::KNOWN_FLAGS, 0);
    platform_write_value(process_id, output_address, output_length, response)
}
