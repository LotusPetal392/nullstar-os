#![no_std]
#![no_main]

use nswp_logging::{LOGGING_OBSERVER_ROLE, LOGGING_SERVICE_ID};
use service_route::{Authorizer, ProviderGeneration, RouteFailure, RouteKey};
use userspace::{
    application_launch::{
        ApplicationCapability, ApplicationComponentCapability, ApplicationComponentLaunch,
        ApplicationComponentLaunchError, ApplicationIdentityError, ApplicationInstallScope,
        ApplicationInstallation, ApplicationInstance, ApplicationLaunch,
        ApplicationLaunchSelection, ApplicationNamespace, ApplicationNamespaceError,
        ApplicationNamespaceSources, ApplicationProfile, ApplicationProfileSet,
        ApplicationTrustClass, AuthorizedApplication, ComponentProfileSet,
        InstalledApplicationComponent, PackageVerification, authorize_application_launch,
        spawn_application,
    },
    application_lifecycle::{
        ApplicationFailure, ApplicationLifecyclePolicy, ApplicationLifecycleState,
        ApplicationTerminationReason, SupervisedApplication,
    },
    application_service::{BASELINE_DESKTOP_ROUTES, DISPLAY_CLIENT_ROUTE, LOGGING_PRODUCER_ROUTE},
    handle::{Endpoint, OwnedHandle},
    ipc::{self, CapabilityHandle, Rights, Signals},
    runtime_context::CapabilityRole,
    service_route::{NativeRouteTable, RouteReply, ServiceNamespaceEvent, ServiceNamespaceIngress},
    syscall,
};

const JOB_WAIT_YIELDS: usize = 4096;
const IDENTITY_PACKAGE: u64 = 11;
const IDENTITY_PACKAGE_GENERATION: u64 = 12;
const IDENTITY_APPLICATION: u64 = 13;
const IDENTITY_USER: u64 = 14;
const IDENTITY_SESSION: u64 = 15;
const MANAGER_GENERATION: u64 = 16;
const IDENTITY_PUBLISHER: u64 = 17;
const IDENTITY_SIGNING_LINEAGE: u64 = 18;
const IDENTITY_INSTALLATION: u64 = 19;
const APPLICATION_ROUTE_GENERATION: u64 = 1;
const ROOT_COMPONENT: u64 = 21;
const DESKTOP_CHILD_COMPONENT: u64 = 22;
const WORKER_COMPONENT: u64 = 23;

userspace::entry!(rust_main);
userspace::panic_handler!();

fn rust_main(_initial_stack: *const usize) -> ! {
    syscall::exit(
        if application_component_probe() && application_lifecycle_probe() {
            0
        } else {
            1
        },
    )
}

fn application_component_probe() -> bool {
    const ROOT_REPORT: u8 = ROOT_COMPONENT as u8;
    const DESKTOP_CHILD_REPORT: u8 = DESKTOP_CHILD_COMPONENT as u8;
    const WORKER_REPORT: u8 = WORKER_COMPONENT as u8;

    let components = [
        InstalledApplicationComponent::new(
            ROOT_COMPONENT,
            b"/application-component-target",
            ApplicationProfileSet::DESKTOP,
            true,
        ),
        InstalledApplicationComponent::new(
            DESKTOP_CHILD_COMPONENT,
            b"/application-component-target",
            ApplicationProfileSet::DESKTOP_CHILD,
            false,
        ),
        InstalledApplicationComponent::new(
            WORKER_COMPONENT,
            b"/application-component-target",
            ApplicationProfileSet::WORKER,
            false,
        ),
    ];
    let verification = PackageVerification {
        package: IDENTITY_PACKAGE,
        package_generation: IDENTITY_PACKAGE_GENERATION,
        application: IDENTITY_APPLICATION,
        publisher: IDENTITY_PUBLISHER,
        signing_lineage: IDENTITY_SIGNING_LINEAGE,
        trust_class: ApplicationTrustClass::Repository,
        system_application: false,
        components: &components,
    };
    let installation = ApplicationInstallation {
        installation: IDENTITY_INSTALLATION,
        package: IDENTITY_PACKAGE,
        package_generation: IDENTITY_PACKAGE_GENERATION,
        application: IDENTITY_APPLICATION,
        publisher: IDENTITY_PUBLISHER,
        signing_lineage: IDENTITY_SIGNING_LINEAGE,
        trust_class: ApplicationTrustClass::Repository,
        scope: ApplicationInstallScope::User,
        owner_user: IDENTITY_USER,
        system_application: false,
    };
    let selection = ApplicationLaunchSelection {
        component: ROOT_COMPONENT,
        user: IDENTITY_USER,
        session: IDENTITY_SESSION,
        profile: ApplicationProfile::Desktop,
    };
    let mut wrong_lineage = verification;
    wrong_lineage.signing_lineage += 1;
    if authorize_application_launch(wrong_lineage, installation, selection)
        != Err(ApplicationIdentityError::SigningLineageMismatch)
    {
        return false;
    }
    let mut wrong_user = selection;
    wrong_user.user += 1;
    if authorize_application_launch(verification, installation, wrong_user)
        != Err(ApplicationIdentityError::UserScopeMismatch)
    {
        return false;
    }
    let Ok(authorization) = authorize_application_launch(verification, installation, selection)
    else {
        return false;
    };

    let Ok(status) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let Ok(service_namespace) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let Ok(private_storage) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let Ok(service_namespace_receive) = ipc::duplicate(service_namespace.as_raw(), Rights::RECEIVE)
    else {
        return false;
    };
    let mut service_namespace_ingress =
        match ServiceNamespaceIngress::bind(service_namespace_receive, BASELINE_DESKTOP_ROUTES) {
            Ok(ingress) => ingress,
            Err(_) => {
                let _ = ipc::close(service_namespace_receive);
                return false;
            }
        };
    let Ok(logging_provider) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let Ok(logging_authority) = ipc::duplicate(
        logging_provider.as_raw(),
        Rights::SEND | Rights::DUPLICATE | Rights::TRANSFER,
    ) else {
        return false;
    };
    let mut application_routes = NativeRouteTable::<CapabilityHandle, 1>::new();
    let route_generation = ProviderGeneration::new(APPLICATION_ROUTE_GENERATION)
        .expect("application route generation is nonzero");
    if let Err(error) =
        application_routes.publish(LOGGING_PRODUCER_ROUTE, route_generation, logging_authority)
    {
        let _ = ipc::close(error.into_authority());
        return false;
    }
    let aliased_sources = ApplicationNamespaceSources {
        service_namespace: service_namespace.as_raw(),
        private_storage: service_namespace.as_raw(),
    };
    if ApplicationNamespace::new(authorization, aliased_sources)
        != Err(ApplicationNamespaceError::AliasedEndpoint(
            CapabilityRole::PRIVATE_STORAGE,
        ))
    {
        return false;
    }
    let namespace_sources = ApplicationNamespaceSources {
        service_namespace: service_namespace.as_raw(),
        private_storage: private_storage.as_raw(),
    };
    let Ok(namespace) = ApplicationNamespace::new(authorization, namespace_sources) else {
        return false;
    };
    let root_capabilities =
        [
            ApplicationCapability::new(status.as_raw(), Rights::SEND, CapabilityRole::READINESS)
                .delegable_to(ComponentProfileSet::ALL_REDUCED),
        ];
    let root_launch = ApplicationLaunch::new(
        b"/application-component-target root",
        authorization,
        namespace,
        MANAGER_GENERATION,
        &root_capabilities,
    )
    .with_process_limit(4);
    let Ok(application) = spawn_application(root_launch) else {
        return false;
    };
    if application.principal().publisher != IDENTITY_PUBLISHER
        || application.principal().signing_lineage != IDENTITY_SIGNING_LINEAGE
        || application.provenance().installation != IDENTITY_INSTALLATION
        || application.provenance().scope != ApplicationInstallScope::User
    {
        return false;
    }
    let mut process_ids = [application.process_id, 0, 0];
    let mut route_probe = ApplicationRouteProbe::default();

    let forbidden_capabilities = [ApplicationComponentCapability::new(
        service_namespace.as_raw(),
        Rights::SEND,
        CapabilityRole::SERVICE_NAMESPACE,
    )];
    let forbidden = application.spawn_component(ApplicationComponentLaunch::new(
        b"/application-component-target desktop-child",
        DESKTOP_CHILD_COMPONENT,
        ApplicationProfile::DesktopChild,
        &forbidden_capabilities,
    ));
    let escalating_capabilities = [ApplicationComponentCapability::new(
        status.as_raw(),
        Rights::SEND | Rights::DUPLICATE,
        CapabilityRole::READINESS,
    )];
    let escalating = application.spawn_component(ApplicationComponentLaunch::new(
        b"/application-component-target worker",
        WORKER_COMPONENT,
        ApplicationProfile::Worker,
        &escalating_capabilities,
    ));
    let identity_capabilities = [ApplicationComponentCapability::new(
        status.as_raw(),
        Rights::SEND,
        CapabilityRole::READINESS,
    )];
    let undeclared = application.spawn_component(ApplicationComponentLaunch::new(
        b"/application-component-target worker",
        99,
        ApplicationProfile::Worker,
        &identity_capabilities,
    ));
    let wrong_profile = application.spawn_component(ApplicationComponentLaunch::new(
        b"/application-component-target desktop-child",
        WORKER_COMPONENT,
        ApplicationProfile::DesktopChild,
        &identity_capabilities,
    ));
    let mut result = forbidden
        == Err(ApplicationComponentLaunchError::AuthorityNotDelegable(
            CapabilityRole::SERVICE_NAMESPACE,
        ))
        && escalating
            == Err(ApplicationComponentLaunchError::AuthorityEscalation(
                CapabilityRole::READINESS,
            ))
        && undeclared
            == Err(ApplicationComponentLaunchError::Identity(
                ApplicationIdentityError::ComponentNotAuthorized,
            ))
        && wrong_profile
            == Err(ApplicationComponentLaunchError::Identity(
                ApplicationIdentityError::ProfileNotAuthorized,
            ));

    let component_capabilities = [ApplicationComponentCapability::new(
        status.as_raw(),
        Rights::SEND,
        CapabilityRole::READINESS,
    )];
    if result {
        match application.spawn_component(ApplicationComponentLaunch::new(
            b"/application-component-target desktop-child",
            DESKTOP_CHILD_COMPONENT,
            ApplicationProfile::DesktopChild,
            &component_capabilities,
        )) {
            Ok(component) => {
                process_ids[1] = component.process_id;
                result = component.identity.component == DESKTOP_CHILD_COMPONENT
                    && component.profile == ApplicationProfile::DesktopChild;
            }
            Err(_) => result = false,
        }
    }
    if result {
        match application.spawn_component(ApplicationComponentLaunch::new(
            b"/application-component-target worker",
            WORKER_COMPONENT,
            ApplicationProfile::Worker,
            &component_capabilities,
        )) {
            Ok(component) => {
                process_ids[2] = component.process_id;
                result = component.identity.component == WORKER_COMPONENT
                    && component.profile == ApplicationProfile::Worker;
            }
            Err(_) => result = false,
        }
    }

    let mut received = 0_u8;
    if result {
        for _ in 0..process_ids.len() {
            let mut report = [0_u8; 2];
            let mut message = None;
            for _ in 0..JOB_WAIT_YIELDS {
                pump_application_namespace(
                    &mut service_namespace_ingress,
                    &application_routes,
                    application.process_id,
                    &mut route_probe,
                );
                match status.try_receive(&mut report) {
                    Ok(received_message) => {
                        message = Some(received_message);
                        break;
                    }
                    Err(error) if error == ipc::Error::TRY_AGAIN => {
                        let _ = syscall::yield_now();
                    }
                    Err(_) => break,
                }
            }
            let Some(message) = message else {
                result = false;
                break;
            };
            let expected = match report {
                [1, ROOT_REPORT] => Some((1 << 0, process_ids[0])),
                [2, DESKTOP_CHILD_REPORT] => Some((1 << 1, process_ids[1])),
                [3, WORKER_REPORT] => Some((1 << 2, process_ids[2])),
                _ => None,
            };
            let Some((bit, sender)) = expected else {
                result = false;
                break;
            };
            if message.bytes != report.len()
                || message.capability.is_some()
                || message.sender_process_id != sender
                || received & bit != 0
            {
                result = false;
                break;
            }
            received |= bit;
        }
        result &= received == 0b111 && route_probe.complete();
    }

    if result {
        result = receive_namespace_report(&logging_provider, application.process_id, 1)
            && receive_namespace_report(&private_storage, application.process_id, 2);
    }

    if !result {
        let _ = ipc::job_terminate(application.job.as_raw());
    }
    let mut exited = true;
    for process_id in process_ids
        .into_iter()
        .filter(|process_id| *process_id != 0)
    {
        exited &= syscall::wait_child(process_id).is_ok_and(|status| status.success());
    }
    let mut completed = 0_u8;
    for _ in 0..process_ids
        .into_iter()
        .filter(|process_id| *process_id != 0)
        .count()
    {
        let Some(exit) = bounded_job_wait(application.job.as_raw()) else {
            exited = false;
            break;
        };
        let Some(index) = process_ids
            .iter()
            .position(|process_id| *process_id == exit.process_id)
        else {
            exited = false;
            break;
        };
        let bit = 1 << index;
        if completed & bit != 0 || !exit.status.success() {
            exited = false;
            break;
        }
        completed |= bit;
    }
    let expected_completions = (1_u8
        << process_ids
            .iter()
            .filter(|process_id| **process_id != 0)
            .count())
        - 1;
    result
        && exited
        && completed == expected_completions
        && application.job.info().is_ok_and(|info| info.size == 0)
}

fn application_lifecycle_probe() -> bool {
    let Some(authorization) = authorized_root_application() else {
        return false;
    };
    let Ok(service_namespace) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let Ok(private_storage) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let sources = ApplicationNamespaceSources {
        service_namespace: service_namespace.as_raw(),
        private_storage: private_storage.as_raw(),
    };
    let Some((first, first_readiness)) = launch_lifecycle_attempt(
        authorization,
        sources,
        b"/application-component-target lifecycle-unready",
    ) else {
        return false;
    };
    let Ok(policy) = ApplicationLifecyclePolicy::new(64, 1, 4) else {
        return false;
    };
    let Ok(mut supervised) = SupervisedApplication::new(first, first_readiness, policy) else {
        return false;
    };

    if !poll_until_state(&mut supervised, ApplicationLifecycleState::RelaunchPending)
        || supervised.lifecycle().last_failure() != Some(ApplicationFailure::ReadinessTimeout)
        || supervised.lifecycle().relaunch_count() != 1
        || supervised.completion_count() != 1
    {
        return false;
    }

    let Some((second, second_readiness)) = launch_lifecycle_attempt(
        authorization,
        sources,
        b"/application-component-target lifecycle-running",
    ) else {
        return false;
    };
    if supervised
        .install_relaunch(second, second_readiness)
        .is_err()
        || !poll_until_state(&mut supervised, ApplicationLifecycleState::Running)
        || supervised.lifecycle().attempt() != 2
    {
        return false;
    }
    if supervised
        .request_termination(ApplicationTerminationReason::SessionTeardown)
        .is_err()
        || !poll_until_state(&mut supervised, ApplicationLifecycleState::Stopped)
    {
        return false;
    }
    supervised.lifecycle().termination_reason()
        == Some(ApplicationTerminationReason::SessionTeardown)
        && supervised.lifecycle().relaunch_count() == 1
        && supervised.completion_count() == 2
        && supervised
            .instance()
            .job
            .info()
            .is_ok_and(|info| info.size == 0)
        && ipc::job_try_wait(supervised.instance().job.as_raw()) == Err(ipc::Error::NO_CHILD)
}

fn authorized_root_application() -> Option<AuthorizedApplication> {
    let components = [
        InstalledApplicationComponent::new(
            ROOT_COMPONENT,
            b"/application-component-target",
            ApplicationProfileSet::DESKTOP,
            true,
        ),
        InstalledApplicationComponent::new(
            DESKTOP_CHILD_COMPONENT,
            b"/application-component-target",
            ApplicationProfileSet::DESKTOP_CHILD,
            false,
        ),
        InstalledApplicationComponent::new(
            WORKER_COMPONENT,
            b"/application-component-target",
            ApplicationProfileSet::WORKER,
            false,
        ),
    ];
    authorize_application_launch(
        PackageVerification {
            package: IDENTITY_PACKAGE,
            package_generation: IDENTITY_PACKAGE_GENERATION,
            application: IDENTITY_APPLICATION,
            publisher: IDENTITY_PUBLISHER,
            signing_lineage: IDENTITY_SIGNING_LINEAGE,
            trust_class: ApplicationTrustClass::Repository,
            system_application: false,
            components: &components,
        },
        ApplicationInstallation {
            installation: IDENTITY_INSTALLATION,
            package: IDENTITY_PACKAGE,
            package_generation: IDENTITY_PACKAGE_GENERATION,
            application: IDENTITY_APPLICATION,
            publisher: IDENTITY_PUBLISHER,
            signing_lineage: IDENTITY_SIGNING_LINEAGE,
            trust_class: ApplicationTrustClass::Repository,
            scope: ApplicationInstallScope::User,
            owner_user: IDENTITY_USER,
            system_application: false,
        },
        ApplicationLaunchSelection {
            component: ROOT_COMPONENT,
            user: IDENTITY_USER,
            session: IDENTITY_SESSION,
            profile: ApplicationProfile::Desktop,
        },
    )
    .ok()
}

fn launch_lifecycle_attempt(
    authorization: AuthorizedApplication,
    sources: ApplicationNamespaceSources,
    command: &[u8],
) -> Option<(ApplicationInstance<1>, OwnedHandle<Endpoint>)> {
    let readiness = OwnedHandle::<Endpoint>::create().ok()?;
    let namespace = ApplicationNamespace::new(authorization, sources).ok()?;
    let capabilities = [ApplicationCapability::new(
        readiness.as_raw(),
        Rights::SEND,
        CapabilityRole::READINESS,
    )];
    let launch = ApplicationLaunch::new(
        command,
        authorization,
        namespace,
        MANAGER_GENERATION,
        &capabilities,
    )
    .with_process_limit(2);
    let instance = spawn_application(launch).ok()?;
    Some((instance, readiness))
}

fn poll_until_state<const N: usize>(
    supervised: &mut SupervisedApplication<N>,
    expected: ApplicationLifecycleState,
) -> bool {
    for _ in 0..JOB_WAIT_YIELDS {
        match supervised.poll() {
            Ok(state) if state == expected => return true,
            Ok(ApplicationLifecycleState::Completed)
            | Ok(ApplicationLifecycleState::Stopped)
            | Ok(ApplicationLifecycleState::Failed)
            | Err(_) => return false,
            Ok(_) => {
                if syscall::yield_now().is_err() {
                    return false;
                }
            }
        }
    }
    false
}

#[derive(Default)]
struct ApplicationRouteProbe {
    accepted: usize,
    unavailable: usize,
    denied: usize,
    failed: bool,
}

impl ApplicationRouteProbe {
    fn complete(&self) -> bool {
        !self.failed && self.accepted == 1 && self.unavailable == 1 && self.denied == 1
    }
}

struct ExactApplicationCaller {
    process_id: u64,
}

impl Authorizer<u64> for ExactApplicationCaller {
    type Error = ();

    fn authorize(&mut self, caller: &u64, _key: RouteKey) -> Result<(), Self::Error> {
        if *caller == self.process_id {
            Ok(())
        } else {
            Err(())
        }
    }
}

fn pump_application_namespace(
    ingress: &mut ServiceNamespaceIngress<{ BASELINE_DESKTOP_ROUTES.len() }>,
    routes: &NativeRouteTable<CapabilityHandle, 1>,
    application_process_id: u64,
    probe: &mut ApplicationRouteProbe,
) {
    for _ in 0..8 {
        let event = match ingress.try_accept() {
            Ok(Some(event)) => event,
            Ok(None) => break,
            Err(_) => {
                probe.failed = true;
                break;
            }
        };
        match event {
            ServiceNamespaceEvent::Allowed(request) => {
                let key = request.key();
                let mut caller = ExactApplicationCaller {
                    process_id: application_process_id,
                };
                let Ok(authorized) = request.authorize(&mut caller) else {
                    probe.failed = true;
                    continue;
                };
                match authorized.resolve(routes) {
                    Ok(RouteReply::Accepted { generation })
                        if key == LOGGING_PRODUCER_ROUTE
                            && generation.get() == APPLICATION_ROUTE_GENERATION =>
                    {
                        probe.accepted += 1;
                    }
                    Ok(RouteReply::Failure(RouteFailure::Unavailable))
                        if key == DISPLAY_CLIENT_ROUTE =>
                    {
                        probe.unavailable += 1;
                    }
                    _ => probe.failed = true,
                }
            }
            ServiceNamespaceEvent::Denied(denial) => {
                let expected = RouteKey::new(LOGGING_SERVICE_ID, LOGGING_OBSERVER_ROLE);
                if denial.key == expected
                    && denial.sender_process_id == application_process_id
                    && denial.reply_error.is_none()
                {
                    probe.denied += 1;
                } else {
                    probe.failed = true;
                }
            }
        }
    }
}

fn receive_namespace_report(
    endpoint: &OwnedHandle<Endpoint>,
    expected_sender: u64,
    expected_marker: u8,
) -> bool {
    let mut report = [0_u8; 1];
    for _ in 0..JOB_WAIT_YIELDS {
        match endpoint.try_receive(&mut report) {
            Ok(message) => {
                return message.bytes == 1
                    && message.capability.is_none()
                    && message.sender_process_id == expected_sender
                    && report[0] == expected_marker;
            }
            Err(error) if error == ipc::Error::TRY_AGAIN => {
                if syscall::yield_now().is_err() {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    false
}

fn bounded_job_wait(handle: ipc::CapabilityHandle) -> Option<ipc::JobExit> {
    for _ in 0..JOB_WAIT_YIELDS {
        if ipc::wait_one(handle, Signals::READABLE, ipc::Deadline::INFINITE).is_err() {
            return None;
        }
        match ipc::job_try_wait(handle) {
            Ok(exit) => return Some(exit),
            Err(error) if error == ipc::Error::TRY_AGAIN => {
                if syscall::yield_now().is_err() {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    None
}
