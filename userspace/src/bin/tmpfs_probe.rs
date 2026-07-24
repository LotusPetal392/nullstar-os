#![no_std]
#![no_main]

use userspace::{
    ipc::{self, ObjectKind, Rights},
    syscall,
    tmpfs::{self, Error},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SERVICE_HANDLE: u64 = 1;
const NAME: &[u8] = b"phase3.txt";
const PAYLOAD: &[u8] = b"userspace tmpfs is alive";

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let info = match ipc::wait_for_handle(SERVICE_HANDLE) {
        Ok(info) => info,
        Err(_) => syscall::exit(1),
    };
    if info.kind != ObjectKind::Endpoint || info.rights != Rights::SEND {
        syscall::exit(2);
    }
    if tmpfs::write(SERVICE_HANDLE, NAME, PAYLOAD).ok() != Some(PAYLOAD.len()) {
        syscall::exit(3);
    }
    if tmpfs::stat(SERVICE_HANDLE, NAME).ok() != Some(PAYLOAD.len()) {
        syscall::exit(4);
    }
    let mut read_buffer = [0_u8; 64];
    let count = match tmpfs::read(SERVICE_HANDLE, NAME, 0, &mut read_buffer) {
        Ok(count) => count,
        Err(_) => syscall::exit(5),
    };
    if count != PAYLOAD.len() || &read_buffer[..count] != PAYLOAD {
        syscall::exit(6);
    }
    let mut listing = [0_u8; 64];
    let listed = match tmpfs::list(SERVICE_HANDLE, &mut listing) {
        Ok(count) => count,
        Err(_) => syscall::exit(7),
    };
    if &listing[..listed] != NAME {
        syscall::exit(8);
    }
    if tmpfs::remove(SERVICE_HANDLE, NAME).is_err()
        || tmpfs::stat(SERVICE_HANDLE, NAME) != Err(Error::NotFound)
    {
        syscall::exit(9);
    }
    syscall::exit(0)
}
