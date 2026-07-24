#![no_std]
#![no_main]

use core::{mem::size_of, slice};

use userspace::{
    ipc::{self, ObjectKind, Rights},
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
    name_length: usize,
    name: [u8; protocol::MAX_NAME_BYTES],
    length: usize,
    data: [u8; protocol::MAX_FILE_BYTES],
}

impl File {
    const EMPTY: Self = Self {
        used: false,
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
    if !valid_bootstrap(READY_HANDLE, ObjectKind::Endpoint, Rights::SEND)
        || !valid_bootstrap(REQUEST_HANDLE, ObjectKind::Endpoint, Rights::RECEIVE)
        || ipc::send(READY_HANDLE, READY_MESSAGE, None).is_err()
    {
        syscall::exit(2);
    }

    let mut files = [File::EMPTY; protocol::MAX_FILES];
    let mut request_bytes = [0_u8; size_of::<protocol::Request>()];
    loop {
        let message = match ipc::receive(REQUEST_HANDLE, &mut request_bytes) {
            Ok(message) => message,
            Err(_) => syscall::exit(3),
        };
        let Some(reply_capability) = message.capability else {
            continue;
        };
        if message.bytes != request_bytes.len() || reply_capability.rights != Rights::SEND {
            let _ = ipc::close(reply_capability.handle);
            continue;
        }

        let request = unsafe {
            core::ptr::read_unaligned(request_bytes.as_ptr() as *const protocol::Request)
        };
        let reply = dispatch(&mut files, &request);
        let reply_bytes = unsafe {
            slice::from_raw_parts(
                &reply as *const protocol::Reply as *const u8,
                size_of::<protocol::Reply>(),
            )
        };
        let _ = ipc::send(reply_capability.handle, reply_bytes, None);
        let _ = ipc::close(reply_capability.handle);
    }
}

fn valid_bootstrap(handle: u64, kind: ObjectKind, rights: Rights) -> bool {
    ipc::wait_for_handle(handle)
        .is_ok_and(|info| info.kind == kind && info.rights == rights)
}

fn dispatch(files: &mut [File; protocol::MAX_FILES], request: &protocol::Request) -> protocol::Reply {
    let mut reply = protocol::Reply::EMPTY;
    reply.operation = request.operation;
    if request.version != protocol::VERSION {
        reply.status = protocol::status::INVALID;
        return reply;
    }
    let name_length = request.name_length as usize;
    let data_length = request.data_length as usize;
    if name_length > protocol::MAX_NAME_BYTES || data_length > protocol::MAX_DATA_BYTES {
        reply.status = protocol::status::INVALID;
        return reply;
    }
    let name = &request.name[..name_length];
    if request.operation != protocol::operation::LIST
        && (name.is_empty() || name.iter().any(|byte| *byte == b'/'))
    {
        reply.status = protocol::status::INVALID;
        return reply;
    }

    match request.operation {
        protocol::operation::WRITE => write_file(files, name, request, &mut reply),
        protocol::operation::READ => read_file(files, name, request, &mut reply),
        protocol::operation::STAT => stat_file(files, name, &mut reply),
        protocol::operation::REMOVE => remove_file(files, name, &mut reply),
        protocol::operation::LIST => list_files(files, data_length, &mut reply),
        _ => reply.status = protocol::status::INVALID,
    }
    reply
}

fn find_file(files: &[File; protocol::MAX_FILES], name: &[u8]) -> Option<usize> {
    files.iter().position(|file| file.named(name))
}

fn write_file(
    files: &mut [File; protocol::MAX_FILES],
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
        file.used = true;
        file.name_length = name.len();
        file.name[..name.len()].copy_from_slice(name);
    }
    file.data[offset..end].copy_from_slice(&request.data[..count]);
    file.length = file.length.max(end);
    reply.value = count as u32;
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

fn list_files(
    files: &[File; protocol::MAX_FILES],
    capacity: usize,
    reply: &mut protocol::Reply,
) {
    let capacity = capacity.min(reply.data.len());
    let mut cursor = 0usize;
    for file in files.iter().filter(|file| file.used) {
        let needed = file.name_length + usize::from(cursor != 0);
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
