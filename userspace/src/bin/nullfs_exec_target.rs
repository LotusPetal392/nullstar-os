#![no_std]
#![no_main]

use userspace::{args::Args, platform, syscall};

userspace::entry!(rust_main);
userspace::panic_handler!();

const TARGET_PATH: &[u8] = b"/Applications/ExecProbe/bin/exec-target";
const SYSTEM_TARGET_PATH: &[u8] = b"/System/bin/exec-target";
const SPAWN_MODE: &[u8] = b"spawn";
const SYSTEM_SPAWN_MODE: &[u8] = b"system-spawn";
const FORK_EXEC_MODE: &[u8] = b"fork-exec";
const SPAWN_SUCCESS: u64 = 41;
const FORK_EXEC_SUCCESS: u64 = 42;

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let target_path = arguments.get(0);
    if target_path != Some(TARGET_PATH) && target_path != Some(SYSTEM_TARGET_PATH) {
        syscall::exit(64);
    }

    let process_id = syscall::getpid().unwrap_or_else(|_| syscall::exit(1));
    let process_group = platform::get_process_group(0).unwrap_or_else(|_| syscall::exit(2));

    if arguments.len() == 2 && arguments.get(1) == Some(SPAWN_MODE) {
        if target_path != Some(TARGET_PATH) || process_id == 0 || process_group != process_id {
            syscall::exit(3);
        }
        syscall::exit(SPAWN_SUCCESS);
    }

    if arguments.len() == 2 && arguments.get(1) == Some(SYSTEM_SPAWN_MODE) {
        if target_path != Some(SYSTEM_TARGET_PATH) || process_id == 0 || process_group != process_id
        {
            syscall::exit(3);
        }
        syscall::exit(SPAWN_SUCCESS);
    }

    if arguments.len() == 6 && arguments.get(1) == Some(FORK_EXEC_MODE) {
        let expected_process_id = arguments
            .get(2)
            .and_then(parse_decimal)
            .unwrap_or_else(|| syscall::exit(4));
        let expected_process_group = arguments
            .get(3)
            .and_then(parse_decimal)
            .unwrap_or_else(|| syscall::exit(5));
        let preserved = arguments
            .get(4)
            .and_then(parse_decimal)
            .unwrap_or_else(|| syscall::exit(7));
        let closed = arguments
            .get(5)
            .and_then(parse_decimal)
            .unwrap_or_else(|| syscall::exit(8));
        if process_id != expected_process_id
            || process_group != expected_process_group
            || process_id == process_group
        {
            syscall::exit(6);
        }
        if !write_all_with_retry(preserved, b"preserved after routed exec\n") {
            syscall::exit(9);
        }
        match syscall::write_all(closed, b"cloexec must be closed\n") {
            Err(error) if error == syscall::Errno::BAD_FILE_DESCRIPTOR => {}
            _ => syscall::exit(10),
        }
        syscall::exit(FORK_EXEC_SUCCESS);
    }

    syscall::exit(64)
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

fn parse_decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    let mut value = 0_u64;
    for &byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
    }
    Some(value)
}
