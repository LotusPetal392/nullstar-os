#![no_std]
#![no_main]

use userspace::{
    abi::INIT_PROCESS_ID,
    ipc::{self, CapabilityHandle, Rights},
    supervisor::{
        ServiceRuntime, ServiceSpec, ServiceStatusDisposition, ShellStatusDisposition,
        shell_status_disposition,
    },
    syscall::{self, ProcessId, STDERR, STDOUT, SpawnFlags},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const INIT_READY: &[u8] = b"userspace init ready: pid=1\n";
const SERVICE_COMMAND: &[u8] = b"/service-probe";
const SERVICE_READY_MESSAGE: &[u8] = b"service-ready: probe";
const SERVICE_STARTING: &[u8] = b"userspace init: starting probe service\n";
const SERVICE_RESTARTING: &[u8] = b"userspace init: probe service exited; restarting\n";
const SERVICE_READY: &[u8] = b"userspace init: probe service ready\n";
const SERVICE_FAILED: &[u8] = b"userspace init: probe service exhausted restart budget\n";
const SERVICE_BOOTSTRAP_FAILED: &[u8] = b"userspace init: failed to grant service endpoint\n";
const SERVICE_PROTOCOL_FAILED: &[u8] = b"userspace init: invalid service readiness message\n";
const SHELL_COMMAND: &[u8] = b"/ush";
const SHELL_LAUNCHED: &[u8] = b"userspace init launched /ush\n";
const SHELL_RESTARTING: &[u8] = b"userspace init: /ush exited; restarting\n";
const WRONG_PROCESS_ID: &[u8] = b"userspace init: expected process id 1\n";
const SHELL_SPAWN_FAILED: &[u8] = b"userspace init: failed to launch /ush\n";
const SHELL_WAIT_FAILED: &[u8] = b"userspace init: failed while waiting for /ush\n";
const SHELL_FOREGROUND_FAILED: &[u8] =
    b"userspace init: failed to restore /ush to the foreground\n";

const PROBE_SERVICE: ServiceSpec = ServiceSpec {
    name: b"probe",
    command: SERVICE_COMMAND,
    ready_message: SERVICE_READY_MESSAGE,
    bootstrap_handle: 1,
    restart_limit: 3,
    restart_backoff_yields: 32,
};

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    if syscall::getpid() != Ok(INIT_PROCESS_ID) {
        fail(WRONG_PROCESS_ID);
    }
    if syscall::write_all(STDOUT, INIT_READY).is_err() {
        syscall::exit(1);
    }

    let readiness_endpoint = match ipc::endpoint_create() {
        Ok(endpoint) => endpoint,
        Err(_) => fail(SERVICE_BOOTSTRAP_FAILED),
    };
    let mut service = ServiceRuntime::new(PROBE_SERVICE);
    start_service(&mut service, readiness_endpoint);
    let mut shell_process_id = spawn_shell();

    loop {
        if let Some(service_process_id) = service.process_id() {
            match syscall::try_wait_child(service_process_id) {
                Ok(status) => match service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        let _ = syscall::write_all(STDOUT, SERVICE_RESTARTING);
                        backoff(backoff_yields);
                        start_service(&mut service, readiness_endpoint);
                    }
                    ServiceStatusDisposition::Failed => fail(SERVICE_FAILED),
                },
                Err(error) if error == syscall::Errno::TRY_AGAIN => {}
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(_) => fail(SERVICE_FAILED),
            }
        }

        match syscall::try_wait_child(shell_process_id) {
            Ok(status) => match shell_status_disposition(status.raw()) {
                ShellStatusDisposition::WaitForNextEvent => {}
                ShellStatusDisposition::RestoreForeground => {
                    if syscall::foreground_process_group(shell_process_id).is_err() {
                        fail(SHELL_FOREGROUND_FAILED);
                    }
                }
                ShellStatusDisposition::RestartShell => {
                    let _ = syscall::write_all(STDOUT, SHELL_RESTARTING);
                    shell_process_id = spawn_shell();
                }
            },
            Err(error) if error == syscall::Errno::TRY_AGAIN => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(SHELL_WAIT_FAILED),
        }

        if syscall::yield_now().is_err() {
            fail(SHELL_WAIT_FAILED);
        }
    }
}

fn start_service(service: &mut ServiceRuntime, readiness_endpoint: CapabilityHandle) {
    loop {
        let spec = service.spec();
        let _ = syscall::write_all(STDOUT, SERVICE_STARTING);
        let process_id = match syscall::spawn_command(
            spec.command,
            SpawnFlags::NEW_PROCESS_GROUP,
            None,
            None,
            None,
            None,
        ) {
            Ok(process_id) => process_id,
            Err(_) => fail(SERVICE_FAILED),
        };
        service.note_spawned(process_id);
        if ipc::grant_child(
            process_id,
            readiness_endpoint,
            Rights::SEND,
            spec.bootstrap_handle,
        )
        .ok()
            != Some(spec.bootstrap_handle)
        {
            fail(SERVICE_BOOTSTRAP_FAILED);
        }

        let mut ready_buffer = [0_u8; 64];
        loop {
            match ipc::try_receive(readiness_endpoint, &mut ready_buffer) {
                Ok(message) => {
                    if message.sender_process_id != process_id
                        || message.capability.is_some()
                        || message.bytes != spec.ready_message.len()
                        || &ready_buffer[..message.bytes] != spec.ready_message
                    {
                        fail(SERVICE_PROTOCOL_FAILED);
                    }
                    service.note_ready();
                    let _ = syscall::write_all(STDOUT, SERVICE_READY);
                    return;
                }
                Err(error) if error == ipc::Error::TRY_AGAIN => {}
                Err(_) => fail(SERVICE_PROTOCOL_FAILED),
            }

            match syscall::try_wait_child(process_id) {
                Ok(status) => match service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        let _ = syscall::write_all(STDOUT, SERVICE_RESTARTING);
                        backoff(backoff_yields);
                        break;
                    }
                    ServiceStatusDisposition::Failed => fail(SERVICE_FAILED),
                },
                Err(error) if error == syscall::Errno::TRY_AGAIN => {}
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(_) => fail(SERVICE_FAILED),
            }

            if syscall::yield_now().is_err() {
                fail(SERVICE_FAILED);
            }
        }
    }
}

fn spawn_shell() -> ProcessId {
    let shell_process_id = match syscall::spawn_command(
        SHELL_COMMAND,
        SpawnFlags::FOREGROUND | SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
    ) {
        Ok(process_id) => process_id,
        Err(_) => fail(SHELL_SPAWN_FAILED),
    };
    let _ = syscall::write_all(STDOUT, SHELL_LAUNCHED);
    shell_process_id
}

fn backoff(yields: u32) {
    for _ in 0..yields {
        if syscall::yield_now().is_err() {
            fail(SERVICE_FAILED);
        }
    }
}

fn fail(message: &[u8]) -> ! {
    let _ = syscall::write_all(STDERR, message);
    syscall::exit(1)
}
