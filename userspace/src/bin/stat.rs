#![no_std]
#![no_main]

use userspace::{
    abi::{file, limits},
    args::Args,
    platform,
    syscall::{self, STDERR, STDOUT},
};

userspace::managed_tool_entry!(rust_main);
userspace::panic_handler!();

const USAGE: &[u8] = b"usage: stat PATH\n";
const FAILURE: &[u8] = b"stat: metadata lookup failed\n";

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if arguments.len() != 2 {
        let _ = syscall::write_all(STDERR, USAGE);
        syscall::exit(64);
    }
    let Some(path) = arguments.get(1) else {
        syscall::exit(64);
    };
    if path.is_empty() || path.len() > limits::MAX_PATH_BYTES {
        let _ = syscall::write_all(STDERR, USAGE);
        syscall::exit(64);
    }

    let metadata = match platform::stat(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            let _ = syscall::write_all(STDERR, FAILURE);
            syscall::exit(1);
        }
    };

    if syscall::write_all(STDOUT, b"path: ").is_err()
        || syscall::write_all(STDOUT, path).is_err()
        || syscall::write_all(STDOUT, b"\ntype: ").is_err()
        || syscall::write_all(STDOUT, kind_name(metadata.kind)).is_err()
        || syscall::write_all(STDOUT, b"\nsize: ").is_err()
        || write_decimal(metadata.size).is_err()
        || syscall::write_all(STDOUT, b"\nflags:").is_err()
        || write_flags(metadata.flags).is_err()
        || syscall::write_all(STDOUT, b"\n").is_err()
    {
        syscall::exit(1);
    }
    syscall::exit(0)
}

fn kind_name(kind: u64) -> &'static [u8] {
    match kind {
        file::KIND_FILE => b"file",
        file::KIND_DIRECTORY => b"directory",
        file::KIND_TERMINAL => b"terminal",
        file::KIND_PIPE => b"pipe",
        _ => b"unknown",
    }
}

fn write_flags(flags: u64) -> syscall::Result<()> {
    let mut found = false;
    for (mask, name) in [
        (file::FLAG_READ_ONLY, b" read-only" as &[u8]),
        (file::FLAG_HIDDEN, b" hidden"),
        (file::FLAG_SYSTEM, b" system"),
    ] {
        if flags & mask != 0 {
            syscall::write_all(STDOUT, name)?;
            found = true;
        }
    }
    if !found {
        syscall::write_all(STDOUT, b" none")?;
    }
    Ok(())
}

fn write_decimal(mut value: u64) -> syscall::Result<()> {
    let mut bytes = [0_u8; 20];
    let mut start = bytes.len();
    if value == 0 {
        return syscall::write_all(STDOUT, b"0");
    }
    while value != 0 {
        start -= 1;
        bytes[start] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    syscall::write_all(STDOUT, &bytes[start..])
}
