#![no_std]
#![no_main]

use core::mem::size_of;

use userspace::{
    filesystem::{self, Error, Node, protocol},
    ipc::{self, ObjectKind, Rights},
    syscall,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SERVICE_HANDLE: u64 = 1;
const BUFFER_ID: u64 = 1;
const BUFFER_BYTES: usize = 4096;
const WELCOME: &[u8] = b"NullStar persistent storage service fixture.\n";
const README: &[u8] = b"This volume is prepared for read-only Phase 4 service integration.\n";

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    if !matches!(
        ipc::wait_for_handle(SERVICE_HANDLE),
        Ok(info) if info.kind == ObjectKind::Endpoint && info.rights == Rights::SEND
    ) {
        syscall::exit(1);
    }

    let session = match filesystem::connect_service(SERVICE_HANDLE, 1) {
        Ok(session) => session,
        Err(_) => syscall::exit(2),
    };
    let root = Node::root(session);
    let root_attributes = match session.attributes(2, root) {
        Ok(attributes) => attributes,
        Err(_) => syscall::exit(3),
    };
    if root_attributes.node_id != root.id()
        || root_attributes.kind != protocol::node_kind::DIRECTORY
        || root_attributes.mode != 0o755
    {
        syscall::exit(4);
    }

    let docs = match session.lookup_node(3, root, b"docs") {
        Ok(node) => node,
        Err(_) => syscall::exit(5),
    };
    let welcome = match session.lookup_node(4, root, b"welcome.txt") {
        Ok(node) => node,
        Err(_) => syscall::exit(6),
    };
    if docs.id() == root.id()
        || welcome.id() == root.id()
        || docs.id() == 2
        || welcome.id() == 3
        || session.lookup_node(5, root, b"missing") != Err(Error::NotFound)
    {
        syscall::exit(7);
    }
    let welcome_attributes = match session.attributes(6, welcome) {
        Ok(attributes) => attributes,
        Err(_) => syscall::exit(8),
    };
    if welcome_attributes.node_id != welcome.id()
        || welcome_attributes.kind != protocol::node_kind::FILE
        || welcome_attributes.size != WELCOME.len() as u64
        || welcome_attributes.mode != 0o644
    {
        syscall::exit(9);
    }

    let shared_memory = match ipc::shared_memory_create(BUFFER_BYTES) {
        Ok(handle) => handle,
        Err(_) => syscall::exit(10),
    };
    if session
        .attach_shared_buffer(7, BUFFER_ID, shared_memory, BUFFER_BYTES)
        .is_err()
    {
        syscall::exit(11);
    }

    let read_bulk = protocol::BulkBuffer {
        buffer_id: BUFFER_ID,
        offset: 0,
        length: 128,
    };
    if session.read_to_shared_buffer(8, welcome, 0, read_bulk).ok() != Some(WELCOME.len()) {
        syscall::exit(12);
    }
    let mut welcome_bytes = [0_u8; WELCOME.len()];
    if ipc::shared_memory_read(shared_memory, 0, &mut welcome_bytes).ok()
        != Some(welcome_bytes.len())
        || welcome_bytes != WELCOME
        || session
            .read_to_shared_buffer(9, welcome, WELCOME.len() as u64, read_bulk)
            .ok()
            != Some(0)
    {
        syscall::exit(13);
    }

    if !directory_contains_fixture(session, root, shared_memory) {
        syscall::exit(14);
    }

    let readme = match session.lookup_node(12, docs, b"readme.txt") {
        Ok(node) => node,
        Err(_) => syscall::exit(15),
    };
    let nested_bulk = protocol::BulkBuffer {
        buffer_id: BUFFER_ID,
        offset: 256,
        length: 256,
    };
    if session
        .read_to_shared_buffer(13, readme, 0, nested_bulk)
        .ok()
        != Some(README.len())
    {
        syscall::exit(16);
    }
    let mut readme_bytes = [0_u8; README.len()];
    if ipc::shared_memory_read(shared_memory, 256, &mut readme_bytes).ok()
        != Some(readme_bytes.len())
        || readme_bytes != README
    {
        syscall::exit(17);
    }

    let first_open = match session.open_node(14, welcome, protocol::request_flags::READ) {
        Ok(node) => node,
        Err(_) => syscall::exit(18),
    };
    let second_open = match session.open_node(15, welcome, protocol::request_flags::READ) {
        Ok(node) => node,
        Err(_) => syscall::exit(19),
    };
    if first_open != welcome
        || second_open != welcome
        || session.close_node(16, first_open).is_err()
        || session.close_node(17, second_open).is_err()
        || session.close_node(18, welcome) != Err(Error::StaleNode)
    {
        syscall::exit(20);
    }

    if session.open_node(
        19,
        welcome,
        protocol::request_flags::READ | protocol::request_flags::WRITE,
    ) != Err(Error::Service(protocol::status::PERMISSION))
        || session.write_from_shared_buffer(20, welcome, 0, read_bulk, false)
            != Err(Error::Service(protocol::status::PERMISSION))
        || session.create_file(21, root, b"denied", true, false)
            != Err(Error::Service(protocol::status::PERMISSION))
        || session.unlink(22, root, b"welcome.txt")
            != Err(Error::Service(protocol::status::PERMISSION))
    {
        syscall::exit(21);
    }

    if session.detach_shared_buffer(23, BUFFER_ID).is_err()
        || session.disconnect(24).is_err()
        || ipc::close(shared_memory).is_err()
    {
        syscall::exit(22);
    }
    syscall::exit(0)
}

fn directory_contains_fixture(
    session: filesystem::Session,
    root: Node,
    shared_memory: u64,
) -> bool {
    let bytes = size_of::<protocol::DirectoryEntry>();
    let bulk = protocol::BulkBuffer {
        buffer_id: BUFFER_ID,
        offset: 1024,
        length: bytes as u64,
    };
    let mut cookie = 0;
    let mut found_docs = false;
    let mut found_welcome = false;

    for page in 0..2_u64 {
        let Ok(batch) = session.read_directory_to_shared_buffer(10 + page, root, cookie, bulk)
        else {
            return false;
        };
        if batch.count != 1 || batch.end != (page == 1) {
            return false;
        }
        let mut record = protocol::DirectoryEntry::EMPTY;
        let record_bytes = unsafe {
            core::slice::from_raw_parts_mut(
                (&mut record as *mut protocol::DirectoryEntry).cast::<u8>(),
                bytes,
            )
        };
        if ipc::shared_memory_read(shared_memory, 1024, record_bytes).ok() != Some(bytes)
            || record.node_id == protocol::INVALID_ID
            || record.next_cookie <= cookie
        {
            return false;
        }
        cookie = record.next_cookie;

        let Some(name) = entry_name(&record) else {
            return false;
        };
        if name == b"docs" && record.kind == protocol::node_kind::DIRECTORY {
            found_docs = true;
        } else if name == b"welcome.txt" && record.kind == protocol::node_kind::FILE {
            found_welcome = true;
        } else {
            return false;
        }
    }
    found_docs && found_welcome
}

fn entry_name(entry: &protocol::DirectoryEntry) -> Option<&[u8]> {
    let length = usize::from(entry.name_length);
    (length <= protocol::MAX_NAME_BYTES
        && entry.reserved == 0
        && entry.name[length..].iter().all(|byte| *byte == 0))
    .then_some(&entry.name[..length])
}
