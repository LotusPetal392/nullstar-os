#![no_std]
#![no_main]

use core::{mem::size_of, slice};

use userspace::{
    abi::INIT_PROCESS_ID,
    args::Args,
    ipc::{self, ObjectKind, Rights},
    nullfs_primary_volume, platform,
    service_route::receive_service_generation,
    syscall,
    vfs::protocol,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const READY_HANDLE: u64 = 1;
const REQUEST_HANDLE: u64 = 2;
const GENERATION_HANDOFF_HANDLE: u64 = 5;
const READY_MESSAGE: &[u8] = b"service-ready: vfs";
const CONTAINMENT_TEST_ARGUMENT: &[u8] = b"--containment-test";
const CONTAINMENT_DESCENDANT_MARKER: &[u8] =
    b"vfs-service: containment descendant escaped process group\n";

struct Route {
    path: &'static [u8],
    id: u32,
    backend: u16,
}

const _: () =
    assert!(size_of::<protocol::Request>() <= userspace::abi::limits::MAX_IPC_MESSAGE_BYTES);
const _: () =
    assert!(size_of::<protocol::Reply>() <= userspace::abi::limits::MAX_IPC_MESSAGE_BYTES);

const ROUTES: &[Route] = &[
    Route {
        path: b"/System/Applications",
        id: protocol::route::SYSTEM_APPLICATIONS,
        backend: protocol::backend::NULLFS,
    },
    Route {
        path: b"/System/services",
        id: protocol::route::SYSTEM_SERVICES,
        backend: protocol::backend::NULLFS,
    },
    Route {
        path: b"/System/drivers",
        id: protocol::route::SYSTEM_DRIVERS,
        backend: protocol::backend::NULLFS,
    },
    Route {
        path: b"/System/var/log",
        id: protocol::route::SYSTEM_VAR_LOG,
        backend: protocol::backend::NULLFS,
    },
    Route {
        path: b"/System/var",
        id: protocol::route::SYSTEM_VAR,
        backend: protocol::backend::NULLFS,
    },
    Route {
        path: b"/System/config",
        id: protocol::route::SYSTEM_CONFIG,
        backend: protocol::backend::NULLFS,
    },
    Route {
        path: b"/System/bin",
        id: protocol::route::SYSTEM_BIN,
        backend: protocol::backend::NULLFS,
    },
    Route {
        path: b"/System/lib",
        id: protocol::route::SYSTEM_LIB,
        backend: protocol::backend::NULLFS,
    },
    Route {
        path: b"/Applications",
        id: protocol::route::APPLICATIONS,
        backend: protocol::backend::NULLFS,
    },
    Route {
        path: nullfs_primary_volume::MOUNT_PATH.as_bytes(),
        id: protocol::route::NULLSTAR_VOLUME,
        backend: protocol::backend::NULLFS,
    },
    Route {
        path: b"/Volumes",
        id: protocol::route::VOLUMES,
        backend: protocol::backend::NAMESPACE,
    },
    Route {
        path: b"/System",
        id: protocol::route::SYSTEM,
        backend: protocol::backend::NULLFS,
    },
    Route {
        path: b"/Users",
        id: protocol::route::USERS,
        backend: protocol::backend::NULLFS,
    },
    Route {
        path: b"/tmp",
        id: protocol::route::TMP,
        backend: protocol::backend::TMPFS,
    },
    Route {
        path: b"/dev",
        id: protocol::route::DEV,
        backend: protocol::backend::NAMESPACE,
    },
    Route {
        path: b"/",
        id: protocol::route::ROOT,
        backend: protocol::backend::BOOT_FILESYSTEM,
    },
];

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

    if !valid_bootstrap(READY_HANDLE, ObjectKind::Endpoint, Rights::SEND)
        || !valid_bootstrap(REQUEST_HANDLE, ObjectKind::Endpoint, Rights::RECEIVE)
    {
        syscall::exit(2);
    }
    let generation = match receive_service_generation(GENERATION_HANDOFF_HANDLE, INIT_PROCESS_ID) {
        Ok(generation) => generation.get(),
        Err(_) => syscall::exit(3),
    };
    if containment_test && generation == 1 {
        spawn_containment_descendant();
    }
    if ipc::send(READY_HANDLE, READY_MESSAGE, None).is_err() {
        syscall::exit(4);
    }

    let mut bytes = [0_u8; userspace::abi::limits::MAX_IPC_MESSAGE_BYTES];
    loop {
        let message = match ipc::receive(REQUEST_HANDLE, &mut bytes) {
            Ok(message) => message,
            Err(_) => syscall::exit(5),
        };
        let Some(reply_capability) = message.capability else {
            continue;
        };
        if reply_capability.rights != Rights::SEND
            || message.bytes != size_of::<protocol::Request>()
        {
            let _ = ipc::close(reply_capability.handle);
            continue;
        }
        let request =
            unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const protocol::Request) };
        let reply = resolve(&request);
        let reply_bytes = unsafe {
            slice::from_raw_parts(
                (&reply as *const protocol::Reply).cast::<u8>(),
                size_of::<protocol::Reply>(),
            )
        };
        let _ = ipc::send(reply_capability.handle, reply_bytes, None);
        let _ = ipc::close(reply_capability.handle);
    }
}

fn resolve(request: &protocol::Request) -> protocol::Reply {
    let mut reply = protocol::Reply {
        operation: request.operation,
        request_id: request.request_id,
        ..protocol::Reply::EMPTY
    };
    let path_length = usize::from(request.path_length);
    if request.version != protocol::VERSION
        || request.operation != protocol::operation::RESOLVE
        || request.request_id == 0
        || request.reserved != [0; 6]
        || path_length == 0
        || path_length > request.path.len()
    {
        reply.status = protocol::status::INVALID;
        return reply;
    }
    let path = &request.path[..path_length];
    if path[0] != b'/' || (path_length > 1 && path[path_length - 1] == b'/') {
        reply.status = protocol::status::INVALID;
        return reply;
    }
    for route in ROUTES {
        if path == route.path
            || route.path == b"/"
            || (path.starts_with(route.path) && path.get(route.path.len()) == Some(&b'/'))
        {
            reply.route_id = route.id;
            reply.backend = route.backend;
            reply.prefix_length = route.path.len() as u16;
            let backing_prefix = if route.backend == protocol::backend::NULLFS
                && route.id != protocol::route::NULLSTAR_VOLUME
            {
                Some(route.path)
            } else {
                None
            };
            if let Some(backing_prefix) = backing_prefix {
                reply.flags = protocol::reply_flags::BINDING;
                reply.backing_prefix_length = backing_prefix.len() as u16;
                reply.backing_prefix[..backing_prefix.len()].copy_from_slice(backing_prefix);
            }
            return reply;
        }
    }
    reply.status = protocol::status::NOT_FOUND;
    reply
}

fn valid_bootstrap(handle: u64, kind: ObjectKind, rights: Rights) -> bool {
    matches!(ipc::wait_for_handle(handle), Ok(info) if info.kind == kind && info.rights == rights)
}
