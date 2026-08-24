#![no_std]
#![no_main]

use userspace::{
    filesystem::{Node, connect_service, protocol as filesystem_protocol},
    ipc::{self, ObjectKind, Rights},
    syscall,
    tmpfs::{Error, Mount},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SERVICE_SLOT: u64 = 1;
const NAME: &[u8] = b"phase4.txt";
const PAYLOAD: &[u8] = b"restart-aware userspace tmpfs";
const GENERIC_PAYLOAD: &[u8] = b"generic shared-memory filesystem service payload";
const GENERIC_BUFFER_ID: u64 = 1;
const GENERIC_READ_OFFSET: usize = 64;
const DIRECTORY_BUFFER_OFFSET: usize = 128;
const CREATED_NAME: &[u8] = b"generic-created.txt";

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let (service, info) = match ipc::wait_for_handle_at_slot(SERVICE_SLOT) {
        Ok(capability) => capability,
        Err(_) => syscall::exit(1),
    };
    if info.kind != ObjectKind::Endpoint || info.rights != Rights::SEND {
        syscall::exit(2);
    }
    let mount = match Mount::connect(service) {
        Ok(mount) => mount,
        Err(_) => syscall::exit(3),
    };
    if mount.generation() == 0 || mount.write(NAME, PAYLOAD).ok() != Some(PAYLOAD.len()) {
        syscall::exit(4);
    }
    if mount.stat(NAME).ok() != Some(PAYLOAD.len()) {
        syscall::exit(5);
    }
    let mut read_buffer = [0_u8; 64];
    let count = match mount.read(NAME, 0, &mut read_buffer) {
        Ok(count) => count,
        Err(_) => syscall::exit(6),
    };
    if count != PAYLOAD.len() || &read_buffer[..count] != PAYLOAD {
        syscall::exit(7);
    }
    let mut listing = [0_u8; 64];
    let listed = match mount.list(&mut listing) {
        Ok(count) => count,
        Err(_) => syscall::exit(8),
    };
    if &listing[..listed] != NAME {
        syscall::exit(9);
    }
    let session = match connect_service(service, 1) {
        Ok(session) => session,
        Err(_) => syscall::exit(10),
    };
    let node = match session.lookup_node(2, Node::root(session), NAME) {
        Ok(node) => node,
        Err(_) => syscall::exit(11),
    };
    let attributes = match session.attributes(3, node) {
        Ok(attributes) => attributes,
        Err(_) => syscall::exit(12),
    };
    if attributes.node_id != node.id()
        || attributes.kind != userspace::filesystem::protocol::node_kind::FILE
        || attributes.size != PAYLOAD.len() as u64
    {
        syscall::exit(13);
    }
    if session.stable_identity(17, node)
        != Err(userspace::filesystem::Error::Service(
            filesystem_protocol::status::NOT_SUPPORTED,
        ))
    {
        syscall::exit(37);
    }
    let shared_memory = match ipc::shared_memory_create(512) {
        Ok(handle) => handle,
        Err(_) => syscall::exit(14),
    };
    if ipc::shared_memory_write(shared_memory, 0, GENERIC_PAYLOAD).ok()
        != Some(GENERIC_PAYLOAD.len())
        || session
            .attach_shared_buffer(4, GENERIC_BUFFER_ID, shared_memory, 512)
            .is_err()
    {
        syscall::exit(15);
    }
    let write_buffer = filesystem_protocol::BulkBuffer {
        buffer_id: GENERIC_BUFFER_ID,
        offset: 0,
        length: GENERIC_PAYLOAD.len() as u64,
    };
    if session
        .write_from_shared_buffer(5, node, 0, write_buffer, false)
        .ok()
        != Some(GENERIC_PAYLOAD.len())
    {
        syscall::exit(16);
    }
    let read_buffer = filesystem_protocol::BulkBuffer {
        buffer_id: GENERIC_BUFFER_ID,
        offset: GENERIC_READ_OFFSET as u64,
        length: GENERIC_PAYLOAD.len() as u64,
    };
    if session.read_to_shared_buffer(6, node, 0, read_buffer).ok() != Some(GENERIC_PAYLOAD.len()) {
        syscall::exit(17);
    }
    let mut generic_readback = [0_u8; 64];
    if ipc::shared_memory_read(
        shared_memory,
        GENERIC_READ_OFFSET,
        &mut generic_readback[..GENERIC_PAYLOAD.len()],
    )
    .ok()
        != Some(GENERIC_PAYLOAD.len())
        || &generic_readback[..GENERIC_PAYLOAD.len()] != GENERIC_PAYLOAD
    {
        syscall::exit(18);
    }
    let mut legacy_readback = [0_u8; 64];
    if mount.read(NAME, 0, &mut legacy_readback).ok() != Some(GENERIC_PAYLOAD.len())
        || &legacy_readback[..GENERIC_PAYLOAD.len()] != GENERIC_PAYLOAD
    {
        syscall::exit(19);
    }
    let attributes = match session.attributes(7, node) {
        Ok(attributes) => attributes,
        Err(_) => syscall::exit(20),
    };
    if attributes.size != GENERIC_PAYLOAD.len() as u64 {
        syscall::exit(21);
    }
    let created = match session.create_file(8, Node::root(session), CREATED_NAME, true, false) {
        Ok(node) => node,
        Err(_) => syscall::exit(22),
    };
    let opened = match session.open_node(
        9,
        created,
        filesystem_protocol::request_flags::READ | filesystem_protocol::request_flags::WRITE,
    ) {
        Ok(node) => node,
        Err(_) => syscall::exit(23),
    };
    if opened.id() != created.id() {
        syscall::exit(24);
    }
    let directory_buffer = filesystem_protocol::BulkBuffer {
        buffer_id: GENERIC_BUFFER_ID,
        offset: DIRECTORY_BUFFER_OFFSET as u64,
        length: (2 * core::mem::size_of::<filesystem_protocol::DirectoryEntry>()) as u64,
    };
    let batch =
        match session.read_directory_to_shared_buffer(10, Node::root(session), 0, directory_buffer)
        {
            Ok(batch) => batch,
            Err(_) => syscall::exit(25),
        };
    if batch.count != 2 || !batch.end {
        syscall::exit(26);
    }
    let mut directory_bytes =
        [0_u8; 2 * core::mem::size_of::<filesystem_protocol::DirectoryEntry>()];
    if ipc::shared_memory_read(shared_memory, DIRECTORY_BUFFER_OFFSET, &mut directory_bytes).ok()
        != Some(directory_bytes.len())
    {
        syscall::exit(27);
    }
    let first = unsafe {
        core::ptr::read_unaligned(
            directory_bytes.as_ptr() as *const filesystem_protocol::DirectoryEntry
        )
    };
    let second = unsafe {
        core::ptr::read_unaligned(
            directory_bytes
                .as_ptr()
                .add(core::mem::size_of::<filesystem_protocol::DirectoryEntry>())
                as *const filesystem_protocol::DirectoryEntry,
        )
    };
    if first.node_id != node.id()
        || &first.name[..usize::from(first.name_length)] != NAME
        || second.node_id != created.id()
        || second.next_cookie != created.id()
        || &second.name[..usize::from(second.name_length)] != CREATED_NAME
    {
        syscall::exit(28);
    }
    if session
        .unlink(11, Node::root(session), CREATED_NAME)
        .is_err()
    {
        syscall::exit(29);
    }
    let unlinked = match session.attributes(12, opened) {
        Ok(attributes) => attributes,
        Err(_) => syscall::exit(30),
    };
    if unlinked.link_count != 0 || session.close_node(13, opened).is_err() {
        syscall::exit(31);
    }
    if session.attributes(14, opened) != Err(userspace::filesystem::Error::StaleNode) {
        syscall::exit(32);
    }
    if session.detach_shared_buffer(15, GENERIC_BUFFER_ID).is_err()
        || ipc::close(shared_memory).is_err()
    {
        syscall::exit(33);
    }
    if session.disconnect(16).is_err() {
        syscall::exit(34);
    }
    if mount.remove(NAME).is_err() || mount.stat(NAME) != Err(Error::NotFound) {
        syscall::exit(35);
    }
    if mount.disconnect().is_err() {
        syscall::exit(36);
    }
    syscall::exit(0)
}
