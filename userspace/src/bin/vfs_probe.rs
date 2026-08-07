#![no_std]
#![no_main]

use core::{mem::size_of, slice};

use nullfs_format::BLOCK_SIZE;
use userspace::{
    abi::{errno, file},
    args::Args,
    ipc::{self, ObjectKind, Rights, Transfer},
    nullfs_primary_volume, platform, syscall,
    vfs::protocol,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SERVICE_HANDLE: u64 = 1;
const RESTART_CONTROL_HANDLE: u64 = 2;
const READINESS_MODE: &[u8] = b"readiness";
const FULL_MODE: &[u8] = b"full";
const BOOTSTRAP_MODE: &[u8] = b"bootstrap";
const NULLFS_RESTART_MODE: &[u8] = b"nullfs-restart";
const NULLFS_RESTART_READY: &[u8] =
    b"nullfs-restart: live descriptor and persistent mutation ready";

const NULLFS_RESTART_REPLACEMENT: &[u8] = b"nullfs-restart: replacement registered";
const NULLFS_MOUNT: &[u8] = nullfs_primary_volume::MOUNT_PATH.as_bytes();
const NULLFS_DOCS: &[u8] = b"/Volumes/NullStar/docs";
const NULLFS_WELCOME: &[u8] = b"/Volumes/NullStar/welcome.txt";
const NULLFS_README: &[u8] = b"/Volumes/NullStar/docs/readme.txt";
const NULLFS_MISSING: &[u8] = b"/Volumes/NullStar/missing";
const NULLFS_PUBLIC_PROBE: &[u8] = b"/Applications/nullstar-vfs-probe-v1.bin";
const NULLFS_PUBLIC_PROBE_RAW: &[u8] = b"/Volumes/NullStar/Applications/nullstar-vfs-probe-v1.bin";
const WELCOME: &[u8] = b"NullStar persistent storage service fixture.\n";
const README: &[u8] = b"This volume is a deterministic NullFS integration fixture.\n";
const INITIAL_BYTES: &[u8] = b"NullStar public VFS";
const APPEND_PREFIX: &[u8] = b" ";
const APPEND_SUFFIX: &[u8] = b"append";
const PREFIXED_BYTES: &[u8] = b"NullStar public VFS ";
const COMBINED_BYTES: &[u8] = b"NullStar public VFS append";
const TRUNCATED_BYTES: &[u8] = b"short";
const OPEN_UNLINKED_BYTES: &[u8] = b"alive";
const NULLFS_EXEC_TARGET: &[u8] = b"/Applications/ExecProbe/bin/exec-target";
const NULLFS_EXEC_SPAWN_COMMAND: &[u8] = b"/Applications/ExecProbe/bin/exec-target spawn";
const NULLFS_EXEC_MISSING_COMMAND: &[u8] = b"/Applications/ExecProbe/bin/missing-target";
const NULLFS_EXEC_NOT_DIRECTORY_COMMAND: &[u8] = b"/Applications/ExecProbe/bin/exec-target/child";
const NULLFS_EXEC_MALFORMED_COMMAND: &[u8] = b"/Applications/ExecProbe/bin/malformed-target";
const NULLFS_EXEC_FORK_PREFIX: &[u8] = b"/Applications/ExecProbe/bin/exec-target fork-exec ";
const NULLFS_EXEC_PRESERVED_PATH: &[u8] = b"/tmp/nullfs-exec-preserved";
const NULLFS_EXEC_CLOSED_PATH: &[u8] = b"/tmp/nullfs-exec-closed";
const NULLFS_EXEC_SPAWN_STATUS: u64 = 41;
const NULLFS_EXEC_FORK_STATUS: u64 = 42;

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
        protocol::backend::NULLFS,
        13,
    ),
    (
        b"/Volumes/NullStar",
        protocol::route::NULLSTAR_VOLUME,
        protocol::backend::NULLFS,
        17,
    ),
    (
        b"/Volumes/NullStar/docs/readme.txt",
        protocol::route::NULLSTAR_VOLUME,
        protocol::backend::NULLFS,
        17,
    ),
    (
        b"/Volumes/Disk",
        protocol::route::VOLUMES,
        protocol::backend::NAMESPACE,
        8,
    ),
];

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if arguments.len() == 2 && arguments.get(1) == Some(NULLFS_RESTART_MODE) {
        probe_nullfs_restart();
    }
    let readiness = arguments.len() == 2 && arguments.get(1) == Some(READINESS_MODE);
    let full =
        arguments.len() == 1 || (arguments.len() == 2 && arguments.get(1) == Some(FULL_MODE));
    let bootstrap = arguments.len() == 2 && arguments.get(1) == Some(BOOTSTRAP_MODE);
    if !readiness && !full && !bootstrap {
        syscall::exit(57);
    }
    if !matches!(
        ipc::wait_for_handle(SERVICE_HANDLE),
        Ok(info) if info.kind == ObjectKind::Endpoint && info.rights == Rights::SEND
    ) {
        syscall::exit(1);
    }
    if readiness {
        probe_readiness();
        syscall::exit(0);
    }
    if bootstrap {
        probe_bootstrap();
        syscall::exit(0);
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
            || !reply_binding_matches(&reply, route_id)
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
    let executable_result = probe_nullfs_executable();
    if executable_result != 0 {
        syscall::exit(130 + executable_result);
    }
    syscall::exit(0)
}

fn probe_bootstrap() {
    let reply = query(b"/hello.txt", 1).unwrap_or_else(|| syscall::exit(110));
    if reply.status != protocol::status::OK
        || reply.route_id != protocol::route::ROOT
        || reply.backend != protocol::backend::BOOT_FILESYSTEM
        || reply.prefix_length != 1
        || !reply_binding_matches(&reply, protocol::route::ROOT)
        || platform::stat(b"/hello.txt").ok().map(|stat| stat.kind) != Some(file::KIND_FILE)
    {
        syscall::exit(111);
    }
    let descriptor = syscall::open(b"/hello.txt", syscall::OpenFlags::READ)
        .unwrap_or_else(|_| syscall::exit(112));
    let mut byte = [0_u8; 1];
    if syscall::read(descriptor, &mut byte).ok() != Some(1) || syscall::close(descriptor).is_err() {
        syscall::exit(113);
    }
}

fn probe_readiness() {
    const READINESS_CASES: &[(&[u8], u32, u16, u16)] = &[
        (
            b"/",
            protocol::route::ROOT,
            protocol::backend::BOOT_FILESYSTEM,
            1,
        ),
        (b"/tmp", protocol::route::TMP, protocol::backend::TMPFS, 4),
        (
            b"/Volumes/NullStar",
            protocol::route::NULLSTAR_VOLUME,
            protocol::backend::NULLFS,
            17,
        ),
    ];
    for (index, &(path, route_id, backend, prefix_length)) in READINESS_CASES.iter().enumerate() {
        let Some(reply) = query(path, index as u32 + 1) else {
            syscall::exit(100);
        };
        if reply.status != protocol::status::OK
            || reply.route_id != route_id
            || reply.backend != backend
            || reply.prefix_length != prefix_length
            || !reply_binding_matches(&reply, route_id)
        {
            syscall::exit(101);
        }
    }
    for path in [b"/".as_slice(), b"/tmp", NULLFS_MOUNT] {
        if platform::stat(path).ok().map(|stat| stat.kind) != Some(file::KIND_DIRECTORY) {
            syscall::exit(102);
        }
    }
    let descriptor = syscall::open(b"/hello.txt", syscall::OpenFlags::READ)
        .unwrap_or_else(|_| syscall::exit(103));
    let mut byte = [0_u8; 1];
    if syscall::read(descriptor, &mut byte).ok() != Some(1) || syscall::close(descriptor).is_err() {
        syscall::exit(104);
    }
}

fn probe_mounted_nullfs() {
    let mut entries = [platform::DirectoryEntry::EMPTY; 1];
    if platform::read_directory(b"/Volumes", 0, &mut entries).ok() != Some(1)
        || entries[0].name() != nullfs_primary_volume::DISPLAY_NAME.as_bytes()
        || entries[0].kind != file::KIND_DIRECTORY
    {
        syscall::exit(19);
    }

    if !stat_matches(NULLFS_MOUNT, file::KIND_DIRECTORY, 0)
        || !stat_matches(b"/Applications", file::KIND_DIRECTORY, BLOCK_SIZE as u64)
    {
        syscall::exit(20);
    }
    if !directory_contains(NULLFS_MOUNT, &[b"System", b"Applications", b"Users"]) {
        syscall::exit(58);
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
            flags: 0,
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

    if !recover_public_probe_artifact() {
        syscall::exit(58);
    }
    if !nullfs_root_is_valid() {
        syscall::exit(33);
    }
    if !nullfs_docs_is_valid() {
        syscall::exit(36);
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
    if platform::chdir(b"/Applications").is_err()
        || platform::getcwd(&mut cwd).ok() != Some(b"/Applications".as_slice())
        || platform::chdir(b"/").is_err()
        || platform::getcwd(&mut cwd).ok() != Some(b"/".as_slice())
    {
        syscall::exit(45);
    }

    if !syscall_failed_with(
        syscall::open(NULLFS_MOUNT, syscall::OpenFlags::READ),
        errno::IS_DIRECTORY,
    ) {
        syscall::exit(46);
    }
    if syscall::close(relative_readme).is_err()
        || syscall::close(readme).is_err()
        || syscall::close(welcome).is_err()
    {
        syscall::exit(52);
    }

    probe_public_nullfs_mutation();
    if !nullfs_root_is_valid() {
        syscall::exit(89);
    }
    if !fixture_files_are_exact() {
        syscall::exit(59);
    }

    for _ in 0..4 {
        for _ in 0..2 {
            if syscall::yield_now().is_err() {
                syscall::exit(53);
            }
        }
        let descriptor = match open_with_retry(NULLFS_WELCOME, syscall::OpenFlags::READ) {
            Some(descriptor) => descriptor,
            None => syscall::exit(54),
        };
        welcome_bytes.fill(0);
        if syscall::read(descriptor, &mut welcome_bytes).ok() != Some(WELCOME.len())
            || welcome_bytes != WELCOME
        {
            syscall::exit(55);
        }
        if syscall::close(descriptor).is_err() {
            syscall::exit(56);
        }
    }
}

fn probe_public_nullfs_mutation() {
    let created = open_with_retry(
        NULLFS_PUBLIC_PROBE,
        syscall::OpenFlags::READ | syscall::OpenFlags::WRITE | syscall::OpenFlags::CREATE,
    )
    .unwrap_or_else(|| syscall::exit(71));
    if !descriptor_stat_matches(created, 0) {
        syscall::exit(72);
    }
    if !write_all_with_retry(created, INITIAL_BYTES) {
        syscall::exit(73);
    }
    if !descriptor_stat_matches(created, INITIAL_BYTES.len() as u64)
        || !stat_matches(
            NULLFS_PUBLIC_PROBE_RAW,
            file::KIND_FILE,
            INITIAL_BYTES.len() as u64,
        )
        || syscall::seek(created, syscall::SeekFrom::Start(0)).ok() != Some(0)
        || !read_matches(created, INITIAL_BYTES)
    {
        syscall::exit(74);
    }
    if syscall::close(created).is_err() {
        syscall::exit(75);
    }

    let observer = open_with_retry(
        NULLFS_PUBLIC_PROBE_RAW,
        syscall::OpenFlags::READ | syscall::OpenFlags::WRITE,
    )
    .unwrap_or_else(|| syscall::exit(76));
    let append = open_with_retry(
        NULLFS_PUBLIC_PROBE,
        syscall::OpenFlags::READ | syscall::OpenFlags::WRITE | syscall::OpenFlags::APPEND,
    )
    .unwrap_or_else(|| syscall::exit(77));
    if syscall::seek(observer, syscall::SeekFrom::End(0)).ok() != Some(INITIAL_BYTES.len() as u64)
        || !write_all_with_retry(observer, APPEND_PREFIX)
        || !descriptor_stat_matches(append, PREFIXED_BYTES.len() as u64)
        || !write_all_with_retry(append, APPEND_SUFFIX)
        || syscall::seek(append, syscall::SeekFrom::Current(0)).ok()
            != Some(COMBINED_BYTES.len() as u64)
        || !descriptor_stat_matches(observer, COMBINED_BYTES.len() as u64)
        || syscall::seek(observer, syscall::SeekFrom::End(0)).ok()
            != Some(COMBINED_BYTES.len() as u64)
        || !stat_matches(
            NULLFS_PUBLIC_PROBE_RAW,
            file::KIND_FILE,
            COMBINED_BYTES.len() as u64,
        )
        || syscall::seek(append, syscall::SeekFrom::Start(0)).ok() != Some(0)
        || !read_matches(append, COMBINED_BYTES)
    {
        syscall::exit(78);
    }
    if syscall::close(append).is_err() {
        syscall::exit(79);
    }

    let truncated = open_with_retry(
        NULLFS_PUBLIC_PROBE,
        syscall::OpenFlags::READ | syscall::OpenFlags::WRITE | syscall::OpenFlags::TRUNCATE,
    )
    .unwrap_or_else(|| syscall::exit(80));
    if !descriptor_stat_matches(truncated, 0)
        || !descriptor_stat_matches(observer, 0)
        || syscall::seek(observer, syscall::SeekFrom::End(0)).ok() != Some(0)
        || !stat_matches(NULLFS_PUBLIC_PROBE, file::KIND_FILE, 0)
    {
        syscall::exit(81);
    }
    if !write_all_with_retry(truncated, TRUNCATED_BYTES)
        || !descriptor_stat_matches(truncated, TRUNCATED_BYTES.len() as u64)
        || !descriptor_stat_matches(observer, TRUNCATED_BYTES.len() as u64)
        || syscall::seek(observer, syscall::SeekFrom::End(0)).ok()
            != Some(TRUNCATED_BYTES.len() as u64)
        || syscall::seek(truncated, syscall::SeekFrom::Start(0)).ok() != Some(0)
        || !read_matches(truncated, TRUNCATED_BYTES)
    {
        syscall::exit(82);
    }

    let surviving = platform::dup(truncated).unwrap_or_else(|_| syscall::exit(83));
    if !unlink_with_retry(NULLFS_PUBLIC_PROBE)
        || !platform_failed_with(platform::stat(NULLFS_PUBLIC_PROBE), errno::NO_ENTRY)
        || !platform_failed_with(platform::stat(NULLFS_PUBLIC_PROBE_RAW), errno::NO_ENTRY)
    {
        syscall::exit(84);
    }
    if syscall::close(truncated).is_err() {
        syscall::exit(85);
    }
    if !descriptor_stat_matches(surviving, TRUNCATED_BYTES.len() as u64)
        || syscall::seek(surviving, syscall::SeekFrom::Start(0)).ok() != Some(0)
        || !read_matches(surviving, TRUNCATED_BYTES)
    {
        syscall::exit(86);
    }
    if syscall::seek(surviving, syscall::SeekFrom::Start(0)).ok() != Some(0)
        || !write_all_with_retry(surviving, OPEN_UNLINKED_BYTES)
        || !descriptor_stat_matches(observer, OPEN_UNLINKED_BYTES.len() as u64)
        || syscall::seek(observer, syscall::SeekFrom::Start(0)).ok() != Some(0)
        || !read_matches(observer, OPEN_UNLINKED_BYTES)
        || syscall::seek(surviving, syscall::SeekFrom::Start(0)).ok() != Some(0)
        || !read_matches(surviving, OPEN_UNLINKED_BYTES)
    {
        syscall::exit(87);
    }
    if syscall::close(surviving).is_err() {
        syscall::exit(88);
    }
    if syscall::seek(observer, syscall::SeekFrom::Start(0)).ok() != Some(0)
        || !read_matches(observer, OPEN_UNLINKED_BYTES)
        || syscall::close(observer).is_err()
    {
        syscall::exit(90);
    }
    for _ in 0..2 {
        syscall::yield_now().unwrap_or_else(|_| syscall::exit(91));
    }
}

fn probe_nullfs_restart() -> ! {
    if !matches!(
        ipc::wait_for_handle(SERVICE_HANDLE),
        Ok(info) if info.kind == ObjectKind::Endpoint && info.rights == Rights::SEND
    ) || !matches!(
        ipc::wait_for_handle(RESTART_CONTROL_HANDLE),
        Ok(info) if info.kind == ObjectKind::Endpoint && info.rights == Rights::RECEIVE
    ) {
        syscall::exit(90);
    }
    if !recover_public_probe_artifact() {
        syscall::exit(91);
    }

    let stale = open_with_retry(
        NULLFS_PUBLIC_PROBE,
        syscall::OpenFlags::READ | syscall::OpenFlags::WRITE | syscall::OpenFlags::CREATE,
    )
    .unwrap_or_else(|| syscall::exit(92));
    if !descriptor_stat_matches(stale, 0) {
        syscall::exit(93);
    }
    if !write_all_with_retry(stale, COMBINED_BYTES)
        || !descriptor_stat_matches(stale, COMBINED_BYTES.len() as u64)
        || syscall::seek(stale, syscall::SeekFrom::Start(0)).ok() != Some(0)
    {
        syscall::exit(94);
    }
    if ipc::send(SERVICE_HANDLE, NULLFS_RESTART_READY, None).is_err() {
        syscall::exit(95);
    }

    let mut control = [0_u8; 64];
    let message = match ipc::receive(RESTART_CONTROL_HANDLE, &mut control) {
        Ok(message) => message,
        Err(_) => syscall::exit(96),
    };
    if message.sender_process_id != 1
        || message.capability.is_some()
        || message.bytes != NULLFS_RESTART_REPLACEMENT.len()
        || &control[..message.bytes] != NULLFS_RESTART_REPLACEMENT
    {
        syscall::exit(100);
    }
    let executable_result = probe_nullfs_executable();
    if executable_result != 0 {
        syscall::exit(130 + executable_result);
    }
    let mut bytes = [0_u8; COMBINED_BYTES.len()];
    if syscall::read(stale, &mut bytes) != Err(syscall::Errno::IO)
        || !platform_failed_with(platform::fstat(stale), errno::IO)
        || syscall::seek(stale, syscall::SeekFrom::Start(0)) != Err(syscall::Errno::IO)
        || syscall::seek(stale, syscall::SeekFrom::Current(0)) != Err(syscall::Errno::IO)
        || syscall::seek(stale, syscall::SeekFrom::End(0)) != Err(syscall::Errno::IO)
    {
        syscall::exit(98);
    }
    let replacement = open_with_retry(
        NULLFS_PUBLIC_PROBE_RAW,
        syscall::OpenFlags::READ | syscall::OpenFlags::WRITE,
    )
    .unwrap_or_else(|| syscall::exit(100));
    if !descriptor_stat_matches(replacement, COMBINED_BYTES.len() as u64)
        || !read_matches(replacement, COMBINED_BYTES)
    {
        syscall::exit(101);
    }
    if !unlink_with_retry(NULLFS_PUBLIC_PROBE)
        || !platform_failed_with(platform::stat(NULLFS_PUBLIC_PROBE), errno::NO_ENTRY)
        || !platform_failed_with(platform::stat(NULLFS_PUBLIC_PROBE_RAW), errno::NO_ENTRY)
    {
        syscall::exit(102);
    }
    if syscall::seek(replacement, syscall::SeekFrom::Start(0)).ok() != Some(0)
        || !read_matches(replacement, COMBINED_BYTES)
    {
        syscall::exit(103);
    }
    if !nullfs_root_is_valid() {
        syscall::exit(104);
    }
    if syscall::close(stale).is_err() || syscall::close(replacement).is_err() {
        syscall::exit(105);
    }
    for _ in 0..8 {
        syscall::yield_now().unwrap_or_else(|_| syscall::exit(106));
    }
    if !fixture_files_are_exact()
        || !nullfs_root_is_valid()
        || !platform_failed_with(platform::stat(NULLFS_PUBLIC_PROBE), errno::NO_ENTRY)
        || !platform_failed_with(platform::stat(NULLFS_PUBLIC_PROBE_RAW), errno::NO_ENTRY)
    {
        syscall::exit(107);
    }
    syscall::exit(0)
}

fn probe_nullfs_executable() -> u64 {
    let mut executable_kind = None;
    for _ in 0..64 {
        match platform::stat(NULLFS_EXEC_TARGET) {
            Ok(stat) => {
                executable_kind = Some(stat.kind);
                break;
            }
            Err(error) if error == platform::Errno::TRY_AGAIN => {
                if syscall::yield_now().is_err() {
                    return 1;
                }
            }
            Err(_) => return 1,
        }
    }
    if executable_kind != Some(file::KIND_FILE) {
        return 1;
    }

    let malformed = match syscall::spawn_command(
        NULLFS_EXEC_MALFORMED_COMMAND,
        syscall::SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
    ) {
        Ok(process_id) => process_id,
        Err(_) => return 8,
    };
    if syscall::wait_child(malformed)
        .ok()
        .map(|status| status.raw())
        != Some(126)
    {
        return 8;
    }

    let spawned = match syscall::spawn_command(
        NULLFS_EXEC_SPAWN_COMMAND,
        syscall::SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
    ) {
        Ok(process_id) => process_id,
        Err(_) => return 2,
    };
    if syscall::wait_child(spawned).ok().map(|status| status.raw())
        != Some(NULLFS_EXEC_SPAWN_STATUS)
    {
        return 3;
    }

    let parent_process_id = match syscall::getpid() {
        Ok(process_id) => process_id,
        Err(_) => return 4,
    };
    let parent_process_group = match platform::get_process_group(0) {
        Ok(process_group) if process_group == parent_process_id => process_group,
        _ => return 5,
    };
    let child = match syscall::fork() {
        Ok(process_id) => process_id,
        Err(_) => return 6,
    };
    if child == 0 {
        let preserved = syscall::open(
            NULLFS_EXEC_PRESERVED_PATH,
            syscall::OpenFlags::WRITE | syscall::OpenFlags::CREATE | syscall::OpenFlags::TRUNCATE,
        )
        .unwrap_or_else(|_| syscall::exit(128));
        let closed = syscall::open(
            NULLFS_EXEC_CLOSED_PATH,
            syscall::OpenFlags::WRITE
                | syscall::OpenFlags::CREATE
                | syscall::OpenFlags::TRUNCATE
                | syscall::OpenFlags::CLOSE_ON_EXEC,
        )
        .unwrap_or_else(|_| syscall::exit(129));
        let child_process_id = syscall::getpid().unwrap_or_else(|_| syscall::exit(120));
        let child_process_group =
            platform::get_process_group(0).unwrap_or_else(|_| syscall::exit(121));
        if child_process_id == parent_process_id
            || child_process_group != parent_process_group
            || child_process_id == child_process_group
        {
            syscall::exit(122);
        }
        match syscall::execve(NULLFS_EXEC_MISSING_COMMAND) {
            Err(error) if error == syscall::Errno::NO_ENTRY => {}
            _ => syscall::exit(123),
        }
        match syscall::execve(NULLFS_EXEC_NOT_DIRECTORY_COMMAND) {
            Err(error) if i64::from(error.code()) == -errno::NOT_DIRECTORY => {}
            _ => syscall::exit(131),
        }
        match syscall::execve(NULLFS_EXEC_MALFORMED_COMMAND) {
            Err(error) if error == syscall::Errno::IO => {}
            _ => syscall::exit(127),
        }
        if syscall::write_all(preserved, b"preserved before routed exec\n").is_err()
            || syscall::write_all(closed, b"cloexec before routed exec\n").is_err()
        {
            syscall::exit(130);
        }

        let mut command = [0_u8; 128];
        command[..NULLFS_EXEC_FORK_PREFIX.len()].copy_from_slice(NULLFS_EXEC_FORK_PREFIX);
        let mut length = NULLFS_EXEC_FORK_PREFIX.len();
        if !append_decimal(&mut command, &mut length, child_process_id)
            || !append_byte(&mut command, &mut length, b' ')
            || !append_decimal(&mut command, &mut length, child_process_group)
            || !append_byte(&mut command, &mut length, b' ')
            || !append_decimal(&mut command, &mut length, preserved)
            || !append_byte(&mut command, &mut length, b' ')
            || !append_decimal(&mut command, &mut length, closed)
        {
            syscall::exit(124);
        }
        if syscall::execve(&command[..length]).is_err() {
            syscall::exit(125);
        }
        syscall::exit(126);
    }

    if syscall::wait_child(child).ok().map(|status| status.raw()) == Some(NULLFS_EXEC_FORK_STATUS) {
        0
    } else {
        7
    }
}

fn append_decimal(buffer: &mut [u8], length: &mut usize, mut value: u64) -> bool {
    let mut digits = [0_u8; 20];
    let mut count = 0;
    loop {
        digits[count] = b'0' + (value % 10) as u8;
        count += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    for &digit in digits[..count].iter().rev() {
        if !append_byte(buffer, length, digit) {
            return false;
        }
    }
    true
}

fn append_byte(buffer: &mut [u8], length: &mut usize, byte: u8) -> bool {
    let Some(slot) = buffer.get_mut(*length) else {
        return false;
    };
    *slot = byte;
    *length += 1;
    true
}

fn recover_public_probe_artifact() -> bool {
    let stat = match platform::stat(NULLFS_PUBLIC_PROBE) {
        Ok(stat) => stat,
        Err(error) if error == platform::Errno::NO_ENTRY => return true,
        Err(_) => return false,
    };
    if stat.kind != file::KIND_FILE || stat.flags != 0 {
        return false;
    }
    let expected = if stat.size == 0 {
        &[]
    } else if stat.size == INITIAL_BYTES.len() as u64 {
        INITIAL_BYTES
    } else if stat.size == PREFIXED_BYTES.len() as u64 {
        PREFIXED_BYTES
    } else if stat.size == COMBINED_BYTES.len() as u64 {
        COMBINED_BYTES
    } else if stat.size == TRUNCATED_BYTES.len() as u64 {
        TRUNCATED_BYTES
    } else {
        return false;
    };
    let Some(descriptor) = open_with_retry(NULLFS_PUBLIC_PROBE, syscall::OpenFlags::READ) else {
        return false;
    };
    if !read_matches(descriptor, expected)
        || !unlink_with_retry(NULLFS_PUBLIC_PROBE)
        || !platform_failed_with(platform::stat(NULLFS_PUBLIC_PROBE), errno::NO_ENTRY)
    {
        let _ = syscall::close(descriptor);
        return false;
    }
    syscall::close(descriptor).is_ok()
}

fn nullfs_root_is_valid() -> bool {
    let mut start_index = 0;
    let mut found_docs = false;
    let mut found_welcome = false;

    for _ in 0..128 {
        let mut entries = [platform::DirectoryEntry::EMPTY; 2];
        let Some(count) = read_directory_with_retry(NULLFS_MOUNT, start_index, &mut entries) else {
            return false;
        };
        for entry in &entries[..count] {
            if !valid_directory_entry(entry) {
                return false;
            }
            if entry.name() == b"docs" {
                if found_docs || entry.kind != file::KIND_DIRECTORY {
                    return false;
                }
                found_docs = true;
            } else if entry.name() == b"welcome.txt" {
                if found_welcome || entry.kind != file::KIND_FILE {
                    return false;
                }
                found_welcome = true;
            } else if entry.name() == b"nullstar-vfs-probe-v1.bin" {
                return false;
            }
        }
        if count < entries.len() {
            return found_docs && found_welcome;
        }
        let Some(next_index) = start_index.checked_add(count) else {
            return false;
        };
        start_index = next_index;
    }
    false
}

fn valid_directory_entry(entry: &platform::DirectoryEntry) -> bool {
    let Ok(name_length) = usize::try_from(entry.name_length) else {
        return false;
    };
    name_length != 0
        && name_length <= entry.name.len()
        && matches!(entry.kind, file::KIND_FILE | file::KIND_DIRECTORY)
        && entry.flags == 0
        && !entry.name[..name_length].contains(&b'/')
        && !entry.name[..name_length].contains(&0)
        && entry.name[name_length..].iter().all(|byte| *byte == 0)
}

fn nullfs_docs_is_valid() -> bool {
    let mut start_index = 0;
    let mut found_readme = false;

    for _ in 0..128 {
        let mut entries = [platform::DirectoryEntry::EMPTY; 2];
        let Some(count) = read_directory_with_retry(NULLFS_DOCS, start_index, &mut entries) else {
            return false;
        };
        for entry in &entries[..count] {
            if !valid_directory_entry(entry) {
                return false;
            }
            if entry.name() == b"readme.txt" {
                if found_readme || entry.kind != file::KIND_FILE {
                    return false;
                }
                found_readme = true;
            }
        }
        if count < entries.len() {
            return found_readme;
        }
        let Some(next_index) = start_index.checked_add(count) else {
            return false;
        };
        start_index = next_index;
    }
    false
}

fn fixture_files_are_exact() -> bool {
    path_contents_match(NULLFS_WELCOME, WELCOME) && path_contents_match(NULLFS_README, README)
}

fn path_contents_match(path: &[u8], expected: &[u8]) -> bool {
    let Some(descriptor) = open_with_retry(path, syscall::OpenFlags::READ) else {
        return false;
    };
    let matches = descriptor_stat_matches(descriptor, expected.len() as u64)
        && read_matches(descriptor, expected);
    syscall::close(descriptor).is_ok() && matches
}

fn descriptor_stat_matches(descriptor: syscall::FileDescriptor, size: u64) -> bool {
    platform::fstat(descriptor).ok()
        == Some(file::Stat {
            kind: file::KIND_FILE,
            size,
            flags: 0,
        })
}

fn stat_matches(path: &[u8], kind: u64, size: u64) -> bool {
    platform::stat(path).ok()
        == Some(file::Stat {
            kind,
            size,
            flags: 0,
        })
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

fn write_all_with_retry(descriptor: syscall::FileDescriptor, mut bytes: &[u8]) -> bool {
    for _ in 0..64 {
        if bytes.is_empty() {
            return true;
        }
        match syscall::write(descriptor, bytes) {
            Ok(0) => return false,
            Ok(written) if written <= bytes.len() => bytes = &bytes[written..],
            Ok(_) => return false,
            Err(error) if error == syscall::Errno::TRY_AGAIN => {
                if syscall::yield_now().is_err() {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    bytes.is_empty()
}

fn read_matches(descriptor: syscall::FileDescriptor, expected: &[u8]) -> bool {
    if expected.len() > README.len() {
        return false;
    }
    let mut actual = [0_u8; README.len()];
    let mut offset = 0;
    for _ in 0..64 {
        if offset == expected.len() {
            break;
        }
        match syscall::read(descriptor, &mut actual[offset..expected.len()]) {
            Ok(0) => return false,
            Ok(read) if read <= expected.len() - offset => offset += read,
            Ok(_) => return false,
            Err(error) if error == syscall::Errno::TRY_AGAIN => {
                if syscall::yield_now().is_err() {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    if offset != expected.len() || &actual[..offset] != expected {
        return false;
    }
    let mut extra = [0_u8; 1];
    for _ in 0..8 {
        match syscall::read(descriptor, &mut extra) {
            Ok(0) => return true,
            Ok(_) => return false,
            Err(error) if error == syscall::Errno::TRY_AGAIN => {
                if syscall::yield_now().is_err() {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    false
}

fn unlink_with_retry(path: &[u8]) -> bool {
    for _ in 0..8 {
        match platform::unlink(path) {
            Ok(()) => return true,
            Err(error) if error == platform::Errno::TRY_AGAIN => {
                if syscall::yield_now().is_err() {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    false
}

fn read_directory_with_retry(
    path: &[u8],
    start_index: usize,
    entries: &mut [platform::DirectoryEntry],
) -> Option<usize> {
    for _ in 0..256 {
        match platform::read_directory(path, start_index, entries) {
            Ok(count) => return Some(count),
            Err(error) if error == platform::Errno::TRY_AGAIN => {
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
        let Some(count) = read_directory_with_retry(path, offset, &mut entries) else {
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

fn reply_binding_is_canonical(reply: &protocol::Reply) -> bool {
    reply.binding_prefix().is_ok()
}

fn reply_binding_matches(reply: &protocol::Reply, route_id: u32) -> bool {
    if route_id == protocol::route::APPLICATIONS {
        reply.binding_prefix() == Ok(Some("/Applications"))
    } else {
        reply.binding_prefix() == Ok(None)
    }
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
        && reply.reserved == [0; 8]
        && reply_binding_is_canonical(&reply))
    .then_some(reply)
}
