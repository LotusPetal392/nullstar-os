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

#[cfg(test)]
mod tests {
    use super::{ShellStatusDisposition, shell_status_disposition};
    use crate::abi::{child_status, signal};

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
}
