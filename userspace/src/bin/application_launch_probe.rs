#![no_std]
#![no_main]

use userspace::{
    application_launch::{
        ApplicationCapability, ApplicationComponentCapability, ApplicationComponentLaunch,
        ApplicationComponentLaunchError, ApplicationIdentityError, ApplicationInstallScope,
        ApplicationInstallation, ApplicationLaunch, ApplicationLaunchSelection, ApplicationProfile,
        ApplicationProfileSet, ApplicationTrustClass, ComponentProfileSet,
        InstalledApplicationComponent, PackageVerification, authorize_application_launch,
        spawn_application,
    },
    handle::{Endpoint, OwnedHandle},
    ipc::{self, Rights, Signals},
    runtime_context::CapabilityRole,
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

userspace::entry!(rust_main);
userspace::panic_handler!();

fn rust_main(_initial_stack: *const usize) -> ! {
    syscall::exit(if application_component_probe() { 0 } else { 1 })
}

fn application_component_probe() -> bool {
    const ROOT_COMPONENT: u64 = 21;
    const DESKTOP_CHILD_COMPONENT: u64 = 22;
    const WORKER_COMPONENT: u64 = 23;
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
    let Ok(namespace) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let root_capabilities = [
        ApplicationCapability::new(status.as_raw(), Rights::SEND, CapabilityRole::READINESS)
            .delegable_to(ComponentProfileSet::ALL_REDUCED),
        ApplicationCapability::new(
            namespace.as_raw(),
            Rights::SEND,
            CapabilityRole::SERVICE_NAMESPACE,
        ),
    ];
    let root_launch = ApplicationLaunch::new(
        b"/application-component-target root",
        authorization,
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

    let forbidden_capabilities = [ApplicationComponentCapability::new(
        namespace.as_raw(),
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
        result &= received == 0b111;
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
