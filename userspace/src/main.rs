#![no_std]
#![no_main]

use userspace::{
    abi::INIT_PROCESS_ID,
    ipc::{self, CapabilityHandle, Rights},
    platform,
    supervisor::{
        ServiceRuntime, ServiceSpec, ServiceStatusDisposition, ShellStatusDisposition,
        shell_status_disposition,
    },
    syscall::{self, ProcessId, STDERR, STDOUT, SpawnFlags},
    tmpfs::Mount,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const INIT_READY: &[u8] = b"userspace init ready: pid=1\n";
const SERVICE_COMMAND: &[u8] = b"/tmpfs-service";
const SERVICE_READY_MESSAGE: &[u8] = b"service-ready: tmpfs";
const SERVICE_STARTING: &[u8] = b"userspace init: starting tmpfs service\n";
const SERVICE_RESTARTING: &[u8] = b"userspace init: tmpfs service exited; restarting\n";
const SERVICE_READY: &[u8] = b"userspace init: tmpfs service ready\n";
const SERVICE_FAILED: &[u8] = b"userspace init: tmpfs service exhausted restart budget\n";
const SERVICE_BOOTSTRAP_FAILED: &[u8] = b"userspace init: failed to grant tmpfs capabilities\n";
const SERVICE_PROTOCOL_FAILED: &[u8] = b"userspace init: invalid tmpfs readiness message\n";
const TMPFS_PROBE_COMMAND: &[u8] = b"/tmpfs-probe";
const TMPFS_PROBE_FAILED: &[u8] = b"userspace init: userspace tmpfs probe failed\n";
const TMPFS_PROBE_PASSED: &[u8] = b"userspace init: userspace tmpfs probe passed\n";
const SHELL_COMMAND: &[u8] = b"/ush";
const SHELL_LAUNCHED: &[u8] = b"userspace init launched /ush\n";
const SHELL_RESTARTING: &[u8] = b"userspace init: /ush exited; restarting\n";
const WRONG_PROCESS_ID: &[u8] = b"userspace init: expected process id 1\n";
const SHELL_SPAWN_FAILED: &[u8] = b"userspace init: failed to launch /ush\n";
const SHELL_WAIT_FAILED: &[u8] = b"userspace init: failed while waiting for /ush\n";
const SHELL_FOREGROUND_FAILED: &[u8] =
    b"userspace init: failed to restore /ush to the foreground\n";

const READY_HANDLE: u64 = 1;
const REQUEST_HANDLE: u64 = 2;

const TMPFS_SERVICE: ServiceSpec = ServiceSpec {
    name: b"tmpfs",
    command: SERVICE_COMMAND,
    ready_message: SERVICE_READY_MESSAGE,
    bootstrap_handle: READY_HANDLE,
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

    let readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
    let request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
    let mut service = ServiceRuntime::new(TMPFS_SERVICE);
    start_service(&mut service, readiness_endpoint, request_endpoint);
    register_tmpfs_proxy(request_endpoint);
    run_tmpfs_probe(request_endpoint);
    let mut shell_process_id = spawn_shell();

    loop {
        if let Some(service_process_id) = service.process_id() {
            match syscall::try_wait_child(service_process_id) {
                Ok(status) => match service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        let _ = syscall::write_all(STDOUT, SERVICE_RESTARTING);
                        backoff(backoff_yields);
                        start_service(&mut service, readiness_endpoint, request_endpoint);
                        register_tmpfs_proxy(request_endpoint);
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

fn register_tmpfs_proxy(request_endpoint: CapabilityHandle) {
    let mount = Mount::connect(request_endpoint).unwrap_or_else(|_| fail(SERVICE_PROTOCOL_FAILED));
    let generation = mount.generation();
    mount
        .disconnect()
        .unwrap_or_else(|_| fail(SERVICE_PROTOCOL_FAILED));
    platform::register_tmpfs_service(request_endpoint, generation)
        .unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
}

fn start_service(
    service: &mut ServiceRuntime,
    readiness_endpoint: CapabilityHandle,
    request_endpoint: CapabilityHandle,
) {
    loop {
        let spec = service.spec();
        let _ = syscall::write_all(STDOUT, SERVICE_STARTING);
        let barrier = syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(SERVICE_FAILED));
        let process_id = syscall::spawn_command_with_barrier(
            spec.command,
            SpawnFlags::NEW_PROCESS_GROUP,
            None,
            None,
            None,
            None,
            &barrier,
        )
        .unwrap_or_else(|_| fail(SERVICE_FAILED));
        service.note_spawned(process_id);
        if ipc::grant_child(process_id, readiness_endpoint, Rights::SEND, READY_HANDLE).ok()
            != Some(READY_HANDLE)
            || ipc::grant_child(
                process_id,
                request_endpoint,
                Rights::RECEIVE,
                REQUEST_HANDLE,
            )
            .ok()
                != Some(REQUEST_HANDLE)
        {
            fail(SERVICE_BOOTSTRAP_FAILED);
        }
        barrier
            .release()
            .unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));

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
            let _ = syscall::yield_now();
        }
    }
}

fn run_tmpfs_probe(request_endpoint: CapabilityHandle) {
    let barrier = syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(TMPFS_PROBE_FAILED));
    let process_id = syscall::spawn_command_with_barrier(
        TMPFS_PROBE_COMMAND,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &barrier,
    )
    .unwrap_or_else(|_| fail(TMPFS_PROBE_FAILED));
    if ipc::grant_child(process_id, request_endpoint, Rights::SEND, 1).ok() != Some(1) {
        fail(TMPFS_PROBE_FAILED);
    }
    barrier
        .release()
        .unwrap_or_else(|_| fail(TMPFS_PROBE_FAILED));
    loop {
        match syscall::wait_child(process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) if status.success() => break,
            Ok(_) => fail(TMPFS_PROBE_FAILED),
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(TMPFS_PROBE_FAILED),
        }
    }
    let _ = syscall::write_all(STDOUT, TMPFS_PROBE_PASSED);
}

fn spawn_shell() -> ProcessId {
    let process_id = syscall::spawn_command(
        SHELL_COMMAND,
        SpawnFlags::FOREGROUND | SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
    )
    .unwrap_or_else(|_| fail(SHELL_SPAWN_FAILED));
    let _ = syscall::write_all(STDOUT, SHELL_LAUNCHED);
    process_id
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
