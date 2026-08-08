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
    abi::INIT_PROCESS_ID,
    args::Args,
    block_device::{self, protocol},
    ipc::{self, ObjectKind, Rights},
    nullfs_primary_volume,
    service_route::receive_service_generation,
    syscall,
};

userspace::entry!(rust_main);

const READY_HANDLE: u64 = 1;
const REQUEST_HANDLE: u64 = 2;
const BLOCK_HANDLE: u64 = 3;
const CRASH_TEST_HANDLE: u64 = 4;
const GENERATION_HANDOFF_HANDLE: u64 = 5;
const READY_MESSAGE: &[u8] = b"service-ready: nullfs";
const SHARED_BUFFER_BYTES: usize = 4096;
const SHARED_BUFFER_ID: u64 = 1;

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    allocator::init();

    let arguments = unsafe { Args::from_stack(initial_stack) };
    let crash_test = if arguments.len() == 2
        && arguments.get(0) == Some(b"/nullfs-service")
        && arguments.get(1) == Some(b"--writable")
    {
        false
    } else if arguments.len() == 3
        && arguments.get(0) == Some(b"/nullfs-service")
        && arguments.get(1) == Some(b"--writable")
        && arguments.get(2) == Some(b"--crash-test")
    {
        true
    } else {
        fail(2, b"usage: /nullfs-service --writable [--crash-test]\n");
    };

    require_handle(
        READY_HANDLE,
        Rights::SEND,
        10,
        b"nullfs: handle 1 must be Endpoint SEND\n",
    );
    require_handle(
        REQUEST_HANDLE,
        Rights::RECEIVE,
        11,
        b"nullfs: handle 2 must be Endpoint RECEIVE\n",
    );
    require_handle(
        BLOCK_HANDLE,
        Rights::SEND,
        12,
        b"nullfs: handle 3 must be Endpoint SEND\n",
    );
    let crash_test_hook = if crash_test {
        require_handle(
            CRASH_TEST_HANDLE,
            Rights::RECEIVE,
            14,
            b"nullfs: handle 4 must be Endpoint RECEIVE in crash-test mode\n",
        );
        Some(server::CrashTestHook::new(CRASH_TEST_HANDLE))
    } else {
        None
    };
    let service_generation =
        match receive_service_generation(GENERATION_HANDOFF_HANDLE, INIT_PROCESS_ID) {
            Ok(generation) => generation.get(),
            Err(_) => fail(13, b"nullfs: generation handoff failed\n"),
        };

    let mut session = match block_device::connect_service(BLOCK_HANDLE, 1) {
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
        crash_test_hook,
    )
}

fn require_handle(handle: u64, rights: Rights, exit_code: u64, message: &[u8]) {
    if !matches!(
        ipc::wait_for_handle(handle),
        Ok(info) if info.kind == ObjectKind::Endpoint && info.rights == rights
    ) {
        fail(exit_code, message);
    }
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
