#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate alloc;

#[path = "../allocator.rs"]
mod allocator;

use core::alloc::Layout;

use boot_generation::{SELECTION_RECORD_BYTES, Selection};
use nullfs_core::Filesystem;
use nullfs_userspace_blockdev::SessionBlockDevice;
use userspace::{
    block_device::{self, protocol},
    boot_generation_fixture as fixture,
    ipc::{self, ObjectKind, Rights},
    nullfs_primary_volume, platform,
    syscall::{self, OpenFlags},
};

userspace::entry!(rust_main);

const BLOCK_SLOT: u64 = 1;
const SHARED_BUFFER_BYTES: usize = 4096;
const SHARED_BUFFER_ID: u64 = 1;
const FILE_BUFFER_BYTES: usize = 128;

const STAGED_MARKER: &[u8] = b"boot-generation-probe: generation 2 staged and selected\n";
const ROLLBACK_MARKER: &[u8] = b"boot-generation-probe: generation 1 rollback selected\n";
const VERIFIED_MARKER: &[u8] =
    b"boot-generation-probe: rollback persisted with both generations retained\n";

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    allocator::init();

    let block = match ipc::wait_for_handle_at_slot(BLOCK_SLOT) {
        Ok((handle, info)) if info.kind == ObjectKind::Endpoint && info.rights == Rights::SEND => {
            handle
        }
        _ => fail(
            10,
            b"boot-generation-probe: slot 1 must contain Endpoint SEND\n",
        ),
    };
    if platform::open_writable_nullfs_block_device_endpoint(&nullfs_primary_volume::FILESYSTEM_UUID)
        .err()
        != Some(platform::Errno::PERMISSION)
    {
        fail(
            11,
            b"boot-generation-probe: writable authority was ambient\n",
        );
    }

    let mut session = block_device::connect_service(block, 1)
        .unwrap_or_else(|_| fail(20, b"boot-generation-probe: block connect failed\n"));
    let info = session
        .info(2)
        .unwrap_or_else(|_| fail(21, b"boot-generation-probe: block info failed\n"));
    if info.is_read_only()
        || !info.supports(
            protocol::features::READ | protocol::features::WRITE | protocol::features::FLUSH,
        )
    {
        fail(
            22,
            b"boot-generation-probe: writable durable block rights missing\n",
        );
    }

    let shared_memory = ipc::shared_memory_create(SHARED_BUFFER_BYTES)
        .unwrap_or_else(|_| fail(23, b"boot-generation-probe: shared buffer failed\n"));
    session
        .attach_shared_buffer(3, SHARED_BUFFER_ID, shared_memory, SHARED_BUFFER_BYTES)
        .unwrap_or_else(|_| fail(24, b"boot-generation-probe: buffer attach failed\n"));
    let device = SessionBlockDevice::new(session, info, 4)
        .unwrap_or_else(|_| fail(25, b"boot-generation-probe: block geometry invalid\n"));
    let mut filesystem = Filesystem::try_mount_read_write(device)
        .unwrap_or_else(|_| fail(26, b"boot-generation-probe: NullFS mount failed\n"));
    if filesystem.superblock().filesystem_uuid != nullfs_primary_volume::FILESYSTEM_UUID {
        fail(27, b"boot-generation-probe: NullFS identity mismatch\n");
    }

    verify_canonical_artifacts(&mut filesystem);
    let canonical_bytes = read_nullfs_selection(&mut filesystem);
    let firmware_bytes = read_fat_selection();
    if canonical_bytes != firmware_bytes {
        fail(
            30,
            b"boot-generation-probe: canonical and firmware selectors differ\n",
        );
    }
    let current = Selection::decode(&canonical_bytes)
        .unwrap_or_else(|_| fail(31, b"boot-generation-probe: selector is invalid\n"));

    let marker = if current == fixture::initial_selection() {
        stage_generation_2(&mut filesystem, &canonical_bytes);
        STAGED_MARKER
    } else if current == fixture::staged_selection() {
        rollback_generation_1(&mut filesystem);
        ROLLBACK_MARKER
    } else if current == fixture::rollback_selection() {
        verify_fat_file(fixture::FIRMWARE_SLOT_0_PATH, fixture::GENERATION_1_KERNEL);
        verify_fat_file(fixture::FIRMWARE_SLOT_1_PATH, fixture::GENERATION_2_KERNEL);
        VERIFIED_MARKER
    } else {
        fail(32, b"boot-generation-probe: unexpected selection phase\n");
    };

    cleanly_disconnect(filesystem, shared_memory);
    if syscall::write_all(syscall::STDOUT, marker).is_err() {
        syscall::exit(40);
    }
    syscall::exit(0)
}

fn stage_generation_2(
    filesystem: &mut Filesystem<SessionBlockDevice>,
    initial_selector: &[u8; SELECTION_RECORD_BYTES],
) {
    verify_fat_file(fixture::FIRMWARE_SLOT_0_PATH, fixture::GENERATION_1_KERNEL);
    verify_fat_file(fixture::FIRMWARE_SLOT_1_PATH, b"");

    write_fat_file(fixture::FIRMWARE_SLOT_1_PATH, fixture::GENERATION_2_KERNEL);
    verify_fat_file(fixture::FIRMWARE_SLOT_1_PATH, fixture::GENERATION_2_KERNEL);
    if read_fat_selection() != *initial_selector {
        fail(
            50,
            b"boot-generation-probe: staging changed active selector\n",
        );
    }

    let staged = fixture::staged_selection().encode();
    write_nullfs_selection(filesystem, &staged);
    write_fat_file(fixture::FIRMWARE_SELECTION_PATH, &staged);
    verify_selection_pair(filesystem, &staged);
    verify_fat_file(fixture::FIRMWARE_SLOT_0_PATH, fixture::GENERATION_1_KERNEL);
    verify_fat_file(fixture::FIRMWARE_SLOT_1_PATH, fixture::GENERATION_2_KERNEL);
}

fn rollback_generation_1(filesystem: &mut Filesystem<SessionBlockDevice>) {
    verify_fat_file(fixture::FIRMWARE_SLOT_0_PATH, fixture::GENERATION_1_KERNEL);
    verify_fat_file(fixture::FIRMWARE_SLOT_1_PATH, fixture::GENERATION_2_KERNEL);

    let rollback = fixture::rollback_selection().encode();
    write_nullfs_selection(filesystem, &rollback);
    write_fat_file(fixture::FIRMWARE_SELECTION_PATH, &rollback);
    verify_selection_pair(filesystem, &rollback);
    verify_fat_file(fixture::FIRMWARE_SLOT_0_PATH, fixture::GENERATION_1_KERNEL);
    verify_fat_file(fixture::FIRMWARE_SLOT_1_PATH, fixture::GENERATION_2_KERNEL);
}

fn verify_canonical_artifacts(filesystem: &mut Filesystem<SessionBlockDevice>) {
    verify_nullfs_file(
        filesystem,
        fixture::GENERATION_1_PATH,
        fixture::GENERATION_1_KERNEL,
    );
    verify_nullfs_file(
        filesystem,
        fixture::GENERATION_1_MANIFEST_PATH,
        fixture::GENERATION_1_MANIFEST,
    );
    verify_nullfs_file(
        filesystem,
        fixture::GENERATION_2_PATH,
        fixture::GENERATION_2_KERNEL,
    );
    verify_nullfs_file(
        filesystem,
        fixture::GENERATION_2_MANIFEST_PATH,
        fixture::GENERATION_2_MANIFEST,
    );
}

fn verify_selection_pair(
    filesystem: &mut Filesystem<SessionBlockDevice>,
    expected: &[u8; SELECTION_RECORD_BYTES],
) {
    let canonical = read_nullfs_selection(filesystem);
    let firmware = read_fat_selection();
    if canonical != *expected
        || firmware != *expected
        || Selection::decode(&canonical).is_err()
        || Selection::decode(&firmware).is_err()
    {
        fail(
            60,
            b"boot-generation-probe: selector publication verification failed\n",
        );
    }
}

fn read_nullfs_selection(
    filesystem: &mut Filesystem<SessionBlockDevice>,
) -> [u8; SELECTION_RECORD_BYTES] {
    let mut bytes = [0; SELECTION_RECORD_BYTES];
    let node = filesystem
        .lookup_path(filesystem.root(), fixture::CANONICAL_SELECTION_PATH)
        .unwrap_or_else(|_| fail(61, b"boot-generation-probe: selection lookup failed\n"));
    let attributes = filesystem
        .attributes(node)
        .unwrap_or_else(|_| fail(62, b"boot-generation-probe: selection stat failed\n"));
    if attributes.size != SELECTION_RECORD_BYTES as u64
        || filesystem.read(node, 0, &mut bytes).ok() != Some(bytes.len())
    {
        fail(
            63,
            b"boot-generation-probe: selection read was incomplete\n",
        );
    }
    bytes
}

fn write_nullfs_selection(
    filesystem: &mut Filesystem<SessionBlockDevice>,
    bytes: &[u8; SELECTION_RECORD_BYTES],
) {
    let node = filesystem
        .lookup_path(filesystem.root(), fixture::CANONICAL_SELECTION_PATH)
        .unwrap_or_else(|_| fail(64, b"boot-generation-probe: selection lookup failed\n"));
    if filesystem.write(node, 0, bytes).ok() != Some(bytes.len()) || filesystem.sync().is_err() {
        fail(
            65,
            b"boot-generation-probe: canonical selector write failed\n",
        );
    }
    if read_nullfs_selection(filesystem) != *bytes {
        fail(
            66,
            b"boot-generation-probe: canonical selector readback failed\n",
        );
    }
}

fn verify_nullfs_file(
    filesystem: &mut Filesystem<SessionBlockDevice>,
    path: &str,
    expected: &[u8],
) {
    if expected.len() > FILE_BUFFER_BYTES {
        fail(70, b"boot-generation-probe: fixture exceeds buffer\n");
    }
    let node = filesystem
        .lookup_path(filesystem.root(), path)
        .unwrap_or_else(|_| fail(71, b"boot-generation-probe: canonical artifact missing\n"));
    let attributes = filesystem
        .attributes(node)
        .unwrap_or_else(|_| fail(72, b"boot-generation-probe: artifact stat failed\n"));
    let mut bytes = [0; FILE_BUFFER_BYTES];
    if attributes.size != expected.len() as u64
        || filesystem.read(node, 0, &mut bytes[..expected.len()]).ok() != Some(expected.len())
        || &bytes[..expected.len()] != expected
    {
        fail(73, b"boot-generation-probe: canonical artifact mismatch\n");
    }
}

fn read_fat_selection() -> [u8; SELECTION_RECORD_BYTES] {
    let mut bytes = [0; SELECTION_RECORD_BYTES];
    let count = read_fat_file(fixture::FIRMWARE_SELECTION_PATH, &mut bytes);
    if count != bytes.len() || Selection::decode(&bytes).is_err() {
        fail(80, b"boot-generation-probe: firmware selector invalid\n");
    }
    bytes
}

fn verify_fat_file(path: &[u8], expected: &[u8]) {
    if expected.len() > FILE_BUFFER_BYTES {
        fail(81, b"boot-generation-probe: FAT fixture exceeds buffer\n");
    }
    let mut bytes = [0; FILE_BUFFER_BYTES];
    let count = read_fat_file(path, &mut bytes);
    if count != expected.len() || &bytes[..count] != expected {
        fail(82, b"boot-generation-probe: FAT artifact mismatch\n");
    }
}

fn read_fat_file(path: &[u8], output: &mut [u8]) -> usize {
    let descriptor = syscall::open(path, OpenFlags::READ)
        .unwrap_or_else(|_| fail(83, b"boot-generation-probe: FAT open failed\n"));
    let mut completed = 0;
    loop {
        if completed == output.len() {
            let mut extra = [0; 1];
            if syscall::read(descriptor, &mut extra).ok() != Some(0) {
                let _ = syscall::close(descriptor);
                fail(84, b"boot-generation-probe: FAT file too large\n");
            }
            break;
        }
        let count = syscall::read(descriptor, &mut output[completed..])
            .unwrap_or_else(|_| fail(85, b"boot-generation-probe: FAT read failed\n"));
        if count == 0 {
            break;
        }
        completed += count;
    }
    syscall::close(descriptor)
        .unwrap_or_else(|_| fail(86, b"boot-generation-probe: FAT close failed\n"));
    completed
}

fn write_fat_file(path: &[u8], bytes: &[u8]) {
    let descriptor = syscall::open(
        path,
        OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
    )
    .unwrap_or_else(|_| fail(87, b"boot-generation-probe: FAT write open failed\n"));
    syscall::write_all(descriptor, bytes)
        .unwrap_or_else(|_| fail(88, b"boot-generation-probe: FAT write failed\n"));
    syscall::close(descriptor)
        .unwrap_or_else(|_| fail(89, b"boot-generation-probe: FAT write close failed\n"));
}

fn cleanly_disconnect(filesystem: Filesystem<SessionBlockDevice>, shared_memory: u64) {
    let device = filesystem
        .unmount()
        .unwrap_or_else(|_| fail(90, b"boot-generation-probe: NullFS unmount failed\n"));
    let request_id = device
        .next_request_id()
        .unwrap_or_else(|| fail(91, b"boot-generation-probe: request IDs exhausted\n"));
    let mut session = device.into_session();
    session
        .disconnect(request_id)
        .unwrap_or_else(|_| fail(92, b"boot-generation-probe: block disconnect failed\n"));
    ipc::close(shared_memory)
        .unwrap_or_else(|_| fail(93, b"boot-generation-probe: shared-buffer close failed\n"));
}

fn fail(code: u64, message: &[u8]) -> ! {
    let _ = syscall::write_all(syscall::STDERR, message);
    syscall::exit(code)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    fail(101, b"boot-generation-probe: panic\n")
}

#[alloc_error_handler]
fn allocation_error(_layout: Layout) -> ! {
    fail(102, b"boot-generation-probe: process heap exhausted\n")
}
