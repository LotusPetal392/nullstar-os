#![no_std]
#![no_main]

use nswp_logging::{LOGGING_OBSERVER_ROLE, LOGGING_SERVICE_ID};
use service_route::{RouteFailure, RouteKey};
use userspace::{
    abi::limits,
    application_launch::{
        ApplicationInstallScope, ApplicationProfile, ApplicationStart, ApplicationTrustClass,
    },
    application_lifecycle::APPLICATION_READY_MESSAGE,
    application_service::{DISPLAY_CLIENT_ROUTE, LOGGING_PRODUCER_ROUTE},
    args::Args,
    handle::Endpoint,
    ipc::{self, ObjectKind, Rights},
    platform,
    runtime_context::{CapabilityRole, StartupCapabilityPolicy},
    service_route::{ResolveError, ResolvedRoute, RouteResolution},
    syscall::{self, OpenFlags},
};

const IDENTITY_PACKAGE: u64 = 11;
const IDENTITY_PACKAGE_GENERATION: u64 = 12;
const IDENTITY_APPLICATION: u64 = 13;
const IDENTITY_USER: u64 = 14;
const IDENTITY_SESSION: u64 = 15;
const MANAGER_GENERATION: u64 = 16;
const IDENTITY_PUBLISHER: u64 = 17;
const IDENTITY_SIGNING_LINEAGE: u64 = 18;
const IDENTITY_INSTALLATION: u64 = 19;
const ROOT_COMPONENT: u64 = 21;
const DESKTOP_CHILD_COMPONENT: u64 = 22;
const WORKER_COMPONENT: u64 = 23;

const POLICIES: [StartupCapabilityPolicy; 3] = [
    StartupCapabilityPolicy {
        role: CapabilityRole::READINESS,
        kind: ObjectKind::Endpoint,
        minimum_rights: Rights::SEND,
        maximum_rights: Rights::SEND,
        required: true,
    },
    StartupCapabilityPolicy {
        role: CapabilityRole::SERVICE_NAMESPACE,
        kind: ObjectKind::Endpoint,
        minimum_rights: Rights::SEND,
        maximum_rights: Rights::SEND,
        required: false,
    },
    StartupCapabilityPolicy {
        role: CapabilityRole::PRIVATE_STORAGE,
        kind: ObjectKind::Endpoint,
        minimum_rights: Rights::SEND,
        maximum_rights: Rights::SEND,
        required: false,
    },
];

userspace::application_entry!(rust_main, 3, &POLICIES);
userspace::panic_handler!();

fn rust_main(initial_stack: *const usize, mut start: ApplicationStart<3>) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let expected = match arguments.get(1) {
        Some(b"root") if arguments.len() == 2 => (
            ApplicationProfile::Desktop,
            ROOT_COMPONENT,
            true,
            1_u8,
            0_u8,
        ),
        Some(b"desktop-child") if arguments.len() == 2 => (
            ApplicationProfile::DesktopChild,
            DESKTOP_CHILD_COMPONENT,
            false,
            2,
            0,
        ),
        Some(b"worker") if arguments.len() == 2 => {
            (ApplicationProfile::Worker, WORKER_COMPONENT, false, 3, 0)
        }
        Some(b"lifecycle-unready") if arguments.len() == 2 => {
            (ApplicationProfile::Desktop, ROOT_COMPONENT, true, 0, 1)
        }
        Some(b"lifecycle-running") if arguments.len() == 2 => {
            (ApplicationProfile::Desktop, ROOT_COMPONENT, true, 0, 2)
        }
        _ => syscall::exit(1),
    };
    if start.profile != expected.0
        || start.identity.package != IDENTITY_PACKAGE
        || start.identity.package_generation != IDENTITY_PACKAGE_GENERATION
        || start.identity.application != IDENTITY_APPLICATION
        || start.identity.component != expected.1
        || start.identity.user != IDENTITY_USER
        || start.identity.session != IDENTITY_SESSION
        || start.manager_generation != MANAGER_GENERATION
        || start.principal.application != IDENTITY_APPLICATION
        || start.principal.publisher != IDENTITY_PUBLISHER
        || start.principal.signing_lineage != IDENTITY_SIGNING_LINEAGE
        || start.principal.trust_class != ApplicationTrustClass::Repository
        || start.principal.system_application
        || start.provenance.installation != IDENTITY_INSTALLATION
        || start.provenance.scope != ApplicationInstallScope::User
        || namespace_roles()
            .iter()
            .any(|role| start.context.contains(*role) != expected.2)
    {
        syscall::exit(2);
    }
    for descriptor in 0..limits::MAX_OPEN_FILES as u64 {
        match syscall::close(descriptor) {
            Err(error) if error == syscall::Errno::BAD_FILE_DESCRIPTOR => {}
            _ => syscall::exit(3),
        }
    }
    if !ambient_paths_are_sealed() {
        syscall::exit(4);
    }
    let status = match start
        .context
        .take::<Endpoint>(CapabilityRole::READINESS, Rights::SEND)
    {
        Ok(status) => status,
        Err(_) => syscall::exit(5),
    };
    if expected.4 == 0 && expected.2 {
        let service_namespace = match start
            .context
            .take::<Endpoint>(CapabilityRole::SERVICE_NAMESPACE, Rights::SEND)
        {
            Ok(endpoint) => endpoint,
            Err(_) => syscall::exit(6),
        };
        if !exercise_service_namespace(service_namespace.as_raw()) {
            syscall::exit(7);
        }
        drop(service_namespace);
        let private_storage = match start
            .context
            .take::<Endpoint>(CapabilityRole::PRIVATE_STORAGE, Rights::SEND)
        {
            Ok(endpoint) => endpoint,
            Err(_) => syscall::exit(6),
        };
        if ipc::send(private_storage.as_raw(), &[2], None).is_err() {
            syscall::exit(7);
        }
        drop(private_storage);
    }
    if expected.4 != 0 {
        let service_namespace = match start
            .context
            .take::<Endpoint>(CapabilityRole::SERVICE_NAMESPACE, Rights::SEND)
        {
            Ok(endpoint) => endpoint,
            Err(_) => syscall::exit(6),
        };
        let private_storage = match start
            .context
            .take::<Endpoint>(CapabilityRole::PRIVATE_STORAGE, Rights::SEND)
        {
            Ok(endpoint) => endpoint,
            Err(_) => syscall::exit(6),
        };
        drop(service_namespace);
        drop(private_storage);
        if !start.context.is_empty() {
            syscall::exit(8);
        }
        if expected.4 == 2 && ipc::send(status.as_raw(), APPLICATION_READY_MESSAGE, None).is_err() {
            syscall::exit(8);
        }
        loop {
            let _ = syscall::yield_now();
        }
    }
    if !start.context.is_empty()
        || ipc::send(status.as_raw(), &[expected.3, expected.1 as u8], None).is_err()
    {
        syscall::exit(8);
    }
    drop(status);
    syscall::exit(0)
}

fn exercise_service_namespace(namespace: u64) -> bool {
    let Ok(mut logging) = RouteResolution::begin(namespace, LOGGING_PRODUCER_ROUTE) else {
        return false;
    };
    let Ok(route) = wait_for_route(&mut logging) else {
        return false;
    };
    if route.key() != LOGGING_PRODUCER_ROUTE
        || route.generation().get() != 1
        || route.broker_process_id() == 0
        || route.object_id() == 0
    {
        return false;
    }
    let provider = route.into_handle();
    let provider_sent = ipc::send(provider, &[1], None).is_ok();
    let provider_closed = ipc::close(provider).is_ok();
    if !provider_sent || !provider_closed {
        return false;
    }

    let Ok(mut unavailable) = RouteResolution::begin(namespace, DISPLAY_CLIENT_ROUTE) else {
        return false;
    };
    if !matches!(
        wait_for_route(&mut unavailable),
        Err(ResolveError::BrokerFailure(RouteFailure::Unavailable))
    ) {
        return false;
    }

    let denied_key = RouteKey::new(LOGGING_SERVICE_ID, LOGGING_OBSERVER_ROLE);
    let Ok(mut denied) = RouteResolution::begin(namespace, denied_key) else {
        return false;
    };
    matches!(
        wait_for_route(&mut denied),
        Err(ResolveError::BrokerFailure(RouteFailure::Unauthorized))
    )
}

fn wait_for_route(resolution: &mut RouteResolution) -> Result<ResolvedRoute, ResolveError> {
    for _ in 0..4096 {
        match resolution.try_complete()? {
            Some(route) => return Ok(route),
            None => {
                if syscall::yield_now().is_err() {
                    break;
                }
            }
        }
    }
    Err(ResolveError::Receive(ipc::Error::TRY_AGAIN))
}

const fn namespace_roles() -> [CapabilityRole; 2] {
    [
        CapabilityRole::SERVICE_NAMESPACE,
        CapabilityRole::PRIVATE_STORAGE,
    ]
}

fn ambient_paths_are_sealed() -> bool {
    let mut entries = [];
    syscall::open(b"/", OpenFlags::READ) == Err(syscall::Errno::PERMISSION)
        && platform::stat(b"/") == Err(platform::Errno::PERMISSION)
        && platform::read_directory(b"/", 0, &mut entries) == Err(platform::Errno::PERMISSION)
        && platform::chdir(b"/") == Err(platform::Errno::PERMISSION)
        && platform::unlink(b"/forbidden") == Err(platform::Errno::PERMISSION)
        && syscall::execve(b"/forbidden") == Err(syscall::Errno::PERMISSION)
}
