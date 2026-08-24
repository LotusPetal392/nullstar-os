//! Native application-launch foundation.
//!
//! This module implements the native root-launch and reduced-component
//! foundation behind the application-sandbox architecture. It deliberately
//! composes existing mechanisms instead of adding a second authority model:
//!
//! - `fork` creates the direct child;
//! - the child scrubs its inherited descriptor and capability tables before it
//!   acknowledges the parent;
//! - the parent assigns the blocked child to a fresh job before release;
//! - exactly one receive-only bootstrap endpoint is installed in slot 1;
//! - application capabilities are carried inside the typed `NSPC` startup
//!   envelope and are rights-reduced by receiver policy;
//! - `NSPD` data carries trusted descriptive application identity and profile;
//! - reduced components reuse the root job and derive identity from the root;
//! - component capabilities require explicit profile opt-in and monotonic
//!   rights reduction.
//!
//! Package signature verification, persistent grants, private directory
//! construction, service-namespace construction, and portal policy remain
//! application-manager responsibilities above this layer.

use crate::{
    abi::{limits, signal},
    args::Args,
    handle::{AnyObject, Endpoint, Job, OwnedHandle},
    ipc::{self, ObjectKind, Rights, Transfer},
    platform,
    process_start::{
        PROCESS_START_BOOTSTRAP_SLOT, StartupDataError, StartupIdentity, StartupLaunch,
        StartupLaunchReason, StartupSectionId, StartupSectionPayload, StartupTransportError,
        ValidatedProcessStart, encode_startup_arguments, encode_startup_environment,
        receive_process_start_data, send_process_start_data,
    },
    runtime_context::{
        ApplicationProcess, CapabilityRole, ProcessContext, StartupCapabilityPolicy,
        StartupMessage, StartupReceiveError, StartupResource, StartupRuntimeRole, StartupSendError,
        send_startup_message,
    },
    syscall::{self, DescriptorFlags, FileDescriptor, PipePair, ProcessId},
};

/// Provisional namespace/profile identifiers for native application components.
///
/// These values are descriptive launch metadata, not authority. They are kept
/// separate from the system-service namespace profile (`1`).
pub const DESKTOP_NAMESPACE_PROFILE_ID: u64 = 2;
pub const DESKTOP_CHILD_NAMESPACE_PROFILE_ID: u64 = 3;
pub const WORKER_NAMESPACE_PROFILE_ID: u64 = 4;

const REQUIRED_SECTIONS: [StartupSectionId; 4] = [
    StartupSectionId::IDENTITY,
    StartupSectionId::ARGUMENTS,
    StartupSectionId::ENVIRONMENT,
    StartupSectionId::LAUNCH,
];
const ISOLATION_ACK: u8 = 1;
const DEFAULT_PROCESS_LIMIT: usize = 16;

/// Native application component class selected by the trusted launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationProfile {
    Desktop,
    DesktopChild,
    Worker,
}

impl ApplicationProfile {
    pub const fn namespace_profile_id(self) -> u64 {
        match self {
            Self::Desktop => DESKTOP_NAMESPACE_PROFILE_ID,
            Self::DesktopChild => DESKTOP_CHILD_NAMESPACE_PROFILE_ID,
            Self::Worker => WORKER_NAMESPACE_PROFILE_ID,
        }
    }

    pub const fn from_namespace_profile_id(value: u64) -> Option<Self> {
        match value {
            DESKTOP_NAMESPACE_PROFILE_ID => Some(Self::Desktop),
            DESKTOP_CHILD_NAMESPACE_PROFILE_ID => Some(Self::DesktopChild),
            WORKER_NAMESPACE_PROFILE_ID => Some(Self::Worker),
            _ => None,
        }
    }

    pub const fn is_reduced_component(self) -> bool {
        matches!(self, Self::DesktopChild | Self::Worker)
    }
}

/// Component profiles to which one root capability may be delegated.
///
/// The empty default is fail-closed: a capability supplied to the desktop root
/// is not eligible for component startup unless the trusted manager opts it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentProfileSet(u8);

impl ComponentProfileSet {
    pub const EMPTY: Self = Self(0);
    pub const DESKTOP_CHILD: Self = Self(1 << 0);
    pub const WORKER: Self = Self(1 << 1);
    pub const ALL_REDUCED: Self = Self(Self::DESKTOP_CHILD.0 | Self::WORKER.0);

    pub const fn contains(self, profile: ApplicationProfile) -> bool {
        let required = match profile {
            ApplicationProfile::Desktop => return false,
            ApplicationProfile::DesktopChild => Self::DESKTOP_CHILD.0,
            ApplicationProfile::Worker => Self::WORKER.0,
        };
        self.0 & required == required
    }

    const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Trusted descriptive identity attached to one application process launch.
///
/// The values identify the package/application/component selected by the
/// application manager. They never grant authority by themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationIdentity {
    pub package: u64,
    pub package_generation: u64,
    pub application: u64,
    pub component: u64,
    pub user: u64,
    pub session: u64,
}

impl ApplicationIdentity {
    pub const fn is_valid(self) -> bool {
        self.package != 0
            && self.package_generation != 0
            && self.application != 0
            && self.component != 0
            && self.user != 0
            && self.session != 0
    }
}

/// One capability that the trusted launcher may place in the application
/// startup context.
#[derive(Debug, Clone, Copy)]
pub struct ApplicationCapability {
    pub source_handle: u64,
    pub rights: Rights,
    pub role: CapabilityRole,
    component_profiles: ComponentProfileSet,
}

impl ApplicationCapability {
    pub const fn new(source_handle: u64, rights: Rights, role: CapabilityRole) -> Self {
        Self {
            source_handle,
            rights,
            role,
            component_profiles: ComponentProfileSet::EMPTY,
        }
    }

    /// Allows reduced components in `profiles` to request this exact source
    /// capability with equal or narrower rights.
    pub const fn delegable_to(mut self, profiles: ComponentProfileSet) -> Self {
        self.component_profiles = profiles;
        self
    }

    pub const fn component_profiles(self) -> ComponentProfileSet {
        self.component_profiles
    }
}

/// One explicit capability requested for a reduced component.
#[derive(Debug, Clone, Copy)]
pub struct ApplicationComponentCapability {
    pub source_handle: u64,
    pub rights: Rights,
    pub role: CapabilityRole,
}

impl ApplicationComponentCapability {
    pub const fn new(source_handle: u64, rights: Rights, role: CapabilityRole) -> Self {
        Self {
            source_handle,
            rights,
            role,
        }
    }

    const fn as_application_capability(self) -> ApplicationCapability {
        ApplicationCapability::new(self.source_handle, self.rights, self.role)
    }
}

/// Immutable inputs for one mediated application launch.
#[derive(Debug, Clone, Copy)]
pub struct ApplicationLaunch<'a, const N: usize> {
    /// Absolute canonical executable followed by whitespace-separated arguments.
    pub command: &'a [u8],
    pub identity: ApplicationIdentity,
    pub profile: ApplicationProfile,
    pub manager_generation: u64,
    pub process_limit: usize,
    pub capabilities: &'a [ApplicationCapability; N],
}

/// Immutable inputs for one reduced component launched inside an existing
/// application job and identity boundary.
#[derive(Debug, Clone, Copy)]
pub struct ApplicationComponentLaunch<'a, const N: usize> {
    pub command: &'a [u8],
    pub component: u64,
    pub profile: ApplicationProfile,
    pub capabilities: &'a [ApplicationComponentCapability; N],
}

impl<'a, const N: usize> ApplicationComponentLaunch<'a, N> {
    pub const fn new(
        command: &'a [u8],
        component: u64,
        profile: ApplicationProfile,
        capabilities: &'a [ApplicationComponentCapability; N],
    ) -> Self {
        Self {
            command,
            component,
            profile,
            capabilities,
        }
    }
}

impl<'a, const N: usize> ApplicationLaunch<'a, N> {
    pub const fn new(
        command: &'a [u8],
        identity: ApplicationIdentity,
        profile: ApplicationProfile,
        manager_generation: u64,
        capabilities: &'a [ApplicationCapability; N],
    ) -> Self {
        Self {
            command,
            identity,
            profile,
            manager_generation,
            process_limit: DEFAULT_PROCESS_LIMIT,
            capabilities,
        }
    }

    pub const fn with_process_limit(mut self, process_limit: usize) -> Self {
        self.process_limit = process_limit;
        self
    }
}

/// A successfully released application process, its job authority, and
/// rights-bounded duplicates of component-delegable sources retained by its
/// manager.
#[derive(Debug)]
pub struct ApplicationInstance<const N: usize> {
    pub process_id: ProcessId,
    pub job: OwnedHandle<Job>,
    identity: ApplicationIdentity,
    profile: ApplicationProfile,
    manager_generation: u64,
    authority_ceiling: [ApplicationCapability; N],
    authority_sources: [Option<OwnedHandle<AnyObject>>; N],
}

/// A reduced component released into its application's existing job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationComponentInstance {
    pub process_id: ProcessId,
    pub identity: ApplicationIdentity,
    pub profile: ApplicationProfile,
}

/// Validated application startup state delivered before application entry.
#[derive(Debug)]
pub struct ApplicationStart<const N: usize> {
    pub context: ProcessContext<ApplicationProcess, N>,
    pub identity: ApplicationIdentity,
    pub profile: ApplicationProfile,
    pub manager_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationLaunchError {
    InvalidDescription,
    Descriptor(syscall::Errno),
    Capability(ipc::Error),
    Startup(ApplicationStartupSendError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationComponentLaunchError {
    InvalidDescription,
    RootProfileRequired,
    AuthorityNotDelegable(CapabilityRole),
    AuthorityEscalation(CapabilityRole),
    AuthorityNotReduced,
    Descriptor(syscall::Errno),
    Capability(ipc::Error),
    Startup(ApplicationStartupSendError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationStartupSendError {
    InvalidDescription,
    CapabilityDuplicate(ipc::Error),
    CapabilityMessage(StartupSendError),
    Data(StartupDataError),
    Transport(StartupTransportError),
    Clock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationStartError {
    InvalidBootstrap,
    UnexpectedInitialCapability,
    Bootstrap(ipc::Error),
    Capabilities(StartupReceiveError),
    Transport(StartupTransportError),
    Data(StartupDataError),
    InvalidDescription,
    ProcessIdentity,
}

/// Launches one application component behind descriptor/capability isolation and
/// a fresh job boundary.
///
/// The returned job handle remains manager-owned. Dropping it does not kill the
/// application because jobs are kernel-rooted while members remain; managers
/// that need lifecycle control must retain the handle and terminate/drain it
/// explicitly.
pub fn spawn_application<const N: usize>(
    launch: ApplicationLaunch<'_, N>,
) -> Result<ApplicationInstance<N>, ApplicationLaunchError> {
    validate_launch(&launch).map_err(|_| ApplicationLaunchError::InvalidDescription)?;
    let authority_sources = retain_component_sources(launch.profile, launch.capabilities)?;

    let job = OwnedHandle::<Job>::create().map_err(ApplicationLaunchError::Capability)?;
    ipc::job_set_process_limit(job.as_raw(), launch.process_limit)
        .map_err(ApplicationLaunchError::Capability)?;

    let child = spawn_application_in_job(launch, job.as_raw())?;

    Ok(ApplicationInstance {
        process_id: child,
        job,
        identity: launch.identity,
        profile: launch.profile,
        manager_generation: launch.manager_generation,
        authority_ceiling: *launch.capabilities,
        authority_sources,
    })
}

impl<const N: usize> ApplicationInstance<N> {
    pub const fn identity(&self) -> ApplicationIdentity {
        self.identity
    }

    pub const fn profile(&self) -> ApplicationProfile {
        self.profile
    }

    pub const fn manager_generation(&self) -> u64 {
        self.manager_generation
    }

    /// Launches a reduced child component inside this application's existing
    /// job, identity, user, session, and manager-generation boundary.
    ///
    /// Every requested capability must name the exact manager-owned source and
    /// role retained in the root launch ceiling, be opted into the selected
    /// component profile, and request no broader rights. The complete root
    /// authority set cannot be reproduced unchanged.
    pub fn spawn_component<const M: usize>(
        &self,
        launch: ApplicationComponentLaunch<'_, M>,
    ) -> Result<ApplicationComponentInstance, ApplicationComponentLaunchError> {
        if self.profile != ApplicationProfile::Desktop {
            return Err(ApplicationComponentLaunchError::RootProfileRequired);
        }
        if launch.component == self.identity.component {
            return Err(ApplicationComponentLaunchError::InvalidDescription);
        }
        validate_component_launch(&self.authority_ceiling, &launch)?;
        let identity = ApplicationIdentity {
            component: launch.component,
            ..self.identity
        };
        let mut capabilities = launch
            .capabilities
            .map(ApplicationComponentCapability::as_application_capability);
        for capability in &mut capabilities {
            let Some(index) = self
                .authority_ceiling
                .iter()
                .position(|allowed| allowed.role == capability.role)
            else {
                return Err(ApplicationComponentLaunchError::InvalidDescription);
            };
            let Some(source) = self.authority_sources[index].as_ref() else {
                return Err(ApplicationComponentLaunchError::InvalidDescription);
            };
            capability.source_handle = source.as_raw();
        }
        let application_launch = ApplicationLaunch::new(
            launch.command,
            identity,
            launch.profile,
            self.manager_generation,
            &capabilities,
        );
        let process_id =
            spawn_application_in_job(application_launch, self.job.as_raw()).map_err(|error| {
                match error {
                    ApplicationLaunchError::InvalidDescription => {
                        ApplicationComponentLaunchError::InvalidDescription
                    }
                    ApplicationLaunchError::Descriptor(error) => {
                        ApplicationComponentLaunchError::Descriptor(error)
                    }
                    ApplicationLaunchError::Capability(error) => {
                        ApplicationComponentLaunchError::Capability(error)
                    }
                    ApplicationLaunchError::Startup(error) => {
                        ApplicationComponentLaunchError::Startup(error)
                    }
                }
            })?;
        Ok(ApplicationComponentInstance {
            process_id,
            identity,
            profile: launch.profile,
        })
    }
}

fn retain_component_sources<const N: usize>(
    profile: ApplicationProfile,
    capabilities: &[ApplicationCapability; N],
) -> Result<[Option<OwnedHandle<AnyObject>>; N], ApplicationLaunchError> {
    let mut sources = [const { None }; N];
    if profile != ApplicationProfile::Desktop {
        return Ok(sources);
    }
    for (index, capability) in capabilities.iter().enumerate() {
        if capability.component_profiles.is_empty() {
            continue;
        }
        let retained = ipc::duplicate(
            capability.source_handle,
            capability.rights | Rights::DUPLICATE | Rights::TRANSFER,
        )
        .map_err(ApplicationLaunchError::Capability)?;
        sources[index] = Some(
            // SAFETY: `duplicate` returned a new, manager-owned untyped handle,
            // which this array adopts exactly once and closes on drop.
            unsafe { OwnedHandle::<AnyObject>::from_raw(retained) }
                .map_err(ApplicationLaunchError::Capability)?,
        );
    }
    Ok(sources)
}

/// Receives and validates a mandatory native-application startup stream.
///
/// Before adoption, bootstrap slot 1 must be the only live capability. The
/// trusted sender is the direct parent that mediated this application launch.
///
/// # Safety
///
/// `initial_stack` must identify the untouched initial stack layout constructed
/// by the process loader and remain mapped while the returned startup state is
/// validated.
pub unsafe fn receive_application_start<const N: usize>(
    initial_stack: *const usize,
    policies: &[StartupCapabilityPolicy],
) -> Result<ApplicationStart<N>, ApplicationStartError> {
    if (2..=limits::MAX_CAPABILITIES_PER_PROCESS as u64).any(|slot| ipc::info_at_slot(slot).is_ok())
    {
        return Err(ApplicationStartError::UnexpectedInitialCapability);
    }
    let bootstrap = unsafe { OwnedHandle::<Endpoint>::from_slot(PROCESS_START_BOOTSTRAP_SLOT) }
        .map_err(ApplicationStartError::Bootstrap)?;
    if !bootstrap
        .info()
        .is_ok_and(|info| info.kind == ObjectKind::Endpoint && info.rights == Rights::RECEIVE)
    {
        return Err(ApplicationStartError::InvalidBootstrap);
    }
    let process_id = syscall::getpid().map_err(|_| ApplicationStartError::ProcessIdentity)?;
    let parent = platform::getppid().map_err(|_| ApplicationStartError::ProcessIdentity)?;
    if parent == 0 {
        return Err(ApplicationStartError::ProcessIdentity);
    }
    let context =
        ProcessContext::<ApplicationProcess, N>::receive_startup(&bootstrap, parent, policies)
            .map_err(ApplicationStartError::Capabilities)?;
    let data = receive_process_start_data::<4608, 4>(
        &bootstrap,
        parent,
        &REQUIRED_SECTIONS,
        &REQUIRED_SECTIONS,
    )
    .map_err(ApplicationStartError::Transport)?;
    let start = ValidatedProcessStart::from_data(&data).map_err(ApplicationStartError::Data)?;
    bootstrap
        .close()
        .map_err(ApplicationStartError::Bootstrap)?;

    let profile = ApplicationProfile::from_namespace_profile_id(start.launch.namespace_profile)
        .ok_or(ApplicationStartError::InvalidDescription)?;
    let identity = ApplicationIdentity {
        package: start.identity.package,
        package_generation: start.identity.package_generation,
        application: start.identity.application,
        component: start.identity.component,
        user: start.identity.user,
        session: start.identity.session,
    };
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let executable = arguments
        .get(0)
        .ok_or(ApplicationStartError::InvalidDescription)?;
    if !identity.is_valid()
        || start.identity.process != process_id
        || start.identity.executable != crate::managed_startup::numeric_executable_id(executable)
        || start.identity.service != 0
        || start.launch.launch != process_id
        || start.launch.manager_generation == 0
        || start.launch.monotonic_start_ns == 0
        || start.launch.attempt != 1
        || start.launch.reason != StartupLaunchReason::User
        || start.launch.flags != 0
        || !arguments_match(arguments, start)
    {
        return Err(ApplicationStartError::InvalidDescription);
    }

    Ok(ApplicationStart {
        context,
        identity,
        profile,
        manager_generation: start.launch.manager_generation,
    })
}

fn validate_launch<const N: usize>(launch: &ApplicationLaunch<'_, N>) -> Result<(), ()> {
    if !launch.identity.is_valid()
        || launch.manager_generation == 0
        || launch.process_limit == 0
        || launch.process_limit > limits::MAX_JOB_PROCESSES
        || N > limits::MAX_IPC_MESSAGE_HANDLES
        || !capability_descriptions_valid(launch.capabilities)
    {
        return Err(());
    }
    validate_command(launch.command)
}

fn validate_component_launch<const N: usize, const M: usize>(
    authority_ceiling: &[ApplicationCapability; N],
    launch: &ApplicationComponentLaunch<'_, M>,
) -> Result<(), ApplicationComponentLaunchError> {
    if launch.component == 0
        || !launch.profile.is_reduced_component()
        || M > limits::MAX_IPC_MESSAGE_HANDLES
        || !component_capability_descriptions_valid(launch.capabilities)
        || validate_command(launch.command).is_err()
    {
        return Err(ApplicationComponentLaunchError::InvalidDescription);
    }

    let mut strictly_reduced = authority_ceiling.is_empty();
    for requested in launch.capabilities {
        let Some(allowed) = authority_ceiling
            .iter()
            .find(|allowed| allowed.role == requested.role)
        else {
            return Err(ApplicationComponentLaunchError::AuthorityNotDelegable(
                requested.role,
            ));
        };
        if allowed.source_handle != requested.source_handle
            || !allowed.component_profiles.contains(launch.profile)
        {
            return Err(ApplicationComponentLaunchError::AuthorityNotDelegable(
                requested.role,
            ));
        }
        if !allowed.rights.contains(requested.rights) {
            return Err(ApplicationComponentLaunchError::AuthorityEscalation(
                requested.role,
            ));
        }
        strictly_reduced |= allowed.rights != requested.rights;
    }
    strictly_reduced |= M < N;
    if !strictly_reduced {
        return Err(ApplicationComponentLaunchError::AuthorityNotReduced);
    }
    Ok(())
}

fn capability_descriptions_valid<const N: usize>(
    capabilities: &[ApplicationCapability; N],
) -> bool {
    capabilities.iter().enumerate().all(|(index, capability)| {
        capability.source_handle != 0
            && capability.rights != Rights::EMPTY
            && !capabilities[..index]
                .iter()
                .any(|other| other.role == capability.role)
    })
}

fn component_capability_descriptions_valid<const N: usize>(
    capabilities: &[ApplicationComponentCapability; N],
) -> bool {
    capabilities.iter().enumerate().all(|(index, capability)| {
        capability.source_handle != 0
            && capability.rights != Rights::EMPTY
            && !capabilities[..index]
                .iter()
                .any(|other| other.role == capability.role)
    })
}

fn validate_command(command: &[u8]) -> Result<(), ()> {
    let mut arguments = [&[][..]; limits::MAX_ARGUMENTS];
    let count = split_command(command, &mut arguments).ok_or(())?;
    if !is_absolute_canonical_executable(arguments[0])
        || arguments[..count]
            .iter()
            .any(|argument| argument.is_empty())
    {
        return Err(());
    }
    Ok(())
}

fn spawn_application_in_job<const N: usize>(
    launch: ApplicationLaunch<'_, N>,
    job: ipc::CapabilityHandle,
) -> Result<ProcessId, ApplicationLaunchError> {
    let mut release = LaunchReleaseBarrier::new().map_err(ApplicationLaunchError::Descriptor)?;
    let mut isolated =
        CapabilityDescriptorIsolation::new().map_err(ApplicationLaunchError::Descriptor)?;
    let child = syscall::fork().map_err(ApplicationLaunchError::Descriptor)?;
    if child == 0 {
        launch_isolated_child(launch.command, release.pair, isolated.pair)
    }

    if platform::set_process_group(child, child).is_err() {
        terminate_and_reap(child);
        return Err(ApplicationLaunchError::InvalidDescription);
    }
    if let Err(error) = isolated.wait_for_child() {
        terminate_and_reap(child);
        return Err(ApplicationLaunchError::Descriptor(error));
    }
    if let Err(error) = ipc::job_assign(job, child) {
        terminate_and_reap(child);
        return Err(ApplicationLaunchError::Capability(error));
    }
    if let Err(error) = install_application_start(child, launch) {
        terminate_and_reap(child);
        return Err(ApplicationLaunchError::Startup(error));
    }
    if let Err(error) = release.release() {
        terminate_and_reap(child);
        return Err(ApplicationLaunchError::Descriptor(error));
    }
    Ok(child)
}

fn install_application_start<const N: usize>(
    child: ProcessId,
    launch: ApplicationLaunch<'_, N>,
) -> Result<(), ApplicationStartupSendError> {
    let (sender, receiver) =
        ipc::endpoint_create_pair().map_err(ApplicationStartupSendError::CapabilityDuplicate)?;
    let granted = ipc::grant_child(
        child,
        receiver,
        Rights::RECEIVE,
        PROCESS_START_BOOTSTRAP_SLOT,
    )
    .is_ok();
    if !granted {
        let _ = ipc::close(sender);
        let _ = ipc::close(receiver);
        return Err(ApplicationStartupSendError::InvalidDescription);
    }
    let result = send_application_start(sender, child, launch);
    let sender_closed = ipc::close(sender).is_ok();
    let receiver_closed = ipc::close(receiver).is_ok();
    match result {
        Err(error) => Err(error),
        Ok(()) if sender_closed && receiver_closed => Ok(()),
        Ok(()) => Err(ApplicationStartupSendError::InvalidDescription),
    }
}

fn send_application_start<const N: usize>(
    sender: u64,
    process_id: ProcessId,
    launch: ApplicationLaunch<'_, N>,
) -> Result<(), ApplicationStartupSendError> {
    let mut resources = [None; N];
    let mut transfers = [Transfer {
        handle: 0,
        rights: Rights::EMPTY,
    }; N];
    let mut duplicates = [0_u64; N];
    for (index, capability) in launch.capabilities.iter().enumerate() {
        let duplicate = match ipc::duplicate(
            capability.source_handle,
            capability.rights | Rights::TRANSFER,
        ) {
            Ok(handle) => handle,
            Err(error) => {
                close_duplicates(&duplicates[..index]);
                return Err(ApplicationStartupSendError::CapabilityDuplicate(error));
            }
        };
        duplicates[index] = duplicate;
        resources[index] = Some(StartupResource {
            role: capability.role,
            required: true,
        });
        transfers[index] = Transfer {
            handle: duplicate,
            rights: capability.rights,
        };
    }
    let message = StartupMessage::<N>::new(StartupRuntimeRole::Application, resources)
        .map_err(|_| ApplicationStartupSendError::InvalidDescription)?;
    if let Err(error) = send_startup_message(sender, &message, &transfers) {
        close_duplicates(&duplicates);
        return Err(ApplicationStartupSendError::CapabilityMessage(error));
    }

    let mut arguments = [&[][..]; limits::MAX_ARGUMENTS];
    let argument_count = split_command(launch.command, &mut arguments)
        .ok_or(ApplicationStartupSendError::InvalidDescription)?;
    let mut argument_bytes = [0_u8; limits::MAX_ARGUMENT_BYTES];
    let argument_length =
        encode_startup_arguments(&arguments[..argument_count], &mut argument_bytes)
            .map_err(ApplicationStartupSendError::Data)?;
    let mut environment_bytes = [0_u8; limits::MAX_ENVIRONMENT_BYTES];
    let environment_length = encode_startup_environment(&[], &mut environment_bytes)
        .map_err(ApplicationStartupSendError::Data)?;
    let monotonic_start_ns =
        platform::monotonic_time_ns().map_err(|_| ApplicationStartupSendError::Clock)?;
    let identity = StartupIdentity {
        process: process_id,
        package: launch.identity.package,
        package_generation: launch.identity.package_generation,
        executable: crate::managed_startup::numeric_executable_id(arguments[0]),
        application: launch.identity.application,
        service: 0,
        component: launch.identity.component,
        user: launch.identity.user,
        session: launch.identity.session,
    }
    .encode();
    let launch_data = StartupLaunch {
        launch: process_id,
        manager_generation: launch.manager_generation,
        namespace_profile: launch.profile.namespace_profile_id(),
        monotonic_start_ns,
        attempt: 1,
        reason: StartupLaunchReason::User,
        flags: 0,
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
            bytes: &launch_data,
        },
    ];
    send_process_start_data(sender, &sections).map_err(ApplicationStartupSendError::Transport)
}

fn close_duplicates(handles: &[u64]) {
    for handle in handles.iter().copied().filter(|handle| *handle != 0) {
        let _ = ipc::close(handle);
    }
}

#[derive(Debug)]
struct LaunchReleaseBarrier {
    pair: PipePair,
    reader_open: bool,
    writer_open: bool,
}

impl LaunchReleaseBarrier {
    fn new() -> syscall::Result<Self> {
        let pair = syscall::pipe_pair()?;
        if syscall::set_descriptor_flags(pair.reader, DescriptorFlags::CLOSE_ON_EXEC).is_err()
            || syscall::set_descriptor_flags(pair.writer, DescriptorFlags::CLOSE_ON_EXEC).is_err()
        {
            let _ = syscall::close(pair.reader);
            let _ = syscall::close(pair.writer);
            return Err(syscall::Errno::IO);
        }
        Ok(Self {
            pair,
            reader_open: true,
            writer_open: true,
        })
    }

    fn parent_after_fork(&mut self) -> syscall::Result<()> {
        if self.reader_open {
            syscall::close(self.pair.reader)?;
            self.reader_open = false;
        }
        Ok(())
    }

    fn release(&mut self) -> syscall::Result<()> {
        self.parent_after_fork()?;
        if self.writer_open {
            syscall::close(self.pair.writer)?;
            self.writer_open = false;
        }
        Ok(())
    }
}

impl Drop for LaunchReleaseBarrier {
    fn drop(&mut self) {
        if self.reader_open {
            let _ = syscall::close(self.pair.reader);
        }
        if self.writer_open {
            let _ = syscall::close(self.pair.writer);
        }
    }
}

#[derive(Debug)]
struct CapabilityDescriptorIsolation {
    pair: PipePair,
    reader_open: bool,
    writer_open: bool,
}

impl CapabilityDescriptorIsolation {
    fn new() -> syscall::Result<Self> {
        let pair = syscall::pipe_pair()?;
        if syscall::set_descriptor_flags(pair.reader, DescriptorFlags::CLOSE_ON_EXEC).is_err()
            || syscall::set_descriptor_flags(pair.writer, DescriptorFlags::CLOSE_ON_EXEC).is_err()
        {
            let _ = syscall::close(pair.reader);
            let _ = syscall::close(pair.writer);
            return Err(syscall::Errno::IO);
        }
        Ok(Self {
            pair,
            reader_open: true,
            writer_open: true,
        })
    }

    fn wait_for_child(&mut self) -> syscall::Result<()> {
        if self.writer_open {
            syscall::close(self.pair.writer)?;
            self.writer_open = false;
        }
        let mut byte = [0_u8; 1];
        let result = loop {
            match syscall::read(self.pair.reader, &mut byte) {
                Ok(1) if byte[0] == ISOLATION_ACK => break Ok(()),
                Ok(_) => break Err(syscall::Errno::IO),
                Err(error) if error == syscall::Errno::INTERRUPTED => {}
                Err(error) => break Err(error),
            }
        };
        if self.reader_open {
            let close = syscall::close(self.pair.reader);
            self.reader_open = false;
            result.and(close)
        } else {
            result
        }
    }
}

impl Drop for CapabilityDescriptorIsolation {
    fn drop(&mut self) {
        if self.reader_open {
            let _ = syscall::close(self.pair.reader);
        }
        if self.writer_open {
            let _ = syscall::close(self.pair.writer);
        }
    }
}

fn launch_isolated_child(command: &[u8], release: PipePair, isolation: PipePair) -> ! {
    if syscall::close(release.writer).is_err()
        || syscall::close(isolation.reader).is_err()
        || close_descriptors_except(release.reader, isolation.writer).is_err()
        || syscall::close_all_capabilities().is_err()
        || syscall::write_all(isolation.writer, &[ISOLATION_ACK]).is_err()
        || syscall::close(isolation.writer).is_err()
    {
        syscall::exit(126);
    }

    wait_for_release(release.reader);
    if syscall::execve(command).is_err() {
        syscall::exit(126);
    }
    syscall::exit(127)
}

/// Closes every inherited userspace file descriptor except the two private
/// launch-control descriptors. Both survivors are closed before `execve`, so a
/// successful application image begins with an empty descriptor table.
fn close_descriptors_except(first: FileDescriptor, second: FileDescriptor) -> syscall::Result<()> {
    for descriptor in 0..limits::MAX_OPEN_FILES as u64 {
        if descriptor == first || descriptor == second {
            continue;
        }
        match syscall::close(descriptor) {
            Ok(()) | Err(syscall::Errno::BAD_FILE_DESCRIPTOR) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn wait_for_release(reader: FileDescriptor) {
    let mut byte = [0_u8; 1];
    loop {
        match syscall::read(reader, &mut byte) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            Err(_) => syscall::exit(126),
        }
    }
    if syscall::close(reader).is_err() {
        syscall::exit(126);
    }
}

fn terminate_and_reap(process_id: ProcessId) {
    let _ = platform::kill(process_id, signal::KILL);
    loop {
        match syscall::wait_child(process_id) {
            Ok(status) if status.continued() || status.stopped_signal().is_some() => {}
            Err(error) if error == syscall::Errno::INTERRUPTED => {}
            _ => break,
        }
    }
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

fn is_absolute_canonical_executable(executable: &[u8]) -> bool {
    executable.first() == Some(&b'/')
        && executable.len() > 1
        && executable
            .split(|byte| *byte == b'/')
            .skip(1)
            .all(|component| !component.is_empty() && component != b"." && component != b"..")
}

fn arguments_match(arguments: Args<'_>, start: ValidatedProcessStart<'_>) -> bool {
    arguments.len() == start.arguments.len()
        && arguments
            .iter()
            .enumerate()
            .all(|(index, argument)| start.arguments.get(index) == Some(argument))
}

#[cfg(test)]
mod tests {
    use super::{
        ApplicationCapability, ApplicationComponentCapability, ApplicationComponentLaunch,
        ApplicationComponentLaunchError, ApplicationIdentity, ApplicationLaunch,
        ApplicationProfile, ComponentProfileSet, DESKTOP_CHILD_NAMESPACE_PROFILE_ID,
        DESKTOP_NAMESPACE_PROFILE_ID, WORKER_NAMESPACE_PROFILE_ID,
        is_absolute_canonical_executable, validate_component_launch, validate_launch,
    };
    use crate::{ipc::Rights, runtime_context::CapabilityRole};

    const IDENTITY: ApplicationIdentity = ApplicationIdentity {
        package: 1,
        package_generation: 1,
        application: 2,
        component: 3,
        user: 4,
        session: 5,
    };

    #[test]
    fn profiles_have_distinct_non_system_namespace_ids() {
        assert_eq!(
            ApplicationProfile::Desktop.namespace_profile_id(),
            DESKTOP_NAMESPACE_PROFILE_ID
        );
        assert_eq!(
            ApplicationProfile::DesktopChild.namespace_profile_id(),
            DESKTOP_CHILD_NAMESPACE_PROFILE_ID
        );
        assert_eq!(
            ApplicationProfile::Worker.namespace_profile_id(),
            WORKER_NAMESPACE_PROFILE_ID
        );
        assert_ne!(DESKTOP_NAMESPACE_PROFILE_ID, 1);
        assert_ne!(
            DESKTOP_NAMESPACE_PROFILE_ID,
            DESKTOP_CHILD_NAMESPACE_PROFILE_ID
        );
        assert_ne!(
            DESKTOP_CHILD_NAMESPACE_PROFILE_ID,
            WORKER_NAMESPACE_PROFILE_ID
        );
    }

    #[test]
    fn application_executable_must_be_absolute_and_canonical() {
        assert!(is_absolute_canonical_executable(b"/Applications/Test/app"));
        assert!(!is_absolute_canonical_executable(b"Applications/Test/app"));
        assert!(!is_absolute_canonical_executable(
            b"/Applications/../bin/app"
        ));
        assert!(!is_absolute_canonical_executable(b"/Applications//app"));
    }

    #[test]
    fn launch_requires_nonzero_identity_and_bounded_process_limit() {
        let capabilities = [];
        let launch = ApplicationLaunch::new(
            b"/Applications/Test/app --probe",
            IDENTITY,
            ApplicationProfile::Desktop,
            1,
            &capabilities,
        );
        assert!(validate_launch(&launch).is_ok());
        assert!(validate_launch(&launch.with_process_limit(0)).is_err());
        let invalid = ApplicationLaunch::new(
            b"/Applications/Test/app",
            ApplicationIdentity {
                application: 0,
                ..IDENTITY
            },
            ApplicationProfile::Desktop,
            1,
            &capabilities,
        );
        assert!(validate_launch(&invalid).is_err());
    }

    #[test]
    fn component_allowlist_requires_exact_delegable_source_and_reduced_rights() {
        let ceiling = [
            ApplicationCapability::new(11, Rights::SEND, CapabilityRole::LOGGING)
                .delegable_to(ComponentProfileSet::ALL_REDUCED),
            ApplicationCapability::new(12, Rights::SEND, CapabilityRole::SERVICE_NAMESPACE),
        ];
        let requested = [ApplicationComponentCapability::new(
            11,
            Rights::SEND,
            CapabilityRole::LOGGING,
        )];
        let launch = ApplicationComponentLaunch::new(
            b"/Applications/Test/worker",
            4,
            ApplicationProfile::Worker,
            &requested,
        );
        assert_eq!(validate_component_launch(&ceiling, &launch), Ok(()));

        let wrong_source = [ApplicationComponentCapability::new(
            13,
            Rights::SEND,
            CapabilityRole::LOGGING,
        )];
        assert_eq!(
            validate_component_launch(
                &ceiling,
                &ApplicationComponentLaunch::new(
                    b"/Applications/Test/worker",
                    4,
                    ApplicationProfile::Worker,
                    &wrong_source,
                ),
            ),
            Err(ApplicationComponentLaunchError::AuthorityNotDelegable(
                CapabilityRole::LOGGING
            ))
        );

        let nondelegable = [ApplicationComponentCapability::new(
            12,
            Rights::SEND,
            CapabilityRole::SERVICE_NAMESPACE,
        )];
        assert_eq!(
            validate_component_launch(
                &ceiling,
                &ApplicationComponentLaunch::new(
                    b"/Applications/Test/worker",
                    4,
                    ApplicationProfile::Worker,
                    &nondelegable,
                ),
            ),
            Err(ApplicationComponentLaunchError::AuthorityNotDelegable(
                CapabilityRole::SERVICE_NAMESPACE
            ))
        );
    }

    #[test]
    fn component_launch_rejects_profile_relaxation_and_authority_cloning() {
        let ceiling = [
            ApplicationCapability::new(11, Rights::SEND, CapabilityRole::LOGGING)
                .delegable_to(ComponentProfileSet::ALL_REDUCED),
        ];
        let requested = [ApplicationComponentCapability::new(
            11,
            Rights::SEND,
            CapabilityRole::LOGGING,
        )];
        assert_eq!(
            validate_component_launch(
                &ceiling,
                &ApplicationComponentLaunch::new(
                    b"/Applications/Test/child",
                    4,
                    ApplicationProfile::Desktop,
                    &requested,
                ),
            ),
            Err(ApplicationComponentLaunchError::InvalidDescription)
        );
        assert_eq!(
            validate_component_launch(
                &ceiling,
                &ApplicationComponentLaunch::new(
                    b"/Applications/Test/child",
                    4,
                    ApplicationProfile::DesktopChild,
                    &requested,
                ),
            ),
            Err(ApplicationComponentLaunchError::AuthorityNotReduced)
        );

        let escalated = [ApplicationComponentCapability::new(
            11,
            Rights::SEND | Rights::DUPLICATE,
            CapabilityRole::LOGGING,
        )];
        assert_eq!(
            validate_component_launch(
                &ceiling,
                &ApplicationComponentLaunch::new(
                    b"/Applications/Test/child",
                    4,
                    ApplicationProfile::DesktopChild,
                    &escalated,
                ),
            ),
            Err(ApplicationComponentLaunchError::AuthorityEscalation(
                CapabilityRole::LOGGING
            ))
        );
    }
}
