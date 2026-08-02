#![no_std]
#![no_main]

use core::mem::size_of;

use userspace::{
    args::Args,
    filesystem::{self, Error, Node, protocol},
    ipc::{self, ObjectKind, Rights},
    syscall,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SERVICE_HANDLE: u64 = 1;
const READINESS_MODE: &[u8] = b"readiness";
const FULL_MODE: &[u8] = b"full";
const BUFFER_ID: u64 = 1;
const BUFFER_BYTES: usize = 4096;
const WELCOME: &[u8] = b"NullStar persistent storage service fixture.\n";
const README: &[u8] = b"This volume is a deterministic NullFS integration fixture.\n";
const WRITABLE_DIRECTORY_A: &[u8] = b"nullfs-probe-a";
const WRITABLE_DIRECTORY_B: &[u8] = b"nullfs-probe-b";
const WRITABLE_FILE: &[u8] = b"payload.bin";
const RENAMED_FILE: &[u8] = b"renamed.bin";
const PUBLIC_VFS_PROBE_FILE: &[u8] = b"nullstar-vfs-probe-v1.bin";
const INITIAL_BYTES: &[u8] = b"NullStar writable";
const APPEND_BYTES: &[u8] = b" probe";
const COMPLETE_BYTES: &[u8] = b"NullStar writable probe";
const TRUNCATED_BYTES: &[u8] = b"NullStar";
const INITIAL_OFFSET: usize = 0;
const APPEND_OFFSET: usize = 128;
const READBACK_OFFSET: usize = 256;
const RENAME_OFFSET: usize = 512;
const DIRECTORY_OFFSET: usize = 1024;
const RECOVERY_READ_OFFSET: usize = 2048;
const ROOT_ENTRY_CAPACITY: usize = 2;
const RECOVERY_ENTRY_CAPACITY: usize = 2;

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let readiness = arguments.len() == 2 && arguments.get(1) == Some(READINESS_MODE);
    let full =
        arguments.len() == 1 || (arguments.len() == 2 && arguments.get(1) == Some(FULL_MODE));
    if !readiness && !full {
        syscall::exit(57);
    }
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
    if readiness {
        if session.disconnect(7).is_err() {
            syscall::exit(10);
        }
        syscall::exit(0);
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

    if !directory_contains_fixture(session, root, shared_memory, 10, true) {
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

    if session.detach_shared_buffer(23, BUFFER_ID).is_err() || session.disconnect(24).is_err() {
        syscall::exit(22);
    }

    let writable_session = match filesystem::connect_writable_service(SERVICE_HANDLE, 25) {
        Ok(session) => session,
        Err(_) => syscall::exit(23),
    };
    if !writable_session.is_writable() {
        syscall::exit(24);
    }

    if writable_session
        .attach_shared_buffer(26, BUFFER_ID, shared_memory, BUFFER_BYTES)
        .is_err()
    {
        syscall::exit(25);
    }
    let writable_root = Node::root(writable_session);
    if !recover_probe_artifacts(writable_session, writable_root, shared_memory) {
        syscall::exit(54);
    }

    if ipc::shared_memory_write(shared_memory, INITIAL_OFFSET, INITIAL_BYTES).ok()
        != Some(INITIAL_BYTES.len())
        || ipc::shared_memory_write(shared_memory, APPEND_OFFSET, APPEND_BYTES).ok()
            != Some(APPEND_BYTES.len())
        || ipc::shared_memory_write(shared_memory, RENAME_OFFSET, RENAMED_FILE).ok()
            != Some(RENAMED_FILE.len())
    {
        syscall::exit(26);
    }

    let directory_a =
        match writable_session.create_directory(44, writable_root, WRITABLE_DIRECTORY_A) {
            Ok(node) => node,
            Err(_) => syscall::exit(27),
        };
    let directory_b =
        match writable_session.create_directory(45, writable_root, WRITABLE_DIRECTORY_B) {
            Ok(node) => node,
            Err(_) => syscall::exit(28),
        };
    let file = match writable_session.create_file(46, directory_a, WRITABLE_FILE, true, false) {
        Ok(node) => node,
        Err(_) => syscall::exit(29),
    };

    let initial_bulk = protocol::BulkBuffer {
        buffer_id: BUFFER_ID,
        offset: INITIAL_OFFSET as u64,
        length: INITIAL_BYTES.len() as u64,
    };
    if writable_session
        .write_from_shared_buffer(47, file, 0, initial_bulk, false)
        .ok()
        != Some(INITIAL_BYTES.len())
    {
        syscall::exit(30);
    }
    let append_bulk = protocol::BulkBuffer {
        buffer_id: BUFFER_ID,
        offset: APPEND_OFFSET as u64,
        length: APPEND_BYTES.len() as u64,
    };
    if writable_session
        .write_from_shared_buffer(48, file, 0, append_bulk, true)
        .ok()
        != Some(APPEND_BYTES.len())
    {
        syscall::exit(31);
    }

    let attributes = match writable_session.attributes(49, file) {
        Ok(attributes) => attributes,
        Err(_) => syscall::exit(32),
    };
    if attributes.node_id != file.id()
        || attributes.kind != protocol::node_kind::FILE
        || attributes.size != COMPLETE_BYTES.len() as u64
    {
        syscall::exit(33);
    }
    let readback_bulk = protocol::BulkBuffer {
        buffer_id: BUFFER_ID,
        offset: READBACK_OFFSET as u64,
        length: COMPLETE_BYTES.len() as u64,
    };
    if writable_session
        .read_to_shared_buffer(50, file, 0, readback_bulk)
        .ok()
        != Some(COMPLETE_BYTES.len())
    {
        syscall::exit(34);
    }
    let mut complete_readback = [0_u8; COMPLETE_BYTES.len()];
    if ipc::shared_memory_read(shared_memory, READBACK_OFFSET, &mut complete_readback).ok()
        != Some(complete_readback.len())
        || complete_readback != COMPLETE_BYTES
    {
        syscall::exit(35);
    }

    if writable_session
        .truncate(51, file, TRUNCATED_BYTES.len() as u64)
        .is_err()
    {
        syscall::exit(36);
    }
    let attributes = match writable_session.attributes(52, file) {
        Ok(attributes) => attributes,
        Err(_) => syscall::exit(37),
    };
    if attributes.node_id != file.id()
        || attributes.kind != protocol::node_kind::FILE
        || attributes.size != TRUNCATED_BYTES.len() as u64
    {
        syscall::exit(38);
    }
    if writable_session
        .read_to_shared_buffer(53, file, 0, readback_bulk)
        .ok()
        != Some(TRUNCATED_BYTES.len())
    {
        syscall::exit(39);
    }
    let mut truncated_readback = [0_u8; TRUNCATED_BYTES.len()];
    if ipc::shared_memory_read(shared_memory, READBACK_OFFSET, &mut truncated_readback).ok()
        != Some(truncated_readback.len())
        || truncated_readback != TRUNCATED_BYTES
    {
        syscall::exit(40);
    }

    let rename_bulk = protocol::BulkBuffer {
        buffer_id: BUFFER_ID,
        offset: RENAME_OFFSET as u64,
        length: RENAMED_FILE.len() as u64,
    };
    if writable_session
        .rename(54, directory_a, WRITABLE_FILE, directory_b, rename_bulk)
        .is_err()
    {
        syscall::exit(41);
    }
    if writable_session.lookup_node(55, directory_a, WRITABLE_FILE) != Err(Error::NotFound) {
        syscall::exit(42);
    }
    let renamed = match writable_session.lookup_node(56, directory_b, RENAMED_FILE) {
        Ok(node) => node,
        Err(_) => syscall::exit(43),
    };
    if renamed.id() != file.id() {
        syscall::exit(44);
    }
    if writable_session
        .unlink(57, directory_b, RENAMED_FILE)
        .is_err()
    {
        syscall::exit(45);
    }
    if writable_session.lookup_node(58, directory_b, RENAMED_FILE) != Err(Error::NotFound) {
        syscall::exit(46);
    }

    if writable_session
        .rmdir(59, writable_root, WRITABLE_DIRECTORY_A)
        .is_err()
    {
        syscall::exit(47);
    }
    if writable_session
        .rmdir(60, writable_root, WRITABLE_DIRECTORY_B)
        .is_err()
    {
        syscall::exit(48);
    }
    if writable_session.sync(61).is_err() {
        syscall::exit(49);
    }
    if !directory_contains_fixture(writable_session, writable_root, shared_memory, 62, false) {
        syscall::exit(50);
    }
    if writable_session
        .detach_shared_buffer(64, BUFFER_ID)
        .is_err()
    {
        syscall::exit(51);
    }
    if writable_session.disconnect(65).is_err() {
        syscall::exit(52);
    }
    if ipc::close(shared_memory).is_err() {
        syscall::exit(53);
    }

    syscall::exit(0)
}

#[derive(Clone, Copy)]
struct RecoveredDirectory {
    node: Node,
    has_file: bool,
}

fn recover_probe_artifacts(session: filesystem::Session, root: Node, shared_memory: u64) -> bool {
    let directory_a = match inspect_reserved_directory(
        session,
        root,
        WRITABLE_DIRECTORY_A,
        WRITABLE_FILE,
        shared_memory,
        27,
    ) {
        Ok(directory) => directory,
        Err(()) => return false,
    };
    let directory_b = match inspect_reserved_directory(
        session,
        root,
        WRITABLE_DIRECTORY_B,
        RENAMED_FILE,
        shared_memory,
        33,
    ) {
        Ok(directory) => directory,
        Err(()) => return false,
    };

    if let Some(directory) = directory_a
        && directory.has_file
        && session.unlink(39, directory.node, WRITABLE_FILE).is_err()
    {
        return false;
    }
    if let Some(directory) = directory_b
        && directory.has_file
        && session.unlink(40, directory.node, RENAMED_FILE).is_err()
    {
        return false;
    }
    if directory_a.is_some() && session.rmdir(41, root, WRITABLE_DIRECTORY_A).is_err() {
        return false;
    }
    if directory_b.is_some() && session.rmdir(42, root, WRITABLE_DIRECTORY_B).is_err() {
        return false;
    }

    (directory_a.is_none() && directory_b.is_none()) || session.sync(43).is_ok()
}

fn inspect_reserved_directory(
    session: filesystem::Session,
    root: Node,
    directory_name: &[u8],
    expected_file_name: &[u8],
    shared_memory: u64,
    first_request_id: u64,
) -> Result<Option<RecoveredDirectory>, ()> {
    let directory = match session.lookup_node(first_request_id, root, directory_name) {
        Ok(directory) => directory,
        Err(Error::NotFound) => return Ok(None),
        Err(_) => return Err(()),
    };
    let attributes = session
        .attributes(first_request_id + 1, directory)
        .map_err(|_| ())?;
    if attributes.node_id != directory.id() || attributes.kind != protocol::node_kind::DIRECTORY {
        return Err(());
    }

    let entry_bytes = size_of::<protocol::DirectoryEntry>();
    let bulk = protocol::BulkBuffer {
        buffer_id: BUFFER_ID,
        offset: DIRECTORY_OFFSET as u64,
        length: (RECOVERY_ENTRY_CAPACITY * entry_bytes) as u64,
    };
    let batch = session
        .read_directory_to_shared_buffer(first_request_id + 2, directory, 0, bulk)
        .map_err(|_| ())?;
    if !batch.end || batch.count > 1 {
        return Err(());
    }
    if batch.count == 0 {
        return Ok(Some(RecoveredDirectory {
            node: directory,
            has_file: false,
        }));
    }

    let record = read_directory_record(shared_memory, 0).ok_or(())?;
    let name = entry_name(&record).ok_or(())?;
    if record.node_id == protocol::INVALID_ID
        || record.node_id == directory.id()
        || record.next_cookie == 0
        || record.kind != protocol::node_kind::FILE
        || name != expected_file_name
    {
        return Err(());
    }
    let file = session
        .lookup_node(first_request_id + 3, directory, expected_file_name)
        .map_err(|_| ())?;
    if file.id() != record.node_id
        || !valid_recovery_file(session, file, shared_memory, first_request_id + 4)
    {
        return Err(());
    }

    Ok(Some(RecoveredDirectory {
        node: directory,
        has_file: true,
    }))
}

fn valid_recovery_file(
    session: filesystem::Session,
    file: Node,
    shared_memory: u64,
    first_request_id: u64,
) -> bool {
    let Ok(attributes) = session.attributes(first_request_id, file) else {
        return false;
    };
    let Ok(size) = usize::try_from(attributes.size) else {
        return false;
    };
    let expected = if size == 0 {
        b"".as_slice()
    } else if size == INITIAL_BYTES.len() {
        INITIAL_BYTES
    } else if size == COMPLETE_BYTES.len() {
        COMPLETE_BYTES
    } else if size == TRUNCATED_BYTES.len() {
        TRUNCATED_BYTES
    } else {
        return false;
    };
    if attributes.node_id != file.id() || attributes.kind != protocol::node_kind::FILE {
        return false;
    }
    if expected.is_empty() {
        return true;
    }

    let bulk = protocol::BulkBuffer {
        buffer_id: BUFFER_ID,
        offset: RECOVERY_READ_OFFSET as u64,
        length: COMPLETE_BYTES.len() as u64,
    };
    if session
        .read_to_shared_buffer(first_request_id + 1, file, 0, bulk)
        .ok()
        != Some(expected.len())
    {
        return false;
    }
    let mut contents = [0_u8; COMPLETE_BYTES.len()];
    ipc::shared_memory_read(
        shared_memory,
        RECOVERY_READ_OFFSET,
        &mut contents[..expected.len()],
    )
    .ok()
        == Some(expected.len())
        && &contents[..expected.len()] == expected
}

fn directory_contains_fixture(
    session: filesystem::Session,
    root: Node,
    shared_memory: u64,
    request_id: u64,
    allow_probe_artifacts: bool,
) -> bool {
    let entry_bytes = size_of::<protocol::DirectoryEntry>();
    let bulk = protocol::BulkBuffer {
        buffer_id: BUFFER_ID,
        offset: DIRECTORY_OFFSET as u64,
        length: (ROOT_ENTRY_CAPACITY * entry_bytes) as u64,
    };
    let mut cookie = 0;
    let mut found_docs = false;
    let mut found_welcome = false;
    let mut found_directory_a = false;
    let mut found_directory_b = false;
    let mut found_public_vfs_probe = false;

    for _ in 0..128 {
        let Ok(batch) = session.read_directory_to_shared_buffer(request_id, root, cookie, bulk)
        else {
            return false;
        };
        if batch.count > ROOT_ENTRY_CAPACITY || batch.count == 0 && !batch.end {
            return false;
        }

        for index in 0..batch.count {
            let Some(record) = read_directory_record(shared_memory, index) else {
                return false;
            };
            if record.node_id == protocol::INVALID_ID || record.next_cookie <= cookie {
                return false;
            }
            cookie = record.next_cookie;

            let Some(name) = entry_name(&record) else {
                return false;
            };
            if name == b"docs" {
                if found_docs || record.kind != protocol::node_kind::DIRECTORY {
                    return false;
                }
                found_docs = true;
            } else if name == b"welcome.txt" {
                if found_welcome || record.kind != protocol::node_kind::FILE {
                    return false;
                }
                found_welcome = true;
            } else if name == WRITABLE_DIRECTORY_A {
                if !allow_probe_artifacts
                    || found_directory_a
                    || record.kind != protocol::node_kind::DIRECTORY
                {
                    return false;
                }
                found_directory_a = true;
            } else if name == WRITABLE_DIRECTORY_B {
                if !allow_probe_artifacts
                    || found_directory_b
                    || record.kind != protocol::node_kind::DIRECTORY
                {
                    return false;
                }
                found_directory_b = true;
            } else if name == PUBLIC_VFS_PROBE_FILE {
                if found_public_vfs_probe || record.kind != protocol::node_kind::FILE {
                    return false;
                }
                found_public_vfs_probe = true;
            } else if !matches!(
                record.kind,
                protocol::node_kind::FILE
                    | protocol::node_kind::DIRECTORY
                    | protocol::node_kind::SYMBOLIC_LINK
            ) {
                return false;
            }
        }

        if batch.end {
            return found_docs && found_welcome;
        }
    }
    false
}

fn read_directory_record(shared_memory: u64, index: usize) -> Option<protocol::DirectoryEntry> {
    let bytes = size_of::<protocol::DirectoryEntry>();
    let offset = index.checked_mul(bytes)?.checked_add(DIRECTORY_OFFSET)?;
    let mut record = protocol::DirectoryEntry::EMPTY;
    let record_bytes = unsafe {
        core::slice::from_raw_parts_mut(
            (&mut record as *mut protocol::DirectoryEntry).cast::<u8>(),
            bytes,
        )
    };
    (ipc::shared_memory_read(shared_memory, offset, record_bytes).ok() == Some(bytes))
        .then_some(record)
}

fn entry_name(entry: &protocol::DirectoryEntry) -> Option<&[u8]> {
    let length = usize::from(entry.name_length);
    (length != 0
        && length <= protocol::MAX_NAME_BYTES
        && entry.reserved == 0
        && !entry.name[..length].contains(&b'/')
        && !entry.name[..length].contains(&0)
        && entry.name[length..].iter().all(|byte| *byte == 0))
    .then_some(&entry.name[..length])
}
