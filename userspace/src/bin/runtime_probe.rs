#![no_std]
#![no_main]

use userspace::{
    abi::{capability, file, signal},
    args::Args,
    heap::BumpHeap,
    platform::{self, DirectoryEntry},
    syscall::{self, OpenFlags, SpawnFlags, STDERR, STDIN, STDOUT},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SUCCESS: &[u8] = b"userspace Rust runtime probe passed\n";
const DIRECTORY_PAGE: usize = 8;

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let Some(argument) = arguments.get(1) else {
        syscall::exit(64);
    };
    if arguments.len() != 2 || argument.is_empty() {
        syscall::exit(64);
    }

    let mut heap = BumpHeap::<4096>::new();
    let block_length = {
        let Some(block) = heap.allocate(257, 16) else {
            syscall::exit(1);
        };
        if !(block.as_ptr() as usize).is_multiple_of(16) {
            syscall::exit(1);
        }
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = (index & 0xff) as u8;
        }
        if block[0] != 0 || block[256] != 0 {
            syscall::exit(1);
        }
        block.len()
    };

    let copy_matches = {
        let Some(copy) = heap.copy_bytes(argument, 8) else {
            syscall::exit(1);
        };
        copy == argument
    };
    if !copy_matches || heap.used() <= block_length {
        syscall::exit(1);
    }

    heap.reset();
    if heap.used() != 0 || heap.remaining() != heap.capacity() {
        syscall::exit(1);
    }
    if !platform_probe(argument) {
        syscall::exit(1);
    }
    if syscall::getpid().is_err() || syscall::write_all(STDOUT, SUCCESS).is_err() {
        syscall::exit(1);
    }
    syscall::exit(0)
}

fn platform_probe(argument: &[u8]) -> bool {
    let Ok(info) = platform::system_info() else {
        return false;
    };
    if info.abi_major != userspace::abi::ABI_VERSION_MAJOR
        || info.abi_minor != userspace::abi::ABI_VERSION_MINOR
        || info.capabilities & capability::PLATFORM_V1 != capability::PLATFORM_V1
        || info.capabilities & capability::PROCESS_GROUP_CONTROL == 0
        || info.page_size != 4096
        || info.maximum_open_files < 3
    {
        return false;
    }
    let Ok(process_group) = platform::get_process_group(0) else {
        return false;
    };
    if process_group == 0
        || platform::set_process_group(0, process_group).ok() != Some(process_group)
    {
        return false;
    }

    let Ok(hello_stat) = platform::stat(b"/hello.txt") else {
        return false;
    };
    if !hello_stat.is_file() || hello_stat.size < 2 {
        return false;
    }

    if !root_directory_has_expected_entries() {
        return false;
    }

    let mut cwd = [0_u8; 64];
    let Ok(initial_directory) = platform::getcwd(&mut cwd) else {
        return false;
    };
    if initial_directory != b"/" || platform::chdir(b"/tmp").is_err() {
        return false;
    }
    let Ok(tmp_directory) = platform::getcwd(&mut cwd) else {
        return false;
    };
    if tmp_directory != b"/tmp" {
        return false;
    }
    let Ok(relative_stat) = platform::stat(b".") else {
        return false;
    };
    if !relative_stat.is_directory() {
        return false;
    }
    let mut directory_page = [DirectoryEntry::EMPTY; 1];
    if platform::read_directory(b".", 0, &mut directory_page).is_err() {
        return false;
    }
    if argument != b"runtime-smoke" && (!relative_open_probe() || !relative_spawn_probe()) {
        return false;
    }
    if syscall::environment_set(b"PWD", b"/").is_ok() {
        return false;
    }
    if platform::chdir(b"..").is_err() {
        return false;
    }
    let Ok(parent_directory) = platform::getcwd(&mut cwd) else {
        return false;
    };
    if parent_directory != b"/" {
        return false;
    }

    if !descriptor_probe()
        || (argument != b"runtime-smoke" && !ordinary_descriptor_probe())
        || platform::getppid().is_err()
        || platform::kill(u64::MAX, signal::TERMINATE).err() != Some(platform::Errno::NO_PROCESS)
    {
        return false;
    }
    true
}

fn relative_open_probe() -> bool {
    const CONTENTS: &[u8] = b"cwd-aware open\n";
    let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE;
    let Ok(descriptor) = syscall::open(b"cwd-open.txt", flags) else {
        return false;
    };
    let written = syscall::write_all(descriptor, CONTENTS).is_ok();
    let closed = syscall::close(descriptor).is_ok();
    written
        && closed
        && platform::stat(b"/tmp/cwd-open.txt")
            .is_ok_and(|stat| stat.is_file() && stat.size == CONTENTS.len() as u64)
}

fn relative_spawn_probe() -> bool {
    let Ok(process_id) = syscall::spawn_command(
        b"../pwd",
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
    ) else {
        return false;
    };
    syscall::wait_child(process_id).is_ok_and(|status| status.success())
}

fn root_directory_has_expected_entries() -> bool {
    let mut offset = 0usize;
    let mut found_hello = false;
    let mut found_tmp = false;
    loop {
        let mut entries = [DirectoryEntry::EMPTY; DIRECTORY_PAGE];
        let Ok(count) = platform::read_directory(b"/", offset, &mut entries) else {
            return false;
        };
        for entry in &entries[..count] {
            found_hello |= entry.is_file() && ascii_eq_ignore_case(entry.name(), b"hello.txt");
            found_tmp |=
                entry.kind == file::KIND_DIRECTORY && ascii_eq_ignore_case(entry.name(), b"tmp");
        }
        offset = offset.saturating_add(count);
        if count < entries.len() {
            break;
        }
        if offset > 128 {
            return false;
        }
    }
    found_hello && found_tmp
}

fn descriptor_probe() -> bool {
    let Ok(stdout_stat) = platform::fstat(STDOUT) else {
        return false;
    };
    if !matches!(stdout_stat.kind, file::KIND_TERMINAL | file::KIND_FILE) {
        return false;
    }

    match platform::dup(STDOUT) {
        Ok(duplicate) if stdout_stat.kind == file::KIND_FILE => {
            let duplicate_matches = platform::fstat(duplicate)
                .is_ok_and(|stat| stat.kind == stdout_stat.kind && stat.flags == stdout_stat.flags);
            if syscall::close(duplicate).is_err() || !duplicate_matches {
                return false;
            }
        }
        Err(error)
            if stdout_stat.kind == file::KIND_TERMINAL
                && error == platform::Errno::NOT_IMPLEMENTED => {}
        _ => return false,
    }

    if platform::dup2(STDOUT, STDOUT).ok() != Some(STDOUT)
        || platform::dup2(STDOUT, STDERR).ok() != Some(STDERR)
        || platform::dup2(STDOUT, STDIN).err() != Some(platform::Errno::BAD_FILE_DESCRIPTOR)
    {
        return false;
    }
    platform::fstat(STDERR)
        .is_ok_and(|stat| stat.kind == stdout_stat.kind && stat.flags == stdout_stat.flags)
}

fn ordinary_descriptor_probe() -> bool {
    let Ok(descriptor) = syscall::open(b"/hello.txt", OpenFlags::READ) else {
        return false;
    };
    let Ok(stat) = platform::fstat(descriptor) else {
        let _ = syscall::close(descriptor);
        return false;
    };
    if !stat.is_file() {
        let _ = syscall::close(descriptor);
        return false;
    }

    let Ok(duplicate) = platform::dup(descriptor) else {
        let _ = syscall::close(descriptor);
        return false;
    };
    let Ok(reference) = syscall::open(b"/hello.txt", OpenFlags::READ) else {
        let _ = syscall::close(duplicate);
        let _ = syscall::close(descriptor);
        return false;
    };

    let mut first = [0_u8; 1];
    let mut second = [0_u8; 1];
    let mut expected = [0_u8; 2];
    let shared_offset = syscall::read(descriptor, &mut first).ok() == Some(1)
        && syscall::read(duplicate, &mut second).ok() == Some(1)
        && syscall::read(reference, &mut expected).ok() == Some(2)
        && first[0] == expected[0]
        && second[0] == expected[1];

    const DUP2_TARGET: u64 = 15;
    let dup2_ok = platform::dup2(descriptor, DUP2_TARGET).ok() == Some(DUP2_TARGET)
        && platform::fstat(DUP2_TARGET).is_ok();

    let _ = syscall::close(DUP2_TARGET);
    let _ = syscall::close(reference);
    let _ = syscall::close(duplicate);
    let _ = syscall::close(descriptor);
    shared_offset && dup2_ok
}

fn ascii_eq_ignore_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}
