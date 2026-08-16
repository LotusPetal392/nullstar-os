//! One-shot monotonic timer state for capability-backed waits.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidDeadline;

/// A one-shot timer that remains signaled until canceled or re-armed.
#[derive(Debug, Default)]
pub struct State {
    deadline_ns: Option<u64>,
    fired: bool,
}

impl State {
    pub const fn new() -> Self {
        Self {
            deadline_ns: None,
            fired: false,
        }
    }

    /// Replace the current arm. Deadlines at or before `now_ns` fire immediately.
    pub fn arm(&mut self, deadline_ns: u64, now_ns: u64) -> Result<(), InvalidDeadline> {
        if deadline_ns == u64::MAX {
            return Err(InvalidDeadline);
        }
        self.fired = deadline_ns <= now_ns;
        self.deadline_ns = (!self.fired).then_some(deadline_ns);
        Ok(())
    }

    /// Clear both a pending arm and the fired signal.
    pub fn cancel(&mut self) {
        self.deadline_ns = None;
        self.fired = false;
    }

    /// Advance the timer and report whether this call asserted the fired signal.
    pub fn advance(&mut self, now_ns: u64) -> bool {
        let Some(deadline_ns) = self.deadline_ns else {
            return false;
        };
        if now_ns < deadline_ns {
            return false;
        }
        self.deadline_ns = None;
        self.fired = true;
        true
    }

    pub const fn is_armed(&self) -> bool {
        self.deadline_ns.is_some()
    }

    pub const fn is_fired(&self) -> bool {
        self.fired
    }
}

#[cfg(test)]
mod tests {
    use super::{InvalidDeadline, State};

    #[test]
    fn future_deadline_fires_once_and_remains_asserted() {
        let mut timer = State::new();
        timer.arm(20, 10).unwrap();
        assert!(timer.is_armed());
        assert!(!timer.is_fired());
        assert!(!timer.advance(19));
        assert!(timer.advance(20));
        assert!(!timer.is_armed());
        assert!(timer.is_fired());
        assert!(!timer.advance(30));
        assert!(timer.is_fired());
    }

    #[test]
    fn past_deadline_fires_immediately() {
        let mut timer = State::new();
        timer.arm(9, 10).unwrap();
        assert!(!timer.is_armed());
        assert!(timer.is_fired());
    }

    #[test]
    fn rearm_and_cancel_clear_the_fired_level() {
        let mut timer = State::new();
        timer.arm(10, 10).unwrap();
        timer.arm(30, 20).unwrap();
        assert!(timer.is_armed());
        assert!(!timer.is_fired());
        timer.cancel();
        assert!(!timer.is_armed());
        assert!(!timer.is_fired());
        assert!(!timer.advance(40));
    }

    #[test]
    fn infinite_deadline_is_rejected_without_changing_state() {
        let mut timer = State::new();
        timer.arm(30, 20).unwrap();
        assert_eq!(timer.arm(u64::MAX, 25), Err(InvalidDeadline));
        assert!(timer.is_armed());
        assert!(!timer.is_fired());
    }
}
