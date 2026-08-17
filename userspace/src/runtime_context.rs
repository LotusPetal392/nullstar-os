//! Typed, capability-bearing process contexts and service bindings.
//!
//! A [`ProcessContext`] owns the explicit capabilities supplied to one runtime
//! role. Capabilities are selected by stable role identifiers, validated against
//! their kernel object kind, and tightened to the exact rights requested by the
//! consumer. [`ServiceBinding`] adds a protocol and endpoint-side type without
//! turning a discoverable protocol name into authority.

use core::{array, marker::PhantomData};

use nswp_runtime::ProtocolDescriptor;

use crate::{
    abi::limits,
    handle::{AnyObject, BorrowedHandle, Endpoint, KnownObjectType, ObjectType, OwnedHandle},
    ipc::{self, ObjectKind, Rights},
};

const STARTUP_MAGIC: [u8; 4] = *b"NSPC";
const STARTUP_HEADER_BYTES: usize = 16;
const STARTUP_RESOURCE_BYTES: usize = 8;
const STARTUP_RESOURCE_REQUIRED: u16 = 1;

/// First supported version of the capability-bearing process-start record.
pub const STARTUP_VERSION: u16 = 1;

/// Stable identity for one capability role in a process startup contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityRole(u32);

impl CapabilityRole {
    pub const BOOTSTRAP: Self = Self(1);
    pub const LIFECYCLE: Self = Self(2);
    pub const LOGGING: Self = Self(3);
    pub const CONFIGURATION: Self = Self(4);
    pub const SERVICE_NAMESPACE: Self = Self(5);
    pub const PRIVATE_STORAGE: Self = Self(6);
    pub const JOB: Self = Self(7);

    pub const fn new(value: u32) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Compile-time process-role marker.
pub trait RuntimeRole: private::Sealed {
    const NAME: &'static str;
    const STARTUP_ROLE: StartupRuntimeRole;
}

/// Stable wire identity for the runtime role selected by the process manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum StartupRuntimeRole {
    Application = 1,
    Service = 2,
    Driver = 3,
    Tool = 4,
}

impl StartupRuntimeRole {
    const fn from_raw(raw: u16) -> Option<Self> {
        match raw {
            1 => Some(Self::Application),
            2 => Some(Self::Service),
            3 => Some(Self::Driver),
            4 => Some(Self::Tool),
            _ => None,
        }
    }
}

/// Native application process context.
#[derive(Debug)]
pub enum ApplicationProcess {}

/// Native system-service process context.
#[derive(Debug)]
pub enum ServiceProcess {}

/// Restricted userspace-driver process context.
#[derive(Debug)]
pub enum DriverProcess {}

/// Administrative or recovery-tool process context.
#[derive(Debug)]
pub enum ToolProcess {}

mod private {
    pub trait Sealed {}
    impl Sealed for super::ApplicationProcess {}
    impl Sealed for super::ServiceProcess {}
    impl Sealed for super::DriverProcess {}
    impl Sealed for super::ToolProcess {}
    impl Sealed for super::Client {}
    impl Sealed for super::Server {}
}

impl RuntimeRole for ApplicationProcess {
    const NAME: &'static str = "application";
    const STARTUP_ROLE: StartupRuntimeRole = StartupRuntimeRole::Application;
}

impl RuntimeRole for ServiceProcess {
    const NAME: &'static str = "service";
    const STARTUP_ROLE: StartupRuntimeRole = StartupRuntimeRole::Service;
}

impl RuntimeRole for DriverProcess {
    const NAME: &'static str = "driver";
    const STARTUP_ROLE: StartupRuntimeRole = StartupRuntimeRole::Driver;
}

impl RuntimeRole for ToolProcess {
    const NAME: &'static str = "tool";
    const STARTUP_ROLE: StartupRuntimeRole = StartupRuntimeRole::Tool;
}

/// One positional capability declaration in a startup message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupResource {
    pub role: CapabilityRole,
    pub required: bool,
}

/// Decoded, allocation-free startup record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupMessage<const N: usize> {
    runtime_role: StartupRuntimeRole,
    resources: [Option<StartupResource>; N],
    resource_count: usize,
}

impl<const N: usize> StartupMessage<N> {
    pub fn new(
        runtime_role: StartupRuntimeRole,
        resources: [Option<StartupResource>; N],
    ) -> Result<Self, StartupError> {
        let resource_count = resources.iter().flatten().count();
        if resource_count > limits::MAX_IPC_MESSAGE_HANDLES
            || resources[..resource_count].iter().any(Option::is_none)
            || resources[resource_count..].iter().any(Option::is_some)
        {
            return Err(StartupError::ResourceLimit);
        }
        validate_resource_roles(&resources, resource_count)?;
        Ok(Self {
            runtime_role,
            resources,
            resource_count,
        })
    }

    pub const fn runtime_role(&self) -> StartupRuntimeRole {
        self.runtime_role
    }

    pub const fn resource_count(&self) -> usize {
        self.resource_count
    }

    pub fn resources(&self) -> &[Option<StartupResource>] {
        &self.resources[..self.resource_count]
    }

    /// Writes the canonical little-endian startup record and returns its length.
    pub fn encode(&self, output: &mut [u8]) -> Result<usize, StartupError> {
        let length = STARTUP_HEADER_BYTES + self.resource_count * STARTUP_RESOURCE_BYTES;
        if output.len() < length {
            return Err(StartupError::MessageBounds);
        }
        output[..length].fill(0);
        output[..4].copy_from_slice(&STARTUP_MAGIC);
        put_u16(output, 4, STARTUP_VERSION);
        put_u16(output, 6, self.runtime_role as u16);
        put_u16(output, 8, self.resource_count as u16);
        put_u16(output, 10, STARTUP_RESOURCE_BYTES as u16);
        for (index, resource) in self.resources().iter().flatten().enumerate() {
            let offset = STARTUP_HEADER_BYTES + index * STARTUP_RESOURCE_BYTES;
            put_u32(output, offset, resource.role.value());
            put_u16(
                output,
                offset + 4,
                if resource.required {
                    STARTUP_RESOURCE_REQUIRED
                } else {
                    0
                },
            );
        }
        Ok(length)
    }

    /// Decodes a canonical startup record, rejecting extensions until negotiated.
    pub fn decode(bytes: &[u8]) -> Result<Self, StartupError> {
        if bytes.len() < STARTUP_HEADER_BYTES || bytes[..4] != STARTUP_MAGIC {
            return Err(StartupError::MalformedMessage);
        }
        let version = get_u16(bytes, 4);
        if version != STARTUP_VERSION {
            return Err(StartupError::UnsupportedVersion(version));
        }
        let runtime_role = StartupRuntimeRole::from_raw(get_u16(bytes, 6))
            .ok_or(StartupError::MalformedMessage)?;
        let resource_count = get_u16(bytes, 8) as usize;
        if resource_count > N || resource_count > limits::MAX_IPC_MESSAGE_HANDLES {
            return Err(StartupError::ResourceLimit);
        }
        if get_u16(bytes, 10) as usize != STARTUP_RESOURCE_BYTES
            || bytes[12..STARTUP_HEADER_BYTES]
                .iter()
                .any(|byte| *byte != 0)
            || bytes.len() != STARTUP_HEADER_BYTES + resource_count * STARTUP_RESOURCE_BYTES
        {
            return Err(StartupError::MalformedMessage);
        }
        let mut resources = array::from_fn(|_| None);
        for (index, slot) in resources.iter_mut().enumerate().take(resource_count) {
            let offset = STARTUP_HEADER_BYTES + index * STARTUP_RESOURCE_BYTES;
            let role = CapabilityRole::new(get_u32(bytes, offset))
                .ok_or(StartupError::MalformedMessage)?;
            let flags = get_u16(bytes, offset + 4);
            if flags & !STARTUP_RESOURCE_REQUIRED != 0
                || bytes[offset + 6..offset + STARTUP_RESOURCE_BYTES]
                    .iter()
                    .any(|byte| *byte != 0)
            {
                return Err(StartupError::MalformedMessage);
            }
            *slot = Some(StartupResource {
                role,
                required: flags & STARTUP_RESOURCE_REQUIRED != 0,
            });
        }
        validate_resource_roles(&resources, resource_count)?;
        Ok(Self {
            runtime_role,
            resources,
            resource_count,
        })
    }
}

/// Trusted receiver policy for one semantic startup capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupCapabilityPolicy {
    pub role: CapabilityRole,
    pub kind: ObjectKind,
    pub minimum_rights: Rights,
    pub maximum_rights: Rights,
    pub required: bool,
}

/// Why a process-start record or one of its attached capabilities was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupError {
    MalformedMessage,
    UnsupportedVersion(u16),
    MessageBounds,
    ResourceLimit,
    RuntimeRoleMismatch,
    DuplicateRole(CapabilityRole),
    InvalidPolicy(CapabilityRole),
    UnknownRequiredRole(CapabilityRole),
    MissingRequiredRole(CapabilityRole),
    WrongObjectKind(CapabilityRole),
    InsufficientRights(CapabilityRole),
    Kernel(CapabilityRole, ipc::Error),
}

/// Failed startup adoption with ownership of every still-live attachment.
#[derive(Debug)]
pub struct StartupContextError<const N: usize> {
    error: StartupError,
    handles: [Option<OwnedHandle<AnyObject>>; N],
}

impl<const N: usize> StartupContextError<N> {
    const fn new(handles: [Option<OwnedHandle<AnyObject>>; N], error: StartupError) -> Self {
        Self { error, handles }
    }

    pub const fn error(&self) -> StartupError {
        self.error
    }

    pub fn into_handles(self) -> [Option<OwnedHandle<AnyObject>>; N] {
        self.handles
    }

    pub fn into_parts(self) -> ([Option<OwnedHandle<AnyObject>>; N], StartupError) {
        (self.handles, self.error)
    }
}

/// One owned capability before it is claimed by a role-specific consumer.
#[derive(Debug)]
pub struct ContextCapability {
    role: CapabilityRole,
    handle: OwnedHandle<AnyObject>,
}

impl ContextCapability {
    pub fn new<T: ObjectType>(role: CapabilityRole, handle: OwnedHandle<T>) -> Self {
        Self {
            role,
            handle: handle.erase(),
        }
    }

    pub const fn role(&self) -> CapabilityRole {
        self.role
    }

    pub fn handle(&self) -> &OwnedHandle<AnyObject> {
        &self.handle
    }

    pub fn into_handle(self) -> OwnedHandle<AnyObject> {
        self.handle
    }
}

/// Why a process-context capability could not be claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextError {
    DuplicateRole(CapabilityRole),
    MissingRole(CapabilityRole),
    WrongObjectKind(CapabilityRole),
    InsufficientRights(CapabilityRole),
    Kernel(CapabilityRole, ipc::Error),
    InvalidProtocol(ConformanceError),
}

/// Fixed-capacity authority set for one compile-time process role.
#[derive(Debug)]
pub struct ProcessContext<R: RuntimeRole, const N: usize> {
    capabilities: [Option<ContextCapability>; N],
    role: PhantomData<R>,
}

impl<R: RuntimeRole, const N: usize> ProcessContext<R, N> {
    /// Constructs a context after rejecting ambiguous duplicate role entries.
    pub fn new(
        capabilities: [Option<ContextCapability>; N],
    ) -> Result<Self, ([Option<ContextCapability>; N], ContextError)> {
        for index in 0..N {
            let Some(candidate) = capabilities[index].as_ref() else {
                continue;
            };
            if capabilities[..index]
                .iter()
                .flatten()
                .any(|current| current.role == candidate.role)
            {
                let role = candidate.role;
                return Err((capabilities, ContextError::DuplicateRole(role)));
            }
        }
        Ok(Self {
            capabilities,
            role: PhantomData,
        })
    }

    /// Adopts the positional capabilities attached to one startup record.
    ///
    /// The receiver policy, rather than sender-controlled metadata, defines the
    /// accepted object kind and authority range. Unknown optional capabilities
    /// are closed. Known capabilities are rights-reduced to the intersection of
    /// their delivered rights and the policy ceiling before entering the context.
    pub fn from_startup(
        bytes: &[u8],
        mut handles: [Option<OwnedHandle<AnyObject>>; N],
        policy: &[StartupCapabilityPolicy],
    ) -> Result<Self, StartupContextError<N>> {
        let message = match StartupMessage::<N>::decode(bytes) {
            Ok(message) => message,
            Err(error) => return Err(StartupContextError::new(handles, error)),
        };
        if message.runtime_role != R::STARTUP_ROLE {
            return Err(StartupContextError::new(
                handles,
                StartupError::RuntimeRoleMismatch,
            ));
        }
        if handles[..message.resource_count]
            .iter()
            .any(Option::is_none)
            || handles[message.resource_count..]
                .iter()
                .any(Option::is_some)
        {
            return Err(StartupContextError::new(
                handles,
                StartupError::MalformedMessage,
            ));
        }
        if let Err(error) = validate_startup_policy(policy) {
            return Err(StartupContextError::new(handles, error));
        }

        let mut tightened_rights = [None; N];
        for (index, resource) in message.resources().iter().flatten().enumerate() {
            let Some(expected) = policy.iter().find(|entry| entry.role == resource.role) else {
                if resource.required {
                    return Err(StartupContextError::new(
                        handles,
                        StartupError::UnknownRequiredRole(resource.role),
                    ));
                }
                continue;
            };
            let handle = handles[index]
                .as_ref()
                .expect("validated startup handle remains present");
            let info = match handle.info() {
                Ok(info) => info,
                Err(error) => {
                    return Err(StartupContextError::new(
                        handles,
                        StartupError::Kernel(resource.role, error),
                    ));
                }
            };
            if info.kind != expected.kind {
                return Err(StartupContextError::new(
                    handles,
                    StartupError::WrongObjectKind(resource.role),
                ));
            }
            if !info.rights.contains(expected.minimum_rights) {
                return Err(StartupContextError::new(
                    handles,
                    StartupError::InsufficientRights(resource.role),
                ));
            }
            tightened_rights[index] =
                Rights::from_bits(info.rights.bits() & expected.maximum_rights.bits());
        }
        for expected in policy.iter().filter(|entry| entry.required) {
            if !message
                .resources()
                .iter()
                .flatten()
                .any(|resource| resource.role == expected.role)
            {
                return Err(StartupContextError::new(
                    handles,
                    StartupError::MissingRequiredRole(expected.role),
                ));
            }
        }

        for (index, resource) in message.resources().iter().flatten().enumerate() {
            let Some(rights) = tightened_rights[index] else {
                continue;
            };
            let handle = handles[index]
                .as_mut()
                .expect("validated startup handle remains present");
            let delivered = match handle.info() {
                Ok(info) => info.rights,
                Err(error) => {
                    return Err(StartupContextError::new(
                        handles,
                        StartupError::Kernel(resource.role, error),
                    ));
                }
            };
            if delivered != rights
                && let Err(error) = handle.replace_rights(rights)
            {
                return Err(StartupContextError::new(
                    handles,
                    StartupError::Kernel(resource.role, error),
                ));
            }
        }

        let mut capabilities = array::from_fn(|_| None);
        for (index, resource) in message.resources().iter().flatten().enumerate() {
            if tightened_rights[index].is_none() {
                drop(handles[index].take());
                continue;
            }
            let handle = handles[index]
                .take()
                .expect("validated startup handle remains present");
            capabilities[index] = Some(ContextCapability {
                role: resource.role,
                handle,
            });
        }
        Ok(Self {
            capabilities,
            role: PhantomData,
        })
    }

    pub const fn runtime_role(&self) -> &'static str {
        R::NAME
    }

    pub fn len(&self) -> usize {
        self.capabilities.iter().flatten().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, role: CapabilityRole) -> bool {
        self.capabilities
            .iter()
            .flatten()
            .any(|capability| capability.role == role)
    }

    /// Claims one capability, validates its object type, and permanently
    /// tightens it to `required_rights`. Every failure restores the capability
    /// to this context so ownership is never lost implicitly.
    pub fn take<T: KnownObjectType>(
        &mut self,
        role: CapabilityRole,
        required_rights: Rights,
    ) -> Result<OwnedHandle<T>, ContextError> {
        let Some(index) = self.capabilities.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|capability| capability.role == role)
        }) else {
            return Err(ContextError::MissingRole(role));
        };
        let capability = self.capabilities[index]
            .take()
            .expect("located context capability remains present");
        let handle = match capability.handle.try_cast::<T>() {
            Ok(handle) => handle,
            Err((error, handle)) => {
                self.capabilities[index] = Some(ContextCapability { role, handle });
                return if error == ipc::Error::INVALID_ARGUMENT {
                    Err(ContextError::WrongObjectKind(role))
                } else {
                    Err(ContextError::Kernel(role, error))
                };
            }
        };
        let rights = match handle.info() {
            Ok(info) => info.rights,
            Err(error) => {
                self.capabilities[index] = Some(ContextCapability::new(role, handle));
                return Err(ContextError::Kernel(role, error));
            }
        };
        if !rights.contains(required_rights) {
            self.capabilities[index] = Some(ContextCapability::new(role, handle));
            return Err(ContextError::InsufficientRights(role));
        }
        let mut handle = handle;
        if rights != required_rights
            && let Err(error) = handle.replace_rights(required_rights)
        {
            self.capabilities[index] = Some(ContextCapability::new(role, handle));
            return Err(ContextError::Kernel(role, error));
        }
        Ok(handle)
    }

    pub fn into_capabilities(self) -> [Option<ContextCapability>; N] {
        self.capabilities
    }
}

fn validate_resource_roles<const N: usize>(
    resources: &[Option<StartupResource>; N],
    resource_count: usize,
) -> Result<(), StartupError> {
    for index in 0..resource_count {
        let candidate = resources[index].expect("bounded resource is present");
        if resources[..index]
            .iter()
            .flatten()
            .any(|current| current.role == candidate.role)
        {
            return Err(StartupError::DuplicateRole(candidate.role));
        }
    }
    Ok(())
}

fn validate_startup_policy(policy: &[StartupCapabilityPolicy]) -> Result<(), StartupError> {
    if policy.len() > limits::MAX_IPC_MESSAGE_HANDLES {
        let role = policy
            .first()
            .map_or(CapabilityRole::BOOTSTRAP, |entry| entry.role);
        return Err(StartupError::InvalidPolicy(role));
    }
    for (index, candidate) in policy.iter().enumerate() {
        if policy[..index]
            .iter()
            .any(|current| current.role == candidate.role)
            || !candidate.maximum_rights.contains(candidate.minimum_rights)
            || !rights_for_kind(candidate.kind).contains(candidate.maximum_rights)
        {
            return Err(StartupError::InvalidPolicy(candidate.role));
        }
    }
    Ok(())
}

const fn rights_for_kind(kind: ObjectKind) -> Rights {
    match kind {
        ObjectKind::Endpoint => Rights::ENDPOINT,
        ObjectKind::Notification => Rights::NOTIFICATION,
        ObjectKind::SharedMemory => Rights::SHARED_MEMORY,
        ObjectKind::KernelEarlyLogReader => Rights::KERNEL_EARLY_LOG_READER,
        ObjectKind::Job => Rights::JOB,
        ObjectKind::WaitSet => Rights::WAIT_SET,
        ObjectKind::EventPort => Rights::EVENT_PORT,
        ObjectKind::Timer => Rights::TIMER,
        ObjectKind::Event => Rights::EVENT,
    }
}

fn put_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn get_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([input[offset], input[offset + 1]])
}

fn get_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
    ])
}

impl<R: RuntimeRole, const N: usize> Default for ProcessContext<R, N> {
    fn default() -> Self {
        Self {
            capabilities: array::from_fn(|_| None),
            role: PhantomData,
        }
    }
}

/// Static service identity and NSWP contract used to type an endpoint.
///
/// Generated bindings can return their runtime descriptor directly; this layer
/// no longer maintains a second, provisional version-and-limits declaration.
pub trait ServiceProtocol {
    const NAME: &'static str;
    const CLIENT_RIGHTS: Rights;
    const SERVER_RIGHTS: Rights;

    fn descriptor() -> ProtocolDescriptor<'static>;
}

/// Protocol-side validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConformanceError {
    InvalidName,
    InvalidVersion,
    InvalidVersionProfiles,
    InvalidFeatures,
    InvalidMethods,
    MessageLimit,
    HandleLimit,
    OutstandingLimit,
    EmptyRights,
    InvalidEndpointRights,
}

/// Validates the common, allocation-free part of a protocol declaration.
pub fn validate_protocol<P: ServiceProtocol>() -> Result<(), ConformanceError> {
    let descriptor = P::descriptor();
    if !valid_protocol_name(P::NAME) {
        return Err(ConformanceError::InvalidName);
    }
    if descriptor.major == 0 || descriptor.min_minor > descriptor.max_minor {
        return Err(ConformanceError::InvalidVersion);
    }
    if descriptor.versions.is_empty()
        || descriptor.versions.first().map(|version| version.minor) != Some(descriptor.min_minor)
        || descriptor.versions.last().map(|version| version.minor) != Some(descriptor.max_minor)
        || descriptor.versions.iter().any(|version| {
            version.minimum_body_bytes == 0
                || version.minimum_body_bytes > descriptor.limits.max_body_bytes
                || version.minimum_handles > descriptor.limits.max_handles
        })
        || descriptor.versions.windows(2).any(|versions| {
            versions[0].minor.checked_add(1) != Some(versions[1].minor)
                || versions[0].minimum_body_bytes > versions[1].minimum_body_bytes
                || versions[0].minimum_handles > versions[1].minimum_handles
        })
    {
        return Err(ConformanceError::InvalidVersionProfiles);
    }
    if descriptor
        .requested_features
        .iter()
        .enumerate()
        .any(|(index, feature)| {
            feature.id == 0
                || descriptor.requested_features[..index]
                    .iter()
                    .any(|previous| previous.id >= feature.id)
        })
        || descriptor
            .available_features
            .iter()
            .enumerate()
            .any(|(index, feature)| {
                feature.id == 0
                    || feature.since_minor < descriptor.min_minor
                    || feature.since_minor > descriptor.max_minor
                    || descriptor.available_features[..index]
                        .iter()
                        .any(|previous| previous.id >= feature.id)
            })
    {
        return Err(ConformanceError::InvalidFeatures);
    }
    if descriptor
        .methods
        .iter()
        .enumerate()
        .any(|(index, method)| {
            method.ordinal == 0
                || descriptor.methods[..index]
                    .iter()
                    .any(|previous| previous.ordinal >= method.ordinal)
        })
    {
        return Err(ConformanceError::InvalidMethods);
    }
    if descriptor.limits.max_body_bytes == 0
        || descriptor.limits.max_body_bytes as usize > nswp_runtime::MAX_BODY_BYTES
        || descriptor.limits.max_body_bytes as usize > limits::MAX_IPC_MESSAGE_BYTES
    {
        return Err(ConformanceError::MessageLimit);
    }
    if descriptor.limits.max_handles as usize > limits::MAX_IPC_MESSAGE_HANDLES {
        return Err(ConformanceError::HandleLimit);
    }
    if descriptor.limits.max_outstanding == 0
        || descriptor.limits.max_outstanding as usize > nswp_runtime::MAX_OUTSTANDING
    {
        return Err(ConformanceError::OutstandingLimit);
    }
    for rights in [P::CLIENT_RIGHTS, P::SERVER_RIGHTS] {
        if rights == Rights::EMPTY {
            return Err(ConformanceError::EmptyRights);
        }
        if rights.bits() & !Rights::ENDPOINT.bits() != 0
            || !(rights.contains(Rights::SEND) || rights.contains(Rights::RECEIVE))
        {
            return Err(ConformanceError::InvalidEndpointRights);
        }
    }
    Ok(())
}

/// Validates one message shape against a protocol's declared transport bounds.
pub fn validate_message_shape<P: ServiceProtocol>(
    bytes: usize,
    handles: usize,
) -> Result<(), ConformanceError> {
    validate_protocol::<P>()?;
    let descriptor = P::descriptor();
    if bytes > descriptor.limits.max_body_bytes as usize {
        return Err(ConformanceError::MessageLimit);
    }
    if handles > descriptor.limits.max_handles as usize {
        return Err(ConformanceError::HandleLimit);
    }
    Ok(())
}

fn valid_protocol_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 || !name.as_bytes().contains(&b'.') {
        return false;
    }
    let mut segment_start = true;
    for byte in name.bytes() {
        if byte == b'.' {
            if segment_start {
                return false;
            }
            segment_start = true;
            continue;
        }
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-') {
            return false;
        }
        segment_start = false;
    }
    !segment_start
}

/// Client side of a typed service endpoint.
#[derive(Debug)]
pub enum Client {}

/// Server side of a typed service endpoint.
#[derive(Debug)]
pub enum Server {}

/// Compile-time endpoint side for a service binding.
pub trait BindingSide: private::Sealed {
    fn required_rights<P: ServiceProtocol>() -> Rights;
}

impl BindingSide for Client {
    fn required_rights<P: ServiceProtocol>() -> Rights {
        P::CLIENT_RIGHTS
    }
}

impl BindingSide for Server {
    fn required_rights<P: ServiceProtocol>() -> Rights {
        P::SERVER_RIGHTS
    }
}

/// Error from binding an already-owned endpoint, preserving that ownership.
#[derive(Debug)]
pub struct BindError {
    error: BindErrorKind,
    endpoint: OwnedHandle<Endpoint>,
}

/// Why an owned endpoint could not become a typed service binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindErrorKind {
    InvalidProtocol(ConformanceError),
    InsufficientRights,
    Kernel(ipc::Error),
}

impl BindError {
    pub const fn error(&self) -> BindErrorKind {
        self.error
    }

    pub fn into_endpoint(self) -> OwnedHandle<Endpoint> {
        self.endpoint
    }
}

/// Endpoint authority tied to one protocol contract and endpoint side.
#[derive(Debug)]
pub struct ServiceBinding<P: ServiceProtocol, S: BindingSide> {
    endpoint: OwnedHandle<Endpoint>,
    protocol: PhantomData<(P, S)>,
}

impl<P: ServiceProtocol, S: BindingSide> ServiceBinding<P, S> {
    pub fn from_context<R: RuntimeRole, const N: usize>(
        context: &mut ProcessContext<R, N>,
        role: CapabilityRole,
    ) -> Result<Self, ContextError> {
        validate_protocol::<P>().map_err(ContextError::InvalidProtocol)?;
        let endpoint = context.take::<Endpoint>(role, S::required_rights::<P>())?;
        Ok(Self {
            endpoint,
            protocol: PhantomData,
        })
    }

    pub fn bind(mut endpoint: OwnedHandle<Endpoint>) -> Result<Self, BindError> {
        if let Err(error) = validate_protocol::<P>() {
            return Err(BindError {
                error: BindErrorKind::InvalidProtocol(error),
                endpoint,
            });
        }
        let required = S::required_rights::<P>();
        let rights = match endpoint.info() {
            Ok(info) => info.rights,
            Err(error) => {
                return Err(BindError {
                    error: BindErrorKind::Kernel(error),
                    endpoint,
                });
            }
        };
        if !rights.contains(required) {
            return Err(BindError {
                error: BindErrorKind::InsufficientRights,
                endpoint,
            });
        }
        if rights != required
            && let Err(error) = endpoint.replace_rights(required)
        {
            return Err(BindError {
                error: BindErrorKind::Kernel(error),
                endpoint,
            });
        }
        Ok(Self {
            endpoint,
            protocol: PhantomData,
        })
    }

    pub fn descriptor(&self) -> ProtocolDescriptor<'static> {
        P::descriptor()
    }

    pub fn endpoint(&self) -> BorrowedHandle<'_, Endpoint> {
        self.endpoint.borrow()
    }

    pub fn into_endpoint(self) -> OwnedHandle<Endpoint> {
        self.endpoint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nswp_core::{ConnectionLimits, MinorVersionProfile, ProtocolId};

    const TEST_PROTOCOL_ID: ProtocolId = match ProtocolId::from_bytes([
        0x10, 0x61, 0xa2, 0x41, 0xb6, 0x42, 0x4d, 0x58, 0x80, 0x55, 0x61, 0xf2, 0x8d, 0xec, 0x93,
        0x21,
    ]) {
        Ok(protocol_id) => protocol_id,
        Err(_) => panic!("test protocol id must be canonical"),
    };
    static TEST_VERSIONS: [MinorVersionProfile; 1] = [MinorVersionProfile {
        minor: 0,
        minimum_body_bytes: 1,
        minimum_handles: 0,
    }];

    struct Conforming;

    impl ServiceProtocol for Conforming {
        const NAME: &'static str = "test.runtime";
        const CLIENT_RIGHTS: Rights = Rights::SEND;
        const SERVER_RIGHTS: Rights = Rights::RECEIVE;

        fn descriptor() -> ProtocolDescriptor<'static> {
            ProtocolDescriptor {
                protocol_id: TEST_PROTOCOL_ID,
                major: 1,
                min_minor: 0,
                max_minor: 0,
                limits: ConnectionLimits {
                    max_body_bytes: 64,
                    max_handles: 0,
                    max_outstanding: 2,
                },
                requested_features: &[],
                available_features: &[],
                versions: &TEST_VERSIONS,
                feature_set_fits: nswp_runtime::no_features_fit,
                methods: &[],
            }
        }
    }

    struct InvalidName;

    impl ServiceProtocol for InvalidName {
        const NAME: &'static str = "TestRuntime";
        const CLIENT_RIGHTS: Rights = Rights::SEND;
        const SERVER_RIGHTS: Rights = Rights::RECEIVE;

        fn descriptor() -> ProtocolDescriptor<'static> {
            Conforming::descriptor()
        }
    }

    #[test]
    fn protocol_conformance_checks_identity_and_message_bounds() {
        assert_eq!(validate_protocol::<Conforming>(), Ok(()));
        assert_eq!(validate_message_shape::<Conforming>(64, 0), Ok(()));
        assert_eq!(
            validate_message_shape::<Conforming>(65, 0),
            Err(ConformanceError::MessageLimit)
        );
        assert_eq!(
            validate_message_shape::<Conforming>(1, 1),
            Err(ConformanceError::HandleLimit)
        );
        assert_eq!(
            validate_protocol::<InvalidName>(),
            Err(ConformanceError::InvalidName)
        );
    }

    #[test]
    fn startup_message_round_trips_canonical_resources() {
        let resources = [
            Some(StartupResource {
                role: CapabilityRole::LIFECYCLE,
                required: true,
            }),
            Some(StartupResource {
                role: CapabilityRole::LOGGING,
                required: false,
            }),
            None,
            None,
        ];
        let message = StartupMessage::new(StartupRuntimeRole::Service, resources).unwrap();
        let mut bytes = [0xa5; 64];
        let length = message.encode(&mut bytes).unwrap();
        assert_eq!(length, 32);
        assert_eq!(StartupMessage::<4>::decode(&bytes[..length]), Ok(message));
        assert!(bytes[length..].iter().all(|byte| *byte == 0xa5));
    }

    #[test]
    fn startup_message_rejects_ambiguous_and_noncanonical_resources() {
        let duplicate = [
            Some(StartupResource {
                role: CapabilityRole::LOGGING,
                required: true,
            }),
            Some(StartupResource {
                role: CapabilityRole::LOGGING,
                required: false,
            }),
        ];
        assert_eq!(
            StartupMessage::new(StartupRuntimeRole::Service, duplicate),
            Err(StartupError::DuplicateRole(CapabilityRole::LOGGING))
        );

        let message = StartupMessage::new(
            StartupRuntimeRole::Application,
            [Some(StartupResource {
                role: CapabilityRole::LIFECYCLE,
                required: true,
            })],
        )
        .unwrap();
        let mut bytes = [0; 24];
        message.encode(&mut bytes).unwrap();
        bytes[22] = 1;
        assert_eq!(
            StartupMessage::<1>::decode(&bytes),
            Err(StartupError::MalformedMessage)
        );
    }

    #[test]
    fn role_specific_context_rejects_ambiguous_authority() {
        let first = unsafe { OwnedHandle::<AnyObject>::from_raw(101) }.unwrap();
        let second = unsafe { OwnedHandle::<AnyObject>::from_raw(102) }.unwrap();
        let capabilities = [
            Some(ContextCapability::new(CapabilityRole::LOGGING, first)),
            Some(ContextCapability::new(CapabilityRole::LOGGING, second)),
        ];
        let (capabilities, error) =
            ProcessContext::<ServiceProcess, 2>::new(capabilities).unwrap_err();
        assert_eq!(error, ContextError::DuplicateRole(CapabilityRole::LOGGING));
        assert_eq!(capabilities.iter().flatten().count(), 2);
    }

    #[test]
    fn empty_context_retains_its_compile_time_role() {
        let context = ProcessContext::<ApplicationProcess, 3>::default();
        assert_eq!(context.runtime_role(), "application");
        assert!(context.is_empty());
        assert!(!context.contains(CapabilityRole::SERVICE_NAMESPACE));
    }
}
