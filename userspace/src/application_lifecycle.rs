//! Bounded lifecycle supervision for one managed application job.

use crate::{
    abi::limits,
    application_launch::{ApplicationInstance, ApplicationProfile},
    handle::{Endpoint, OwnedHandle},
    ipc::{self, ObjectKind, Rights},
    syscall::{self, ChildStatus},
};

pub const APPLICATION_READY_MESSAGE: &[u8] = b"application-ready:v1";
pub const MAX_APPLICATION_RELAUNCHES: u32 = 8;
pub const MAX_APPLICATION_LIFECYCLE_YIELDS: u32 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationLifecyclePolicy {
    readiness_yields: u32,
    relaunch_limit: u32,
    relaunch_backoff_yields: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationLifecyclePolicyError {
    ZeroReadinessDeadline,
    RelaunchLimitExceeded,
    YieldLimitExceeded,
}

impl ApplicationLifecyclePolicy {
    pub const fn new(
        readiness_yields: u32,
        relaunch_limit: u32,
        relaunch_backoff_yields: u32,
    ) -> Result<Self, ApplicationLifecyclePolicyError> {
        if readiness_yields == 0 {
            return Err(ApplicationLifecyclePolicyError::ZeroReadinessDeadline);
        }
        if relaunch_limit > MAX_APPLICATION_RELAUNCHES {
            return Err(ApplicationLifecyclePolicyError::RelaunchLimitExceeded);
        }
        if readiness_yields > MAX_APPLICATION_LIFECYCLE_YIELDS
            || relaunch_backoff_yields > MAX_APPLICATION_LIFECYCLE_YIELDS
        {
            return Err(ApplicationLifecyclePolicyError::YieldLimitExceeded);
        }
        Ok(Self {
            readiness_yields,
            relaunch_limit,
            relaunch_backoff_yields,
        })
    }

    pub const fn readiness_yields(self) -> u32 {
        self.readiness_yields
    }

    pub const fn relaunch_limit(self) -> u32 {
        self.relaunch_limit
    }

    pub const fn relaunch_backoff_yields(self) -> u32 {
        self.relaunch_backoff_yields
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationLifecycleState {
    Starting,
    Running,
    Draining,
    Backoff,
    RelaunchPending,
    Completed,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationFailure {
    ReadinessTimeout,
    InvalidReadiness,
    ExitedBeforeReadiness(ChildStatus),
    RootFailure(ChildStatus),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationTerminationReason {
    User,
    SessionTeardown,
    ManagerShutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainOutcome {
    Completed,
    Stopped(ApplicationTerminationReason),
    Failed(ApplicationFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationLifecycleAction {
    None,
    TerminateJob,
    Relaunch { attempt: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationReadyDisposition {
    Accepted,
    Rejected,
    Stale,
}

/// Pure application lifecycle state machine. Kernel operations are performed by
/// [`SupervisedApplication`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationLifecycle {
    policy: ApplicationLifecyclePolicy,
    state: ApplicationLifecycleState,
    root_process_id: u64,
    attempt: u32,
    relaunch_count: u32,
    readiness_yields_remaining: u32,
    backoff_yields_remaining: u32,
    drain_outcome: Option<DrainOutcome>,
    last_failure: Option<ApplicationFailure>,
    termination_reason: Option<ApplicationTerminationReason>,
}

impl ApplicationLifecycle {
    pub const fn new(root_process_id: u64, policy: ApplicationLifecyclePolicy) -> Option<Self> {
        if root_process_id == 0 {
            return None;
        }
        Some(Self {
            policy,
            state: ApplicationLifecycleState::Starting,
            root_process_id,
            attempt: 1,
            relaunch_count: 0,
            readiness_yields_remaining: policy.readiness_yields,
            backoff_yields_remaining: 0,
            drain_outcome: None,
            last_failure: None,
            termination_reason: None,
        })
    }

    pub const fn state(self) -> ApplicationLifecycleState {
        self.state
    }

    pub const fn root_process_id(self) -> u64 {
        self.root_process_id
    }

    pub const fn attempt(self) -> u32 {
        self.attempt
    }

    pub const fn relaunch_count(self) -> u32 {
        self.relaunch_count
    }

    pub const fn last_failure(self) -> Option<ApplicationFailure> {
        self.last_failure
    }

    pub const fn termination_reason(self) -> Option<ApplicationTerminationReason> {
        self.termination_reason
    }

    pub fn observe_readiness(
        &mut self,
        sender_process_id: u64,
        valid_message: bool,
    ) -> (ApplicationReadyDisposition, ApplicationLifecycleAction) {
        if self.state != ApplicationLifecycleState::Starting {
            return (
                ApplicationReadyDisposition::Stale,
                ApplicationLifecycleAction::None,
            );
        }
        if sender_process_id != self.root_process_id || !valid_message {
            self.begin_failure(ApplicationFailure::InvalidReadiness);
            return (
                ApplicationReadyDisposition::Rejected,
                ApplicationLifecycleAction::TerminateJob,
            );
        }
        self.state = ApplicationLifecycleState::Running;
        self.readiness_yields_remaining = 0;
        (
            ApplicationReadyDisposition::Accepted,
            ApplicationLifecycleAction::None,
        )
    }

    pub fn tick(&mut self) -> ApplicationLifecycleAction {
        match self.state {
            ApplicationLifecycleState::Starting => {
                if self.readiness_yields_remaining <= 1 {
                    self.readiness_yields_remaining = 0;
                    self.begin_failure(ApplicationFailure::ReadinessTimeout);
                    ApplicationLifecycleAction::TerminateJob
                } else {
                    self.readiness_yields_remaining -= 1;
                    ApplicationLifecycleAction::None
                }
            }
            ApplicationLifecycleState::Backoff => {
                if self.backoff_yields_remaining <= 1 {
                    self.backoff_yields_remaining = 0;
                    self.state = ApplicationLifecycleState::RelaunchPending;
                    ApplicationLifecycleAction::Relaunch {
                        attempt: self.attempt.saturating_add(1),
                    }
                } else {
                    self.backoff_yields_remaining -= 1;
                    ApplicationLifecycleAction::None
                }
            }
            _ => ApplicationLifecycleAction::None,
        }
    }

    pub fn observe_root_exit(&mut self, status: ChildStatus) -> ApplicationLifecycleAction {
        match self.state {
            ApplicationLifecycleState::Starting => {
                self.begin_failure(ApplicationFailure::ExitedBeforeReadiness(status));
                ApplicationLifecycleAction::TerminateJob
            }
            ApplicationLifecycleState::Running => {
                self.drain_outcome = Some(if status.success() {
                    DrainOutcome::Completed
                } else {
                    let failure = ApplicationFailure::RootFailure(status);
                    self.last_failure = Some(failure);
                    DrainOutcome::Failed(failure)
                });
                self.state = ApplicationLifecycleState::Draining;
                ApplicationLifecycleAction::TerminateJob
            }
            _ => ApplicationLifecycleAction::None,
        }
    }

    pub fn request_termination(
        &mut self,
        reason: ApplicationTerminationReason,
    ) -> ApplicationLifecycleAction {
        self.termination_reason = Some(reason);
        match self.state {
            ApplicationLifecycleState::Starting | ApplicationLifecycleState::Running => {
                self.state = ApplicationLifecycleState::Draining;
                self.drain_outcome = Some(DrainOutcome::Stopped(reason));
                ApplicationLifecycleAction::TerminateJob
            }
            ApplicationLifecycleState::Draining => {
                self.drain_outcome = Some(DrainOutcome::Stopped(reason));
                ApplicationLifecycleAction::None
            }
            ApplicationLifecycleState::Backoff | ApplicationLifecycleState::RelaunchPending => {
                self.state = ApplicationLifecycleState::Stopped;
                self.drain_outcome = None;
                self.backoff_yields_remaining = 0;
                ApplicationLifecycleAction::None
            }
            _ => ApplicationLifecycleAction::None,
        }
    }

    pub fn note_job_drained(&mut self) -> ApplicationLifecycleAction {
        if self.state != ApplicationLifecycleState::Draining {
            return ApplicationLifecycleAction::None;
        }
        match self.drain_outcome.take() {
            Some(DrainOutcome::Completed) => {
                self.state = ApplicationLifecycleState::Completed;
                ApplicationLifecycleAction::None
            }
            Some(DrainOutcome::Stopped(reason)) => {
                self.termination_reason = Some(reason);
                self.state = ApplicationLifecycleState::Stopped;
                ApplicationLifecycleAction::None
            }
            Some(DrainOutcome::Failed(failure)) => {
                self.last_failure = Some(failure);
                if self.relaunch_count >= self.policy.relaunch_limit {
                    self.state = ApplicationLifecycleState::Failed;
                    return ApplicationLifecycleAction::None;
                }
                self.relaunch_count += 1;
                if self.policy.relaunch_backoff_yields == 0 {
                    self.state = ApplicationLifecycleState::RelaunchPending;
                    ApplicationLifecycleAction::Relaunch {
                        attempt: self.attempt.saturating_add(1),
                    }
                } else {
                    self.state = ApplicationLifecycleState::Backoff;
                    self.backoff_yields_remaining = self.policy.relaunch_backoff_yields;
                    ApplicationLifecycleAction::None
                }
            }
            None => {
                self.state = ApplicationLifecycleState::Failed;
                ApplicationLifecycleAction::None
            }
        }
    }

    pub fn note_relaunched(&mut self, root_process_id: u64) -> bool {
        if self.state != ApplicationLifecycleState::RelaunchPending || root_process_id == 0 {
            return false;
        }
        let Some(attempt) = self.attempt.checked_add(1) else {
            self.state = ApplicationLifecycleState::Failed;
            return false;
        };
        self.root_process_id = root_process_id;
        self.attempt = attempt;
        self.readiness_yields_remaining = self.policy.readiness_yields;
        self.backoff_yields_remaining = 0;
        self.drain_outcome = None;
        self.state = ApplicationLifecycleState::Starting;
        true
    }

    fn begin_failure(&mut self, failure: ApplicationFailure) {
        self.last_failure = Some(failure);
        self.drain_outcome = Some(DrainOutcome::Failed(failure));
        self.state = ApplicationLifecycleState::Draining;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationSupervisionError {
    InvalidReadinessEndpoint,
    Readiness(ipc::Error),
    Job(ipc::Error),
    Reap {
        process_id: u64,
        source: syscall::Errno,
    },
    ReapStatusMismatch {
        process_id: u64,
        expected: ChildStatus,
        actual: ChildStatus,
    },
    InvalidState,
    IdentityMismatch,
}

/// Owns one application job and applies [`ApplicationLifecycle`] decisions to kernel objects.
pub struct SupervisedApplication<const N: usize> {
    instance: ApplicationInstance<N>,
    readiness: Option<OwnedHandle<Endpoint>>,
    lifecycle: ApplicationLifecycle,
    completion_count: usize,
}

impl<const N: usize> SupervisedApplication<N> {
    pub fn new(
        instance: ApplicationInstance<N>,
        readiness: OwnedHandle<Endpoint>,
        policy: ApplicationLifecyclePolicy,
    ) -> Result<Self, ApplicationSupervisionError> {
        validate_readiness_endpoint(&readiness)?;
        let lifecycle = ApplicationLifecycle::new(instance.process_id, policy)
            .ok_or(ApplicationSupervisionError::InvalidState)?;
        Ok(Self {
            instance,
            readiness: Some(readiness),
            lifecycle,
            completion_count: 0,
        })
    }

    pub const fn lifecycle(&self) -> &ApplicationLifecycle {
        &self.lifecycle
    }

    pub const fn instance(&self) -> &ApplicationInstance<N> {
        &self.instance
    }

    pub fn instance_mut(&mut self) -> &mut ApplicationInstance<N> {
        &mut self.instance
    }

    pub const fn completion_count(&self) -> usize {
        self.completion_count
    }

    pub fn poll(&mut self) -> Result<ApplicationLifecycleState, ApplicationSupervisionError> {
        if self.lifecycle.state() == ApplicationLifecycleState::Starting {
            self.poll_readiness()?;
        } else if self.lifecycle.state() == ApplicationLifecycleState::Backoff {
            let action = self.lifecycle.tick();
            self.apply_action(action)?;
        }
        self.drain_available_completions()?;
        Ok(self.lifecycle.state())
    }

    pub fn request_termination(
        &mut self,
        reason: ApplicationTerminationReason,
    ) -> Result<ApplicationLifecycleState, ApplicationSupervisionError> {
        let action = self.lifecycle.request_termination(reason);
        self.readiness.take();
        self.apply_action(action)?;
        self.drain_available_completions()?;
        Ok(self.lifecycle.state())
    }

    pub fn install_relaunch(
        &mut self,
        instance: ApplicationInstance<N>,
        readiness: OwnedHandle<Endpoint>,
    ) -> Result<(), ApplicationSupervisionError> {
        if self.lifecycle.state() != ApplicationLifecycleState::RelaunchPending {
            return Err(ApplicationSupervisionError::InvalidState);
        }
        if instance.identity() != self.instance.identity()
            || instance.profile() != ApplicationProfile::Desktop
            || instance.principal() != self.instance.principal()
            || instance.provenance() != self.instance.provenance()
            || instance.manager_generation() != self.instance.manager_generation()
        {
            return Err(ApplicationSupervisionError::IdentityMismatch);
        }
        validate_readiness_endpoint(&readiness)?;
        if !self.lifecycle.note_relaunched(instance.process_id) {
            return Err(ApplicationSupervisionError::InvalidState);
        }
        self.instance = instance;
        self.readiness = Some(readiness);
        Ok(())
    }

    fn poll_readiness(&mut self) -> Result<(), ApplicationSupervisionError> {
        let Some(readiness) = self.readiness.as_ref() else {
            return Err(ApplicationSupervisionError::InvalidState);
        };
        let mut bytes = [0_u8; limits::MAX_IPC_MESSAGE_BYTES];
        let (disposition, action) = match readiness.try_receive(&mut bytes) {
            Ok(message) => {
                let valid = message.bytes == APPLICATION_READY_MESSAGE.len()
                    && bytes[..message.bytes] == *APPLICATION_READY_MESSAGE
                    && message.capability.is_none();
                let result = self
                    .lifecycle
                    .observe_readiness(message.sender_process_id, valid);
                self.readiness.take();
                result
            }
            Err(error) if error == ipc::Error::TRY_AGAIN => {
                let action = self.lifecycle.tick();
                if action == ApplicationLifecycleAction::TerminateJob {
                    self.readiness.take();
                }
                (ApplicationReadyDisposition::Stale, action)
            }
            Err(error) => return Err(ApplicationSupervisionError::Readiness(error)),
        };
        let _ = disposition;
        self.apply_action(action)
    }

    fn drain_available_completions(&mut self) -> Result<(), ApplicationSupervisionError> {
        for _ in 0..limits::MAX_JOB_PROCESSES {
            match ipc::job_try_wait(self.instance.job.as_raw()) {
                Ok(exit) => {
                    self.reap_completion(exit)?;
                    self.completion_count = self.completion_count.saturating_add(1);
                    if exit.process_id == self.lifecycle.root_process_id() {
                        let action = self.lifecycle.observe_root_exit(exit.status);
                        if action == ApplicationLifecycleAction::TerminateJob {
                            self.readiness.take();
                        }
                        self.apply_action(action)?;
                    }
                }
                Err(error) if error == ipc::Error::TRY_AGAIN => break,
                Err(error) if error == ipc::Error::NO_CHILD => {
                    let action = self.lifecycle.note_job_drained();
                    self.apply_action(action)?;
                    break;
                }
                Err(error) => return Err(ApplicationSupervisionError::Job(error)),
            }
        }
        Ok(())
    }

    fn reap_completion(&self, exit: ipc::JobExit) -> Result<(), ApplicationSupervisionError> {
        match syscall::wait_child(exit.process_id) {
            Ok(actual) if actual == exit.status => Ok(()),
            Ok(actual) => Err(ApplicationSupervisionError::ReapStatusMismatch {
                process_id: exit.process_id,
                expected: exit.status,
                actual,
            }),
            Err(source)
                if source == syscall::Errno::NO_CHILD
                    && exit.process_id != self.lifecycle.root_process_id() =>
            {
                Ok(())
            }
            Err(source) => Err(ApplicationSupervisionError::Reap {
                process_id: exit.process_id,
                source,
            }),
        }
    }

    fn apply_action(
        &self,
        action: ApplicationLifecycleAction,
    ) -> Result<(), ApplicationSupervisionError> {
        if action == ApplicationLifecycleAction::TerminateJob {
            ipc::job_terminate(self.instance.job.as_raw())
                .map(|_| ())
                .map_err(ApplicationSupervisionError::Job)
        } else {
            Ok(())
        }
    }
}

fn validate_readiness_endpoint(
    readiness: &OwnedHandle<Endpoint>,
) -> Result<(), ApplicationSupervisionError> {
    let info = readiness
        .info()
        .map_err(ApplicationSupervisionError::Readiness)?;
    if info.kind != ObjectKind::Endpoint || info.rights != Rights::ENDPOINT || info.size != 0 {
        return Err(ApplicationSupervisionError::InvalidReadinessEndpoint);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(readiness: u32, relaunches: u32, backoff: u32) -> ApplicationLifecyclePolicy {
        ApplicationLifecyclePolicy::new(readiness, relaunches, backoff).unwrap()
    }

    #[test]
    fn policy_bounds_deadlines_relaunches_and_backoff() {
        assert_eq!(
            ApplicationLifecyclePolicy::new(0, 0, 0),
            Err(ApplicationLifecyclePolicyError::ZeroReadinessDeadline)
        );
        assert_eq!(
            ApplicationLifecyclePolicy::new(1, MAX_APPLICATION_RELAUNCHES + 1, 0),
            Err(ApplicationLifecyclePolicyError::RelaunchLimitExceeded)
        );
        assert_eq!(
            ApplicationLifecyclePolicy::new(1, 0, MAX_APPLICATION_LIFECYCLE_YIELDS + 1),
            Err(ApplicationLifecyclePolicyError::YieldLimitExceeded)
        );
    }

    #[test]
    fn readiness_is_root_pinned_and_timeout_relaunch_is_bounded() {
        let mut lifecycle = ApplicationLifecycle::new(10, policy(2, 1, 1)).unwrap();
        assert_eq!(lifecycle.tick(), ApplicationLifecycleAction::None);
        assert_eq!(lifecycle.tick(), ApplicationLifecycleAction::TerminateJob);
        assert_eq!(
            lifecycle.last_failure(),
            Some(ApplicationFailure::ReadinessTimeout)
        );
        assert_eq!(
            lifecycle.note_job_drained(),
            ApplicationLifecycleAction::None
        );
        assert_eq!(lifecycle.state(), ApplicationLifecycleState::Backoff);
        assert_eq!(
            lifecycle.tick(),
            ApplicationLifecycleAction::Relaunch { attempt: 2 }
        );
        assert!(lifecycle.note_relaunched(11));
        assert_eq!(lifecycle.attempt(), 2);
        assert_eq!(
            lifecycle.observe_readiness(99, true),
            (
                ApplicationReadyDisposition::Rejected,
                ApplicationLifecycleAction::TerminateJob,
            )
        );
        assert_eq!(
            lifecycle.last_failure(),
            Some(ApplicationFailure::InvalidReadiness)
        );
        assert_eq!(
            lifecycle.note_job_drained(),
            ApplicationLifecycleAction::None
        );
        assert_eq!(lifecycle.state(), ApplicationLifecycleState::Failed);
    }

    #[test]
    fn clean_completion_and_explicit_session_stop_never_relaunch() {
        let mut completed = ApplicationLifecycle::new(20, policy(4, 2, 0)).unwrap();
        assert_eq!(
            completed.observe_readiness(20, true),
            (
                ApplicationReadyDisposition::Accepted,
                ApplicationLifecycleAction::None,
            )
        );
        assert_eq!(
            completed.observe_root_exit(ChildStatus::from_raw(0)),
            ApplicationLifecycleAction::TerminateJob
        );
        assert_eq!(
            completed.note_job_drained(),
            ApplicationLifecycleAction::None
        );
        assert_eq!(completed.state(), ApplicationLifecycleState::Completed);

        let mut stopped = ApplicationLifecycle::new(30, policy(4, 2, 0)).unwrap();
        assert_eq!(
            stopped.request_termination(ApplicationTerminationReason::SessionTeardown),
            ApplicationLifecycleAction::TerminateJob
        );
        assert_eq!(stopped.note_job_drained(), ApplicationLifecycleAction::None);
        assert_eq!(stopped.state(), ApplicationLifecycleState::Stopped);
        assert_eq!(stopped.relaunch_count(), 0);
        assert_eq!(
            stopped.termination_reason(),
            Some(ApplicationTerminationReason::SessionTeardown)
        );
    }

    #[test]
    fn root_failure_relaunches_once_then_running_generation_can_be_stopped() {
        let mut lifecycle = ApplicationLifecycle::new(40, policy(4, 1, 0)).unwrap();
        lifecycle.observe_readiness(40, true);
        let failure = ChildStatus::from_raw(75);
        assert_eq!(
            lifecycle.observe_root_exit(failure),
            ApplicationLifecycleAction::TerminateJob
        );
        assert_eq!(
            lifecycle.note_job_drained(),
            ApplicationLifecycleAction::Relaunch { attempt: 2 }
        );
        assert!(lifecycle.note_relaunched(41));
        assert_eq!(lifecycle.relaunch_count(), 1);
        lifecycle.observe_readiness(41, true);
        assert_eq!(lifecycle.state(), ApplicationLifecycleState::Running);
        lifecycle.request_termination(ApplicationTerminationReason::User);
        lifecycle.note_job_drained();
        assert_eq!(lifecycle.state(), ApplicationLifecycleState::Stopped);
    }
}
