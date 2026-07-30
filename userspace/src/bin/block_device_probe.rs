#![no_std]
#![no_main]

use nullfs_format::{BLOCK_SIZE, MountMode, Superblock};
use userspace::{
    args::Args,
    block_device::{self, Error},
    ipc::{self, ObjectKind, Rights},
    platform, syscall,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SERVICE_HANDLE: u64 = 1;
const FAT_PARTITION_INDEX: u32 = 2;
const NULLFS_PARTITION_INDEX: u32 = 3;
const NULLFS_SUPERBLOCK_SECTOR: u64 = 128;
const NULLFS_SUPERBLOCK_SECTORS: u32 = 8;
const NULLFS_BLOCK_COUNT: u64 = 256;
const NULLFS_BLOCK_DEVICE_SECTORS: u64 = NULLFS_BLOCK_COUNT * (BLOCK_SIZE / BLOCK_BYTES) as u64;
// The deterministic fixture allocates upward from block 168 and leaves final block 255 free.
const NULLFS_WRITABLE_PROBE_SECTOR: u64 =
    (NULLFS_BLOCK_COUNT - 1) * (BLOCK_SIZE / BLOCK_BYTES) as u64;
const NULLFS_LABEL: &str = "NULLSTAR_DATA";
const NULLFS_UUID: [u8; 16] = [
    0x4e, 0x75, 0x6c, 0x6c, 0x53, 0x74, 0x61, 0x72, 0x2d, 0x4e, 0x75, 0x6c, 0x6c, 0x46, 0x53, 0x01,
];
const BUFFER_ID: u64 = 1;
const BLOCK_BYTES: usize = 512;
const BUFFER_BYTES: usize = 8192;

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let read_only_nullfs_mode = arguments.get(1) == Some(b"nullfs");
    let writable_nullfs_mode = arguments.get(1) == Some(b"nullfs-writable");
    let nullfs_mode = read_only_nullfs_mode || writable_nullfs_mode;
    if arguments.len() != usize::from(nullfs_mode) + 1 {
        syscall::exit(1);
    }
    let partition_index = if nullfs_mode {
        NULLFS_PARTITION_INDEX
    } else {
        FAT_PARTITION_INDEX
    };
    if !matches!(
        ipc::wait_for_handle(SERVICE_HANDLE),
        Ok(info) if info.kind == ObjectKind::Endpoint && info.rights == Rights::SEND
    ) {
        syscall::exit(1);
    }
    if platform::open_block_device_endpoint(partition_index).err()
        != Some(platform::Errno::PERMISSION)
    {
        syscall::exit(2);
    }
    if writable_nullfs_mode
        && platform::open_writable_block_device_endpoint(partition_index).err()
            != Some(platform::Errno::PERMISSION)
    {
        syscall::exit(20);
    }

    let mut session = match block_device::connect_service(SERVICE_HANDLE, 1) {
        Ok(session) => session,
        Err(_) => syscall::exit(3),
    };
    let info = match session.info(2) {
        Ok(info) => info,
        Err(_) => syscall::exit(4),
    };
    if writable_nullfs_mode {
        if info.logical_block_size() != BLOCK_BYTES as u32
            || info.block_count() != NULLFS_BLOCK_DEVICE_SECTORS
            || info.is_read_only()
            || !info.supports(
                block_device::protocol::features::READ
                    | block_device::protocol::features::WRITE
                    | block_device::protocol::features::FLUSH,
            )
        {
            syscall::exit(5);
        }
    } else if info.logical_block_size() != BLOCK_BYTES as u32
        || info.block_count() == 0
        || !info.is_read_only()
        || !info.supports(block_device::protocol::features::READ)
        || info.supports(block_device::protocol::features::WRITE)
    {
        syscall::exit(5);
    }

    let shared_memory = match ipc::shared_memory_create(BUFFER_BYTES) {
        Ok(handle) => handle,
        Err(_) => syscall::exit(6),
    };
    if session
        .attach_shared_buffer(3, BUFFER_ID, shared_memory, BUFFER_BYTES)
        .is_err()
    {
        syscall::exit(7);
    }
    if writable_nullfs_mode {
        probe_writable_nullfs(&mut session, info, shared_memory);
    }
    if read_only_nullfs_mode {
        probe_nullfs(&mut session, info, shared_memory);
    }
    if session.read_to_shared_buffer(4, info, 0, 1, 0).ok() != Some(1) {
        syscall::exit(8);
    }

    let mut block = [0_u8; BLOCK_BYTES];
    if ipc::shared_memory_read(shared_memory, 0, &mut block).ok() != Some(block.len())
        || &block[3..11] != b"MSWIN4.1"
        || u16::from_le_bytes([block[11], block[12]]) != BLOCK_BYTES as u16
        || block[16] != 2
        || &block[54..62] != b"FAT16   "
        || block[510..512] != [0x55, 0xaa]
    {
        syscall::exit(9);
    }
    let mut range_request = transfer_request(
        &session,
        block_device::protocol::operation::READ,
        5,
        info.block_count(),
        1,
    );
    if session.exchange_protocol_request(&range_request) != Err(Error::Range) {
        syscall::exit(10);
    }
    let write_request =
        transfer_request(&session, block_device::protocol::operation::WRITE, 6, 0, 1);
    if session.exchange_protocol_request(&write_request) != Err(Error::ReadOnly) {
        syscall::exit(11);
    }
    range_request.request_id = 7;
    range_request.block_offset = 0;
    range_request.block_count = 9;
    range_request.buffer_length = 9 * BLOCK_BYTES as u64;
    if session.exchange_protocol_request(&range_request) != Err(Error::Range)
        || session.flush(8) != Err(Error::NotSupported)
        || session
            .read_to_shared_buffer(9, info, 0, 1, BLOCK_BYTES)
            .ok()
            != Some(1)
    {
        syscall::exit(12);
    }
    let mut reread = [0_u8; BLOCK_BYTES];
    if ipc::shared_memory_read(shared_memory, BLOCK_BYTES, &mut reread).ok() != Some(reread.len())
        || reread != block
    {
        syscall::exit(13);
    }
    if session.disconnect(10).is_err() || ipc::close(shared_memory).is_err() {
        syscall::exit(14);
    }
    syscall::exit(0)
}

fn probe_writable_nullfs(
    session: &mut block_device::Session,
    info: block_device::DeviceInfo,
    shared_memory: u64,
) -> ! {
    if session
        .read_to_shared_buffer(4, info, NULLFS_WRITABLE_PROBE_SECTOR, 1, 0)
        .ok()
        != Some(1)
    {
        syscall::exit(21);
    }
    let mut observed = [0_u8; BLOCK_BYTES];
    if ipc::shared_memory_read(shared_memory, 0, &mut observed).ok() != Some(observed.len()) {
        syscall::exit(22);
    }
    let original = [0_u8; BLOCK_BYTES];
    let mut modified = original;
    modified[0] = 0xa5;
    if observed == modified {
        let recovered = ipc::shared_memory_write(shared_memory, 0, &original).ok()
            == Some(original.len())
            && session.write_from_shared_buffer(5, info, NULLFS_WRITABLE_PROBE_SECTOR, 1, 0)
                == Ok(1)
            && session.flush(6).is_ok()
            && session
                .read_to_shared_buffer(7, info, NULLFS_WRITABLE_PROBE_SECTOR, 1, 3 * BLOCK_BYTES)
                .ok()
                == Some(1);
        let mut reread = [0_u8; BLOCK_BYTES];
        if !recovered
            || ipc::shared_memory_read(shared_memory, 3 * BLOCK_BYTES, &mut reread).ok()
                != Some(reread.len())
            || reread != original
        {
            syscall::exit(29);
        }
    } else if observed != original {
        syscall::exit(22);
    }
    if ipc::shared_memory_write(shared_memory, BLOCK_BYTES, &modified).ok() != Some(modified.len())
    {
        syscall::exit(23);
    }

    let mut failure = 0;
    if session.write_from_shared_buffer(8, info, NULLFS_WRITABLE_PROBE_SECTOR, 1, BLOCK_BYTES)
        != Ok(1)
    {
        failure = 24;
    }
    if failure == 0 && session.flush(9).is_err() {
        failure = 25;
    }
    if failure == 0
        && session
            .read_to_shared_buffer(10, info, NULLFS_WRITABLE_PROBE_SECTOR, 1, 2 * BLOCK_BYTES)
            .ok()
            != Some(1)
    {
        failure = 26;
    }
    if failure == 0 {
        let mut reread = [0_u8; BLOCK_BYTES];
        if ipc::shared_memory_read(shared_memory, 2 * BLOCK_BYTES, &mut reread).ok()
            != Some(reread.len())
            || reread != modified
        {
            failure = 27;
        }
    }
    if failure == 0 {
        let range_request = transfer_request(
            session,
            block_device::protocol::operation::WRITE,
            11,
            info.block_count(),
            1,
        );
        if session.exchange_protocol_request(&range_request) != Err(Error::Range) {
            failure = 28;
        }
    }

    let original_staged =
        ipc::shared_memory_write(shared_memory, 0, &original).ok() == Some(original.len());
    let original_written =
        session.write_from_shared_buffer(12, info, NULLFS_WRITABLE_PROBE_SECTOR, 1, 0) == Ok(1);
    let original_flushed = session.flush(13).is_ok();
    let original_reread = session
        .read_to_shared_buffer(14, info, NULLFS_WRITABLE_PROBE_SECTOR, 1, 3 * BLOCK_BYTES)
        .ok()
        == Some(1);
    let mut restored = [0_u8; BLOCK_BYTES];
    let original_verified = ipc::shared_memory_read(shared_memory, 3 * BLOCK_BYTES, &mut restored)
        .ok()
        == Some(restored.len())
        && restored == original;
    let restoration_succeeded = original_staged
        && original_written
        && original_flushed
        && original_reread
        && original_verified;

    let disconnected = session.disconnect(15).is_ok();
    let shared_memory_closed = ipc::close(shared_memory).is_ok();
    if !restoration_succeeded {
        syscall::exit(29);
    }
    if !disconnected || !shared_memory_closed {
        syscall::exit(30);
    }
    if failure != 0 {
        syscall::exit(failure);
    }
    syscall::exit(0)
}

fn probe_nullfs(
    session: &mut block_device::Session,
    info: block_device::DeviceInfo,
    shared_memory: u64,
) -> ! {
    if session
        .read_to_shared_buffer(
            4,
            info,
            NULLFS_SUPERBLOCK_SECTOR,
            NULLFS_SUPERBLOCK_SECTORS,
            0,
        )
        .ok()
        != Some(NULLFS_SUPERBLOCK_SECTORS)
    {
        syscall::exit(15);
    }
    let mut superblock_bytes = [0_u8; BLOCK_SIZE];
    if ipc::shared_memory_read(shared_memory, 0, &mut superblock_bytes).ok()
        != Some(superblock_bytes.len())
    {
        syscall::exit(16);
    }
    let Some(device_bytes) = info.block_count().checked_mul(BLOCK_BYTES as u64) else {
        syscall::exit(17);
    };
    let Ok(superblock) =
        Superblock::decode(&superblock_bytes, Some(device_bytes), MountMode::ReadOnly)
    else {
        syscall::exit(17);
    };
    if superblock.label() != NULLFS_LABEL
        || superblock.filesystem_uuid != NULLFS_UUID
        || superblock.capacity_blocks != NULLFS_BLOCK_COUNT
    {
        syscall::exit(18);
    }
    let write_request = transfer_request(
        session,
        block_device::protocol::operation::WRITE,
        5,
        NULLFS_SUPERBLOCK_SECTOR,
        NULLFS_SUPERBLOCK_SECTORS,
    );
    let range_request = transfer_request(
        session,
        block_device::protocol::operation::READ,
        6,
        info.block_count(),
        1,
    );
    if session.exchange_protocol_request(&write_request) != Err(Error::ReadOnly)
        || session.exchange_protocol_request(&range_request) != Err(Error::Range)
        || session.flush(7) != Err(Error::NotSupported)
        || session.disconnect(8).is_err()
        || ipc::close(shared_memory).is_err()
    {
        syscall::exit(19);
    }
    syscall::exit(0)
}

fn transfer_request(
    session: &block_device::Session,
    operation: u16,
    request_id: u64,
    block_offset: u64,
    block_count: u32,
) -> block_device::protocol::Request {
    let mut request = block_device::protocol::Request::EMPTY;
    request.operation = operation;
    request.request_id = request_id;
    request.session_id = session.id();
    request.generation = session.generation();
    request.buffer_id = BUFFER_ID;
    request.buffer_length = u64::from(block_count) * BLOCK_BYTES as u64;
    request.block_offset = block_offset;
    request.block_count = block_count;
    request
}
