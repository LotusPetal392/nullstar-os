#![no_std]
#![no_main]

use nswp_logging::{LOGGING_OBSERVER_ROLE, LOGGING_PRODUCER_ROLE, LOGGING_SERVICE_ID};
use service_control::{
    DesiredState, ListResponse, ObservedState, Operation, RequestId, ServiceControlFailure,
    ServiceControlRequest, ServiceControlResponse, ServiceId, ServiceRecord, TargetOutcome,
    TargetResponse,
};
use service_route::{Authorizer, ProviderGeneration, ProviderGenerationSequence, RouteKey};
use userspace::{
    abi::{INIT_PROCESS_ID, signal},
    early_log,
    ipc::{self, CapabilityHandle, ObjectKind, Rights},
    nullfs_primary_volume, platform,
    service_control::{
        ControlExchange, ControlIngress, LOGGING_SERVICE_ID as CONTROL_LOGGING_SERVICE_ID,
        NULLFS_SERVICE_ID, TMPFS_SERVICE_ID, VFS_SERVICE_ID,
    },
    service_route::{NativeRouteTable, RouteIngress, queue_service_generation},
    supervisor::{
        ReadyDisposition, RestartRequestError, ServiceRuntime, ServiceSpec, ServiceState,
        ServiceStatusDisposition, ShellStatusDisposition, StartRequestError, StopRequestError,
        shell_status_disposition,
    },
    syscall::{self, ProcessId, STDERR, STDOUT, SpawnFlags},
    tmpfs::Mount,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const INIT_READY: &[u8] = b"userspace init ready: pid=1\n";
const LOGGING_SERVICE_COMMAND: &[u8] = b"/logging-service";
const LOGGING_SERVICE_IGNORE_TERMINATE_COMMAND: &[u8] = b"/logging-service --ignore-terminate";
const LOGGING_SERVICE_SUPPRESS_READINESS_COMMAND: &[u8] = b"/logging-service --suppress-readiness";
const LOGGING_SERVICE_READY_MESSAGE: &[u8] = b"service-ready: logging";
const LOGGING_SERVICE_STARTING: &[u8] = b"userspace init: starting logging service\n";
const LOGGING_SERVICE_RESTARTING: &[u8] = b"userspace init: logging service exited; restarting\n";
const LOGGING_SERVICE_READY: &[u8] = b"userspace init: logging service ready\n";
const LOGGING_SERVICE_FAILED: &[u8] = b"userspace init: logging service exhausted restart budget\n";
const LOGGING_SERVICE_FORCE_TERMINATING: &[u8] =
    b"userspace init: logging service termination grace expired; forcing exit\n";
const LOGGING_SERVICE_READINESS_TIMEOUT: &[u8] =
    b"userspace init: logging service readiness deadline expired; forcing exit\n";
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
const SV_STATUS_NULLFS_COMMAND: &[u8] = b"/sv status nullfs";
const SV_START_LOGGING_COMMAND: &[u8] = b"/sv start logging";
const SV_STOP_LOGGING_COMMAND: &[u8] = b"/sv stop logging";
const SV_RESTART_LOGGING_COMMAND: &[u8] = b"/sv restart logging";
const SV_RESTART_NULLFS_COMMAND: &[u8] = b"/sv restart nullfs";
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
const NULLFS_RESTART_PROBE_OFFLINE: &[u8] = b"nullfs-restart: offline failure observed";
const NULLFS_RESTART_PROBE_REPLACEMENT: &[u8] = b"nullfs-restart: replacement registered";
const NULLFS_RESTART_PROBE_PASSED: &[u8] =
    b"userspace init: NullFS restart persistent VFS mutation and stale descriptors verified\n";
const NULLFS_RESTART_PROBE_FAILED: &[u8] = b"userspace init: NullFS restart probe failed\n";
const LOGGING_LIFECYCLE_TEST_PASSED: &[u8] = b"userspace init: logging live start, stop, route withdrawal, restart fencing, and generation replacement verified\n";
const LOGGING_LIFECYCLE_TEST_FAILED: &[u8] = b"userspace init: logging lifecycle test failed\n";
const BOOT_MODE_PATH: &[u8] = b"/BOOTMODE";
const NULLFS_RESTART_TEST_BOOT_MODE: &[u8] = b"nullfs-restart-test\n";
const LOGGING_LIFECYCLE_TEST_BOOT_MODE: &[u8] = b"logging-lifecycle-test\n";
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
const SV_OBSERVATION_HANDLE: u64 = 1;
const SV_MUTATION_HANDLE: u64 = 2;
const NULLFS_BLOCK_HANDLE: u64 = 3;
const LOGGING_OBSERVER_INGRESS_HANDLE: u64 = 3;
const LOGGING_EARLY_LOG_HANDLE: u64 = 4;
const GENERATION_HANDOFF_HANDLE: u64 = 5;
const SHELL_SERVICE_CONTROL_HANDLE: u64 = 2;
const SHELL_SERVICE_CONTROL_MUTATION_HANDLE: u64 = 3;
const MAX_BOOTSTRAP_ROUTES: usize = 2;
const ROUTE_PUMP_BUDGET: usize = 4;
const SERVICE_CONTROL_PUMP_BUDGET: usize = 4;
const LOGGING_TERMINATION_GRACE_YIELDS: u32 = 64;
const LOGGING_READINESS_GRACE_YIELDS: u32 = 2_048;
const LOGGING_TEST_READINESS_GRACE_YIELDS: u32 = 64;
const LOGGING_FORCE_TERMINATION_ATTEMPTS: u32 = 64;
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

struct ServiceRegistryMut<'a> {
    logging: &'a mut ServiceRuntime,
    logging_generation: ProviderGeneration,
    nullfs: &'a mut ServiceRuntime,
    nullfs_generation: ProviderGeneration,
    tmpfs: &'a mut ServiceRuntime,
    tmpfs_generation: ProviderGeneration,
    vfs: &'a mut ServiceRuntime,
    vfs_generation: ProviderGeneration,
}

struct ServiceControlState {
    observation_source: CapabilityHandle,
    observation_ingress: ControlIngress,
    mutation_source: CapabilityHandle,
    mutation_ingress: ControlIngress,
}

impl ServiceControlState {
    fn new() -> Self {
        let observation_source =
            ipc::endpoint_create().unwrap_or_else(|_| fail(SERVICE_CONTROL_BOOTSTRAP_FAILED));
        let observation_receive = ipc::duplicate(observation_source, Rights::RECEIVE)
            .unwrap_or_else(|_| fail(SERVICE_CONTROL_BOOTSTRAP_FAILED));
        let observation_ingress = ControlIngress::bind(observation_receive)
            .unwrap_or_else(|_| fail(SERVICE_CONTROL_BOOTSTRAP_FAILED));
        let mutation_source =
            ipc::endpoint_create().unwrap_or_else(|_| fail(SERVICE_CONTROL_BOOTSTRAP_FAILED));
        let mutation_receive = ipc::duplicate(mutation_source, Rights::RECEIVE)
            .unwrap_or_else(|_| fail(SERVICE_CONTROL_BOOTSTRAP_FAILED));
        let mutation_ingress = ControlIngress::bind(mutation_receive)
            .unwrap_or_else(|_| fail(SERVICE_CONTROL_BOOTSTRAP_FAILED));
        Self {
            observation_source,
            observation_ingress,
            mutation_source,
            mutation_ingress,
        }
    }

    fn pump_observation(&mut self, registry: ServiceRegistryView<'_>) {
        for _ in 0..SERVICE_CONTROL_PUMP_BUDGET {
            let request = match self.observation_ingress.try_accept() {
                Ok(Some(request)) => request,
                Ok(None) => break,
                Err(_) => continue,
            };
            let response = service_control_response(request.request(), registry);
            let _ = request.reply(response);
        }
    }

    fn pump_mutation(&mut self, registry: ServiceRegistryMut<'_>) -> usize {
        let mut processed = 0;
        for _ in 0..SERVICE_CONTROL_PUMP_BUDGET {
            let request = match self.mutation_ingress.try_accept() {
                Ok(Some(request)) => request,
                Ok(None) => break,
                Err(_) => {
                    processed += 1;
                    continue;
                }
            };
            processed += 1;
            let response = service_mutation_response(
                request.request(),
                ServiceRegistryMut {
                    logging: &mut *registry.logging,
                    logging_generation: registry.logging_generation,
                    nullfs: &mut *registry.nullfs,
                    nullfs_generation: registry.nullfs_generation,
                    tmpfs: &mut *registry.tmpfs,
                    tmpfs_generation: registry.tmpfs_generation,
                    vfs: &mut *registry.vfs,
                    vfs_generation: registry.vfs_generation,
                },
            );
            let _ = request.reply(response);
        }
        processed
    }

    fn drain_mutation(&mut self, registry: ServiceRegistryMut<'_>) {
        loop {
            let processed = self.pump_mutation(ServiceRegistryMut {
                logging: &mut *registry.logging,
                logging_generation: registry.logging_generation,
                nullfs: &mut *registry.nullfs,
                nullfs_generation: registry.nullfs_generation,
                tmpfs: &mut *registry.tmpfs,
                tmpfs_generation: registry.tmpfs_generation,
                vfs: &mut *registry.vfs,
                vfs_generation: registry.vfs_generation,
            });
            if processed < SERVICE_CONTROL_PUMP_BUDGET {
                break;
            }
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

fn service_mutation_response(
    request: ServiceControlRequest,
    registry: ServiceRegistryMut<'_>,
) -> ServiceControlResponse {
    match request {
        ServiceControlRequest::List { cursor } => ServiceControlResponse::list(
            ListResponse::failure(cursor, ServiceControlFailure::AccessDenied),
        ),
        ServiceControlRequest::Status { service } => ServiceControlResponse::status(
            TargetResponse::failure(service, ServiceControlFailure::AccessDenied),
        ),
        ServiceControlRequest::Start { service } => {
            let response = if service == CONTROL_LOGGING_SERVICE_ID {
                request_service_start(service, registry.logging, registry.logging_generation)
            } else if matches!(
                service,
                NULLFS_SERVICE_ID | TMPFS_SERVICE_ID | VFS_SERVICE_ID
            ) {
                TargetResponse::failure(service, ServiceControlFailure::Unsupported)
            } else {
                TargetResponse::failure(service, ServiceControlFailure::NotFound)
            };
            ServiceControlResponse::start(response)
                .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROTOCOL_FAILED))
        }
        ServiceControlRequest::Stop { service } => {
            let response = if service == CONTROL_LOGGING_SERVICE_ID {
                request_service_stop(service, registry.logging, registry.logging_generation)
            } else if matches!(
                service,
                NULLFS_SERVICE_ID | TMPFS_SERVICE_ID | VFS_SERVICE_ID
            ) {
                TargetResponse::failure(service, ServiceControlFailure::Unsupported)
            } else {
                TargetResponse::failure(service, ServiceControlFailure::NotFound)
            };
            ServiceControlResponse::stop(response)
                .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROTOCOL_FAILED))
        }
        ServiceControlRequest::Restart { service } => {
            let response = if service == CONTROL_LOGGING_SERVICE_ID {
                request_service_restart(service, registry.logging, registry.logging_generation)
            } else if service == NULLFS_SERVICE_ID {
                request_service_restart(service, registry.nullfs, registry.nullfs_generation)
            } else if service == TMPFS_SERVICE_ID {
                request_service_restart(service, registry.tmpfs, registry.tmpfs_generation)
            } else if service == VFS_SERVICE_ID {
                request_service_restart(service, registry.vfs, registry.vfs_generation)
            } else {
                TargetResponse::failure(service, ServiceControlFailure::NotFound)
            };
            ServiceControlResponse::restart(response)
                .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROTOCOL_FAILED))
        }
    }
}

fn request_service_start(
    service: ServiceId,
    runtime: &mut ServiceRuntime,
    generation: ProviderGeneration,
) -> TargetResponse {
    match runtime.request_start() {
        Ok(()) => TargetResponse::success(service_record(service, runtime, generation)),
        Err(StartRequestError::InvalidState) => {
            TargetResponse::failure(service, ServiceControlFailure::InvalidState)
        }
    }
}

fn request_service_stop(
    service: ServiceId,
    runtime: &mut ServiceRuntime,
    generation: ProviderGeneration,
) -> TargetResponse {
    let request = match runtime.request_stop() {
        Ok(request) => request,
        Err(StopRequestError::InvalidState) => {
            return TargetResponse::failure(service, ServiceControlFailure::InvalidState);
        }
        Err(StopRequestError::TransitionExhausted) => {
            return TargetResponse::failure(service, ServiceControlFailure::Exhausted);
        }
    };
    if let Some(process_group) = request.process_group()
        && !matches!(
            syscall::signal_process_group(process_group, signal::TERMINATE),
            Ok(signaled) if signaled != 0
        )
    {
        if runtime.cancel_stop(request).is_err() {
            fail(SERVICE_CONTROL_PROTOCOL_FAILED);
        }
        return TargetResponse::failure(service, ServiceControlFailure::Busy);
    }
    TargetResponse::success(service_record(service, runtime, generation))
}

fn request_service_restart(
    service: ServiceId,
    runtime: &mut ServiceRuntime,
    generation: ProviderGeneration,
) -> TargetResponse {
    let process_group = match runtime.request_restart() {
        Ok(process_group) => process_group,
        Err(RestartRequestError::AlreadyPending) => {
            return TargetResponse::failure(service, ServiceControlFailure::Busy);
        }
        Err(RestartRequestError::InvalidState) => {
            return TargetResponse::failure(service, ServiceControlFailure::InvalidState);
        }
    };
    if !matches!(
        syscall::signal_process_group(process_group, signal::TERMINATE),
        Ok(signaled) if signaled != 0
    ) {
        runtime.cancel_restart();
        return TargetResponse::failure(service, ServiceControlFailure::Busy);
    }
    TargetResponse::success(service_record(service, runtime, generation))
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
        ServiceState::Stopping => (ObservedState::Stopping, Some(managed_generation)),
        ServiceState::Restarting => (ObservedState::Terminating, Some(managed_generation)),
        ServiceState::Backoff => (ObservedState::Stopped, None),
        ServiceState::Failed => (ObservedState::Quarantined, None),
    };
    ServiceRecord::new(service, generation, observed, runtime.desired_state())
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
    readiness_received: bool,
    child_stopped: bool,
    routes_published: bool,
    readiness_yields_remaining: u32,
    readiness_force_termination_sent: bool,
    force_termination_attempts_remaining: u32,
}

impl LoggingGeneration {
    fn close(self) {
        ipc::close(self.readiness_source)
            .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        ipc::close(self.producer_source).unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
        ipc::close(self.observer_source).unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
    }
}

struct LoggingLifecycle {
    current: Option<LoggingGeneration>,
    last_generation: ProviderGeneration,
    backoff_yields_remaining: u32,
    termination_grace_yields_remaining: Option<u32>,
    force_termination_sent: bool,
    force_termination_attempts_remaining: u32,
}

impl LoggingLifecycle {
    fn running(generation: LoggingGeneration) -> Self {
        if !generation.routes_published {
            fail(LOGGING_SERVICE_PROTOCOL_FAILED);
        }
        Self {
            last_generation: generation.generation,
            current: Some(generation),
            backoff_yields_remaining: 0,
            termination_grace_yields_remaining: None,
            force_termination_sent: false,
            force_termination_attempts_remaining: 0,
        }
    }

    const fn generation(&self) -> ProviderGeneration {
        self.last_generation
    }

    fn withdraw_routes(&mut self, route_broker: &mut RouteBrokerState) {
        let Some(generation) = self.current.as_mut() else {
            return;
        };
        if generation.routes_published {
            route_broker.withdraw(generation.generation);
            generation.routes_published = false;
        }
    }

    fn close_current(&mut self) {
        if let Some(generation) = self.current.take() {
            generation.close();
        }
        self.termination_grace_yields_remaining = None;
        self.force_termination_sent = false;
        self.force_termination_attempts_remaining = 0;
    }

    fn install(&mut self, generation: LoggingGeneration) {
        if self.current.is_some() {
            fail(LOGGING_SERVICE_PROTOCOL_FAILED);
        }
        self.last_generation = generation.generation;
        self.current = Some(generation);
    }
}

#[derive(Clone, Copy)]
enum LoggingLifecycleCheckPhase {
    SpawnStop,
    WaitStopClient(ProcessId),
    WaitStopped,
    WaitStartClient(ProcessId),
    WaitReady,
    WaitReadyStatus(ProcessId),
    WaitRestartPending(ProcessId),
    WaitRestartResults(ProcessId),
    WaitRestartReady,
    WaitReadyDuplicate,
    WaitRestartFence,
    BeginUnsupported(usize),
    WaitUnsupported(usize),
    SpawnReadinessTimeout,
    WaitReadinessRestartClient(ProcessId),
    WaitReadinessFailure,
    Complete,
}

struct LoggingLifecycleCheck {
    phase: LoggingLifecycleCheckPhase,
    initial_generation: ProviderGeneration,
    initial_producer_object_id: u64,
    initial_observer_object_id: u64,
    started_generation: Option<ProviderGeneration>,
    started_producer_object_id: u64,
    started_observer_object_id: u64,
    initial_restart_count: u32,
    restart_routes_withdrawn: bool,
    restart_client_completed: bool,
    duplicate_restart_completed: bool,
    pending_exchange: Option<ControlExchange>,
    filesystem_records: [ServiceRecord; 3],
}

impl LoggingLifecycleCheck {
    fn new(
        service: &ServiceRuntime,
        lifecycle: &LoggingLifecycle,
        route_broker: &RouteBrokerState,
        filesystem_records: [ServiceRecord; 3],
    ) -> Self {
        let generation = lifecycle
            .current
            .as_ref()
            .unwrap_or_else(|| fail(LOGGING_LIFECYCLE_TEST_FAILED));
        if service.state() != ServiceState::Running
            || service.desired_state() != DesiredState::Running
            || !generation.routes_published
            || published_route(route_broker, LOGGING_PRODUCER_KEY)
                != Some((generation.generation, generation.producer_object_id))
            || published_route(route_broker, LOGGING_OBSERVER_KEY)
                != Some((generation.generation, generation.observer_object_id))
        {
            fail(LOGGING_LIFECYCLE_TEST_FAILED);
        }
        Self {
            phase: LoggingLifecycleCheckPhase::SpawnStop,
            initial_generation: generation.generation,
            initial_producer_object_id: generation.producer_object_id,
            initial_observer_object_id: generation.observer_object_id,
            started_generation: None,
            started_producer_object_id: 0,
            started_observer_object_id: 0,
            initial_restart_count: service.restart_count(),
            restart_routes_withdrawn: false,
            restart_client_completed: false,
            duplicate_restart_completed: false,
            pending_exchange: None,
            filesystem_records,
        }
    }

    fn begin_mutation(
        &mut self,
        control: &ServiceControlState,
        request_id: u64,
        request: ServiceControlRequest,
    ) {
        if self.pending_exchange.is_some() {
            fail(LOGGING_LIFECYCLE_TEST_FAILED);
        }
        let authority = ipc::duplicate(control.mutation_source, Rights::SEND)
            .unwrap_or_else(|_| fail(LOGGING_LIFECYCLE_TEST_FAILED));
        let exchange = ControlExchange::begin_mutation(
            authority,
            RequestId::new(request_id).unwrap_or_else(|| fail(LOGGING_LIFECYCLE_TEST_FAILED)),
            request,
        )
        .unwrap_or_else(|_| fail(LOGGING_LIFECYCLE_TEST_FAILED));
        ipc::close(authority).unwrap_or_else(|_| fail(LOGGING_LIFECYCLE_TEST_FAILED));
        self.pending_exchange = Some(exchange);
    }

    fn try_complete_mutation(&mut self) -> Option<ServiceControlResponse> {
        let exchange = self
            .pending_exchange
            .as_mut()
            .unwrap_or_else(|| fail(LOGGING_LIFECYCLE_TEST_FAILED));
        let reply = match exchange.try_complete() {
            Ok(Some(reply)) => reply,
            Ok(None) => return None,
            Err(_) => fail(LOGGING_LIFECYCLE_TEST_FAILED),
        };
        if reply.server_process_id() != INIT_PROCESS_ID {
            fail(LOGGING_LIFECYCLE_TEST_FAILED);
        }
        self.pending_exchange = None;
        Some(reply.response())
    }

    fn note_restart_route_state(&mut self, route_broker: &RouteBrokerState) {
        if published_route(route_broker, LOGGING_PRODUCER_KEY).is_none()
            && published_route(route_broker, LOGGING_OBSERVER_KEY).is_none()
        {
            self.restart_routes_withdrawn = true;
        }
    }

    fn advance(
        &mut self,
        service: &ServiceRuntime,
        lifecycle: &LoggingLifecycle,
        route_broker: &RouteBrokerState,
        control: &ServiceControlState,
        filesystem_records: [ServiceRecord; 3],
    ) {
        match self.phase {
            LoggingLifecycleCheckPhase::SpawnStop => {
                let process_id = spawn_sv_command(
                    SV_STOP_LOGGING_COMMAND,
                    control.mutation_source,
                    SV_MUTATION_HANDLE,
                );
                self.phase = LoggingLifecycleCheckPhase::WaitStopClient(process_id);
            }
            LoggingLifecycleCheckPhase::WaitStopClient(process_id) => {
                let Some(success) = try_wait_probe(process_id) else {
                    return;
                };
                if !success
                    || service.desired_state() != DesiredState::Stopped
                    || !matches!(
                        service.state(),
                        ServiceState::Stopping | ServiceState::Stopped
                    )
                    || published_route(route_broker, LOGGING_PRODUCER_KEY).is_some()
                    || published_route(route_broker, LOGGING_OBSERVER_KEY).is_some()
                {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                self.phase = LoggingLifecycleCheckPhase::WaitStopped;
            }
            LoggingLifecycleCheckPhase::WaitStopped => {
                if service.state() != ServiceState::Stopped {
                    return;
                }
                if service.desired_state() != DesiredState::Stopped
                    || lifecycle.current.is_some()
                    || service.restart_count() != self.initial_restart_count
                {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                let process_id = spawn_sv_command(
                    SV_START_LOGGING_COMMAND,
                    control.mutation_source,
                    SV_MUTATION_HANDLE,
                );
                self.phase = LoggingLifecycleCheckPhase::WaitStartClient(process_id);
            }
            LoggingLifecycleCheckPhase::WaitStartClient(process_id) => {
                let Some(success) = try_wait_probe(process_id) else {
                    return;
                };
                if !success || service.desired_state() != DesiredState::Running {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                self.phase = LoggingLifecycleCheckPhase::WaitReady;
            }
            LoggingLifecycleCheckPhase::WaitReady => {
                if service.state() != ServiceState::Running {
                    return;
                }
                let generation = lifecycle
                    .current
                    .as_ref()
                    .unwrap_or_else(|| fail(LOGGING_LIFECYCLE_TEST_FAILED));
                if service.restart_count() != self.initial_restart_count
                    || generation.generation.get()
                        != self.initial_generation.get().saturating_add(1)
                    || generation.producer_object_id == self.initial_producer_object_id
                    || generation.observer_object_id == self.initial_observer_object_id
                    || published_route(route_broker, LOGGING_PRODUCER_KEY)
                        != Some((generation.generation, generation.producer_object_id))
                    || published_route(route_broker, LOGGING_OBSERVER_KEY)
                        != Some((generation.generation, generation.observer_object_id))
                {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                self.started_generation = Some(generation.generation);
                self.started_producer_object_id = generation.producer_object_id;
                self.started_observer_object_id = generation.observer_object_id;
                let process_id = spawn_sv_command(
                    SV_STATUS_LOGGING_COMMAND,
                    control.observation_source,
                    SV_OBSERVATION_HANDLE,
                );
                self.phase = LoggingLifecycleCheckPhase::WaitReadyStatus(process_id);
            }
            LoggingLifecycleCheckPhase::WaitReadyStatus(process_id) => {
                let Some(success) = try_wait_probe(process_id) else {
                    return;
                };
                if !success {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                let process_id = spawn_sv_command(
                    SV_RESTART_LOGGING_COMMAND,
                    control.mutation_source,
                    SV_MUTATION_HANDLE,
                );
                self.phase = LoggingLifecycleCheckPhase::WaitRestartPending(process_id);
            }
            LoggingLifecycleCheckPhase::WaitRestartPending(process_id) => {
                self.note_restart_route_state(route_broker);
                if !service.controlled_restart_pending() {
                    if let Some(success) = try_wait_probe(process_id)
                        && !success
                    {
                        fail(LOGGING_LIFECYCLE_TEST_FAILED);
                    }
                    return;
                }
                if service.desired_state() != DesiredState::Running {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                self.begin_mutation(
                    control,
                    0x4c4f_4747_494e_4701,
                    ServiceControlRequest::Restart {
                        service: CONTROL_LOGGING_SERVICE_ID,
                    },
                );
                self.phase = LoggingLifecycleCheckPhase::WaitRestartResults(process_id);
            }
            LoggingLifecycleCheckPhase::WaitRestartResults(process_id) => {
                self.note_restart_route_state(route_broker);
                if !self.restart_client_completed
                    && let Some(success) = try_wait_probe(process_id)
                {
                    if !success {
                        fail(LOGGING_LIFECYCLE_TEST_FAILED);
                    }
                    self.restart_client_completed = true;
                }
                if !self.duplicate_restart_completed
                    && let Some(response) = self.try_complete_mutation()
                {
                    require_control_failure(
                        response,
                        Operation::Restart,
                        CONTROL_LOGGING_SERVICE_ID,
                        ServiceControlFailure::Busy,
                    );
                    self.duplicate_restart_completed = true;
                }
                if self.restart_client_completed && self.duplicate_restart_completed {
                    self.phase = LoggingLifecycleCheckPhase::WaitRestartReady;
                }
            }
            LoggingLifecycleCheckPhase::WaitRestartReady => {
                self.note_restart_route_state(route_broker);
                if service.state() != ServiceState::Running {
                    return;
                }
                let previous_generation = self
                    .started_generation
                    .unwrap_or_else(|| fail(LOGGING_LIFECYCLE_TEST_FAILED));
                let generation = lifecycle
                    .current
                    .as_ref()
                    .unwrap_or_else(|| fail(LOGGING_LIFECYCLE_TEST_FAILED));
                if !self.restart_routes_withdrawn
                    || !service.controlled_restart_pending()
                    || service.restart_count() != self.initial_restart_count
                    || generation.generation.get() != previous_generation.get().saturating_add(1)
                    || generation.producer_object_id == self.started_producer_object_id
                    || generation.observer_object_id == self.started_observer_object_id
                    || published_route(route_broker, LOGGING_PRODUCER_KEY)
                        != Some((generation.generation, generation.producer_object_id))
                    || published_route(route_broker, LOGGING_OBSERVER_KEY)
                        != Some((generation.generation, generation.observer_object_id))
                {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                self.begin_mutation(
                    control,
                    0x4c4f_4747_494e_4702,
                    ServiceControlRequest::Restart {
                        service: CONTROL_LOGGING_SERVICE_ID,
                    },
                );
                self.phase = LoggingLifecycleCheckPhase::WaitReadyDuplicate;
            }
            LoggingLifecycleCheckPhase::WaitReadyDuplicate => {
                let Some(response) = self.try_complete_mutation() else {
                    return;
                };
                require_control_failure(
                    response,
                    Operation::Restart,
                    CONTROL_LOGGING_SERVICE_ID,
                    ServiceControlFailure::Busy,
                );
                self.phase = LoggingLifecycleCheckPhase::WaitRestartFence;
            }
            LoggingLifecycleCheckPhase::WaitRestartFence => {
                if service.controlled_restart_pending() {
                    return;
                }
                self.phase = LoggingLifecycleCheckPhase::BeginUnsupported(0);
            }
            LoggingLifecycleCheckPhase::BeginUnsupported(index) => {
                let (operation, service_id, request) = unsupported_filesystem_request(index);
                self.begin_mutation(control, 0x4c4f_4747_494e_4710 + index as u64, request);
                if operation != request.operation() || request.service() != Some(service_id) {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                self.phase = LoggingLifecycleCheckPhase::WaitUnsupported(index);
            }
            LoggingLifecycleCheckPhase::WaitUnsupported(index) => {
                let Some(response) = self.try_complete_mutation() else {
                    return;
                };
                let (operation, service_id, _) = unsupported_filesystem_request(index);
                require_control_failure(
                    response,
                    operation,
                    service_id,
                    ServiceControlFailure::Unsupported,
                );
                if filesystem_records != self.filesystem_records {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                if index == 5 {
                    self.phase = LoggingLifecycleCheckPhase::SpawnReadinessTimeout;
                } else {
                    self.phase = LoggingLifecycleCheckPhase::BeginUnsupported(index + 1);
                }
            }
            LoggingLifecycleCheckPhase::SpawnReadinessTimeout => {
                let process_id = spawn_sv_command(
                    SV_RESTART_LOGGING_COMMAND,
                    control.mutation_source,
                    SV_MUTATION_HANDLE,
                );
                self.phase = LoggingLifecycleCheckPhase::WaitReadinessRestartClient(process_id);
            }
            LoggingLifecycleCheckPhase::WaitReadinessRestartClient(process_id) => {
                let Some(success) = try_wait_probe(process_id) else {
                    return;
                };
                if !success
                    || service.desired_state() != DesiredState::Running
                    || !service.controlled_restart_pending()
                    || published_route(route_broker, LOGGING_PRODUCER_KEY).is_some()
                    || published_route(route_broker, LOGGING_OBSERVER_KEY).is_some()
                {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                self.phase = LoggingLifecycleCheckPhase::WaitReadinessFailure;
            }
            LoggingLifecycleCheckPhase::WaitReadinessFailure => {
                if service.state() != ServiceState::Failed {
                    return;
                }
                if service.desired_state() != DesiredState::Running
                    || service.process_id().is_some()
                    || service.controlled_restart_pending()
                    || service.restart_count() != LOGGING_SERVICE.restart_limit
                    || lifecycle.current.is_some()
                    || published_route(route_broker, LOGGING_PRODUCER_KEY).is_some()
                    || published_route(route_broker, LOGGING_OBSERVER_KEY).is_some()
                {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                let _ = syscall::write_all(STDOUT, LOGGING_LIFECYCLE_TEST_PASSED);
                self.phase = LoggingLifecycleCheckPhase::Complete;
            }
            LoggingLifecycleCheckPhase::Complete => {}
        }
    }
}

fn require_control_failure(
    response: ServiceControlResponse,
    operation: Operation,
    service: ServiceId,
    expected: ServiceControlFailure,
) {
    let target = response
        .target_response()
        .unwrap_or_else(|| fail(LOGGING_LIFECYCLE_TEST_FAILED));
    if response.operation() != operation
        || target.service() != service
        || target.outcome() != TargetOutcome::Failure(expected)
    {
        fail(LOGGING_LIFECYCLE_TEST_FAILED);
    }
}

fn unsupported_filesystem_request(index: usize) -> (Operation, ServiceId, ServiceControlRequest) {
    match index {
        0 => (
            Operation::Stop,
            NULLFS_SERVICE_ID,
            ServiceControlRequest::Stop {
                service: NULLFS_SERVICE_ID,
            },
        ),
        1 => (
            Operation::Start,
            NULLFS_SERVICE_ID,
            ServiceControlRequest::Start {
                service: NULLFS_SERVICE_ID,
            },
        ),
        2 => (
            Operation::Stop,
            TMPFS_SERVICE_ID,
            ServiceControlRequest::Stop {
                service: TMPFS_SERVICE_ID,
            },
        ),
        3 => (
            Operation::Start,
            TMPFS_SERVICE_ID,
            ServiceControlRequest::Start {
                service: TMPFS_SERVICE_ID,
            },
        ),
        4 => (
            Operation::Stop,
            VFS_SERVICE_ID,
            ServiceControlRequest::Stop {
                service: VFS_SERVICE_ID,
            },
        ),
        5 => (
            Operation::Start,
            VFS_SERVICE_ID,
            ServiceControlRequest::Start {
                service: VFS_SERVICE_ID,
            },
        ),
        _ => fail(LOGGING_LIFECYCLE_TEST_FAILED),
    }
}

fn published_route(
    route_broker: &RouteBrokerState,
    key: RouteKey,
) -> Option<(ProviderGeneration, u64)> {
    let route = route_broker.routes.published(key)?;
    let object_id = ipc::info(*route.authority).ok()?.object_id;
    Some((route.generation, object_id))
}

fn spawn_sv_command(
    command: &[u8],
    authority: CapabilityHandle,
    target_handle: CapabilityHandle,
) -> ProcessId {
    let barrier =
        syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(LOGGING_LIFECYCLE_TEST_FAILED));
    let process_id = syscall::spawn_command_with_barrier(
        command,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &barrier,
    )
    .unwrap_or_else(|_| fail(LOGGING_LIFECYCLE_TEST_FAILED));
    if ipc::grant_child(process_id, authority, Rights::SEND, target_handle).ok()
        != Some(target_handle)
    {
        fail(LOGGING_LIFECYCLE_TEST_FAILED);
    }
    barrier
        .release()
        .unwrap_or_else(|_| fail(LOGGING_LIFECYCLE_TEST_FAILED));
    process_id
}

fn try_wait_probe(process_id: ProcessId) -> Option<bool> {
    match syscall::try_wait_child(process_id) {
        Ok(status) if status.continued() || status.stopped_signal().is_some() => None,
        Ok(status) => Some(status.success()),
        Err(error) if error == syscall::Errno::TRY_AGAIN => None,
        Err(error) if error == syscall::Errno::INTERRUPTED => None,
        Err(_) => fail(LOGGING_LIFECYCLE_TEST_FAILED),
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
    let boot_mode = init_boot_mode();
    let nullfs_restart_test = boot_mode == InitBootMode::NullfsRestartTest;
    let logging_lifecycle_test = boot_mode == InitBootMode::LoggingLifecycleTest;

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
    let mut logging_lifecycle = LoggingLifecycle::running(logging_generation);

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
        &mut nullfs_request_endpoint,
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
    let mut request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
    let mut service = ServiceRuntime::new(TMPFS_SERVICE);
    let mut tmpfs_generations = ProviderGenerationSequence::new();
    let mut tmpfs_generation = start_service(
        &mut service,
        &mut tmpfs_generations,
        readiness_endpoint,
        &mut request_endpoint,
        &[],
        &TMPFS_MESSAGES,
    );
    register_tmpfs_proxy(tmpfs_generation, request_endpoint);
    run_tmpfs_probe(request_endpoint);
    let vfs_readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
    let mut vfs_request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
    let mut vfs_service = ServiceRuntime::new(VFS_SERVICE);
    let mut vfs_generations = ProviderGenerationSequence::new();
    let mut vfs_generation = start_service(
        &mut vfs_service,
        &mut vfs_generations,
        vfs_readiness_endpoint,
        &mut vfs_request_endpoint,
        &[],
        &VFS_MESSAGES,
    );
    register_vfs_router(vfs_generation, vfs_request_endpoint);
    let mut service_control = ServiceControlState::new();
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
            &mut service_control,
            ServiceRegistryMut {
                logging: &mut logging_service,
                logging_generation: logging_lifecycle.generation(),
                nullfs: &mut nullfs_service,
                nullfs_generation,
                tmpfs: &mut service,
                tmpfs_generation,
                vfs: &mut vfs_service,
                vfs_generation,
            },
            &mut nullfs_generations,
            nullfs_readiness_endpoint,
            &mut nullfs_request_endpoint,
            nullfs_block_capability,
        );
        run_service_control_probe(
            SV_STATUS_NULLFS_COMMAND,
            b"userspace init: sv status nullfs passed\n",
            &mut service_control,
            ServiceRegistryView {
                logging: &logging_service,
                logging_generation: logging_lifecycle.generation(),
                nullfs: &nullfs_service,
                nullfs_generation,
                tmpfs: &service,
                tmpfs_generation,
                vfs: &vfs_service,
                vfs_generation,
            },
        );
        let _ = syscall::write_all(STDOUT, NULLFS_RESTART_PROBE_PASSED);
    } else {
        run_probe(
            VFS_READINESS_PROBE_COMMAND,
            vfs_request_endpoint,
            VFS_READINESS_PROBE_FAILED,
            VFS_READINESS_PROBE_PASSED,
        );
    }

    let registry = ServiceRegistryView {
        logging: &logging_service,
        logging_generation: logging_lifecycle.generation(),
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
    let mut shell_process_id = spawn_shell(
        route_broker.observer_grant_source,
        service_control.observation_source,
        service_control.mutation_source,
    );
    let mut logging_lifecycle_check = if logging_lifecycle_test {
        Some(LoggingLifecycleCheck::new(
            &logging_service,
            &logging_lifecycle,
            &route_broker,
            [
                service_record(NULLFS_SERVICE_ID, &nullfs_service, nullfs_generation),
                service_record(TMPFS_SERVICE_ID, &service, tmpfs_generation),
                service_record(VFS_SERVICE_ID, &vfs_service, vfs_generation),
            ],
        ))
    } else {
        None
    };

    loop {
        poll_logging_child(
            &mut logging_service,
            &mut logging_lifecycle,
            &mut route_broker,
        );
        let logging_restart_ready = logging_service.state() == ServiceState::Running
            && logging_service.controlled_restart_pending();
        let mutation_count = service_control.pump_mutation(ServiceRegistryMut {
            logging: &mut logging_service,
            logging_generation: logging_lifecycle.generation(),
            nullfs: &mut nullfs_service,
            nullfs_generation,
            tmpfs: &mut service,
            tmpfs_generation,
            vfs: &mut vfs_service,
            vfs_generation,
        });
        sync_logging_routes(&logging_service, &mut logging_lifecycle, &mut route_broker);
        if logging_restart_ready
            && mutation_count < SERVICE_CONTROL_PUMP_BUDGET
            && logging_service.state() == ServiceState::Running
        {
            logging_service.complete_restart();
        }
        advance_logging_lifecycle(
            &mut logging_service,
            &mut logging_lifecycle,
            &mut route_broker,
            &mut logging_generations,
            logging_early_log_reader,
            logging_lifecycle_test,
        );
        service_control.pump_observation(ServiceRegistryView {
            logging: &logging_service,
            logging_generation: logging_lifecycle.generation(),
            nullfs: &nullfs_service,
            nullfs_generation,
            tmpfs: &service,
            tmpfs_generation,
            vfs: &vfs_service,
            vfs_generation,
        });
        route_broker.pump();

        if let Some(service_process_id) = nullfs_service.process_id() {
            match syscall::try_wait_child(service_process_id) {
                Ok(status) => match nullfs_service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        let _ = syscall::write_all(STDOUT, NULLFS_SERVICE_RESTARTING);
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Nullfs,
                            nullfs_generation,
                            NULLFS_SERVICE_FAILED,
                        );
                        ipc::close(nullfs_request_endpoint)
                            .unwrap_or_else(|_| fail(NULLFS_SERVICE_FAILED));
                        nullfs_request_endpoint = ipc::endpoint_create()
                            .unwrap_or_else(|_| fail(NULLFS_SERVICE_BOOTSTRAP_FAILED));
                        backoff(backoff_yields);
                        nullfs_generation = start_service(
                            &mut nullfs_service,
                            &mut nullfs_generations,
                            nullfs_readiness_endpoint,
                            &mut nullfs_request_endpoint,
                            &[nullfs_block_capability],
                            &NULLFS_MESSAGES,
                        );
                        register_nullfs_proxy(nullfs_generation, nullfs_request_endpoint);
                        service_control.drain_mutation(ServiceRegistryMut {
                            logging: &mut logging_service,
                            logging_generation: logging_lifecycle.generation(),
                            nullfs: &mut nullfs_service,
                            nullfs_generation,
                            tmpfs: &mut service,
                            tmpfs_generation,
                            vfs: &mut vfs_service,
                            vfs_generation,
                        });
                        nullfs_service.complete_restart();
                    }
                    ServiceStatusDisposition::Stopped => {
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Nullfs,
                            nullfs_generation,
                            NULLFS_SERVICE_FAILED,
                        );
                        ipc::close(nullfs_request_endpoint)
                            .unwrap_or_else(|_| fail(NULLFS_SERVICE_FAILED));
                    }
                    ServiceStatusDisposition::Failed => {
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Nullfs,
                            nullfs_generation,
                            NULLFS_SERVICE_FAILED,
                        );
                        let _ = ipc::close(nullfs_request_endpoint);
                        fail(NULLFS_SERVICE_FAILED)
                    }
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
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Tmpfs,
                            tmpfs_generation,
                            SERVICE_FAILED,
                        );
                        ipc::close(request_endpoint).unwrap_or_else(|_| fail(SERVICE_FAILED));
                        request_endpoint = ipc::endpoint_create()
                            .unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
                        backoff(backoff_yields);
                        tmpfs_generation = start_service(
                            &mut service,
                            &mut tmpfs_generations,
                            readiness_endpoint,
                            &mut request_endpoint,
                            &[],
                            &TMPFS_MESSAGES,
                        );
                        register_tmpfs_proxy(tmpfs_generation, request_endpoint);
                        service_control.drain_mutation(ServiceRegistryMut {
                            logging: &mut logging_service,
                            logging_generation: logging_lifecycle.generation(),
                            nullfs: &mut nullfs_service,
                            nullfs_generation,
                            tmpfs: &mut service,
                            tmpfs_generation,
                            vfs: &mut vfs_service,
                            vfs_generation,
                        });
                        service.complete_restart();
                    }
                    ServiceStatusDisposition::Stopped => {
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Tmpfs,
                            tmpfs_generation,
                            SERVICE_FAILED,
                        );
                        ipc::close(request_endpoint).unwrap_or_else(|_| fail(SERVICE_FAILED));
                    }
                    ServiceStatusDisposition::Failed => {
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Tmpfs,
                            tmpfs_generation,
                            SERVICE_FAILED,
                        );
                        let _ = ipc::close(request_endpoint);
                        fail(SERVICE_FAILED)
                    }
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
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Vfs,
                            vfs_generation,
                            VFS_SERVICE_FAILED,
                        );
                        ipc::close(vfs_request_endpoint)
                            .unwrap_or_else(|_| fail(VFS_SERVICE_FAILED));
                        vfs_request_endpoint = ipc::endpoint_create()
                            .unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
                        backoff(backoff_yields);
                        vfs_generation = start_service(
                            &mut vfs_service,
                            &mut vfs_generations,
                            vfs_readiness_endpoint,
                            &mut vfs_request_endpoint,
                            &[],
                            &VFS_MESSAGES,
                        );
                        register_vfs_router(vfs_generation, vfs_request_endpoint);
                        service_control.drain_mutation(ServiceRegistryMut {
                            logging: &mut logging_service,
                            logging_generation: logging_lifecycle.generation(),
                            nullfs: &mut nullfs_service,
                            nullfs_generation,
                            tmpfs: &mut service,
                            tmpfs_generation,
                            vfs: &mut vfs_service,
                            vfs_generation,
                        });
                        vfs_service.complete_restart();
                    }
                    ServiceStatusDisposition::Stopped => {
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Vfs,
                            vfs_generation,
                            VFS_SERVICE_FAILED,
                        );
                        ipc::close(vfs_request_endpoint)
                            .unwrap_or_else(|_| fail(VFS_SERVICE_FAILED));
                    }
                    ServiceStatusDisposition::Failed => {
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Vfs,
                            vfs_generation,
                            VFS_SERVICE_FAILED,
                        );
                        let _ = ipc::close(vfs_request_endpoint);
                        fail(VFS_SERVICE_FAILED)
                    }
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
                    shell_process_id = spawn_shell(
                        route_broker.observer_grant_source,
                        service_control.observation_source,
                        service_control.mutation_source,
                    );
                }
            },
            Err(error) if error == syscall::Errno::TRY_AGAIN => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(SHELL_WAIT_FAILED),
        }

        if let Some(check) = logging_lifecycle_check.as_mut() {
            check.advance(
                &logging_service,
                &logging_lifecycle,
                &route_broker,
                &service_control,
                [
                    service_record(NULLFS_SERVICE_ID, &nullfs_service, nullfs_generation),
                    service_record(TMPFS_SERVICE_ID, &service, tmpfs_generation),
                    service_record(VFS_SERVICE_ID, &vfs_service, vfs_generation),
                ],
            );
        }

        if syscall::yield_now().is_err() {
            fail(SHELL_WAIT_FAILED);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitBootMode {
    Normal,
    SmokeTest,
    NullfsRestartTest,
    LoggingLifecycleTest,
}

fn init_boot_mode() -> InitBootMode {
    let descriptor = syscall::open(BOOT_MODE_PATH, syscall::OpenFlags::READ)
        .unwrap_or_else(|_| fail(BOOT_MODE_PROBE_FAILED));
    let mut bytes = [0_u8; 32];
    let count =
        syscall::read(descriptor, &mut bytes).unwrap_or_else(|_| fail(BOOT_MODE_PROBE_FAILED));
    syscall::close(descriptor).unwrap_or_else(|_| fail(BOOT_MODE_PROBE_FAILED));
    match &bytes[..count] {
        b"normal" | b"normal\n" => InitBootMode::Normal,
        b"smoke-test" | b"smoke-test\n" => InitBootMode::SmokeTest,
        b"nullfs-restart-test" | NULLFS_RESTART_TEST_BOOT_MODE => InitBootMode::NullfsRestartTest,
        b"logging-lifecycle-test" | LOGGING_LIFECYCLE_TEST_BOOT_MODE => {
            InitBootMode::LoggingLifecycleTest
        }
        _ => fail(BOOT_MODE_PROBE_FAILED),
    }
}

fn run_nullfs_restart_probe(
    control: &mut ServiceControlState,
    registry: ServiceRegistryMut<'_>,
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

    let old_service_process_id = registry
        .nullfs
        .process_id()
        .unwrap_or_else(|| fail(NULLFS_RESTART_PROBE_FAILED));
    let old_generation = registry.nullfs_generation;
    let restart_count = registry.nullfs.restart_count();
    let old_endpoint_info =
        ipc::info(*request_endpoint).unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    if old_endpoint_info.size != 0
        || syscall::signal_process_group(old_service_process_id, signal::STOP).ok() != Some(1)
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    loop {
        match syscall::wait_child(old_service_process_id) {
            Ok(status) if status.stopped_signal() == Some(signal::STOP) => {
                if registry.nullfs.observe_status(status.raw())
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
    if !request_queued {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }

    let restart_barrier =
        syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let restart_process_id = syscall::spawn_command_with_barrier(
        SV_RESTART_NULLFS_COMMAND,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &restart_barrier,
    )
    .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    if ipc::grant_child(restart_process_id, control.mutation_source, Rights::SEND, 2).ok()
        != Some(2)
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    restart_barrier
        .release()
        .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));

    let mut restart_requested = false;
    for _ in 0..256 {
        control.pump_mutation(ServiceRegistryMut {
            logging: &mut *registry.logging,
            logging_generation: registry.logging_generation,
            nullfs: &mut *registry.nullfs,
            nullfs_generation: registry.nullfs_generation,
            tmpfs: &mut *registry.tmpfs,
            tmpfs_generation: registry.tmpfs_generation,
            vfs: &mut *registry.vfs,
            vfs_generation: registry.vfs_generation,
        });
        if registry.nullfs.state() == ServiceState::Restarting {
            restart_requested = true;
            break;
        }
        match syscall::try_wait_child(restart_process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(_) => fail(NULLFS_RESTART_PROBE_FAILED),
            Err(error) if error == syscall::Errno::TRY_AGAIN => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(NULLFS_RESTART_PROBE_FAILED),
        }
        syscall::yield_now().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    }
    if !restart_requested {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }

    let backoff_yields = loop {
        match syscall::wait_child(old_service_process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {
                if registry.nullfs.observe_status(status.raw())
                    != ServiceStatusDisposition::WaitForNextEvent
                {
                    fail(NULLFS_RESTART_PROBE_FAILED);
                }
            }
            Ok(status) => match registry.nullfs.observe_status(status.raw()) {
                ServiceStatusDisposition::Restart { backoff_yields } => break backoff_yields,
                ServiceStatusDisposition::WaitForNextEvent
                | ServiceStatusDisposition::Stopped
                | ServiceStatusDisposition::Failed => fail(NULLFS_RESTART_PROBE_FAILED),
            },
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(NULLFS_RESTART_PROBE_FAILED),
        }
    };
    if backoff_yields != 0 || registry.nullfs.restart_count() != restart_count {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    offline_filesystem_provider(
        platform::FilesystemProvider::Nullfs,
        old_generation,
        NULLFS_RESTART_PROBE_FAILED,
    );
    offline_filesystem_provider(
        platform::FilesystemProvider::Nullfs,
        old_generation,
        NULLFS_RESTART_PROBE_FAILED,
    );
    let mut offline = [0_u8; 64];
    loop {
        match ipc::try_receive(ready_endpoint, &mut offline) {
            Ok(message) => {
                if message.sender_process_id != probe_process_id
                    || message.capability.is_some()
                    || message.bytes != NULLFS_RESTART_PROBE_OFFLINE.len()
                    || &offline[..message.bytes] != NULLFS_RESTART_PROBE_OFFLINE
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
    let _ = syscall::write_all(STDOUT, NULLFS_SERVICE_RESTARTING);
    backoff(backoff_yields);

    let queued_restart_barrier =
        syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let queued_restart_process_id = syscall::spawn_command_with_barrier(
        SV_RESTART_NULLFS_COMMAND,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &queued_restart_barrier,
    )
    .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    if ipc::grant_child(
        queued_restart_process_id,
        control.mutation_source,
        Rights::SEND,
        2,
    )
    .ok()
        != Some(2)
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    queued_restart_barrier
        .release()
        .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let mut queued_during_restart = false;
    for _ in 0..256 {
        match ipc::info(control.mutation_source) {
            Ok(info) if info.size != 0 => {
                queued_during_restart = true;
                break;
            }
            Ok(_) => {}
            Err(_) => fail(NULLFS_RESTART_PROBE_FAILED),
        }
        if !matches!(
            syscall::try_wait_child(queued_restart_process_id),
            Err(error) if error == syscall::Errno::TRY_AGAIN
                || error == syscall::Errno::INTERRUPTED
        ) {
            fail(NULLFS_RESTART_PROBE_FAILED);
        }
        syscall::yield_now().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    }
    if !queued_during_restart {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }

    ipc::close(*request_endpoint).unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let mut replacement_request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    if ipc::info(replacement_request_endpoint)
        .map(|info| info.object_id == old_endpoint_info.object_id)
        .unwrap_or(true)
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    let generation = start_service(
        registry.nullfs,
        generations,
        readiness_endpoint,
        &mut replacement_request_endpoint,
        &[block_capability],
        &NULLFS_MESSAGES,
    );
    if generation.get() != old_generation.get().saturating_add(1) {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    register_nullfs_proxy(generation, replacement_request_endpoint);
    if platform::offline_filesystem_provider(
        platform::FilesystemProvider::Nullfs,
        u32::try_from(old_generation.get()).unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED)),
    )
    .err()
        != Some(platform::Errno::INVALID_ARGUMENT)
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    ipc::send(control_endpoint, NULLFS_RESTART_PROBE_REPLACEMENT, None)
        .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    *request_endpoint = replacement_request_endpoint;

    control.drain_mutation(ServiceRegistryMut {
        logging: &mut *registry.logging,
        logging_generation: registry.logging_generation,
        nullfs: &mut *registry.nullfs,
        nullfs_generation: generation,
        tmpfs: &mut *registry.tmpfs,
        tmpfs_generation: registry.tmpfs_generation,
        vfs: &mut *registry.vfs,
        vfs_generation: registry.vfs_generation,
    });
    registry.nullfs.complete_restart();
    if registry.nullfs.state() != ServiceState::Running
        || registry.nullfs.controlled_restart_pending()
        || registry.nullfs.restart_count() != restart_count
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    loop {
        match syscall::wait_child(queued_restart_process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) if !status.success() => break,
            Ok(_) => fail(NULLFS_RESTART_PROBE_FAILED),
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(NULLFS_RESTART_PROBE_FAILED),
        }
    }

    loop {
        match syscall::wait_child(probe_process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) if status.success() => break,
            Ok(_) => fail(NULLFS_RESTART_PROBE_FAILED),
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(NULLFS_RESTART_PROBE_FAILED),
        }
    }
    loop {
        match syscall::wait_child(restart_process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) if status.success() => break,
            Ok(_) => fail(NULLFS_RESTART_PROBE_FAILED),
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(NULLFS_RESTART_PROBE_FAILED),
        }
    }
    ipc::close(ready_endpoint).unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    ipc::close(control_endpoint).unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    generation
}

fn offline_filesystem_provider(
    provider: platform::FilesystemProvider,
    service_generation: ProviderGeneration,
    failure_message: &[u8],
) {
    let generation =
        u32::try_from(service_generation.get()).unwrap_or_else(|_| fail(failure_message));
    platform::offline_filesystem_provider(provider, generation)
        .unwrap_or_else(|_| fail(failure_message));
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
        let mut readiness_yields_remaining = LOGGING_READINESS_GRACE_YIELDS;
        let mut force_termination_attempts_remaining = LOGGING_FORCE_TERMINATION_ATTEMPTS;
        let mut force_termination_sent = false;
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
                                | ServiceStatusDisposition::Stopped
                                | ServiceStatusDisposition::Failed => fail(LOGGING_SERVICE_FAILED),
                            },
                        }
                    }
                    if service.note_ready() != ReadyDisposition::Accepted {
                        fail(LOGGING_SERVICE_PROTOCOL_FAILED);
                    }
                    route_broker.publish(generation, producer_source, observer_source);
                    let _ = syscall::write_all(STDOUT, LOGGING_MESSAGES.ready);
                    return LoggingGeneration {
                        generation,
                        readiness_source,
                        producer_source,
                        observer_source,
                        producer_object_id,
                        observer_object_id,
                        readiness_received: true,
                        child_stopped: false,
                        routes_published: true,
                        readiness_yields_remaining: 0,
                        readiness_force_termination_sent: false,
                        force_termination_attempts_remaining: 0,
                    };
                }
                Err(error) if error == ipc::Error::TRY_AGAIN => {
                    if readiness_yields_remaining != 0 {
                        readiness_yields_remaining -= 1;
                    } else if !force_termination_sent
                        && request_forced_termination(
                            process_id,
                            &mut force_termination_attempts_remaining,
                        )
                    {
                        let _ = syscall::write_all(STDOUT, LOGGING_SERVICE_READINESS_TIMEOUT);
                        force_termination_sent = true;
                    }
                }
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
                    ServiceStatusDisposition::Stopped | ServiceStatusDisposition::Failed => {
                        fail(LOGGING_SERVICE_FAILED)
                    }
                },
                Err(error) if error == syscall::Errno::TRY_AGAIN => {}
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(_) => fail(LOGGING_SERVICE_FAILED),
            }
            let _ = syscall::yield_now();
        }
    }
}

fn begin_logging_generation(
    service: &mut ServiceRuntime,
    generations: &mut ProviderGenerationSequence,
    early_log_reader: CapabilityHandle,
    lifecycle_test: bool,
) -> LoggingGeneration {
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
    let suppress_readiness = lifecycle_test && generation.get() >= 4;
    let command = if suppress_readiness {
        LOGGING_SERVICE_SUPPRESS_READINESS_COMMAND
    } else if lifecycle_test {
        LOGGING_SERVICE_IGNORE_TERMINATE_COMMAND
    } else {
        spec.command
    };
    let _ = syscall::write_all(STDOUT, LOGGING_MESSAGES.starting);
    let barrier = syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(LOGGING_SERVICE_FAILED));
    let process_id = syscall::spawn_command_with_barrier(
        command,
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

    LoggingGeneration {
        generation,
        readiness_source,
        producer_source,
        observer_source,
        producer_object_id,
        observer_object_id,
        readiness_received: false,
        child_stopped: false,
        routes_published: false,
        readiness_yields_remaining: if suppress_readiness {
            LOGGING_TEST_READINESS_GRACE_YIELDS
        } else {
            LOGGING_READINESS_GRACE_YIELDS
        },
        readiness_force_termination_sent: false,
        force_termination_attempts_remaining: LOGGING_FORCE_TERMINATION_ATTEMPTS,
    }
}

fn poll_logging_child(
    service: &mut ServiceRuntime,
    lifecycle: &mut LoggingLifecycle,
    route_broker: &mut RouteBrokerState,
) {
    let Some(process_id) = service.process_id() else {
        return;
    };
    let status = match syscall::try_wait_child(process_id) {
        Ok(status) => status,
        Err(error) if error == syscall::Errno::TRY_AGAIN => return,
        Err(error) if error == syscall::Errno::INTERRUPTED => return,
        Err(_) => fail(LOGGING_SERVICE_FAILED),
    };

    if status.stopped_signal().is_some() {
        if service.observe_status(status.raw()) != ServiceStatusDisposition::WaitForNextEvent {
            fail(LOGGING_SERVICE_PROTOCOL_FAILED);
        }
        if let Some(generation) = lifecycle.current.as_mut() {
            generation.child_stopped = true;
        }
        return;
    }
    if status.continued() {
        if service.observe_status(status.raw()) != ServiceStatusDisposition::WaitForNextEvent {
            fail(LOGGING_SERVICE_PROTOCOL_FAILED);
        }
        if let Some(generation) = lifecycle.current.as_mut() {
            generation.child_stopped = false;
        }
        return;
    }

    let disposition = service.observe_status(status.raw());
    lifecycle.withdraw_routes(route_broker);
    lifecycle.close_current();
    match disposition {
        ServiceStatusDisposition::Stopped => {
            lifecycle.backoff_yields_remaining = 0;
        }
        ServiceStatusDisposition::Restart { backoff_yields } => {
            lifecycle.backoff_yields_remaining = backoff_yields;
            let _ = syscall::write_all(STDOUT, LOGGING_MESSAGES.restarting);
        }
        ServiceStatusDisposition::Failed => {
            lifecycle.backoff_yields_remaining = 0;
        }
        ServiceStatusDisposition::WaitForNextEvent => fail(LOGGING_SERVICE_PROTOCOL_FAILED),
    }
}

fn request_forced_termination(process_id: ProcessId, attempts_remaining: &mut u32) -> bool {
    if platform::kill(process_id, signal::KILL).is_ok() {
        return true;
    }
    if *attempts_remaining == 0 {
        fail(LOGGING_SERVICE_FAILED);
    }
    *attempts_remaining -= 1;
    false
}

fn sync_logging_routes(
    service: &ServiceRuntime,
    lifecycle: &mut LoggingLifecycle,
    route_broker: &mut RouteBrokerState,
) {
    if !matches!(
        service.state(),
        ServiceState::Stopping | ServiceState::Restarting
    ) {
        lifecycle.termination_grace_yields_remaining = None;
        lifecycle.force_termination_sent = false;
        lifecycle.force_termination_attempts_remaining = 0;
        return;
    }

    lifecycle.withdraw_routes(route_broker);
    let Some(process_id) = service.process_id() else {
        lifecycle.termination_grace_yields_remaining = None;
        lifecycle.force_termination_sent = false;
        lifecycle.force_termination_attempts_remaining = 0;
        return;
    };
    match lifecycle.termination_grace_yields_remaining {
        None => {
            lifecycle.termination_grace_yields_remaining = Some(LOGGING_TERMINATION_GRACE_YIELDS);
            lifecycle.force_termination_attempts_remaining = LOGGING_FORCE_TERMINATION_ATTEMPTS;
        }
        Some(remaining) if remaining != 0 => {
            lifecycle.termination_grace_yields_remaining = Some(remaining - 1);
        }
        Some(_) if !lifecycle.force_termination_sent => {
            if request_forced_termination(
                process_id,
                &mut lifecycle.force_termination_attempts_remaining,
            ) {
                let _ = syscall::write_all(STDOUT, LOGGING_SERVICE_FORCE_TERMINATING);
                lifecycle.force_termination_sent = true;
            }
        }
        Some(_) => {}
    }
}

fn advance_logging_lifecycle(
    service: &mut ServiceRuntime,
    lifecycle: &mut LoggingLifecycle,
    route_broker: &mut RouteBrokerState,
    generations: &mut ProviderGenerationSequence,
    early_log_reader: CapabilityHandle,
    lifecycle_test: bool,
) {
    if lifecycle.current.is_none() {
        if lifecycle.backoff_yields_remaining != 0 {
            lifecycle.backoff_yields_remaining -= 1;
            return;
        }
        if service.should_start() || service.state() == ServiceState::Backoff {
            let generation =
                begin_logging_generation(service, generations, early_log_reader, lifecycle_test);
            lifecycle.install(generation);
        }
        return;
    }

    if service.state() != ServiceState::Starting {
        return;
    }
    let process_id = service
        .process_id()
        .unwrap_or_else(|| fail(LOGGING_SERVICE_PROTOCOL_FAILED));
    let generation = lifecycle
        .current
        .as_mut()
        .unwrap_or_else(|| fail(LOGGING_SERVICE_PROTOCOL_FAILED));
    if !generation.readiness_received {
        let mut ready_buffer = [0_u8; 64];
        match ipc::try_receive(generation.readiness_source, &mut ready_buffer) {
            Ok(message) => {
                let spec = service.spec();
                if message.sender_process_id != process_id
                    || message.capability.is_some()
                    || message.bytes != spec.ready_message.len()
                    || &ready_buffer[..message.bytes] != spec.ready_message
                {
                    fail(LOGGING_SERVICE_PROTOCOL_FAILED);
                }
                generation.readiness_received = true;
                return;
            }
            Err(error) if error == ipc::Error::TRY_AGAIN => {
                if generation.readiness_yields_remaining != 0 {
                    generation.readiness_yields_remaining -= 1;
                } else if !generation.readiness_force_termination_sent
                    && request_forced_termination(
                        process_id,
                        &mut generation.force_termination_attempts_remaining,
                    )
                {
                    let _ = syscall::write_all(STDOUT, LOGGING_SERVICE_READINESS_TIMEOUT);
                    generation.readiness_force_termination_sent = true;
                }
                return;
            }
            Err(_) => fail(LOGGING_SERVICE_PROTOCOL_FAILED),
        }
    }
    if generation.child_stopped {
        return;
    }
    if service.note_ready() != ReadyDisposition::Accepted {
        fail(LOGGING_SERVICE_PROTOCOL_FAILED);
    }
    route_broker.publish(
        generation.generation,
        generation.producer_source,
        generation.observer_source,
    );
    generation.routes_published = true;
    let _ = syscall::write_all(STDOUT, LOGGING_MESSAGES.ready);
}

fn start_service(
    service: &mut ServiceRuntime,
    generations: &mut ProviderGenerationSequence,
    readiness_endpoint: CapabilityHandle,
    request_endpoint: &mut CapabilityHandle,
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
                *request_endpoint,
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
                    if service.note_ready() != ReadyDisposition::Accepted {
                        fail(messages.protocol_failed);
                    }
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
                        ipc::close(*request_endpoint)
                            .unwrap_or_else(|_| fail(messages.bootstrap_failed));
                        *request_endpoint = ipc::endpoint_create()
                            .unwrap_or_else(|_| fail(messages.bootstrap_failed));
                        break;
                    }
                    ServiceStatusDisposition::Stopped | ServiceStatusDisposition::Failed => {
                        fail(messages.failed)
                    }
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
                ServiceStatusDisposition::WaitForNextEvent
                | ServiceStatusDisposition::Stopped
                | ServiceStatusDisposition::Failed => fail(LOGGING_COLLECTOR_RESTART_POLICY_FAILED),
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
    if ipc::grant_child(
        process_id,
        control.observation_source,
        Rights::SEND,
        READY_HANDLE,
    )
    .ok()
        != Some(READY_HANDLE)
    {
        fail(SERVICE_CONTROL_PROBE_FAILED);
    }
    barrier
        .release()
        .unwrap_or_else(|_| fail(SERVICE_CONTROL_PROBE_FAILED));

    let mut remaining = LOGGING_PROBE_MAX_YIELDS;
    loop {
        control.pump_observation(registry);
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
    service_control_mutation: CapabilityHandle,
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
        || ipc::grant_child(
            process_id,
            service_control_mutation,
            Rights::SEND | Rights::DUPLICATE,
            SHELL_SERVICE_CONTROL_MUTATION_HANDLE,
        )
        .ok()
            != Some(SHELL_SERVICE_CONTROL_MUTATION_HANDLE)
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
