#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate alloc;

mod allocator;
mod server;

use core::alloc::Layout;

use nullfs_core::Filesystem;
use nullfs_format::NodeKind;
use nullfs_userspace_blockdev::SessionBlockDevice;
use userspace::{
    args::Args,
    block_device::{self, protocol},
    handle::Endpoint,
    ipc::{self, ObjectKind, Rights},
    managed_startup::{ManagedServiceIdentity, numeric_service_id, receive_managed_service_start},
    nullfs_primary_volume, platform,
    runtime_context::{CapabilityRole, StartupCapabilityPolicy},
    syscall,
};

userspace::entry!(rust_main);

const NULLFS_EXECUTABLE_ID: u64 = 2;
const SERVICE_IDENTITY: ManagedServiceIdentity = ManagedServiceIdentity::new(
    NULLFS_EXECUTABLE_ID,
    numeric_service_id(userspace::service_control::NULLFS_SERVICE_ID.into_bytes()),
    NULLFS_EXECUTABLE_ID,
);
const READY_MESSAGE: &[u8] = b"service-ready: nullfs";
const CRASH_TEST_ARGUMENT: &[u8] = b"--crash-test";
const CONTAINMENT_TEST_ARGUMENT: &[u8] = b"--containment-test";
const CONTAINMENT_DESCENDANT_MARKER: &[u8] =
    b"nullfs-service: containment descendant escaped process group\n";
const SHARED_BUFFER_BYTES: usize = 4096;
const SHARED_BUFFER_ID: u64 = 1;

fn spawn_containment_descendant() {
    let group_ready = match syscall::pipe_pair() {
        Ok(pair) => pair,
        Err(_) => syscall::exit(30),
    };
    match syscall::fork() {
        Ok(0) => {
            let _ = syscall::close(group_ready.reader);
            if platform::set_process_group(0, 0).is_err()
                || syscall::write_all(group_ready.writer, &[1]).is_err()
                || syscall::close(group_ready.writer).is_err()
            {
                syscall::exit(31);
            }
            loop {
                if syscall::yield_now().is_err() {
                    syscall::exit(32);
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
                syscall::exit(33);
            }
        }
        Err(_) => {
            let _ = syscall::close(group_ready.writer);
            let _ = syscall::close(group_ready.reader);
            syscall::exit(30);
        }
    }
}

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    allocator::init();

    let arguments = unsafe { Args::from_stack(initial_stack) };
    if !(2..=4).contains(&arguments.len())
        || arguments.get(0) != Some(b"/nullfs-service")
        || arguments.get(1) != Some(b"--writable")
    {
        fail(
            2,
            b"usage: /nullfs-service --writable [--crash-test] [--containment-test]\n",
        );
    }
    let mut crash_test = false;
    let mut containment_test = false;
    for index in 2..arguments.len() {
        match arguments.get(index) {
            Some(CRASH_TEST_ARGUMENT) if !crash_test => crash_test = true,
            Some(CONTAINMENT_TEST_ARGUMENT) if !containment_test => containment_test = true,
            _ => fail(
                2,
                b"usage: /nullfs-service --writable [--crash-test] [--containment-test]\n",
            ),
        }
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
        StartupCapabilityPolicy {
            role: CapabilityRole::BLOCK_DEVICE,
            kind: ObjectKind::Endpoint,
            minimum_rights: Rights::SEND,
            maximum_rights: Rights::SEND,
            required: true,
        },
        StartupCapabilityPolicy {
            role: CapabilityRole::NULLFS_CRASH_TEST_CONTROL,
            kind: ObjectKind::Endpoint,
            minimum_rights: Rights::RECEIVE,
            maximum_rights: Rights::RECEIVE,
            required: crash_test,
        },
    ];
    let policies = if crash_test {
        &policies[..]
    } else {
        &policies[..3]
    };
    let mut start = match receive_managed_service_start::<4>(arguments, policies, SERVICE_IDENTITY)
    {
        Ok(start) => start,
        Err(_) => fail(10, b"nullfs: managed startup validation failed\n"),
    };
    let readiness = match start
        .context
        .take::<Endpoint>(CapabilityRole::READINESS, Rights::SEND)
    {
        Ok(handle) => handle.into_raw(),
        Err(_) => fail(11, b"nullfs: readiness authority missing\n"),
    };
    let request = match start
        .context
        .take::<Endpoint>(CapabilityRole::SERVICE_REQUEST, Rights::RECEIVE)
    {
        Ok(handle) => handle.into_raw(),
        Err(_) => fail(11, b"nullfs: request authority missing\n"),
    };
    let block = match start
        .context
        .take::<Endpoint>(CapabilityRole::BLOCK_DEVICE, Rights::SEND)
    {
        Ok(handle) => handle.into_raw(),
        Err(_) => fail(12, b"nullfs: block-device authority missing\n"),
    };
    let crash_test_hook = if crash_test {
        match start
            .context
            .take::<Endpoint>(CapabilityRole::NULLFS_CRASH_TEST_CONTROL, Rights::RECEIVE)
        {
            Ok(handle) => Some(server::CrashTestHook::new(handle.into_raw())),
            Err(_) => fail(14, b"nullfs: crash-test authority missing\n"),
        }
    } else {
        None
    };
    if !start.context.is_empty() {
        fail(10, b"nullfs: unexpected startup authority\n");
    }
    let service_generation = start.generation;
    if containment_test {
        spawn_containment_descendant();
    }

    let mut session = match block_device::connect_service(block, 1) {
        Ok(session) => session,
        Err(_) => fail(20, b"nullfs: block-device connect failed\n"),
    };
    let info = match session.info(2) {
        Ok(info) => info,
        Err(_) => fail(21, b"nullfs: block-device info query failed\n"),
    };
    if info.is_read_only()
        || !info.supports(
            protocol::features::READ | protocol::features::WRITE | protocol::features::FLUSH,
        )
    {
        fail(
            22,
            b"nullfs: block device must be writable with READ, WRITE, and FLUSH\n",
        );
    }

    let shared_memory = match ipc::shared_memory_create(SHARED_BUFFER_BYTES) {
        Ok(handle) => handle,
        Err(_) => fail(23, b"nullfs: shared-buffer creation failed\n"),
    };
    if session
        .attach_shared_buffer(3, SHARED_BUFFER_ID, shared_memory, SHARED_BUFFER_BYTES)
        .is_err()
    {
        let _ = ipc::close(shared_memory);
        fail(24, b"nullfs: shared-buffer attachment failed\n");
    }

    let device = match SessionBlockDevice::new(session, info, 4) {
        Ok(device) => device,
        Err(_) => fail(25, b"nullfs: invalid block-device geometry\n"),
    };
    let mut filesystem = match Filesystem::try_mount_read_write(device) {
        Ok(filesystem) => filesystem,
        Err(_) => fail(26, b"nullfs: read-write mount failed\n"),
    };

    let superblock = filesystem.superblock();
    if superblock.filesystem_uuid != nullfs_primary_volume::FILESYSTEM_UUID {
        fail(27, b"nullfs: mounted volume identity mismatch\n");
    }

    let root = filesystem.root();
    let root_attributes = match filesystem.attributes(root) {
        Ok(attributes) => attributes,
        Err(_) => fail(28, b"nullfs: root attribute query failed\n"),
    };
    if root_attributes.node != root || root_attributes.kind != NodeKind::Directory {
        fail(29, b"nullfs: root is not a directory\n");
    }
    server::serve(
        filesystem,
        service_generation,
        root_attributes,
        readiness,
        request,
        crash_test_hook,
    )
}

fn fail(code: u64, message: &[u8]) -> ! {
    let _ = syscall::write_all(syscall::STDERR, message);
    syscall::exit(code)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    fail(101, b"nullfs: panic\n")
}

#[alloc_error_handler]
fn allocation_error(_layout: Layout) -> ! {
    fail(70, b"nullfs: process heap exhausted\n")
}
