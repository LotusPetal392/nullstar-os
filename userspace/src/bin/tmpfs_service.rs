#![no_std]
#![no_main]

use core::{mem::size_of, slice};

use userspace::{
    args::Args,
    filesystem::protocol as filesystem_protocol,
    filesystem_service::{Error as SessionError, NodeReference, NodeReferenceError, SessionTable},
    handle::Endpoint,
    ipc::{self, ObjectKind, ReceivedCapability, Rights},
    managed_startup::{ManagedServiceIdentity, numeric_service_id, receive_managed_service_start},
    platform,
    runtime_context::{CapabilityRole, StartupCapabilityPolicy},
    service_control::TMPFS_SERVICE_ID,
    syscall,
    tmpfs::protocol,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const EXECUTABLE_ID: u64 = 3;
const SERVICE_IDENTITY: ManagedServiceIdentity = ManagedServiceIdentity::new(
    EXECUTABLE_ID,
    numeric_service_id(TMPFS_SERVICE_ID.into_bytes()),
    EXECUTABLE_ID,
);
const READY_MESSAGE: &[u8] = b"service-ready: tmpfs";
const CONTAINMENT_TEST_ARGUMENT: &[u8] = b"--containment-test";
const CONTAINMENT_DESCENDANT_MARKER: &[u8] =
    b"tmpfs-service: containment descendant escaped process group\n";

#[derive(Clone, Copy)]
struct File {
    used: bool,
    linked: bool,
    node_id: u64,
    open_count: u32,
    name_length: usize,
    name: [u8; protocol::MAX_NAME_BYTES],
    length: usize,
    data: [u8; protocol::MAX_FILE_BYTES],
}

impl File {
    const EMPTY: Self = Self {
        used: false,
        linked: false,
        node_id: filesystem_protocol::INVALID_ID,
        open_count: 0,
        name_length: 0,
        name: [0; protocol::MAX_NAME_BYTES],
        length: 0,
        data: [0; protocol::MAX_FILE_BYTES],
    };

    fn named(&self, name: &[u8]) -> bool {
        self.used
            && self.linked
            && self.name_length == name.len()
            && &self.name[..self.name_length] == name
    }
}

fn spawn_containment_descendant() {
    let group_ready = match syscall::pipe_pair() {
        Ok(pair) => pair,
        Err(_) => syscall::exit(20),
    };
    match syscall::fork() {
        Ok(0) => {
            let _ = syscall::close(group_ready.reader);
            if platform::set_process_group(0, 0).is_err()
                || syscall::write_all(group_ready.writer, &[1]).is_err()
                || syscall::close(group_ready.writer).is_err()
            {
                syscall::exit(21);
            }
            loop {
                if syscall::yield_now().is_err() {
                    syscall::exit(22);
                }
            }
        }
        Ok(_) => {
            let _ = syscall::close(group_ready.writer);
            let mut ready = [0_u8; 1];
            let escaped = loop {
                match syscall::read(group_ready.reader, &mut ready) {
                    Ok(1) => break ready[0] == 1,
                    Ok(_) => break false,
                    Err(error) if error == syscall::Errno::INTERRUPTED => {}
                    Err(_) => break false,
                }
            };
            if syscall::close(group_ready.reader).is_err()
                || !escaped
                || syscall::write_all(syscall::STDOUT, CONTAINMENT_DESCENDANT_MARKER).is_err()
            {
                syscall::exit(23);
            }
        }
        Err(_) => {
            let _ = syscall::close(group_ready.writer);
            let _ = syscall::close(group_ready.reader);
            syscall::exit(20);
        }
    }
}

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let containment_test =
        arguments.len() == 2 && arguments.get(1) == Some(CONTAINMENT_TEST_ARGUMENT);
    if !(arguments.len() == 1 || containment_test) {
        syscall::exit(1);
    }

    let policies = [
        StartupCapabilityPolicy {
            role: CapabilityRole::READINESS,
            kind: ObjectKind::Endpoint,
            minimum_rights: Rights::SEND,
            maximum_rights: Rights::SEND,
            required: true,
        },
        StartupCapabilityPolicy {
            role: CapabilityRole::SERVICE_REQUEST,
            kind: ObjectKind::Endpoint,
            minimum_rights: Rights::RECEIVE,
            maximum_rights: Rights::RECEIVE,
            required: true,
        },
    ];
    let mut start = match receive_managed_service_start::<2>(arguments, &policies, SERVICE_IDENTITY)
    {
        Ok(start) => start,
        Err(_) => syscall::exit(2),
    };
    let readiness = match start
        .context
        .take::<Endpoint>(CapabilityRole::READINESS, Rights::SEND)
    {
        Ok(handle) => handle.into_raw(),
        Err(_) => syscall::exit(3),
    };
    let request = match start
        .context
        .take::<Endpoint>(CapabilityRole::SERVICE_REQUEST, Rights::RECEIVE)
    {
        Ok(handle) if start.context.is_empty() => handle.into_raw(),
        _ => syscall::exit(3),
    };
    let generation = start.generation;
    let legacy_generation = match u32::try_from(generation) {
        Ok(generation) => generation,
        Err(_) => syscall::exit(4),
    };
    if containment_test && generation == 1 {
        spawn_containment_descendant();
    }

    let _ = syscall::write_all(syscall::STDERR, b"tmpfs: sending readiness message\n");
    if ipc::send(readiness, READY_MESSAGE, None).is_err() || ipc::close(readiness).is_err() {
        let _ = syscall::write_all(syscall::STDERR, b"tmpfs: readiness send failed\n");
        syscall::exit(5);
    }
    let _ = syscall::write_all(syscall::STDERR, b"tmpfs: ready\n");
    let mut files = [File::EMPTY; protocol::MAX_FILES];
    let mut next_node_id = filesystem_protocol::ROOT_NODE_ID + 1;
    let mut sessions = SessionTable::new();
    let mut request_bytes = [0_u8; userspace::abi::limits::MAX_IPC_MESSAGE_BYTES];
    loop {
        let message = match ipc::receive(request, &mut request_bytes) {
            Ok(message) => message,
            Err(_) => syscall::exit(6),
        };
        if message.bytes == size_of::<protocol::Request>() {
            dispatch_legacy_message(
                &mut files,
                &mut next_node_id,
                legacy_generation,
                &request_bytes,
                message.capability,
            );
        } else if message.bytes == size_of::<filesystem_protocol::Request>() {
            dispatch_filesystem_message(
                &mut files,
                &mut next_node_id,
                generation,
                &mut sessions,
                &request_bytes,
                message.capability,
            );
        } else if let Some(capability) = message.capability {
            let _ = ipc::close(capability.handle);
        }
    }
}

fn dispatch_legacy_message(
    files: &mut [File; protocol::MAX_FILES],
    next_node_id: &mut u64,
    generation: u32,
    request_bytes: &[u8],
    reply_capability: Option<ReceivedCapability>,
) {
    let Some(reply_capability) = reply_capability else {
        return;
    };
    if reply_capability.rights != Rights::SEND {
        let _ = ipc::close(reply_capability.handle);
        return;
    }
    let request =
        unsafe { core::ptr::read_unaligned(request_bytes.as_ptr() as *const protocol::Request) };
    let reply = dispatch(files, next_node_id, generation, &request);
    send_value(reply_capability.handle, &reply);
    let _ = ipc::close(reply_capability.handle);
}

fn dispatch_filesystem_message(
    files: &mut [File; protocol::MAX_FILES],
    next_node_id: &mut u64,
    generation: u64,
    sessions: &mut SessionTable,
    request_bytes: &[u8],
    capability: Option<ReceivedCapability>,
) {
    let request = unsafe {
        core::ptr::read_unaligned(request_bytes.as_ptr() as *const filesystem_protocol::Request)
    };
    if request.version != filesystem_protocol::VERSION
        || request.request_id == filesystem_protocol::INVALID_ID
        || request.reserved != [0; 3]
        || request.flags & !filesystem_protocol::request_flags::ALL != 0
    {
        if let Some(capability) = capability {
            let _ = ipc::close(capability.handle);
        }
        return;
    }

    if request.operation == filesystem_protocol::operation::CONNECT {
        connect_filesystem_session(generation, sessions, &request, capability);
        return;
    }

    let Ok(reply_endpoint) = sessions.reply_endpoint(request.session_id, request.generation) else {
        if let Some(capability) = capability {
            let _ = ipc::close(capability.handle);
        }
        return;
    };
    let mut reply = filesystem_reply(&request);
    match request.operation {
        filesystem_protocol::operation::DISCONNECT => {
            reject_unexpected_capability(capability, &mut reply);
            if reply.status == filesystem_protocol::status::OK
                && !canonical_empty_request_fields(&request)
            {
                reply.status = filesystem_protocol::status::INVALID;
            }
            if reply.status != filesystem_protocol::status::OK {
                send_value(reply_endpoint, &reply);
                return;
            }
            match sessions.disconnect(request.session_id, request.generation) {
                Ok(released) => {
                    release_session_nodes(files, released.node_references);
                    send_value(released.reply_endpoint, &reply);
                    for handle in released
                        .buffer_handles
                        .into_iter()
                        .filter(|handle| *handle != 0)
                    {
                        let _ = ipc::close(handle);
                    }
                    let _ = ipc::close(released.reply_endpoint);
                }
                Err(error) => {
                    reply.status = session_status(error);
                    send_value(reply_endpoint, &reply);
                }
            }
            return;
        }
        filesystem_protocol::operation::ATTACH_BUFFER => {
            attach_filesystem_buffer(sessions, &request, capability, &mut reply);
        }
        filesystem_protocol::operation::DETACH_BUFFER => {
            if let Some(capability) = capability {
                let _ = ipc::close(capability.handle);
                reply.status = filesystem_protocol::status::INVALID;
            } else {
                match sessions.detach_buffer(
                    request.session_id,
                    request.generation,
                    request.bulk.buffer_id,
                ) {
                    Ok(handle) => {
                        let _ = ipc::close(handle);
                    }
                    Err(error) => reply.status = session_status(error),
                }
            }
        }
        filesystem_protocol::operation::LOOKUP => {
            reject_unexpected_capability(capability, &mut reply);
            if reply.status == filesystem_protocol::status::OK {
                lookup_node(files, &request, &mut reply);
            }
        }
        filesystem_protocol::operation::GET_ATTRIBUTES => {
            reject_unexpected_capability(capability, &mut reply);
            if reply.status == filesystem_protocol::status::OK {
                get_node_attributes(files, &request, &mut reply);
            }
        }
        filesystem_protocol::operation::READ => {
            reject_unexpected_capability(capability, &mut reply);
            if reply.status == filesystem_protocol::status::OK {
                read_node_to_buffer(files, sessions, &request, &mut reply);
            }
        }
        filesystem_protocol::operation::WRITE => {
            reject_unexpected_capability(capability, &mut reply);
            if reply.status == filesystem_protocol::status::OK {
                write_node_from_buffer(files, sessions, &request, &mut reply);
            }
        }
        filesystem_protocol::operation::CREATE_FILE => {
            reject_unexpected_capability(capability, &mut reply);
            if reply.status == filesystem_protocol::status::OK {
                create_filesystem_file(files, next_node_id, &request, &mut reply);
            }
        }
        filesystem_protocol::operation::OPEN => {
            reject_unexpected_capability(capability, &mut reply);
            if reply.status == filesystem_protocol::status::OK {
                open_filesystem_node(files, next_node_id, sessions, &request, &mut reply);
            }
        }
        filesystem_protocol::operation::CLOSE_NODE => {
            reject_unexpected_capability(capability, &mut reply);
            if reply.status == filesystem_protocol::status::OK {
                close_filesystem_node(files, sessions, &request, &mut reply);
            }
        }
        filesystem_protocol::operation::READ_DIRECTORY => {
            reject_unexpected_capability(capability, &mut reply);
            if reply.status == filesystem_protocol::status::OK {
                read_directory_to_buffer(files, sessions, &request, &mut reply);
            }
        }
        filesystem_protocol::operation::UNLINK => {
            reject_unexpected_capability(capability, &mut reply);
            if reply.status == filesystem_protocol::status::OK {
                unlink_filesystem_node(files, &request, &mut reply);
            }
        }
        _ => {
            reject_unexpected_capability(capability, &mut reply);
            reply.status = filesystem_protocol::status::NOT_SUPPORTED;
        }
    }
    send_value(reply_endpoint, &reply);
}

fn connect_filesystem_session(
    generation: u64,
    sessions: &mut SessionTable,
    request: &filesystem_protocol::Request,
    capability: Option<ReceivedCapability>,
) {
    let Some(capability) = capability else {
        return;
    };
    if capability.rights != Rights::SEND
        || !ipc::info(capability.handle).is_ok_and(|info| info.kind == ObjectKind::Endpoint)
    {
        let _ = ipc::close(capability.handle);
        return;
    }
    let mut reply = filesystem_reply(request);
    if request.session_id != filesystem_protocol::INVALID_ID
        || request.generation != 0
        || !canonical_empty_request_fields(request)
    {
        reply.status = filesystem_protocol::status::INVALID;
        send_value(capability.handle, &reply);
        let _ = ipc::close(capability.handle);
        return;
    }
    match sessions.connect(generation, capability.handle) {
        Ok(session_id) => {
            reply.session_id = session_id;
            reply.generation = generation;
            reply.node_id = filesystem_protocol::ROOT_NODE_ID;
            reply.node_kind = filesystem_protocol::node_kind::DIRECTORY;
            send_value(capability.handle, &reply);
        }
        Err(error) => {
            reply.status = session_status(error);
            send_value(capability.handle, &reply);
            let _ = ipc::close(capability.handle);
        }
    }
}

fn attach_filesystem_buffer(
    sessions: &mut SessionTable,
    request: &filesystem_protocol::Request,
    capability: Option<ReceivedCapability>,
    reply: &mut filesystem_protocol::Reply,
) {
    let Some(capability) = capability else {
        reply.status = filesystem_protocol::status::INVALID;
        return;
    };
    let required_rights = Rights::READ | Rights::WRITE;
    let Ok(info) = ipc::info(capability.handle) else {
        let _ = ipc::close(capability.handle);
        reply.status = filesystem_protocol::status::INVALID;
        return;
    };
    if info.kind != ObjectKind::SharedMemory
        || capability.rights != required_rights
        || request.bulk.buffer_id == filesystem_protocol::INVALID_ID
        || request.bulk.offset != 0
        || request.bulk.length == 0
        || request.bulk.length > info.size
    {
        let _ = ipc::close(capability.handle);
        reply.status = filesystem_protocol::status::INVALID;
        return;
    }
    match sessions.attach_buffer(
        request.session_id,
        request.generation,
        request.bulk.buffer_id,
        capability.handle,
        request.bulk.length,
    ) {
        Ok(replaced) => {
            if let Some(replaced) = replaced {
                let _ = ipc::close(replaced);
            }
        }
        Err(error) => {
            let _ = ipc::close(capability.handle);
            reply.status = session_status(error);
        }
    }
}

fn lookup_node(
    files: &[File; protocol::MAX_FILES],
    request: &filesystem_protocol::Request,
    reply: &mut filesystem_protocol::Reply,
) {
    if request.node_id != filesystem_protocol::ROOT_NODE_ID {
        reply.status = filesystem_protocol::status::NOT_DIRECTORY;
        return;
    }
    let Some(name) = request_name(request) else {
        reply.status = filesystem_protocol::status::INVALID;
        return;
    };
    let Some(file) = files.iter().find(|file| file.named(name)) else {
        reply.status = filesystem_protocol::status::NOT_FOUND;
        return;
    };
    reply.node_id = file.node_id;
    reply.node_kind = filesystem_protocol::node_kind::FILE;
    reply.value = file.length as u64;
}

fn create_filesystem_file(
    files: &mut [File; protocol::MAX_FILES],
    next_node_id: &mut u64,
    request: &filesystem_protocol::Request,
    reply: &mut filesystem_protocol::Reply,
) {
    let allowed = filesystem_protocol::request_flags::EXCLUSIVE
        | filesystem_protocol::request_flags::TRUNCATE;
    if request.node_id != filesystem_protocol::ROOT_NODE_ID || request.flags & !allowed != 0 {
        reply.status = filesystem_protocol::status::INVALID;
        return;
    }
    let Some(name) = request_name(request) else {
        reply.status = filesystem_protocol::status::INVALID;
        return;
    };
    if let Some(index) = find_file(files, name) {
        if request.flags & filesystem_protocol::request_flags::EXCLUSIVE != 0 {
            reply.status = filesystem_protocol::status::EXISTS;
            return;
        }
        if request.flags & filesystem_protocol::request_flags::TRUNCATE != 0 {
            files[index].length = 0;
        }
        reply.node_id = files[index].node_id;
        reply.node_kind = filesystem_protocol::node_kind::FILE;
        reply.value = files[index].length as u64;
        return;
    }
    let Some(index) = files.iter().position(|file| !file.used) else {
        reply.status = filesystem_protocol::status::NO_SPACE;
        return;
    };
    initialize_file(&mut files[index], next_node_id, name);
    reply.node_id = files[index].node_id;
    reply.node_kind = filesystem_protocol::node_kind::FILE;
}

fn open_filesystem_node(
    files: &mut [File; protocol::MAX_FILES],
    next_node_id: &mut u64,
    sessions: &mut SessionTable,
    request: &filesystem_protocol::Request,
    reply: &mut filesystem_protocol::Reply,
) {
    let allowed = filesystem_protocol::request_flags::READ
        | filesystem_protocol::request_flags::WRITE
        | filesystem_protocol::request_flags::APPEND
        | filesystem_protocol::request_flags::TRUNCATE
        | filesystem_protocol::request_flags::CREATE
        | filesystem_protocol::request_flags::EXCLUSIVE;
    let name_length = usize::from(request.name_length);
    if request.secondary_node_id != filesystem_protocol::INVALID_ID
        || request.file_offset != 0
        || request.bulk != filesystem_protocol::BulkBuffer::NONE
        || name_length > filesystem_protocol::MAX_NAME_BYTES
        || request.name[name_length..].iter().any(|byte| *byte != 0)
        || request.flags & !allowed != 0
        || request.flags & filesystem_protocol::request_flags::EXCLUSIVE != 0
            && request.flags & filesystem_protocol::request_flags::CREATE == 0
        || request.flags & filesystem_protocol::request_flags::APPEND != 0
            && request.flags & filesystem_protocol::request_flags::WRITE == 0
        || request.flags & filesystem_protocol::request_flags::TRUNCATE != 0
            && request.flags & filesystem_protocol::request_flags::WRITE == 0
    {
        reply.status = filesystem_protocol::status::INVALID;
        return;
    }
    if request.name_length != 0 {
        if request.node_id != filesystem_protocol::ROOT_NODE_ID {
            reply.status = filesystem_protocol::status::NOT_DIRECTORY;
            return;
        }
        let Some(name) = request_name(request) else {
            reply.status = filesystem_protocol::status::INVALID;
            return;
        };
        let (index, node_id, create) = if let Some(index) = find_file(files, name) {
            if request.flags & filesystem_protocol::request_flags::EXCLUSIVE != 0 {
                reply.status = filesystem_protocol::status::EXISTS;
                return;
            }
            (index, files[index].node_id, false)
        } else if request.flags & filesystem_protocol::request_flags::CREATE != 0 {
            let Some(index) = files.iter().position(|file| !file.used) else {
                reply.status = filesystem_protocol::status::NO_SPACE;
                return;
            };
            (index, *next_node_id, true)
        } else {
            reply.status = filesystem_protocol::status::NOT_FOUND;
            return;
        };
        if files[index].open_count == u32::MAX {
            reply.status = filesystem_protocol::status::NO_SPACE;
            return;
        }
        if let Err(error) =
            sessions.record_open_node(request.session_id, request.generation, node_id)
        {
            reply.status = node_reference_status(error);
            return;
        }
        if create {
            initialize_file(&mut files[index], next_node_id, name);
        }
        files[index].open_count += 1;
        if request.flags & filesystem_protocol::request_flags::TRUNCATE != 0 {
            files[index].length = 0;
        }
        reply.node_id = files[index].node_id;
        reply.node_kind = filesystem_protocol::node_kind::FILE;
        reply.value = files[index].length as u64;
        return;
    }
    let Some(index) = files
        .iter()
        .position(|file| file.used && file.node_id == request.node_id)
    else {
        reply.status = if request.node_id == filesystem_protocol::ROOT_NODE_ID {
            filesystem_protocol::status::IS_DIRECTORY
        } else {
            filesystem_protocol::status::STALE_NODE
        };
        return;
    };
    if files[index].open_count == u32::MAX {
        reply.status = filesystem_protocol::status::NO_SPACE;
        return;
    }
    if let Err(error) =
        sessions.record_open_node(request.session_id, request.generation, files[index].node_id)
    {
        reply.status = node_reference_status(error);
        return;
    }
    files[index].open_count += 1;
    if request.flags & filesystem_protocol::request_flags::TRUNCATE != 0 {
        files[index].length = 0;
    }
    reply.node_id = files[index].node_id;
    reply.node_kind = filesystem_protocol::node_kind::FILE;
    reply.value = files[index].length as u64;
}

fn canonical_empty_request_fields(request: &filesystem_protocol::Request) -> bool {
    request.flags == 0
        && request.node_id == filesystem_protocol::INVALID_ID
        && request.secondary_node_id == filesystem_protocol::INVALID_ID
        && request.file_offset == 0
        && request.bulk == filesystem_protocol::BulkBuffer::NONE
        && request.name_length == 0
        && request.name == [0; filesystem_protocol::MAX_NAME_BYTES]
}

fn close_filesystem_node(
    files: &mut [File; protocol::MAX_FILES],
    sessions: &mut SessionTable,
    request: &filesystem_protocol::Request,
    reply: &mut filesystem_protocol::Reply,
) {
    if request.flags != 0
        || request.secondary_node_id != filesystem_protocol::INVALID_ID
        || request.file_offset != 0
        || request.bulk != filesystem_protocol::BulkBuffer::NONE
        || request.name_length != 0
        || request.name != [0; filesystem_protocol::MAX_NAME_BYTES]
    {
        reply.status = filesystem_protocol::status::INVALID;
        return;
    }
    let Some(index) = files
        .iter()
        .position(|file| file.used && file.node_id == request.node_id && file.open_count != 0)
    else {
        reply.status = filesystem_protocol::status::STALE_NODE;
        return;
    };
    if let Err(error) = sessions.close_node(request.session_id, request.generation, request.node_id)
    {
        reply.status = node_reference_status(error);
        return;
    }
    files[index].open_count -= 1;
    reclaim_if_unreferenced(&mut files[index]);
}

fn release_session_nodes(
    files: &mut [File; protocol::MAX_FILES],
    references: [NodeReference; userspace::filesystem_service::MAX_NODE_REFERENCES_PER_SESSION],
) {
    for reference in references
        .into_iter()
        .filter(|reference| reference.node_id != filesystem_protocol::INVALID_ID)
    {
        let Some(file) = files
            .iter_mut()
            .find(|file| file.used && file.node_id == reference.node_id)
        else {
            continue;
        };
        file.open_count = file.open_count.saturating_sub(reference.references);
        reclaim_if_unreferenced(file);
    }
}

fn reclaim_if_unreferenced(file: &mut File) {
    if file.used && !file.linked && file.open_count == 0 {
        *file = File::EMPTY;
    }
}

fn read_directory_to_buffer(
    files: &[File; protocol::MAX_FILES],
    sessions: &SessionTable,
    request: &filesystem_protocol::Request,
    reply: &mut filesystem_protocol::Reply,
) {
    if request.node_id != filesystem_protocol::ROOT_NODE_ID {
        reply.status = filesystem_protocol::status::NOT_DIRECTORY;
        return;
    }
    if request.flags != 0 {
        reply.status = filesystem_protocol::status::INVALID;
        return;
    }
    let Some((buffer, buffer_offset, requested)) = checked_bulk_range(sessions, request, reply)
    else {
        return;
    };
    let record_size = size_of::<filesystem_protocol::DirectoryEntry>();
    let capacity = requested / record_size;
    if capacity == 0 {
        reply.status = filesystem_protocol::status::RANGE;
        return;
    }
    let mut count = 0usize;
    let mut cookie = request.file_offset;
    while count < capacity {
        let Some(file) = files
            .iter()
            .filter(|file| file.used && file.linked && file.node_id > cookie)
            .min_by_key(|file| file.node_id)
        else {
            break;
        };
        let mut entry = filesystem_protocol::DirectoryEntry::EMPTY;
        entry.node_id = file.node_id;
        entry.next_cookie = file.node_id;
        entry.kind = filesystem_protocol::node_kind::FILE;
        entry.name_length = file.name_length as u16;
        entry.name[..file.name_length].copy_from_slice(&file.name[..file.name_length]);
        let offset = buffer_offset + count * record_size;
        match ipc::shared_memory_write(buffer.handle, offset, value_bytes(&entry)) {
            Ok(written) if written == record_size => count += 1,
            _ => {
                reply.status = filesystem_protocol::status::IO;
                return;
            }
        }
        cookie = file.node_id;
    }
    reply.value = count as u64;
    if !files
        .iter()
        .any(|file| file.used && file.linked && file.node_id > cookie)
    {
        reply.flags |= filesystem_protocol::reply_flags::END_OF_DIRECTORY;
    }
}

fn request_name(request: &filesystem_protocol::Request) -> Option<&[u8]> {
    let name_length = usize::from(request.name_length);
    (name_length != 0
        && name_length <= filesystem_protocol::MAX_NAME_BYTES
        && !request.name[..name_length].contains(&b'/')
        && !request.name[..name_length].contains(&0))
    .then_some(&request.name[..name_length])
}

fn unlink_filesystem_node(
    files: &mut [File; protocol::MAX_FILES],
    request: &filesystem_protocol::Request,
    reply: &mut filesystem_protocol::Reply,
) {
    if request.node_id != filesystem_protocol::ROOT_NODE_ID || request.flags != 0 {
        reply.status = filesystem_protocol::status::INVALID;
        return;
    }
    let Some(name) = request_name(request) else {
        reply.status = filesystem_protocol::status::INVALID;
        return;
    };
    let Some(index) = find_file(files, name) else {
        reply.status = filesystem_protocol::status::NOT_FOUND;
        return;
    };
    files[index].linked = false;
    reclaim_if_unreferenced(&mut files[index]);
}

fn get_node_attributes(
    files: &[File; protocol::MAX_FILES],
    request: &filesystem_protocol::Request,
    reply: &mut filesystem_protocol::Reply,
) {
    let attributes = if request.node_id == filesystem_protocol::ROOT_NODE_ID {
        let mut attributes = filesystem_protocol::NodeAttributes::EMPTY;
        attributes.node_id = filesystem_protocol::ROOT_NODE_ID;
        attributes.kind = filesystem_protocol::node_kind::DIRECTORY;
        attributes.link_count = 1;
        attributes
    } else {
        let Some(file) = files
            .iter()
            .find(|file| file.used && file.node_id == request.node_id)
        else {
            reply.status = filesystem_protocol::status::STALE_NODE;
            return;
        };
        let mut attributes = filesystem_protocol::NodeAttributes::EMPTY;
        attributes.node_id = file.node_id;
        attributes.size = file.length as u64;
        attributes.allocated_size = protocol::MAX_FILE_BYTES as u64;
        attributes.kind = filesystem_protocol::node_kind::FILE;
        attributes.link_count = u32::from(file.linked);
        attributes
    };
    let bytes = value_bytes(&attributes);
    reply.data[..bytes.len()].copy_from_slice(bytes);
    reply.data_length = bytes.len() as u16;
    reply.node_id = attributes.node_id;
    reply.node_kind = attributes.kind;
}

fn read_node_to_buffer(
    files: &[File; protocol::MAX_FILES],
    sessions: &SessionTable,
    request: &filesystem_protocol::Request,
    reply: &mut filesystem_protocol::Reply,
) {
    if request.flags != 0 {
        reply.status = filesystem_protocol::status::INVALID;
        return;
    }
    let Some(file) = files
        .iter()
        .find(|file| file.used && file.node_id == request.node_id)
    else {
        reply.status = filesystem_protocol::status::STALE_NODE;
        return;
    };
    let Some((buffer, buffer_offset, requested)) = checked_bulk_range(sessions, request, reply)
    else {
        return;
    };
    let Ok(file_offset) = usize::try_from(request.file_offset) else {
        reply.status = filesystem_protocol::status::RANGE;
        return;
    };
    if file_offset > file.length {
        reply.status = filesystem_protocol::status::RANGE;
        return;
    }
    let count = requested.min(file.length - file_offset);
    match ipc::shared_memory_write(
        buffer.handle,
        buffer_offset,
        &file.data[file_offset..file_offset + count],
    ) {
        Ok(written) if written == count => reply.value = written as u64,
        _ => reply.status = filesystem_protocol::status::IO,
    }
}

fn write_node_from_buffer(
    files: &mut [File; protocol::MAX_FILES],
    sessions: &SessionTable,
    request: &filesystem_protocol::Request,
    reply: &mut filesystem_protocol::Reply,
) {
    if request.flags & !filesystem_protocol::request_flags::APPEND != 0 {
        reply.status = filesystem_protocol::status::INVALID;
        return;
    }
    let Some(index) = files
        .iter()
        .position(|file| file.used && file.node_id == request.node_id)
    else {
        reply.status = filesystem_protocol::status::STALE_NODE;
        return;
    };
    let Some((buffer, buffer_offset, requested)) = checked_bulk_range(sessions, request, reply)
    else {
        return;
    };
    if requested > protocol::MAX_FILE_BYTES {
        reply.status = filesystem_protocol::status::RANGE;
        return;
    }
    let file_offset = if request.flags & filesystem_protocol::request_flags::APPEND != 0 {
        files[index].length
    } else {
        let Ok(offset) = usize::try_from(request.file_offset) else {
            reply.status = filesystem_protocol::status::RANGE;
            return;
        };
        offset
    };
    let Some(end) = file_offset.checked_add(requested) else {
        reply.status = filesystem_protocol::status::RANGE;
        return;
    };
    if end > protocol::MAX_FILE_BYTES {
        reply.status = filesystem_protocol::status::NO_SPACE;
        return;
    }
    let mut bytes = [0_u8; protocol::MAX_FILE_BYTES];
    match ipc::shared_memory_read(buffer.handle, buffer_offset, &mut bytes[..requested]) {
        Ok(read) if read == requested => {
            let file = &mut files[index];
            file.data[file_offset..end].copy_from_slice(&bytes[..requested]);
            file.length = file.length.max(end);
            reply.value = requested as u64;
            filesystem_protocol::encode_write_reply_offset(reply, end as u64);
        }
        _ => reply.status = filesystem_protocol::status::IO,
    }
}

fn checked_bulk_range(
    sessions: &SessionTable,
    request: &filesystem_protocol::Request,
    reply: &mut filesystem_protocol::Reply,
) -> Option<(userspace::filesystem_service::BufferSlot, usize, usize)> {
    let buffer = match sessions.buffer(
        request.session_id,
        request.generation,
        request.bulk.buffer_id,
    ) {
        Ok(buffer) => buffer,
        Err(error) => {
            reply.status = session_status(error);
            return None;
        }
    };
    let Some(end) = request.bulk.end() else {
        reply.status = filesystem_protocol::status::RANGE;
        return None;
    };
    if request.bulk.length == 0 || end > buffer.length {
        reply.status = filesystem_protocol::status::RANGE;
        return None;
    }
    let Ok(offset) = usize::try_from(request.bulk.offset) else {
        reply.status = filesystem_protocol::status::RANGE;
        return None;
    };
    let Ok(length) = usize::try_from(request.bulk.length) else {
        reply.status = filesystem_protocol::status::RANGE;
        return None;
    };
    Some((buffer, offset, length))
}

fn reject_unexpected_capability(
    capability: Option<ReceivedCapability>,
    reply: &mut filesystem_protocol::Reply,
) {
    if let Some(capability) = capability {
        let _ = ipc::close(capability.handle);
        reply.status = filesystem_protocol::status::INVALID;
    }
}

fn filesystem_reply(request: &filesystem_protocol::Request) -> filesystem_protocol::Reply {
    let mut reply = filesystem_protocol::Reply::EMPTY;
    reply.operation = request.operation;
    reply.request_id = request.request_id;
    reply.session_id = request.session_id;
    reply.generation = request.generation;
    reply
}

fn session_status(error: SessionError) -> i32 {
    match error {
        SessionError::NoSpace => filesystem_protocol::status::NO_SPACE,
        SessionError::StaleSession => filesystem_protocol::status::STALE_SESSION,
        SessionError::InvalidBuffer => filesystem_protocol::status::STALE_BUFFER,
    }
}

fn node_reference_status(error: NodeReferenceError) -> i32 {
    match error {
        NodeReferenceError::NoSpace => filesystem_protocol::status::NO_SPACE,
        NodeReferenceError::StaleSession => filesystem_protocol::status::STALE_SESSION,
        NodeReferenceError::UnknownNode => filesystem_protocol::status::STALE_NODE,
    }
}

fn send_value<T>(endpoint: u64, value: &T) {
    let _ = ipc::send(endpoint, value_bytes(value), None);
}

fn value_bytes<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

fn dispatch(
    files: &mut [File; protocol::MAX_FILES],
    next_node_id: &mut u64,
    generation: u32,
    request: &protocol::Request,
) -> protocol::Reply {
    let mut reply = protocol::Reply::EMPTY;
    reply.operation = request.operation;
    reply.generation = generation;
    if request.version != protocol::VERSION {
        reply.status = protocol::status::INVALID;
        return reply;
    }
    if request.operation == protocol::operation::MOUNT {
        return reply;
    }
    if request.generation != generation {
        reply.status = protocol::status::STALE_MOUNT;
        return reply;
    }
    let name_length = request.name_length as usize;
    let data_length = request.data_length as usize;
    if name_length > protocol::MAX_NAME_BYTES || data_length > protocol::MAX_DATA_BYTES {
        reply.status = protocol::status::INVALID;
        return reply;
    }
    let name = &request.name[..name_length];
    if request.operation != protocol::operation::LIST && (name.is_empty() || name.contains(&b'/')) {
        reply.status = protocol::status::INVALID;
        return reply;
    }

    match request.operation {
        protocol::operation::WRITE => write_file(files, next_node_id, name, request, &mut reply),
        protocol::operation::READ => read_file(files, name, request, &mut reply),
        protocol::operation::STAT => stat_file(files, name, &mut reply),
        protocol::operation::REMOVE => remove_file(files, name, &mut reply),
        protocol::operation::LIST => list_files(files, data_length, &mut reply),
        protocol::operation::OPEN => open_file(files, next_node_id, name, request, &mut reply),
        _ => reply.status = protocol::status::INVALID,
    }
    reply
}

fn find_file(files: &[File; protocol::MAX_FILES], name: &[u8]) -> Option<usize> {
    files.iter().position(|file| file.named(name))
}

fn open_file(
    files: &mut [File; protocol::MAX_FILES],
    next_node_id: &mut u64,
    name: &[u8],
    request: &protocol::Request,
    reply: &mut protocol::Reply,
) {
    let flags = request.offset;
    if flags & !(protocol::open_flags::CREATE | protocol::open_flags::TRUNCATE) != 0 {
        reply.status = protocol::status::INVALID;
        return;
    }
    let index = find_file(files, name).or_else(|| {
        (flags & protocol::open_flags::CREATE != 0)
            .then(|| files.iter().position(|file| !file.used))
            .flatten()
    });
    let Some(index) = index else {
        reply.status = if flags & protocol::open_flags::CREATE != 0 {
            protocol::status::NO_SPACE
        } else {
            protocol::status::NOT_FOUND
        };
        return;
    };
    let file = &mut files[index];
    if !file.used {
        initialize_file(file, next_node_id, name);
    }
    if flags & protocol::open_flags::TRUNCATE != 0 {
        file.length = 0;
    }
    reply.value = file.length as u32;
}

fn write_file(
    files: &mut [File; protocol::MAX_FILES],
    next_node_id: &mut u64,
    name: &[u8],
    request: &protocol::Request,
    reply: &mut protocol::Reply,
) {
    let offset = request.offset as usize;
    let count = request.data_length as usize;
    let Some(end) = offset.checked_add(count) else {
        reply.status = protocol::status::RANGE;
        return;
    };
    if end > protocol::MAX_FILE_BYTES {
        reply.status = protocol::status::RANGE;
        return;
    }
    let index = find_file(files, name).or_else(|| files.iter().position(|file| !file.used));
    let Some(index) = index else {
        reply.status = protocol::status::NO_SPACE;
        return;
    };
    let file = &mut files[index];
    if !file.used {
        initialize_file(file, next_node_id, name);
    }
    file.data[offset..end].copy_from_slice(&request.data[..count]);
    file.length = file.length.max(end);
    reply.value = count as u32;
}

fn initialize_file(file: &mut File, next_node_id: &mut u64, name: &[u8]) {
    file.used = true;
    file.linked = true;
    file.node_id = *next_node_id;
    *next_node_id = next_node_id.saturating_add(1);
    file.name_length = name.len();
    file.name[..name.len()].copy_from_slice(name);
}

fn read_file(
    files: &[File; protocol::MAX_FILES],
    name: &[u8],
    request: &protocol::Request,
    reply: &mut protocol::Reply,
) {
    let Some(index) = find_file(files, name) else {
        reply.status = protocol::status::NOT_FOUND;
        return;
    };
    let file = &files[index];
    let offset = request.offset as usize;
    if offset > file.length {
        reply.status = protocol::status::RANGE;
        return;
    }
    let count = (request.data_length as usize).min(file.length - offset);
    reply.data[..count].copy_from_slice(&file.data[offset..offset + count]);
    reply.data_length = count as u16;
    reply.value = file.length as u32;
}

fn stat_file(files: &[File; protocol::MAX_FILES], name: &[u8], reply: &mut protocol::Reply) {
    let Some(index) = find_file(files, name) else {
        reply.status = protocol::status::NOT_FOUND;
        return;
    };
    reply.value = files[index].length as u32;
}

fn remove_file(files: &mut [File; protocol::MAX_FILES], name: &[u8], reply: &mut protocol::Reply) {
    let Some(index) = find_file(files, name) else {
        reply.status = protocol::status::NOT_FOUND;
        return;
    };
    files[index].linked = false;
    reclaim_if_unreferenced(&mut files[index]);
}

fn list_files(files: &[File; protocol::MAX_FILES], capacity: usize, reply: &mut protocol::Reply) {
    let capacity = capacity.min(reply.data.len());
    let mut cursor = 0usize;
    for file in files.iter().filter(|file| file.used && file.linked) {
        let needed = file.name_length + if cursor != 0 { 1 } else { 0 };
        if cursor + needed > capacity {
            break;
        }
        if cursor != 0 {
            reply.data[cursor] = b'\n';
            cursor += 1;
        }
        reply.data[cursor..cursor + file.name_length]
            .copy_from_slice(&file.name[..file.name_length]);
        cursor += file.name_length;
    }
    reply.data_length = cursor as u16;
    reply.value = files.iter().filter(|file| file.used && file.linked).count() as u32;
}
