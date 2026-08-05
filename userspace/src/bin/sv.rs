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
const MUTATION_GRANT_HANDLE: u64 = 2;
const USAGE: &[u8] =
    b"usage: sv list | sv status SERVICE | sv start SERVICE | sv stop SERVICE | sv restart SERVICE\n";
const UNKNOWN_SERVICE: &[u8] = b"sv: unknown service\n";
const FAILURE: &[u8] = b"sv: operation failed\n";
const MUTATION_OUTCOME_UNKNOWN: &[u8] =
    b"sv: mutation outcome unknown; inspect service status before retrying\n";
const MUTATION_COMMITTED_OUTPUT_FAILED: &[u8] =
    b"sv: mutation committed, but its result could not be printed\n";

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
        (3, Some(b"start"), Some(name)) => {
            let Some(service) = sv::service_id(name) else {
                fail(UNKNOWN_SERVICE, 64);
            };
            sv::start(MUTATION_GRANT_HANDLE, service)
        }
        (3, Some(b"stop"), Some(name)) => {
            let Some(service) = sv::service_id(name) else {
                fail(UNKNOWN_SERVICE, 64);
            };
            sv::stop(MUTATION_GRANT_HANDLE, service)
        }
        (3, Some(b"restart"), Some(name)) => {
            let Some(service) = sv::service_id(name) else {
                fail(UNKNOWN_SERVICE, 64);
            };
            sv::restart(MUTATION_GRANT_HANDLE, service)
        }
        _ => fail(USAGE, 64),
    };
    match result {
        Ok(()) => syscall::exit(0),
        Err(sv::Error::MutationOutcomeUnknown) => fail(MUTATION_OUTCOME_UNKNOWN, 2),
        Err(sv::Error::MutationCommittedButOutputFailed(_)) => {
            fail(MUTATION_COMMITTED_OUTPUT_FAILED, 3)
        }
        Err(_) => fail(FAILURE, 1),
    }
}

fn fail(message: &[u8], code: u64) -> ! {
    let _ = syscall::write_all(STDERR, message);
    syscall::exit(code)
}
