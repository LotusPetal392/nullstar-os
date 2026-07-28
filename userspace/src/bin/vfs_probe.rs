#![no_std]
#![no_main]

use core::{mem::size_of, slice};

use nullfs_format::BLOCK_SIZE;
use userspace::{
    abi::{errno, file},
    ipc::{self, ObjectKind, Rights, Transfer},
    platform, syscall,
    vfs::protocol,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SERVICE_HANDLE: u64 = 1;
const NULLFS_MOUNT: &[u8] = b"/Volumes/NULLSTAR_DATA";
const NULLFS_DOCS: &[u8] = b"/Volumes/NULLSTAR_DATA/docs";
const NULLFS_WELCOME: &[u8] = b"/Volumes/NULLSTAR_DATA/welcome.txt";
const NULLFS_README: &[u8] = b"/Volumes/NULLSTAR_DATA/docs/readme.txt";
const NULLFS_MISSING: &[u8] = b"/Volumes/NULLSTAR_DATA/missing";
const WELCOME: &[u8] = b"NullStar persistent storage service fixture.\n";
const README: &[u8] = b"This volume is prepared for read-only Phase 4 service integration.\n";

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
        b"/Volumes/NULLSTAR_DATA",
        protocol::route::NULLSTAR_DATA,
        protocol::backend::NULLFS,
        22,
    ),
    (
        b"/Volumes/NULLSTAR_DATA/docs/readme.txt",
        protocol::route::NULLSTAR_DATA,
        protocol::backend::NULLFS,
        22,
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
    let mut invalid_path_entries = [platform::DirectoryEntry::EMPTY; 1];
    if platform::read_directory(b"/invalid", 0, &mut invalid_path_entries).err()
        != Some(platform::Errno::NO_ENTRY)
    {
        syscall::exit(14);
    }
    if platform::read_directory(b"invalid", 0, &mut invalid_path_entries).err()
        != Some(platform::Errno::NO_ENTRY)
    {
        syscall::exit(15);
    }
    let Ok(ls_process) = syscall::spawn_command(
        b"/ls /invalid",
        syscall::SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
    ) else {
        syscall::exit(16);
    };
    if syscall::wait_child(ls_process)
        .ok()
        .map(|status| status.raw())
        != Some(1)
    {
        syscall::exit(17);
    }
    if platform::stat(b"/hello.txt").ok().map(|stat| stat.kind) != Some(file::KIND_FILE) {
        syscall::exit(18);
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
    let Ok(boot_file) = syscall::open(b"/hello.txt", syscall::OpenFlags::READ) else {
        syscall::exit(6);
    };
    let mut first_byte = [0_u8; 1];
    if syscall::read(boot_file, &mut first_byte).ok() != Some(1)
        || syscall::close(boot_file).is_err()
    {
        syscall::exit(6);
    }
    let Ok(tmp_file) = syscall::open(
        b"/tmp/vfs-open-probe",
        syscall::OpenFlags::READ
            | syscall::OpenFlags::WRITE
            | syscall::OpenFlags::CREATE
            | syscall::OpenFlags::TRUNCATE,
    ) else {
        syscall::exit(6);
    };
    let Ok(duplicate) = platform::dup(tmp_file) else {
        syscall::exit(6);
    };
    let mut unlinked_bytes = [0_u8; 11];
    if syscall::write_all(tmp_file, b"routed open").is_err()
        || platform::stat(b"/tmp/vfs-open-probe")
            .ok()
            .map(|stat| stat.kind)
            != Some(file::KIND_FILE)
        || platform::unlink(b"/tmp/vfs-open-probe").is_err()
        || platform::stat(b"/tmp/vfs-open-probe").err() != Some(platform::Errno::NO_ENTRY)
        || syscall::close(tmp_file).is_err()
        || syscall::yield_now().is_err()
        || syscall::yield_now().is_err()
        || syscall::seek(duplicate, syscall::SeekFrom::Start(0)).ok() != Some(0)
        || syscall::read(duplicate, &mut unlinked_bytes).ok() != Some(unlinked_bytes.len())
        || unlinked_bytes != *b"routed open"
        || syscall::close(duplicate).is_err()
    {
        syscall::exit(6);
    }
    for _ in 0..33 {
        let Some(file) = open_with_retry(
            b"/tmp/vfs-close-reuse-probe",
            syscall::OpenFlags::READ
                | syscall::OpenFlags::WRITE
                | syscall::OpenFlags::CREATE
                | syscall::OpenFlags::TRUNCATE,
        ) else {
            syscall::exit(6);
        };
        if platform::unlink(b"/tmp/vfs-close-reuse-probe").is_err()
            || syscall::close(file).is_err()
            || syscall::yield_now().is_err()
        {
            syscall::exit(6);
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
        syscall::exit(7);
    }
    let mut cwd = [0_u8; 64];
    if platform::chdir(b"/System/var").is_err() {
        syscall::exit(8);
    }
    if platform::getcwd(&mut cwd).ok() != Some(b"/System/var".as_slice()) {
        syscall::exit(9);
    }
    if platform::chdir(b"/").is_err() {
        syscall::exit(10);
    }
    if platform::chdir(b"/Volumes").is_err() {
        syscall::exit(11);
    }
    if platform::getcwd(&mut cwd).ok() != Some(b"/Volumes".as_slice()) {
        syscall::exit(12);
    }
    if platform::chdir(b"/").is_err() {
        syscall::exit(13);
    }

    probe_mounted_nullfs();
    syscall::exit(0)
}

fn probe_mounted_nullfs() {
    let mut entries = [platform::DirectoryEntry::EMPTY; 1];
    if platform::read_directory(b"/Volumes", 0, &mut entries).ok() != Some(1)
        || entries[0].name() != b"NULLSTAR_DATA"
        || entries[0].kind != file::KIND_DIRECTORY
    {
        syscall::exit(19);
    }

    if !stat_matches(NULLFS_MOUNT, file::KIND_DIRECTORY, 0) {
        syscall::exit(20);
    }
    if !stat_matches(NULLFS_DOCS, file::KIND_DIRECTORY, BLOCK_SIZE as u64) {
        syscall::exit(21);
    }
    if !stat_matches(NULLFS_WELCOME, file::KIND_FILE, WELCOME.len() as u64) {
        syscall::exit(22);
    }
    if !platform_failed_with(platform::stat(NULLFS_MISSING), errno::NO_ENTRY) {
        syscall::exit(23);
    }

    let welcome = match syscall::open(NULLFS_WELCOME, syscall::OpenFlags::READ) {
        Ok(descriptor) => descriptor,
        Err(_) => syscall::exit(24),
    };
    let mut welcome_bytes = [0_u8; WELCOME.len()];
    if syscall::read(welcome, &mut welcome_bytes).ok() != Some(WELCOME.len())
        || welcome_bytes != WELCOME
    {
        syscall::exit(25);
    }
    if platform::fstat(welcome).ok()
        != Some(file::Stat {
            kind: file::KIND_FILE,
            size: WELCOME.len() as u64,
            flags: file::FLAG_READ_ONLY,
        })
    {
        syscall::exit(26);
    }
    const WELCOME_TAIL: &[u8] = b"fixture.\n";
    if syscall::seek(
        welcome,
        syscall::SeekFrom::End(-(WELCOME_TAIL.len() as i64)),
    )
    .ok()
        != Some((WELCOME.len() - WELCOME_TAIL.len()) as u64)
    {
        syscall::exit(27);
    }
    let mut tail = [0_u8; WELCOME_TAIL.len()];
    if syscall::read(welcome, &mut tail).ok() != Some(tail.len()) || tail != WELCOME_TAIL {
        syscall::exit(28);
    }

    let readme = match syscall::open(NULLFS_README, syscall::OpenFlags::READ) {
        Ok(descriptor) => descriptor,
        Err(_) => syscall::exit(30),
    };
    let mut readme_bytes = [0_u8; README.len()];
    if syscall::read(readme, &mut readme_bytes).ok() != Some(README.len()) || readme_bytes != README
    {
        syscall::exit(31);
    }

    entries[0] = platform::DirectoryEntry::EMPTY;
    if platform::read_directory(NULLFS_MOUNT, 0, &mut entries).ok() != Some(1)
        || !directory_entry_matches(&entries[0], b"docs", file::KIND_DIRECTORY)
    {
        syscall::exit(33);
    }
    entries[0] = platform::DirectoryEntry::EMPTY;
    if platform::read_directory(NULLFS_MOUNT, 1, &mut entries).ok() != Some(1)
        || !directory_entry_matches(&entries[0], b"welcome.txt", file::KIND_FILE)
    {
        syscall::exit(34);
    }
    entries[0] = platform::DirectoryEntry::EMPTY;
    if platform::read_directory(NULLFS_MOUNT, 2, &mut entries).ok() != Some(0) {
        syscall::exit(35);
    }
    entries[0] = platform::DirectoryEntry::EMPTY;
    if platform::read_directory(NULLFS_DOCS, 0, &mut entries).ok() != Some(1)
        || !directory_entry_matches(&entries[0], b"readme.txt", file::KIND_FILE)
    {
        syscall::exit(36);
    }
    entries[0] = platform::DirectoryEntry::EMPTY;
    if platform::read_directory(NULLFS_DOCS, 1, &mut entries).ok() != Some(0) {
        syscall::exit(37);
    }

    let mut cwd = [0_u8; 64];
    if platform::chdir(NULLFS_MOUNT).is_err() {
        syscall::exit(38);
    }
    if platform::getcwd(&mut cwd).ok() != Some(NULLFS_MOUNT) {
        syscall::exit(39);
    }
    if platform::chdir(b"docs").is_err() {
        syscall::exit(40);
    }
    if platform::getcwd(&mut cwd).ok() != Some(NULLFS_DOCS) {
        syscall::exit(41);
    }
    let relative_readme = match syscall::open(b"readme.txt", syscall::OpenFlags::READ) {
        Ok(descriptor) => descriptor,
        Err(_) => syscall::exit(42),
    };
    readme_bytes.fill(0);
    if syscall::read(relative_readme, &mut readme_bytes).ok() != Some(README.len())
        || readme_bytes != README
    {
        syscall::exit(43);
    }
    if platform::chdir(b"/").is_err() || platform::getcwd(&mut cwd).ok() != Some(b"/".as_slice()) {
        syscall::exit(45);
    }

    if !syscall_failed_with(
        syscall::open(NULLFS_MOUNT, syscall::OpenFlags::READ),
        errno::IS_DIRECTORY,
    ) {
        syscall::exit(46);
    }
    if !syscall_failed_with(
        syscall::open(NULLFS_WELCOME, syscall::OpenFlags::WRITE),
        errno::READ_ONLY,
    ) {
        syscall::exit(47);
    }
    if !syscall_failed_with(
        syscall::open(
            b"/Volumes/NULLSTAR_DATA/denied",
            syscall::OpenFlags::WRITE | syscall::OpenFlags::CREATE,
        ),
        errno::READ_ONLY,
    ) {
        syscall::exit(48);
    }
    if !syscall_failed_with(
        syscall::open(
            NULLFS_WELCOME,
            syscall::OpenFlags::WRITE | syscall::OpenFlags::TRUNCATE,
        ),
        errno::READ_ONLY,
    ) {
        syscall::exit(49);
    }
    if !syscall_failed_with(
        syscall::open(
            NULLFS_WELCOME,
            syscall::OpenFlags::WRITE | syscall::OpenFlags::APPEND,
        ),
        errno::READ_ONLY,
    ) {
        syscall::exit(50);
    }
    if !platform_failed_with(platform::unlink(NULLFS_WELCOME), errno::READ_ONLY) {
        syscall::exit(51);
    }
    if syscall::close(relative_readme).is_err()
        || syscall::close(readme).is_err()
        || syscall::close(welcome).is_err()
    {
        syscall::exit(52);
    }
}

fn stat_matches(path: &[u8], kind: u64, size: u64) -> bool {
    platform::stat(path).ok()
        == Some(file::Stat {
            kind,
            size,
            flags: file::FLAG_READ_ONLY,
        })
}

fn directory_entry_matches(entry: &platform::DirectoryEntry, name: &[u8], kind: u64) -> bool {
    entry.name() == name && entry.kind == kind && entry.flags == file::FLAG_READ_ONLY
}

fn syscall_failed_with<T>(result: syscall::Result<T>, expected: i64) -> bool {
    result
        .err()
        .is_some_and(|error| i64::from(error.code()) == -expected)
}

fn platform_failed_with<T>(result: platform::Result<T>, expected: i64) -> bool {
    result
        .err()
        .is_some_and(|error| i64::from(error.code()) == -expected)
}

fn open_with_retry(path: &[u8], flags: syscall::OpenFlags) -> Option<syscall::FileDescriptor> {
    for _ in 0..8 {
        match syscall::open(path, flags) {
            Ok(descriptor) => return Some(descriptor),
            Err(error) if error == syscall::Errno::TRY_AGAIN => {
                syscall::yield_now().ok()?;
            }
            Err(_) => return None,
        }
    }
    None
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
