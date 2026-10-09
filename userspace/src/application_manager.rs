//! Private, parent-authenticated control transport for a live application portal.
//!
//! A binding is an attestation from the launch manager, never an application-supplied identity.
//! Only this module decodes it, after checking the kernel sender and manager generation.

use crate::{
    abi::limits,
    application_identity::{
        ApplicationIdentityMetadata, ApplicationInstallation, ApplicationLaunchSelection,
        ApplicationProfile, ApplicationProfileSet, AuthorizedApplication,
        InstalledApplicationComponent, PackageVerification, authorize_application_launch,
    },
    handle::{Endpoint, OwnedHandle, ReceivedMessage},
    ipc::{self, Deadline, Rights, Signals},
    managed_startup::{
        ManagedToolCapability, ManagedToolCommand, ManagedToolStartOrigin,
        send_managed_tool_start_with_capabilities,
    },
    process_start::PROCESS_START_BOOTSTRAP_SLOT,
    syscall::{self, SpawnFlags},
};

pub const CONTROL_RIGHTS: Rights = Rights::SEND.union(Rights::RECEIVE).union(Rights::WAIT);
pub const CONTROL_BYTES: usize = 144;
pub const CONTROL_TIMEOUT_NS: u64 = 60_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)] // Bounded no-allocator wire dispatch; no heap indirection.
pub enum PortalManagerCommand {
    Bind {
        process_id: u64,
        authorization: AuthorizedApplication,
    },
    Unbind {
        process_id: u64,
    },
    Shutdown,
}

impl PortalManagerCommand {
    pub fn encode(self, generation: u64, sequence: u64) -> [u8; CONTROL_BYTES] {
        let mut bytes = [0; CONTROL_BYTES];
        bytes[..4].copy_from_slice(b"NSPM");
        bytes[4] = 1;
        put(&mut bytes, 8, generation);
        put(&mut bytes, 16, sequence);
        match self {
            Self::Bind {
                process_id,
                authorization,
            } => {
                bytes[5] = 1;
                put(&mut bytes, 24, process_id);
                bytes[32..104].copy_from_slice(&authorization.encode_metadata());
                let identity = authorization.identity();
                put(&mut bytes, 104, identity.component);
                put(&mut bytes, 112, identity.user);
                put(&mut bytes, 120, identity.session);
                put(
                    &mut bytes,
                    128,
                    authorization.profile().namespace_profile_id(),
                );
            }
            Self::Unbind { process_id } => {
                bytes[5] = 2;
                put(&mut bytes, 24, process_id);
            }
            Self::Shutdown => bytes[5] = 3,
        }
        bytes
    }
}

/// Decoder state is private to the authenticated manager ingress. Sequence numbers never wrap.
pub struct PortalManagerAdmission {
    manager: u64,
    generation: u64,
    session: u64,
    last_sequence: u64,
}

impl PortalManagerAdmission {
    pub fn new(manager: u64, generation: u64, session: u64) -> Option<Self> {
        (manager != 0 && generation != 0 && session != 0).then_some(Self {
            manager,
            generation,
            session,
            last_sequence: 0,
        })
    }

    pub fn admit(&mut self, sender: u64, bytes: &[u8]) -> Option<(u64, PortalManagerCommand)> {
        if sender != self.manager
            || bytes.len() != CONTROL_BYTES
            || bytes[..4] != *b"NSPM"
            || bytes[4] != 1
            || bytes[6..8] != [0; 2]
            || bytes[136..] != [0; 8]
            || get(bytes, 8) != self.generation
            || get(bytes, 16) <= self.last_sequence
        {
            return None;
        }
        let process_id = get(bytes, 24);
        let command = match bytes[5] {
            1 if process_id != 0 => {
                let metadata = ApplicationIdentityMetadata::decode(&bytes[32..104])?;
                let component_id = get(bytes, 104);
                let user = get(bytes, 112);
                let session = get(bytes, 120);
                if session != self.session
                    || get(bytes, 128) != ApplicationProfile::Desktop.namespace_profile_id()
                {
                    return None;
                }
                // This proof is used solely for portal grants. It has no authorized executable,
                // so it cannot be reused to launch a component through application_launch.
                let components = [InstalledApplicationComponent {
                    component: component_id,
                    executable: 1,
                    profiles: ApplicationProfileSet::DESKTOP,
                    entry: true,
                }];
                let p = metadata.principal;
                let verification = PackageVerification {
                    package: metadata.package,
                    package_generation: metadata.package_generation,
                    application: p.application,
                    publisher: p.publisher,
                    signing_lineage: p.signing_lineage,
                    trust_class: p.trust_class,
                    system_application: p.system_application,
                    components: &components,
                };
                let installation = ApplicationInstallation {
                    installation: metadata.provenance.installation,
                    package: metadata.package,
                    package_generation: metadata.package_generation,
                    application: p.application,
                    publisher: p.publisher,
                    signing_lineage: p.signing_lineage,
                    trust_class: p.trust_class,
                    scope: metadata.provenance.scope,
                    owner_user: user,
                    system_application: p.system_application,
                };
                let authorization = authorize_application_launch(
                    verification,
                    installation,
                    ApplicationLaunchSelection {
                        component: component_id,
                        user,
                        session,
                        profile: ApplicationProfile::Desktop,
                    },
                )
                .ok()?;
                PortalManagerCommand::Bind {
                    process_id,
                    authorization: authorization.without_executables(),
                }
            }
            2 if process_id != 0 && bytes[32..136] == [0; 104] => {
                PortalManagerCommand::Unbind { process_id }
            }
            3 if bytes[24..136] == [0; 112] => PortalManagerCommand::Shutdown,
            _ => return None,
        };
        self.last_sequence = get(bytes, 16);
        Some((self.last_sequence, command))
    }
}

pub fn put(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub fn get(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("checked wire length"),
    )
}

/// Serial manager client; a failed exchange is outcome-unknown and must not be retried.
pub struct PortalManagerClient {
    endpoint: OwnedHandle<Endpoint>,
    portal: u64,
    generation: u64,
    next_sequence: u64,
    failed: bool,
}

impl PortalManagerClient {
    pub fn new(endpoint: OwnedHandle<Endpoint>, portal: u64, generation: u64) -> Option<Self> {
        (portal != 0 && generation != 0).then_some(Self {
            endpoint,
            portal,
            generation,
            next_sequence: 1,
            failed: false,
        })
    }

    pub fn command(&mut self, command: PortalManagerCommand) -> ipc::Result<()> {
        if self.failed {
            return Err(ipc::Error::INVALID_ARGUMENT);
        }
        self.failed = true;
        let sequence = self.next_sequence;
        self.next_sequence = sequence
            .checked_add(1)
            .ok_or(ipc::Error::INVALID_ARGUMENT)?;
        self.endpoint
            .send(&command.encode(self.generation, sequence))?;
        let mut bytes = [0; limits::MAX_IPC_MESSAGE_BYTES];
        let message = receive(&self.endpoint, &mut bytes)?;
        if message.sender_process_id != self.portal
            || message.capability.is_some()
            || message.bytes != 24
            || bytes[..8] != *b"NSPA\x01\x01\0\0"
            || get(&bytes, 8) != self.generation
            || get(&bytes, 16) != sequence
        {
            return Err(ipc::Error::INVALID_ARGUMENT);
        }
        self.failed = false;
        Ok(())
    }
}

/// Application manager integration: binds the blocked root before launch release and withdraws
/// portal authority when supervision leaves the starting/running states.
pub struct PortalManagedApplication<const N: usize> {
    supervised: crate::application_lifecycle::SupervisedApplication<N>,
    bound: bool,
}

impl<const N: usize> PortalManagedApplication<N> {
    pub fn launch(
        client: &mut PortalManagerClient,
        launch: crate::application_launch::ApplicationLaunch<'_, N>,
        readiness: OwnedHandle<Endpoint>,
        policy: crate::application_lifecycle::ApplicationLifecyclePolicy,
    ) -> Result<Self, crate::application_launch::ApplicationLaunchError> {
        use crate::application_launch::{ApplicationLaunchError, spawn_application_with_admission};
        let authorization = launch.authorization;
        let mut bound_process = None;
        let result = spawn_application_with_admission(launch, |process_id| {
            client
                .command(PortalManagerCommand::Bind {
                    process_id,
                    authorization,
                })
                .map_err(|_| ApplicationLaunchError::AdmissionRejected)?;
            bound_process = Some(process_id);
            Ok(())
        });
        let instance = match result {
            Ok(instance) => instance,
            Err(error) => {
                if let Some(process_id) = bound_process {
                    let _ = client.command(PortalManagerCommand::Unbind { process_id });
                }
                return Err(error);
            }
        };
        // Validate before moving the instance into the existing supervisor.
        if !readiness
            .info()
            .is_ok_and(|i| i.kind == ipc::ObjectKind::Endpoint && i.rights == Rights::ENDPOINT)
        {
            let _ = client.command(PortalManagerCommand::Unbind {
                process_id: instance.process_id,
            });
            let _ = ipc::job_terminate(instance.job.as_raw());
            let _ = syscall::wait_child(instance.process_id);
            return Err(ApplicationLaunchError::InvalidDescription);
        }
        let supervised =
            crate::application_lifecycle::SupervisedApplication::new(instance, readiness, policy)
                .map_err(|_| ApplicationLaunchError::InvalidDescription)?;
        Ok(Self {
            supervised,
            bound: true,
        })
    }

    pub fn process_id(&self) -> u64 {
        self.supervised.instance().process_id
    }

    pub fn unbind(&mut self, client: &mut PortalManagerClient) -> ipc::Result<()> {
        if self.bound {
            client.command(PortalManagerCommand::Unbind {
                process_id: self.process_id(),
            })?;
            self.bound = false;
        }
        Ok(())
    }

    pub fn poll(
        &mut self,
        client: &mut PortalManagerClient,
    ) -> Result<crate::application_lifecycle::ApplicationLifecycleState, ipc::Error> {
        use crate::application_lifecycle::ApplicationLifecycleState;
        let state = self
            .supervised
            .poll()
            .map_err(|_| ipc::Error::INVALID_ARGUMENT)?;
        if !matches!(
            state,
            ApplicationLifecycleState::Starting | ApplicationLifecycleState::Running
        ) {
            self.unbind(client)?;
        }
        Ok(state)
    }
}

impl<const N: usize> Drop for PortalManagedApplication<N> {
    fn drop(&mut self) {
        let _ = self.supervised.request_termination(
            crate::application_lifecycle::ApplicationTerminationReason::ManagerShutdown,
        );
        // Supervision drains the job on normal completion. Error unwinding remains bounded.
        for _ in 0..4096 {
            if matches!(
                self.supervised.poll(),
                Ok(
                    crate::application_lifecycle::ApplicationLifecycleState::Stopped
                        | crate::application_lifecycle::ApplicationLifecycleState::Completed
                        | crate::application_lifecycle::ApplicationLifecycleState::Failed
                )
            ) {
                break;
            }
            let _ = syscall::yield_now();
        }
    }
}

/// Receives with an absolute timeout; malformed attached capabilities remain RAII-owned.
pub fn receive(
    endpoint: &OwnedHandle<Endpoint>,
    bytes: &mut [u8; limits::MAX_IPC_MESSAGE_BYTES],
) -> ipc::Result<ReceivedMessage> {
    let now = crate::platform::monotonic_time_ns().map_err(|_| ipc::Error::INVALID_ARGUMENT)?;
    let deadline = Deadline::from_monotonic_ns(now.saturating_add(CONTROL_TIMEOUT_NS));
    loop {
        match endpoint.try_receive(bytes) {
            Err(error) if error == ipc::Error::TRY_AGAIN => {
                let signals = endpoint.wait(Signals::READABLE | Signals::PEER_CLOSED, deadline)?;
                if signals.contains(Signals::PEER_CLOSED) && !signals.contains(Signals::READABLE) {
                    return Err(ipc::Error::INVALID_ARGUMENT);
                }
            }
            result => return result,
        }
    }
}

/// Managed child launch with the same isolation handshake and startup contract as PID 1 tools.
/// Callers retain and supervise the returned PID; failed startup kills and reaps the child.
pub fn spawn<const N: usize>(
    command: &[u8],
    capabilities: &[ManagedToolCapability; N],
) -> Result<u64, syscall::Errno> {
    let barrier = syscall::LaunchBarrier::new()?;
    let child = syscall::spawn_command_with_barrier(
        command,
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
        &barrier,
    )?;
    let result = (|| {
        let (sender, receiver) =
            OwnedHandle::<Endpoint>::create_pair().map_err(|_| syscall::Errno::IO)?;
        ipc::grant_child(
            child,
            receiver.as_raw(),
            Rights::RECEIVE,
            PROCESS_START_BOOTSTRAP_SLOT,
        )
        .map_err(|_| syscall::Errno::IO)?;
        send_managed_tool_start_with_capabilities(
            sender.as_raw(),
            child,
            ManagedToolCommand::new(command, &[]),
            ManagedToolStartOrigin::Parent,
            capabilities,
        )
        .map_err(|_| syscall::Errno::IO)?;
        drop(sender);
        drop(receiver);
        barrier.release()?;
        Ok(child)
    })();
    if result.is_err() {
        let _ = crate::platform::kill(child, crate::abi::signal::KILL);
        let _ = syscall::wait_child(child);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_control_rejects_forgery_replay_and_noncanonical_commands() {
        let mut admission = PortalManagerAdmission::new(10, 2, 3).unwrap();
        let bytes = PortalManagerCommand::Unbind { process_id: 50 }.encode(2, 1);
        assert!(admission.admit(11, &bytes).is_none());
        assert!(admission.admit(10, &bytes[..143]).is_none());
        let mut malformed = bytes;
        malformed[100] = 1;
        assert!(admission.admit(10, &malformed).is_none());
        assert!(
            admission
                .admit(10, &PortalManagerCommand::Shutdown.encode(1, 1))
                .is_none()
        );
        assert_eq!(
            admission.admit(10, &bytes),
            Some((1, PortalManagerCommand::Unbind { process_id: 50 }))
        );
        assert!(admission.admit(10, &bytes).is_none());
        assert_eq!(
            admission.admit(10, &PortalManagerCommand::Shutdown.encode(2, 2)),
            Some((2, PortalManagerCommand::Shutdown))
        );
    }
}
