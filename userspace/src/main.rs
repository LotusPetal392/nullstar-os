#![no_std]
#![no_main]

use nswp_logging::{LOGGING_OBSERVER_ROLE, LOGGING_PRODUCER_ROLE, LOGGING_SERVICE_ID};
use service_control::{
    DesiredState, ListResponse, ObservedState, ServiceControlFailure, ServiceControlRequest,
    ServiceControlResponse, ServiceId, ServiceRecord, TargetResponse,
};
use service_route::{Authorizer, ProviderGeneration, ProviderGenerationSequence, RouteKey};
use userspace::{
    abi::{INIT_PROCESS_ID, signal},
    early_log,
    ipc::{self, CapabilityHandle, ObjectKind, Rights},
    nullfs_primary_volume, platform,
    service_control::{
        ControlIngress, LOGGING_SERVICE_ID as CONTROL_LOGGING_SERVICE_ID, NULLFS_SERVICE_ID,
        TMPFS_SERVICE_ID, VFS_SERVICE_ID,
    },
    service_route::{NativeRouteTable, RouteIngress, queue_service_generation},
    supervisor::{
        ServiceRuntime, ServiceSpec, ServiceState, ServiceStatusDisposition,
        ShellStatusDisposition, shell_status_disposition,
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
const LOGCTL_COMMAND: &[u8] = b"/logctl show";
const LOGCTL_FAILED: &[u8] = b"userspace init: logctl show failed\n";
const LOGCTL_PASSED: &[u8] = b"userspace init: logctl show passed\n";
const SV_LIST_COMMAND: &[u8] = b"/sv list";
const SV_STATUS_LOGGING_COMMAND: &[u8] = b"/sv status logging";
const SV_LIST_PASSED: &[u8] = b"userspace init: sv list passed\n";
const SV_STATUS_LOGGING_PASSED: &[u8] = b"userspace init: sv status logging passed\n";
const SERVICE_CONTROL_BOOTSTRAP_FAILED: &[u8] =
    b"userspace init: failed to create service-control observation endpoint\n";
const SERVICE_CONTROL_PROBE_FAILED: &[u8] = b"userspace init: service-control probe failed\n";
const SERVICE_CONTROL_PROTOCOL_FAILED: &[u8] = b"userspace init: service-control state invalid\n";
const LOGGING_COLLECTOR_TEST_PASSED: &[u8] = b"userspace init: logging collector ring, backpressure, redaction, and route generation isolation verified\n";
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
const LOGGING_OBSERVER_INGRESS_HANDLE: u64 = 3;
const LOGGING_EARLY_LOG_HANDLE: u64 = 4;
const GENERATION_HANDOFF_HANDLE: u64 = 5;
const SHELL_SERVICE_CONTROL_HANDLE: u64 = 2;
const MAX_BOOTSTRAP_ROUTES: usize = 2;
const ROUTE_PUMP_BUDGET: usize = 4;
const SERVICE_CONTROL_PUMP_BUDGET: usize = 4;
const LOGGING_PROBE_STATUS_HANDLE: u64 = 3;
const LOGGING_PROBE_CONTROL_HANDLE: u64 = 4;

const LOGGING_SERVICE: ServiceSpec = ServiceSpec {
    name: b"logging",
    command: LOGGING_SERVICE_COMMAND,
    ready_message: LOGGING_SERVICE_READY_MESSAGE,
    bootstrap_handle: READY_HANDLE,
    restart_limit: 3,
    restart_backoff_yields: 32,
    fatal_startup_exit_status: Some(early_log::IMPORT_FAILURE_EXIT_STATUS),
};
const NULLFS_SERVICE: ServiceSpec = ServiceSpec {
    name: b"nullfs",
    command: NULLFS_SERVICE_COMMAND,
    ready_message: NULLFS_SERVICE_READY_MESSAGE,
    bootstrap_handle: READY_HANDLE,
    restart_limit: 3,
    restart_backoff_yields: 32,
    fatal_startup_exit_status: None,
};
const TMPFS_SERVICE: ServiceSpec = ServiceSpec {
    name: b"tmpfs",
    command: SERVICE_COMMAND,
    ready_message: SERVICE_READY_MESSAGE,
    bootstrap_handle: READY_HANDLE,
    restart_limit: 3,
    restart_backoff_yields: 32,
    fatal_startup_exit_status: None,
};
const VFS_SERVICE: ServiceSpec = ServiceSpec {
    name: b"vfs",
    command: VFS_SERVICE_COMMAND,
    ready_message: VFS_SERVICE_READY_MESSAGE,
    bootstrap_handle: READY_HANDLE,
    restart_limit: 3,
    restart_backoff_yields: 32,
    fatal_startup_exit_status: None,
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

const LOGGING_PRODUCER_KEY: RouteKey = RouteKey::new(LOGGING_SERVICE_ID, LOGGING_PRODUCER_ROLE);
const LOGGING_OBSERVER_KEY: RouteKey = RouteKey::new(LOGGING_SERVICE_ID, LOGGING_OBSERVER_ROLE);

struct RouteBrokerState {
    routes: NativeRouteTable<CapabilityHandle, MAX_BOOTSTRAP_ROUTES>,
    producer_grant_source: CapabilityHandle,
    observer_grant_source: CapabilityHandle,
    producer_ingress: RouteIngress,
    observer_ingress: RouteIngress,
}

impl RouteBrokerState {
    fn new() -> Self {
        let producer_grant_source =
            ipc::endpoint_create().unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let observer_grant_source =
            ipc::endpoint_create().unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let producer_receive = ipc::duplicate(producer_grant_source, Rights::RECEIVE)
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let observer_receive = ipc::duplicate(observer_grant_source, Rights::RECEIVE)
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let producer_ingress = RouteIngress::bind(producer_receive, LOGGING_PRODUCER_KEY)
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let observer_ingress = RouteIngress::bind(observer_receive, LOGGING_OBSERVER_KEY)
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        Self {
            routes: NativeRouteTable::new(),
            producer_grant_source,
            observer_grant_source,
            producer_ingress,
            observer_ingress,
        }
    }

    fn pump(&mut self) {
        for _ in 0..ROUTE_PUMP_BUDGET {
            let mut progressed = false;
            progressed |= pump_route_ingress(&mut self.producer_ingress, &self.routes);
            progressed |= pump_route_ingress(&mut self.observer_ingress, &self.routes);
            if !progressed {
                break;
            }
        }
    }

    fn publish(
        &mut self,
        generation: ProviderGeneration,
        producer_source: CapabilityHandle,
        observer_source: CapabilityHandle,
    ) {
        let producer_authority = ipc::duplicate(
            producer_source,
            Rights::SEND | Rights::DUPLICATE | Rights::TRANSFER,
        )
        .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let observer_authority = ipc::duplicate(
            observer_source,
            Rights::SEND | Rights::DUPLICATE | Rights::TRANSFER,
        )
        .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        if !matches!(
            self.routes
                .publish(LOGGING_PRODUCER_KEY, generation, producer_authority),
            Ok(None)
        ) {
            fail(LOGGING_SERVICE_BOOTSTRAP_FAILED);
        }
        if !matches!(
            self.routes
                .publish(LOGGING_OBSERVER_KEY, generation, observer_authority),
            Ok(None)
        ) {
            fail(LOGGING_SERVICE_BOOTSTRAP_FAILED);
        }
    }

    fn withdraw(&mut self, generation: ProviderGeneration) {
        let producer = self
            .routes
            .withdraw(LOGGING_PRODUCER_KEY, generation)
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let observer = self
            .routes
            .withdraw(LOGGING_OBSERVER_KEY, generation)
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        ipc::close(producer).unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        ipc::close(observer).unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
    }
}

#[derive(Clone, Copy)]
struct ServiceRegistryView<'a> {
    logging: &'a ServiceRuntime,
    logging_generation: ProviderGeneration,
    nullfs: &'a ServiceRuntime,
    nullfs_generation: ProviderGeneration,
    tmpfs: &'a ServiceRuntime,
    tmpfs_generation: ProviderGeneration,
    vfs: &'a ServiceRuntime,
    vfs_generation: ProviderGeneration,
}

struct ServiceControlState {
    source: CapabilityHandle,
    ingress: ControlIngress,
}

impl ServiceControlState {
    fn new() -> Self {
        let source =
            ipc::endpoint_create().unwrap_or_else(|_| fail(SERVICE_CONTROL_BOOTSTRAP_FAILED));
        let receive = ipc::duplicate(source, Rights::RECEIVE)
            .unwrap_or_else(|_| fail(SERVICE_CONTROL_BOOTSTRAP_FAILED));
        let ingress = ControlIngress::bind(receive)
            .unwrap_or_else(|_| fail(SERVICE_CONTROL_BOOTSTRAP_FAILED));
        Self { source, ingress }
    }

    fn pump(&mut self, registry: ServiceRegistryView<'_>) {
        for _ in 0..SERVICE_CONTROL_PUMP_BUDGET {
            let request = match self.ingress.try_accept() {
                Ok(Some(request)) => request,
                Ok(None) => break,
                Err(_) => continue,
            };
            let response = service_control_response(request.request(), registry);
            let _ = request.reply(response);
        }
    }
}

fn service_control_response(
    request: ServiceControlRequest,
    registry: ServiceRegistryView<'_>,
) -> ServiceControlResponse {
    match request {
        ServiceControlRequest::List { cursor } => {
            let response = match cursor {
                0 => ListResponse::record(
                    cursor,
                    service_record(
                        CONTROL_LOGGING_SERVICE_ID,
                        registry.logging,
                        registry.logging_generation,
                    ),
                    1,
                )
                .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROTOCOL_FAILED)),
                1 => ListResponse::record(
                    cursor,
                    service_record(
                        NULLFS_SERVICE_ID,
                        registry.nullfs,
                        registry.nullfs_generation,
                    ),
                    2,
                )
                .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROTOCOL_FAILED)),
                2 => ListResponse::record(
                    cursor,
                    service_record(TMPFS_SERVICE_ID, registry.tmpfs, registry.tmpfs_generation),
                    3,
                )
                .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROTOCOL_FAILED)),
                3 => ListResponse::record(
                    cursor,
                    service_record(VFS_SERVICE_ID, registry.vfs, registry.vfs_generation),
                    0,
                )
                .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROTOCOL_FAILED)),
                _ => ListResponse::failure(cursor, ServiceControlFailure::NotFound),
            };
            ServiceControlResponse::list(response)
        }
        ServiceControlRequest::Status { service } => {
            let response = match service_status(service, registry) {
                Some(record) => TargetResponse::success(record),
                None => TargetResponse::failure(service, ServiceControlFailure::NotFound),
            };
            ServiceControlResponse::status(response)
        }
        ServiceControlRequest::Start { service } => ServiceControlResponse::start(
            TargetResponse::failure(service, ServiceControlFailure::AccessDenied),
        )
        .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROTOCOL_FAILED)),
        ServiceControlRequest::Stop { service } => ServiceControlResponse::stop(
            TargetResponse::failure(service, ServiceControlFailure::AccessDenied),
        )
        .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROTOCOL_FAILED)),
        ServiceControlRequest::Restart { service } => ServiceControlResponse::restart(
            TargetResponse::failure(service, ServiceControlFailure::AccessDenied),
        )
        .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROTOCOL_FAILED)),
    }
}

fn service_status(service: ServiceId, registry: ServiceRegistryView<'_>) -> Option<ServiceRecord> {
    if service == CONTROL_LOGGING_SERVICE_ID {
        Some(service_record(
            service,
            registry.logging,
            registry.logging_generation,
        ))
    } else if service == NULLFS_SERVICE_ID {
        Some(service_record(
            service,
            registry.nullfs,
            registry.nullfs_generation,
        ))
    } else if service == TMPFS_SERVICE_ID {
        Some(service_record(
            service,
            registry.tmpfs,
            registry.tmpfs_generation,
        ))
    } else if service == VFS_SERVICE_ID {
        Some(service_record(
            service,
            registry.vfs,
            registry.vfs_generation,
        ))
    } else {
        None
    }
}

fn service_record(
    service: ServiceId,
    runtime: &ServiceRuntime,
    managed_generation: ProviderGeneration,
) -> ServiceRecord {
    let (observed, generation) = match runtime.state() {
        ServiceState::Stopped => (ObservedState::Stopped, None),
        ServiceState::Starting => (ObservedState::Starting, Some(managed_generation)),
        ServiceState::Running => (ObservedState::Ready, Some(managed_generation)),
        ServiceState::Backoff => (ObservedState::Stopped, None),
        ServiceState::Failed => (ObservedState::Quarantined, None),
    };
    ServiceRecord::new(service, generation, observed, DesiredState::Running)
        .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROTOCOL_FAILED))
}

struct AllowGrantedRoute;

impl Authorizer<u64> for AllowGrantedRoute {
    type Error = ();

    fn authorize(&mut self, caller: &u64, _key: RouteKey) -> Result<(), Self::Error> {
        if *caller == 0 { Err(()) } else { Ok(()) }
    }
}

fn pump_route_ingress(
    ingress: &mut RouteIngress,
    routes: &NativeRouteTable<CapabilityHandle, MAX_BOOTSTRAP_ROUTES>,
) -> bool {
    let request = match ingress.try_accept() {
        Ok(Some(request)) => request,
        Ok(None) => return false,
        Err(_) => return true,
    };
    if let Ok(authorized) = request.authorize(&mut AllowGrantedRoute) {
        let _ = authorized.resolve(routes);
    }
    true
}

struct LoggingGeneration {
    generation: ProviderGeneration,
    readiness_source: CapabilityHandle,
    producer_source: CapabilityHandle,
    observer_source: CapabilityHandle,
    producer_object_id: u64,
    observer_object_id: u64,
}

impl LoggingGeneration {
    fn close(self) {
        ipc::close(self.readiness_source)
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        ipc::close(self.producer_source).unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        ipc::close(self.observer_source).unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
    }
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

    let mut route_broker = RouteBrokerState::new();
    let logging_early_log_reader =
        early_log::open_reader().unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
    if !matches!(
        ipc::info(logging_early_log_reader),
        Ok(info)
            if info.kind == ObjectKind::KernelEarlyLogReader
                && info.rights == Rights::KERNEL_EARLY_LOG_READER
    ) {
        fail(LOGGING_SERVICE_BOOTSTRAP_FAILED);
    }
    let mut logging_service = ServiceRuntime::new(LOGGING_SERVICE);
    let mut logging_generations = ProviderGenerationSequence::new();
    let mut logging_generation = start_logging_generation(
        &mut logging_service,
        &mut route_broker,
        &mut logging_generations,
        logging_early_log_reader,
    );
    if nullfs_restart_test {
        logging_generation = run_logging_collector_restart_test(
            &mut logging_service,
            &mut route_broker,
            &mut logging_generations,
            logging_early_log_reader,
            logging_generation,
        );
    } else {
        run_logging_probe(
            &logging_service,
            &mut route_broker,
            LOGGING_PROBE_COMMAND,
            LOGGING_PROBE_PASSED,
        );
    }
    run_logctl_show(&logging_service, &mut route_broker);

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
    let mut nullfs_generations = ProviderGenerationSequence::new();
    let nullfs_block_capability = BootstrapCapability {
        source_handle: nullfs_service_block_endpoint,
        rights: Rights::SEND,
        target_handle: NULLFS_BLOCK_HANDLE,
    };
    let mut nullfs_generation = start_service(
        &mut nullfs_service,
        &mut nullfs_generations,
        nullfs_readiness_endpoint,
        nullfs_request_endpoint,
        &[nullfs_block_capability],
        &NULLFS_MESSAGES,
    );
    run_probe(
        NULLFS_READINESS_PROBE_COMMAND,
        nullfs_request_endpoint,
        NULLFS_READINESS_PROBE_FAILED,
        NULLFS_READINESS_PROBE_PASSED,
    );
    register_nullfs_proxy(nullfs_generation, nullfs_request_endpoint);

    let readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
    let request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
    let mut service = ServiceRuntime::new(TMPFS_SERVICE);
    let mut tmpfs_generations = ProviderGenerationSequence::new();
    let mut tmpfs_generation = start_service(
        &mut service,
        &mut tmpfs_generations,
        readiness_endpoint,
        request_endpoint,
        &[],
        &TMPFS_MESSAGES,
    );
    register_tmpfs_proxy(tmpfs_generation, request_endpoint);
    run_tmpfs_probe(request_endpoint);
    let vfs_readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
    let vfs_request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
    let mut vfs_service = ServiceRuntime::new(VFS_SERVICE);
    let mut vfs_generations = ProviderGenerationSequence::new();
    let mut vfs_generation = start_service(
        &mut vfs_service,
        &mut vfs_generations,
        vfs_readiness_endpoint,
        vfs_request_endpoint,
        &[],
        &VFS_MESSAGES,
    );
    register_vfs_router(vfs_generation, vfs_request_endpoint);
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
        nullfs_generation = run_nullfs_restart_probe(
            &mut nullfs_service,
            &mut nullfs_generations,
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

    let mut service_control = ServiceControlState::new();
    let registry = ServiceRegistryView {
        logging: &logging_service,
        logging_generation: logging_generation.generation,
        nullfs: &nullfs_service,
        nullfs_generation,
        tmpfs: &service,
        tmpfs_generation,
        vfs: &vfs_service,
        vfs_generation,
    };
    run_service_control_probe(
        SV_LIST_COMMAND,
        SV_LIST_PASSED,
        &mut service_control,
        registry,
    );
    run_service_control_probe(
        SV_STATUS_LOGGING_COMMAND,
        SV_STATUS_LOGGING_PASSED,
        &mut service_control,
        registry,
    );
    let mut shell_process_id =
        spawn_shell(route_broker.observer_grant_source, service_control.source);

    loop {
        if let Some(service_process_id) = logging_service.process_id() {
            match syscall::try_wait_child(service_process_id) {
                Ok(status) => match logging_service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        route_broker.withdraw(logging_generation.generation);
                        logging_generation.close();
                        let _ = syscall::write_all(STDOUT, LOGGING_SERVICE_RESTARTING);
                        backoff(backoff_yields);
                        logging_generation = start_logging_generation(
                            &mut logging_service,
                            &mut route_broker,
                            &mut logging_generations,
                            logging_early_log_reader,
                        );
                    }
                    ServiceStatusDisposition::Failed => fail(LOGGING_SERVICE_FAILED),
                },
                Err(error) if error == syscall::Errno::TRY_AGAIN => {}
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(_) => fail(LOGGING_SERVICE_FAILED),
            }
        }
        route_broker.pump();
        service_control.pump(ServiceRegistryView {
            logging: &logging_service,
            logging_generation: logging_generation.generation,
            nullfs: &nullfs_service,
            nullfs_generation,
            tmpfs: &service,
            tmpfs_generation,
            vfs: &vfs_service,
            vfs_generation,
        });

        if let Some(service_process_id) = nullfs_service.process_id() {
            match syscall::try_wait_child(service_process_id) {
                Ok(status) => match nullfs_service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        let _ = syscall::write_all(STDOUT, NULLFS_SERVICE_RESTARTING);
                        backoff(backoff_yields);
                        nullfs_generation = start_service(
                            &mut nullfs_service,
                            &mut nullfs_generations,
                            nullfs_readiness_endpoint,
                            nullfs_request_endpoint,
                            &[nullfs_block_capability],
                            &NULLFS_MESSAGES,
                        );
                        register_nullfs_proxy(nullfs_generation, nullfs_request_endpoint);
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
                        tmpfs_generation = start_service(
                            &mut service,
                            &mut tmpfs_generations,
                            readiness_endpoint,
                            request_endpoint,
                            &[],
                            &TMPFS_MESSAGES,
                        );
                        register_tmpfs_proxy(tmpfs_generation, request_endpoint);
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
                        vfs_generation = start_service(
                            &mut vfs_service,
                            &mut vfs_generations,
                            vfs_readiness_endpoint,
                            vfs_request_endpoint,
                            &[],
                            &VFS_MESSAGES,
                        );
                        register_vfs_router(vfs_generation, vfs_request_endpoint);
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
                    shell_process_id =
                        spawn_shell(route_broker.observer_grant_source, service_control.source);
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
    generations: &mut ProviderGenerationSequence,
    readiness_endpoint: CapabilityHandle,
    request_endpoint: &mut CapabilityHandle,
    block_capability: BootstrapCapability,
) -> ProviderGeneration {
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
    let generation = start_service(
        service,
        generations,
        readiness_endpoint,
        replacement_request_endpoint,
        &[block_capability],
        &NULLFS_MESSAGES,
    );
    register_nullfs_proxy(generation, replacement_request_endpoint);
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
    generation
}

fn register_nullfs_proxy(
    service_generation: ProviderGeneration,
    request_endpoint: CapabilityHandle,
) {
    let generation = u32::try_from(service_generation.get())
        .unwrap_or_else(|_| fail(NULLFS_SERVICE_PROTOCOL_FAILED));
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

fn register_tmpfs_proxy(
    service_generation: ProviderGeneration,
    request_endpoint: CapabilityHandle,
) {
    let mount = Mount::connect(request_endpoint).unwrap_or_else(|_| fail(SERVICE_PROTOCOL_FAILED));
    let generation =
        u32::try_from(service_generation.get()).unwrap_or_else(|_| fail(SERVICE_PROTOCOL_FAILED));
    if mount.generation() != generation {
        fail(SERVICE_PROTOCOL_FAILED);
    }
    mount
        .disconnect()
        .unwrap_or_else(|_| fail(SERVICE_PROTOCOL_FAILED));
    platform::register_tmpfs_service(request_endpoint, generation)
        .unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
}

fn register_vfs_router(service_generation: ProviderGeneration, request_endpoint: CapabilityHandle) {
    let generation = u32::try_from(service_generation.get())
        .unwrap_or_else(|_| fail(VFS_SERVICE_PROTOCOL_FAILED));
    platform::register_vfs_service(request_endpoint, generation)
        .unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
}

fn start_logging_generation(
    service: &mut ServiceRuntime,
    route_broker: &mut RouteBrokerState,
    generations: &mut ProviderGenerationSequence,
    early_log_reader: CapabilityHandle,
) -> LoggingGeneration {
    'attempt: loop {
        let generation = generations
            .next_generation()
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let generation_handoff_source =
            ipc::endpoint_create().unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        queue_service_generation(generation_handoff_source, generation)
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let readiness_source =
            ipc::endpoint_create().unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let producer_source =
            ipc::endpoint_create().unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let observer_source =
            ipc::endpoint_create().unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let producer_object_id = ipc::info(producer_source)
            .map(|info| info.object_id)
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        let observer_object_id = ipc::info(observer_source)
            .map(|info| info.object_id)
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        if producer_object_id == 0
            || observer_object_id == 0
            || producer_object_id == observer_object_id
        {
            fail(LOGGING_SERVICE_BOOTSTRAP_FAILED);
        }

        let spec = service.spec();
        let _ = syscall::write_all(STDOUT, LOGGING_MESSAGES.starting);
        let barrier =
            syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(LOGGING_SERVICE_FAILED));
        let process_id = syscall::spawn_command_with_barrier(
            spec.command,
            SpawnFlags::NEW_PROCESS_GROUP,
            None,
            None,
            None,
            None,
            &barrier,
        )
        .unwrap_or_else(|_| fail(LOGGING_SERVICE_FAILED));
        service.note_spawned(process_id);
        if ipc::grant_child(process_id, readiness_source, Rights::SEND, READY_HANDLE).ok()
            != Some(READY_HANDLE)
            || ipc::grant_child(process_id, producer_source, Rights::RECEIVE, REQUEST_HANDLE).ok()
                != Some(REQUEST_HANDLE)
            || ipc::grant_child(
                process_id,
                observer_source,
                Rights::RECEIVE,
                LOGGING_OBSERVER_INGRESS_HANDLE,
            )
            .ok()
                != Some(LOGGING_OBSERVER_INGRESS_HANDLE)
            || ipc::grant_child(
                process_id,
                early_log_reader,
                Rights::READ,
                LOGGING_EARLY_LOG_HANDLE,
            )
            .ok()
                != Some(LOGGING_EARLY_LOG_HANDLE)
            || ipc::grant_child(
                process_id,
                generation_handoff_source,
                Rights::RECEIVE,
                GENERATION_HANDOFF_HANDLE,
            )
            .ok()
                != Some(GENERATION_HANDOFF_HANDLE)
        {
            fail(LOGGING_SERVICE_BOOTSTRAP_FAILED);
        }
        ipc::close(generation_handoff_source)
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        barrier
            .release()
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));

        let mut ready_buffer = [0_u8; 64];
        loop {
            route_broker.pump();
            match ipc::try_receive(readiness_source, &mut ready_buffer) {
                Ok(message) => {
                    if message.sender_process_id != process_id
                        || message.capability.is_some()
                        || message.bytes != spec.ready_message.len()
                        || &ready_buffer[..message.bytes] != spec.ready_message
                    {
                        fail(LOGGING_SERVICE_PROTOCOL_FAILED);
                    }
                    loop {
                        match syscall::try_wait_child(process_id) {
                            Err(error) if error == syscall::Errno::INTERRUPTED => {}
                            Err(error) if error == syscall::Errno::TRY_AGAIN => break,
                            Err(_) => fail(LOGGING_SERVICE_FAILED),
                            Ok(status) if status.continued() => {
                                if service.observe_status(status.raw())
                                    != ServiceStatusDisposition::WaitForNextEvent
                                {
                                    fail(LOGGING_SERVICE_PROTOCOL_FAILED);
                                }
                            }
                            Ok(status) if status.stopped_signal().is_some() => {
                                fail(LOGGING_SERVICE_PROTOCOL_FAILED)
                            }
                            Ok(status) => match service.observe_status(status.raw()) {
                                ServiceStatusDisposition::Restart { backoff_yields } => {
                                    ipc::close(readiness_source)
                                        .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
                                    ipc::close(producer_source)
                                        .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
                                    ipc::close(observer_source)
                                        .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
                                    let _ = syscall::write_all(STDOUT, LOGGING_MESSAGES.restarting);
                                    backoff(backoff_yields);
                                    continue 'attempt;
                                }
                                ServiceStatusDisposition::WaitForNextEvent
                                | ServiceStatusDisposition::Failed => fail(LOGGING_SERVICE_FAILED),
                            },
                        }
                    }
                    service.note_ready();
                    route_broker.publish(generation, producer_source, observer_source);
                    let _ = syscall::write_all(STDOUT, LOGGING_MESSAGES.ready);
                    return LoggingGeneration {
                        generation,
                        readiness_source,
                        producer_source,
                        observer_source,
                        producer_object_id,
                        observer_object_id,
                    };
                }
                Err(error) if error == ipc::Error::TRY_AGAIN => {}
                Err(_) => fail(LOGGING_SERVICE_PROTOCOL_FAILED),
            }

            match syscall::try_wait_child(process_id) {
                Ok(status) => match service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        ipc::close(readiness_source)
                            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
                        ipc::close(producer_source)
                            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
                        ipc::close(observer_source)
                            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
                        let _ = syscall::write_all(STDOUT, LOGGING_MESSAGES.restarting);
                        backoff(backoff_yields);
                        break;
                    }
                    ServiceStatusDisposition::Failed => fail(LOGGING_SERVICE_FAILED),
                },
                Err(error) if error == syscall::Errno::TRY_AGAIN => {}
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(_) => fail(LOGGING_SERVICE_FAILED),
            }
            let _ = syscall::yield_now();
        }
    }
}

fn start_service(
    service: &mut ServiceRuntime,
    generations: &mut ProviderGenerationSequence,
    readiness_endpoint: CapabilityHandle,
    request_endpoint: CapabilityHandle,
    additional_capabilities: &[BootstrapCapability],
    messages: &ServiceMessages,
) -> ProviderGeneration {
    loop {
        let spec = service.spec();
        let generation = generations
            .next_generation()
            .unwrap_or_else(|_| fail(messages.bootstrap_failed));
        let generation_handoff_source =
            ipc::endpoint_create().unwrap_or_else(|_| fail(messages.bootstrap_failed));
        queue_service_generation(generation_handoff_source, generation)
            .unwrap_or_else(|_| fail(messages.bootstrap_failed));
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
            || ipc::grant_child(
                process_id,
                generation_handoff_source,
                Rights::RECEIVE,
                GENERATION_HANDOFF_HANDLE,
            )
            .ok()
                != Some(GENERATION_HANDOFF_HANDLE)
            || additional_capabilities.iter().copied().any(|capability| {
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
        ipc::close(generation_handoff_source).unwrap_or_else(|_| fail(messages.bootstrap_failed));
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
                    return generation;
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
    route_broker: &mut RouteBrokerState,
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
    grant_logging_probe_routes(probe_process_id, route_broker);
    barrier
        .release()
        .unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
    wait_for_logging_probe_exit(service, route_broker, probe_process_id);
    let _ = syscall::write_all(STDOUT, passed_message);
}

fn run_logctl_show(service: &ServiceRuntime, route_broker: &mut RouteBrokerState) {
    let barrier = syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(LOGCTL_FAILED));
    let process_id = syscall::spawn_command_with_barrier(
        LOGCTL_COMMAND,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &barrier,
    )
    .unwrap_or_else(|_| fail(LOGCTL_FAILED));
    if ipc::grant_child(
        process_id,
        route_broker.observer_grant_source,
        Rights::SEND,
        READY_HANDLE,
    )
    .ok()
        != Some(READY_HANDLE)
    {
        fail(LOGCTL_FAILED);
    }
    barrier.release().unwrap_or_else(|_| fail(LOGCTL_FAILED));

    let mut remaining = LOGGING_PROBE_MAX_YIELDS;
    loop {
        require_service_running(service);
        route_broker.pump();
        match syscall::try_wait_child(process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) if status.success() => break,
            Ok(_) => fail(LOGCTL_FAILED),
            Err(error) if error == syscall::Errno::TRY_AGAIN => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(LOGCTL_FAILED),
        }
        if remaining == 0 || syscall::yield_now().is_err() {
            fail(LOGCTL_FAILED);
        }
        remaining -= 1;
    }
    let _ = syscall::write_all(STDOUT, LOGCTL_PASSED);
}

fn run_logging_collector_restart_test(
    service: &mut ServiceRuntime,
    route_broker: &mut RouteBrokerState,
    generations: &mut ProviderGenerationSequence,
    early_log_reader: CapabilityHandle,
    generation: LoggingGeneration,
) -> LoggingGeneration {
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
    grant_logging_probe_routes(probe_process_id, route_broker);
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
        route_broker,
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
    wait_for_logging_service_stop(service, route_broker, old_service_process_id);
    if service.restart_count() != 0 {
        fail(LOGGING_COLLECTOR_RESTART_POLICY_FAILED);
    }
    require_empty_endpoint(generation.producer_source);
    require_empty_endpoint(generation.observer_source);
    if ipc::send(control_endpoint, LOGGING_PROBE_FILL_QUEUE, None).is_err() {
        fail(LOGGING_PROBE_FAILED);
    }
    wait_for_logging_probe_message(
        service,
        route_broker,
        probe_process_id,
        status_endpoint,
        LOGGING_PROBE_BACKPRESSURE,
    );
    if ipc::info(generation.producer_source)
        .map(|info| info.size)
        .ok()
        != Some(8)
    {
        fail(LOGGING_PROBE_FAILED);
    }
    if syscall::signal_process_group(old_service_process_id, signal::CONTINUE).ok() != Some(1) {
        fail(LOGGING_PROBE_FAILED);
    }
    wait_for_logging_service_continue(service, route_broker, old_service_process_id);
    wait_for_logging_probe_exit(service, route_broker, probe_process_id);

    require_empty_endpoint(generation.readiness_source);
    require_empty_endpoint(generation.producer_source);
    require_empty_endpoint(generation.observer_source);
    require_empty_endpoint(status_endpoint);
    require_empty_endpoint(control_endpoint);
    if syscall::signal_process_group(old_service_process_id, signal::TERMINATE).ok() != Some(1) {
        fail(LOGGING_PROBE_FAILED);
    }
    let backoff_yields =
        wait_for_logging_service_restart(service, route_broker, old_service_process_id);
    if service.restart_count() != 1 {
        fail(LOGGING_COLLECTOR_RESTART_POLICY_FAILED);
    }
    let old_generation = generation.generation;
    let old_producer_object_id = generation.producer_object_id;
    let old_observer_object_id = generation.observer_object_id;
    route_broker.withdraw(old_generation);
    generation.close();
    let _ = syscall::write_all(STDOUT, LOGGING_SERVICE_RESTARTING);
    backoff(backoff_yields);
    let replacement =
        start_logging_generation(service, route_broker, generations, early_log_reader);
    if service.process_id() == Some(old_service_process_id)
        || service.restart_count() != 1
        || replacement.generation.get() <= old_generation.get()
        || replacement.producer_object_id == old_producer_object_id
        || replacement.observer_object_id == old_observer_object_id
    {
        fail(LOGGING_COLLECTOR_RESTART_POLICY_FAILED);
    }
    run_logging_probe(service, route_broker, LOGGING_RESTART_PROBE_COMMAND, &[]);
    require_empty_endpoint(replacement.producer_source);
    require_empty_endpoint(replacement.observer_source);
    ipc::close(status_endpoint).unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
    ipc::close(control_endpoint).unwrap_or_else(|_| fail(LOGGING_PROBE_FAILED));
    let _ = syscall::write_all(STDOUT, LOGGING_COLLECTOR_TEST_PASSED);
    replacement
}

fn grant_logging_probe_routes(probe_process_id: ProcessId, route_broker: &RouteBrokerState) {
    if ipc::grant_child(
        probe_process_id,
        route_broker.producer_grant_source,
        Rights::SEND,
        READY_HANDLE,
    )
    .ok()
        != Some(READY_HANDLE)
        || ipc::grant_child(
            probe_process_id,
            route_broker.observer_grant_source,
            Rights::SEND,
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
    route_broker: &mut RouteBrokerState,
    probe_process_id: ProcessId,
    endpoint: CapabilityHandle,
    expected: &[u8],
) {
    let mut remaining = LOGGING_PROBE_MAX_YIELDS;
    let mut buffer = [0_u8; 64];
    loop {
        require_service_running(service);
        route_broker.pump();
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
        yield_logging_probe(&mut remaining);
    }
}

fn wait_for_logging_probe_exit(
    service: &ServiceRuntime,
    route_broker: &mut RouteBrokerState,
    probe_process_id: ProcessId,
) {
    let mut remaining = LOGGING_PROBE_MAX_YIELDS;
    loop {
        require_service_running(service);
        route_broker.pump();
        match syscall::try_wait_child(probe_process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) if status.success() => return,
            Ok(_) => fail(LOGGING_PROBE_FAILED),
            Err(error) if error == syscall::Errno::TRY_AGAIN => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(LOGGING_PROBE_FAILED),
        }
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

fn wait_for_logging_service_stop(
    service: &mut ServiceRuntime,
    route_broker: &mut RouteBrokerState,
    process_id: ProcessId,
) {
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
        route_broker.pump();
        yield_logging_probe(&mut remaining);
    }
}

fn wait_for_logging_service_continue(
    service: &mut ServiceRuntime,
    route_broker: &mut RouteBrokerState,
    process_id: ProcessId,
) {
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
        route_broker.pump();
        yield_logging_probe(&mut remaining);
    }
}

fn wait_for_logging_service_restart(
    service: &mut ServiceRuntime,
    route_broker: &mut RouteBrokerState,
    process_id: ProcessId,
) -> u32 {
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
        route_broker.pump();
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

fn run_service_control_probe(
    command: &[u8],
    passed_message: &[u8],
    control: &mut ServiceControlState,
    registry: ServiceRegistryView<'_>,
) {
    let barrier =
        syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(SERVICE_CONTROL_PROBE_FAILED));
    let process_id = syscall::spawn_command_with_barrier(
        command,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &barrier,
    )
    .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROBE_FAILED));
    if ipc::grant_child(process_id, control.source, Rights::SEND, READY_HANDLE).ok()
        != Some(READY_HANDLE)
    {
        fail(SERVICE_CONTROL_PROBE_FAILED);
    }
    barrier
        .release()
        .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROBE_FAILED));

    let mut remaining = LOGGING_PROBE_MAX_YIELDS;
    loop {
        control.pump(registry);
        match syscall::try_wait_child(process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) if status.success() => break,
            Ok(_) => fail(SERVICE_CONTROL_PROBE_FAILED),
            Err(error) if error == syscall::Errno::TRY_AGAIN => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(SERVICE_CONTROL_PROBE_FAILED),
        }
        if remaining == 0 || syscall::yield_now().is_err() {
            fail(SERVICE_CONTROL_PROBE_FAILED);
        }
        remaining -= 1;
    }
    let _ = syscall::write_all(STDOUT, passed_message);
}

fn spawn_shell(
    observer_ingress: CapabilityHandle,
    service_control_observer: CapabilityHandle,
) -> ProcessId {
    let barrier = syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(SHELL_SPAWN_FAILED));
    let process_id = syscall::spawn_command_with_barrier(
        SHELL_COMMAND,
        SpawnFlags::FOREGROUND | SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &barrier,
    )
    .unwrap_or_else(|_| fail(SHELL_SPAWN_FAILED));
    if ipc::grant_child(
        process_id,
        observer_ingress,
        Rights::SEND | Rights::DUPLICATE,
        READY_HANDLE,
    )
    .ok()
        != Some(READY_HANDLE)
        || ipc::grant_child(
            process_id,
            service_control_observer,
            Rights::SEND | Rights::DUPLICATE,
            SHELL_SERVICE_CONTROL_HANDLE,
        )
        .ok()
            != Some(SHELL_SERVICE_CONTROL_HANDLE)
    {
        fail(SHELL_SPAWN_FAILED);
    }
    barrier
        .release()
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
