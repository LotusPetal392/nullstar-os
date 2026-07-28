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
const BLOCK_DEVICE_PROBE_COMMAND: &[u8] = b"/block-device-probe";
const BLOCK_DEVICE_PROBE_FAILED: &[u8] = b"userspace init: read-only block-device probe failed\n";
const BLOCK_DEVICE_PROBE_PASSED: &[u8] = b"userspace init: read-only block-device probe passed\n";
const NULLFS_BLOCK_DEVICE_PROBE_COMMAND: &[u8] = b"/block-device-probe nullfs";
const NULLFS_BLOCK_DEVICE_PROBE_FAILED: &[u8] =
    b"userspace init: read-only NullFS partition probe failed\n";
const NULLFS_BLOCK_DEVICE_PROBE_PASSED: &[u8] =
    b"userspace init: read-only NullFS partition probe passed\n";
const BLOCK_DEVICE_BOOTSTRAP_FAILED: &[u8] =
    b"userspace init: failed to acquire block-device endpoint\n";
const NULLFS_SERVICE_COMMAND: &[u8] = b"/nullfs-service";
const NULLFS_SERVICE_READY_MESSAGE: &[u8] = b"service-ready: nullfs";
const NULLFS_SERVICE_STARTING: &[u8] = b"userspace init: starting NullFS service\n";
const NULLFS_SERVICE_RESTARTING: &[u8] = b"userspace init: NullFS service exited; restarting\n";
const NULLFS_SERVICE_READY: &[u8] = b"userspace init: read-only NullFS service mounted\n";
const NULLFS_SERVICE_FAILED: &[u8] = b"userspace init: NullFS service exhausted restart budget\n";
const NULLFS_SERVICE_BOOTSTRAP_FAILED: &[u8] =
    b"userspace init: failed to grant NullFS capabilities\n";
const NULLFS_SERVICE_PROTOCOL_FAILED: &[u8] = b"userspace init: invalid NullFS readiness message\n";
const NULLFS_PROBE_COMMAND: &[u8] = b"/nullfs-probe";
const NULLFS_PROBE_FAILED: &[u8] = b"userspace init: userspace NullFS probe failed\n";
const NULLFS_PROBE_PASSED: &[u8] = b"userspace init: userspace NullFS probe passed\n";
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
const VFS_SERVICE_COMMAND: &[u8] = b"/vfs-service";
const VFS_SERVICE_READY_MESSAGE: &[u8] = b"service-ready: vfs";
const VFS_SERVICE_STARTING: &[u8] = b"userspace init: starting vfs service\n";
const VFS_SERVICE_RESTARTING: &[u8] = b"userspace init: vfs service exited; restarting\n";
const VFS_SERVICE_READY: &[u8] = b"userspace init: vfs service ready\n";
const VFS_SERVICE_FAILED: &[u8] = b"userspace init: vfs service exhausted restart budget\n";
const VFS_SERVICE_BOOTSTRAP_FAILED: &[u8] = b"userspace init: failed to grant vfs capabilities\n";
const VFS_SERVICE_PROTOCOL_FAILED: &[u8] = b"userspace init: invalid vfs readiness message\n";
const VFS_PROBE_COMMAND: &[u8] = b"/vfs-probe";
const VFS_PROBE_FAILED: &[u8] = b"userspace init: userspace vfs probe failed\n";
const VFS_PROBE_PASSED: &[u8] = b"userspace init: userspace vfs probe passed\n";
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
const NULLFS_BLOCK_HANDLE: u64 = 3;

const NULLFS_SERVICE: ServiceSpec = ServiceSpec {
    name: b"nullfs",
    command: NULLFS_SERVICE_COMMAND,
    ready_message: NULLFS_SERVICE_READY_MESSAGE,
    bootstrap_handle: READY_HANDLE,
    restart_limit: 3,
    restart_backoff_yields: 32,
};
const TMPFS_SERVICE: ServiceSpec = ServiceSpec {
    name: b"tmpfs",
    command: SERVICE_COMMAND,
    ready_message: SERVICE_READY_MESSAGE,
    bootstrap_handle: READY_HANDLE,
    restart_limit: 3,
    restart_backoff_yields: 32,
};
const VFS_SERVICE: ServiceSpec = ServiceSpec {
    name: b"vfs",
    command: VFS_SERVICE_COMMAND,
    ready_message: VFS_SERVICE_READY_MESSAGE,
    bootstrap_handle: READY_HANDLE,
    restart_limit: 3,
    restart_backoff_yields: 32,
};

struct ServiceMessages {
    starting: &'static [u8],
    restarting: &'static [u8],
    ready: &'static [u8],
    failed: &'static [u8],
    bootstrap_failed: &'static [u8],
    protocol_failed: &'static [u8],
}

#[derive(Clone, Copy)]
struct BootstrapCapability {
    source_handle: CapabilityHandle,
    rights: Rights,
    target_handle: CapabilityHandle,
}

const NULLFS_MESSAGES: ServiceMessages = ServiceMessages {
    starting: NULLFS_SERVICE_STARTING,
    restarting: NULLFS_SERVICE_RESTARTING,
    ready: NULLFS_SERVICE_READY,
    failed: NULLFS_SERVICE_FAILED,
    bootstrap_failed: NULLFS_SERVICE_BOOTSTRAP_FAILED,
    protocol_failed: NULLFS_SERVICE_PROTOCOL_FAILED,
};
const TMPFS_MESSAGES: ServiceMessages = ServiceMessages {
    starting: SERVICE_STARTING,
    restarting: SERVICE_RESTARTING,
    ready: SERVICE_READY,
    failed: SERVICE_FAILED,
    bootstrap_failed: SERVICE_BOOTSTRAP_FAILED,
    protocol_failed: SERVICE_PROTOCOL_FAILED,
};
const VFS_MESSAGES: ServiceMessages = ServiceMessages {
    starting: VFS_SERVICE_STARTING,
    restarting: VFS_SERVICE_RESTARTING,
    ready: VFS_SERVICE_READY,
    failed: VFS_SERVICE_FAILED,
    bootstrap_failed: VFS_SERVICE_BOOTSTRAP_FAILED,
    protocol_failed: VFS_SERVICE_PROTOCOL_FAILED,
};

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    if syscall::getpid() != Ok(INIT_PROCESS_ID) {
        fail(WRONG_PROCESS_ID);
    }
    if syscall::write_all(STDOUT, INIT_READY).is_err() {
        syscall::exit(1);
    }

    let block_device_endpoint = platform::open_block_device_endpoint(2)
        .unwrap_or_else(|_| fail(BLOCK_DEVICE_BOOTSTRAP_FAILED));
    if !matches!(
        ipc::info(block_device_endpoint),
        Ok(info)
            if info.kind == ipc::ObjectKind::Endpoint
                && info.rights == (Rights::SEND | Rights::TRANSFER)
    ) {
        fail(BLOCK_DEVICE_BOOTSTRAP_FAILED);
    }
    run_probe(
        BLOCK_DEVICE_PROBE_COMMAND,
        block_device_endpoint,
        BLOCK_DEVICE_PROBE_FAILED,
        BLOCK_DEVICE_PROBE_PASSED,
    );
    ipc::close(block_device_endpoint).unwrap_or_else(|_| fail(BLOCK_DEVICE_BOOTSTRAP_FAILED));

    let nullfs_block_device_endpoint = platform::open_block_device_endpoint(3)
        .unwrap_or_else(|_| fail(BLOCK_DEVICE_BOOTSTRAP_FAILED));
    if !matches!(
        ipc::info(nullfs_block_device_endpoint),
        Ok(info)
            if info.kind == ipc::ObjectKind::Endpoint
                && info.rights == (Rights::SEND | Rights::TRANSFER)
    ) {
        fail(BLOCK_DEVICE_BOOTSTRAP_FAILED);
    }
    run_probe(
        NULLFS_BLOCK_DEVICE_PROBE_COMMAND,
        nullfs_block_device_endpoint,
        NULLFS_BLOCK_DEVICE_PROBE_FAILED,
        NULLFS_BLOCK_DEVICE_PROBE_PASSED,
    );
    ipc::close(nullfs_block_device_endpoint)
        .unwrap_or_else(|_| fail(BLOCK_DEVICE_BOOTSTRAP_FAILED));

    let nullfs_service_block_endpoint = platform::open_block_device_endpoint(3)
        .unwrap_or_else(|_| fail(NULLFS_SERVICE_BOOTSTRAP_FAILED));
    if !matches!(
        ipc::info(nullfs_service_block_endpoint),
        Ok(info)
            if info.kind == ipc::ObjectKind::Endpoint
                && info.rights == (Rights::SEND | Rights::TRANSFER)
    ) {
        fail(NULLFS_SERVICE_BOOTSTRAP_FAILED);
    }
    let nullfs_readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_SERVICE_BOOTSTRAP_FAILED));
    let nullfs_request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_SERVICE_BOOTSTRAP_FAILED));
    let mut nullfs_service = ServiceRuntime::new(NULLFS_SERVICE);
    let nullfs_block_capability = BootstrapCapability {
        source_handle: nullfs_service_block_endpoint,
        rights: Rights::SEND,
        target_handle: NULLFS_BLOCK_HANDLE,
    };
    start_service(
        &mut nullfs_service,
        nullfs_readiness_endpoint,
        nullfs_request_endpoint,
        Some(nullfs_block_capability),
        &NULLFS_MESSAGES,
    );
    run_probe(
        NULLFS_PROBE_COMMAND,
        nullfs_request_endpoint,
        NULLFS_PROBE_FAILED,
        NULLFS_PROBE_PASSED,
    );
    register_nullfs_proxy(&nullfs_service, nullfs_request_endpoint);

    let readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
    let request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
    let mut service = ServiceRuntime::new(TMPFS_SERVICE);
    start_service(
        &mut service,
        readiness_endpoint,
        request_endpoint,
        None,
        &TMPFS_MESSAGES,
    );
    register_tmpfs_proxy(request_endpoint);
    run_tmpfs_probe(request_endpoint);
    let vfs_readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
    let vfs_request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
    let mut vfs_service = ServiceRuntime::new(VFS_SERVICE);
    start_service(
        &mut vfs_service,
        vfs_readiness_endpoint,
        vfs_request_endpoint,
        None,
        &VFS_MESSAGES,
    );
    register_vfs_router(&vfs_service, vfs_request_endpoint);
    run_probe(
        VFS_PROBE_COMMAND,
        vfs_request_endpoint,
        VFS_PROBE_FAILED,
        VFS_PROBE_PASSED,
    );
    let mut shell_process_id = spawn_shell();

    loop {
        if let Some(service_process_id) = nullfs_service.process_id() {
            match syscall::try_wait_child(service_process_id) {
                Ok(status) => match nullfs_service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        let _ = syscall::write_all(STDOUT, NULLFS_SERVICE_RESTARTING);
                        backoff(backoff_yields);
                        start_service(
                            &mut nullfs_service,
                            nullfs_readiness_endpoint,
                            nullfs_request_endpoint,
                            Some(nullfs_block_capability),
                            &NULLFS_MESSAGES,
                        );
                        register_nullfs_proxy(&nullfs_service, nullfs_request_endpoint);
                    }
                    ServiceStatusDisposition::Failed => fail(NULLFS_SERVICE_FAILED),
                },
                Err(error) if error == syscall::Errno::TRY_AGAIN => {}
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(_) => fail(NULLFS_SERVICE_FAILED),
            }
        }

        if let Some(service_process_id) = service.process_id() {
            match syscall::try_wait_child(service_process_id) {
                Ok(status) => match service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        let _ = syscall::write_all(STDOUT, SERVICE_RESTARTING);
                        backoff(backoff_yields);
                        start_service(
                            &mut service,
                            readiness_endpoint,
                            request_endpoint,
                            None,
                            &TMPFS_MESSAGES,
                        );
                        register_tmpfs_proxy(request_endpoint);
                    }
                    ServiceStatusDisposition::Failed => fail(SERVICE_FAILED),
                },
                Err(error) if error == syscall::Errno::TRY_AGAIN => {}
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(_) => fail(SERVICE_FAILED),
            }
        }

        if let Some(service_process_id) = vfs_service.process_id() {
            match syscall::try_wait_child(service_process_id) {
                Ok(status) => match vfs_service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        let _ = syscall::write_all(STDOUT, VFS_SERVICE_RESTARTING);
                        backoff(backoff_yields);
                        start_service(
                            &mut vfs_service,
                            vfs_readiness_endpoint,
                            vfs_request_endpoint,
                            None,
                            &VFS_MESSAGES,
                        );
                        register_vfs_router(&vfs_service, vfs_request_endpoint);
                    }
                    ServiceStatusDisposition::Failed => fail(VFS_SERVICE_FAILED),
                },
                Err(error) if error == syscall::Errno::TRY_AGAIN => {}
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(_) => fail(VFS_SERVICE_FAILED),
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

fn register_nullfs_proxy(service: &ServiceRuntime, request_endpoint: CapabilityHandle) {
    let generation = service
        .process_id()
        .and_then(|process_id| u32::try_from(process_id).ok())
        .filter(|generation| *generation != 0)
        .unwrap_or_else(|| fail(NULLFS_SERVICE_PROTOCOL_FAILED));
    for _ in 0..8 {
        match platform::register_nullfs_service(request_endpoint, generation) {
            Ok(()) => return,
            Err(error) if error == platform::Errno::TRY_AGAIN => {
                syscall::yield_now().unwrap_or_else(|_| fail(NULLFS_SERVICE_BOOTSTRAP_FAILED));
            }
            Err(_) => fail(NULLFS_SERVICE_BOOTSTRAP_FAILED),
        }
    }
    fail(NULLFS_SERVICE_BOOTSTRAP_FAILED)
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

fn register_vfs_router(service: &ServiceRuntime, request_endpoint: CapabilityHandle) {
    let generation = service
        .process_id()
        .and_then(|process_id| u32::try_from(process_id).ok())
        .filter(|generation| *generation != 0)
        .unwrap_or_else(|| fail(VFS_SERVICE_PROTOCOL_FAILED));
    platform::register_vfs_service(request_endpoint, generation)
        .unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
}

fn start_service(
    service: &mut ServiceRuntime,
    readiness_endpoint: CapabilityHandle,
    request_endpoint: CapabilityHandle,
    additional_capability: Option<BootstrapCapability>,
    messages: &ServiceMessages,
) {
    loop {
        let spec = service.spec();
        let _ = syscall::write_all(STDOUT, messages.starting);
        let barrier = syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(messages.failed));
        let process_id = syscall::spawn_command_with_barrier(
            spec.command,
            SpawnFlags::NEW_PROCESS_GROUP,
            None,
            None,
            None,
            None,
            &barrier,
        )
        .unwrap_or_else(|_| fail(messages.failed));
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
            || additional_capability.is_some_and(|capability| {
                ipc::grant_child(
                    process_id,
                    capability.source_handle,
                    capability.rights,
                    capability.target_handle,
                )
                .ok()
                    != Some(capability.target_handle)
            })
        {
            fail(messages.bootstrap_failed);
        }
        barrier
            .release()
            .unwrap_or_else(|_| fail(messages.bootstrap_failed));

        let mut ready_buffer = [0_u8; 64];
        loop {
            match ipc::try_receive(readiness_endpoint, &mut ready_buffer) {
                Ok(message) => {
                    if message.sender_process_id != process_id
                        || message.capability.is_some()
                        || message.bytes != spec.ready_message.len()
                        || &ready_buffer[..message.bytes] != spec.ready_message
                    {
                        fail(messages.protocol_failed);
                    }
                    service.note_ready();
                    let _ = syscall::write_all(STDOUT, messages.ready);
                    return;
                }
                Err(error) if error == ipc::Error::TRY_AGAIN => {}
                Err(_) => fail(messages.protocol_failed),
            }

            match syscall::try_wait_child(process_id) {
                Ok(status) => match service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        let _ = syscall::write_all(STDOUT, messages.restarting);
                        backoff(backoff_yields);
                        break;
                    }
                    ServiceStatusDisposition::Failed => fail(messages.failed),
                },
                Err(error) if error == syscall::Errno::TRY_AGAIN => {}
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(_) => fail(messages.failed),
            }
            let _ = syscall::yield_now();
        }
    }
}

fn run_tmpfs_probe(request_endpoint: CapabilityHandle) {
    run_probe(
        TMPFS_PROBE_COMMAND,
        request_endpoint,
        TMPFS_PROBE_FAILED,
        TMPFS_PROBE_PASSED,
    );
}

fn run_probe(
    command: &[u8],
    request_endpoint: CapabilityHandle,
    failed_message: &[u8],
    passed_message: &[u8],
) {
    let barrier = syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(failed_message));
    let process_id = syscall::spawn_command_with_barrier(
        command,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &barrier,
    )
    .unwrap_or_else(|_| fail(failed_message));
    if ipc::grant_child(process_id, request_endpoint, Rights::SEND, 1).ok() != Some(1) {
        fail(failed_message);
    }
    barrier.release().unwrap_or_else(|_| fail(failed_message));
    loop {
        match syscall::wait_child(process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) if status.success() => break,
            Ok(_) => fail(failed_message),
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(failed_message),
        }
    }
    let _ = syscall::write_all(STDOUT, passed_message);
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
