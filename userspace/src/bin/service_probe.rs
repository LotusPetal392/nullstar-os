#![no_std]
#![no_main]

use userspace::{
    ipc::{self, ObjectKind, Rights},
    platform,
    syscall::{self, OpenFlags},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const BOOTSTRAP_HANDLE: u64 = 1;
const FIRST_START_MARKER: &[u8] = b"/tmp/service-probe.started";
const READY_MESSAGE: &[u8] = b"service-ready: probe";

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    if platform::stat(FIRST_START_MARKER).is_err() {
        let descriptor = match syscall::open(
            FIRST_START_MARKER,
            OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
        ) {
            Ok(descriptor) => descriptor,
            Err(_) => syscall::exit(70),
        };
        if syscall::write_all(descriptor, b"restart requested\n").is_err()
            || syscall::close(descriptor).is_err()
        {
            syscall::exit(71);
        }
        syscall::exit(75);
    }

    let info = match ipc::wait_for_handle(BOOTSTRAP_HANDLE) {
        Ok(info) => info,
        Err(_) => syscall::exit(72),
    };
    if info.kind != ObjectKind::Endpoint || info.rights != Rights::SEND {
        syscall::exit(73);
    }
    if ipc::send(BOOTSTRAP_HANDLE, READY_MESSAGE, None).is_err() {
        syscall::exit(74);
    }

    loop {
        if syscall::yield_now().is_err() {
            syscall::exit(76);
        }
    }
}
