#![no_std]
#![no_main]

use userspace::{
    abi::INIT_PROCESS_ID,
    args::Args,
    definition_service_probe,
    handle::{Endpoint, OwnedHandle},
    ipc::{self, ObjectKind, Rights},
    platform,
    process_start::{
        PROCESS_START_BOOTSTRAP_SLOT, StartupLaunchReason, StartupSectionId, ValidatedProcessStart,
        receive_process_start_data,
    },
    runtime_context::{CapabilityRole, ProcessContext, ServiceProcess, StartupCapabilityPolicy},
    service_route::receive_service_generation,
    syscall,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    if arguments.len() != 1 || arguments.get(0) != Some(definition_service_probe::EXECUTABLE_PATH) {
        syscall::exit(2);
    }
    if [2, 3, 4, 5, 6]
        .into_iter()
        .any(|slot| ipc::info_at_slot(slot).is_ok())
    {
        syscall::exit(3);
    }
    let bootstrap =
        match unsafe { OwnedHandle::<Endpoint>::from_slot(PROCESS_START_BOOTSTRAP_SLOT) } {
            Ok(handle)
                if handle.info().is_ok_and(|info| {
                    info.kind == ObjectKind::Endpoint && info.rights == Rights::RECEIVE
                }) =>
            {
                handle
            }
            _ => syscall::exit(3),
        };
    let policies = [
        StartupCapabilityPolicy {
            role: CapabilityRole::SERVICE_GENERATION,
            kind: ObjectKind::Endpoint,
            minimum_rights: Rights::RECEIVE,
            maximum_rights: Rights::RECEIVE,
            required: true,
        },
        StartupCapabilityPolicy {
            role: CapabilityRole::READINESS,
            kind: ObjectKind::Endpoint,
            minimum_rights: Rights::SEND,
            maximum_rights: Rights::SEND,
            required: true,
        },
    ];
    let mut context = match ProcessContext::<ServiceProcess, 2>::receive_startup(
        &bootstrap,
        INIT_PROCESS_ID,
        &policies,
    ) {
        Ok(context) => context,
        Err(_) => syscall::exit(4),
    };
    const SECTIONS: [StartupSectionId; 4] = [
        StartupSectionId::IDENTITY,
        StartupSectionId::ARGUMENTS,
        StartupSectionId::ENVIRONMENT,
        StartupSectionId::LAUNCH,
    ];
    let data = match receive_process_start_data::<4608, 4>(
        &bootstrap,
        INIT_PROCESS_ID,
        &SECTIONS,
        &SECTIONS,
    ) {
        Ok(data) => data,
        Err(_) => syscall::exit(4),
    };
    let start = match ValidatedProcessStart::from_data(&data) {
        Ok(start) => start,
        Err(_) => syscall::exit(4),
    };
    if bootstrap.close().is_err() {
        syscall::exit(4);
    }
    let generation_endpoint =
        match context.take::<Endpoint>(CapabilityRole::SERVICE_GENERATION, Rights::RECEIVE) {
            Ok(handle) => handle,
            Err(_) => syscall::exit(4),
        };
    let readiness = match context.take::<Endpoint>(CapabilityRole::READINESS, Rights::SEND) {
        Ok(handle) => handle,
        Err(_) => syscall::exit(4),
    };
    let generation =
        match receive_service_generation(generation_endpoint.into_raw(), INIT_PROCESS_ID) {
            Ok(generation) => generation,
            Err(_) => syscall::exit(4),
        };
    let process_id = match syscall::getpid() {
        Ok(process_id) => process_id,
        Err(_) => syscall::exit(4),
    };
    let expected_reason = if generation.get() == 1 {
        StartupLaunchReason::Activation
    } else {
        StartupLaunchReason::Restart
    };
    if start.identity.process != process_id
        || start.identity.package != definition_service_probe::SYSTEM_PACKAGE_ID
        || start.identity.package_generation != generation.get()
        || start.identity.executable != definition_service_probe::EXECUTABLE_ID
        || start.identity.application != 0
        || start.identity.service != definition_service_probe::SERVICE_NUMERIC_ID
        || start.identity.component != definition_service_probe::COMPONENT_ID
        || start.identity.user != 0
        || start.identity.session != 0
        || start.arguments.len() != 2
        || start.arguments.get(0) != Some(definition_service_probe::EXECUTABLE_PATH)
        || start.arguments.get(1) != Some(definition_service_probe::MANAGED_ARGUMENT)
        || !start.environment.is_empty()
        || start.launch.launch != generation.get()
        || start.launch.manager_generation != generation.get()
        || start.launch.namespace_profile != definition_service_probe::NAMESPACE_PROFILE_ID
        || start.launch.attempt != u32::try_from(generation.get()).unwrap_or(u32::MAX)
        || start.launch.reason != expected_reason
        || start.launch.flags != 0
    {
        syscall::exit(4);
    }
    if generation.get() == 1 {
        let group_ready = match syscall::pipe_pair() {
            Ok(pair) => pair,
            Err(_) => syscall::exit(6),
        };
        match syscall::fork() {
            Ok(0) => {
                let _ = syscall::close(group_ready.reader);
                if platform::set_process_group(0, 0).is_err()
                    || syscall::write_all(group_ready.writer, &[1]).is_err()
                    || syscall::close(group_ready.writer).is_err()
                {
                    syscall::exit(7);
                }
                loop {
                    if syscall::yield_now().is_err() {
                        syscall::exit(8);
                    }
                }
            }
            Ok(_) => {
                let _ = syscall::close(group_ready.writer);
                let mut ready = [0_u8; 1];
                let escaped = loop {
                    match syscall::read(group_ready.reader, &mut ready) {
                        Ok(1) => break ready[0] == 1,
                        Ok(_) => break false,
                        Err(error) if error == syscall::Errno::INTERRUPTED => {}
                        Err(_) => break false,
                    }
                };
                if syscall::close(group_ready.reader).is_err() || !escaped {
                    syscall::exit(9);
                }
            }
            Err(_) => {
                let _ = syscall::close(group_ready.writer);
                let _ = syscall::close(group_ready.reader);
                syscall::exit(6);
            }
        }
        let _ = syscall::write_all(
            syscall::STDOUT,
            b"definition-service-probe: intentional first-generation failure\n",
        );
        syscall::exit(75);
    }
    if readiness
        .send(definition_service_probe::READY_MESSAGE)
        .is_err()
        || readiness.close().is_err()
        || !context.is_empty()
        || syscall::write_all(
            syscall::STDOUT,
            b"definition-service-probe: definition-backed generation ready\n",
        )
        .is_err()
    {
        syscall::exit(5);
    }

    loop {
        if syscall::yield_now().is_err() {
            syscall::exit(6);
        }
    }
}
