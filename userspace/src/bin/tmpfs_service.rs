#![no_std]
#![no_main]

use core::{mem::size_of, slice};

use userspace::{
    filesystem::protocol as filesystem_protocol,
    filesystem_service::{Error as SessionError, SessionTable},
    ipc::{self, ObjectKind, ReceivedCapability, Rights},
    syscall,
    tmpfs::protocol,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const READY_HANDLE: u64 = 1;
const REQUEST_HANDLE: u64 = 2;
const READY_MESSAGE: &[u8] = b"service-ready: tmpfs";

#[derive(Clone, Copy)]
struct File {
    used: bool,
    node_id: u64,
    name_length: usize,
    name: [u8; protocol::MAX_NAME_BYTES],
    length: usize,
    data: [u8; protocol::MAX_FILE_BYTES],
}

impl File {
    const EMPTY: Self = Self {
        used: false,
        node_id: filesystem_protocol::INVALID_ID,
        name_length: 0,
        name: [0; protocol::MAX_NAME_BYTES],
        length: 0,
        data: [0; protocol::MAX_FILE_BYTES],
    };

    fn named(&self, name: &[u8]) -> bool {
        self.used && self.name_length == name.len() && &self.name[..self.name_length] == name
    }
}

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let _ = syscall::write_all(syscall::STDERR, b"tmpfs: validating readiness handle\n");
    if !valid_bootstrap(READY_HANDLE, ObjectKind::Endpoint, Rights::SEND) {
        let _ = syscall::write_all(syscall::STDERR, b"tmpfs: invalid readiness handle\n");
        syscall::exit(2);
    }

    let _ = syscall::write_all(syscall::STDERR, b"tmpfs: validating request handle\n");
    if !valid_bootstrap(REQUEST_HANDLE, ObjectKind::Endpoint, Rights::RECEIVE) {
        let _ = syscall::write_all(syscall::STDERR, b"tmpfs: invalid request handle\n");
        syscall::exit(3);
    }

    let _ = syscall::write_all(syscall::STDERR, b"tmpfs: sending readiness message\n");
    if ipc::send(READY_HANDLE, READY_MESSAGE, None).is_err() {
        let _ = syscall::write_all(syscall::STDERR, b"tmpfs: readiness send failed\n");
        syscall::exit(4);
    }
    let _ = syscall::write_all(syscall::STDERR, b"tmpfs: ready\n");

    let generation = syscall::getpid().unwrap_or(1);
    let mut files = [File::EMPTY; protocol::MAX_FILES];
    let mut next_node_id = filesystem_protocol::ROOT_NODE_ID + 1;
    let mut sessions = SessionTable::new();
    let mut request_bytes = [0_u8; userspace::abi::limits::MAX_IPC_MESSAGE_BYTES];
    loop {
        let message = match ipc::receive(REQUEST_HANDLE, &mut request_bytes) {
            Ok(message) => message,
            Err(_) => syscall::exit(5),
        };
        if message.bytes == size_of::<protocol::Request>() {
            dispatch_legacy_message(
                &mut files,
                &mut next_node_id,
                generation as u32,
                &request_bytes,
                message.capability,
            );
        } else if message.bytes == size_of::<filesystem_protocol::Request>() {
            dispatch_filesystem_message(
                &files,
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
    files: &[File; protocol::MAX_FILES],
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
            if reply.status != filesystem_protocol::status::OK {
                send_value(reply_endpoint, &reply);
                return;
            }
            match sessions.disconnect(request.session_id, request.generation) {
                Ok(released) => {
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
    let name_length = usize::from(request.name_length);
    if name_length == 0
        || name_length > filesystem_protocol::MAX_NAME_BYTES
        || request.name[..name_length].contains(&b'/')
        || request.name[..name_length].contains(&0)
    {
        reply.status = filesystem_protocol::status::INVALID;
        return;
    }
    let name = &request.name[..name_length];
    let Some(file) = files.iter().find(|file| file.named(name)) else {
        reply.status = filesystem_protocol::status::NOT_FOUND;
        return;
    };
    reply.node_id = file.node_id;
    reply.node_kind = filesystem_protocol::node_kind::FILE;
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
        attributes.link_count = 1;
        attributes
    };
    let bytes = value_bytes(&attributes);
    reply.data[..bytes.len()].copy_from_slice(bytes);
    reply.data_length = bytes.len() as u16;
    reply.node_id = attributes.node_id;
    reply.node_kind = attributes.kind;
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

fn send_value<T>(endpoint: u64, value: &T) {
    let _ = ipc::send(endpoint, value_bytes(value), None);
}

fn value_bytes<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

fn valid_bootstrap(handle: u64, kind: ObjectKind, rights: Rights) -> bool {
    ipc::wait_for_handle(handle).is_ok_and(|info| info.kind == kind && info.rights == rights)
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
    files[index] = File::EMPTY;
}

fn list_files(files: &[File; protocol::MAX_FILES], capacity: usize, reply: &mut protocol::Reply) {
    let capacity = capacity.min(reply.data.len());
    let mut cursor = 0usize;
    for file in files.iter().filter(|file| file.used) {
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
    reply.value = files.iter().filter(|file| file.used).count() as u32;
}
