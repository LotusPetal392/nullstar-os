#![no_std]
#![no_main]

use userspace::{
    abi::limits,
    application_manager::{CONTROL_RIGHTS, get, receive},
    application_permission::{
        ApplicationResourceIdentity, ApplicationResourceKind, ApplicationResourceRestorer,
    },
    application_permission_persistence::recover_application_permission_store,
    application_permission_storage::{
        ApplicationPermissionFilePersistence, NullFsApplicationPermissionFile,
    },
    application_portal_compositor::ApplicationPortalCompositor,
    application_portal_filesystem::ApplicationPortalFilesystemBrowser,
    application_portal_service::{ApplicationPortalService, PortalServiceResources},
    application_portal_startup::ApplicationPortalStartup,
    application_portal_transport::{
        APPLICATION_PORTAL_CLIENT_SOURCE_RIGHTS, APPLICATION_PORTAL_GESTURE_SOURCE_RIGHTS,
    },
    filesystem,
    handle::{Endpoint, OwnedHandle},
    ipc::{ObjectKind, Rights},
    managed_startup::ManagedToolStart,
    platform,
    runtime_context::{CapabilityRole, StartupCapabilityPolicy},
    syscall,
};

const POLICIES: [StartupCapabilityPolicy; 2] = [
    StartupCapabilityPolicy {
        role: CapabilityRole::SERVICE_REQUEST,
        kind: ObjectKind::Endpoint,
        minimum_rights: CONTROL_RIGHTS,
        maximum_rights: CONTROL_RIGHTS,
        required: true,
    },
    StartupCapabilityPolicy {
        role: CapabilityRole::PRIVATE_STORAGE,
        kind: ObjectKind::Endpoint,
        minimum_rights: Rights::SEND,
        maximum_rights: Rights::SEND,
        required: true,
    },
];

userspace::managed_capability_tool_entry!(rust_main, 2, &POLICIES);
userspace::panic_handler!();

// The single process entry borrows this registry once. Keeping its bounded session storage in
// BSS avoids copying it through multiple constructor temporaries on the 256 KiB userspace stack.
static mut PICKERS: ApplicationPortalCompositor = ApplicationPortalCompositor::new();

/// Concrete live service state kept in BSS so its transport, store, and request registries never
/// occupy the 256 KiB userspace stack while nested polling calls accumulate their own frames on
/// top of rust_main's live frame.
type PortalServiceState = ApplicationPortalService<
    'static,
    ApplicationPermissionFilePersistence<NullFsApplicationPermissionFile>,
>;

static mut SERVICE: core::mem::MaybeUninit<PortalServiceState> = core::mem::MaybeUninit::uninit();

fn rust_main(_: *const usize, mut start: ManagedToolStart<2>) -> ! {
    let manager = start
        .context
        .take::<Endpoint>(CapabilityRole::SERVICE_REQUEST, CONTROL_RIGHTS)
        .unwrap_or_else(|_| syscall::exit(2));
    let filesystem = start
        .context
        .take::<Endpoint>(CapabilityRole::PRIVATE_STORAGE, Rights::SEND)
        .unwrap_or_else(|_| syscall::exit(3));
    let mut bytes = [0; limits::MAX_IPC_MESSAGE_BYTES];
    let message = receive(&manager, &mut bytes).unwrap_or_else(|_| syscall::exit(4));
    if message.bytes != 80
        || message.capability.is_some()
        || platform::getppid().ok() != Some(message.sender_process_id)
    {
        syscall::exit(5);
    }
    let startup =
        ApplicationPortalStartup::decode(&bytes[..48]).unwrap_or_else(|_| syscall::exit(6));
    if startup.application_manager_process_id() != message.sender_process_id {
        syscall::exit(7);
    }
    let uuid: [u8; 16] = bytes[48..64].try_into().unwrap();
    let identity = ApplicationResourceIdentity::new(
        uuid,
        get(&bytes, 64),
        get(&bytes, 72),
        ApplicationResourceKind::File,
    )
    .unwrap_or_else(|| syscall::exit(8));
    let storage_session = filesystem::connect_writable_service(filesystem.as_raw(), 1)
        .unwrap_or_else(|_| syscall::exit(9));
    let file = ApplicationResourceRestorer::new(storage_session, uuid)
        .unwrap_or_else(|| syscall::exit(10))
        .restore(2, identity)
        .unwrap_or_else(|_| syscall::exit(11));
    let adapter = NullFsApplicationPermissionFile::attach_existing(storage_session, file, 1, 3)
        .unwrap_or_else(|_| syscall::exit(12));
    let mut persistence = ApplicationPermissionFilePersistence::new(adapter);
    // Never silently reformat a corrupt or uncommitted store at startup.
    let recovered = recover_application_permission_store(&mut persistence)
        .unwrap_or_else(|_| syscall::exit(13));
    let browser_session =
        filesystem::connect_service(filesystem.as_raw(), 1).unwrap_or_else(|_| syscall::exit(14));
    let (browser, root) = ApplicationPortalFilesystemBrowser::attach(browser_session, uuid, 1, 2)
        .unwrap_or_else(|_| syscall::exit(15));
    let (transport, clients, gestures) = startup
        .start_transport()
        .unwrap_or_else(|_| syscall::exit(16));
    let (mut compositor, compositor_source) =
        OwnedHandle::<Endpoint>::create_pair().unwrap_or_else(|_| syscall::exit(17));
    compositor
        .replace_rights(CONTROL_RIGHTS)
        .unwrap_or_else(|_| syscall::exit(18));
    manager
        .send_move(
            b"portal-clients:v1",
            clients.into_endpoint(),
            APPLICATION_PORTAL_CLIENT_SOURCE_RIGHTS,
        )
        .unwrap_or_else(|_| syscall::exit(19));
    manager
        .send_move(
            b"portal-gestures:v1",
            gestures.into_endpoint(),
            APPLICATION_PORTAL_GESTURE_SOURCE_RIGHTS,
        )
        .unwrap_or_else(|_| syscall::exit(20));
    manager
        .send_move(
            b"portal-input:v1",
            compositor_source,
            CONTROL_RIGHTS | Rights::TRANSFER,
        )
        .unwrap_or_else(|_| syscall::exit(21));
    let service_slot = core::ptr::addr_of_mut!(SERVICE);
    // SAFETY: rust_main runs once and is the sole accessor of SERVICE; it writes exactly one
    // initialized value before reading, and never touches PICKERS directly after handing this
    // exclusive borrow to the service. PICKERS lives for the program duration, so its borrow can
    // be 'static while stored inside SERVICE.
    let picker_ui: &'static mut ApplicationPortalCompositor =
        unsafe { &mut *core::ptr::addr_of_mut!(PICKERS) };
    unsafe {
        (*service_slot).write(ApplicationPortalService::new(
            startup,
            PortalServiceResources {
                manager,
                compositor,
                filesystem,
                transport,
                browser,
                root,
                store: recovered.store,
                persistence,
                commit: recovered.commit,
            },
            picker_ui,
        ));
    }
    let service = unsafe { (*service_slot).as_mut_ptr() };
    loop {
        match unsafe { (&mut *service).poll() } {
            Ok(true) => {
                let _ = syscall::yield_now();
            }
            Ok(false) => {
                // SAFETY: SERVICE holds exactly one initialized value; read moves it onto the
                // stack to be finished. Shutdown depth is shallow, so this temporary fits easily.
                let owned = unsafe { core::ptr::read(service) };
                let persistence = owned.finish();
                if persistence.into_file().detach().is_err()
                    || storage_session.disconnect(100).is_err()
                    || browser_session.disconnect(100).is_err()
                {
                    syscall::exit(23);
                }
                syscall::exit(0);
            }
            // Exiting closes every broker even if persistence publication had an unknown outcome.
            Err(_) => syscall::exit(22),
        }
    }
}
