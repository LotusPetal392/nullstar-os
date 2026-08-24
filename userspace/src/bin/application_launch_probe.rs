#![no_std]
#![no_main]

use userspace::{
    application_launch::{
        ApplicationCapability, ApplicationComponentCapability, ApplicationComponentLaunch,
        ApplicationComponentLaunchError, ApplicationIdentity, ApplicationLaunch,
        ApplicationProfile, ComponentProfileSet, spawn_application,
    },
    handle::{Endpoint, OwnedHandle},
    ipc::{self, Rights, Signals},
    runtime_context::CapabilityRole,
    syscall,
};

const JOB_WAIT_YIELDS: usize = 4096;

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
        ApplicationIdentity {
            package: 11,
            package_generation: 12,
            application: 13,
            component: ROOT_COMPONENT,
            user: 14,
            session: 15,
        },
        ApplicationProfile::Desktop,
        16,
        &root_capabilities,
    )
    .with_process_limit(4);
    let Ok(application) = spawn_application(root_launch) else {
        return false;
    };
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
    let mut result = forbidden
        == Err(ApplicationComponentLaunchError::AuthorityNotDelegable(
            CapabilityRole::SERVICE_NAMESPACE,
        ))
        && escalating
            == Err(ApplicationComponentLaunchError::AuthorityEscalation(
                CapabilityRole::READINESS,
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
