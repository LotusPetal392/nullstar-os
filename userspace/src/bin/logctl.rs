#![no_std]
#![no_main]

use userspace::{
    args::Args,
    handle::Endpoint,
    ipc::{ObjectKind, Rights},
    logctl,
    managed_startup::ManagedToolStart,
    runtime_context::{CapabilityRole, StartupCapabilityPolicy},
    syscall::{self, STDERR, STDOUT},
};

userspace::managed_capability_tool_entry!(rust_main, 1, &STARTUP_POLICIES);
userspace::panic_handler!();

const STARTUP_POLICIES: [StartupCapabilityPolicy; 1] = [StartupCapabilityPolicy {
    role: CapabilityRole::LOGGING_OBSERVER_INGRESS,
    kind: ObjectKind::Endpoint,
    minimum_rights: Rights::SEND,
    maximum_rights: Rights::SEND,
    required: true,
}];
const COMPLETE_MARKER: &[u8] = b"logctl: show complete\n";
const USAGE: &[u8] = b"usage: logctl show\n";
const SHOW_FAILED: &[u8] = b"logctl: history query failed\n";

fn rust_main(initial_stack: *const usize, mut start: ManagedToolStart<1>) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if arguments.len() != 2 || arguments.get(1) != Some(b"show") {
        fail(USAGE, 2);
    }
    let observer = match start
        .context
        .take::<Endpoint>(CapabilityRole::LOGGING_OBSERVER_INGRESS, Rights::SEND)
    {
        Ok(observer) if start.context.is_empty() => observer,
        _ => syscall::exit(125),
    };
    if logctl::show(observer.as_raw()).is_err()
        || syscall::write_all(STDOUT, COMPLETE_MARKER).is_err()
    {
        fail(SHOW_FAILED, 3);
    }
    syscall::exit(0)
}

fn fail(message: &[u8], code: u64) -> ! {
    let _ = syscall::write_all(STDERR, message);
    syscall::exit(code)
}
