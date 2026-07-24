#![no_std]
#![no_main]

use userspace::{
    ipc::{self, ObjectKind, Rights},
    syscall,
    tmpfs::{Error, Mount},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SERVICE_HANDLE: u64 = 1;
const NAME: &[u8] = b"phase4.txt";
const PAYLOAD: &[u8] = b"restart-aware userspace tmpfs";

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let info = match ipc::wait_for_handle(SERVICE_HANDLE) {
        Ok(info) => info,
        Err(_) => syscall::exit(1),
    };
    if info.kind != ObjectKind::Endpoint || info.rights != Rights::SEND {
        syscall::exit(2);
    }
    let mount = match Mount::connect(SERVICE_HANDLE) {
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
    if mount.remove(NAME).is_err() || mount.stat(NAME) != Err(Error::NotFound) {
        syscall::exit(10);
    }
    syscall::exit(0)
}
