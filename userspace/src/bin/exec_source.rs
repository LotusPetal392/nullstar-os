#![no_std]
#![no_main]

use userspace::{
    platform,
    syscall::{self, DescriptorFlags, Errno, OpenFlags},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const PRESERVED_PATH: &[u8] = b"/tmp/exec-preserved.txt";
const CLOSED_PATH: &[u8] = b"/tmp/exec-closed.txt";
const VALID_EXEC: &[u8] = b"../exec-target alpha beta";
const INVALID_EXEC: &[u8] = b"/missing-exec";

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let preserved = match syscall::open(
        PRESERVED_PATH,
        OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
    ) {
        Ok(descriptor) => descriptor,
        Err(_) => syscall::exit(1),
    };
    let closed = match syscall::open(
        CLOSED_PATH,
        OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::CLOSE_ON_EXEC,
    ) {
        Ok(descriptor) => descriptor,
        Err(_) => syscall::exit(2),
    };
    if preserved != 3 || closed != 4 {
        syscall::exit(3);
    }

    if syscall::write_all(preserved, b"source-before-failed-exec\n").is_err()
        || syscall::write_all(closed, b"cloexec-before-failed-exec\n").is_err()
    {
        syscall::exit(4);
    }

    match syscall::execve(INVALID_EXEC) {
        Err(error) if error == Errno::NO_ENTRY => {}
        _ => syscall::exit(5),
    }

    if syscall::write_all(preserved, b"source-after-failed-exec\n").is_err()
        || syscall::write_all(closed, b"cloexec-after-failed-exec\n").is_err()
    {
        syscall::exit(6);
    }

    if syscall::set_descriptor_flags(preserved, DescriptorFlags::CLOSE_ON_EXEC).is_err()
        || syscall::set_descriptor_flags(preserved, DescriptorFlags::EMPTY).is_err()
        || platform::chdir(b"/tmp").is_err()
    {
        syscall::exit(7);
    }

    if syscall::execve(VALID_EXEC).is_err() {
        syscall::exit(8);
    }
    syscall::exit(9)
}
