#![no_std]
#![no_main]

//! Headless integration pilot for the reusable application manager and live portal service.
//! Fixture package policy and compositor input are confined to this executable.

use service_route::{Authorizer, ProviderGeneration, RouteKey};
use userspace::{
    abi::limits,
    application_identity::{
        ApplicationInstallScope, ApplicationInstallation, ApplicationLaunchSelection,
        ApplicationProfile, ApplicationProfileSet, ApplicationTrustClass, AuthorizedApplication,
        InstalledApplicationComponent, PackageVerification, authorize_application_launch,
    },
    application_launch::{
        ApplicationCapability, ApplicationLaunch, ApplicationNamespace, ApplicationNamespaceSources,
    },
    application_lifecycle::{ApplicationLifecyclePolicy, ApplicationLifecycleState},
    application_manager::{
        CONTROL_RIGHTS, PortalManagedApplication, PortalManagerClient, PortalManagerCommand, get,
        put, receive, spawn,
    },
    application_permission::{
        ApplicationPermissionStore, ApplicationResourceKind, ApplicationResourceResolver,
    },
    application_permission_persistence::{
        commit_application_permission_store, recover_application_permission_store,
    },
    application_permission_storage::{
        ApplicationPermissionFilePersistence, NullFsApplicationPermissionFile,
    },
    application_portal::TrustedUserGestureTicket,
    application_portal_service::{SNAPSHOT_BYTES, encode_input},
    application_portal_startup::ApplicationPortalStartup,
    application_portal_surface::PortalKey,
    application_portal_transport::{
        APPLICATION_PORTAL_CLIENT_SOURCE_RIGHTS, APPLICATION_PORTAL_GESTURE_SOURCE_RIGHTS,
    },
    application_service::{BASELINE_DESKTOP_ROUTES, PORTAL_CLIENT_ROUTE},
    args::Args,
    filesystem::{self, Node},
    handle::{Endpoint, OwnedHandle},
    ipc::{self, ObjectKind, Rights},
    managed_startup::{ManagedToolCapability, ManagedToolStart},
    nullfs_primary_volume, platform,
    runtime_context::{CapabilityRole, StartupCapabilityPolicy},
    service_route::{NativeRouteTable, ServiceNamespaceEvent, ServiceNamespaceIngress},
    syscall,
};

const SOURCE: Rights = Rights::SEND
    .union(Rights::DUPLICATE)
    .union(Rights::TRANSFER);
const STATE_NAME: &[u8] = b"portal-manager-integration.bin";
const POLICIES: [StartupCapabilityPolicy; 2] = [
    StartupCapabilityPolicy {
        role: CapabilityRole::PRIVATE_STORAGE,
        kind: ObjectKind::Endpoint,
        minimum_rights: SOURCE,
        maximum_rights: SOURCE,
        required: false,
    },
    StartupCapabilityPolicy {
        role: CapabilityRole::SERVICE_REQUEST,
        kind: ObjectKind::Endpoint,
        minimum_rights: CONTROL_RIGHTS,
        maximum_rights: CONTROL_RIGHTS,
        required: false,
    },
];

userspace::managed_capability_tool_entry!(rust_main, 2, &POLICIES);
userspace::panic_handler!();

trait At<T> {
    fn at(self, code: u64) -> Result<T, u64>;
}
impl<T, E> At<T> for Result<T, E> {
    fn at(self, code: u64) -> Result<T, u64> {
        self.map_err(|_| code)
    }
}
fn check(value: bool, code: u64) -> Result<(), u64> {
    if value { Ok(()) } else { Err(code) }
}

fn rust_main(stack: *const usize, mut start: ManagedToolStart<2>) -> ! {
    let args = unsafe { Args::from_stack(stack) };
    let result = match (args.len(), args.get(1)) {
        (2, Some(b"probe")) => start
            .context
            .take::<Endpoint>(CapabilityRole::PRIVATE_STORAGE, SOURCE)
            .at(2)
            .and_then(|filesystem| {
                check(start.context.is_empty(), 3).and_then(|()| run_manager(filesystem))
            }),
        (2, Some(b"compositor-probe")) => start
            .context
            .take::<Endpoint>(CapabilityRole::SERVICE_REQUEST, CONTROL_RIGHTS)
            .at(4)
            .and_then(|control| {
                check(start.context.is_empty(), 5).and_then(|()| run_compositor(control))
            }),
        _ => Err(1),
    };
    if let Err(code) = result {
        let _ = syscall::write_all(syscall::STDOUT, b"application-manager integration failed\n");
        syscall::exit(code);
    }
    syscall::exit(0)
}

fn authorization() -> Result<AuthorizedApplication, u64> {
    let components = [InstalledApplicationComponent::new(
        21,
        b"/application-portal-target",
        ApplicationProfileSet::DESKTOP,
        true,
    )];
    authorize_application_launch(
        PackageVerification {
            package: 11,
            package_generation: 12,
            application: 13,
            publisher: 17,
            signing_lineage: 18,
            trust_class: ApplicationTrustClass::Repository,
            system_application: false,
            components: &components,
        },
        ApplicationInstallation {
            installation: 19,
            package: 11,
            package_generation: 12,
            application: 13,
            publisher: 17,
            signing_lineage: 18,
            trust_class: ApplicationTrustClass::Repository,
            scope: ApplicationInstallScope::User,
            owner_user: 14,
            system_application: false,
        },
        ApplicationLaunchSelection {
            component: 21,
            user: 14,
            session: 15,
            profile: ApplicationProfile::Desktop,
        },
    )
    .at(10)
}

/// Children in this pilot have no descendants. The application itself uses whole-job supervision.
struct Child(u64);
impl Drop for Child {
    fn drop(&mut self) {
        if self.0 != 0 {
            let _ = platform::kill(self.0, userspace::abi::signal::KILL);
            let _ = syscall::wait_child(self.0);
        }
    }
}
impl Child {
    fn finish(&mut self) -> Result<(), u64> {
        let status = syscall::wait_child(self.0).at(11)?;
        self.0 = 0;
        check(status.success(), 12)
    }
}

fn receive_source(
    control: &OwnedHandle<Endpoint>,
    sender: u64,
    label: &[u8],
    rights: Rights,
) -> Result<OwnedHandle<Endpoint>, u64> {
    let mut bytes = [0; limits::MAX_IPC_MESSAGE_BYTES];
    let message = receive(control, &mut bytes).at(13)?;
    check(
        message.sender_process_id == sender && &bytes[..message.bytes] == label,
        14,
    )?;
    let cap = message.capability.ok_or(15_u64)?;
    check(cap.rights == rights, 16)?;
    let endpoint = cap.handle.try_cast::<Endpoint>().map_err(|_| 17_u64)?;
    check(endpoint.info().at(18)?.rights == rights, 19)?;
    Ok(endpoint)
}

fn expect(control: &OwnedHandle<Endpoint>, sender: u64, expected: &[u8]) -> Result<(), u64> {
    let mut bytes = [0; limits::MAX_IPC_MESSAGE_BYTES];
    let message = receive(control, &mut bytes).at(20)?;
    check(
        message.sender_process_id == sender
            && message.capability.is_none()
            && &bytes[..message.bytes] == expected,
        21,
    )
}

fn run_manager(filesystem: OwnedHandle<Endpoint>) -> Result<(), u64> {
    let session = filesystem::connect_writable_service(filesystem.as_raw(), 1).at(22)?;
    let root = Node::root(session);
    let file = session
        .create_file(2, root, STATE_NAME, true, false)
        .at(23)?;
    let adapter = NullFsApplicationPermissionFile::format_new(session, file, 1, 3).at(24)?;
    let mut persistence = ApplicationPermissionFilePersistence::new(adapter);
    commit_application_permission_store(&mut persistence, &ApplicationPermissionStore::new(), None)
        .at(25)?;
    persistence.into_file().detach().at(26)?;
    let uuid = nullfs_primary_volume::FILESYSTEM_UUID;
    let file_identity = ApplicationResourceResolver::new(session, uuid)
        .ok_or(27_u64)?
        .resolve(20, file, ApplicationResourceKind::File)
        .at(28)?;
    let (compositor_control, compositor_child) = OwnedHandle::<Endpoint>::create_pair().at(29)?;
    let mut compositor = Child(
        spawn(
            b"/application-manager compositor-probe",
            &[ManagedToolCapability::new(
                compositor_child.as_raw(),
                CONTROL_RIGHTS,
                CapabilityRole::SERVICE_REQUEST,
            )],
        )
        .at(30)?,
    );
    drop(compositor_child);
    let (portal_control, portal_child) = OwnedHandle::<Endpoint>::create_pair().at(31)?;
    let mut portal = Child(
        spawn(
            b"/application-portal-service",
            &[
                ManagedToolCapability::new(
                    portal_child.as_raw(),
                    CONTROL_RIGHTS,
                    CapabilityRole::SERVICE_REQUEST,
                ),
                ManagedToolCapability::new(
                    filesystem.as_raw(),
                    Rights::SEND,
                    CapabilityRole::PRIVATE_STORAGE,
                ),
            ],
        )
        .at(32)?,
    );
    drop(portal_child);
    let startup =
        ApplicationPortalStartup::new(syscall::getpid().at(33)?, compositor.0, 1, 15).at(34)?;
    let mut config = [0; 80];
    config[..48].copy_from_slice(&startup.encode());
    config[48..64].copy_from_slice(&uuid);
    put(&mut config, 64, file_identity.object_id());
    put(&mut config, 72, file_identity.object_generation());
    portal_control.send(&config).at(35)?;
    let clients = receive_source(
        &portal_control,
        portal.0,
        b"portal-clients:v1",
        APPLICATION_PORTAL_CLIENT_SOURCE_RIGHTS,
    )?;
    let gestures = receive_source(
        &portal_control,
        portal.0,
        b"portal-gestures:v1",
        APPLICATION_PORTAL_GESTURE_SOURCE_RIGHTS,
    )?;
    let input = receive_source(
        &portal_control,
        portal.0,
        b"portal-input:v1",
        CONTROL_RIGHTS | Rights::TRANSFER,
    )?;
    compositor_control.send(&portal.0.to_le_bytes()).at(36)?;
    compositor_control
        .send_move(b"gestures", gestures, Rights::SEND)
        .map_err(|_| 37_u64)?;
    compositor_control
        .send_move(b"input", input, CONTROL_RIGHTS)
        .map_err(|_| 38_u64)?;
    let mut manager = PortalManagerClient::new(portal_control, portal.0, 1).ok_or(39_u64)?;

    let auth = authorization()?;
    let namespace_source = OwnedHandle::<Endpoint>::create().at(40)?;
    let storage_source = OwnedHandle::<Endpoint>::create().at(41)?;
    let namespace = ApplicationNamespace::new(
        auth,
        ApplicationNamespaceSources {
            service_namespace: namespace_source.as_raw(),
            private_storage: storage_source.as_raw(),
        },
    )
    .at(42)?;
    let mut ingress = ServiceNamespaceIngress::bind(
        namespace_source
            .duplicate(Rights::RECEIVE)
            .at(43)?
            .into_raw(),
        BASELINE_DESKTOP_ROUTES,
    )
    .at(44)?;
    let mut routes = NativeRouteTable::<u64, 1>::new();
    routes
        .publish(
            PORTAL_CLIENT_ROUTE,
            ProviderGeneration::new(1).unwrap(),
            clients.as_raw(),
        )
        .map_err(|_| 45_u64)?;
    let readiness = OwnedHandle::<Endpoint>::create().at(46)?;
    let (app_control, app_source) = OwnedHandle::<Endpoint>::create_pair().at(47)?;
    let capabilities = [
        ApplicationCapability::new(readiness.as_raw(), Rights::SEND, CapabilityRole::READINESS),
        ApplicationCapability::new(
            app_source.as_raw(),
            CONTROL_RIGHTS,
            CapabilityRole::CONFIGURATION,
        ),
    ];
    let mut application = PortalManagedApplication::launch(
        &mut manager,
        ApplicationLaunch::new(
            b"/application-portal-target",
            auth,
            namespace,
            1,
            &capabilities,
        )
        .with_process_limit(2),
        readiness,
        ApplicationLifecyclePolicy::new(100_000, 0, 0).at(48)?,
    )
    .at(49)?;
    drop(app_source);
    let app_pid = application.process_id();
    for request_id in 1..=3 {
        let mut ticket_request = [0; 24];
        ticket_request[..4].copy_from_slice(b"NSTG");
        put(&mut ticket_request, 8, app_pid);
        put(&mut ticket_request, 16, request_id);
        compositor_control.send(&ticket_request).at(50)?;
        expect(&compositor_control, compositor.0, b"gesture-ready")?;
        app_control.send(&request_id.to_le_bytes()).at(51)?;
        if request_id < 3 {
            wait_application_report(
                &app_control,
                &mut application,
                &mut manager,
                &mut ingress,
                &routes,
                request_id,
            )?;
        } else {
            expect(&compositor_control, compositor.0, b"picker-pending")?;
            application.unbind(&mut manager).at(52)?;
            // The pending request receives Cancelled and the earlier live file broker is closed.
            wait_application_report(
                &app_control,
                &mut application,
                &mut manager,
                &mut ingress,
                &routes,
                request_id,
            )?;
        }
    }
    let mut completed = false;
    for _ in 0..100_000 {
        match application.poll(&mut manager).at(53)? {
            ApplicationLifecycleState::Completed => {
                completed = true;
                break;
            }
            ApplicationLifecycleState::Failed => return Err(54),
            _ => {
                syscall::yield_now().at(55)?;
            }
        }
    }
    check(completed, 56)?;
    manager.command(PortalManagerCommand::Shutdown).at(57)?;
    portal.finish()?;
    compositor_control.send(b"stop").at(58)?;
    compositor.finish()?;

    // Recover the actual file after the portal exits: one persistent grant, no cancellation grants.
    let adapter = NullFsApplicationPermissionFile::attach_existing(session, file, 1, 30).at(59)?;
    let mut persistence = ApplicationPermissionFilePersistence::new(adapter);
    let recovered = recover_application_permission_store(&mut persistence).at(60)?;
    check(
        recovered.store.records().count() == 1 && recovered.commit.sequence() == 2,
        61,
    )?;
    persistence.into_file().detach().at(62)?;
    session.unlink(40, root, STATE_NAME).at(63)?;
    session.sync(41).at(64)?;
    session.disconnect(42).at(65)?;
    syscall::write_all(syscall::STDOUT, b"application-manager: live portal selection, NullFS I/O, cancellation, unbind, job drainage, and durable recovery verified\n").at(66)?;
    Ok(())
}

struct Caller(u64);
impl Authorizer<u64> for Caller {
    type Error = ();
    fn authorize(&mut self, caller: &u64, _: RouteKey) -> Result<(), ()> {
        if *caller == self.0 { Ok(()) } else { Err(()) }
    }
}

fn wait_application_report(
    control: &OwnedHandle<Endpoint>,
    application: &mut PortalManagedApplication<2>,
    manager: &mut PortalManagerClient,
    ingress: &mut ServiceNamespaceIngress<6>,
    routes: &NativeRouteTable<u64, 1>,
    expected: u64,
) -> Result<(), u64> {
    let mut bytes = [0; limits::MAX_IPC_MESSAGE_BYTES];
    for _ in 0..100_000 {
        if let Some(ServiceNamespaceEvent::Allowed(request)) = ingress.try_accept().at(67)? {
            request
                .authorize(&mut Caller(application.process_id()))
                .map_err(|_| 68_u64)?
                .resolve(routes)
                .at(69)?;
        }
        match control.try_receive(&mut bytes) {
            Ok(message) => {
                return check(
                    message.sender_process_id == application.process_id()
                        && message.capability.is_none()
                        && message.bytes == 8
                        && get(&bytes, 0) == expected,
                    70,
                );
            }
            Err(error) if error == ipc::Error::TRY_AGAIN => {}
            Err(_) => return Err(71),
        }
        check(
            !matches!(
                application.poll(manager).at(72)?,
                ApplicationLifecycleState::Failed
            ),
            73,
        )?;
        syscall::yield_now().at(74)?;
    }
    Err(75)
}

fn run_compositor(control: OwnedHandle<Endpoint>) -> Result<(), u64> {
    let parent = platform::getppid().at(80)?;
    let mut bytes = [0; limits::MAX_IPC_MESSAGE_BYTES];
    let message = receive(&control, &mut bytes).at(81)?;
    check(
        message.sender_process_id == parent && message.bytes == 8 && message.capability.is_none(),
        82,
    )?;
    let portal = get(&bytes, 0);
    let gestures = receive_source(&control, parent, b"gestures", Rights::SEND)?;
    let input = receive_source(&control, parent, b"input", CONTROL_RIGHTS)?;
    loop {
        let message = receive(&control, &mut bytes).at(83)?;
        check(
            message.sender_process_id == parent && message.capability.is_none(),
            84,
        )?;
        if &bytes[..message.bytes] == b"stop" {
            return Ok(());
        }
        check(message.bytes == 24 && bytes[..8] == *b"NSTG\0\0\0\0", 85)?;
        let app = get(&bytes, 8);
        let request = get(&bytes, 16);
        let now = platform::monotonic_time_ns().at(86)?;
        let ticket = TrustedUserGestureTicket::new(
            request,
            app,
            14,
            15,
            13,
            19,
            100,
            request,
            request,
            now,
            now.saturating_add(5_000_000_000),
        )
        .ok_or(87_u64)?;
        gestures.send(&ticket.encode()).at(88)?;
        let message = receive(&input, &mut bytes).at(89)?;
        check(
            message.sender_process_id == portal
                && message.capability.is_none()
                && message.bytes == 16
                && bytes[..8] == *b"NSGA\0\0\0\0"
                && get(&bytes, 8) == request,
            90,
        )?;
        control.send(b"gesture-ready").at(91)?;
        let mut sequence = 0;
        loop {
            let message = receive(&input, &mut bytes).at(92)?;
            check(
                message.sender_process_id == portal
                    && message.capability.is_none()
                    && message.bytes == SNAPSHOT_BYTES
                    && bytes[..4] == *b"NSPU"
                    && bytes[4] == 1
                    && get(&bytes, 16) == app
                    && get(&bytes, 24) == request,
                93,
            )?;
            let session = get(&bytes, 8);
            if request == 3 {
                control.send(b"picker-pending").at(94)?;
                break;
            }
            let name_len = bytes[6] as usize;
            check(name_len <= 96, 95)?;
            let key = if request == 2 {
                PortalKey::Escape
            } else if &bytes[64..64 + name_len] == b"welcome.txt" {
                PortalKey::Enter
            } else if get(&bytes, 32) + 1 < get(&bytes, 40) {
                PortalKey::Down
            } else if bytes[5] == 0 {
                PortalKey::LoadMore
            } else {
                return Err(96);
            };
            sequence += 1;
            input
                .send(&encode_input(session, session, sequence, key))
                .at(97)?;
            if matches!(key, PortalKey::Escape | PortalKey::Enter) {
                break;
            }
        }
    }
}
