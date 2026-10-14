#![no_std]
#![no_main]

use userspace::{
    abi::limits,
    application_launch::ApplicationStart,
    application_lifecycle::APPLICATION_READY_MESSAGE,
    application_manager::{CONTROL_RIGHTS, get, receive},
    application_permission::{ApplicationGrantRights, ApplicationGrantScope},
    application_portal::{
        ApplicationPortalOperation, ApplicationPortalRequest, ApplicationPortalResponse,
        ApplicationPortalStatus,
    },
    application_portal_transport::ApplicationPortalReplyReceiver,
    application_service::PORTAL_CLIENT_ROUTE,
    filesystem::{self, Node, protocol},
    handle::{Endpoint, OwnedHandle, SharedMemory},
    ipc::{ObjectKind, Rights},
    platform,
    runtime_context::{CapabilityRole, StartupCapabilityPolicy},
    service_route::RouteResolution,
    syscall,
};

const POLICIES: [StartupCapabilityPolicy; 4] = [
    StartupCapabilityPolicy {
        role: CapabilityRole::READINESS,
        kind: ObjectKind::Endpoint,
        minimum_rights: Rights::SEND,
        maximum_rights: Rights::SEND,
        required: true,
    },
    StartupCapabilityPolicy {
        role: CapabilityRole::CONFIGURATION,
        kind: ObjectKind::Endpoint,
        minimum_rights: CONTROL_RIGHTS,
        maximum_rights: CONTROL_RIGHTS,
        required: true,
    },
    StartupCapabilityPolicy {
        role: CapabilityRole::SERVICE_NAMESPACE,
        kind: ObjectKind::Endpoint,
        minimum_rights: Rights::SEND,
        maximum_rights: Rights::SEND,
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
userspace::application_entry!(rust_main, 4, &POLICIES);
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

fn rust_main(_: *const usize, start: ApplicationStart<4>) -> ! {
    syscall::exit(run(start).err().unwrap_or(0))
}

fn run(mut start: ApplicationStart<4>) -> Result<(), u64> {
    check(
        start.identity.application == 13
            && start.identity.user == 14
            && start.identity.session == 15
            && start.manager_generation == 1,
        1,
    )?;
    let parent = platform::getppid().at(2)?;
    let ready = start
        .context
        .take::<Endpoint>(CapabilityRole::READINESS, Rights::SEND)
        .at(3)?;
    let control = start
        .context
        .take::<Endpoint>(CapabilityRole::CONFIGURATION, CONTROL_RIGHTS)
        .at(4)?;
    let namespace = start
        .context
        .take::<Endpoint>(CapabilityRole::SERVICE_NAMESPACE, Rights::SEND)
        .at(5)?;
    let storage = start
        .context
        .take::<Endpoint>(CapabilityRole::PRIVATE_STORAGE, Rights::SEND)
        .at(6)?;
    check(start.context.is_empty(), 7)?;
    drop(storage);
    check(
        syscall::open(b"/Volumes/NullStar/welcome.txt", syscall::OpenFlags::READ).is_err(),
        8,
    )?;
    ready.send(APPLICATION_READY_MESSAGE).at(9)?;
    drop(ready);
    let mut bytes = [0; limits::MAX_IPC_MESSAGE_BYTES];
    let mut resource: Option<OwnedHandle<Endpoint>> = None;
    let mut portal: Option<OwnedHandle<Endpoint>> = None;
    for request_id in 1..=3 {
        let message = receive(&control, &mut bytes).at(10)?;
        check(
            message.sender_process_id == parent
                && message.capability.is_none()
                && message.bytes == 8
                && get(&bytes, 0) == request_id,
            11,
        )?;
        if portal.is_none() {
            let mut resolution =
                RouteResolution::begin(namespace.as_raw(), PORTAL_CLIENT_ROUTE).at(12)?;
            for _ in 0..100_000 {
                if let Some(route) = resolution.try_complete().at(13)? {
                    let handle = route.into_handle();
                    // RouteResolution has validated the exact SEND endpoint returned by the manager.
                    portal = Some(unsafe { OwnedHandle::<Endpoint>::from_raw(handle) }.at(14)?);
                    break;
                }
                syscall::yield_now().at(15)?;
            }
        }
        let portal = portal.as_ref().ok_or(16_u64)?;
        check(portal.info().at(17)?.rights == Rights::SEND, 18)?;
        let request = ApplicationPortalRequest::new(
            request_id,
            request_id,
            100,
            ApplicationPortalOperation::OpenFile,
            ApplicationGrantRights::READ,
            ApplicationGrantScope::Persistent,
        )
        .ok_or(19_u64)?;
        let (reply, reply_source) = ApplicationPortalReplyReceiver::mint().at(20)?;
        portal
            .send_move(
                &request.encode(),
                reply_source.into_endpoint(),
                Rights::SEND,
            )
            .map_err(|_| 21_u64)?;
        let reply_endpoint = reply.into_endpoint();
        let message = receive(&reply_endpoint, &mut bytes).at(22)?;
        let response = ApplicationPortalResponse::decode(&bytes[..message.bytes]).at(23)?;
        check(response.request_id() == request_id, 24)?;
        if request_id == 1 {
            check(response.status() == ApplicationPortalStatus::Selected, 25)?;
            let capability = message.capability.ok_or(26_u64)?;
            check(capability.rights == Rights::SEND, 27)?;
            let selected = capability
                .handle
                .try_cast::<Endpoint>()
                .map_err(|_| 28_u64)?;
            let session = filesystem::connect_service(selected.as_raw(), 1).at(29)?;
            let shared = OwnedHandle::<SharedMemory>::create(256).at(30)?;
            session
                .attach_shared_buffer(2, 1, shared.as_raw(), 256)
                .at(31)?;
            let length = session
                .read_to_shared_buffer(
                    3,
                    Node::root(session),
                    0,
                    protocol::BulkBuffer {
                        buffer_id: 1,
                        offset: 0,
                        length: 256,
                    },
                )
                .at(32)?;
            let mut text = [0; 256];
            shared.read(0, &mut text).at(33)?;
            check(
                &text[..length] == b"NullStar persistent storage service fixture.\n",
                34,
            )?;
            check(session.truncate(4, Node::root(session), 0).is_err(), 35)?;
            // Keep the grant session alive so manager unbind must actively close its broker.
            resource = Some(selected);
        } else {
            check(
                response.status() == ApplicationPortalStatus::Cancelled
                    && message.capability.is_none(),
                36,
            )?;
            if request_id == 3 {
                check(resource.as_ref().ok_or(37_u64)?.send(b"stale").is_err(), 38)?;
            }
        }
        control.send(&request_id.to_le_bytes()).at(39)?;
    }
    Ok(())
}
