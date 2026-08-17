//! Shared receiver for PID 1-managed service bootstrap streams.
//!
//! The bootstrap endpoint authenticates descriptive launch data by pinning PID 1
//! as its sender. Capabilities remain the only source of authority; generation
//! and identity fields describe the launch and are validated before service code
//! consumes any transferred handle.

use crate::{
    abi::{INIT_PROCESS_ID, limits},
    args::Args,
    handle::{Endpoint, OwnedHandle},
    ipc::{self, ObjectKind, Rights},
    process_start::{
        PROCESS_START_BOOTSTRAP_HANDLE, StartupDataError, StartupLaunchReason, StartupSectionId,
        StartupTransportError, ValidatedProcessStart, receive_process_start_data,
    },
    runtime_context::{
        ProcessContext, ServiceProcess, StartupCapabilityPolicy, StartupReceiveError,
    },
    syscall,
};

pub const SYSTEM_PACKAGE_ID: u64 = 1;
pub const SYSTEM_PACKAGE_GENERATION: u64 = 1;
pub const SYSTEM_NAMESPACE_PROFILE_ID: u64 = 1;

const REQUIRED_SECTIONS: [StartupSectionId; 4] = [
    StartupSectionId::IDENTITY,
    StartupSectionId::ARGUMENTS,
    StartupSectionId::ENVIRONMENT,
    StartupSectionId::LAUNCH,
];

/// Expected descriptive identity for one statically managed system service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedServiceIdentity {
    pub executable: u64,
    pub service: u64,
    pub component: u64,
}

impl ManagedServiceIdentity {
    pub const fn new(executable: u64, service: u64, component: u64) -> Self {
        Self {
            executable,
            service,
            component,
        }
    }
}

/// Validated authority and manager generation supplied to one service process.
#[derive(Debug)]
pub struct ManagedServiceStart<const N: usize> {
    pub context: ProcessContext<ServiceProcess, N>,
    pub generation: u64,
}

/// Receives and validates the complete managed-service startup stream.
pub fn receive_managed_service_start<const N: usize>(
    initial_arguments: Args<'_>,
    policies: &[StartupCapabilityPolicy],
    expected_identity: ManagedServiceIdentity,
) -> Result<ManagedServiceStart<N>, ManagedServiceStartError> {
    if (2..=limits::MAX_CAPABILITIES_PER_PROCESS as u64).any(|handle| ipc::info(handle).is_ok()) {
        return Err(ManagedServiceStartError::UnexpectedInitialCapability);
    }
    let bootstrap = unsafe { OwnedHandle::<Endpoint>::from_raw(PROCESS_START_BOOTSTRAP_HANDLE) }
        .map_err(ManagedServiceStartError::Bootstrap)?;
    if !bootstrap
        .info()
        .is_ok_and(|info| info.kind == ObjectKind::Endpoint && info.rights == Rights::RECEIVE)
    {
        return Err(ManagedServiceStartError::InvalidBootstrap);
    }
    let context =
        ProcessContext::<ServiceProcess, N>::receive_startup(&bootstrap, INIT_PROCESS_ID, policies)
            .map_err(ManagedServiceStartError::Capabilities)?;
    let data = receive_process_start_data::<4608, 4>(
        &bootstrap,
        INIT_PROCESS_ID,
        &REQUIRED_SECTIONS,
        &REQUIRED_SECTIONS,
    )
    .map_err(ManagedServiceStartError::Transport)?;
    let start = ValidatedProcessStart::from_data(&data).map_err(ManagedServiceStartError::Data)?;
    bootstrap
        .close()
        .map_err(ManagedServiceStartError::Bootstrap)?;
    let process_id = syscall::getpid().map_err(|_| ManagedServiceStartError::ProcessIdentity)?;
    let generation = start.launch.manager_generation;
    let expected_reason = if generation == 1 {
        StartupLaunchReason::Activation
    } else {
        StartupLaunchReason::Restart
    };
    if generation == 0
        || start.identity.process != process_id
        || start.identity.package != SYSTEM_PACKAGE_ID
        || start.identity.package_generation != SYSTEM_PACKAGE_GENERATION
        || start.identity.executable != expected_identity.executable
        || start.identity.application != 0
        || start.identity.service != expected_identity.service
        || start.identity.component != expected_identity.component
        || start.identity.user != 0
        || start.identity.session != 0
        || start.arguments.len() != initial_arguments.len()
        || initial_arguments
            .iter()
            .enumerate()
            .any(|(index, argument)| start.arguments.get(index) != Some(argument))
        || !start.environment.is_empty()
        || start.launch.launch != generation
        || start.launch.namespace_profile != SYSTEM_NAMESPACE_PROFILE_ID
        || start.launch.attempt == 0
        || start.launch.reason != expected_reason
        || start.launch.flags != 0
    {
        return Err(ManagedServiceStartError::InvalidDescription);
    }
    Ok(ManagedServiceStart {
        context,
        generation,
    })
}

/// Stable numeric identity used by the provisional process-start identity section.
pub const fn numeric_service_id(bytes: [u8; 16]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedServiceStartError {
    UnexpectedInitialCapability,
    Bootstrap(ipc::Error),
    InvalidBootstrap,
    Capabilities(StartupReceiveError),
    Transport(StartupTransportError),
    Data(StartupDataError),
    ProcessIdentity,
    InvalidDescription,
}

#[cfg(test)]
mod tests {
    use super::numeric_service_id;

    #[test]
    fn numeric_service_identity_uses_canonical_uuid_prefix_order() {
        assert_eq!(
            numeric_service_id([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,]),
            0x0807_0605_0403_0201
        );
    }
}
