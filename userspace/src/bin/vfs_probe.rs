#![no_std]
#![no_main]

use core::{mem::size_of, slice};

use userspace::{
    abi::file,
    ipc::{self, ObjectKind, Rights, Transfer},
    platform, syscall,
    vfs::protocol,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SERVICE_HANDLE: u64 = 1;

const CASES: &[(&[u8], u32, u16, u16)] = &[
    (
        b"/",
        protocol::route::ROOT,
        protocol::backend::BOOT_FILESYSTEM,
        1,
    ),
    (
        b"/dev",
        protocol::route::DEV,
        protocol::backend::NAMESPACE,
        4,
    ),
    (
        b"/tmp/cache",
        protocol::route::TMP,
        protocol::backend::TMPFS,
        4,
    ),
    (
        b"/System/config/boot",
        protocol::route::SYSTEM_CONFIG,
        protocol::backend::NAMESPACE,
        14,
    ),
    (
        b"/System/var/log/kernel",
        protocol::route::SYSTEM_VAR_LOG,
        protocol::backend::NAMESPACE,
        15,
    ),
    (
        b"/System/var/cache",
        protocol::route::SYSTEM_VAR,
        protocol::backend::NAMESPACE,
        11,
    ),
    (
        b"/System/bin",
        protocol::route::SYSTEM_BIN,
        protocol::backend::NAMESPACE,
        11,
    ),
    (
        b"/System/services",
        protocol::route::SYSTEM_SERVICES,
        protocol::backend::NAMESPACE,
        16,
    ),
    (
        b"/System/drivers",
        protocol::route::SYSTEM_DRIVERS,
        protocol::backend::NAMESPACE,
        15,
    ),
    (
        b"/System/lib",
        protocol::route::SYSTEM_LIB,
        protocol::backend::NAMESPACE,
        11,
    ),
    (
        b"/System/Applications/Finder",
        protocol::route::SYSTEM_APPLICATIONS,
        protocol::backend::NAMESPACE,
        20,
    ),
    (
        b"/Users/natalie",
        protocol::route::USERS,
        protocol::backend::NAMESPACE,
        6,
    ),
    (
        b"/Applications/App",
        protocol::route::APPLICATIONS,
        protocol::backend::NAMESPACE,
        13,
    ),
    (
        b"/Volumes/Disk",
        protocol::route::VOLUMES,
        protocol::backend::NAMESPACE,
        8,
    ),
];

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    if !matches!(
        ipc::wait_for_handle(SERVICE_HANDLE),
        Ok(info) if info.kind == ObjectKind::Endpoint && info.rights == Rights::SEND
    ) {
        syscall::exit(1);
    }
    for (index, &(path, route_id, backend, prefix_length)) in CASES.iter().enumerate() {
        let reply = match query(path, index as u32 + 1) {
            Some(reply) => reply,
            None => syscall::exit(2),
        };
        if reply.status != protocol::status::OK
            || reply.route_id != route_id
            || reply.backend != backend
            || reply.prefix_length != prefix_length
        {
            syscall::exit(3);
        }
    }
    if platform::stat(b"/hello.txt").ok().map(|stat| stat.kind) != Some(file::KIND_FILE) {
        syscall::exit(4);
    }
    const NAMESPACE_DIRECTORIES: &[&[u8]] = &[
        b"/dev",
        b"/tmp",
        b"/System",
        b"/System/config",
        b"/System/var",
        b"/System/var/log",
        b"/System/bin",
        b"/System/services",
        b"/System/drivers",
        b"/System/lib",
        b"/System/Applications",
        b"/Users",
        b"/Applications",
        b"/Volumes",
    ];
    for path in NAMESPACE_DIRECTORIES {
        if platform::stat(path).ok().map(|stat| stat.kind) != Some(file::KIND_DIRECTORY) {
            syscall::exit(5);
        }
    }
    if !directory_contains(
        b"/",
        &[
            b"dev",
            b"tmp",
            b"System",
            b"Users",
            b"Applications",
            b"Volumes",
        ],
    ) || !directory_contains(
        b"/System",
        &[
            b"config",
            b"var",
            b"bin",
            b"services",
            b"drivers",
            b"lib",
            b"Applications",
        ],
    ) || !directory_contains(b"/System/var", &[b"log"])
    {
        syscall::exit(6);
    }
    let mut cwd = [0_u8; 64];
    if platform::chdir(b"/System/var").is_err() {
        syscall::exit(7);
    }
    if platform::getcwd(&mut cwd).ok() != Some(b"/System/var".as_slice()) {
        syscall::exit(8);
    }
    if platform::chdir(b"/").is_err() {
        syscall::exit(9);
    }
    if platform::chdir(b"/Volumes").is_err() {
        syscall::exit(10);
    }
    if platform::getcwd(&mut cwd).ok() != Some(b"/Volumes".as_slice()) {
        syscall::exit(11);
    }
    if platform::chdir(b"/").is_err() {
        syscall::exit(12);
    }
    syscall::exit(0)
}

fn directory_contains(path: &[u8], names: &[&[u8]]) -> bool {
    let mut found = 0_u64;
    let mut offset = 0usize;
    loop {
        let mut entries = [platform::DirectoryEntry::EMPTY; 8];
        let Ok(count) = platform::read_directory(path, offset, &mut entries) else {
            return false;
        };
        for entry in &entries[..count] {
            for (index, name) in names.iter().enumerate() {
                if entry.kind == file::KIND_DIRECTORY && entry.name() == *name {
                    found |= 1_u64 << index;
                }
            }
        }
        offset = offset.saturating_add(count);
        if count < entries.len() {
            break;
        }
        if offset > 128 {
            return false;
        }
    }
    found == (1_u64 << names.len()) - 1
}

fn query(path: &[u8], request_id: u32) -> Option<protocol::Reply> {
    let reply_endpoint = ipc::endpoint_create().ok()?;
    let mut request = protocol::Request {
        operation: protocol::operation::RESOLVE,
        request_id,
        path_length: path.len() as u16,
        ..protocol::Request::EMPTY
    };
    request.path[..path.len()].copy_from_slice(path);
    let request_bytes = unsafe {
        slice::from_raw_parts(
            (&request as *const protocol::Request).cast::<u8>(),
            size_of::<protocol::Request>(),
        )
    };
    if ipc::send(
        SERVICE_HANDLE,
        request_bytes,
        Some(Transfer {
            handle: reply_endpoint,
            rights: Rights::SEND,
        }),
    )
    .is_err()
    {
        let _ = ipc::close(reply_endpoint);
        return None;
    }
    let mut reply_bytes = [0_u8; size_of::<protocol::Reply>()];
    let message = ipc::receive(reply_endpoint, &mut reply_bytes).ok()?;
    let _ = ipc::close(reply_endpoint);
    if message.bytes != reply_bytes.len() || message.capability.is_some() {
        return None;
    }
    let reply =
        unsafe { core::ptr::read_unaligned(reply_bytes.as_ptr() as *const protocol::Reply) };
    (reply.version == protocol::VERSION
        && reply.operation == protocol::operation::RESOLVE
        && reply.request_id == request_id
        && reply.reserved == [0; 8])
        .then_some(reply)
}
