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
    Backoff,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceSpec {
    pub name: &'static [u8],
    pub command: &'static [u8],
    pub ready_message: &'static [u8],
    pub bootstrap_handle: u64,
    pub restart_limit: u32,
    pub restart_backoff_yields: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatusDisposition {
    WaitForNextEvent,
    Restart { backoff_yields: u32 },
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceRuntime {
    spec: ServiceSpec,
    process_id: Option<u64>,
    restart_count: u32,
    state: ServiceState,
}

impl ServiceRuntime {
    pub const fn new(spec: ServiceSpec) -> Self {
        Self {
            spec,
            process_id: None,
            restart_count: 0,
            state: ServiceState::Stopped,
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

    pub fn note_spawned(&mut self, process_id: u64) {
        self.process_id = Some(process_id);
        self.state = ServiceState::Starting;
    }

    pub fn note_ready(&mut self) {
        self.state = ServiceState::Running;
    }

    pub fn observe_status(&mut self, status: u64) -> ServiceStatusDisposition {
        if status == child_status::CONTINUED
            || (status >= child_status::STOPPED_BASE && status < child_status::CONTINUED)
        {
            return ServiceStatusDisposition::WaitForNextEvent;
        }

        self.process_id = None;
        if self.restart_count >= self.spec.restart_limit {
            self.state = ServiceState::Failed;
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
    use super::{
        ServiceRuntime, ServiceSpec, ServiceState, ServiceStatusDisposition,
        ShellStatusDisposition, shell_status_disposition,
    };
    use crate::abi::{child_status, signal};

    const TEST_SERVICE: ServiceSpec = ServiceSpec {
        name: b"test",
        command: b"/test-service",
        ready_message: b"ready",
        bootstrap_handle: 1,
        restart_limit: 2,
        restart_backoff_yields: 8,
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
    fn service_transitions_from_starting_to_running() {
        let mut service = ServiceRuntime::new(TEST_SERVICE);
        service.note_spawned(7);
        assert_eq!(service.process_id(), Some(7));
        assert_eq!(service.state(), ServiceState::Starting);
        service.note_ready();
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
