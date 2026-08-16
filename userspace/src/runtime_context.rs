//! Typed, capability-bearing process contexts and service bindings.
//!
//! A [`ProcessContext`] owns the explicit capabilities supplied to one runtime
//! role. Capabilities are selected by stable role identifiers, validated against
//! their kernel object kind, and tightened to the exact rights requested by the
//! consumer. [`ServiceBinding`] adds a protocol and endpoint-side type without
//! turning a discoverable protocol name into authority.

use core::{array, marker::PhantomData};

use crate::{
    abi::limits,
    handle::{AnyObject, BorrowedHandle, Endpoint, KnownObjectType, ObjectType, OwnedHandle},
    ipc::{self, Rights},
};

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
}

impl RuntimeRole for ServiceProcess {
    const NAME: &'static str = "service";
}

impl RuntimeRole for DriverProcess {
    const NAME: &'static str = "driver";
}

impl RuntimeRole for ToolProcess {
    const NAME: &'static str = "tool";
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

impl<R: RuntimeRole, const N: usize> Default for ProcessContext<R, N> {
    fn default() -> Self {
        Self {
            capabilities: array::from_fn(|_| None),
            role: PhantomData,
        }
    }
}

/// Version and bounded transport contract for one service protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolDescriptor {
    pub name: &'static str,
    pub major: u16,
    pub minor: u16,
    pub max_message_bytes: usize,
    pub max_handles: usize,
}

/// Static protocol contract used to type a service endpoint.
pub trait ServiceProtocol {
    const DESCRIPTOR: ProtocolDescriptor;
    const CLIENT_RIGHTS: Rights;
    const SERVER_RIGHTS: Rights;
}

/// Protocol-side validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConformanceError {
    InvalidName,
    InvalidVersion,
    MessageLimit,
    HandleLimit,
    EmptyRights,
    InvalidEndpointRights,
}

/// Validates the common, allocation-free part of a protocol declaration.
pub fn validate_protocol<P: ServiceProtocol>() -> Result<(), ConformanceError> {
    let descriptor = P::DESCRIPTOR;
    if !valid_protocol_name(descriptor.name) {
        return Err(ConformanceError::InvalidName);
    }
    if descriptor.major == 0 {
        return Err(ConformanceError::InvalidVersion);
    }
    if descriptor.max_message_bytes == 0
        || descriptor.max_message_bytes > limits::MAX_IPC_MESSAGE_BYTES
    {
        return Err(ConformanceError::MessageLimit);
    }
    if descriptor.max_handles > limits::MAX_IPC_MESSAGE_HANDLES {
        return Err(ConformanceError::HandleLimit);
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
    if bytes > P::DESCRIPTOR.max_message_bytes {
        return Err(ConformanceError::MessageLimit);
    }
    if handles > P::DESCRIPTOR.max_handles {
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

    pub const fn descriptor(&self) -> ProtocolDescriptor {
        P::DESCRIPTOR
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

    struct Conforming;

    impl ServiceProtocol for Conforming {
        const DESCRIPTOR: ProtocolDescriptor = ProtocolDescriptor {
            name: "test.runtime",
            major: 1,
            minor: 0,
            max_message_bytes: 64,
            max_handles: 1,
        };
        const CLIENT_RIGHTS: Rights = Rights::SEND;
        const SERVER_RIGHTS: Rights = Rights::RECEIVE;
    }

    struct InvalidName;

    impl ServiceProtocol for InvalidName {
        const DESCRIPTOR: ProtocolDescriptor = ProtocolDescriptor {
            name: "TestRuntime",
            ..Conforming::DESCRIPTOR
        };
        const CLIENT_RIGHTS: Rights = Rights::SEND;
        const SERVER_RIGHTS: Rights = Rights::RECEIVE;
    }

    #[test]
    fn protocol_conformance_checks_identity_and_message_bounds() {
        assert_eq!(validate_protocol::<Conforming>(), Ok(()));
        assert_eq!(validate_message_shape::<Conforming>(64, 1), Ok(()));
        assert_eq!(
            validate_message_shape::<Conforming>(65, 1),
            Err(ConformanceError::MessageLimit)
        );
        assert_eq!(
            validate_message_shape::<Conforming>(1, 2),
            Err(ConformanceError::HandleLimit)
        );
        assert_eq!(
            validate_protocol::<InvalidName>(),
            Err(ConformanceError::InvalidName)
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
