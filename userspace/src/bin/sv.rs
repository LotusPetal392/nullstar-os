#![no_std]
#![no_main]

use userspace::{
    args::Args,
    sv,
    syscall::{self, STDERR},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const OBSERVATION_GRANT_HANDLE: u64 = 1;
const USAGE: &[u8] = b"usage: sv list | sv status SERVICE\n";
const UNKNOWN_SERVICE: &[u8] = b"sv: unknown service\n";
const FAILURE: &[u8] = b"sv: observation failed\n";

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let result = match (arguments.len(), arguments.get(1), arguments.get(2)) {
        (2, Some(b"list"), None) => sv::list(OBSERVATION_GRANT_HANDLE),
        (3, Some(b"status"), Some(name)) => {
            let Some(service) = sv::service_id(name) else {
                fail(UNKNOWN_SERVICE, 64);
            };
            sv::status(OBSERVATION_GRANT_HANDLE, service)
        }
        _ => fail(USAGE, 64),
    };
    if result.is_err() {
        fail(FAILURE, 1);
    }
    syscall::exit(0)
}

fn fail(message: &[u8], code: u64) -> ! {
    let _ = syscall::write_all(STDERR, message);
    syscall::exit(code)
}
