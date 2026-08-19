#![no_std]
#![no_main]

use userspace::{
    args::Args,
    handle::Endpoint,
    ipc::{ObjectKind, Rights},
    managed_startup::ManagedToolStart,
    runtime_context::{CapabilityRole, StartupCapabilityPolicy},
    sv,
    syscall::{self, STDERR},
};

userspace::managed_capability_tool_entry!(rust_main, 1, &STARTUP_POLICIES);
userspace::panic_handler!();

const STARTUP_POLICIES: [StartupCapabilityPolicy; 2] = [
    StartupCapabilityPolicy {
        role: CapabilityRole::SERVICE_CONTROL_OBSERVATION,
        kind: ObjectKind::Endpoint,
        minimum_rights: Rights::SEND,
        maximum_rights: Rights::SEND,
        required: false,
    },
    StartupCapabilityPolicy {
        role: CapabilityRole::SERVICE_CONTROL_MUTATION,
        kind: ObjectKind::Endpoint,
        minimum_rights: Rights::SEND,
        maximum_rights: Rights::SEND,
        required: false,
    },
];
const USAGE: &[u8] =
    b"usage: sv list | sv status SERVICE | sv start SERVICE | sv stop SERVICE | sv restart SERVICE\n";
const UNKNOWN_SERVICE: &[u8] = b"sv: unknown service\n";
const FAILURE: &[u8] = b"sv: operation failed\n";
const MUTATION_OUTCOME_UNKNOWN: &[u8] =
    b"sv: mutation outcome unknown; inspect service status before retrying\n";
const MUTATION_COMMITTED_OUTPUT_FAILED: &[u8] =
    b"sv: mutation committed, but its result could not be printed\n";

fn rust_main(initial_stack: *const usize, mut start: ManagedToolStart<1>) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let role = match arguments.get(1) {
        Some(b"list") | Some(b"status") => CapabilityRole::SERVICE_CONTROL_OBSERVATION,
        Some(b"start") | Some(b"stop") | Some(b"restart") => {
            CapabilityRole::SERVICE_CONTROL_MUTATION
        }
        _ => fail(USAGE, 64),
    };
    let authority = match start.context.take::<Endpoint>(role, Rights::SEND) {
        Ok(authority) if start.context.is_empty() => authority,
        _ => syscall::exit(125),
    };
    let result = match (arguments.len(), arguments.get(1), arguments.get(2)) {
        (2, Some(b"list"), None) => sv::list(authority.as_raw()),
        (3, Some(b"status"), Some(name)) => {
            let Some(service) = sv::service_id(name) else {
                fail(UNKNOWN_SERVICE, 64);
            };
            sv::status(authority.as_raw(), service)
        }
        (3, Some(b"start"), Some(name)) => {
            let Some(service) = sv::service_id(name) else {
                fail(UNKNOWN_SERVICE, 64);
            };
            sv::start(authority.as_raw(), service)
        }
        (3, Some(b"stop"), Some(name)) => {
            let Some(service) = sv::service_id(name) else {
                fail(UNKNOWN_SERVICE, 64);
            };
            sv::stop(authority.as_raw(), service)
        }
        (3, Some(b"restart"), Some(name)) => {
            let Some(service) = sv::service_id(name) else {
                fail(UNKNOWN_SERVICE, 64);
            };
            sv::restart(authority.as_raw(), service)
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
