//! Common managed-startup harness for provisional userspace driver services.
//!
//! The current driver binaries do not own hardware yet. This harness gives each binary the same
//! fail-closed startup contract while their device protocols and kernel-delegated hardware
//! capabilities are designed.

use crate::{
    abi::limits,
    args::Args,
    handle::Endpoint,
    ipc::{self, ObjectKind, Rights},
    managed_startup::{ManagedServiceIdentity, receive_managed_service_start},
    runtime_context::{CapabilityRole, StartupCapabilityPolicy},
    syscall,
};

const STARTUP_ERROR: u64 = 2;
const CAPABILITY_ERROR: u64 = 3;
const READINESS_ERROR: u64 = 4;
const REQUEST_ERROR: u64 = 5;

/// Starts one provisional driver service with readiness and request endpoints.
///
/// Until a driver-specific protocol is implemented, request packets are drained and any attached
/// authority is immediately closed. This prevents malformed or unexpected requests from leaking
/// capabilities while keeping the managed service responsive to process lifecycle control.
///
/// # Safety
///
/// `initial_stack` must be the untouched stack pointer supplied to the process entry point.
pub unsafe fn run_provisional_driver_service(
    initial_stack: *const usize,
    identity: ManagedServiceIdentity,
    ready_message: &[u8],
) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if arguments.len() != 1 {
        syscall::exit(1);
    }

    let policies = [
        StartupCapabilityPolicy {
            role: CapabilityRole::READINESS,
            kind: ObjectKind::Endpoint,
            minimum_rights: Rights::SEND,
            maximum_rights: Rights::SEND,
            required: true,
        },
        StartupCapabilityPolicy {
            role: CapabilityRole::SERVICE_REQUEST,
            kind: ObjectKind::Endpoint,
            minimum_rights: Rights::RECEIVE,
            maximum_rights: Rights::RECEIVE,
            required: true,
        },
    ];
    let mut start = match receive_managed_service_start::<2>(arguments, &policies, identity) {
        Ok(start) => start,
        Err(_) => syscall::exit(STARTUP_ERROR),
    };
    let readiness = match start
        .context
        .take::<Endpoint>(CapabilityRole::READINESS, Rights::SEND)
    {
        Ok(handle) => handle,
        Err(_) => syscall::exit(CAPABILITY_ERROR),
    };
    let request = match start
        .context
        .take::<Endpoint>(CapabilityRole::SERVICE_REQUEST, Rights::RECEIVE)
    {
        Ok(handle) if start.context.is_empty() => handle,
        _ => syscall::exit(CAPABILITY_ERROR),
    };
    if readiness.send(ready_message).is_err() || readiness.close().is_err() {
        syscall::exit(READINESS_ERROR);
    }

    let mut bytes = [0_u8; limits::MAX_IPC_MESSAGE_BYTES];
    loop {
        if ipc::receive(request.as_raw(), &mut bytes).is_err() {
            syscall::exit(REQUEST_ERROR);
        }
    }
}
