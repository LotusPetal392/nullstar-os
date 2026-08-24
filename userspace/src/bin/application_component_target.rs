#![no_std]
#![no_main]

use userspace::{
    abi::limits,
    application_launch::{
        ApplicationInstallScope, ApplicationProfile, ApplicationStart, ApplicationTrustClass,
    },
    args::Args,
    handle::Endpoint,
    ipc::{self, ObjectKind, Rights},
    runtime_context::{CapabilityRole, StartupCapabilityPolicy},
    syscall,
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

const POLICIES: [StartupCapabilityPolicy; 2] = [
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
];

userspace::application_entry!(rust_main, 2, &POLICIES);
userspace::panic_handler!();

fn rust_main(initial_stack: *const usize, mut start: ApplicationStart<2>) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let expected = match arguments.get(1) {
        Some(b"root") if arguments.len() == 2 => {
            (ApplicationProfile::Desktop, ROOT_COMPONENT, true, 1_u8)
        }
        Some(b"desktop-child") if arguments.len() == 2 => (
            ApplicationProfile::DesktopChild,
            DESKTOP_CHILD_COMPONENT,
            false,
            2,
        ),
        Some(b"worker") if arguments.len() == 2 => {
            (ApplicationProfile::Worker, WORKER_COMPONENT, false, 3)
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
        || start.context.contains(CapabilityRole::SERVICE_NAMESPACE) != expected.2
    {
        syscall::exit(2);
    }
    for descriptor in 0..limits::MAX_OPEN_FILES as u64 {
        match syscall::close(descriptor) {
            Err(error) if error == syscall::Errno::BAD_FILE_DESCRIPTOR => {}
            _ => syscall::exit(3),
        }
    }
    let status = match start
        .context
        .take::<Endpoint>(CapabilityRole::READINESS, Rights::SEND)
    {
        Ok(status) => status,
        Err(_) => syscall::exit(4),
    };
    if expected.2 {
        let namespace = match start
            .context
            .take::<Endpoint>(CapabilityRole::SERVICE_NAMESPACE, Rights::SEND)
        {
            Ok(namespace) => namespace,
            Err(_) => syscall::exit(5),
        };
        drop(namespace);
    }
    if !start.context.is_empty()
        || ipc::send(status.as_raw(), &[expected.3, expected.1 as u8], None).is_err()
    {
        syscall::exit(6);
    }
    drop(status);
    syscall::exit(0)
}
