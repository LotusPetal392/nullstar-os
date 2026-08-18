//! Shared receiver for PID 1-managed service bootstrap streams.
//!
//! The bootstrap endpoint authenticates descriptive launch data by pinning PID 1
//! as its sender. Capabilities remain the only source of authority; generation
//! and identity fields describe the launch and are validated before service code
//! consumes any transferred handle.

use core::sync::atomic::{AtomicU8, Ordering};

use crate::{
    abi::{INIT_PROCESS_ID, limits},
    args::Args,
    environment::Environment,
    handle::{Endpoint, OwnedHandle},
    ipc::{self, ObjectKind, Rights},
    platform,
    process_start::{
        PROCESS_START_BOOTSTRAP_HANDLE, StartupDataError, StartupIdentity, StartupLaunch,
        StartupLaunchReason, StartupSectionId, StartupSectionPayload, StartupTransportError,
        ValidatedProcessStart, encode_startup_arguments, encode_startup_environment,
        receive_process_start_data, send_process_start_data,
    },
    runtime_context::{
        ProcessContext, ServiceProcess, StartupCapabilityPolicy, StartupMessage,
        StartupReceiveError, StartupRuntimeRole, ToolProcess, send_startup_message,
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

const MANAGED_TOOL_MODE_UNINITIALIZED: u8 = 0;
const MANAGED_TOOL_MODE_LEGACY: u8 = 1;
const MANAGED_TOOL_MODE_MANAGED: u8 = 2;
static MANAGED_TOOL_MODE: AtomicU8 = AtomicU8::new(MANAGED_TOOL_MODE_UNINITIALIZED);

/// One ordinary tool command and the compatibility environment inherited by it.
#[derive(Debug, Clone, Copy)]
pub struct ManagedToolCommand<'a> {
    command: &'a [u8],
    environment: &'a [(&'a [u8], &'a [u8])],
}

impl<'a> ManagedToolCommand<'a> {
    pub const fn new(command: &'a [u8], environment: &'a [(&'a [u8], &'a [u8])]) -> Self {
        Self {
            command,
            environment,
        }
    }

    pub const fn command(self) -> &'a [u8] {
        self.command
    }

    pub const fn environment(self) -> &'a [(&'a [u8], &'a [u8])] {
        self.environment
    }
}

/// Whether the current tool entered through the transitional legacy path or a
/// validated managed-start stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedToolStartMode {
    Uninitialized,
    Legacy,
    Managed,
}

/// Which trusted process queued an ordinary tool's startup stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedToolStartOrigin {
    Parent,
    Exec,
}

impl ManagedToolStartOrigin {
    const EXEC_FLAG: u16 = 1;

    const fn launch_flags(self) -> u16 {
        match self {
            Self::Parent => 0,
            Self::Exec => Self::EXEC_FLAG,
        }
    }
}

pub fn managed_tool_start_mode() -> ManagedToolStartMode {
    match MANAGED_TOOL_MODE.load(Ordering::Relaxed) {
        MANAGED_TOOL_MODE_LEGACY => ManagedToolStartMode::Legacy,
        MANAGED_TOOL_MODE_MANAGED => ManagedToolStartMode::Managed,
        _ => ManagedToolStartMode::Uninitialized,
    }
}

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

/// Stable provisional identity for an executable spelling in a managed tool
/// launch. This is descriptive only and never substitutes for executable or
/// namespace authority.
pub const fn numeric_executable_id(path: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut index = 0;
    while index < path.len() {
        hash ^= path[index] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        index += 1;
    }
    if hash == 0 { 1 } else { hash }
}

/// Sends one capability-empty managed startup stream for an ordinary tool.
///
/// The caller must keep the child behind a launch barrier until this returns.
/// Any error after the first message requires terminating that attempt because
/// the receiver may have observed only a prefix of the stream.
pub fn send_managed_tool_start(
    sender: u64,
    process_id: u64,
    command: ManagedToolCommand<'_>,
    origin: ManagedToolStartOrigin,
) -> Result<(), ManagedToolStartSendError> {
    if sender == 0 || process_id == 0 {
        return Err(ManagedToolStartSendError::InvalidDescription);
    }
    let mut raw_arguments = [&[][..]; limits::MAX_ARGUMENTS];
    let argument_count = split_command(command.command, &mut raw_arguments)
        .ok_or(ManagedToolStartSendError::InvalidDescription)?;
    let mut executable_bytes = [0; limits::MAX_ARGUMENT_BYTES];
    let executable_argument =
        canonical_executable_argument(raw_arguments[0], &mut executable_bytes)
            .ok_or(ManagedToolStartSendError::InvalidDescription)?;
    let mut arguments = [&[][..]; limits::MAX_ARGUMENTS];
    arguments[0] = executable_argument;
    arguments[1..argument_count].copy_from_slice(&raw_arguments[1..argument_count]);
    let executable = numeric_executable_id(arguments[0]);
    let capability_message = StartupMessage::<0>::new(StartupRuntimeRole::Tool, [])
        .map_err(ManagedToolStartSendError::Capabilities)?;
    send_startup_message(sender, &capability_message, &[])
        .map_err(ManagedToolStartSendError::CapabilityTransport)?;

    let mut argument_bytes = [0; limits::MAX_ARGUMENT_BYTES];
    let argument_length =
        encode_startup_arguments(&arguments[..argument_count], &mut argument_bytes)
            .map_err(ManagedToolStartSendError::Data)?;
    let mut environment_bytes = [0; limits::MAX_ENVIRONMENT_BYTES];
    let environment_length =
        encode_startup_environment(command.environment, &mut environment_bytes)
            .map_err(ManagedToolStartSendError::Data)?;
    let monotonic_start_ns =
        platform::monotonic_time_ns().map_err(|_| ManagedToolStartSendError::Clock)?;
    let identity = StartupIdentity {
        process: process_id,
        package: SYSTEM_PACKAGE_ID,
        package_generation: SYSTEM_PACKAGE_GENERATION,
        executable,
        application: 0,
        service: 0,
        component: executable,
        user: 0,
        session: 0,
    }
    .encode();
    let launch = StartupLaunch {
        launch: process_id,
        manager_generation: 0,
        namespace_profile: SYSTEM_NAMESPACE_PROFILE_ID,
        monotonic_start_ns,
        attempt: 1,
        reason: StartupLaunchReason::User,
        flags: origin.launch_flags(),
    }
    .encode();
    let sections = [
        StartupSectionPayload {
            id: StartupSectionId::IDENTITY,
            required: true,
            bytes: &identity,
        },
        StartupSectionPayload {
            id: StartupSectionId::ARGUMENTS,
            required: true,
            bytes: &argument_bytes[..argument_length],
        },
        StartupSectionPayload {
            id: StartupSectionId::ENVIRONMENT,
            required: true,
            bytes: &environment_bytes[..environment_length],
        },
        StartupSectionPayload {
            id: StartupSectionId::LAUNCH,
            required: true,
            bytes: &launch,
        },
    ];
    send_process_start_data(sender, &sections).map_err(ManagedToolStartSendError::Transport)
}

/// Validates a managed tool startup before application entry. A missing
/// bootstrap handle preserves the transitional kernel-direct launch path; a
/// present but malformed handle or stream always fails closed.
pub fn initialize_managed_tool_start(
    initial_stack: *const usize,
) -> Result<ManagedToolStartMode, ManagedToolStartError> {
    let mode = receive_managed_tool_start(initial_stack)?;
    MANAGED_TOOL_MODE.store(
        match mode {
            ManagedToolStartMode::Legacy => MANAGED_TOOL_MODE_LEGACY,
            ManagedToolStartMode::Managed => MANAGED_TOOL_MODE_MANAGED,
            ManagedToolStartMode::Uninitialized => MANAGED_TOOL_MODE_UNINITIALIZED,
        },
        Ordering::Relaxed,
    );
    Ok(mode)
}

fn receive_managed_tool_start(
    initial_stack: *const usize,
) -> Result<ManagedToolStartMode, ManagedToolStartError> {
    let bootstrap_info = match ipc::info(PROCESS_START_BOOTSTRAP_HANDLE) {
        Ok(info) => info,
        Err(_) => return Ok(ManagedToolStartMode::Legacy),
    };
    if bootstrap_info.kind != ObjectKind::Endpoint || bootstrap_info.rights != Rights::RECEIVE {
        return Err(ManagedToolStartError::InvalidBootstrap);
    }
    if (2..=limits::MAX_CAPABILITIES_PER_PROCESS as u64).any(|handle| ipc::info(handle).is_ok()) {
        return Err(ManagedToolStartError::UnexpectedInitialCapability);
    }
    let bootstrap = unsafe { OwnedHandle::<Endpoint>::from_raw(PROCESS_START_BOOTSTRAP_HANDLE) }
        .map_err(ManagedToolStartError::Bootstrap)?;
    let process_id = syscall::getpid().map_err(|_| ManagedToolStartError::ProcessIdentity)?;
    let parent_process_id =
        platform::getppid().map_err(|_| ManagedToolStartError::ProcessIdentity)?;
    let (context, expected_sender, origin) =
        receive_managed_tool_context(&bootstrap, parent_process_id, process_id)?;
    if !context.is_empty() {
        return Err(ManagedToolStartError::UnexpectedInitialCapability);
    }
    let data = receive_process_start_data::<4608, 4>(
        &bootstrap,
        expected_sender,
        &REQUIRED_SECTIONS,
        &REQUIRED_SECTIONS,
    )
    .map_err(ManagedToolStartError::Transport)?;
    let start = ValidatedProcessStart::from_data(&data).map_err(ManagedToolStartError::Data)?;
    bootstrap
        .close()
        .map_err(ManagedToolStartError::Bootstrap)?;

    let arguments = unsafe { Args::from_stack(initial_stack) };
    let environment = unsafe { Environment::from_stack(initial_stack) };
    let executable = arguments
        .get(0)
        .map(numeric_executable_id)
        .ok_or(ManagedToolStartError::InvalidDescription)?;
    if start.identity.process != process_id
        || start.identity.package != SYSTEM_PACKAGE_ID
        || start.identity.package_generation != SYSTEM_PACKAGE_GENERATION
        || start.identity.executable != executable
        || start.identity.application != 0
        || start.identity.service != 0
        || start.identity.component != executable
        || start.identity.user != 0
        || start.identity.session != 0
        || !arguments_match(arguments, start)
        || !environment_matches(environment, start)
        || start.launch.launch != process_id
        || start.launch.manager_generation != 0
        || start.launch.namespace_profile != SYSTEM_NAMESPACE_PROFILE_ID
        || start.launch.monotonic_start_ns == 0
        || start.launch.attempt != 1
        || start.launch.reason != StartupLaunchReason::User
        || start.launch.flags != origin.launch_flags()
    {
        return Err(ManagedToolStartError::InvalidDescription);
    }
    Ok(ManagedToolStartMode::Managed)
}

fn receive_managed_tool_context(
    bootstrap: &OwnedHandle<Endpoint>,
    parent_process_id: u64,
    process_id: u64,
) -> Result<(ProcessContext<ToolProcess, 0>, u64, ManagedToolStartOrigin), ManagedToolStartError> {
    let mut bytes = [0; limits::MAX_IPC_MESSAGE_BYTES];
    let message = loop {
        match bootstrap.try_receive_many::<0>(&mut bytes) {
            Ok(message) => break message,
            Err(error) if error.error() == ipc::Error::TRY_AGAIN => {
                syscall::yield_now().map_err(|_| {
                    ManagedToolStartError::Capabilities(StartupReceiveError::Ipc(ipc::Error::IO))
                })?;
            }
            Err(error) => {
                return Err(ManagedToolStartError::Capabilities(
                    StartupReceiveError::Ipc(error.error()),
                ));
            }
        }
    };
    let origin = if message.sender_process_id == parent_process_id {
        ManagedToolStartOrigin::Parent
    } else if message.sender_process_id == process_id {
        ManagedToolStartOrigin::Exec
    } else {
        return Err(ManagedToolStartError::Capabilities(
            StartupReceiveError::WrongSender,
        ));
    };
    let handles = message
        .capabilities
        .map(|capability| capability.map(|capability| capability.handle));
    let context =
        ProcessContext::<ToolProcess, 0>::from_startup(&bytes[..message.bytes], handles, &[])
            .map_err(|error| {
                ManagedToolStartError::Capabilities(StartupReceiveError::Startup(error.error()))
            })?;
    Ok((context, message.sender_process_id, origin))
}

fn split_command<'a>(
    command: &'a [u8],
    arguments: &mut [&'a [u8]; limits::MAX_ARGUMENTS],
) -> Option<usize> {
    let mut count = 0;
    for argument in command
        .split(u8::is_ascii_whitespace)
        .filter(|argument| !argument.is_empty())
    {
        if count == arguments.len() {
            return None;
        }
        arguments[count] = argument;
        count += 1;
    }
    (count != 0).then_some(count)
}

fn canonical_executable_argument<'a>(argument: &'a [u8], output: &'a mut [u8]) -> Option<&'a [u8]> {
    if argument.first() == Some(&b'/') {
        return Some(argument);
    }
    let length = argument.len().checked_add(1)?;
    if argument.is_empty() || length > output.len() {
        return None;
    }
    output[0] = b'/';
    output[1..length].copy_from_slice(argument);
    Some(&output[..length])
}

fn arguments_match(arguments: Args<'_>, start: ValidatedProcessStart<'_>) -> bool {
    start.arguments.len() == arguments.len()
        && arguments
            .iter()
            .enumerate()
            .all(|(index, argument)| start.arguments.get(index) == Some(argument))
}

fn environment_matches(environment: Environment<'_>, start: ValidatedProcessStart<'_>) -> bool {
    start.environment.len() == environment.len()
        && environment.iter().all(|entry| {
            let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
                return false;
            };
            start.environment.find(&entry[..separator]) == Some(&entry[separator + 1..])
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedToolStartSendError {
    InvalidDescription,
    Capabilities(crate::runtime_context::StartupError),
    CapabilityTransport(crate::runtime_context::StartupSendError),
    Data(StartupDataError),
    Clock,
    Transport(StartupTransportError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedToolStartError {
    UnexpectedInitialCapability,
    Bootstrap(ipc::Error),
    InvalidBootstrap,
    Capabilities(StartupReceiveError),
    Transport(StartupTransportError),
    Data(StartupDataError),
    ProcessIdentity,
    InvalidDescription,
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
    use super::{
        ManagedToolStartOrigin, canonical_executable_argument, numeric_executable_id,
        numeric_service_id, split_command,
    };
    use crate::abi::limits;

    #[test]
    fn numeric_service_identity_uses_canonical_uuid_prefix_order() {
        assert_eq!(
            numeric_service_id([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,]),
            0x0807_0605_0403_0201
        );
    }

    #[test]
    fn executable_identity_is_stable_nonzero_and_spelling_sensitive() {
        assert_eq!(numeric_executable_id(b"cat"), numeric_executable_id(b"cat"));
        assert_ne!(numeric_executable_id(b"cat"), 0);
        assert_ne!(
            numeric_executable_id(b"cat"),
            numeric_executable_id(b"/cat")
        );
    }

    #[test]
    fn exec_handoffs_have_a_distinct_authenticated_launch_flag() {
        assert_eq!(ManagedToolStartOrigin::Parent.launch_flags(), 0);
        assert_eq!(ManagedToolStartOrigin::Exec.launch_flags(), 1);
    }

    #[test]
    fn managed_command_split_matches_the_legacy_loader_contract() {
        let mut arguments = [&[][..]; limits::MAX_ARGUMENTS];
        let count = split_command(b"  runtime-probe   managed-startup ", &mut arguments).unwrap();
        assert_eq!(
            &arguments[..count],
            &[&b"runtime-probe"[..], &b"managed-startup"[..]]
        );
        assert_eq!(split_command(b" \t\n", &mut arguments), None);
    }

    #[test]
    fn relative_executable_arguments_match_kernel_canonicalization() {
        let mut output = [0; 16];
        assert_eq!(
            canonical_executable_argument(b"cat", &mut output),
            Some(&b"/cat"[..])
        );
        assert_eq!(
            canonical_executable_argument(b"/cat", &mut output),
            Some(&b"/cat"[..])
        );
    }
}
