#![no_std]
#![no_main]

use userspace::{
    abi::{INIT_PROCESS_ID, signal},
    ipc::{self, CapabilityHandle, Rights},
    nullfs_primary_volume, platform,
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
const LOGGING_SERVICE_COMMAND: &[u8] = b"/logging-service";
const LOGGING_SERVICE_READY_MESSAGE: &[u8] = b"service-ready: logging";
const LOGGING_SERVICE_STARTING: &[u8] = b"userspace init: starting logging service\n";
const LOGGING_SERVICE_RESTARTING: &[u8] = b"userspace init: logging service exited; restarting\n";
const LOGGING_SERVICE_READY: &[u8] = b"userspace init: logging service ready\n";
const LOGGING_SERVICE_FAILED: &[u8] = b"userspace init: logging service exhausted restart budget\n";
const LOGGING_SERVICE_BOOTSTRAP_FAILED: &[u8] =
    b"userspace init: failed to grant logging capabilities\n";
const LOGGING_SERVICE_PROTOCOL_FAILED: &[u8] =
    b"userspace init: invalid logging readiness message\n";
const LOGGING_PROBE_COMMAND: &[u8] = b"/logging-probe";
const LOGGING_STRESS_PROBE_COMMAND: &[u8] = b"/logging-probe collector-stress";
const LOGGING_RESTART_PROBE_COMMAND: &[u8] = b"/logging-probe after-restart";
const LOGGING_PROBE_BOUND: &[u8] = b"logging-probe: bound";
const LOGGING_PROBE_FILL_QUEUE: &[u8] = b"logging-probe: fill queue";
const LOGGING_PROBE_BACKPRESSURE: &[u8] = b"logging-probe: backpressure verified";
const LOGGING_PROBE_MAX_YIELDS: u32 = 65_536;
const LOGGING_PROBE_FAILED: &[u8] = b"userspace init: native NSWP logging probe failed\n";
const LOGGING_PROBE_PASSED: &[u8] = b"userspace init: native NSWP logging probe passed\n";
const LOGGING_COLLECTOR_TEST_PASSED: &[u8] =
    b"userspace init: logging collector ring, backpressure, redaction, and restart verified\n";
const LOGGING_COLLECTOR_EXIT_WAIT_FAILED: &[u8] =
    b"userspace init: logging collector service exit wait failed\n";
const LOGGING_COLLECTOR_EXIT_NO_CHILD: &[u8] =
    b"userspace init: logging collector exit reported no child\n";
const LOGGING_COLLECTOR_RESTART_POLICY_FAILED: &[u8] =
    b"userspace init: logging collector restart policy failed\n";
const BLOCK_DEVICE_PROBE_COMMAND: &[u8] = b"/block-device-probe";
const BLOCK_DEVICE_PROBE_FAILED: &[u8] = b"userspace init: read-only block-device probe failed\n";
const BLOCK_DEVICE_PROBE_PASSED: &[u8] = b"userspace init: read-only block-device probe passed\n";

const WRITABLE_NULLFS_BLOCK_DEVICE_PROBE_COMMAND: &[u8] = b"/block-device-probe nullfs-writable";
const WRITABLE_NULLFS_BLOCK_DEVICE_PROBE_FAILED: &[u8] =
    b"userspace init: writable NullFS partition probe failed\n";
const WRITABLE_NULLFS_BLOCK_DEVICE_PROBE_PASSED: &[u8] =
    b"userspace init: writable NullFS partition probe passed\n";
const BLOCK_DEVICE_BOOTSTRAP_FAILED: &[u8] =
    b"userspace init: failed to acquire block-device endpoint\n";
const NULLFS_SERVICE_COMMAND: &[u8] = b"/nullfs-service --writable";
const NULLFS_SERVICE_READY_MESSAGE: &[u8] = b"service-ready: nullfs";
const NULLFS_SERVICE_STARTING: &[u8] = b"userspace init: starting NullFS service\n";
const NULLFS_SERVICE_RESTARTING: &[u8] = b"userspace init: NullFS service exited; restarting\n";
const NULLFS_SERVICE_READY: &[u8] = b"userspace init: writable NullFS service ready\n";
const NULLFS_SERVICE_FAILED: &[u8] = b"userspace init: NullFS service exhausted restart budget\n";
const NULLFS_SERVICE_BOOTSTRAP_FAILED: &[u8] =
    b"userspace init: failed to grant NullFS capabilities\n";
const NULLFS_SERVICE_PROTOCOL_FAILED: &[u8] = b"userspace init: invalid NullFS readiness message\n";
const NULLFS_READINESS_PROBE_COMMAND: &[u8] = b"/nullfs-probe readiness";
const NULLFS_READINESS_PROBE_FAILED: &[u8] = b"userspace init: NullFS readiness failed\n";
const NULLFS_READINESS_PROBE_PASSED: &[u8] = b"userspace init: NullFS readiness passed\n";
const NULLFS_FULL_PROBE_COMMAND: &[u8] = b"/nullfs-probe full";
const NULLFS_FULL_PROBE_FAILED: &[u8] = b"userspace init: full NullFS probe failed\n";
const NULLFS_FULL_PROBE_PASSED: &[u8] = b"userspace init: full NullFS probe passed\n";
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
const VFS_READINESS_PROBE_COMMAND: &[u8] = b"/vfs-probe readiness";
const VFS_READINESS_PROBE_FAILED: &[u8] = b"userspace init: vfs readiness failed\n";
const VFS_READINESS_PROBE_PASSED: &[u8] = b"userspace init: vfs readiness passed\n";
const VFS_FULL_PROBE_COMMAND: &[u8] = b"/vfs-probe full";
const VFS_FULL_PROBE_FAILED: &[u8] = b"userspace init: full vfs probe failed\n";
const VFS_FULL_PROBE_PASSED: &[u8] = b"userspace init: full vfs probe passed\n";
const NULLFS_RESTART_PROBE_COMMAND: &[u8] = b"/vfs-probe nullfs-restart";
const NULLFS_RESTART_PROBE_READY: &[u8] =
    b"nullfs-restart: live descriptor and persistent mutation ready";
const NULLFS_RESTART_PROBE_BEGIN_READ: &[u8] = b"nullfs-restart: begin stale read";
const NULLFS_RESTART_PROBE_PASSED: &[u8] =
    b"userspace init: NullFS restart persistent VFS mutation and stale descriptors verified\n";
const NULLFS_RESTART_PROBE_FAILED: &[u8] = b"userspace init: NullFS restart probe failed\n";
const BOOT_MODE_PATH: &[u8] = b"/BOOTMODE";
const NULLFS_RESTART_TEST_BOOT_MODE: &[u8] = b"nullfs-restart-test\n";
const BOOT_MODE_PROBE_FAILED: &[u8] = b"userspace init: unable to read boot mode\n";
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
const LOGGING_RESPONSE_HANDLE: u64 = 3;
const LOGGING_PROBE_STATUS_HANDLE: u64 = 3;
const LOGGING_PROBE_CONTROL_HANDLE: u64 = 4;

const LOGGING_SERVICE: ServiceSpec = ServiceSpec {
    name: b"logging",
    command: LOGGING_SERVICE_COMMAND,
    ready_message: LOGGING_SERVICE_READY_MESSAGE,
    bootstrap_handle: READY_HANDLE,
    restart_limit: 3,
    restart_backoff_yields: 32,
};
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

const LOGGING_MESSAGES: ServiceMessages = ServiceMessages {
    starting: LOGGING_SERVICE_STARTING,
    restarting: LOGGING_SERVICE_RESTARTING,
    ready: LOGGING_SERVICE_READY,
    failed: LOGGING_SERVICE_FAILED,
    bootstrap_failed: LOGGING_SERVICE_BOOTSTRAP_FAILED,
    protocol_failed: LOGGING_SERVICE_PROTOCOL_FAILED,
};
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
    let nullfs_restart_test = nullfs_restart_test_boot();

    let logging_readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
    let logging_request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
    let logging_response_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
    let mut logging_service = ServiceRuntime::new(LOGGING_SERVICE);
    let logging_response_capability = BootstrapCapability {
        source_handle: logging_response_endpoint,
        rights: Rights::SEND,
        target_handle: LOGGING_RESPONSE_HANDLE,
    };
    start_service(
        &mut logging_service,
        logging_readiness_endpoint,
        logging_request_endpoint,
        Some(logging_response_capability),
        &LOGGING_MESSAGES,
    );
    if nullfs_restart_test {
        run_logging_collector_restart_test(
            &mut logging_service,
            logging_readiness_endpoint,
            logging_request_endpoint,
            logging_response_endpoint,
            logging_response_capability,
        );
    } else {
        run_logging_probe(
            &logging_service,
            logging_request_endpoint,
            logging_response_endpoint,
            LOGGING_PROBE_COMMAND,
            LOGGING_PROBE_PASSED,
        );
    }

    let mut missing_nullfs_uuid = nullfs_primary_volume::FILESYSTEM_UUID;
    missing_nullfs_uuid[15] ^= 0xff;
    if platform::open_writable_nullfs_block_device_endpoint(&missing_nullfs_uuid).err()
        != Some(platform::Errno::NO_ENTRY)
    {
        fail(BLOCK_DEVICE_BOOTSTRAP_FAILED);
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

    let writable_nullfs_block_device_endpoint =
        platform::open_writable_nullfs_block_device_endpoint(
            &nullfs_primary_volume::FILESYSTEM_UUID,
        )
        .unwrap_or_else(|_| fail(BLOCK_DEVICE_BOOTSTRAP_FAILED));
    if !matches!(
        ipc::info(writable_nullfs_block_device_endpoint),
        Ok(info)
            if info.kind == ipc::ObjectKind::Endpoint
                && info.rights == (Rights::SEND | Rights::TRANSFER)
    ) {
        fail(BLOCK_DEVICE_BOOTSTRAP_FAILED);
    }
    run_probe(
        WRITABLE_NULLFS_BLOCK_DEVICE_PROBE_COMMAND,
        writable_nullfs_block_device_endpoint,
        WRITABLE_NULLFS_BLOCK_DEVICE_PROBE_FAILED,
        WRITABLE_NULLFS_BLOCK_DEVICE_PROBE_PASSED,
    );
    ipc::close(writable_nullfs_block_device_endpoint)
        .unwrap_or_else(|_| fail(BLOCK_DEVICE_BOOTSTRAP_FAILED));

    let nullfs_service_block_endpoint = platform::open_writable_nullfs_block_device_endpoint(
        &nullfs_primary_volume::FILESYSTEM_UUID,
    )
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
    let mut nullfs_request_endpoint =
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
        NULLFS_READINESS_PROBE_COMMAND,
        nullfs_request_endpoint,
        NULLFS_READINESS_PROBE_FAILED,
        NULLFS_READINESS_PROBE_PASSED,
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
    if nullfs_restart_test {
        run_probe(
            NULLFS_FULL_PROBE_COMMAND,
            nullfs_request_endpoint,
            NULLFS_FULL_PROBE_FAILED,
            NULLFS_FULL_PROBE_PASSED,
        );
        run_probe(
            VFS_FULL_PROBE_COMMAND,
            vfs_request_endpoint,
            VFS_FULL_PROBE_FAILED,
            VFS_FULL_PROBE_PASSED,
        );
        run_nullfs_restart_probe(
            &mut nullfs_service,
            nullfs_readiness_endpoint,
            &mut nullfs_request_endpoint,
            nullfs_block_capability,
        );
    } else {
        run_probe(
            VFS_READINESS_PROBE_COMMAND,
            vfs_request_endpoint,
            VFS_READINESS_PROBE_FAILED,
            VFS_READINESS_PROBE_PASSED,
        );
    }
    let mut shell_process_id = spawn_shell();

    loop {
        if let Some(service_process_id) = logging_service.process_id() {
            match syscall::try_wait_child(service_process_id) {
                Ok(status) => match logging_service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        let _ = syscall::write_all(STDOUT, LOGGING_SERVICE_RESTARTING);
                        backoff(backoff_yields);
                        start_service(
                            &mut logging_service,
                            logging_readiness_endpoint,
                            logging_request_endpoint,
                            Some(logging_response_capability),
                            &LOGGING_MESSAGES,
                        );
                    }
                    ServiceStatusDisposition::Failed => fail(LOGGING_SERVICE_FAILED),
                },
                Err(error) if error == syscall::Errno::TRY_AGAIN => {}
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(_) => fail(LOGGING_SERVICE_FAILED),
            }
        }

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

fn nullfs_restart_test_boot() -> bool {
    let descriptor = syscall::open(BOOT_MODE_PATH, syscall::OpenFlags::READ)
        .unwrap_or_else(|_| fail(BOOT_MODE_PROBE_FAILED));
    let mut bytes = [0_u8; 32];
    let count =
        syscall::read(descriptor, &mut bytes).unwrap_or_else(|_| fail(BOOT_MODE_PROBE_FAILED));
    syscall::close(descriptor).unwrap_or_else(|_| fail(BOOT_MODE_PROBE_FAILED));
    match &bytes[..count] {
        b"nullfs-restart-test" | NULLFS_RESTART_TEST_BOOT_MODE => true,
        b"normal" | b"normal\n" | b"smoke-test" | b"smoke-test\n" => false,
        _ => fail(BOOT_MODE_PROBE_FAILED),
    }
}

fn run_nullfs_restart_probe(
    service: &mut ServiceRuntime,
    readiness_endpoint: CapabilityHandle,
    request_endpoint: &mut CapabilityHandle,
    block_capability: BootstrapCapability,
) {
    let ready_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let control_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let barrier =
        syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let probe_process_id = syscall::spawn_command_with_barrier(
        NULLFS_RESTART_PROBE_COMMAND,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &barrier,
    )
    .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    if ipc::grant_child(probe_process_id, ready_endpoint, Rights::SEND, READY_HANDLE).ok()
        != Some(READY_HANDLE)
        || ipc::grant_child(
            probe_process_id,
            control_endpoint,
            Rights::RECEIVE,
            REQUEST_HANDLE,
        )
        .ok()
            != Some(REQUEST_HANDLE)
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    barrier
        .release()
        .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));

    let mut ready = [0_u8; 64];
    loop {
        match ipc::try_receive(ready_endpoint, &mut ready) {
            Ok(message) => {
                if message.sender_process_id != probe_process_id
                    || message.capability.is_some()
                    || message.bytes != NULLFS_RESTART_PROBE_READY.len()
                    || &ready[..message.bytes] != NULLFS_RESTART_PROBE_READY
                {
                    fail(NULLFS_RESTART_PROBE_FAILED);
                }
                break;
            }
            Err(error) if error == ipc::Error::TRY_AGAIN => {}
            Err(_) => fail(NULLFS_RESTART_PROBE_FAILED),
        }
        if !matches!(
            syscall::try_wait_child(probe_process_id),
            Err(error) if error == syscall::Errno::TRY_AGAIN
                || error == syscall::Errno::INTERRUPTED
        ) {
            fail(NULLFS_RESTART_PROBE_FAILED);
        }
        syscall::yield_now().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    }

    let old_service_process_id = service
        .process_id()
        .unwrap_or_else(|| fail(NULLFS_RESTART_PROBE_FAILED));
    if ipc::info(*request_endpoint)
        .map(|info| info.size)
        .unwrap_or(1)
        != 0
        || syscall::signal_process_group(old_service_process_id, signal::STOP).ok() != Some(1)
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    loop {
        match syscall::wait_child(old_service_process_id) {
            Ok(status) if status.stopped_signal() == Some(signal::STOP) => {
                if service.observe_status(status.raw())
                    != ServiceStatusDisposition::WaitForNextEvent
                {
                    fail(NULLFS_RESTART_PROBE_FAILED);
                }
                break;
            }
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(_) => fail(NULLFS_RESTART_PROBE_FAILED),
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(NULLFS_RESTART_PROBE_FAILED),
        }
    }

    ipc::send(control_endpoint, NULLFS_RESTART_PROBE_BEGIN_READ, None)
        .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let mut request_queued = false;
    for _ in 0..256 {
        match ipc::info(*request_endpoint) {
            Ok(info) if info.size != 0 => {
                request_queued = true;
                break;
            }
            Ok(_) => {}
            Err(_) => fail(NULLFS_RESTART_PROBE_FAILED),
        }
        if !matches!(
            syscall::try_wait_child(probe_process_id),
            Err(error) if error == syscall::Errno::TRY_AGAIN
                || error == syscall::Errno::INTERRUPTED
        ) {
            fail(NULLFS_RESTART_PROBE_FAILED);
        }
        syscall::yield_now().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    }
    if !request_queued
        || syscall::signal_process_group(old_service_process_id, signal::TERMINATE).ok() != Some(1)
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }

    let backoff_yields = loop {
        match syscall::wait_child(old_service_process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {
                if service.observe_status(status.raw())
                    != ServiceStatusDisposition::WaitForNextEvent
                {
                    fail(NULLFS_RESTART_PROBE_FAILED);
                }
            }
            Ok(status) => match service.observe_status(status.raw()) {
                ServiceStatusDisposition::Restart { backoff_yields } => break backoff_yields,
                ServiceStatusDisposition::WaitForNextEvent | ServiceStatusDisposition::Failed => {
                    fail(NULLFS_RESTART_PROBE_FAILED)
                }
            },
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(NULLFS_RESTART_PROBE_FAILED),
        }
    };
    let _ = syscall::write_all(STDOUT, NULLFS_SERVICE_RESTARTING);
    backoff(backoff_yields);

    let replacement_request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    start_service(
        service,
        readiness_endpoint,
        replacement_request_endpoint,
        Some(block_capability),
        &NULLFS_MESSAGES,
    );
    register_nullfs_proxy(service, replacement_request_endpoint);
    let stale_request_endpoint = *request_endpoint;
    *request_endpoint = replacement_request_endpoint;
    ipc::close(stale_request_endpoint).unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));

    loop {
        match syscall::wait_child(probe_process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) if status.success() => break,
            Ok(_) => fail(NULLFS_RESTART_PROBE_FAILED),
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(NULLFS_RESTART_PROBE_FAILED),
        }
    }
    ipc::close(ready_endpoint).unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    ipc::close(control_endpoint).unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let _ = syscall::write_all(STDOUT, NULLFS_RESTART_PROBE_PASSED);
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

fn run_logging_probe(
    service: &ServiceRuntime,
    request_endpoint: CapabilityHandle,
    response_endpoint: CapabilityHandle,
    command: &[u8],
    passed_message: &[u8],
) {
    let barrier = syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
    let probe_process_id = syscall::spawn_command_with_barrier(
        command,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &barrier,
    )
    .unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
    grant_logging_probe_endpoints(probe_process_id, request_endpoint, response_endpoint);
    barrier
        .release()
        .unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
    wait_for_logging_probe_exit(service, probe_process_id);
    let _ = syscall::write_all(STDOUT, passed_message);
}

fn run_logging_collector_restart_test(
    service: &mut ServiceRuntime,
    readiness_endpoint: CapabilityHandle,
    request_endpoint: CapabilityHandle,
    response_endpoint: CapabilityHandle,
    response_capability: BootstrapCapability,
) {
    let status_endpoint = ipc::endpoint_create().unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
    let control_endpoint = ipc::endpoint_create().unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
    let barrier = syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
    let probe_process_id = syscall::spawn_command_with_barrier(
        LOGGING_STRESS_PROBE_COMMAND,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &barrier,
    )
    .unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
    grant_logging_probe_endpoints(probe_process_id, request_endpoint, response_endpoint);
    if ipc::grant_child(
        probe_process_id,
        status_endpoint,
        Rights::SEND,
        LOGGING_PROBE_STATUS_HANDLE,
    )
    .ok()
        != Some(LOGGING_PROBE_STATUS_HANDLE)
        || ipc::grant_child(
            probe_process_id,
            control_endpoint,
            Rights::RECEIVE,
            LOGGING_PROBE_CONTROL_HANDLE,
        )
        .ok()
            != Some(LOGGING_PROBE_CONTROL_HANDLE)
    {
        fail(LOGGING_PROBE_FAILED);
    }
    barrier
        .release()
        .unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));

    wait_for_logging_probe_message(
        service,
        probe_process_id,
        status_endpoint,
        LOGGING_PROBE_BOUND,
    );
    let old_service_process_id = service
        .process_id()
        .unwrap_or_else(|| fail(LOGGING_PROBE_FAILED));
    if syscall::signal_process_group(old_service_process_id, signal::STOP).ok() != Some(1) {
        fail(LOGGING_PROBE_FAILED);
    }
    wait_for_logging_service_stop(service, old_service_process_id);
    if service.restart_count() != 0 {
        fail(LOGGING_COLLECTOR_RESTART_POLICY_FAILED);
    }
    require_empty_endpoint(request_endpoint);
    require_empty_endpoint(response_endpoint);
    if ipc::send(control_endpoint, LOGGING_PROBE_FILL_QUEUE, None).is_err() {
        fail(LOGGING_PROBE_FAILED);
    }
    wait_for_logging_probe_message(
        service,
        probe_process_id,
        status_endpoint,
        LOGGING_PROBE_BACKPRESSURE,
    );
    if ipc::info(request_endpoint).map(|info| info.size).ok() != Some(8) {
        fail(LOGGING_PROBE_FAILED);
    }
    if syscall::signal_process_group(old_service_process_id, signal::CONTINUE).ok() != Some(1) {
        fail(LOGGING_PROBE_FAILED);
    }
    wait_for_logging_service_continue(service, old_service_process_id);
    wait_for_logging_probe_exit(service, probe_process_id);

    require_empty_endpoint(readiness_endpoint);
    require_empty_endpoint(request_endpoint);
    require_empty_endpoint(response_endpoint);
    require_empty_endpoint(status_endpoint);
    require_empty_endpoint(control_endpoint);
    if syscall::signal_process_group(old_service_process_id, signal::TERMINATE).ok() != Some(1) {
        fail(LOGGING_PROBE_FAILED);
    }
    let backoff_yields = wait_for_logging_service_restart(service, old_service_process_id);
    if service.restart_count() != 1 {
        fail(LOGGING_COLLECTOR_RESTART_POLICY_FAILED);
    }
    let _ = syscall::write_all(STDOUT, LOGGING_SERVICE_RESTARTING);
    backoff(backoff_yields);
    start_service(
        service,
        readiness_endpoint,
        request_endpoint,
        Some(response_capability),
        &LOGGING_MESSAGES,
    );
    if service.process_id() == Some(old_service_process_id) || service.restart_count() != 1 {
        fail(LOGGING_COLLECTOR_RESTART_POLICY_FAILED);
    }
    run_logging_probe(
        service,
        request_endpoint,
        response_endpoint,
        LOGGING_RESTART_PROBE_COMMAND,
        &[],
    );
    require_empty_endpoint(request_endpoint);
    require_empty_endpoint(response_endpoint);
    ipc::close(status_endpoint).unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
    ipc::close(control_endpoint).unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
    let _ = syscall::write_all(STDOUT, LOGGING_COLLECTOR_TEST_PASSED);
}

fn grant_logging_probe_endpoints(
    probe_process_id: ProcessId,
    request_endpoint: CapabilityHandle,
    response_endpoint: CapabilityHandle,
) {
    if ipc::grant_child(
        probe_process_id,
        request_endpoint,
        Rights::SEND,
        READY_HANDLE,
    )
    .ok()
        != Some(READY_HANDLE)
        || ipc::grant_child(
            probe_process_id,
            response_endpoint,
            Rights::RECEIVE,
            REQUEST_HANDLE,
        )
        .ok()
            != Some(REQUEST_HANDLE)
    {
        fail(LOGGING_PROBE_FAILED);
    }
}

fn wait_for_logging_probe_message(
    service: &ServiceRuntime,
    probe_process_id: ProcessId,
    endpoint: CapabilityHandle,
    expected: &[u8],
) {
    let mut remaining = LOGGING_PROBE_MAX_YIELDS;
    let mut buffer = [0_u8; 64];
    loop {
        match ipc::try_receive(endpoint, &mut buffer) {
            Ok(message)
                if message.sender_process_id == probe_process_id
                    && message.capability.is_none()
                    && message.bytes == expected.len()
                    && &buffer[..message.bytes] == expected =>
            {
                return;
            }
            Ok(_) => fail(LOGGING_PROBE_FAILED),
            Err(error) if error == ipc::Error::TRY_AGAIN => {}
            Err(_) => fail(LOGGING_PROBE_FAILED),
        }
        require_process_running(probe_process_id);
        require_service_running(service);
        yield_logging_probe(&mut remaining);
    }
}

fn wait_for_logging_probe_exit(service: &ServiceRuntime, probe_process_id: ProcessId) {
    let mut remaining = LOGGING_PROBE_MAX_YIELDS;
    loop {
        match syscall::try_wait_child(probe_process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) if status.success() => return,
            Ok(_) => fail(LOGGING_PROBE_FAILED),
            Err(error) if error == syscall::Errno::TRY_AGAIN => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(LOGGING_PROBE_FAILED),
        }
        require_service_running(service);
        yield_logging_probe(&mut remaining);
    }
}

fn require_process_running(process_id: ProcessId) {
    match syscall::try_wait_child(process_id) {
        Err(error)
            if error == syscall::Errno::TRY_AGAIN || error == syscall::Errno::INTERRUPTED => {}
        Ok(status) if status.continued() => {}
        _ => fail(LOGGING_PROBE_FAILED),
    }
}

fn require_service_running(service: &ServiceRuntime) {
    let process_id = service
        .process_id()
        .unwrap_or_else(|| fail(LOGGING_PROBE_FAILED));
    require_process_running(process_id);
}

fn wait_for_logging_service_stop(service: &mut ServiceRuntime, process_id: ProcessId) {
    let mut remaining = LOGGING_PROBE_MAX_YIELDS;
    loop {
        match syscall::try_wait_child(process_id) {
            Ok(status) if status.stopped_signal() == Some(signal::STOP) => {
                if service.observe_status(status.raw())
                    != ServiceStatusDisposition::WaitForNextEvent
                {
                    fail(LOGGING_PROBE_FAILED);
                }
                return;
            }
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {
                if service.observe_status(status.raw())
                    != ServiceStatusDisposition::WaitForNextEvent
                {
                    fail(LOGGING_PROBE_FAILED);
                }
            }
            Ok(_) => fail(LOGGING_PROBE_FAILED),
            Err(error) if error == syscall::Errno::TRY_AGAIN => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(LOGGING_PROBE_FAILED),
        }
        yield_logging_probe(&mut remaining);
    }
}

fn wait_for_logging_service_continue(service: &mut ServiceRuntime, process_id: ProcessId) {
    let mut remaining = LOGGING_PROBE_MAX_YIELDS;
    loop {
        match syscall::try_wait_child(process_id) {
            Ok(status) if status.continued() => {
                if service.observe_status(status.raw())
                    != ServiceStatusDisposition::WaitForNextEvent
                {
                    fail(LOGGING_PROBE_FAILED);
                }
                return;
            }
            Ok(status) if status.stopped_signal().is_some() => {
                if service.observe_status(status.raw())
                    != ServiceStatusDisposition::WaitForNextEvent
                {
                    fail(LOGGING_PROBE_FAILED);
                }
            }
            Ok(_) => fail(LOGGING_PROBE_FAILED),
            Err(error) if error == syscall::Errno::TRY_AGAIN => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(LOGGING_PROBE_FAILED),
        }
        yield_logging_probe(&mut remaining);
    }
}

fn wait_for_logging_service_restart(service: &mut ServiceRuntime, process_id: ProcessId) -> u32 {
    let mut remaining = LOGGING_PROBE_MAX_YIELDS;
    loop {
        match syscall::try_wait_child(process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {
                if service.observe_status(status.raw())
                    != ServiceStatusDisposition::WaitForNextEvent
                {
                    fail(LOGGING_COLLECTOR_RESTART_POLICY_FAILED);
                }
            }
            Ok(status) => match service.observe_status(status.raw()) {
                ServiceStatusDisposition::Restart { backoff_yields } => return backoff_yields,
                ServiceStatusDisposition::WaitForNextEvent | ServiceStatusDisposition::Failed => {
                    fail(LOGGING_COLLECTOR_RESTART_POLICY_FAILED)
                }
            },
            Err(error) if error == syscall::Errno::TRY_AGAIN => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(error) if error == syscall::Errno::NO_CHILD => {
                fail(LOGGING_COLLECTOR_EXIT_NO_CHILD)
            }
            Err(_) => fail(LOGGING_COLLECTOR_EXIT_WAIT_FAILED),
        }
        yield_logging_probe(&mut remaining);
    }
}

fn require_empty_endpoint(endpoint: CapabilityHandle) {
    if ipc::info(endpoint).map(|info| info.size).ok() != Some(0) {
        fail(LOGGING_PROBE_FAILED);
    }
}

fn yield_logging_probe(remaining: &mut u32) {
    if *remaining == 0 {
        fail(LOGGING_PROBE_FAILED);
    }
    *remaining -= 1;
    syscall::yield_now().unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
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
