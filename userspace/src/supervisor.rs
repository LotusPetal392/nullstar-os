use service_control::DesiredState;

use crate::abi::{child_status, signal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellStatusDisposition {
    WaitForNextEvent,
    RestoreForeground,
    RestartShell,
}

pub const fn shell_status_disposition(status: u64) -> ShellStatusDisposition {
    if status == child_status::CONTINUED {
        return ShellStatusDisposition::WaitForNextEvent;
    }

    let stopped_signal = status.saturating_sub(child_status::STOPPED_BASE);
    if status >= child_status::STOPPED_BASE
        && status < child_status::CONTINUED
        && matches!(stopped_signal, signal::STOP | signal::TERMINAL_STOP)
    {
        return ShellStatusDisposition::RestoreForeground;
    }

    ShellStatusDisposition::RestartShell
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Restarting,
    Backoff,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceSpec {
    pub name: &'static [u8],
    pub command: &'static [u8],
    pub ready_message: &'static [u8],
    pub bootstrap_slot: u64,
    pub restart_limit: u32,
    pub restart_backoff_yields: u32,
    pub fatal_startup_exit_status: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatusDisposition {
    WaitForNextEvent,
    Stopped,
    Restart { backoff_yields: u32 },
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartRequestError {
    AlreadyPending,
    InvalidState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartRequestError {
    InvalidState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopRequestError {
    InvalidState,
    TransitionExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCancelError {
    NotRollbackCapable,
    WrongService,
    StaleTransition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyDisposition {
    Accepted,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopRequest {
    process_group: Option<u64>,
    rollback_epoch: Option<u64>,
    service: ServiceSpec,
    previous_state: ServiceState,
    previous_desired_state: DesiredState,
    previous_restart_pending: bool,
}

impl StopRequest {
    pub const fn process_group(self) -> Option<u64> {
        self.process_group
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceRuntime {
    spec: ServiceSpec,
    process_id: Option<u64>,
    restart_count: u32,
    state: ServiceState,
    desired_state: DesiredState,
    controlled_restart_pending: bool,
    stop_transition_epoch: u64,
}

impl ServiceRuntime {
    pub const fn new(spec: ServiceSpec) -> Self {
        Self {
            spec,
            process_id: None,
            restart_count: 0,
            state: ServiceState::Stopped,
            desired_state: DesiredState::Running,
            controlled_restart_pending: false,
            stop_transition_epoch: 0,
        }
    }

    pub const fn spec(&self) -> ServiceSpec {
        self.spec
    }

    pub const fn process_id(&self) -> Option<u64> {
        self.process_id
    }

    pub const fn restart_count(&self) -> u32 {
        self.restart_count
    }

    pub const fn state(&self) -> ServiceState {
        self.state
    }

    pub const fn desired_state(&self) -> DesiredState {
        self.desired_state
    }

    pub const fn should_start(&self) -> bool {
        matches!(self.desired_state, DesiredState::Running)
            && matches!(self.state, ServiceState::Stopped)
            && self.process_id.is_none()
    }

    pub const fn controlled_restart_pending(&self) -> bool {
        self.controlled_restart_pending
    }

    pub fn note_spawned(&mut self, process_id: u64) {
        self.process_id = Some(process_id);
        self.state = ServiceState::Starting;
    }

    pub fn note_ready(&mut self) -> ReadyDisposition {
        if self.state == ServiceState::Starting
            && self.desired_state == DesiredState::Running
            && self.process_id.is_some()
        {
            self.state = ServiceState::Running;
            ReadyDisposition::Accepted
        } else {
            ReadyDisposition::Stale
        }
    }

    pub fn request_start(&mut self) -> Result<(), StartRequestError> {
        if self.state == ServiceState::Failed && self.process_id.is_none() {
            self.desired_state = DesiredState::Running;
            self.state = ServiceState::Stopped;
            self.restart_count = 0;
            self.controlled_restart_pending = false;
            return Ok(());
        }
        if self.desired_state == DesiredState::Running {
            return Ok(());
        }
        match (self.state, self.process_id) {
            (ServiceState::Stopped | ServiceState::Backoff, None) => {
                self.desired_state = DesiredState::Running;
                self.state = ServiceState::Stopped;
                self.restart_count = 0;
                self.controlled_restart_pending = false;
                Ok(())
            }
            (ServiceState::Stopping, Some(_)) => {
                self.desired_state = DesiredState::Running;
                self.state = ServiceState::Restarting;
                self.restart_count = 0;
                self.controlled_restart_pending = true;
                Ok(())
            }
            _ => Err(StartRequestError::InvalidState),
        }
    }

    pub fn request_stop(&mut self) -> Result<StopRequest, StopRequestError> {
        let request = StopRequest {
            process_group: None,
            rollback_epoch: None,
            service: self.spec,
            previous_state: self.state,
            previous_desired_state: self.desired_state,
            previous_restart_pending: self.controlled_restart_pending,
        };
        if self.desired_state == DesiredState::Stopped {
            return Ok(request);
        }

        match (self.state, self.process_id) {
            (ServiceState::Starting | ServiceState::Running, Some(process_group)) => {
                let rollback_epoch = self
                    .stop_transition_epoch
                    .checked_add(1)
                    .ok_or(StopRequestError::TransitionExhausted)?;
                self.stop_transition_epoch = rollback_epoch;
                self.desired_state = DesiredState::Stopped;
                self.state = ServiceState::Stopping;
                self.controlled_restart_pending = false;
                Ok(StopRequest {
                    process_group: Some(process_group),
                    rollback_epoch: Some(rollback_epoch),
                    ..request
                })
            }
            (ServiceState::Restarting, Some(_)) => {
                self.desired_state = DesiredState::Stopped;
                self.state = ServiceState::Stopping;
                self.controlled_restart_pending = false;
                Ok(request)
            }
            (ServiceState::Stopped | ServiceState::Backoff | ServiceState::Failed, None) => {
                self.desired_state = DesiredState::Stopped;
                self.state = ServiceState::Stopped;
                self.controlled_restart_pending = false;
                Ok(request)
            }
            _ => Err(StopRequestError::InvalidState),
        }
    }

    pub fn cancel_stop(&mut self, request: StopRequest) -> Result<(), StopCancelError> {
        if request.service != self.spec {
            return Err(StopCancelError::WrongService);
        }
        let Some(rollback_epoch) = request.rollback_epoch else {
            return Err(StopCancelError::NotRollbackCapable);
        };
        if self.stop_transition_epoch != rollback_epoch
            || self.state != ServiceState::Stopping
            || self.desired_state != DesiredState::Stopped
            || self.process_id != request.process_group
        {
            return Err(StopCancelError::StaleTransition);
        }
        self.state = request.previous_state;
        self.desired_state = request.previous_desired_state;
        self.controlled_restart_pending = request.previous_restart_pending;
        Ok(())
    }

    pub fn request_restart(&mut self) -> Result<u64, RestartRequestError> {
        if self.desired_state != DesiredState::Running {
            return Err(RestartRequestError::InvalidState);
        }
        if self.controlled_restart_pending {
            return Err(RestartRequestError::AlreadyPending);
        }
        match (self.state, self.process_id) {
            (ServiceState::Running, Some(process_id)) => {
                self.state = ServiceState::Restarting;
                self.controlled_restart_pending = true;
                Ok(process_id)
            }
            _ => Err(RestartRequestError::InvalidState),
        }
    }

    pub fn cancel_restart(&mut self) {
        if self.state == ServiceState::Restarting {
            self.state = ServiceState::Running;
            self.controlled_restart_pending = false;
        }
    }

    pub fn complete_restart(&mut self) {
        if self.state == ServiceState::Running {
            self.controlled_restart_pending = false;
        }
    }

    pub fn observe_status(&mut self, status: u64) -> ServiceStatusDisposition {
        if status == child_status::CONTINUED
            || (child_status::STOPPED_BASE..child_status::CONTINUED).contains(&status)
        {
            return ServiceStatusDisposition::WaitForNextEvent;
        }

        self.process_id = None;
        if self.desired_state == DesiredState::Stopped || self.state == ServiceState::Stopping {
            self.state = ServiceState::Stopped;
            self.controlled_restart_pending = false;
            return ServiceStatusDisposition::Stopped;
        }
        if self.state == ServiceState::Restarting {
            self.state = ServiceState::Backoff;
            return ServiceStatusDisposition::Restart { backoff_yields: 0 };
        }
        if self.state == ServiceState::Starting
            && self.spec.fatal_startup_exit_status == Some(status)
        {
            self.state = ServiceState::Failed;
            self.controlled_restart_pending = false;
            return ServiceStatusDisposition::Failed;
        }
        if self.restart_count >= self.spec.restart_limit {
            self.state = ServiceState::Failed;
            self.controlled_restart_pending = false;
            return ServiceStatusDisposition::Failed;
        }

        self.restart_count = self.restart_count.saturating_add(1);
        self.state = ServiceState::Backoff;
        ServiceStatusDisposition::Restart {
            backoff_yields: self.spec.restart_backoff_yields,
        }
    }
}

#[cfg(test)]
mod tests {
    use service_control::DesiredState;

    use super::{
        ReadyDisposition, RestartRequestError, ServiceRuntime, ServiceSpec, ServiceState,
        ServiceStatusDisposition, ShellStatusDisposition, StopCancelError,
        shell_status_disposition,
    };
    use crate::abi::{child_status, signal};

    const TEST_SERVICE: ServiceSpec = ServiceSpec {
        name: b"test",
        command: b"/test-service",
        ready_message: b"ready",
        bootstrap_slot: 1,
        restart_limit: 2,
        restart_backoff_yields: 8,
        fatal_startup_exit_status: Some(21),
    };
    const OTHER_SERVICE: ServiceSpec = ServiceSpec {
        name: b"other",
        command: b"/other-service",
        ..TEST_SERVICE
    };

    #[test]
    fn final_statuses_restart_the_shell() {
        assert_eq!(
            shell_status_disposition(0),
            ShellStatusDisposition::RestartShell
        );
        assert_eq!(
            shell_status_disposition(child_status::SIGNAL_BASE + signal::INTERRUPT),
            ShellStatusDisposition::RestartShell
        );
    }

    #[test]
    fn stopped_shell_is_returned_to_the_foreground() {
        assert_eq!(
            shell_status_disposition(child_status::STOPPED_BASE + signal::STOP),
            ShellStatusDisposition::RestoreForeground
        );
        assert_eq!(
            shell_status_disposition(child_status::STOPPED_BASE + signal::TERMINAL_STOP),
            ShellStatusDisposition::RestoreForeground
        );
    }

    #[test]
    fn continued_status_waits_for_a_final_event() {
        assert_eq!(
            shell_status_disposition(child_status::CONTINUED),
            ShellStatusDisposition::WaitForNextEvent
        );
    }

    #[test]
    fn enabled_service_initially_requires_convergence() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        assert_eq!(service.desired_state(), DesiredState::Running);
        assert_eq!(service.state(), ServiceState::Stopped);
        assert!(service.should_start());
        assert_eq!(service.request_start(), Ok(()));
        assert!(service.should_start());

        service.note_spawned(7);
        assert!(!service.should_start());
    }

    #[test]
    fn service_transitions_from_starting_to_running() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        assert_eq!(service.process_id(), Some(7));
        assert_eq!(service.state(), ServiceState::Starting);
        assert_eq!(service.note_ready(), ReadyDisposition::Accepted);
        assert_eq!(service.state(), ServiceState::Running);
    }

    #[test]
    fn controlled_restart_is_distinct_from_failure_policy() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        service.note_ready();

        assert_eq!(service.request_restart(), Ok(7));
        assert_eq!(service.state(), ServiceState::Restarting);
        assert_eq!(
            service.request_restart(),
            Err(RestartRequestError::AlreadyPending)
        );
        assert_eq!(
            service.observe_status(child_status::SIGNAL_BASE + signal::TERMINATE),
            ServiceStatusDisposition::Restart { backoff_yields: 0 }
        );
        assert_eq!(service.restart_count(), 0);
        assert_eq!(service.state(), ServiceState::Backoff);
        assert!(service.controlled_restart_pending());

        service.note_spawned(8);
        service.note_ready();
        assert_eq!(service.state(), ServiceState::Running);
        assert_eq!(
            service.request_restart(),
            Err(RestartRequestError::AlreadyPending)
        );
        service.complete_restart();
        assert!(!service.controlled_restart_pending());
        assert_eq!(service.request_restart(), Ok(8));
    }

    #[test]
    fn controlled_stop_converges_without_charging_failure_policy() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        service.note_ready();

        let stop = service.request_stop().unwrap();
        assert_eq!(stop.process_group(), Some(7));
        assert_eq!(service.desired_state(), DesiredState::Stopped);
        assert_eq!(service.state(), ServiceState::Stopping);
        assert_eq!(service.request_stop().unwrap().process_group(), None);
        assert_eq!(
            service.observe_status(child_status::SIGNAL_BASE + signal::TERMINATE),
            ServiceStatusDisposition::Stopped
        );
        assert_eq!(service.state(), ServiceState::Stopped);
        assert_eq!(service.process_id(), None);
        assert_eq!(service.restart_count(), 0);
        assert!(!service.should_start());

        assert_eq!(service.request_start(), Ok(()));
        assert_eq!(service.desired_state(), DesiredState::Running);
        assert!(service.should_start());
        service.note_spawned(8);
        service.note_ready();
        assert_eq!(service.state(), ServiceState::Running);
        assert_eq!(service.restart_count(), 0);
    }

    #[test]
    fn failed_stop_signal_restores_the_exact_previous_state() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        service.note_ready();
        let stop = service.request_stop().unwrap();
        assert_eq!(service.cancel_stop(stop), Ok(()));
        assert_eq!(
            service.cancel_stop(stop),
            Err(StopCancelError::StaleTransition)
        );
        assert_eq!(service.desired_state(), DesiredState::Running);
        assert_eq!(service.state(), ServiceState::Running);
        assert_eq!(service.process_id(), Some(7));
    }

    #[test]
    fn stop_rollback_rejects_finalized_superseded_and_foreign_transitions() {
        let mut finalized = ServiceRuntime::new(TEST_SERVICE);
        finalized.note_spawned(7);
        finalized.note_ready();
        let finalized_stop = finalized.request_stop().unwrap();
        assert_eq!(
            finalized.observe_status(child_status::SIGNAL_BASE + signal::TERMINATE),
            ServiceStatusDisposition::Stopped
        );
        assert_eq!(
            finalized.cancel_stop(finalized_stop),
            Err(StopCancelError::StaleTransition)
        );

        let mut superseded = ServiceRuntime::new(TEST_SERVICE);
        superseded.note_spawned(8);
        superseded.note_ready();
        let superseded_stop = superseded.request_stop().unwrap();
        assert_eq!(superseded.request_start(), Ok(()));
        assert_eq!(
            superseded.cancel_stop(superseded_stop),
            Err(StopCancelError::StaleTransition)
        );

        let mut foreign = ServiceRuntime::new(OTHER_SERVICE);
        foreign.note_spawned(7);
        foreign.note_ready();
        assert_eq!(
            foreign.cancel_stop(finalized_stop),
            Err(StopCancelError::WrongService)
        );
    }

    #[test]
    fn readiness_after_stop_is_stale_and_does_not_republish_the_service() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        let stop = service.request_stop().unwrap();
        assert_eq!(stop.process_group(), Some(7));
        assert_eq!(service.note_ready(), ReadyDisposition::Stale);
        assert_eq!(service.state(), ServiceState::Stopping);
    }

    #[test]
    fn stop_overrides_a_controlled_restart_without_a_second_signal() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        service.note_ready();
        assert_eq!(service.request_restart(), Ok(7));

        let stop = service.request_stop().unwrap();
        assert_eq!(stop.process_group(), None);
        assert_eq!(service.desired_state(), DesiredState::Stopped);
        assert_eq!(service.state(), ServiceState::Stopping);
        assert_eq!(
            service.observe_status(child_status::SIGNAL_BASE + signal::TERMINATE),
            ServiceStatusDisposition::Stopped
        );
        assert!(!service.controlled_restart_pending());
        assert_eq!(service.restart_count(), 0);
    }

    #[test]
    fn start_during_controlled_stop_schedules_one_zero_backoff_replacement() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        service.note_ready();
        let stop = service.request_stop().unwrap();
        assert_eq!(stop.process_group(), Some(7));
        assert_eq!(service.request_start(), Ok(()));
        assert_eq!(service.desired_state(), DesiredState::Running);
        assert_eq!(service.state(), ServiceState::Restarting);
        assert_eq!(
            service.observe_status(child_status::SIGNAL_BASE + signal::TERMINATE),
            ServiceStatusDisposition::Restart { backoff_yields: 0 }
        );
        assert_eq!(service.restart_count(), 0);
    }

    #[test]
    fn stop_cancels_failure_backoff_and_explicit_start_rearms_the_budget() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        assert_eq!(
            service.observe_status(75),
            ServiceStatusDisposition::Restart { backoff_yields: 8 }
        );
        assert_eq!(service.restart_count(), 1);
        assert_eq!(service.request_stop().unwrap().process_group(), None);
        assert_eq!(service.state(), ServiceState::Stopped);
        assert_eq!(service.desired_state(), DesiredState::Stopped);
        assert_eq!(service.request_start(), Ok(()));
        assert_eq!(service.restart_count(), 0);
        assert!(service.should_start());
    }

    #[test]
    fn failed_restart_signal_can_restore_running_state() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        service.note_ready();
        assert_eq!(service.request_restart(), Ok(7));
        service.cancel_restart();
        assert_eq!(service.state(), ServiceState::Running);
        assert_eq!(service.process_id(), Some(7));
        assert!(!service.controlled_restart_pending());
    }

    #[test]
    fn restart_requires_a_running_service() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        assert_eq!(
            service.request_restart(),
            Err(RestartRequestError::InvalidState)
        );
        service.note_spawned(7);
        assert_eq!(
            service.request_restart(),
            Err(RestartRequestError::InvalidState)
        );
    }

    #[test]
    fn fatal_startup_status_is_not_restarted_or_charged_to_the_budget() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);

        assert_eq!(service.observe_status(21), ServiceStatusDisposition::Failed);
        assert_eq!(service.process_id(), None);
        assert_eq!(service.restart_count(), 0);
        assert_eq!(service.state(), ServiceState::Failed);
        assert_eq!(service.desired_state(), DesiredState::Running);
        assert_eq!(service.request_start(), Ok(()));
        assert_eq!(service.state(), ServiceState::Stopped);
        assert_eq!(service.restart_count(), 0);
        assert!(service.should_start());
    }

    #[test]
    fn explicit_start_rearms_an_exhausted_failure_budget() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        for process_id in 7..=9 {
            service.note_spawned(process_id);
            let disposition = service.observe_status(1);
            if process_id == 9 {
                assert_eq!(disposition, ServiceStatusDisposition::Failed);
            } else {
                assert_eq!(
                    disposition,
                    ServiceStatusDisposition::Restart { backoff_yields: 8 }
                );
            }
        }
        assert_eq!(service.state(), ServiceState::Failed);
        assert_eq!(service.restart_count(), 2);
        assert_eq!(service.request_start(), Ok(()));
        assert_eq!(service.state(), ServiceState::Stopped);
        assert_eq!(service.restart_count(), 0);
        assert!(service.should_start());
    }

    #[test]
    fn the_same_status_after_readiness_uses_normal_restart_policy() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        service.note_ready();

        assert_eq!(
            service.observe_status(21),
            ServiceStatusDisposition::Restart { backoff_yields: 8 }
        );
        assert_eq!(service.restart_count(), 1);
        assert_eq!(service.state(), ServiceState::Backoff);
    }

    #[test]
    fn final_status_restarts_once_and_replacement_reaches_running() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        service.note_ready();

        assert_eq!(
            service.observe_status(child_status::SIGNAL_BASE + signal::TERMINATE),
            ServiceStatusDisposition::Restart { backoff_yields: 8 }
        );
        assert_eq!(service.process_id(), None);
        assert_eq!(service.restart_count(), 1);
        assert_eq!(service.state(), ServiceState::Backoff);

        service.note_spawned(8);
        service.note_ready();

        assert_eq!(service.process_id(), Some(8));
        assert_eq!(service.restart_count(), 1);
        assert_eq!(service.state(), ServiceState::Running);
    }

    #[test]
    fn service_restart_budget_is_bounded() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        assert_eq!(
            service.observe_status(75),
            ServiceStatusDisposition::Restart { backoff_yields: 8 }
        );
        assert_eq!(service.restart_count(), 1);
        service.note_spawned(8);
        assert_eq!(
            service.observe_status(child_status::SIGNAL_BASE + signal::TERMINATE),
            ServiceStatusDisposition::Restart { backoff_yields: 8 }
        );
        service.note_spawned(9);
        assert_eq!(service.observe_status(1), ServiceStatusDisposition::Failed);
        assert_eq!(service.state(), ServiceState::Failed);
    }

    #[test]
    fn stopped_service_does_not_consume_restart_budget() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        assert_eq!(
            service.observe_status(child_status::STOPPED_BASE + signal::STOP),
            ServiceStatusDisposition::WaitForNextEvent
        );
        assert_eq!(service.restart_count(), 0);
        assert_eq!(service.process_id(), Some(7));
    }
}
