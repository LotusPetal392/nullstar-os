// Kernel-serviced, capability-based partition block devices.

const MAX_BLOCK_DEVICE_SESSIONS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockDeviceAccess {
    ReadOnly,
    Writable,
}

impl BlockDeviceAccess {
    const fn features(self) -> u64 {
        match self {
            Self::ReadOnly => block_device_protocol::features::READ,
            Self::Writable => {
                block_device_protocol::features::READ
                    | block_device_protocol::features::WRITE
                    | block_device_protocol::features::FLUSH
            }
        }
    }

    const fn device_flags(self) -> u32 {
        match self {
            Self::ReadOnly => block_device_protocol::device_flags::READ_ONLY,
            Self::Writable => 0,
        }
    }

    const fn is_writable(self) -> bool {
        matches!(self, Self::Writable)
    }
}

#[derive(Clone, Copy)]
struct BlockDevicePartition {
    index: u32,
    start_lba: u64,
    block_count: u64,
    logical_block_size: usize,
}

#[derive(Clone, Copy)]
struct BlockDeviceBuffer {
    id: u64,
    object: CapabilityObjectRef,
    length: u64,
}

#[derive(Clone, Copy)]
struct BlockDeviceSession {
    id: u64,
    generation: u64,
    owner_process_id: u64,
    reply_endpoint: CapabilityObjectRef,
    buffer: Option<BlockDeviceBuffer>,
}

struct BlockDeviceEndpoint {
    partition: BlockDevicePartition,
    access: BlockDeviceAccess,
    filesystem_uuid: Option<[u8; 16]>,
    endpoint: Option<CapabilityObjectRef>,
    generation: u64,
    online: bool,
    next_session_id: u64,
    sessions: Vec<BlockDeviceSession>,
}

struct BlockDeviceEndpointState {
    configured: bool,
    poll_cursor: usize,
    devices: Vec<BlockDeviceEndpoint>,
}

impl BlockDeviceEndpointState {
    const fn new() -> Self {
        Self {
            configured: false,
            poll_cursor: 0,
            devices: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
struct BlockDeviceSessionSnapshot {
    partition: BlockDevicePartition,
    access: BlockDeviceAccess,
    session: BlockDeviceSession,
}

#[derive(Clone, Copy)]
struct BlockDeviceTransfer {
    absolute_lba: u64,
    byte_length: usize,
    buffer_offset: usize,
}

static BLOCK_DEVICE_ENDPOINTS: PreemptMutex<BlockDeviceEndpointState> =
    PreemptMutex::new(BlockDeviceEndpointState::new());

pub fn configure_block_device_endpoints(inventory: &crate::partition::Inventory) -> (usize, usize) {
    let mut state = BLOCK_DEVICE_ENDPOINTS.lock();
    if state.configured {
        return block_device_endpoint_counts(&state);
    }

    let logical_block_size = inventory.disk_block_size;
    if logical_block_size == 0 || logical_block_size > block_device_protocol::MAX_TRANSFER_BYTES {
        state.configured = true;
        return (0, 0);
    }

    for partition in inventory.filesystem_candidates() {
        let Some(end_lba) = partition.start_lba.checked_add(partition.block_count) else {
            continue;
        };
        if partition.block_count == 0 || end_lba > inventory.disk_block_count {
            continue;
        }
        let partition_snapshot = BlockDevicePartition {
            index: partition.index,
            start_lba: partition.start_lba,
            block_count: partition.block_count,
            logical_block_size,
        };
        state.devices.push(BlockDeviceEndpoint {
            partition: partition_snapshot,
            access: BlockDeviceAccess::ReadOnly,
            filesystem_uuid: None,
            endpoint: None,
            generation: 0,
            online: true,
            next_session_id: 1,
            sessions: Vec::new(),
        });
        let nullfs_uuid = if matches!(partition.kind, crate::partition::PartitionKind::NullFs)
            && block_device_partition_avoids_disk_metadata(inventory, partition)
            && block_device_partition_is_exclusive(inventory, partition)
        {
            block_device_partition_nullfs_uuid(partition_snapshot)
        } else {
            None
        };
        if let Some(filesystem_uuid) = nullfs_uuid {
            state.devices.push(BlockDeviceEndpoint {
                partition: partition_snapshot,
                access: BlockDeviceAccess::Writable,
                filesystem_uuid: Some(filesystem_uuid),
                endpoint: None,
                generation: 0,
                online: true,
                next_session_id: 1,
                sessions: Vec::new(),
            });
        }
    }
    state.configured = true;
    block_device_endpoint_counts(&state)
}

fn block_device_partition_avoids_disk_metadata(
    inventory: &crate::partition::Inventory,
    partition: &crate::partition::Partition,
) -> bool {
    match inventory.table_kind {
        crate::partition::TableKind::Mbr => {
            (1..=4).contains(&partition.index)
                && partition.start_lba > 0
                && !inventory
                    .partitions()
                    .iter()
                    .any(|other| matches!(other.kind, crate::partition::PartitionKind::Extended))
        }
        crate::partition::TableKind::Gpt | crate::partition::TableKind::SuperFloppy => false,
    }
}

fn block_device_partition_is_exclusive(
    inventory: &crate::partition::Inventory,
    partition: &crate::partition::Partition,
) -> bool {
    inventory
        .partitions()
        .iter()
        .filter(|other| other.index != partition.index)
        .all(|other| !block_device_partitions_overlap(partition, other))
}

fn block_device_partitions_overlap(
    left: &crate::partition::Partition,
    right: &crate::partition::Partition,
) -> bool {
    let Some(left_end) = left.start_lba.checked_add(left.block_count) else {
        return true;
    };
    let Some(right_end) = right.start_lba.checked_add(right.block_count) else {
        return true;
    };
    left.start_lba < right_end && right.start_lba < left_end
}

fn block_device_partition_nullfs_uuid(partition: BlockDevicePartition) -> Option<[u8; 16]> {
    let block_size = partition.logical_block_size;
    if block_size == 0
        || !nullfs_format::BLOCK_SIZE.is_multiple_of(block_size)
        || !nullfs_format::SUPERBLOCK_OFFSET.is_multiple_of(block_size as u64)
    {
        return None;
    }
    let first_relative_lba = nullfs_format::SUPERBLOCK_OFFSET / block_size as u64;
    let protocol_blocks = nullfs_format::BLOCK_SIZE / block_size;
    let protocol_blocks_u64 = u64::try_from(protocol_blocks).ok()?;
    let end_relative_lba = first_relative_lba.checked_add(protocol_blocks_u64)?;
    if end_relative_lba > partition.block_count {
        return None;
    }

    let mut encoded = [0_u8; nullfs_format::BLOCK_SIZE];
    for relative in 0..protocol_blocks {
        let lba = partition
            .start_lba
            .checked_add(first_relative_lba)?
            .checked_add(relative as u64)?;
        let offset = relative * block_size;
        crate::ahci::read_block(lba, &mut encoded[offset..offset + block_size]).ok()?;
    }
    let device_bytes = partition
        .block_count
        .checked_mul(partition.logical_block_size as u64)?;
    let superblock = nullfs_format::Superblock::decode(
        &encoded,
        Some(device_bytes),
        nullfs_format::MountMode::ReadOnly,
    )
    .ok()?;
    Some(superblock.filesystem_uuid)
}

fn block_device_endpoint_counts(state: &BlockDeviceEndpointState) -> (usize, usize) {
    let read_only = state
        .devices
        .iter()
        .filter(|device| device.access == BlockDeviceAccess::ReadOnly)
        .count();
    let writable = state
        .devices
        .iter()
        .filter(|device| device.access == BlockDeviceAccess::Writable)
        .count();
    (read_only, writable)
}



fn open_writable_nullfs_block_device_endpoint(
    process_id: u64,
    uuid_address: u64,
    uuid_length: u64,
) -> u64 {
    if process_id != INIT_PROCESS_ID {
        return error_return(abi::errno::PERMISSION);
    }
    if uuid_length != 16 {
        return error_return(ERR_INVALID_ARGUMENT);
    }
    if !user_range_allows(process_id, uuid_address, 16, false) {
        return error_return(ERR_BAD_ADDRESS);
    }
    let filesystem_uuid = unsafe { ptr::read_unaligned(uuid_address as *const [u8; 16]) };
    if filesystem_uuid == [0; 16] {
        return error_return(ERR_INVALID_ARGUMENT);
    }
    let partition_index = {
        let state = BLOCK_DEVICE_ENDPOINTS.lock();
        let candidates = state.devices.iter().filter_map(|device| {
            if device.access != BlockDeviceAccess::Writable {
                return None;
            }
            device
                .filesystem_uuid
                .map(|uuid| (device.partition.index, uuid))
        });
        match crate::nullfs_volume_selection::select_unique_partition_by_uuid(
            candidates,
            filesystem_uuid,
        ) {
            Ok(index) => index,
            Err(crate::nullfs_volume_selection::SelectionError::Missing) => {
                return error_return(ERR_NO_ENTRY);
            }
            Err(crate::nullfs_volume_selection::SelectionError::Ambiguous) => {
                return error_return(ERR_INVALID_ARGUMENT);
            }
        }
    };
    open_block_device_endpoint(
        process_id,
        u64::from(partition_index),
        BlockDeviceAccess::Writable,
    )
}

fn offline_writable_nullfs_block_device_endpoint(
    process_id: u64,
    uuid_address: u64,
    uuid_length: u64,
    expected_generation: u64,
) -> u64 {
    if process_id != INIT_PROCESS_ID {
        return error_return(abi::errno::PERMISSION);
    }
    if uuid_length != 16 || expected_generation == 0 {
        return error_return(ERR_INVALID_ARGUMENT);
    }
    if !user_range_allows(process_id, uuid_address, 16, false) {
        return error_return(ERR_BAD_ADDRESS);
    }
    let filesystem_uuid = unsafe { ptr::read_unaligned(uuid_address as *const [u8; 16]) };
    if filesystem_uuid == [0; 16] {
        return error_return(ERR_INVALID_ARGUMENT);
    }

    let mut state = BLOCK_DEVICE_ENDPOINTS.lock();
    let mut matched_index = None;
    for (index, device) in state.devices.iter().enumerate() {
        if device.access == BlockDeviceAccess::Writable
            && device.filesystem_uuid == Some(filesystem_uuid)
            && matched_index.replace(index).is_some()
        {
            return error_return(ERR_INVALID_ARGUMENT);
        }
    }
    let Some(matched_index) = matched_index else {
        return error_return(ERR_NO_ENTRY);
    };
    let device = &mut state.devices[matched_index];
    if device.endpoint.is_none()
        || device.generation != expected_generation
        || !device.online
    {
        return error_return(ERR_INVALID_ARGUMENT);
    }
    device.online = false;
    crate::serial_println!(
        "writable NullFS block endpoint offlined: partition={}, generation={}",
        device.partition.index,
        device.generation
    );
    0
}

fn open_block_device_endpoint(
    process_id: u64,
    partition_index: u64,
    access: BlockDeviceAccess,
) -> u64 {
    if process_id != INIT_PROCESS_ID {
        return error_return(abi::errno::PERMISSION);
    }
    let partition_index = match u32::try_from(partition_index) {
        Ok(index) => index,
        Err(_) => return error_return(ERR_INVALID_ARGUMENT),
    };

    let existing =
        {
            let state = BLOCK_DEVICE_ENDPOINTS.lock();
            let Some(device) = state.devices.iter().find(|device| {
                device.partition.index == partition_index && device.access == access
            }) else {
                let error = if access.is_writable()
                    && state
                        .devices
                        .iter()
                        .any(|device| device.partition.index == partition_index)
                {
                    abi::errno::PERMISSION
                } else {
                    ERR_NO_ENTRY
                };
                return error_return(error);
            };
            if !device.online {
                return error_return(ERR_IO);
            }
            device.endpoint
        };
    if let Some(endpoint) = existing {
        return block_device_insert_bootstrap_handle(process_id, endpoint);
    }

    let (endpoint, handle) = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        let endpoint = match registry.create_object(
            abi::capability::KIND_ENDPOINT,
            CapabilityObjectData::Endpoint(EndpointObject {
                queue: alloc::collections::VecDeque::with_capacity(
                    abi::limits::MAX_ENDPOINT_MESSAGES,
                ),
                peer: EndpointPeer::Loopback,
            }),
        ) {
            Ok(endpoint) => endpoint,
            Err(error) => return error_return(error),
        };
        let rights = abi::capability::RIGHT_SEND | abi::capability::RIGHT_TRANSFER;
        let handle = match registry.insert_entry(process_id, endpoint, rights) {
            Ok(handle) => handle,
            Err(error) => {
                registry.collect_garbage();
                return error_return(error);
            }
        };
        (endpoint, handle)
    };
    kernel_capability_root_add(endpoint);

    let committed = {
        let mut state = BLOCK_DEVICE_ENDPOINTS.lock();
        match state
            .devices
            .iter_mut()
            .find(|device| device.partition.index == partition_index && device.access == access)
        {
            Some(device) if device.endpoint.is_none() => {
                device.endpoint = Some(endpoint);
                device.generation = endpoint.id;
                true
            }
            Some(_) | None => false,
        }
    };
    if committed {
        return handle;
    }

    kernel_capability_root_remove(endpoint);
    {
        let mut registry = CAPABILITY_REGISTRY.lock();
        registry.remove_entry(process_id, handle);
        registry.collect_garbage();
    }
    let existing = {
        let state = BLOCK_DEVICE_ENDPOINTS.lock();
        state
            .devices
            .iter()
            .find(|device| device.partition.index == partition_index && device.access == access)
            .and_then(|device| device.endpoint)
    };
    match existing {
        Some(endpoint) => block_device_insert_bootstrap_handle(process_id, endpoint),
        None => error_return(ERR_IO),
    }
}

fn block_device_insert_bootstrap_handle(process_id: u64, endpoint: CapabilityObjectRef) -> u64 {
    let rights = abi::capability::RIGHT_SEND | abi::capability::RIGHT_TRANSFER;
    let mut registry = CAPABILITY_REGISTRY.lock();
    match registry.insert_entry(process_id, endpoint, rights) {
        Ok(handle) => handle,
        Err(error) => error_return(error),
    }
}

fn service_block_device_endpoints() {
    block_device_reap_dead_sessions();
    let Some((partition_index, access, message)) = block_device_take_message() else {
        return;
    };

    if message.capabilities.len() > 1 {
        block_device_release_transferred_many(&message.capabilities);
        return;
    }
    let capability = message.capabilities.first().copied();

    if message.bytes.len() != size_of::<block_device_protocol::Request>() {
        block_device_release_transferred(capability);
        return;
    }
    let request = unsafe {
        ptr::read_unaligned(
            message
                .bytes
                .as_ptr()
                .cast::<block_device_protocol::Request>(),
        )
    };

    if request.operation == block_device_protocol::operation::CONNECT {
        block_device_connect(
            partition_index,
            access,
            message.sender_process_id,
            request,
            capability,
        );
        return;
    }

    let Some(snapshot) = block_device_session_snapshot(
        partition_index,
        access,
        message.sender_process_id,
        request.session_id,
    ) else {
        block_device_release_transferred(capability);
        return;
    };
    let mut reply = block_device_reply(&request);
    if request.generation != snapshot.session.generation {
        reply.status = block_device_protocol::status::STALE_SESSION;
        block_device_release_transferred(capability);
        block_device_queue_reply_or_remove_session(snapshot, reply);
        return;
    }
    if !canonical_block_device_request(&request) {
        reply.status = block_device_protocol::status::INVALID;
        block_device_release_transferred(capability);
        block_device_queue_reply_or_remove_session(snapshot, reply);
        return;
    }
    if request.operation != block_device_protocol::operation::DISCONNECT
        && !block_device_endpoint_online(
            snapshot.partition.index,
            snapshot.access,
            snapshot.session.generation,
        )
    {
        reply.status = block_device_protocol::status::IO;
        block_device_release_transferred(capability);
        block_device_queue_reply_or_remove_session(snapshot, reply);
        return;
    }

    match request.operation {
        block_device_protocol::operation::ATTACH_BUFFER => {
            block_device_attach_buffer(snapshot, request, capability, &mut reply)
        }
        block_device_protocol::operation::INFO => {
            if block_device_reject_unexpected_transfer(capability, &mut reply) {
                reply.features = snapshot.access.features();
                reply.block_count = snapshot.partition.block_count;
                reply.logical_block_size = snapshot.partition.logical_block_size as u32;
                reply.device_flags = snapshot.access.device_flags();
            }
        }
        block_device_protocol::operation::READ => {
            if block_device_reject_unexpected_transfer(capability, &mut reply) {
                block_device_read(snapshot, &request, &mut reply);
            }
        }
        block_device_protocol::operation::WRITE => {
            if block_device_reject_unexpected_transfer(capability, &mut reply) {
                if snapshot.access.is_writable() {
                    block_device_write(snapshot, &request, &mut reply);
                } else {
                    reply.status = block_device_protocol::status::READ_ONLY;
                }
            }
        }
        block_device_protocol::operation::FLUSH => {
            if block_device_reject_unexpected_transfer(capability, &mut reply) {
                if snapshot.access.is_writable() {
                    block_device_flush(&mut reply);
                } else {
                    reply.status = block_device_protocol::status::NOT_SUPPORTED;
                }
            }
        }
        block_device_protocol::operation::DISCONNECT => {
            if !block_device_reject_unexpected_transfer(capability, &mut reply) {
                block_device_queue_reply_or_remove_session(snapshot, reply);
                return;
            }
            let Some(released) = block_device_remove_session(
                partition_index,
                snapshot.access,
                snapshot.session.owner_process_id,
                snapshot.session.id,
            ) else {
                reply.status = block_device_protocol::status::STALE_SESSION;
                block_device_queue_reply_or_remove_session(snapshot, reply);
                return;
            };
            let _queued_before_cleanup = block_device_queue_reply(released.reply_endpoint, reply);
            block_device_release_session(released);
            return;
        }
        _ => reply.status = block_device_protocol::status::NOT_SUPPORTED,
    }
    block_device_queue_reply_or_remove_session(snapshot, reply);
}

fn block_device_take_message() -> Option<(u32, BlockDeviceAccess, EndpointMessage)> {
    let candidates = {
        let mut state = BLOCK_DEVICE_ENDPOINTS.lock();
        let endpoints = state
            .devices
            .iter()
            .filter_map(|device| {
                device
                    .endpoint
                    .map(|endpoint| (device.partition.index, device.access, endpoint))
            })
            .collect::<Vec<_>>();
        if endpoints.is_empty() {
            return None;
        }
        let start = state.poll_cursor % endpoints.len();
        state.poll_cursor = (start + 1) % endpoints.len();
        (0..endpoints.len())
            .map(|offset| endpoints[(start + offset) % endpoints.len()])
            .collect::<Vec<_>>()
    };

    for (partition_index, access, endpoint_object) in candidates {
        let message = {
            let mut registry = CAPABILITY_REGISTRY.lock();
            let Some(index) = registry.object_index(endpoint_object) else {
                continue;
            };
            let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[index].data else {
                continue;
            };
            if let Some(message) = endpoint.queue.front() {
                for capability in &message.capabilities {
                    kernel_capability_root_add(capability.object);
                }
            }
            endpoint.queue.pop_front()
        };
        if let Some(message) = message {
            return Some((partition_index, access, message));
        }
    }
    None
}

fn block_device_connect(
    partition_index: u32,
    access: BlockDeviceAccess,
    sender_process_id: u64,
    request: block_device_protocol::Request,
    capability: Option<TransferredCapability>,
) {
    let Some(capability) = capability else {
        return;
    };
    if capability.rights != abi::capability::RIGHT_SEND
        || !block_device_object_is_endpoint(capability.object)
    {
        block_device_release_roots(&[capability.object]);
        return;
    }

    let mut reply = block_device_reply(&request);
    if !canonical_block_device_request(&request) {
        reply.status = block_device_protocol::status::INVALID;
        block_device_queue_reply(capability.object, reply);
        block_device_release_roots(&[capability.object]);
        return;
    }

    let session = {
        let mut state = BLOCK_DEVICE_ENDPOINTS.lock();
        match state
            .devices
            .iter_mut()
            .find(|device| device.partition.index == partition_index && device.access == access)
        {
            None => Err(block_device_protocol::status::IO),
            Some(device) if !device.online => Err(block_device_protocol::status::IO),
            Some(device)
                if device.sessions.len() >= MAX_BLOCK_DEVICE_SESSIONS
                    || device
                        .sessions
                        .iter()
                        .any(|session| session.owner_process_id == sender_process_id) =>
            {
                Err(block_device_protocol::status::TRY_AGAIN)
            }
            Some(device) => {
                let id = device.next_session_id;
                match id.checked_add(1).filter(|next| *next != 0) {
                    Some(next) if id != block_device_protocol::INVALID_ID => {
                        device.next_session_id = next;
                        let session = BlockDeviceSession {
                            id,
                            generation: device.generation,
                            owner_process_id: sender_process_id,
                            reply_endpoint: capability.object,
                            buffer: None,
                        };
                        device.sessions.push(session);
                        Ok(session)
                    }
                    Some(_) | None => Err(block_device_protocol::status::TRY_AGAIN),
                }
            }
        }
    };

    let session = match session {
        Ok(session) => session,
        Err(status) => {
            reply.status = status;
            block_device_queue_reply(capability.object, reply);
            block_device_release_roots(&[capability.object]);
            return;
        }
    };
    reply.session_id = session.id;
    reply.generation = session.generation;
    if !block_device_queue_reply(session.reply_endpoint, reply)
        && let Some(released) =
            block_device_remove_session(partition_index, access, sender_process_id, session.id)
    {
        block_device_release_session(released);
    }
}

fn block_device_endpoint_online(
    partition_index: u32,
    access: BlockDeviceAccess,
    generation: u64,
) -> bool {
    let state = BLOCK_DEVICE_ENDPOINTS.lock();
    state.devices.iter().any(|device| {
        device.partition.index == partition_index
            && device.access == access
            && device.generation == generation
            && device.online
    })
}

fn block_device_attach_buffer(
    snapshot: BlockDeviceSessionSnapshot,
    request: block_device_protocol::Request,
    capability: Option<TransferredCapability>,
    reply: &mut block_device_protocol::Reply,
) {
    let Some(capability) = capability else {
        reply.status = block_device_protocol::status::INVALID;
        return;
    };
    let required_rights = abi::capability::RIGHT_READ | abi::capability::RIGHT_WRITE;
    let Some(actual_length) = block_device_shared_memory_length(capability.object) else {
        reply.status = block_device_protocol::status::INVALID;
        block_device_release_roots(&[capability.object]);
        return;
    };
    if capability.rights != required_rights || request.buffer_length > actual_length {
        reply.status = block_device_protocol::status::INVALID;
        block_device_release_roots(&[capability.object]);
        return;
    }

    let attached = {
        let mut state = BLOCK_DEVICE_ENDPOINTS.lock();
        state
            .devices
            .iter_mut()
            .find(|device| {
                device.partition.index == snapshot.partition.index
                    && device.access == snapshot.access
            })
            .and_then(|device| {
                device.sessions.iter_mut().find(|session| {
                    session.id == snapshot.session.id
                        && session.generation == snapshot.session.generation
                        && session.owner_process_id == snapshot.session.owner_process_id
                })
            })
            .is_some_and(|session| {
                if session.buffer.is_some() {
                    false
                } else {
                    session.buffer = Some(BlockDeviceBuffer {
                        id: request.buffer_id,
                        object: capability.object,
                        length: request.buffer_length,
                    });
                    true
                }
            })
    };
    if attached {
        reply.buffer_id = request.buffer_id;
    } else {
        reply.status = block_device_protocol::status::INVALID;
        block_device_release_roots(&[capability.object]);
    }
}

fn block_device_read(
    snapshot: BlockDeviceSessionSnapshot,
    request: &block_device_protocol::Request,
    reply: &mut block_device_protocol::Reply,
) {
    let Some(buffer) = snapshot.session.buffer else {
        reply.status = block_device_protocol::status::STALE_BUFFER;
        return;
    };
    if buffer.id != request.buffer_id {
        reply.status = block_device_protocol::status::STALE_BUFFER;
        return;
    }
    let transfer = match checked_block_device_transfer(snapshot.partition, buffer, request) {
        Some(transfer) => transfer,
        None => {
            reply.status = block_device_protocol::status::RANGE;
            return;
        }
    };

    let Some(disk) = crate::ahci::info() else {
        reply.status = block_device_protocol::status::IO;
        return;
    };
    let Ok(disk_block_size) = usize::try_from(disk.logical_block_size) else {
        reply.status = block_device_protocol::status::IO;
        return;
    };
    let Some(partition_end) = snapshot
        .partition
        .start_lba
        .checked_add(snapshot.partition.block_count)
    else {
        reply.status = block_device_protocol::status::RANGE;
        return;
    };
    let Some(transfer_end) = transfer
        .absolute_lba
        .checked_add(u64::from(request.block_count))
    else {
        reply.status = block_device_protocol::status::RANGE;
        return;
    };
    if disk_block_size != snapshot.partition.logical_block_size
        || partition_end > disk.logical_block_count
        || transfer_end > partition_end
        || transfer_end > disk.logical_block_count
    {
        reply.status = block_device_protocol::status::RANGE;
        return;
    }

    let mut scratch = vec![0_u8; transfer.byte_length];
    for relative in 0..request.block_count {
        let Some(lba) = transfer.absolute_lba.checked_add(u64::from(relative)) else {
            reply.status = block_device_protocol::status::RANGE;
            return;
        };
        let Some(offset) = usize::try_from(relative)
            .ok()
            .and_then(|relative| relative.checked_mul(snapshot.partition.logical_block_size))
        else {
            reply.status = block_device_protocol::status::RANGE;
            return;
        };
        let end = offset + snapshot.partition.logical_block_size;
        if crate::ahci::read_block(lba, &mut scratch[offset..end]).is_err() {
            reply.status = block_device_protocol::status::IO;
            return;
        }
    }

    if !block_device_copy_to_shared_memory(buffer.object, transfer.buffer_offset, &scratch) {
        reply.status = block_device_protocol::status::IO;
        return;
    }
    reply.buffer_id = request.buffer_id;
    reply.transferred_blocks = request.block_count;
}

fn block_device_write(
    snapshot: BlockDeviceSessionSnapshot,
    request: &block_device_protocol::Request,
    reply: &mut block_device_protocol::Reply,
) {
    let Some(buffer) = snapshot.session.buffer else {
        reply.status = block_device_protocol::status::STALE_BUFFER;
        return;
    };
    if buffer.id != request.buffer_id {
        reply.status = block_device_protocol::status::STALE_BUFFER;
        return;
    }
    let transfer = match checked_block_device_transfer(snapshot.partition, buffer, request) {
        Some(transfer) => transfer,
        None => {
            reply.status = block_device_protocol::status::RANGE;
            return;
        }
    };

    let Some(disk) = crate::ahci::info() else {
        reply.status = block_device_protocol::status::IO;
        return;
    };
    let Ok(disk_block_size) = usize::try_from(disk.logical_block_size) else {
        reply.status = block_device_protocol::status::IO;
        return;
    };
    let Some(partition_end) = snapshot
        .partition
        .start_lba
        .checked_add(snapshot.partition.block_count)
    else {
        reply.status = block_device_protocol::status::RANGE;
        return;
    };
    let Some(transfer_end) = transfer
        .absolute_lba
        .checked_add(u64::from(request.block_count))
    else {
        reply.status = block_device_protocol::status::RANGE;
        return;
    };
    if disk_block_size != snapshot.partition.logical_block_size
        || partition_end > disk.logical_block_count
        || transfer_end > partition_end
        || transfer_end > disk.logical_block_count
    {
        reply.status = block_device_protocol::status::RANGE;
        return;
    }

    let mut scratch = vec![0_u8; transfer.byte_length];
    if !block_device_copy_from_shared_memory(buffer.object, transfer.buffer_offset, &mut scratch) {
        reply.status = block_device_protocol::status::IO;
        return;
    }
    for relative in 0..request.block_count {
        let Some(lba) = transfer.absolute_lba.checked_add(u64::from(relative)) else {
            reply.status = block_device_protocol::status::RANGE;
            return;
        };
        let Some(offset) = usize::try_from(relative)
            .ok()
            .and_then(|relative| relative.checked_mul(snapshot.partition.logical_block_size))
        else {
            reply.status = block_device_protocol::status::RANGE;
            return;
        };
        let end = offset + snapshot.partition.logical_block_size;
        if crate::ahci::write_block(lba, &scratch[offset..end]).is_err() {
            reply.status = block_device_protocol::status::IO;
            return;
        }
    }

    reply.buffer_id = request.buffer_id;
    reply.transferred_blocks = request.block_count;
}

fn block_device_flush(reply: &mut block_device_protocol::Reply) {
    if crate::ahci::flush().is_err() {
        reply.status = block_device_protocol::status::IO;
    }
}

fn checked_block_device_transfer(
    partition: BlockDevicePartition,
    buffer: BlockDeviceBuffer,
    request: &block_device_protocol::Request,
) -> Option<BlockDeviceTransfer> {
    if request.block_count == 0 || partition.logical_block_size == 0 {
        return None;
    }
    let transfer_end = request
        .block_offset
        .checked_add(u64::from(request.block_count))?;
    if transfer_end > partition.block_count {
        return None;
    }
    let byte_length_u64 = u64::from(request.block_count)
        .checked_mul(u64::try_from(partition.logical_block_size).ok()?)?;
    let byte_length = usize::try_from(byte_length_u64).ok()?;
    if byte_length == 0
        || byte_length > block_device_protocol::MAX_TRANSFER_BYTES
        || request.buffer_length != byte_length_u64
    {
        return None;
    }
    let buffer_end = request.buffer_offset.checked_add(byte_length_u64)?;
    if buffer_end > buffer.length {
        return None;
    }
    Some(BlockDeviceTransfer {
        absolute_lba: partition.start_lba.checked_add(request.block_offset)?,
        byte_length,
        buffer_offset: usize::try_from(request.buffer_offset).ok()?,
    })
}

fn canonical_block_device_request(request: &block_device_protocol::Request) -> bool {
    if request.version != block_device_protocol::VERSION
        || !block_device_protocol::operation::is_defined(request.operation)
        || request.flags & !block_device_protocol::request_flags::ALL != 0
        || request.request_id == block_device_protocol::INVALID_ID
        || request.reserved != [0; 3]
    {
        return false;
    }

    let empty_transfer = request.buffer_id == block_device_protocol::INVALID_ID
        && request.buffer_offset == 0
        && request.buffer_length == 0
        && request.block_offset == 0
        && request.block_count == 0;
    match request.operation {
        block_device_protocol::operation::CONNECT => {
            request.session_id == block_device_protocol::INVALID_ID
                && request.generation == 0
                && empty_transfer
        }
        block_device_protocol::operation::ATTACH_BUFFER => {
            request.session_id != block_device_protocol::INVALID_ID
                && request.generation != 0
                && request.buffer_id != block_device_protocol::INVALID_ID
                && request.buffer_offset == 0
                && request.buffer_length != 0
                && request.block_offset == 0
                && request.block_count == 0
        }
        block_device_protocol::operation::INFO
        | block_device_protocol::operation::FLUSH
        | block_device_protocol::operation::DISCONNECT => {
            request.session_id != block_device_protocol::INVALID_ID
                && request.generation != 0
                && empty_transfer
        }
        block_device_protocol::operation::READ | block_device_protocol::operation::WRITE => {
            request.session_id != block_device_protocol::INVALID_ID
                && request.generation != 0
                && request.buffer_id != block_device_protocol::INVALID_ID
                && request.buffer_length != 0
                && request.block_count != 0
                && request.buffer_end().is_some()
        }
        _ => false,
    }
}

fn block_device_reply(request: &block_device_protocol::Request) -> block_device_protocol::Reply {
    let mut reply = block_device_protocol::Reply::EMPTY;
    reply.operation = request.operation;
    reply.request_id = request.request_id;
    if request.operation != block_device_protocol::operation::CONNECT {
        reply.session_id = request.session_id;
        reply.generation = request.generation;
    }
    reply
}

fn block_device_session_snapshot(
    partition_index: u32,
    access: BlockDeviceAccess,
    owner_process_id: u64,
    session_id: u64,
) -> Option<BlockDeviceSessionSnapshot> {
    let state = BLOCK_DEVICE_ENDPOINTS.lock();
    let device = state
        .devices
        .iter()
        .find(|device| device.partition.index == partition_index && device.access == access)?;
    let session = device
        .sessions
        .iter()
        .find(|session| session.id == session_id && session.owner_process_id == owner_process_id)?;
    device.endpoint?;
    Some(BlockDeviceSessionSnapshot {
        partition: device.partition,
        access: device.access,
        session: *session,
    })
}

fn block_device_remove_session(
    partition_index: u32,
    access: BlockDeviceAccess,
    owner_process_id: u64,
    session_id: u64,
) -> Option<BlockDeviceSession> {
    let mut state = BLOCK_DEVICE_ENDPOINTS.lock();
    let device = state
        .devices
        .iter_mut()
        .find(|device| device.partition.index == partition_index && device.access == access)?;
    let index = device.sessions.iter().position(|session| {
        session.id == session_id && session.owner_process_id == owner_process_id
    })?;
    Some(device.sessions.remove(index))
}

fn block_device_queue_reply_or_remove_session(
    snapshot: BlockDeviceSessionSnapshot,
    reply: block_device_protocol::Reply,
) {
    if block_device_queue_reply(snapshot.session.reply_endpoint, reply) {
        return;
    }
    if let Some(released) = block_device_remove_session(
        snapshot.partition.index,
        snapshot.access,
        snapshot.session.owner_process_id,
        snapshot.session.id,
    ) {
        block_device_release_session(released);
    }
}

fn block_device_reap_dead_sessions() {
    let live_processes = {
        let manager = PROCESS_MANAGER.lock();
        manager
            .processes
            .iter()
            .filter(|process| process.is_live())
            .map(|process| process.process_id)
            .collect::<Vec<_>>()
    };
    let released = {
        let mut state = BLOCK_DEVICE_ENDPOINTS.lock();
        let mut released = Vec::new();
        for device in &mut state.devices {
            let mut index = 0;
            while index < device.sessions.len() {
                if live_processes.contains(&device.sessions[index].owner_process_id) {
                    index += 1;
                } else {
                    released.push(device.sessions.remove(index));
                }
            }
        }
        released
    };
    if released.is_empty() {
        return;
    }
    for session in released {
        block_device_release_session_without_collect(session);
    }
    CAPABILITY_REGISTRY.lock().collect_garbage();
}

fn block_device_release_session(session: BlockDeviceSession) {
    block_device_release_session_without_collect(session);
    CAPABILITY_REGISTRY.lock().collect_garbage();
}

fn block_device_release_session_without_collect(session: BlockDeviceSession) {
    kernel_capability_root_remove(session.reply_endpoint);
    if let Some(buffer) = session.buffer {
        kernel_capability_root_remove(buffer.object);
    }
}

fn block_device_release_transferred(capability: Option<TransferredCapability>) {
    if let Some(capability) = capability {
        block_device_release_roots(&[capability.object]);
    }
}

fn block_device_release_transferred_many(capabilities: &[TransferredCapability]) {
    let objects = capabilities
        .iter()
        .map(|capability| capability.object)
        .collect::<Vec<_>>();
    block_device_release_roots(&objects);
}

fn block_device_release_roots(objects: &[CapabilityObjectRef]) {
    if objects.is_empty() {
        return;
    }
    for object in objects {
        kernel_capability_root_remove(*object);
    }
    CAPABILITY_REGISTRY.lock().collect_garbage();
}

fn block_device_reject_unexpected_transfer(
    capability: Option<TransferredCapability>,
    reply: &mut block_device_protocol::Reply,
) -> bool {
    if capability.is_none() {
        return true;
    }
    reply.status = block_device_protocol::status::INVALID;
    block_device_release_transferred(capability);
    false
}

fn block_device_object_is_endpoint(object: CapabilityObjectRef) -> bool {
    if object.kind != abi::capability::KIND_ENDPOINT {
        return false;
    }
    let registry = CAPABILITY_REGISTRY.lock();
    let Some(index) = registry.object_index(object) else {
        return false;
    };
    matches!(
        registry.objects[index].data,
        CapabilityObjectData::Endpoint(_)
    )
}

fn block_device_shared_memory_length(object: CapabilityObjectRef) -> Option<u64> {
    if object.kind != abi::capability::KIND_SHARED_MEMORY {
        return None;
    }
    let registry = CAPABILITY_REGISTRY.lock();
    let index = registry.object_index(object)?;
    let CapabilityObjectData::SharedMemory(memory) = &registry.objects[index].data else {
        return None;
    };
    u64::try_from(memory.bytes.len()).ok()
}

fn block_device_copy_from_shared_memory(
    object: CapabilityObjectRef,
    offset: usize,
    bytes: &mut [u8],
) -> bool {
    let registry = CAPABILITY_REGISTRY.lock();
    let Some(index) = registry.object_index(object) else {
        return false;
    };
    let CapabilityObjectData::SharedMemory(memory) = &registry.objects[index].data else {
        return false;
    };
    let Some(end) = offset.checked_add(bytes.len()) else {
        return false;
    };
    let Some(source) = memory.bytes.get(offset..end) else {
        return false;
    };
    bytes.copy_from_slice(source);
    true
}

fn block_device_copy_to_shared_memory(
    object: CapabilityObjectRef,
    offset: usize,
    bytes: &[u8],
) -> bool {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(index) = registry.object_index(object) else {
        return false;
    };
    let CapabilityObjectData::SharedMemory(memory) = &mut registry.objects[index].data else {
        return false;
    };
    let Some(end) = offset.checked_add(bytes.len()) else {
        return false;
    };
    let Some(destination) = memory.bytes.get_mut(offset..end) else {
        return false;
    };
    destination.copy_from_slice(bytes);
    true
}

fn block_device_queue_reply(
    endpoint_object: CapabilityObjectRef,
    reply: block_device_protocol::Reply,
) -> bool {
    let bytes = unsafe {
        slice::from_raw_parts(
            (&reply as *const block_device_protocol::Reply).cast::<u8>(),
            size_of::<block_device_protocol::Reply>(),
        )
    }
    .to_vec();
    let queued = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        let Some(index) = registry.object_index(endpoint_object) else {
            return false;
        };
        let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[index].data else {
            return false;
        };
        if endpoint.queue.len() >= abi::limits::MAX_ENDPOINT_MESSAGES {
            false
        } else {
            endpoint.queue.push_back(EndpointMessage {
                sender_process_id: 0,
                bytes,
                capabilities: Vec::new(),
            });
            true
        }
    };
    if queued {
        wake_endpoint_waiter(endpoint_object);
    }
    queued
}

const _: () = assert!(
    size_of::<block_device_protocol::Request>() <= block_device_protocol::MAX_MESSAGE_BYTES
);
const _: () =
    assert!(size_of::<block_device_protocol::Reply>() <= block_device_protocol::MAX_MESSAGE_BYTES);

#[cfg(test)]
mod block_device_endpoint_tests {
    use super::{
        BlockDeviceBuffer, BlockDevicePartition, canonical_block_device_request,
        checked_block_device_transfer,
    };
    use super::{CapabilityObjectRef, block_device_protocol};

    fn read_request() -> block_device_protocol::Request {
        let mut request = block_device_protocol::Request::EMPTY;
        request.operation = block_device_protocol::operation::READ;
        request.request_id = 1;
        request.session_id = 2;
        request.generation = 3;
        request.buffer_id = 4;
        request.buffer_length = 1024;
        request.block_offset = 6;
        request.block_count = 2;
        request
    }

    #[test]
    fn canonical_requests_reject_reserved_and_overflowing_buffer_fields() {
        let mut request = read_request();
        assert!(canonical_block_device_request(&request));
        request.reserved[0] = 1;
        assert!(!canonical_block_device_request(&request));
        request.reserved = [0; 3];
        request.buffer_offset = u64::MAX;
        assert!(!canonical_block_device_request(&request));
    }

    #[test]
    fn checked_transfer_is_partition_relative_and_bounded_to_four_kibibytes() {
        let partition = BlockDevicePartition {
            index: 2,
            start_lba: 100,
            block_count: 16,
            logical_block_size: 512,
        };
        let buffer = BlockDeviceBuffer {
            id: 4,
            object: CapabilityObjectRef { id: 7, kind: 3 },
            length: 4096,
        };
        let request = read_request();
        let transfer = checked_block_device_transfer(partition, buffer, &request).unwrap();
        assert_eq!(transfer.absolute_lba, 106);
        assert_eq!(transfer.byte_length, 1024);

        let mut oversized = request;
        oversized.block_count = 9;
        oversized.buffer_length = 9 * 512;
        assert!(checked_block_device_transfer(partition, buffer, &oversized).is_none());
    }
}
