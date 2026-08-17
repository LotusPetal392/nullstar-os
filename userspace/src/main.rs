#![no_std]
#![no_main]

use nswp_logging::{LOGGING_OBSERVER_ROLE, LOGGING_PRODUCER_ROLE, LOGGING_SERVICE_ID};
use service_control::{
    DesiredState, ListResponse, ObservedState, Operation, RequestId, ServiceControlFailure,
    ServiceControlRequest, ServiceControlResponse, ServiceId, ServiceRecord, TargetOutcome,
    TargetResponse,
};
use service_definition::{MAX_DEFINITION_BYTES, Readiness, RestartPolicy, ServiceDefinition};
use service_route::{Authorizer, ProviderGeneration, ProviderGenerationSequence, RouteKey};
use userspace::{
    abi::{INIT_PROCESS_ID, file, signal},
    definition_service_probe, early_log,
    filesystem::{crash_test, protocol as filesystem_protocol},
    ipc::{self, CapabilityHandle, ObjectKind, Rights, Transfer},
    managed_startup::{
        ManagedServiceIdentity, SYSTEM_NAMESPACE_PROFILE_ID, SYSTEM_PACKAGE_GENERATION,
        SYSTEM_PACKAGE_ID, numeric_service_id,
    },
    nullfs_primary_volume, platform,
    process_start::{
        PROCESS_START_BOOTSTRAP_HANDLE, StartupIdentity, StartupLaunch, StartupLaunchReason,
        StartupSectionId, StartupSectionPayload, encode_startup_arguments,
        encode_startup_environment, send_process_start_data,
    },
    runtime_context::{
        CapabilityRole, StartupMessage, StartupResource, StartupRuntimeRole, send_startup_message,
    },
    service_cleanup::{
        self, Action as CleanupAction, Diagnostic as CleanupDiagnostic,
        JobWaitResult as CleanupJobWaitResult, LeaderResult as CleanupLeaderResult,
        Observation as CleanupObservation, Operation as CleanupOperation, Phase as CleanupPhase,
        Service as CleanupService,
    },
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
    syscall::{self, OpenFlags, ProcessId, STDERR, STDOUT, SpawnFlags},
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
const LOGGING_SERVICE_JOB_DRAINED: &[u8] =
    b"userspace init: logging service generation job drained\n";
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
const SV_RESTART_TMPFS_COMMAND: &[u8] = b"/sv restart tmpfs";
const SV_RESTART_VFS_COMMAND: &[u8] = b"/sv restart vfs";
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
const NULLFS_BOOT_GENERATION_PROBE_COMMAND: &[u8] = b"/nullfs-boot-generation-probe";
const NULLFS_BOOT_GENERATION_PROBE_FAILED: &[u8] =
    b"userspace init: NullFS boot-generation probe failed\n";
const NULLFS_BOOT_GENERATION_PROBE_PASSED: &[u8] =
    b"userspace init: NullFS boot-generation probe passed\n";
const BLOCK_DEVICE_BOOTSTRAP_FAILED: &[u8] =
    b"userspace init: failed to acquire block-device endpoint\n";
const NULLFS_SERVICE_COMMAND: &[u8] = b"/nullfs-service --writable";
const NULLFS_CONTAINMENT_TEST_SERVICE_COMMAND: &[u8] =
    b"/nullfs-service --writable --containment-test";
const NULLFS_CRASH_CONTAINMENT_TEST_SERVICE_COMMAND: &[u8] =
    b"/nullfs-service --writable --crash-test --containment-test";
const NULLFS_SERVICE_READY_MESSAGE: &[u8] = b"service-ready: nullfs";
const NULLFS_SERVICE_STARTING: &[u8] = b"userspace init: starting NullFS service\n";
const NULLFS_SERVICE_RESTARTING: &[u8] = b"userspace init: NullFS service exited; restarting\n";
const NULLFS_SERVICE_READY: &[u8] = b"userspace init: writable NullFS service ready\n";
const NULLFS_SERVICE_FAILED: &[u8] = b"userspace init: NullFS service exhausted restart budget\n";
const NULLFS_SERVICE_BOOTSTRAP_FAILED: &[u8] =
    b"userspace init: failed to grant NullFS capabilities\n";
const NULLFS_SERVICE_PROTOCOL_FAILED: &[u8] = b"userspace init: invalid NullFS readiness message\n";
const NULLFS_SERVICE_JOB_DRAINED: &[u8] =
    b"userspace init: NullFS service generation job drained\n";
const NULLFS_READINESS_PROBE_COMMAND: &[u8] = b"/nullfs-probe readiness";
const NULLFS_READINESS_PROBE_FAILED: &[u8] = b"userspace init: NullFS readiness failed\n";
const NULLFS_READINESS_PROBE_PASSED: &[u8] = b"userspace init: NullFS readiness passed\n";
const NULLFS_FULL_PROBE_COMMAND: &[u8] = b"/nullfs-probe full";
const NULLFS_FULL_PROBE_FAILED: &[u8] = b"userspace init: full NullFS probe failed\n";
const NULLFS_FULL_PROBE_PASSED: &[u8] = b"userspace init: full NullFS probe passed\n";
const SERVICE_COMMAND: &[u8] = b"/tmpfs-service";
const TMPFS_CONTAINMENT_TEST_COMMAND: &[u8] = b"/tmpfs-service --containment-test";
const SERVICE_READY_MESSAGE: &[u8] = b"service-ready: tmpfs";
const SERVICE_STARTING: &[u8] = b"userspace init: starting tmpfs service\n";
const SERVICE_RESTARTING: &[u8] = b"userspace init: tmpfs service exited; restarting\n";
const SERVICE_READY: &[u8] = b"userspace init: tmpfs service ready\n";
const SERVICE_FAILED: &[u8] = b"userspace init: tmpfs service exhausted restart budget\n";
const SERVICE_BOOTSTRAP_FAILED: &[u8] = b"userspace init: failed to grant tmpfs capabilities\n";
const SERVICE_PROTOCOL_FAILED: &[u8] = b"userspace init: invalid tmpfs readiness message\n";
const TMPFS_SERVICE_JOB_DRAINED: &[u8] = b"userspace init: tmpfs service generation job drained\n";
const TMPFS_PROBE_COMMAND: &[u8] = b"/tmpfs-probe";
const TMPFS_PROBE_FAILED: &[u8] = b"userspace init: userspace tmpfs probe failed\n";
const TMPFS_PROBE_PASSED: &[u8] = b"userspace init: userspace tmpfs probe passed\n";
const VFS_SERVICE_COMMAND: &[u8] = b"/vfs-service";
const VFS_CONTAINMENT_TEST_COMMAND: &[u8] = b"/vfs-service --containment-test";
const VFS_SERVICE_READY_MESSAGE: &[u8] = b"service-ready: vfs";
const VFS_SERVICE_STARTING: &[u8] = b"userspace init: starting vfs service\n";
const VFS_SERVICE_RESTARTING: &[u8] = b"userspace init: vfs service exited; restarting\n";
const VFS_SERVICE_READY: &[u8] = b"userspace init: vfs service ready\n";
const VFS_SERVICE_FAILED: &[u8] = b"userspace init: vfs service exhausted restart budget\n";
const VFS_SERVICE_BOOTSTRAP_FAILED: &[u8] = b"userspace init: failed to grant vfs capabilities\n";
const VFS_SERVICE_PROTOCOL_FAILED: &[u8] = b"userspace init: invalid vfs readiness message\n";
const VFS_SERVICE_JOB_DRAINED: &[u8] = b"userspace init: vfs service generation job drained\n";
const VFS_READINESS_PROBE_COMMAND: &[u8] = b"/vfs-probe readiness";
const VFS_READINESS_PROBE_FAILED: &[u8] = b"userspace init: vfs readiness failed\n";
const VFS_READINESS_PROBE_PASSED: &[u8] = b"userspace init: vfs readiness passed\n";
const VFS_FULL_PROBE_COMMAND: &[u8] = b"/vfs-probe full";
const VFS_FULL_PROBE_FAILED: &[u8] = b"userspace init: full vfs probe failed\n";
const VFS_FULL_PROBE_PASSED: &[u8] = b"userspace init: full vfs probe passed\n";
const VFS_BOOTSTRAP_PROBE_COMMAND: &[u8] = b"/vfs-probe bootstrap";
const VFS_BOOTSTRAP_PROBE_PASSED: &[u8] =
    b"userspace init: bootstrap VFS remained available while NullFS was offline\n";
const VFS_OUT_OF_SPACE_PROBE_COMMAND: &[u8] = b"/vfs-probe out-of-space";
const VFS_OUT_OF_SPACE_PROBE_FAILED: &[u8] = b"userspace init: NullFS out-of-space probe failed\n";
const VFS_OUT_OF_SPACE_PROBE_PASSED: &[u8] = b"userspace init: NullFS data and inode exhaustion, service continuity, and resource reclamation verified\n";
const VFS_BLOCK_DEVICE_LOSS_PROBE_COMMAND: &[u8] = b"/vfs-probe block-device-loss";
const VFS_BLOCK_DEVICE_LOSS_PROBE_READY: &[u8] = b"block-device-loss: mutation prepared";
const VFS_BLOCK_DEVICE_LOSS_PROVIDER_OFFLINED: &[u8] = b"block-device-loss: provider offlined";
const VFS_BLOCK_DEVICE_LOSS_MUTATION_FAILED: &[u8] =
    b"block-device-loss: uncertain mutation failed";
const VFS_BLOCK_DEVICE_LOSS_FILESYSTEM_OFFLINED: &[u8] =
    b"block-device-loss: filesystem generation offlined";
const VFS_BLOCK_DEVICE_LOSS_INJECTED: &[u8] =
    b"userspace init: writable NullFS block endpoint loss injected\n";
const VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED: &[u8] =
    b"userspace init: NullFS block-device-loss probe failed\n";
const VFS_BLOCK_DEVICE_LOSS_PROBE_PASSED: &[u8] = b"userspace init: NullFS block-device loss, uncertain mutation fail-stop, stale VFS errors, and bootstrap continuity verified\n";
const VFS_CRASH_RECOVERY_PROBE_COMMAND: &[u8] = b"/vfs-probe crash-recovery";
const VFS_CRASH_RECOVERY_READY: &[u8] = b"crash-recovery: baseline ready";
const VFS_CRASH_RECOVERY_GO: &[u8] = b"crash-recovery: mutation armed";
const VFS_CRASH_RECOVERY_MUTATION_FAILED: &[u8] = b"crash-recovery: uncertain mutation failed";
const VFS_CRASH_RECOVERY_REPLACEMENT: &[u8] = b"crash-recovery: replacement registered";
const VFS_CRASH_RECOVERY_INJECTED: &[u8] =
    b"userspace init: NullFS service-backed mutation crash injected\n";
const VFS_CRASH_RECOVERY_PROBE_FAILED: &[u8] =
    b"userspace init: NullFS crash-recovery probe failed\n";
const VFS_CRASH_RECOVERY_PROBE_PASSED: &[u8] = b"userspace init: NullFS service crash, uncertain VFS failure, dirty remount recovery, stale descriptors, and durable single mutation verified\n";
const DEFINITION_SERVICE_LOADING: &[u8] =
    b"userspace init: loading service definition from /System/services\n";
const DEFINITION_SERVICE_STARTING: &[u8] = b"userspace init: starting definition-backed service\n";
const DEFINITION_SERVICE_RESTARTING: &[u8] =
    b"userspace init: definition-backed service exited; restarting\n";
const DEFINITION_SERVICE_JOB_DRAINED: &[u8] =
    b"userspace init: definition-backed service generation job drained\n";
const DEFINITION_SERVICE_READY: &[u8] = b"userspace init: definition-backed service ready\n";
const DEFINITION_SERVICE_VERIFIED: &[u8] =
    b"userspace init: definition-backed activation and restart verified\n";
const DEFINITION_SERVICE_FAILED: &[u8] =
    b"userspace init: definition-backed service activation failed\n";
const DEFINITION_SERVICE_PROTOCOL_FAILED: &[u8] =
    b"userspace init: invalid definition-backed service readiness\n";
const DEFINITION_SERVICE_READINESS_GRACE_YIELDS: u32 = 2_048;
const NULLFS_RESTART_PROBE_COMMAND: &[u8] = b"/vfs-probe nullfs-restart";
const NULLFS_RESTART_PROBE_READY: &[u8] =
    b"nullfs-restart: live descriptor and persistent mutation ready";

const NULLFS_RESTART_PROBE_REPLACEMENT: &[u8] = b"nullfs-restart: replacement registered";
const NULLFS_RESTART_PROBE_PASSED: &[u8] =
    b"userspace init: NullFS restart persistent VFS mutation and stale descriptors verified\n";
const NULLFS_RESTART_PROBE_FAILED: &[u8] = b"userspace init: NullFS restart probe failed\n";
const LOGGING_LIFECYCLE_TEST_PASSED: &[u8] = b"userspace init: logging live start, stop, route withdrawal, restart fencing, and generation replacement verified\n";
const LOGGING_LIFECYCLE_TEST_FAILED: &[u8] = b"userspace init: logging lifecycle test failed\n";
const BOOT_MODE_PATH: &[u8] = b"/BOOTMODE";
const NULLFS_RESTART_TEST_BOOT_MODE: &[u8] = b"nullfs-restart-test\n";
const NULLFS_OUT_OF_SPACE_TEST_BOOT_MODE: &[u8] = b"nullfs-out-of-space-test\n";
const NULLFS_BLOCK_DEVICE_LOSS_TEST_BOOT_MODE: &[u8] = b"nullfs-block-device-loss-test\n";
const NULLFS_CRASH_RECOVERY_TEST_BOOT_MODE: &[u8] = b"nullfs-crash-recovery-test\n";
const NULLFS_BOOT_GENERATION_TEST_BOOT_MODE: &[u8] = b"nullfs-boot-generation-test\n";
const NULLFS_UNAVAILABLE_TEST_BOOT_MODE: &[u8] = b"nullfs-unavailable-test\n";
const LOGGING_LIFECYCLE_TEST_BOOT_MODE: &[u8] = b"logging-lifecycle-test\n";
const BOOT_MODE_PROBE_FAILED: &[u8] = b"userspace init: unable to read boot mode\n";
const NULLFS_UNAVAILABLE_RECOVERY_HANDOFF: &[u8] =
    b"userspace init: configured primary NullFS volume unavailable; entering recovery\n";
const NULLFS_UNAVAILABLE_TEST_FAILED: &[u8] =
    b"userspace init: unavailable-primary recovery test failed\n";
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
const NULLFS_CRASH_TEST_HANDLE: u64 = 4;
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
const SERVICE_JOB_CLEANUP_YIELDS: u32 = 64;
const NULLFS_QUIESCE_GRACE_YIELDS: u32 = 2_048;
const NULLFS_TEST_QUIESCE_GRACE_YIELDS: u32 = 8;
const NULLFS_FORCE_TERMINATION_ATTEMPTS: u32 = 64;
const LOGGING_PROBE_STATUS_HANDLE: u64 = 3;
const LOGGING_PROBE_CONTROL_HANDLE: u64 = 4;
const TMPFS_EXECUTABLE_ID: u64 = 3;
const VFS_EXECUTABLE_ID: u64 = 4;

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
const NULLFS_CONTAINMENT_TEST_SERVICE: ServiceSpec = ServiceSpec {
    command: NULLFS_CONTAINMENT_TEST_SERVICE_COMMAND,
    ..NULLFS_SERVICE
};
const NULLFS_CRASH_CONTAINMENT_TEST_SERVICE: ServiceSpec = ServiceSpec {
    command: NULLFS_CRASH_CONTAINMENT_TEST_SERVICE_COMMAND,
    ..NULLFS_SERVICE
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
const TMPFS_CONTAINMENT_TEST_SERVICE: ServiceSpec = ServiceSpec {
    command: TMPFS_CONTAINMENT_TEST_COMMAND,
    ..TMPFS_SERVICE
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
const VFS_CONTAINMENT_TEST_SERVICE: ServiceSpec = ServiceSpec {
    command: VFS_CONTAINMENT_TEST_COMMAND,
    ..VFS_SERVICE
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

#[derive(Clone, Copy)]
struct ManagedStartupCapability {
    source_handle: CapabilityHandle,
    rights: Rights,
    role: CapabilityRole,
}

#[derive(Clone, Copy)]
struct ServiceContainment {
    service: CleanupService,
    drained_message: &'static [u8],
}

const TMPFS_CONTAINMENT: ServiceContainment = ServiceContainment {
    service: CleanupService::Tmpfs,
    drained_message: TMPFS_SERVICE_JOB_DRAINED,
};
const NULLFS_CONTAINMENT: ServiceContainment = ServiceContainment {
    service: CleanupService::Nullfs,
    drained_message: NULLFS_SERVICE_JOB_DRAINED,
};
const VFS_CONTAINMENT: ServiceContainment = ServiceContainment {
    service: CleanupService::Vfs,
    drained_message: VFS_SERVICE_JOB_DRAINED,
};

fn managed_containment_identity(containment: ServiceContainment) -> Option<ManagedServiceIdentity> {
    let (executable, service) = match containment.service {
        CleanupService::Tmpfs => (TMPFS_EXECUTABLE_ID, TMPFS_SERVICE_ID),
        CleanupService::Vfs => (VFS_EXECUTABLE_ID, VFS_SERVICE_ID),
        _ => return None,
    };
    Some(ManagedServiceIdentity::new(
        executable,
        numeric_service_id(service.into_bytes()),
        executable,
    ))
}

fn send_managed_service_process_start(
    sender: CapabilityHandle,
    command: &[u8],
    process_id: ProcessId,
    generation: ProviderGeneration,
    restart_count: u32,
    identity: ManagedServiceIdentity,
    capabilities: &[ManagedStartupCapability],
) -> bool {
    if capabilities.is_empty()
        || capabilities.len() > userspace::abi::limits::MAX_IPC_MESSAGE_HANDLES
    {
        return false;
    }
    let mut resources = [None; userspace::abi::limits::MAX_IPC_MESSAGE_HANDLES];
    let mut transfers = [Transfer {
        handle: 0,
        rights: Rights::EMPTY,
    }; userspace::abi::limits::MAX_IPC_MESSAGE_HANDLES];
    let mut duplicates = [0; userspace::abi::limits::MAX_IPC_MESSAGE_HANDLES];
    for (index, capability) in capabilities.iter().enumerate() {
        let duplicate = match ipc::duplicate(
            capability.source_handle,
            capability.rights | Rights::TRANSFER,
        ) {
            Ok(handle) => handle,
            Err(_) => {
                for handle in duplicates[..index].iter().copied() {
                    let _ = ipc::close(handle);
                }
                return false;
            }
        };
        duplicates[index] = duplicate;
        resources[index] = Some(StartupResource {
            role: capability.role,
            required: true,
        });
        transfers[index] = Transfer {
            handle: duplicate,
            rights: capability.rights,
        };
    }
    let message = match StartupMessage::new(StartupRuntimeRole::Service, resources) {
        Ok(message) => message,
        Err(_) => {
            for handle in duplicates[..capabilities.len()].iter().copied() {
                let _ = ipc::close(handle);
            }
            return false;
        }
    };
    if send_startup_message(sender, &message, &transfers[..capabilities.len()]).is_err() {
        for handle in duplicates[..capabilities.len()].iter().copied() {
            let _ = ipc::close(handle);
        }
        return false;
    }

    let mut arguments = [&[][..]; userspace::abi::limits::MAX_ARGUMENTS];
    let mut argument_count = 0;
    for argument in command
        .split(u8::is_ascii_whitespace)
        .filter(|argument| !argument.is_empty())
    {
        if argument_count == arguments.len() {
            return false;
        }
        arguments[argument_count] = argument;
        argument_count += 1;
    }
    if argument_count == 0 {
        return false;
    }
    let mut argument_bytes = [0; userspace::abi::limits::MAX_ARGUMENT_BYTES];
    let argument_length =
        match encode_startup_arguments(&arguments[..argument_count], &mut argument_bytes) {
            Ok(length) => length,
            Err(_) => return false,
        };
    let mut environment_bytes = [0; 4];
    let environment_length = match encode_startup_environment(&[], &mut environment_bytes) {
        Ok(length) => length,
        Err(_) => return false,
    };
    let monotonic_start_ns = match platform::monotonic_time_ns() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let identity_bytes = StartupIdentity {
        process: process_id,
        package: SYSTEM_PACKAGE_ID,
        package_generation: SYSTEM_PACKAGE_GENERATION,
        executable: identity.executable,
        application: 0,
        service: identity.service,
        component: identity.component,
        user: 0,
        session: 0,
    }
    .encode();
    let launch_bytes = StartupLaunch {
        launch: generation.get(),
        manager_generation: generation.get(),
        namespace_profile: SYSTEM_NAMESPACE_PROFILE_ID,
        monotonic_start_ns,
        attempt: restart_count.saturating_add(1),
        reason: if generation.get() == 1 {
            StartupLaunchReason::Activation
        } else {
            StartupLaunchReason::Restart
        },
        flags: 0,
    }
    .encode();
    let sections = [
        StartupSectionPayload {
            id: StartupSectionId::IDENTITY,
            required: true,
            bytes: &identity_bytes,
        },
        StartupSectionPayload {
            id: StartupSectionId::ARGUMENTS,
            required: true,
            bytes: &argument_bytes[..argument_length],
        },
        StartupSectionPayload {
            id: StartupSectionId::ENVIRONMENT,
            required: true,
            bytes: &environment_bytes[..environment_length],
        },
        StartupSectionPayload {
            id: StartupSectionId::LAUNCH,
            required: true,
            bytes: &launch_bytes,
        },
    ];
    send_process_start_data(sender, &sections).is_ok()
}

struct ContainedServiceActivationAttempt {
    containment: ServiceContainment,
    generation_handoff_source: Option<CapabilityHandle>,
    bootstrap_sender: Option<CapabilityHandle>,
    bootstrap_receiver_source: Option<CapabilityHandle>,
    barrier: Option<syscall::LaunchBarrier>,
    process_id: Option<ProcessId>,
    process_group_id: Option<ProcessId>,
    job_management: Option<CapabilityHandle>,
    job: Option<CapabilityHandle>,
    job_assigned: bool,
    cleanup_budget_reported: bool,
}

impl ContainedServiceActivationAttempt {
    const fn new(containment: ServiceContainment) -> Self {
        Self {
            containment,
            generation_handoff_source: None,
            bootstrap_sender: None,
            bootstrap_receiver_source: None,
            barrier: None,
            process_id: None,
            process_group_id: None,
            job_management: None,
            job: None,
            job_assigned: false,
            cleanup_budget_reported: false,
        }
    }

    fn close_capability(&self, handle: &mut Option<CapabilityHandle>) -> bool {
        close_cleanup_capability(
            self.containment.service,
            CleanupPhase::ResourceRelease,
            handle,
        )
    }

    fn release_child(&mut self) -> bool {
        let mut generation_handoff_source = self.generation_handoff_source.take();
        let generation_closed = self.close_capability(&mut generation_handoff_source);
        self.generation_handoff_source = generation_handoff_source;
        let mut bootstrap_sender = self.bootstrap_sender.take();
        let sender_closed = self.close_capability(&mut bootstrap_sender);
        self.bootstrap_sender = bootstrap_sender;
        let mut bootstrap_receiver_source = self.bootstrap_receiver_source.take();
        let receiver_closed = self.close_capability(&mut bootstrap_receiver_source);
        self.bootstrap_receiver_source = bootstrap_receiver_source;
        let barrier_released = release_cleanup_barrier(self.containment.service, &mut self.barrier);
        generation_closed && sender_closed && receiver_closed && barrier_released
    }

    fn abort(&mut self) -> bool {
        let process_clean = if self.job_assigned {
            let clean = terminate_and_drain_service_job(
                self.containment.service,
                &mut self.job,
                &mut self.process_id,
                &mut self.process_group_id,
                self.containment.drained_message,
                &mut self.cleanup_budget_reported,
            );
            if clean {
                self.job_assigned = false;
            }
            clean
        } else {
            let process_group_clean = match self.process_group_id {
                Some(process_group_id) => {
                    if terminate_unassigned_service_process_group(
                        self.containment.service,
                        process_group_id,
                        &mut self.process_id,
                        &mut self.cleanup_budget_reported,
                    ) {
                        self.process_group_id = None;
                        true
                    } else {
                        false
                    }
                }
                None => self.process_id.is_none(),
            };
            process_group_clean
                && close_empty_service_job(
                    self.containment.service,
                    &mut self.job_management,
                    &mut self.job,
                )
        };
        let management_closed = if process_clean {
            let mut handle = self.job_management.take();
            let closed = self.close_capability(&mut handle);
            self.job_management = handle;
            closed
        } else {
            false
        };
        let generation_closed = if process_clean {
            let mut handle = self.generation_handoff_source.take();
            let closed = self.close_capability(&mut handle);
            self.generation_handoff_source = handle;
            closed
        } else {
            false
        };
        let bootstrap_closed = if process_clean {
            let mut sender = self.bootstrap_sender.take();
            let sender_closed = self.close_capability(&mut sender);
            self.bootstrap_sender = sender;
            let mut receiver = self.bootstrap_receiver_source.take();
            let receiver_closed = self.close_capability(&mut receiver);
            self.bootstrap_receiver_source = receiver;
            sender_closed && receiver_closed
        } else {
            false
        };
        let barrier_released = if process_clean {
            release_cleanup_barrier(self.containment.service, &mut self.barrier)
        } else {
            false
        };
        process_clean
            && management_closed
            && generation_closed
            && bootstrap_closed
            && barrier_released
    }

    fn finish_reaped(&mut self) -> bool {
        self.process_id = None;
        self.abort()
    }

    fn into_job(mut self, messages: &ServiceMessages) -> ContainedServiceJob {
        if !self.job_assigned
            || self.process_id.is_none()
            || self.process_group_id.is_none()
            || self.job.is_none()
            || self.job_management.is_some()
            || self.generation_handoff_source.is_some()
            || self.barrier.is_some()
        {
            fail(messages.protocol_failed);
        }
        ContainedServiceJob {
            containment: self.containment,
            job: self.job.take(),
            cleanup_budget_reported: false,
        }
    }
}

struct ContainedServiceJob {
    containment: ServiceContainment,
    job: Option<CapabilityHandle>,
    cleanup_budget_reported: bool,
}

struct NullfsGenerationResources<'a> {
    readiness_endpoint: &'a mut CapabilityHandle,
    request_endpoint: &'a mut CapabilityHandle,
    job: &'a mut ContainedServiceJob,
}

impl ContainedServiceJob {
    fn request_termination(&mut self) -> bool {
        let Some(handle) = self.job else {
            report_service_cleanup_diagnostic(
                self.containment.service,
                CleanupPhase::JobDrain,
                CleanupOperation::JobTerminate,
                CleanupObservation::MissingHandle,
            );
            return false;
        };
        match service_cleanup::classify_job_terminate(
            ipc::job_terminate(handle).map_err(ipc::Error::code),
        ) {
            CleanupAction::Progress => true,
            CleanupAction::Unexpected(observation) => {
                report_service_cleanup_diagnostic(
                    self.containment.service,
                    CleanupPhase::JobDrain,
                    CleanupOperation::JobTerminate,
                    observation,
                );
                false
            }
            CleanupAction::Pending | CleanupAction::Complete | CleanupAction::Retry => false,
        }
    }

    fn drain(&mut self) -> bool {
        let mut leader_process_id = None;
        let mut process_group_id = None;
        terminate_and_drain_service_job(
            self.containment.service,
            &mut self.job,
            &mut leader_process_id,
            &mut process_group_id,
            self.containment.drained_message,
            &mut self.cleanup_budget_reported,
        )
    }
}

struct DefinitionServiceRuntime<'a> {
    definition: ServiceDefinition<'a>,
    process_id: Option<ProcessId>,
    process_group_id: Option<ProcessId>,
    job: Option<CapabilityHandle>,
    generation: Option<ProviderGeneration>,
    readiness_endpoint: Option<CapabilityHandle>,
    restart_count: u32,
    restart_deferred: bool,
    cleanup_attempt: Option<DefinitionActivationAttempt>,
    cleanup_budget_reported: bool,
}

impl<'a> DefinitionServiceRuntime<'a> {
    const fn new(definition: ServiceDefinition<'a>) -> Self {
        Self {
            definition,
            process_id: None,
            process_group_id: None,
            job: None,
            generation: None,
            readiness_endpoint: None,
            restart_count: 0,
            restart_deferred: false,
            cleanup_attempt: None,
            cleanup_budget_reported: false,
        }
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NullfsLifecyclePhase {
    Idle,
    WaitQuiesced {
        process_id: ProcessId,
        generation: ProviderGeneration,
        transition_id: u64,
        yields_remaining: u32,
    },
    QueueUnmount {
        process_id: ProcessId,
        generation: ProviderGeneration,
        transition_id: u64,
        yields_remaining: u32,
    },
    WaitCleanExit {
        process_id: ProcessId,
        generation: ProviderGeneration,
        transition_id: u64,
        yields_remaining: u32,
        clean_seen: bool,
        final_status: Option<u64>,
    },
    ForceWait {
        process_id: ProcessId,
        generation: ProviderGeneration,
        attempts_remaining: u32,
    },
}

struct NullfsLifecycle {
    phase: NullfsLifecyclePhase,
    next_transition_id: u64,
    last_exit_was_clean: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NullfsRestartRequestError {
    AlreadyPending,
    InvalidState,
    Busy,
}

impl NullfsLifecycle {
    const fn new() -> Self {
        Self {
            phase: NullfsLifecyclePhase::Idle,
            next_transition_id: 1,
            last_exit_was_clean: false,
        }
    }

    fn request_restart(
        &mut self,
        service: &mut ServiceRuntime,
        generation: ProviderGeneration,
        request_endpoint: CapabilityHandle,
    ) -> Result<(), NullfsRestartRequestError> {
        if self.phase != NullfsLifecyclePhase::Idle {
            return Err(NullfsRestartRequestError::AlreadyPending);
        }
        let process_id = match service.request_restart() {
            Ok(process_id) => process_id,
            Err(RestartRequestError::AlreadyPending) => {
                return Err(NullfsRestartRequestError::AlreadyPending);
            }
            Err(RestartRequestError::InvalidState) => {
                return Err(NullfsRestartRequestError::InvalidState);
            }
        };
        let transition_id = self.next_transition_id;
        self.next_transition_id = transition_id
            .checked_add(1)
            .filter(|next| *next != 0)
            .unwrap_or_else(|| fail(NULLFS_SERVICE_PROTOCOL_FAILED));
        let message = filesystem_protocol::lifecycle::Message::new(
            filesystem_protocol::lifecycle::kind::QUIESCE,
            generation.get(),
            transition_id,
        )
        .unwrap_or_else(|| fail(NULLFS_SERVICE_PROTOCOL_FAILED));
        if ipc::send(request_endpoint, &message.encode(), None).is_err() {
            service.cancel_restart();
            return Err(NullfsRestartRequestError::Busy);
        }
        self.last_exit_was_clean = false;
        self.phase = NullfsLifecyclePhase::WaitQuiesced {
            process_id,
            generation,
            transition_id,
            yields_remaining: NULLFS_QUIESCE_GRACE_YIELDS,
        };
        Ok(())
    }

    fn advance(
        &mut self,
        current_process_id: Option<ProcessId>,
        current_generation: ProviderGeneration,
        readiness_endpoint: CapabilityHandle,
        request_endpoint: CapabilityHandle,
        job: &mut ContainedServiceJob,
    ) -> Option<u64> {
        if self.phase == NullfsLifecyclePhase::Idle {
            let status = current_process_id.and_then(try_wait_final_status);
            if status.is_some() {
                self.last_exit_was_clean = false;
            }
            return status;
        }

        let (process_id, generation) = match self.phase {
            NullfsLifecyclePhase::Idle => unreachable!(),
            NullfsLifecyclePhase::WaitQuiesced {
                process_id,
                generation,
                ..
            }
            | NullfsLifecyclePhase::QueueUnmount {
                process_id,
                generation,
                ..
            }
            | NullfsLifecyclePhase::WaitCleanExit {
                process_id,
                generation,
                ..
            }
            | NullfsLifecyclePhase::ForceWait {
                process_id,
                generation,
                ..
            } => (process_id, generation),
        };
        if current_process_id != Some(process_id) || current_generation != generation {
            fail(NULLFS_SERVICE_PROTOCOL_FAILED);
        }

        self.receive_event(readiness_endpoint, job);
        if let NullfsLifecyclePhase::WaitCleanExit {
            clean_seen: true,
            final_status: Some(0),
            ..
        } = self.phase
        {
            self.phase = NullfsLifecyclePhase::Idle;
            self.last_exit_was_clean = true;
            return Some(0);
        }

        if let NullfsLifecyclePhase::QueueUnmount {
            process_id,
            generation,
            transition_id,
            yields_remaining,
        } = self.phase
        {
            let message = filesystem_protocol::lifecycle::Message::new(
                filesystem_protocol::lifecycle::kind::UNMOUNT,
                generation.get(),
                transition_id,
            )
            .unwrap_or_else(|| fail(NULLFS_SERVICE_PROTOCOL_FAILED));
            match ipc::send(request_endpoint, &message.encode(), None) {
                Ok(()) => {
                    self.phase = NullfsLifecyclePhase::WaitCleanExit {
                        process_id,
                        generation,
                        transition_id,
                        yields_remaining: NULLFS_QUIESCE_GRACE_YIELDS,
                        clean_seen: false,
                        final_status: None,
                    };
                }
                Err(error) if error == ipc::Error::TRY_AGAIN && yields_remaining != 0 => {
                    self.phase = NullfsLifecyclePhase::QueueUnmount {
                        process_id,
                        generation,
                        transition_id,
                        yields_remaining: yields_remaining - 1,
                    };
                }
                Err(_) => self.begin_force(process_id, generation, job),
            }
        }

        let final_status_pending = !matches!(
            self.phase,
            NullfsLifecyclePhase::WaitCleanExit {
                final_status: Some(_),
                ..
            }
        );
        if final_status_pending && let Some(status) = try_wait_final_status(process_id) {
            match self.phase {
                NullfsLifecyclePhase::WaitCleanExit {
                    process_id,
                    generation,
                    transition_id,
                    yields_remaining,
                    clean_seen,
                    ..
                } if status == 0 && clean_seen => {
                    self.phase = NullfsLifecyclePhase::Idle;
                    self.last_exit_was_clean = true;
                    return Some(status);
                }
                NullfsLifecyclePhase::WaitCleanExit {
                    process_id,
                    generation,
                    transition_id,
                    yields_remaining,
                    clean_seen,
                    ..
                } if status == 0 => {
                    self.phase = NullfsLifecyclePhase::WaitCleanExit {
                        process_id,
                        generation,
                        transition_id,
                        yields_remaining,
                        clean_seen,
                        final_status: Some(status),
                    };
                }
                _ => {
                    offline_filesystem_provider(
                        platform::FilesystemProvider::Nullfs,
                        generation,
                        NULLFS_SERVICE_FAILED,
                    );
                    self.phase = NullfsLifecyclePhase::Idle;
                    self.last_exit_was_clean = false;
                    return Some(status);
                }
            }
        }

        match self.phase {
            NullfsLifecyclePhase::WaitQuiesced {
                process_id,
                generation,
                transition_id: _,
                yields_remaining: 0,
            } => self.begin_force(process_id, generation, job),
            NullfsLifecyclePhase::WaitQuiesced {
                process_id,
                generation,
                transition_id,
                yields_remaining,
            } => {
                self.phase = NullfsLifecyclePhase::WaitQuiesced {
                    process_id,
                    generation,
                    transition_id,
                    yields_remaining: yields_remaining - 1,
                };
            }
            NullfsLifecyclePhase::WaitCleanExit {
                process_id: _,
                generation: _,
                transition_id: _,
                yields_remaining: 0,
                clean_seen: _,
                final_status: Some(status),
            } => {
                self.phase = NullfsLifecyclePhase::Idle;
                self.last_exit_was_clean = false;
                return Some(status);
            }
            NullfsLifecyclePhase::WaitCleanExit {
                process_id,
                generation,
                transition_id: _,
                yields_remaining: 0,
                ..
            } => self.begin_force(process_id, generation, job),
            NullfsLifecyclePhase::WaitCleanExit {
                process_id,
                generation,
                transition_id,
                yields_remaining,
                clean_seen,
                final_status,
            } => {
                self.phase = NullfsLifecyclePhase::WaitCleanExit {
                    process_id,
                    generation,
                    transition_id,
                    yields_remaining: yields_remaining - 1,
                    clean_seen,
                    final_status,
                };
            }
            NullfsLifecyclePhase::ForceWait {
                process_id: _,
                generation: _,
                attempts_remaining: 0,
            } => fail(NULLFS_SERVICE_FAILED),
            NullfsLifecyclePhase::ForceWait {
                process_id,
                generation,
                attempts_remaining,
            } => {
                if !job.request_termination() && attempts_remaining == 1 {
                    fail(NULLFS_SERVICE_FAILED);
                }
                self.phase = NullfsLifecyclePhase::ForceWait {
                    process_id,
                    generation,
                    attempts_remaining: attempts_remaining - 1,
                };
            }
            NullfsLifecyclePhase::Idle | NullfsLifecyclePhase::QueueUnmount { .. } => {}
        }
        None
    }

    fn receive_event(
        &mut self,
        readiness_endpoint: CapabilityHandle,
        job: &mut ContainedServiceJob,
    ) {
        let mut bytes = [0_u8; userspace::abi::limits::MAX_IPC_MESSAGE_BYTES];
        let received = match ipc::try_receive(readiness_endpoint, &mut bytes) {
            Ok(received) => received,
            Err(error) if error == ipc::Error::TRY_AGAIN => return,
            Err(_) => fail(NULLFS_SERVICE_PROTOCOL_FAILED),
        };
        let expected_process = match self.phase {
            NullfsLifecyclePhase::Idle => return,
            NullfsLifecyclePhase::WaitQuiesced { process_id, .. }
            | NullfsLifecyclePhase::QueueUnmount { process_id, .. }
            | NullfsLifecyclePhase::WaitCleanExit { process_id, .. }
            | NullfsLifecyclePhase::ForceWait { process_id, .. } => process_id,
        };
        let capability_valid = if let Some(capability) = received.capability {
            let _ = ipc::close(capability.handle);
            false
        } else {
            true
        };
        let message = filesystem_protocol::lifecycle::Message::decode(&bytes[..received.bytes]);
        if received.sender_process_id != expected_process || !capability_valid {
            self.force_current(job);
            return;
        }
        let Ok(message) = message else {
            self.force_current(job);
            return;
        };
        match self.phase {
            NullfsLifecyclePhase::WaitQuiesced {
                process_id,
                generation,
                transition_id,
                ..
            } if message.kind == filesystem_protocol::lifecycle::kind::QUIESCED
                && message.generation == generation.get()
                && message.transition_id == transition_id =>
            {
                offline_filesystem_provider(
                    platform::FilesystemProvider::Nullfs,
                    generation,
                    NULLFS_SERVICE_FAILED,
                );
                self.phase = NullfsLifecyclePhase::QueueUnmount {
                    process_id,
                    generation,
                    transition_id,
                    yields_remaining: NULLFS_QUIESCE_GRACE_YIELDS,
                };
            }
            NullfsLifecyclePhase::WaitCleanExit {
                process_id,
                generation,
                transition_id,
                yields_remaining,
                final_status,
                ..
            } if message.kind == filesystem_protocol::lifecycle::kind::CLEAN_UNMOUNTED
                && message.generation == generation.get()
                && message.transition_id == transition_id =>
            {
                self.phase = NullfsLifecyclePhase::WaitCleanExit {
                    process_id,
                    generation,
                    transition_id,
                    yields_remaining,
                    clean_seen: true,
                    final_status,
                };
            }
            _ => self.force_current(job),
        }
    }

    fn force_current(&mut self, job: &mut ContainedServiceJob) {
        let (process_id, generation) = match self.phase {
            NullfsLifecyclePhase::Idle => return,
            NullfsLifecyclePhase::WaitQuiesced {
                process_id,
                generation,
                ..
            }
            | NullfsLifecyclePhase::QueueUnmount {
                process_id,
                generation,
                ..
            }
            | NullfsLifecyclePhase::WaitCleanExit {
                process_id,
                generation,
                ..
            }
            | NullfsLifecyclePhase::ForceWait {
                process_id,
                generation,
                ..
            } => (process_id, generation),
        };
        self.begin_force(process_id, generation, job);
    }

    fn begin_force(
        &mut self,
        process_id: ProcessId,
        generation: ProviderGeneration,
        job: &mut ContainedServiceJob,
    ) {
        offline_filesystem_provider(
            platform::FilesystemProvider::Nullfs,
            generation,
            NULLFS_SERVICE_FAILED,
        );
        let _ = syscall::write_all(
            STDOUT,
            b"userspace init: NullFS quiesce failed; forcing dirty recovery\n",
        );
        if !job.request_termination() {
            fail(NULLFS_SERVICE_FAILED);
        }
        self.phase = NullfsLifecyclePhase::ForceWait {
            process_id,
            generation,
            attempts_remaining: NULLFS_FORCE_TERMINATION_ATTEMPTS,
        };
        self.last_exit_was_clean = false;
    }

    fn shorten_quiesce_grace_for_test(&mut self) {
        let NullfsLifecyclePhase::WaitQuiesced {
            process_id,
            generation,
            transition_id,
            ..
        } = self.phase
        else {
            fail(NULLFS_RESTART_PROBE_FAILED);
        };
        self.phase = NullfsLifecyclePhase::WaitQuiesced {
            process_id,
            generation,
            transition_id,
            yields_remaining: NULLFS_TEST_QUIESCE_GRACE_YIELDS,
        };
    }

    const fn last_exit_was_clean(&self) -> bool {
        self.last_exit_was_clean
    }
}

fn try_wait_final_status(process_id: ProcessId) -> Option<u64> {
    match syscall::try_wait_child(process_id) {
        Ok(status) if status.continued() || status.stopped_signal().is_some() => None,
        Ok(status) => Some(status.raw()),
        Err(error)
            if error == syscall::Errno::TRY_AGAIN || error == syscall::Errno::INTERRUPTED =>
        {
            None
        }
        Err(_) => fail(NULLFS_SERVICE_FAILED),
    }
}

struct ServiceControlState {
    observation_source: CapabilityHandle,
    observation_ingress: ControlIngress,
    mutation_source: CapabilityHandle,
    mutation_ingress: ControlIngress,
    nullfs_lifecycle: NullfsLifecycle,
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
            nullfs_lifecycle: NullfsLifecycle::new(),
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

    fn pump_mutation(
        &mut self,
        registry: ServiceRegistryMut<'_>,
        nullfs_request_endpoint: CapabilityHandle,
    ) -> usize {
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
                &mut self.nullfs_lifecycle,
                nullfs_request_endpoint,
            );
            let _ = request.reply(response);
        }
        processed
    }

    fn drain_mutation(
        &mut self,
        registry: ServiceRegistryMut<'_>,
        nullfs_request_endpoint: CapabilityHandle,
    ) {
        loop {
            let processed = self.pump_mutation(
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
                nullfs_request_endpoint,
            );
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
    nullfs_lifecycle: &mut NullfsLifecycle,
    nullfs_request_endpoint: CapabilityHandle,
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
                request_nullfs_restart(
                    service,
                    registry.nullfs,
                    registry.nullfs_generation,
                    nullfs_lifecycle,
                    nullfs_request_endpoint,
                )
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

fn request_nullfs_restart(
    service: ServiceId,
    runtime: &mut ServiceRuntime,
    generation: ProviderGeneration,
    lifecycle: &mut NullfsLifecycle,
    request_endpoint: CapabilityHandle,
) -> TargetResponse {
    match lifecycle.request_restart(runtime, generation, request_endpoint) {
        Ok(()) => TargetResponse::success(service_record(service, runtime, generation)),
        Err(NullfsRestartRequestError::AlreadyPending | NullfsRestartRequestError::Busy) => {
            TargetResponse::failure(service, ServiceControlFailure::Busy)
        }
        Err(NullfsRestartRequestError::InvalidState) => {
            TargetResponse::failure(service, ServiceControlFailure::InvalidState)
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

struct LoggingActivationAttempt {
    generation_handoff_source: Option<CapabilityHandle>,
    readiness_source: Option<CapabilityHandle>,
    producer_source: Option<CapabilityHandle>,
    observer_source: Option<CapabilityHandle>,
    barrier: Option<syscall::LaunchBarrier>,
    process_id: Option<ProcessId>,
    process_group_id: Option<ProcessId>,
    job_management: Option<CapabilityHandle>,
    job: Option<CapabilityHandle>,
    job_assigned: bool,
    cleanup_budget_reported: bool,
}

impl LoggingActivationAttempt {
    const fn new() -> Self {
        Self {
            generation_handoff_source: None,
            readiness_source: None,
            producer_source: None,
            observer_source: None,
            barrier: None,
            process_id: None,
            process_group_id: None,
            job_management: None,
            job: None,
            job_assigned: false,
            cleanup_budget_reported: false,
        }
    }

    fn close_capability(handle: &mut Option<CapabilityHandle>) -> bool {
        close_cleanup_capability(
            CleanupService::Logging,
            CleanupPhase::ResourceRelease,
            handle,
        )
    }

    fn release_barrier(&mut self) -> bool {
        release_cleanup_barrier(CleanupService::Logging, &mut self.barrier)
    }

    fn release_child(&mut self) -> bool {
        let generation_closed = Self::close_capability(&mut self.generation_handoff_source);
        let barrier_released = self.release_barrier();
        generation_closed && barrier_released
    }

    fn abort(&mut self) -> bool {
        let process_clean = if self.job_assigned {
            let clean = terminate_and_drain_service_job(
                CleanupService::Logging,
                &mut self.job,
                &mut self.process_id,
                &mut self.process_group_id,
                LOGGING_SERVICE_JOB_DRAINED,
                &mut self.cleanup_budget_reported,
            );
            if clean {
                self.job_assigned = false;
            }
            clean
        } else {
            let process_group_clean = match self.process_group_id {
                Some(process_group_id) => {
                    if terminate_unassigned_service_process_group(
                        CleanupService::Logging,
                        process_group_id,
                        &mut self.process_id,
                        &mut self.cleanup_budget_reported,
                    ) {
                        self.process_group_id = None;
                        true
                    } else {
                        false
                    }
                }
                None => self.process_id.is_none(),
            };
            process_group_clean
                && close_empty_service_job(
                    CleanupService::Logging,
                    &mut self.job_management,
                    &mut self.job,
                )
        };
        let management_closed = if process_clean {
            Self::close_capability(&mut self.job_management)
        } else {
            false
        };
        let generation_closed = if process_clean {
            Self::close_capability(&mut self.generation_handoff_source)
        } else {
            false
        };
        let readiness_closed = if process_clean {
            Self::close_capability(&mut self.readiness_source)
        } else {
            false
        };
        let producer_closed = if process_clean {
            Self::close_capability(&mut self.producer_source)
        } else {
            false
        };
        let observer_closed = if process_clean {
            Self::close_capability(&mut self.observer_source)
        } else {
            false
        };
        let barrier_released = if process_clean {
            self.release_barrier()
        } else {
            false
        };
        process_clean
            && management_closed
            && generation_closed
            && readiness_closed
            && producer_closed
            && observer_closed
            && barrier_released
    }
}

struct LoggingGeneration {
    generation: ProviderGeneration,
    readiness_source: CapabilityHandle,
    producer_source: CapabilityHandle,
    observer_source: CapabilityHandle,
    producer_object_id: u64,
    observer_object_id: u64,
    job: Option<CapabilityHandle>,
    cleanup_budget_reported: bool,
    readiness_received: bool,
    child_stopped: bool,
    routes_published: bool,
    readiness_yields_remaining: u32,
    readiness_force_termination_sent: bool,
    force_termination_attempts_remaining: u32,
}

impl LoggingGeneration {
    fn drain_job(&mut self) {
        let mut leader_process_id = None;
        let mut process_group_id = None;
        if !terminate_and_drain_service_job(
            CleanupService::Logging,
            &mut self.job,
            &mut leader_process_id,
            &mut process_group_id,
            LOGGING_SERVICE_JOB_DRAINED,
            &mut self.cleanup_budget_reported,
        ) {
            fail(LOGGING_SERVICE_FAILED);
        }
    }

    fn close(self) {
        if self.job.is_some() {
            fail(LOGGING_SERVICE_PROTOCOL_FAILED);
        }
        let mut readiness_source = Some(self.readiness_source);
        if !close_cleanup_capability(
            CleanupService::Logging,
            CleanupPhase::ResourceRelease,
            &mut readiness_source,
        ) {
            fail(LOGGING_SERVICE_BOOTSTRAP_FAILED);
        }
        let mut producer_source = Some(self.producer_source);
        if !close_cleanup_capability(
            CleanupService::Logging,
            CleanupPhase::ResourceRelease,
            &mut producer_source,
        ) {
            fail(LOGGING_SERVICE_BOOTSTRAP_FAILED);
        }
        let mut observer_source = Some(self.observer_source);
        if !close_cleanup_capability(
            CleanupService::Logging,
            CleanupPhase::ResourceRelease,
            &mut observer_source,
        ) {
            fail(LOGGING_SERVICE_BOOTSTRAP_FAILED);
        }
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
        if let Some(mut generation) = self.current.take() {
            generation.drain_job();
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
    SpawnTmpfsRestart,
    WaitTmpfsRestartClient(ProcessId),
    WaitTmpfsReady,
    WaitVfsRestartClient(ProcessId),
    WaitVfsReady,
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
                    self.phase = LoggingLifecycleCheckPhase::SpawnTmpfsRestart;
                } else {
                    self.phase = LoggingLifecycleCheckPhase::BeginUnsupported(index + 1);
                }
            }
            LoggingLifecycleCheckPhase::SpawnTmpfsRestart => {
                let process_id = spawn_sv_command(
                    SV_RESTART_TMPFS_COMMAND,
                    control.mutation_source,
                    SV_MUTATION_HANDLE,
                );
                self.phase = LoggingLifecycleCheckPhase::WaitTmpfsRestartClient(process_id);
            }
            LoggingLifecycleCheckPhase::WaitTmpfsRestartClient(process_id) => {
                let Some(success) = try_wait_probe(process_id) else {
                    return;
                };
                if !success {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                self.phase = LoggingLifecycleCheckPhase::WaitTmpfsReady;
            }
            LoggingLifecycleCheckPhase::WaitTmpfsReady => {
                let previous = self.filesystem_records[1];
                let current = filesystem_records[1];
                if current.observed_state() != ObservedState::Ready {
                    return;
                }
                let previous_generation = previous
                    .generation()
                    .unwrap_or_else(|| fail(LOGGING_LIFECYCLE_TEST_FAILED));
                if filesystem_records[0] != self.filesystem_records[0]
                    || current.service() != TMPFS_SERVICE_ID
                    || current.desired_state() != DesiredState::Running
                    || current.generation().map(ProviderGeneration::get)
                        != Some(previous_generation.get().saturating_add(1))
                {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                self.filesystem_records[1] = current;
                let process_id = spawn_sv_command(
                    SV_RESTART_VFS_COMMAND,
                    control.mutation_source,
                    SV_MUTATION_HANDLE,
                );
                self.phase = LoggingLifecycleCheckPhase::WaitVfsRestartClient(process_id);
            }
            LoggingLifecycleCheckPhase::WaitVfsRestartClient(process_id) => {
                let Some(success) = try_wait_probe(process_id) else {
                    return;
                };
                if !success {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                self.phase = LoggingLifecycleCheckPhase::WaitVfsReady;
            }
            LoggingLifecycleCheckPhase::WaitVfsReady => {
                let previous = self.filesystem_records[2];
                let current = filesystem_records[2];
                if current.observed_state() != ObservedState::Ready {
                    return;
                }
                let previous_generation = previous
                    .generation()
                    .unwrap_or_else(|| fail(LOGGING_LIFECYCLE_TEST_FAILED));
                if filesystem_records[0] != self.filesystem_records[0]
                    || filesystem_records[1] != self.filesystem_records[1]
                    || current.service() != VFS_SERVICE_ID
                    || current.desired_state() != DesiredState::Running
                    || current.generation().map(ProviderGeneration::get)
                        != Some(previous_generation.get().saturating_add(1))
                {
                    fail(LOGGING_LIFECYCLE_TEST_FAILED);
                }
                self.filesystem_records[2] = current;
                self.phase = LoggingLifecycleCheckPhase::SpawnReadinessTimeout;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DefinitionServiceError {
    Io,
    InvalidDefinition,
    Policy,
    Activation,
    Cleanup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DefinitionStartOutcome {
    Ready,
    Exited { successful: bool },
}

fn load_definition_service<'a>(
    buffer: &'a mut [u8; MAX_DEFINITION_BYTES],
) -> Result<ServiceDefinition<'a>, DefinitionServiceError> {
    let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_LOADING);
    let descriptor = {
        let mut attempts = 0_u32;
        loop {
            match syscall::open(definition_service_probe::DEFINITION_PATH, OpenFlags::READ) {
                Ok(descriptor) => break descriptor,
                Err(error) if error == syscall::Errno::TRY_AGAIN && attempts < 64 => {
                    attempts += 1;
                    syscall::yield_now().map_err(|_| DefinitionServiceError::Io)?;
                }
                Err(_) => return Err(DefinitionServiceError::Io),
            }
        }
    };
    let read_result = (|| {
        let stat = {
            let mut attempts = 0_u32;
            loop {
                match platform::fstat(descriptor) {
                    Ok(stat) => break stat,
                    Err(error) if error == platform::Errno::TRY_AGAIN && attempts < 64 => {
                        attempts += 1;
                        syscall::yield_now().map_err(|_| DefinitionServiceError::Io)?;
                    }
                    Err(_) => return Err(DefinitionServiceError::Io),
                }
            }
        };
        let length = usize::try_from(stat.size).map_err(|_| DefinitionServiceError::Io)?;
        if stat.kind != file::KIND_FILE || length == 0 || length > buffer.len() {
            return Err(DefinitionServiceError::InvalidDefinition);
        }
        let mut completed = 0;
        let mut stalled = 0_u32;
        while completed < length {
            match syscall::read(descriptor, &mut buffer[completed..length]) {
                Ok(0) => return Err(DefinitionServiceError::Io),
                Ok(read) if read <= length - completed => {
                    completed += read;
                    stalled = 0;
                }
                Ok(_) => return Err(DefinitionServiceError::Io),
                Err(error) if error == syscall::Errno::TRY_AGAIN && stalled < 64 => {
                    stalled += 1;
                    syscall::yield_now().map_err(|_| DefinitionServiceError::Io)?;
                }
                Err(_) => return Err(DefinitionServiceError::Io),
            }
        }
        let mut extra = [0_u8; 1];
        stalled = 0;
        loop {
            match syscall::read(descriptor, &mut extra) {
                Ok(0) => break,
                Ok(_) => return Err(DefinitionServiceError::InvalidDefinition),
                Err(error) if error == syscall::Errno::TRY_AGAIN && stalled < 64 => {
                    stalled += 1;
                    syscall::yield_now().map_err(|_| DefinitionServiceError::Io)?;
                }
                Err(_) => return Err(DefinitionServiceError::Io),
            }
        }
        Ok(length)
    })();
    let close_result = syscall::close(descriptor);
    if close_result.is_err() {
        return Err(DefinitionServiceError::Io);
    }
    let length = read_result?;
    let definition = service_definition::parse(&buffer[..length])
        .map_err(|_| DefinitionServiceError::InvalidDefinition)?;
    if definition.service_id().as_bytes() != &definition_service_probe::SERVICE_ID_BYTES
        || definition.name() != definition_service_probe::SERVICE_NAME
        || definition.executable().as_bytes() != definition_service_probe::EXECUTABLE_PATH
        || definition.arguments().len() != 1
        || definition.arguments().next().map(str::as_bytes)
            != Some(definition_service_probe::MANAGED_ARGUMENT)
        || definition.readiness() != Readiness::Notify
        || definition.ready_message().map(str::as_bytes)
            != Some(definition_service_probe::READY_MESSAGE)
        || definition.restart_policy() != RestartPolicy::OnFailure
        || definition.restart_limit() != definition_service_probe::RESTART_LIMIT
        || definition.restart_backoff_yields() != definition_service_probe::RESTART_BACKOFF_YIELDS
    {
        return Err(DefinitionServiceError::Policy);
    }
    Ok(definition)
}

fn definition_service_selects_restart(
    runtime: &DefinitionServiceRuntime<'_>,
    successful: bool,
) -> bool {
    match runtime.definition.restart_policy() {
        RestartPolicy::Never => false,
        RestartPolicy::OnFailure => !successful,
        RestartPolicy::Always => true,
    }
}

fn definition_service_consume_restart(runtime: &mut DefinitionServiceRuntime<'_>) -> bool {
    if runtime.restart_count >= runtime.definition.restart_limit() {
        return false;
    }
    runtime.restart_count += 1;
    true
}

fn report_service_cleanup_diagnostic(
    service: CleanupService,
    phase: CleanupPhase,
    operation: CleanupOperation,
    observation: CleanupObservation,
) {
    let diagnostic = CleanupDiagnostic {
        service,
        phase,
        operation,
        observation,
    };
    let mut output = [0_u8; 192];
    if let Some(length) = diagnostic.encode(&mut output) {
        let _ = syscall::write_all(STDERR, &output[..length]);
    } else {
        let _ = syscall::write_all(STDERR, b"init: cleanup diagnostic encoding failed\n");
    }
}

fn report_cleanup_budget_exhausted(
    service: CleanupService,
    phase: CleanupPhase,
    operation: CleanupOperation,
    reported: &mut bool,
) {
    if *reported {
        return;
    }
    *reported = true;
    report_service_cleanup_diagnostic(
        service,
        phase,
        operation,
        CleanupObservation::BudgetExhausted {
            attempts: SERVICE_JOB_CLEANUP_YIELDS,
        },
    );
}

fn reap_service_leader(service: CleanupService, leader_process_id: &mut Option<ProcessId>) -> bool {
    let Some(process_id) = *leader_process_id else {
        return true;
    };
    loop {
        let result = match syscall::try_wait_child(process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {
                CleanupLeaderResult::Transitional
            }
            Ok(_) => CleanupLeaderResult::Terminal,
            Err(error) => CleanupLeaderResult::Error(error.code()),
        };
        match service_cleanup::classify_leader_wait(
            result,
            syscall::Errno::TRY_AGAIN.code(),
            syscall::Errno::INTERRUPTED.code(),
            syscall::Errno::NO_CHILD.code(),
        ) {
            CleanupAction::Complete => {
                *leader_process_id = None;
                return true;
            }
            CleanupAction::Pending => return false,
            CleanupAction::Retry => {}
            CleanupAction::Unexpected(observation) => {
                report_service_cleanup_diagnostic(
                    service,
                    CleanupPhase::LeaderReap,
                    CleanupOperation::TryWaitChild,
                    observation,
                );
                return false;
            }
            CleanupAction::Progress => return false,
        }
    }
}

fn terminate_unassigned_service_process_group(
    service: CleanupService,
    process_group_id: ProcessId,
    leader_process_id: &mut Option<ProcessId>,
    budget_reported: &mut bool,
) -> bool {
    let mut group_empty = false;
    let mut leader_reaped = leader_process_id.is_none();
    for _ in 0..SERVICE_JOB_CLEANUP_YIELDS {
        group_empty = match service_cleanup::classify_group_signal(
            syscall::signal_process_group(process_group_id, signal::KILL)
                .map_err(syscall::Errno::code),
            syscall::Errno::NO_CHILD.code(),
        ) {
            CleanupAction::Progress => false,
            CleanupAction::Complete => true,
            CleanupAction::Unexpected(observation) => {
                report_service_cleanup_diagnostic(
                    service,
                    CleanupPhase::UnassignedProcessGroup,
                    CleanupOperation::SignalProcessGroup,
                    observation,
                );
                return false;
            }
            CleanupAction::Pending | CleanupAction::Retry => return false,
        };
        leader_reaped = reap_service_leader(service, leader_process_id);
        if group_empty && leader_reaped {
            return true;
        }
        match service_cleanup::classify_unit(syscall::yield_now().map_err(syscall::Errno::code)) {
            CleanupAction::Complete => {}
            CleanupAction::Unexpected(observation) => {
                report_service_cleanup_diagnostic(
                    service,
                    CleanupPhase::UnassignedProcessGroup,
                    CleanupOperation::YieldNow,
                    observation,
                );
                return false;
            }
            CleanupAction::Progress | CleanupAction::Pending | CleanupAction::Retry => {
                return false;
            }
        }
    }
    let (phase, operation) = if group_empty && !leader_reaped {
        (CleanupPhase::LeaderReap, CleanupOperation::TryWaitChild)
    } else {
        (
            CleanupPhase::UnassignedProcessGroup,
            CleanupOperation::SignalProcessGroup,
        )
    };
    report_cleanup_budget_exhausted(service, phase, operation, budget_reported);
    false
}

fn close_cleanup_capability(
    service: CleanupService,
    phase: CleanupPhase,
    handle: &mut Option<CapabilityHandle>,
) -> bool {
    let Some(value) = *handle else {
        return true;
    };
    match service_cleanup::classify_unit(ipc::close(value).map_err(ipc::Error::code)) {
        CleanupAction::Complete => {
            *handle = None;
            true
        }
        CleanupAction::Unexpected(observation) => {
            report_service_cleanup_diagnostic(
                service,
                phase,
                CleanupOperation::CapabilityClose,
                observation,
            );
            false
        }
        CleanupAction::Progress | CleanupAction::Pending | CleanupAction::Retry => false,
    }
}

fn release_cleanup_barrier(
    service: CleanupService,
    barrier: &mut Option<syscall::LaunchBarrier>,
) -> bool {
    let Some(value) = barrier.as_mut() else {
        return true;
    };
    match service_cleanup::classify_unit(value.release_in_place().map_err(syscall::Errno::code)) {
        CleanupAction::Complete => {
            *barrier = None;
            true
        }
        CleanupAction::Unexpected(observation) => {
            report_service_cleanup_diagnostic(
                service,
                CleanupPhase::ResourceRelease,
                CleanupOperation::LaunchBarrierRelease,
                observation,
            );
            false
        }
        CleanupAction::Progress | CleanupAction::Pending | CleanupAction::Retry => false,
    }
}

fn close_empty_service_job(
    service: CleanupService,
    job_management: &mut Option<CapabilityHandle>,
    job: &mut Option<CapabilityHandle>,
) -> bool {
    let inspection_handle = (*job).or(*job_management);
    if let Some(handle) = inspection_handle {
        let result = match ipc::job_try_wait(handle) {
            Ok(exit) => CleanupJobWaitResult::Exit {
                process_id: exit.process_id,
                status: exit.status.raw(),
            },
            Err(error) => CleanupJobWaitResult::Error(error.code()),
        };
        match service_cleanup::classify_job_wait(
            result,
            ipc::Error::TRY_AGAIN.code(),
            ipc::Error::NO_CHILD.code(),
            true,
        ) {
            CleanupAction::Complete => {}
            CleanupAction::Unexpected(observation) => {
                report_service_cleanup_diagnostic(
                    service,
                    CleanupPhase::EmptyJobInspection,
                    CleanupOperation::JobTryWait,
                    observation,
                );
                return false;
            }
            CleanupAction::Progress | CleanupAction::Pending | CleanupAction::Retry => {
                return false;
            }
        }
    }
    let job_closed = close_cleanup_capability(service, CleanupPhase::ResourceRelease, job);
    let management_closed =
        close_cleanup_capability(service, CleanupPhase::ResourceRelease, job_management);
    job_closed && management_closed
}

fn terminate_and_drain_service_job(
    service: CleanupService,
    job: &mut Option<CapabilityHandle>,
    leader_process_id: &mut Option<ProcessId>,
    process_group_id: &mut Option<ProcessId>,
    drained_message: &[u8],
    budget_reported: &mut bool,
) -> bool {
    let Some(handle) = *job else {
        report_service_cleanup_diagnostic(
            service,
            CleanupPhase::JobDrain,
            CleanupOperation::JobTryWait,
            CleanupObservation::MissingHandle,
        );
        return false;
    };
    let mut job_drained = false;
    let mut leader_reaped = leader_process_id.is_none();
    for _ in 0..SERVICE_JOB_CLEANUP_YIELDS {
        match service_cleanup::classify_job_terminate(
            ipc::job_terminate(handle).map_err(ipc::Error::code),
        ) {
            CleanupAction::Progress => {}
            CleanupAction::Unexpected(observation) => {
                report_service_cleanup_diagnostic(
                    service,
                    CleanupPhase::JobDrain,
                    CleanupOperation::JobTerminate,
                    observation,
                );
                return false;
            }
            CleanupAction::Pending | CleanupAction::Complete | CleanupAction::Retry => {
                return false;
            }
        }
        job_drained = loop {
            let result = match ipc::job_try_wait(handle) {
                Ok(exit) => CleanupJobWaitResult::Exit {
                    process_id: exit.process_id,
                    status: exit.status.raw(),
                },
                Err(error) => CleanupJobWaitResult::Error(error.code()),
            };
            match service_cleanup::classify_job_wait(
                result,
                ipc::Error::TRY_AGAIN.code(),
                ipc::Error::NO_CHILD.code(),
                false,
            ) {
                CleanupAction::Progress => {}
                CleanupAction::Pending => break false,
                CleanupAction::Complete => break true,
                CleanupAction::Unexpected(observation) => {
                    report_service_cleanup_diagnostic(
                        service,
                        CleanupPhase::JobDrain,
                        CleanupOperation::JobTryWait,
                        observation,
                    );
                    return false;
                }
                CleanupAction::Retry => return false,
            }
        };
        leader_reaped = reap_service_leader(service, leader_process_id);
        if job_drained && leader_reaped {
            if !close_cleanup_capability(service, CleanupPhase::ResourceRelease, job) {
                return false;
            }
            *process_group_id = None;
            let _ = syscall::write_all(STDOUT, drained_message);
            return true;
        }
        match service_cleanup::classify_unit(syscall::yield_now().map_err(syscall::Errno::code)) {
            CleanupAction::Complete => {}
            CleanupAction::Unexpected(observation) => {
                report_service_cleanup_diagnostic(
                    service,
                    CleanupPhase::JobDrain,
                    CleanupOperation::YieldNow,
                    observation,
                );
                return false;
            }
            CleanupAction::Progress | CleanupAction::Pending | CleanupAction::Retry => {
                return false;
            }
        }
    }
    let (phase, operation) = if job_drained && !leader_reaped {
        (CleanupPhase::LeaderReap, CleanupOperation::TryWaitChild)
    } else {
        (CleanupPhase::JobDrain, CleanupOperation::JobTryWait)
    };
    report_cleanup_budget_exhausted(service, phase, operation, budget_reported);
    false
}

fn clear_definition_service_process(
    runtime: &mut DefinitionServiceRuntime<'_>,
    leader_reaped: bool,
) -> bool {
    if leader_reaped {
        runtime.process_id = None;
    }
    let job_clean = match runtime.job {
        Some(_) => terminate_and_drain_service_job(
            CleanupService::Definition,
            &mut runtime.job,
            &mut runtime.process_id,
            &mut runtime.process_group_id,
            DEFINITION_SERVICE_JOB_DRAINED,
            &mut runtime.cleanup_budget_reported,
        ),
        None => {
            runtime.process_group_id = None;
            runtime.process_id.is_none()
        }
    };
    let readiness_closed = close_cleanup_capability(
        CleanupService::Definition,
        CleanupPhase::ResourceRelease,
        &mut runtime.readiness_endpoint,
    );
    if job_clean && readiness_closed {
        runtime.generation = None;
        true
    } else {
        false
    }
}

struct DefinitionActivationAttempt {
    generation_source: Option<CapabilityHandle>,
    readiness_endpoint: Option<CapabilityHandle>,
    bootstrap_sender: Option<CapabilityHandle>,
    bootstrap_receiver_source: Option<CapabilityHandle>,
    barrier: Option<syscall::LaunchBarrier>,
    process_id: Option<ProcessId>,
    process_group_id: Option<ProcessId>,
    job_management: Option<CapabilityHandle>,
    job: Option<CapabilityHandle>,
    job_assigned: bool,
    cleanup_budget_reported: bool,
}

impl DefinitionActivationAttempt {
    const fn new() -> Self {
        Self {
            generation_source: None,
            readiness_endpoint: None,
            bootstrap_sender: None,
            bootstrap_receiver_source: None,
            barrier: None,
            process_id: None,
            process_group_id: None,
            job_management: None,
            job: None,
            job_assigned: false,
            cleanup_budget_reported: false,
        }
    }

    fn close_generation_source(&mut self) -> bool {
        close_cleanup_capability(
            CleanupService::Definition,
            CleanupPhase::ResourceRelease,
            &mut self.generation_source,
        )
    }

    fn close_job_management(&mut self) -> bool {
        close_cleanup_capability(
            CleanupService::Definition,
            CleanupPhase::ResourceRelease,
            &mut self.job_management,
        )
    }

    fn release_barrier(&mut self) -> bool {
        release_cleanup_barrier(CleanupService::Definition, &mut self.barrier)
    }

    fn close_readiness(&mut self) -> bool {
        close_cleanup_capability(
            CleanupService::Definition,
            CleanupPhase::ResourceRelease,
            &mut self.readiness_endpoint,
        )
    }

    fn close_bootstrap(&mut self) -> bool {
        let sender_closed = close_cleanup_capability(
            CleanupService::Definition,
            CleanupPhase::ResourceRelease,
            &mut self.bootstrap_sender,
        );
        let receiver_closed = close_cleanup_capability(
            CleanupService::Definition,
            CleanupPhase::ResourceRelease,
            &mut self.bootstrap_receiver_source,
        );
        sender_closed && receiver_closed
    }

    fn release_child(&mut self) -> bool {
        let generation_closed = self.close_generation_source();
        let bootstrap_closed = self.close_bootstrap();
        let barrier_released = self.release_barrier();
        generation_closed && bootstrap_closed && barrier_released
    }

    fn abort(&mut self) -> bool {
        let generation_closed = self.close_generation_source();
        let readiness_closed = self.close_readiness();
        let bootstrap_closed = self.close_bootstrap();
        let process_clean = if self.job_assigned {
            let clean = terminate_and_drain_service_job(
                CleanupService::Definition,
                &mut self.job,
                &mut self.process_id,
                &mut self.process_group_id,
                DEFINITION_SERVICE_JOB_DRAINED,
                &mut self.cleanup_budget_reported,
            );
            if clean {
                self.job_assigned = false;
            }
            clean
        } else {
            let process_group_clean = match self.process_group_id {
                Some(process_group_id) => {
                    if terminate_unassigned_service_process_group(
                        CleanupService::Definition,
                        process_group_id,
                        &mut self.process_id,
                        &mut self.cleanup_budget_reported,
                    ) {
                        self.process_group_id = None;
                        true
                    } else {
                        false
                    }
                }
                None => self.process_id.is_none(),
            };
            process_group_clean
                && close_empty_service_job(
                    CleanupService::Definition,
                    &mut self.job_management,
                    &mut self.job,
                )
        };
        let management_closed = if process_clean {
            self.close_job_management()
        } else {
            false
        };
        let barrier_released = if process_clean {
            self.release_barrier()
        } else {
            false
        };
        generation_closed
            && readiness_closed
            && bootstrap_closed
            && process_clean
            && management_closed
            && barrier_released
    }

    fn finish_reaped(&mut self) -> bool {
        self.process_id = None;
        self.abort()
    }
}

fn send_definition_process_start(
    attempt: &mut DefinitionActivationAttempt,
    runtime: &DefinitionServiceRuntime<'_>,
    process_id: ProcessId,
    generation: ProviderGeneration,
) -> bool {
    let Some(sender) = attempt.bootstrap_sender else {
        return false;
    };
    let Some(generation_source) = attempt.generation_source else {
        return false;
    };
    let generation_transfer =
        match ipc::duplicate(generation_source, Rights::RECEIVE | Rights::TRANSFER) {
            Ok(handle) => handle,
            Err(_) => return false,
        };
    let readiness_transfer = match attempt.readiness_endpoint {
        Some(readiness) => match ipc::duplicate(readiness, Rights::SEND | Rights::TRANSFER) {
            Ok(handle) => Some(handle),
            Err(_) => {
                let _ = ipc::close(generation_transfer);
                return false;
            }
        },
        None => None,
    };

    let message = match StartupMessage::new(
        StartupRuntimeRole::Service,
        [
            Some(StartupResource {
                role: CapabilityRole::SERVICE_GENERATION,
                required: true,
            }),
            readiness_transfer.map(|_| StartupResource {
                role: CapabilityRole::READINESS,
                required: true,
            }),
        ],
    ) {
        Ok(message) => message,
        Err(_) => {
            let _ = ipc::close(generation_transfer);
            if let Some(handle) = readiness_transfer {
                let _ = ipc::close(handle);
            }
            return false;
        }
    };
    let transfers = [
        Transfer {
            handle: generation_transfer,
            rights: Rights::RECEIVE,
        },
        Transfer {
            handle: readiness_transfer.unwrap_or(0),
            rights: Rights::SEND,
        },
    ];
    let transfer_count = if readiness_transfer.is_some() { 2 } else { 1 };
    if send_startup_message(sender, &message, &transfers[..transfer_count]).is_err() {
        let _ = ipc::close(generation_transfer);
        if let Some(handle) = readiness_transfer {
            let _ = ipc::close(handle);
        }
        return false;
    }

    let mut arguments = [&[][..]; userspace::abi::limits::MAX_ARGUMENTS];
    arguments[0] = runtime.definition.executable().as_bytes();
    let mut argument_count = 1;
    for argument in runtime.definition.arguments() {
        if argument_count == arguments.len() {
            return false;
        }
        arguments[argument_count] = argument.as_bytes();
        argument_count += 1;
    }
    let mut argument_bytes = [0; userspace::abi::limits::MAX_ARGUMENT_BYTES];
    let argument_length =
        match encode_startup_arguments(&arguments[..argument_count], &mut argument_bytes) {
            Ok(length) => length,
            Err(_) => return false,
        };
    let mut environment_bytes = [0; 4];
    let environment_length = match encode_startup_environment(&[], &mut environment_bytes) {
        Ok(length) => length,
        Err(_) => return false,
    };
    let monotonic_start_ns = match platform::monotonic_time_ns() {
        Ok(value) => value,
        Err(_) => return false,
    };
    let identity = StartupIdentity {
        process: process_id,
        package: definition_service_probe::SYSTEM_PACKAGE_ID,
        package_generation: generation.get(),
        executable: definition_service_probe::EXECUTABLE_ID,
        application: 0,
        service: definition_service_probe::SERVICE_NUMERIC_ID,
        component: definition_service_probe::COMPONENT_ID,
        user: 0,
        session: 0,
    }
    .encode();
    let launch = StartupLaunch {
        launch: generation.get(),
        manager_generation: generation.get(),
        namespace_profile: definition_service_probe::NAMESPACE_PROFILE_ID,
        monotonic_start_ns,
        attempt: runtime.restart_count.saturating_add(1),
        reason: if runtime.restart_count == 0 {
            StartupLaunchReason::Activation
        } else {
            StartupLaunchReason::Restart
        },
        flags: 0,
    }
    .encode();
    let sections = [
        StartupSectionPayload {
            id: StartupSectionId::IDENTITY,
            required: true,
            bytes: &identity,
        },
        StartupSectionPayload {
            id: StartupSectionId::ARGUMENTS,
            required: true,
            bytes: &argument_bytes[..argument_length],
        },
        StartupSectionPayload {
            id: StartupSectionId::ENVIRONMENT,
            required: true,
            bytes: &environment_bytes[..environment_length],
        },
        StartupSectionPayload {
            id: StartupSectionId::LAUNCH,
            required: true,
            bytes: &launch,
        },
    ];
    send_process_start_data(sender, &sections).is_ok()
}

fn cleanup_definition_attempt(
    runtime: &mut DefinitionServiceRuntime<'_>,
    mut attempt: DefinitionActivationAttempt,
    leader_reaped: bool,
) -> Result<(), DefinitionServiceError> {
    let cleaned = if leader_reaped {
        attempt.finish_reaped()
    } else {
        attempt.abort()
    };
    if cleaned {
        Ok(())
    } else {
        runtime.cleanup_attempt = Some(attempt);
        Err(DefinitionServiceError::Cleanup)
    }
}

fn start_definition_service(
    runtime: &mut DefinitionServiceRuntime<'_>,
    generations: &mut ProviderGenerationSequence,
) -> Result<DefinitionStartOutcome, DefinitionServiceError> {
    let generation = generations
        .next_generation()
        .map_err(|_| DefinitionServiceError::Activation)?;
    let mut attempt = DefinitionActivationAttempt::new();
    let job_management = match ipc::job_create() {
        Ok(handle) => handle,
        Err(_) => return Err(DefinitionServiceError::Activation),
    };
    attempt.job_management = Some(job_management);
    let job = match ipc::duplicate(job_management, Rights::SIGNAL | Rights::WAIT) {
        Ok(handle) => handle,
        Err(_) => {
            cleanup_definition_attempt(runtime, attempt, false)?;
            return Err(DefinitionServiceError::Activation);
        }
    };
    attempt.job = Some(job);
    let job_handles_valid = matches!(
        ipc::info(job_management),
        Ok(info) if info.kind == ObjectKind::Job && info.rights == Rights::JOB && info.size == 0
    ) && matches!(
        ipc::info(job),
        Ok(info)
            if info.kind == ObjectKind::Job
                && info.rights == Rights::SIGNAL | Rights::WAIT
                && info.size == 0
    ) && ipc::job_try_wait(job).err() == Some(ipc::Error::NO_CHILD);
    if !job_handles_valid {
        cleanup_definition_attempt(runtime, attempt, false)?;
        return Err(DefinitionServiceError::Activation);
    }
    let generation_source = match ipc::endpoint_create() {
        Ok(source) => source,
        Err(_) => {
            cleanup_definition_attempt(runtime, attempt, false)?;
            return Err(DefinitionServiceError::Activation);
        }
    };
    attempt.generation_source = Some(generation_source);
    if queue_service_generation(generation_source, generation).is_err() {
        cleanup_definition_attempt(runtime, attempt, false)?;
        return Err(DefinitionServiceError::Activation);
    }
    if runtime.definition.readiness() == Readiness::Notify {
        let readiness_endpoint = match ipc::endpoint_create() {
            Ok(endpoint) => endpoint,
            Err(_) => {
                cleanup_definition_attempt(runtime, attempt, false)?;
                return Err(DefinitionServiceError::Activation);
            }
        };
        attempt.readiness_endpoint = Some(readiness_endpoint);
    }
    let (bootstrap_sender, bootstrap_receiver_source) = match ipc::endpoint_create_pair() {
        Ok(pair) => pair,
        Err(_) => {
            cleanup_definition_attempt(runtime, attempt, false)?;
            return Err(DefinitionServiceError::Activation);
        }
    };
    attempt.bootstrap_sender = Some(bootstrap_sender);
    attempt.bootstrap_receiver_source = Some(bootstrap_receiver_source);
    let barrier = match syscall::LaunchBarrier::new() {
        Ok(barrier) => barrier,
        Err(_) => {
            cleanup_definition_attempt(runtime, attempt, false)?;
            return Err(DefinitionServiceError::Activation);
        }
    };
    attempt.barrier = Some(barrier);
    let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_STARTING);
    let process_id = match syscall::spawn_command_with_barrier(
        runtime.definition.executable().as_bytes(),
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        attempt
            .barrier
            .as_ref()
            .expect("activation attempt owns its barrier"),
    ) {
        Ok(process_id) => process_id,
        Err(_) => {
            cleanup_definition_attempt(runtime, attempt, false)?;
            return Err(DefinitionServiceError::Activation);
        }
    };
    attempt.process_id = Some(process_id);
    attempt.process_group_id = Some(process_id);
    if ipc::job_assign(job_management, process_id).ok() != Some(process_id) {
        cleanup_definition_attempt(runtime, attempt, false)?;
        return Err(DefinitionServiceError::Activation);
    }
    attempt.job_assigned = true;
    if !matches!(
        ipc::info(job),
        Ok(info)
            if info.kind == ObjectKind::Job
                && info.rights == Rights::SIGNAL | Rights::WAIT
                && info.size == 1
    ) || !attempt.close_job_management()
    {
        cleanup_definition_attempt(runtime, attempt, false)?;
        return Err(DefinitionServiceError::Activation);
    }
    let bootstrap_granted = attempt.bootstrap_receiver_source.is_some_and(|endpoint| {
        ipc::grant_child(
            process_id,
            endpoint,
            Rights::RECEIVE,
            PROCESS_START_BOOTSTRAP_HANDLE,
        )
        .ok()
            == Some(PROCESS_START_BOOTSTRAP_HANDLE)
    });
    let startup_sent = bootstrap_granted
        && send_definition_process_start(&mut attempt, runtime, process_id, generation);
    if !startup_sent || !attempt.release_child() {
        cleanup_definition_attempt(runtime, attempt, false)?;
        return Err(DefinitionServiceError::Activation);
    }

    if runtime.definition.readiness() == Readiness::Immediate {
        runtime.process_id = attempt.process_id.take();
        runtime.process_group_id = attempt.process_group_id.take();
        runtime.job = attempt.job.take();
        runtime.cleanup_budget_reported = false;
        runtime.generation = Some(generation);
        runtime.readiness_endpoint = None;
        runtime.restart_deferred = false;
        let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_READY);
        return Ok(DefinitionStartOutcome::Ready);
    }

    let readiness_endpoint = attempt
        .readiness_endpoint
        .expect("notify readiness created an endpoint");
    let ready_message = runtime
        .definition
        .ready_message()
        .expect("notify readiness has a message")
        .as_bytes();
    let mut readiness_yields_remaining = DEFINITION_SERVICE_READINESS_GRACE_YIELDS;
    let mut ready_buffer = [0_u8; userspace::abi::limits::MAX_IPC_MESSAGE_BYTES];
    loop {
        match ipc::try_receive(readiness_endpoint, &mut ready_buffer) {
            Ok(message) => {
                let has_capability = message.capability.is_some();
                if let Some(capability) = message.capability {
                    let _ = ipc::close(capability.handle);
                }
                if message.sender_process_id != process_id
                    || has_capability
                    || message.bytes != ready_message.len()
                    || &ready_buffer[..message.bytes] != ready_message
                {
                    let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_PROTOCOL_FAILED);
                    cleanup_definition_attempt(runtime, attempt, false)?;
                    return Ok(DefinitionStartOutcome::Exited { successful: false });
                }
                runtime.process_id = attempt.process_id.take();
                runtime.process_group_id = attempt.process_group_id.take();
                runtime.job = attempt.job.take();
                runtime.cleanup_budget_reported = false;
                runtime.generation = Some(generation);
                runtime.readiness_endpoint = attempt.readiness_endpoint.take();
                runtime.restart_deferred = false;
                let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_READY);
                return Ok(DefinitionStartOutcome::Ready);
            }
            Err(error) if error == ipc::Error::TRY_AGAIN => {
                if readiness_yields_remaining == 0 {
                    cleanup_definition_attempt(runtime, attempt, false)?;
                    return Ok(DefinitionStartOutcome::Exited { successful: false });
                }
                readiness_yields_remaining -= 1;
            }
            Err(_) => {
                let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_PROTOCOL_FAILED);
                cleanup_definition_attempt(runtime, attempt, false)?;
                return Ok(DefinitionStartOutcome::Exited { successful: false });
            }
        }
        match syscall::try_wait_child(process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) => {
                cleanup_definition_attempt(runtime, attempt, true)?;
                return Ok(DefinitionStartOutcome::Exited {
                    successful: status.success(),
                });
            }
            Err(error) if error == syscall::Errno::TRY_AGAIN => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => {
                cleanup_definition_attempt(runtime, attempt, false)?;
                return Err(DefinitionServiceError::Activation);
            }
        }
        if syscall::yield_now().is_err() {
            cleanup_definition_attempt(runtime, attempt, false)?;
            return Err(DefinitionServiceError::Activation);
        }
    }
}

fn cleanup_definition_service_runtime(runtime: &mut DefinitionServiceRuntime<'_>) -> bool {
    let attempt_clean = match runtime.cleanup_attempt.take() {
        Some(mut attempt) => {
            if attempt.abort() {
                true
            } else {
                runtime.cleanup_attempt = Some(attempt);
                false
            }
        }
        None => true,
    };
    let process_clean = clear_definition_service_process(runtime, false);
    attempt_clean && process_clean
}

fn converge_definition_service_start(
    runtime: &mut DefinitionServiceRuntime<'_>,
    generations: &mut ProviderGenerationSequence,
) -> Result<bool, DefinitionServiceError> {
    let DefinitionStartOutcome::Exited { successful } =
        start_definition_service(runtime, generations)?
    else {
        return Ok(true);
    };
    if !definition_service_selects_restart(runtime, successful)
        || !definition_service_consume_restart(runtime)
    {
        return Ok(false);
    }
    let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_RESTARTING);
    backoff(runtime.definition.restart_backoff_yields());
    match start_definition_service(runtime, generations) {
        Ok(DefinitionStartOutcome::Ready) => Ok(true),
        Ok(DefinitionStartOutcome::Exited { successful }) => {
            runtime.restart_deferred = definition_service_selects_restart(runtime, successful)
                && runtime.restart_count < runtime.definition.restart_limit();
            Ok(false)
        }
        Err(_) => {
            runtime.restart_deferred = runtime.restart_count < runtime.definition.restart_limit();
            Ok(false)
        }
    }
}

fn poll_definition_service(
    runtime: &mut DefinitionServiceRuntime<'_>,
    generations: &mut ProviderGenerationSequence,
    dependencies_ready: bool,
) {
    if let Some(mut attempt) = runtime.cleanup_attempt.take()
        && !attempt.abort()
    {
        runtime.cleanup_attempt = Some(attempt);
        return;
    }

    if let Some(process_id) = runtime.process_id {
        match syscall::try_wait_child(process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) => {
                runtime.restart_deferred =
                    definition_service_selects_restart(runtime, status.success());
                runtime.process_id = None;
                if !clear_definition_service_process(runtime, true) {
                    return;
                }
            }
            Err(error) if error == syscall::Errno::TRY_AGAIN => return,
            Err(error) if error == syscall::Errno::INTERRUPTED => return,
            Err(error) if error == syscall::Errno::NO_CHILD => {
                runtime.restart_deferred = definition_service_selects_restart(runtime, false);
                runtime.process_id = None;
                if !clear_definition_service_process(runtime, true) {
                    return;
                }
            }
            Err(_) => {
                let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_FAILED);
                return;
            }
        }
    } else if (runtime.process_group_id.is_some()
        || runtime.job.is_some()
        || runtime.readiness_endpoint.is_some()
        || runtime.generation.is_some())
        && !clear_definition_service_process(runtime, true)
    {
        return;
    }

    if !runtime.restart_deferred || !dependencies_ready {
        return;
    }
    if !definition_service_consume_restart(runtime) {
        runtime.restart_deferred = false;
        let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_FAILED);
        return;
    }
    let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_RESTARTING);
    backoff(runtime.definition.restart_backoff_yields());
    match start_definition_service(runtime, generations) {
        Ok(DefinitionStartOutcome::Ready) => runtime.restart_deferred = false,
        Ok(DefinitionStartOutcome::Exited { successful }) => {
            runtime.restart_deferred = definition_service_selects_restart(runtime, successful)
                && runtime.restart_count < runtime.definition.restart_limit();
            if !runtime.restart_deferred {
                let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_FAILED);
            }
        }
        Err(_) => {
            runtime.restart_deferred = runtime.restart_count < runtime.definition.restart_limit();
            if !runtime.restart_deferred {
                let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_FAILED);
            }
        }
    }
}

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    if syscall::getpid() != Ok(INIT_PROCESS_ID) {
        fail(WRONG_PROCESS_ID);
    }
    if syscall::write_all(STDOUT, INIT_READY).is_err() {
        syscall::exit(1);
    }
    let boot_mode = init_boot_mode();
    let nullfs_restart_test = boot_mode == InitBootMode::NullfsRestartTest;
    let nullfs_out_of_space_test = boot_mode == InitBootMode::NullfsOutOfSpaceTest;
    let nullfs_block_device_loss_test = boot_mode == InitBootMode::NullfsBlockDeviceLossTest;
    let nullfs_crash_recovery_test = boot_mode == InitBootMode::NullfsCrashRecoveryTest;
    let nullfs_boot_generation_test = boot_mode == InitBootMode::NullfsBootGenerationTest;
    let logging_lifecycle_test = boot_mode == InitBootMode::LoggingLifecycleTest;
    if boot_mode == InitBootMode::NullfsUnavailableTest {
        match platform::open_writable_nullfs_block_device_endpoint(
            &nullfs_primary_volume::FILESYSTEM_UUID,
        ) {
            Err(platform::Errno::NO_ENTRY) => {
                if syscall::write_all(STDOUT, NULLFS_UNAVAILABLE_RECOVERY_HANDOFF).is_err() {
                    syscall::exit(1);
                }
                syscall::exit(78);
            }
            Ok(endpoint) => {
                let _ = ipc::close(endpoint);
                fail(NULLFS_UNAVAILABLE_TEST_FAILED);
            }
            Err(_) => fail(NULLFS_UNAVAILABLE_TEST_FAILED),
        }
    }

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

    if nullfs_boot_generation_test {
        let boot_generation_endpoint = platform::open_writable_nullfs_block_device_endpoint(
            &nullfs_primary_volume::FILESYSTEM_UUID,
        )
        .unwrap_or_else(|_| fail(BLOCK_DEVICE_BOOTSTRAP_FAILED));
        if !matches!(
            ipc::info(boot_generation_endpoint),
            Ok(info)
                if info.kind == ipc::ObjectKind::Endpoint
                    && info.rights == (Rights::SEND | Rights::TRANSFER)
        ) {
            fail(BLOCK_DEVICE_BOOTSTRAP_FAILED);
        }
        run_probe(
            NULLFS_BOOT_GENERATION_PROBE_COMMAND,
            boot_generation_endpoint,
            NULLFS_BOOT_GENERATION_PROBE_FAILED,
            NULLFS_BOOT_GENERATION_PROBE_PASSED,
        );
        ipc::close(boot_generation_endpoint)
            .unwrap_or_else(|_| fail(BLOCK_DEVICE_BOOTSTRAP_FAILED));
    }

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
    let mut nullfs_readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_SERVICE_BOOTSTRAP_FAILED));
    let mut nullfs_request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_SERVICE_BOOTSTRAP_FAILED));
    let nullfs_crash_hook_endpoint = if nullfs_crash_recovery_test {
        Some(ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_SERVICE_BOOTSTRAP_FAILED)))
    } else {
        None
    };
    let nullfs_spec = if nullfs_crash_recovery_test {
        NULLFS_CRASH_CONTAINMENT_TEST_SERVICE
    } else if nullfs_restart_test || nullfs_block_device_loss_test {
        NULLFS_CONTAINMENT_TEST_SERVICE
    } else {
        NULLFS_SERVICE
    };
    let mut nullfs_service = ServiceRuntime::new(nullfs_spec);
    let mut nullfs_generations = ProviderGenerationSequence::new();
    let nullfs_block_capability = BootstrapCapability {
        source_handle: nullfs_service_block_endpoint,
        rights: Rights::SEND,
        target_handle: NULLFS_BLOCK_HANDLE,
    };
    let nullfs_crash_capability = BootstrapCapability {
        source_handle: nullfs_crash_hook_endpoint.unwrap_or(0),
        rights: Rights::RECEIVE,
        target_handle: NULLFS_CRASH_TEST_HANDLE,
    };
    let nullfs_capabilities = [nullfs_block_capability, nullfs_crash_capability];
    let nullfs_capabilities = if nullfs_crash_recovery_test {
        &nullfs_capabilities[..]
    } else {
        &nullfs_capabilities[..1]
    };
    let (mut nullfs_generation, mut nullfs_job) = start_contained_service(
        &mut nullfs_service,
        &mut nullfs_generations,
        &mut nullfs_readiness_endpoint,
        &mut nullfs_request_endpoint,
        nullfs_capabilities,
        &NULLFS_MESSAGES,
        NULLFS_CONTAINMENT,
    );
    run_probe(
        NULLFS_READINESS_PROBE_COMMAND,
        nullfs_request_endpoint,
        NULLFS_READINESS_PROBE_FAILED,
        NULLFS_READINESS_PROBE_PASSED,
    );
    register_nullfs_proxy(nullfs_generation, nullfs_request_endpoint);

    let mut readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
    let mut request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
    let tmpfs_spec = if logging_lifecycle_test {
        TMPFS_CONTAINMENT_TEST_SERVICE
    } else {
        TMPFS_SERVICE
    };
    let mut service = ServiceRuntime::new(tmpfs_spec);
    let mut tmpfs_generations = ProviderGenerationSequence::new();
    let (mut tmpfs_generation, mut tmpfs_job) = start_contained_service(
        &mut service,
        &mut tmpfs_generations,
        &mut readiness_endpoint,
        &mut request_endpoint,
        &[],
        &TMPFS_MESSAGES,
        TMPFS_CONTAINMENT,
    );
    register_tmpfs_proxy(tmpfs_generation, request_endpoint);
    run_tmpfs_probe(request_endpoint);
    let mut vfs_readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
    let mut vfs_request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
    let vfs_spec = if logging_lifecycle_test {
        VFS_CONTAINMENT_TEST_SERVICE
    } else {
        VFS_SERVICE
    };
    let mut vfs_service = ServiceRuntime::new(vfs_spec);
    let mut vfs_generations = ProviderGenerationSequence::new();
    let (mut vfs_generation, mut vfs_job) = start_contained_service(
        &mut vfs_service,
        &mut vfs_generations,
        &mut vfs_readiness_endpoint,
        &mut vfs_request_endpoint,
        &[],
        &VFS_MESSAGES,
        VFS_CONTAINMENT,
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
            NullfsGenerationResources {
                readiness_endpoint: &mut nullfs_readiness_endpoint,
                request_endpoint: &mut nullfs_request_endpoint,
                job: &mut nullfs_job,
            },
            vfs_request_endpoint,
            nullfs_block_capability,
        );
        let _ = syscall::write_all(STDOUT, NULLFS_RESTART_PROBE_PASSED);
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
    } else if nullfs_block_device_loss_test {
        run_nullfs_block_device_loss_probe(
            &nullfs_service,
            nullfs_generation,
            &mut nullfs_job,
            vfs_request_endpoint,
            nullfs_service_block_endpoint,
        );
    } else if nullfs_crash_recovery_test {
        nullfs_generation = run_nullfs_crash_recovery_probe(
            &mut nullfs_service,
            nullfs_generation,
            &mut nullfs_generations,
            NullfsGenerationResources {
                readiness_endpoint: &mut nullfs_readiness_endpoint,
                request_endpoint: &mut nullfs_request_endpoint,
                job: &mut nullfs_job,
            },
            nullfs_capabilities,
            nullfs_crash_hook_endpoint.unwrap_or_else(|| fail(VFS_CRASH_RECOVERY_PROBE_FAILED)),
        );
    } else if nullfs_out_of_space_test {
        run_probe(
            VFS_OUT_OF_SPACE_PROBE_COMMAND,
            vfs_request_endpoint,
            VFS_OUT_OF_SPACE_PROBE_FAILED,
            VFS_OUT_OF_SPACE_PROBE_PASSED,
        );
    } else {
        run_probe(
            VFS_READINESS_PROBE_COMMAND,
            vfs_request_endpoint,
            VFS_READINESS_PROBE_FAILED,
            VFS_READINESS_PROBE_PASSED,
        );
    }

    let mut definition_bytes = [0_u8; MAX_DEFINITION_BYTES];
    let mut definition_service_generations = ProviderGenerationSequence::new();
    let mut definition_service = match load_definition_service(&mut definition_bytes) {
        Ok(definition) => {
            let mut runtime = DefinitionServiceRuntime::new(definition);
            match converge_definition_service_start(
                &mut runtime,
                &mut definition_service_generations,
            ) {
                Ok(true)
                    if runtime.restart_count == 1
                        && runtime.generation.map(ProviderGeneration::get) == Some(2) =>
                {
                    let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_VERIFIED);
                    Some(runtime)
                }
                Ok(false) if runtime.restart_deferred => Some(runtime),
                Ok(_) | Err(_) => {
                    runtime.restart_deferred = false;
                    let cleaned = cleanup_definition_service_runtime(&mut runtime);
                    let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_FAILED);
                    if cleaned { None } else { Some(runtime) }
                }
            }
        }
        Err(_) => {
            let _ = syscall::write_all(STDOUT, DEFINITION_SERVICE_FAILED);
            None
        }
    };
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
        let mutation_count = service_control.pump_mutation(
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
            nullfs_request_endpoint,
        );
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

        if let Some(status) = service_control.nullfs_lifecycle.advance(
            nullfs_service.process_id(),
            nullfs_generation,
            nullfs_readiness_endpoint,
            nullfs_request_endpoint,
            &mut nullfs_job,
        ) {
            if !nullfs_job.drain() {
                fail(NULLFS_SERVICE_FAILED);
            }
            match nullfs_service.observe_status(status) {
                ServiceStatusDisposition::WaitForNextEvent => {}
                ServiceStatusDisposition::Restart { backoff_yields } => {
                    let _ = syscall::write_all(STDOUT, NULLFS_SERVICE_RESTARTING);
                    offline_filesystem_provider(
                        platform::FilesystemProvider::Nullfs,
                        nullfs_generation,
                        NULLFS_SERVICE_FAILED,
                    );
                    ipc::close(nullfs_readiness_endpoint)
                        .unwrap_or_else(|_| fail(NULLFS_SERVICE_FAILED));
                    ipc::close(nullfs_request_endpoint)
                        .unwrap_or_else(|_| fail(NULLFS_SERVICE_FAILED));
                    nullfs_readiness_endpoint = ipc::endpoint_create()
                        .unwrap_or_else(|_| fail(NULLFS_SERVICE_BOOTSTRAP_FAILED));
                    nullfs_request_endpoint = ipc::endpoint_create()
                        .unwrap_or_else(|_| fail(NULLFS_SERVICE_BOOTSTRAP_FAILED));
                    backoff(backoff_yields);
                    (nullfs_generation, nullfs_job) = start_contained_service(
                        &mut nullfs_service,
                        &mut nullfs_generations,
                        &mut nullfs_readiness_endpoint,
                        &mut nullfs_request_endpoint,
                        nullfs_capabilities,
                        &NULLFS_MESSAGES,
                        NULLFS_CONTAINMENT,
                    );
                    register_nullfs_proxy(nullfs_generation, nullfs_request_endpoint);
                    service_control.drain_mutation(
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
                        nullfs_request_endpoint,
                    );
                    nullfs_service.complete_restart();
                }
                ServiceStatusDisposition::Stopped => {
                    offline_filesystem_provider(
                        platform::FilesystemProvider::Nullfs,
                        nullfs_generation,
                        NULLFS_SERVICE_FAILED,
                    );
                    ipc::close(nullfs_readiness_endpoint)
                        .unwrap_or_else(|_| fail(NULLFS_SERVICE_FAILED));
                    ipc::close(nullfs_request_endpoint)
                        .unwrap_or_else(|_| fail(NULLFS_SERVICE_FAILED));
                }
                ServiceStatusDisposition::Failed => {
                    offline_filesystem_provider(
                        platform::FilesystemProvider::Nullfs,
                        nullfs_generation,
                        NULLFS_SERVICE_FAILED,
                    );
                    let _ = ipc::close(nullfs_readiness_endpoint);
                    let _ = ipc::close(nullfs_request_endpoint);
                    fail(NULLFS_SERVICE_FAILED)
                }
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
                        if !tmpfs_job.drain() {
                            fail(SERVICE_FAILED);
                        }
                        close_contained_endpoint(
                            TMPFS_CONTAINMENT,
                            readiness_endpoint,
                            SERVICE_FAILED,
                        );
                        close_contained_endpoint(
                            TMPFS_CONTAINMENT,
                            request_endpoint,
                            SERVICE_FAILED,
                        );
                        readiness_endpoint = ipc::endpoint_create()
                            .unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
                        request_endpoint = ipc::endpoint_create()
                            .unwrap_or_else(|_| fail(SERVICE_BOOTSTRAP_FAILED));
                        backoff(backoff_yields);
                        (tmpfs_generation, tmpfs_job) = start_contained_service(
                            &mut service,
                            &mut tmpfs_generations,
                            &mut readiness_endpoint,
                            &mut request_endpoint,
                            &[],
                            &TMPFS_MESSAGES,
                            TMPFS_CONTAINMENT,
                        );
                        register_tmpfs_proxy(tmpfs_generation, request_endpoint);
                        service_control.drain_mutation(
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
                            nullfs_request_endpoint,
                        );
                        service.complete_restart();
                    }
                    ServiceStatusDisposition::Stopped => {
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Tmpfs,
                            tmpfs_generation,
                            SERVICE_FAILED,
                        );
                        if !tmpfs_job.drain() {
                            fail(SERVICE_FAILED);
                        }
                        close_contained_endpoint(
                            TMPFS_CONTAINMENT,
                            readiness_endpoint,
                            SERVICE_FAILED,
                        );
                        close_contained_endpoint(
                            TMPFS_CONTAINMENT,
                            request_endpoint,
                            SERVICE_FAILED,
                        );
                    }
                    ServiceStatusDisposition::Failed => {
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Tmpfs,
                            tmpfs_generation,
                            SERVICE_FAILED,
                        );
                        if !tmpfs_job.drain() {
                            fail(SERVICE_FAILED);
                        }
                        close_contained_endpoint(
                            TMPFS_CONTAINMENT,
                            readiness_endpoint,
                            SERVICE_FAILED,
                        );
                        close_contained_endpoint(
                            TMPFS_CONTAINMENT,
                            request_endpoint,
                            SERVICE_FAILED,
                        );
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
                        if !vfs_job.drain() {
                            fail(VFS_SERVICE_FAILED);
                        }
                        close_contained_endpoint(
                            VFS_CONTAINMENT,
                            vfs_readiness_endpoint,
                            VFS_SERVICE_FAILED,
                        );
                        close_contained_endpoint(
                            VFS_CONTAINMENT,
                            vfs_request_endpoint,
                            VFS_SERVICE_FAILED,
                        );
                        vfs_readiness_endpoint = ipc::endpoint_create()
                            .unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
                        vfs_request_endpoint = ipc::endpoint_create()
                            .unwrap_or_else(|_| fail(VFS_SERVICE_BOOTSTRAP_FAILED));
                        backoff(backoff_yields);
                        (vfs_generation, vfs_job) = start_contained_service(
                            &mut vfs_service,
                            &mut vfs_generations,
                            &mut vfs_readiness_endpoint,
                            &mut vfs_request_endpoint,
                            &[],
                            &VFS_MESSAGES,
                            VFS_CONTAINMENT,
                        );
                        register_vfs_router(vfs_generation, vfs_request_endpoint);
                        service_control.drain_mutation(
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
                            nullfs_request_endpoint,
                        );
                        vfs_service.complete_restart();
                    }
                    ServiceStatusDisposition::Stopped => {
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Vfs,
                            vfs_generation,
                            VFS_SERVICE_FAILED,
                        );
                        if !vfs_job.drain() {
                            fail(VFS_SERVICE_FAILED);
                        }
                        close_contained_endpoint(
                            VFS_CONTAINMENT,
                            vfs_readiness_endpoint,
                            VFS_SERVICE_FAILED,
                        );
                        close_contained_endpoint(
                            VFS_CONTAINMENT,
                            vfs_request_endpoint,
                            VFS_SERVICE_FAILED,
                        );
                    }
                    ServiceStatusDisposition::Failed => {
                        offline_filesystem_provider(
                            platform::FilesystemProvider::Vfs,
                            vfs_generation,
                            VFS_SERVICE_FAILED,
                        );
                        if !vfs_job.drain() {
                            fail(VFS_SERVICE_FAILED);
                        }
                        close_contained_endpoint(
                            VFS_CONTAINMENT,
                            vfs_readiness_endpoint,
                            VFS_SERVICE_FAILED,
                        );
                        close_contained_endpoint(
                            VFS_CONTAINMENT,
                            vfs_request_endpoint,
                            VFS_SERVICE_FAILED,
                        );
                        fail(VFS_SERVICE_FAILED)
                    }
                },
                Err(error) if error == syscall::Errno::TRY_AGAIN => {}
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(_) => fail(VFS_SERVICE_FAILED),
            }
        }

        let definition_dependencies_ready = nullfs_service.state() == ServiceState::Running
            && !nullfs_service.controlled_restart_pending()
            && vfs_service.state() == ServiceState::Running
            && !vfs_service.controlled_restart_pending();
        if let Some(runtime) = definition_service.as_mut() {
            poll_definition_service(
                runtime,
                &mut definition_service_generations,
                definition_dependencies_ready,
            );
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
    NullfsOutOfSpaceTest,
    NullfsBlockDeviceLossTest,
    NullfsCrashRecoveryTest,
    NullfsBootGenerationTest,
    NullfsUnavailableTest,
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
        b"nullfs-out-of-space-test" | NULLFS_OUT_OF_SPACE_TEST_BOOT_MODE => {
            InitBootMode::NullfsOutOfSpaceTest
        }
        b"nullfs-block-device-loss-test" | NULLFS_BLOCK_DEVICE_LOSS_TEST_BOOT_MODE => {
            InitBootMode::NullfsBlockDeviceLossTest
        }
        b"nullfs-crash-recovery-test" | NULLFS_CRASH_RECOVERY_TEST_BOOT_MODE => {
            InitBootMode::NullfsCrashRecoveryTest
        }
        b"nullfs-boot-generation-test" | NULLFS_BOOT_GENERATION_TEST_BOOT_MODE => {
            InitBootMode::NullfsBootGenerationTest
        }
        b"nullfs-unavailable-test" | NULLFS_UNAVAILABLE_TEST_BOOT_MODE => {
            InitBootMode::NullfsUnavailableTest
        }
        b"logging-lifecycle-test" | LOGGING_LIFECYCLE_TEST_BOOT_MODE => {
            InitBootMode::LoggingLifecycleTest
        }
        _ => fail(BOOT_MODE_PROBE_FAILED),
    }
}

fn run_nullfs_crash_recovery_probe(
    nullfs_service: &mut ServiceRuntime,
    old_generation: ProviderGeneration,
    generations: &mut ProviderGenerationSequence,
    resources: NullfsGenerationResources<'_>,
    bootstrap_capabilities: &[BootstrapCapability],
    crash_hook_endpoint: CapabilityHandle,
) -> ProviderGeneration {
    const NONCE: u64 = 0x4352_4153_4800_0001;

    let old_process_id = nullfs_service
        .process_id()
        .unwrap_or_else(|| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    let old_restart_count = nullfs_service.restart_count();
    let old_endpoint_info = ipc::info(*resources.request_endpoint)
        .unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));

    let ready_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    let control_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    let barrier =
        syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    let probe_process_id = syscall::spawn_command_with_barrier(
        VFS_CRASH_RECOVERY_PROBE_COMMAND,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &barrier,
    )
    .unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
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
        fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
    }
    barrier
        .release()
        .unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    wait_for_probe_message(
        ready_endpoint,
        probe_process_id,
        VFS_CRASH_RECOVERY_READY,
        VFS_CRASH_RECOVERY_PROBE_FAILED,
    );

    let arm = crash_test::Message::new(crash_test::kind::ARM, old_generation.get(), NONCE, 0)
        .unwrap_or_else(|| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    ipc::send(crash_hook_endpoint, &arm.encode(), None)
        .unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    let _ = syscall::write_all(STDOUT, VFS_CRASH_RECOVERY_INJECTED);
    ipc::send(control_endpoint, VFS_CRASH_RECOVERY_GO, None)
        .unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));

    let mut reached_bytes = [0_u8; crash_test::MESSAGE_BYTES];
    let reached = loop {
        match ipc::try_receive(*resources.readiness_endpoint, &mut reached_bytes) {
            Ok(message) => {
                if message.sender_process_id != old_process_id
                    || message.capability.is_some()
                    || message.bytes != crash_test::MESSAGE_BYTES
                {
                    fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
                }
                break crash_test::Message::decode(&reached_bytes)
                    .unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
            }
            Err(error) if error == ipc::Error::TRY_AGAIN => {}
            Err(_) => fail(VFS_CRASH_RECOVERY_PROBE_FAILED),
        }
        if try_wait_final_status(probe_process_id).is_some() {
            fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
        }
        syscall::yield_now().unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    };
    if reached.kind != crash_test::kind::MUTATION_REACHED
        || reached.generation != old_generation.get()
        || reached.nonce != NONCE
        || reached.request_id == 0
    {
        fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
    }

    let service_status = loop {
        if let Some(status) = try_wait_final_status(old_process_id) {
            break status;
        }
        if try_wait_final_status(probe_process_id).is_some() {
            fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
        }
        syscall::yield_now().unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    };
    if service_status != 37 {
        fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
    }
    if !resources.job.drain() {
        fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
    }
    let backoff_yields = match nullfs_service.observe_status(service_status) {
        ServiceStatusDisposition::Restart { backoff_yields } => backoff_yields,
        _ => fail(VFS_CRASH_RECOVERY_PROBE_FAILED),
    };
    if nullfs_service.restart_count() != old_restart_count.saturating_add(1)
        || nullfs_service.process_id().is_some()
    {
        fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
    }

    let wrong_generation = u32::try_from(old_generation.get().saturating_add(1))
        .unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    if platform::offline_filesystem_provider(platform::FilesystemProvider::Nullfs, wrong_generation)
        .err()
        != Some(platform::Errno::INVALID_ARGUMENT)
    {
        fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
    }
    offline_filesystem_provider(
        platform::FilesystemProvider::Nullfs,
        old_generation,
        VFS_CRASH_RECOVERY_PROBE_FAILED,
    );
    wait_for_probe_message(
        ready_endpoint,
        probe_process_id,
        VFS_CRASH_RECOVERY_MUTATION_FAILED,
        VFS_CRASH_RECOVERY_PROBE_FAILED,
    );

    ipc::close(*resources.readiness_endpoint)
        .unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    ipc::close(*resources.request_endpoint)
        .unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    *resources.readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    let mut replacement_request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    if ipc::info(replacement_request_endpoint)
        .map(|info| info.object_id <= old_endpoint_info.object_id)
        .unwrap_or(true)
    {
        fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
    }
    backoff(backoff_yields);
    let (replacement_generation, replacement_job) = start_contained_service(
        nullfs_service,
        generations,
        resources.readiness_endpoint,
        &mut replacement_request_endpoint,
        bootstrap_capabilities,
        &NULLFS_MESSAGES,
        NULLFS_CONTAINMENT,
    );
    if replacement_generation.get() != old_generation.get().saturating_add(1) {
        fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
    }
    register_nullfs_proxy(replacement_generation, replacement_request_endpoint);
    *resources.request_endpoint = replacement_request_endpoint;
    *resources.job = replacement_job;
    nullfs_service.complete_restart();
    if nullfs_service.state() != ServiceState::Running
        || nullfs_service.controlled_restart_pending()
        || nullfs_service.restart_count() != old_restart_count.saturating_add(1)
    {
        fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
    }

    ipc::send(control_endpoint, VFS_CRASH_RECOVERY_REPLACEMENT, None)
        .unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    let probe_status = loop {
        if let Some(status) = try_wait_final_status(probe_process_id) {
            break status;
        }
        syscall::yield_now().unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    };
    if probe_status != 0 {
        fail(VFS_CRASH_RECOVERY_PROBE_FAILED);
    }
    ipc::close(ready_endpoint).unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    ipc::close(control_endpoint).unwrap_or_else(|_| fail(VFS_CRASH_RECOVERY_PROBE_FAILED));
    let _ = syscall::write_all(STDOUT, VFS_CRASH_RECOVERY_PROBE_PASSED);
    replacement_generation
}

fn run_nullfs_block_device_loss_probe(
    nullfs_service: &ServiceRuntime,
    nullfs_generation: ProviderGeneration,
    job: &mut ContainedServiceJob,
    vfs_request_endpoint: CapabilityHandle,
    block_endpoint: CapabilityHandle,
) {
    let service_process_id = nullfs_service
        .process_id()
        .unwrap_or_else(|| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    let block_generation = ipc::info(block_endpoint)
        .map(|info| info.object_id)
        .unwrap_or_else(|_| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    if block_generation == 0 {
        fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED);
    }

    let ready_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    let control_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    let barrier =
        syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    let probe_process_id = syscall::spawn_command_with_barrier(
        VFS_BLOCK_DEVICE_LOSS_PROBE_COMMAND,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &barrier,
    )
    .unwrap_or_else(|_| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    if ipc::grant_child(probe_process_id, vfs_request_endpoint, Rights::SEND, 1).ok() != Some(1)
        || ipc::grant_child(probe_process_id, ready_endpoint, Rights::SEND, 2).ok() != Some(2)
        || ipc::grant_child(probe_process_id, control_endpoint, Rights::RECEIVE, 3).ok() != Some(3)
    {
        fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED);
    }
    barrier
        .release()
        .unwrap_or_else(|_| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    wait_for_probe_message(
        ready_endpoint,
        probe_process_id,
        VFS_BLOCK_DEVICE_LOSS_PROBE_READY,
        VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED,
    );

    let wrong_generation = block_generation
        .checked_add(1)
        .unwrap_or_else(|| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    if platform::offline_writable_nullfs_block_device_endpoint(
        &nullfs_primary_volume::FILESYSTEM_UUID,
        wrong_generation,
    )
    .err()
        != Some(platform::Errno::INVALID_ARGUMENT)
        || platform::offline_writable_nullfs_block_device_endpoint(
            &nullfs_primary_volume::FILESYSTEM_UUID,
            block_generation,
        )
        .is_err()
        || platform::offline_writable_nullfs_block_device_endpoint(
            &nullfs_primary_volume::FILESYSTEM_UUID,
            block_generation,
        )
        .err()
            != Some(platform::Errno::INVALID_ARGUMENT)
        || platform::open_writable_nullfs_block_device_endpoint(
            &nullfs_primary_volume::FILESYSTEM_UUID,
        )
        .err()
            != Some(platform::Errno::IO)
    {
        fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED);
    }
    let _ = syscall::write_all(STDOUT, VFS_BLOCK_DEVICE_LOSS_INJECTED);
    ipc::send(
        control_endpoint,
        VFS_BLOCK_DEVICE_LOSS_PROVIDER_OFFLINED,
        None,
    )
    .unwrap_or_else(|_| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    wait_for_probe_message(
        ready_endpoint,
        probe_process_id,
        VFS_BLOCK_DEVICE_LOSS_MUTATION_FAILED,
        VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED,
    );

    let mut service_status = None;
    for _ in 0..4_096 {
        if service_status.is_none() {
            service_status = try_wait_final_status(service_process_id);
        }
        if service_status.is_some() {
            break;
        }
        if try_wait_final_status(probe_process_id).is_some() {
            fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED);
        }
        syscall::yield_now().unwrap_or_else(|_| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    }
    if service_status != Some(35) {
        fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED);
    }
    if !job.drain() {
        fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED);
    }
    offline_filesystem_provider(
        platform::FilesystemProvider::Nullfs,
        nullfs_generation,
        VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED,
    );
    ipc::send(
        control_endpoint,
        VFS_BLOCK_DEVICE_LOSS_FILESYSTEM_OFFLINED,
        None,
    )
    .unwrap_or_else(|_| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));

    let mut probe_status = None;
    for _ in 0..4_096 {
        if probe_status.is_none() {
            probe_status = try_wait_final_status(probe_process_id);
        }
        if probe_status.is_some() {
            break;
        }
        syscall::yield_now().unwrap_or_else(|_| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    }
    if probe_status != Some(0) {
        fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED);
    }
    ipc::close(ready_endpoint).unwrap_or_else(|_| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    ipc::close(control_endpoint).unwrap_or_else(|_| fail(VFS_BLOCK_DEVICE_LOSS_PROBE_FAILED));
    let _ = syscall::write_all(STDOUT, VFS_BLOCK_DEVICE_LOSS_PROBE_PASSED);
}

fn wait_for_probe_message(
    endpoint: CapabilityHandle,
    process_id: ProcessId,
    expected: &[u8],
    failure_message: &[u8],
) {
    let mut bytes = [0_u8; 64];
    for _ in 0..4_096 {
        match ipc::try_receive(endpoint, &mut bytes) {
            Ok(message) => {
                if message.sender_process_id != process_id
                    || message.capability.is_some()
                    || message.bytes != expected.len()
                    || &bytes[..message.bytes] != expected
                {
                    fail(failure_message);
                }
                return;
            }
            Err(error) if error == ipc::Error::TRY_AGAIN => {}
            Err(_) => fail(failure_message),
        }
        if try_wait_final_status(process_id).is_some() {
            fail(failure_message);
        }
        syscall::yield_now().unwrap_or_else(|_| fail(failure_message));
    }
    fail(failure_message)
}

fn run_nullfs_restart_probe(
    control: &mut ServiceControlState,
    registry: ServiceRegistryMut<'_>,
    generations: &mut ProviderGenerationSequence,
    resources: NullfsGenerationResources<'_>,
    vfs_request_endpoint: CapabilityHandle,
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
        if try_wait_final_status(probe_process_id).is_some() {
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
    let old_endpoint_info = ipc::info(*resources.request_endpoint)
        .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));

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
        control.pump_mutation(
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
            *resources.request_endpoint,
        );
        if registry.nullfs.state() == ServiceState::Restarting {
            restart_requested = true;
            break;
        }
        if try_wait_final_status(restart_process_id).is_some() {
            fail(NULLFS_RESTART_PROBE_FAILED);
        }
        syscall::yield_now().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    }
    if !restart_requested {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }

    let status = loop {
        if let Some(status) = control.nullfs_lifecycle.advance(
            registry.nullfs.process_id(),
            old_generation,
            *resources.readiness_endpoint,
            *resources.request_endpoint,
            resources.job,
        ) {
            break status;
        }
        if try_wait_final_status(probe_process_id).is_some() {
            fail(NULLFS_RESTART_PROBE_FAILED);
        }
        syscall::yield_now().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    };
    if status != 0
        || !control.nullfs_lifecycle.last_exit_was_clean()
        || registry.nullfs.observe_status(status)
            != (ServiceStatusDisposition::Restart { backoff_yields: 0 })
        || registry.nullfs.restart_count() != restart_count
        || registry.nullfs.process_id().is_some()
        || old_service_process_id == 0
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    if !resources.job.drain() {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }

    run_probe(
        VFS_BOOTSTRAP_PROBE_COMMAND,
        vfs_request_endpoint,
        VFS_FULL_PROBE_FAILED,
        VFS_BOOTSTRAP_PROBE_PASSED,
    );

    let _ = syscall::write_all(STDOUT, NULLFS_SERVICE_RESTARTING);
    ipc::close(*resources.readiness_endpoint).unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    ipc::close(*resources.request_endpoint).unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    *resources.readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let mut replacement_request_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    if ipc::info(replacement_request_endpoint)
        .map(|info| info.object_id <= old_endpoint_info.object_id)
        .unwrap_or(true)
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    let (mut generation, replacement_job) = start_contained_service(
        registry.nullfs,
        generations,
        resources.readiness_endpoint,
        &mut replacement_request_endpoint,
        &[block_capability],
        &NULLFS_MESSAGES,
        NULLFS_CONTAINMENT,
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
    *resources.request_endpoint = replacement_request_endpoint;
    *resources.job = replacement_job;

    control.drain_mutation(
        ServiceRegistryMut {
            logging: &mut *registry.logging,
            logging_generation: registry.logging_generation,
            nullfs: &mut *registry.nullfs,
            nullfs_generation: generation,
            tmpfs: &mut *registry.tmpfs,
            tmpfs_generation: registry.tmpfs_generation,
            vfs: &mut *registry.vfs,
            vfs_generation: registry.vfs_generation,
        },
        *resources.request_endpoint,
    );
    registry.nullfs.complete_restart();
    if registry.nullfs.state() != ServiceState::Running
        || registry.nullfs.controlled_restart_pending()
        || registry.nullfs.restart_count() != restart_count
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    ipc::send(control_endpoint, NULLFS_RESTART_PROBE_REPLACEMENT, None)
        .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));

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

    let forced_process_id = registry
        .nullfs
        .process_id()
        .unwrap_or_else(|| fail(NULLFS_RESTART_PROBE_FAILED));
    if syscall::signal_process_group(forced_process_id, signal::STOP).ok() != Some(1) {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    loop {
        match syscall::wait_child(forced_process_id) {
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

    let forced_barrier =
        syscall::LaunchBarrier::new().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let forced_restart_process_id = syscall::spawn_command_with_barrier(
        SV_RESTART_NULLFS_COMMAND,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &forced_barrier,
    )
    .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    if ipc::grant_child(
        forced_restart_process_id,
        control.mutation_source,
        Rights::SEND,
        2,
    )
    .ok()
        != Some(2)
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    forced_barrier
        .release()
        .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let mut forced_requested = false;
    for _ in 0..256 {
        control.pump_mutation(
            ServiceRegistryMut {
                logging: &mut *registry.logging,
                logging_generation: registry.logging_generation,
                nullfs: &mut *registry.nullfs,
                nullfs_generation: generation,
                tmpfs: &mut *registry.tmpfs,
                tmpfs_generation: registry.tmpfs_generation,
                vfs: &mut *registry.vfs,
                vfs_generation: registry.vfs_generation,
            },
            *resources.request_endpoint,
        );
        if registry.nullfs.state() == ServiceState::Restarting {
            forced_requested = true;
            break;
        }
        syscall::yield_now().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    }
    if !forced_requested {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    control.nullfs_lifecycle.shorten_quiesce_grace_for_test();

    let forced_status = loop {
        if let Some(status) = control.nullfs_lifecycle.advance(
            registry.nullfs.process_id(),
            generation,
            *resources.readiness_endpoint,
            *resources.request_endpoint,
            resources.job,
        ) {
            break status;
        }
        syscall::yield_now().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    };
    if forced_status == 0
        || control.nullfs_lifecycle.last_exit_was_clean()
        || registry.nullfs.observe_status(forced_status)
            != (ServiceStatusDisposition::Restart { backoff_yields: 0 })
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    if !resources.job.drain() {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }

    let forced_old_generation = generation;
    let forced_endpoint_info = ipc::info(*resources.request_endpoint)
        .unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    ipc::close(*resources.readiness_endpoint).unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    ipc::close(*resources.request_endpoint).unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    *resources.readiness_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    let mut forced_replacement_endpoint =
        ipc::endpoint_create().unwrap_or_else(|_| fail(NULLFS_RESTART_PROBE_FAILED));
    if ipc::info(forced_replacement_endpoint)
        .map(|info| info.object_id <= forced_endpoint_info.object_id)
        .unwrap_or(true)
    {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    let (replacement_generation, replacement_job) = start_contained_service(
        registry.nullfs,
        generations,
        resources.readiness_endpoint,
        &mut forced_replacement_endpoint,
        &[block_capability],
        &NULLFS_MESSAGES,
        NULLFS_CONTAINMENT,
    );
    generation = replacement_generation;
    if generation.get() != forced_old_generation.get().saturating_add(1) {
        fail(NULLFS_RESTART_PROBE_FAILED);
    }
    register_nullfs_proxy(generation, forced_replacement_endpoint);
    *resources.request_endpoint = forced_replacement_endpoint;
    *resources.job = replacement_job;
    control.drain_mutation(
        ServiceRegistryMut {
            logging: &mut *registry.logging,
            logging_generation: registry.logging_generation,
            nullfs: &mut *registry.nullfs,
            nullfs_generation: generation,
            tmpfs: &mut *registry.tmpfs,
            tmpfs_generation: registry.tmpfs_generation,
            vfs: &mut *registry.vfs,
            vfs_generation: registry.vfs_generation,
        },
        *resources.request_endpoint,
    );
    registry.nullfs.complete_restart();
    run_probe(
        NULLFS_READINESS_PROBE_COMMAND,
        *resources.request_endpoint,
        NULLFS_RESTART_PROBE_FAILED,
        b"userspace init: NullFS dirty recovery restart passed\n",
    );
    loop {
        match syscall::wait_child(forced_restart_process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Ok(status) if status.success() => break,
            Ok(_) => fail(NULLFS_RESTART_PROBE_FAILED),
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => fail(NULLFS_RESTART_PROBE_FAILED),
        }
    }
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

fn fail_logging_activation(mut attempt: LoggingActivationAttempt, message: &[u8]) -> ! {
    if !attempt.abort() {
        fail(LOGGING_SERVICE_FAILED);
    }
    fail(message)
}

fn launch_logging_generation(
    service: &mut ServiceRuntime,
    generations: &mut ProviderGenerationSequence,
    early_log_reader: CapabilityHandle,
    lifecycle_test: bool,
) -> (ProcessId, LoggingGeneration) {
    let generation = generations
        .next_generation()
        .unwrap_or_else(|_| fail(LOGGING_SERVICE_BOOTSTRAP_FAILED));
    let suppress_readiness = lifecycle_test && generation.get() >= 4;
    let command = if suppress_readiness {
        LOGGING_SERVICE_SUPPRESS_READINESS_COMMAND
    } else if lifecycle_test {
        LOGGING_SERVICE_IGNORE_TERMINATE_COMMAND
    } else {
        service.spec().command
    };
    let readiness_yields_remaining = if suppress_readiness {
        LOGGING_TEST_READINESS_GRACE_YIELDS
    } else {
        LOGGING_READINESS_GRACE_YIELDS
    };
    let mut attempt = LoggingActivationAttempt::new();
    let job_management = match ipc::job_create() {
        Ok(handle) => handle,
        Err(_) => fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED),
    };
    attempt.job_management = Some(job_management);
    let job = match ipc::duplicate(job_management, Rights::SIGNAL | Rights::WAIT) {
        Ok(handle) => handle,
        Err(_) => fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED),
    };
    attempt.job = Some(job);
    let job_handles_valid = matches!(
        ipc::info(job_management),
        Ok(info) if info.kind == ObjectKind::Job && info.rights == Rights::JOB && info.size == 0
    ) && matches!(
        ipc::info(job),
        Ok(info)
            if info.kind == ObjectKind::Job
                && info.rights == Rights::SIGNAL | Rights::WAIT
                && info.size == 0
    ) && ipc::job_try_wait(job).err() == Some(ipc::Error::NO_CHILD);
    if !job_handles_valid {
        fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED);
    }

    let generation_handoff_source = match ipc::endpoint_create() {
        Ok(handle) => handle,
        Err(_) => fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED),
    };
    attempt.generation_handoff_source = Some(generation_handoff_source);
    if queue_service_generation(generation_handoff_source, generation).is_err() {
        fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED);
    }
    let readiness_source = match ipc::endpoint_create() {
        Ok(handle) => handle,
        Err(_) => fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED),
    };
    attempt.readiness_source = Some(readiness_source);
    let producer_source = match ipc::endpoint_create() {
        Ok(handle) => handle,
        Err(_) => fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED),
    };
    attempt.producer_source = Some(producer_source);
    let observer_source = match ipc::endpoint_create() {
        Ok(handle) => handle,
        Err(_) => fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED),
    };
    attempt.observer_source = Some(observer_source);
    let producer_object_id = match ipc::info(producer_source) {
        Ok(info) => info.object_id,
        Err(_) => fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED),
    };
    let observer_object_id = match ipc::info(observer_source) {
        Ok(info) => info.object_id,
        Err(_) => fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED),
    };
    if producer_object_id == 0
        || observer_object_id == 0
        || producer_object_id == observer_object_id
    {
        fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED);
    }

    let _ = syscall::write_all(STDOUT, LOGGING_MESSAGES.starting);
    let barrier = match syscall::LaunchBarrier::new() {
        Ok(barrier) => barrier,
        Err(_) => fail_logging_activation(attempt, LOGGING_SERVICE_FAILED),
    };
    attempt.barrier = Some(barrier);
    let process_id = match syscall::spawn_command_with_barrier(
        command,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        attempt
            .barrier
            .as_ref()
            .expect("logging activation attempt owns its barrier"),
    ) {
        Ok(process_id) => process_id,
        Err(_) => fail_logging_activation(attempt, LOGGING_SERVICE_FAILED),
    };
    attempt.process_id = Some(process_id);
    attempt.process_group_id = Some(process_id);
    if ipc::job_assign(job_management, process_id).ok() != Some(process_id) {
        fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED);
    }
    attempt.job_assigned = true;
    if !matches!(
        ipc::info(job),
        Ok(info)
            if info.kind == ObjectKind::Job
                && info.rights == Rights::SIGNAL | Rights::WAIT
                && info.size == 1
    ) || !LoggingActivationAttempt::close_capability(&mut attempt.job_management)
    {
        fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED);
    }

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
        || !attempt.release_child()
    {
        fail_logging_activation(attempt, LOGGING_SERVICE_BOOTSTRAP_FAILED);
    }

    service.note_spawned(process_id);
    (
        process_id,
        LoggingGeneration {
            generation,
            readiness_source: attempt
                .readiness_source
                .take()
                .expect("logging activation owns readiness source"),
            producer_source: attempt
                .producer_source
                .take()
                .expect("logging activation owns producer source"),
            observer_source: attempt
                .observer_source
                .take()
                .expect("logging activation owns observer source"),
            producer_object_id,
            observer_object_id,
            job: attempt.job.take(),
            cleanup_budget_reported: false,
            readiness_received: false,
            child_stopped: false,
            routes_published: false,
            readiness_yields_remaining,
            readiness_force_termination_sent: false,
            force_termination_attempts_remaining: LOGGING_FORCE_TERMINATION_ATTEMPTS,
        },
    )
}

fn start_logging_generation(
    service: &mut ServiceRuntime,
    route_broker: &mut RouteBrokerState,
    generations: &mut ProviderGenerationSequence,
    early_log_reader: CapabilityHandle,
) -> LoggingGeneration {
    'attempt: loop {
        let spec = service.spec();
        let (process_id, mut logging_generation) =
            launch_logging_generation(service, generations, early_log_reader, false);
        let generation = logging_generation.generation;
        let readiness_source = logging_generation.readiness_source;
        let producer_source = logging_generation.producer_source;
        let observer_source = logging_generation.observer_source;
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
                                    logging_generation.drain_job();
                                    logging_generation.close();
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
                    logging_generation.readiness_received = true;
                    logging_generation.routes_published = true;
                    logging_generation.readiness_yields_remaining = 0;
                    logging_generation.force_termination_attempts_remaining = 0;
                    return logging_generation;
                }
                Err(error) if error == ipc::Error::TRY_AGAIN => {
                    if readiness_yields_remaining != 0 {
                        readiness_yields_remaining -= 1;
                    } else if !force_termination_sent
                        && request_forced_termination(
                            logging_generation
                                .job
                                .unwrap_or_else(|| fail(LOGGING_SERVICE_PROTOCOL_FAILED)),
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
                        logging_generation.drain_job();
                        logging_generation.close();
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
    launch_logging_generation(service, generations, early_log_reader, lifecycle_test).1
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

fn request_forced_termination(job: CapabilityHandle, attempts_remaining: &mut u32) -> bool {
    if ipc::job_terminate(job).is_ok() {
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
    if service.process_id().is_none() {
        lifecycle.termination_grace_yields_remaining = None;
        lifecycle.force_termination_sent = false;
        lifecycle.force_termination_attempts_remaining = 0;
        return;
    }
    let job = lifecycle
        .current
        .as_ref()
        .and_then(|generation| generation.job)
        .unwrap_or_else(|| fail(LOGGING_SERVICE_PROTOCOL_FAILED));
    match lifecycle.termination_grace_yields_remaining {
        None => {
            lifecycle.termination_grace_yields_remaining = Some(LOGGING_TERMINATION_GRACE_YIELDS);
            lifecycle.force_termination_attempts_remaining = LOGGING_FORCE_TERMINATION_ATTEMPTS;
        }
        Some(remaining) if remaining != 0 => {
            lifecycle.termination_grace_yields_remaining = Some(remaining - 1);
        }
        Some(_) if !lifecycle.force_termination_sent => {
            if request_forced_termination(job, &mut lifecycle.force_termination_attempts_remaining)
            {
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
                        generation
                            .job
                            .unwrap_or_else(|| fail(LOGGING_SERVICE_PROTOCOL_FAILED)),
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

fn fail_contained_activation(
    mut attempt: ContainedServiceActivationAttempt,
    messages: &ServiceMessages,
    message: &[u8],
) -> ! {
    if !attempt.abort() {
        fail(messages.failed);
    }
    fail(message)
}

fn close_contained_endpoint(
    containment: ServiceContainment,
    handle: CapabilityHandle,
    failure_message: &[u8],
) {
    let mut handle = Some(handle);
    if !close_cleanup_capability(
        containment.service,
        CleanupPhase::ResourceRelease,
        &mut handle,
    ) {
        fail(failure_message);
    }
}

fn start_contained_service(
    service: &mut ServiceRuntime,
    generations: &mut ProviderGenerationSequence,
    readiness_endpoint: &mut CapabilityHandle,
    request_endpoint: &mut CapabilityHandle,
    additional_capabilities: &[BootstrapCapability],
    messages: &ServiceMessages,
    containment: ServiceContainment,
) -> (ProviderGeneration, ContainedServiceJob) {
    'attempt: loop {
        let spec = service.spec();
        let managed_identity = managed_containment_identity(containment);
        let generation = generations
            .next_generation()
            .unwrap_or_else(|_| fail(messages.bootstrap_failed));
        let mut attempt = ContainedServiceActivationAttempt::new(containment);
        let job_management = match ipc::job_create() {
            Ok(handle) => handle,
            Err(_) => fail_contained_activation(attempt, messages, messages.bootstrap_failed),
        };
        attempt.job_management = Some(job_management);
        let job = match ipc::duplicate(job_management, Rights::SIGNAL | Rights::WAIT) {
            Ok(handle) => handle,
            Err(_) => fail_contained_activation(attempt, messages, messages.bootstrap_failed),
        };
        attempt.job = Some(job);
        let job_handles_valid = matches!(
            ipc::info(job_management),
            Ok(info) if info.kind == ObjectKind::Job && info.rights == Rights::JOB && info.size == 0
        ) && matches!(
            ipc::info(job),
            Ok(info)
                if info.kind == ObjectKind::Job
                    && info.rights == Rights::SIGNAL | Rights::WAIT
                    && info.size == 0
        ) && ipc::job_try_wait(job).err() == Some(ipc::Error::NO_CHILD);
        if !job_handles_valid {
            fail_contained_activation(attempt, messages, messages.bootstrap_failed);
        }

        if managed_identity.is_some() {
            let (sender, receiver) = match ipc::endpoint_create_pair() {
                Ok(pair) => pair,
                Err(_) => fail_contained_activation(attempt, messages, messages.bootstrap_failed),
            };
            attempt.bootstrap_sender = Some(sender);
            attempt.bootstrap_receiver_source = Some(receiver);
        } else {
            let generation_handoff_source = match ipc::endpoint_create() {
                Ok(handle) => handle,
                Err(_) => fail_contained_activation(attempt, messages, messages.bootstrap_failed),
            };
            attempt.generation_handoff_source = Some(generation_handoff_source);
            if queue_service_generation(generation_handoff_source, generation).is_err() {
                fail_contained_activation(attempt, messages, messages.bootstrap_failed);
            }
        }
        let _ = syscall::write_all(STDOUT, messages.starting);
        let barrier = match syscall::LaunchBarrier::new() {
            Ok(barrier) => barrier,
            Err(_) => fail_contained_activation(attempt, messages, messages.failed),
        };
        attempt.barrier = Some(barrier);
        let process_id = match syscall::spawn_command_with_barrier(
            spec.command,
            SpawnFlags::NEW_PROCESS_GROUP,
            None,
            None,
            None,
            None,
            attempt
                .barrier
                .as_ref()
                .expect("contained activation owns its barrier"),
        ) {
            Ok(process_id) => process_id,
            Err(_) => fail_contained_activation(attempt, messages, messages.failed),
        };
        attempt.process_id = Some(process_id);
        attempt.process_group_id = Some(process_id);
        if ipc::job_assign(job_management, process_id).ok() != Some(process_id) {
            fail_contained_activation(attempt, messages, messages.bootstrap_failed);
        }
        attempt.job_assigned = true;
        if !matches!(
            ipc::info(job),
            Ok(info)
                if info.kind == ObjectKind::Job
                    && info.rights == Rights::SIGNAL | Rights::WAIT
                    && info.size == 1
        ) {
            fail_contained_activation(attempt, messages, messages.bootstrap_failed);
        }
        let mut management = attempt.job_management.take();
        if !close_cleanup_capability(
            containment.service,
            CleanupPhase::ResourceRelease,
            &mut management,
        ) {
            attempt.job_management = management;
            fail_contained_activation(attempt, messages, messages.bootstrap_failed);
        }
        attempt.job_management = management;

        let bootstrap_ready = match managed_identity {
            Some(identity) => {
                let capabilities = [
                    ManagedStartupCapability {
                        source_handle: *readiness_endpoint,
                        rights: Rights::SEND,
                        role: CapabilityRole::READINESS,
                    },
                    ManagedStartupCapability {
                        source_handle: *request_endpoint,
                        rights: Rights::RECEIVE,
                        role: CapabilityRole::SERVICE_REQUEST,
                    },
                ];
                additional_capabilities.is_empty()
                    && attempt.bootstrap_receiver_source.is_some_and(|receiver| {
                        ipc::grant_child(
                            process_id,
                            receiver,
                            Rights::RECEIVE,
                            PROCESS_START_BOOTSTRAP_HANDLE,
                        )
                        .ok()
                            == Some(PROCESS_START_BOOTSTRAP_HANDLE)
                    })
                    && attempt.bootstrap_sender.is_some_and(|sender| {
                        send_managed_service_process_start(
                            sender,
                            spec.command,
                            process_id,
                            generation,
                            service.restart_count(),
                            identity,
                            &capabilities,
                        )
                    })
            }
            None => {
                let generation_handoff_source = attempt
                    .generation_handoff_source
                    .expect("legacy contained activation owns generation handoff");
                ipc::grant_child(process_id, *readiness_endpoint, Rights::SEND, READY_HANDLE).ok()
                    == Some(READY_HANDLE)
                    && ipc::grant_child(
                        process_id,
                        *request_endpoint,
                        Rights::RECEIVE,
                        REQUEST_HANDLE,
                    )
                    .ok()
                        == Some(REQUEST_HANDLE)
                    && ipc::grant_child(
                        process_id,
                        generation_handoff_source,
                        Rights::RECEIVE,
                        GENERATION_HANDOFF_HANDLE,
                    )
                    .ok()
                        == Some(GENERATION_HANDOFF_HANDLE)
                    && !additional_capabilities.iter().copied().any(|capability| {
                        ipc::grant_child(
                            process_id,
                            capability.source_handle,
                            capability.rights,
                            capability.target_handle,
                        )
                        .ok()
                            != Some(capability.target_handle)
                    })
            }
        };
        if !bootstrap_ready || !attempt.release_child() {
            fail_contained_activation(attempt, messages, messages.bootstrap_failed);
        }
        service.note_spawned(process_id);

        let mut ready_buffer = [0_u8; 64];
        loop {
            match ipc::try_receive(*readiness_endpoint, &mut ready_buffer) {
                Ok(message) => {
                    if message.sender_process_id != process_id
                        || message.capability.is_some()
                        || message.bytes != spec.ready_message.len()
                        || &ready_buffer[..message.bytes] != spec.ready_message
                    {
                        fail_contained_activation(attempt, messages, messages.protocol_failed);
                    }
                    if service.note_ready() != ReadyDisposition::Accepted {
                        fail_contained_activation(attempt, messages, messages.protocol_failed);
                    }
                    let _ = syscall::write_all(STDOUT, messages.ready);
                    return (generation, attempt.into_job(messages));
                }
                Err(error) if error == ipc::Error::TRY_AGAIN => {}
                Err(_) => fail_contained_activation(attempt, messages, messages.protocol_failed),
            }

            match syscall::try_wait_child(process_id) {
                Ok(status) => match service.observe_status(status.raw()) {
                    ServiceStatusDisposition::WaitForNextEvent => {}
                    ServiceStatusDisposition::Restart { backoff_yields } => {
                        if !attempt.finish_reaped() {
                            fail(messages.failed);
                        }
                        let _ = syscall::write_all(STDOUT, messages.restarting);
                        backoff(backoff_yields);
                        close_contained_endpoint(
                            containment,
                            *readiness_endpoint,
                            messages.bootstrap_failed,
                        );
                        close_contained_endpoint(
                            containment,
                            *request_endpoint,
                            messages.bootstrap_failed,
                        );
                        *readiness_endpoint = ipc::endpoint_create()
                            .unwrap_or_else(|_| fail(messages.bootstrap_failed));
                        *request_endpoint = ipc::endpoint_create()
                            .unwrap_or_else(|_| fail(messages.bootstrap_failed));
                        continue 'attempt;
                    }
                    ServiceStatusDisposition::Stopped | ServiceStatusDisposition::Failed => {
                        if !attempt.finish_reaped() {
                            fail(messages.failed);
                        }
                        fail(messages.failed)
                    }
                },
                Err(error) if error == syscall::Errno::TRY_AGAIN => {}
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(_) => fail_contained_activation(attempt, messages, messages.failed),
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
    mut generation: LoggingGeneration,
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
    generation.drain_job();
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
