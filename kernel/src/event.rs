//! Manual-reset event state shared by generic waits, wait sets, and event ports.

/// One persistent user-controlled signal level.
#[derive(Debug, Default)]
pub struct State {
    signaled: bool,
}

impl State {
    pub const fn new() -> Self {
        Self { signaled: false }
    }

    pub const fn is_signaled(&self) -> bool {
        self.signaled
    }

    /// Assert the event, returning whether the level changed.
    pub fn set(&mut self) -> bool {
        let changed = !self.signaled;
        self.signaled = true;
        changed
    }

    /// Clear the event, returning whether the level changed.
    pub fn reset(&mut self) -> bool {
        let changed = self.signaled;
        self.signaled = false;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::State;

    #[test]
    fn starts_clear_and_set_is_idempotent() {
        let mut event = State::new();

        assert!(!event.is_signaled());
        assert!(event.set());
        assert!(event.is_signaled());
        assert!(!event.set());
        assert!(event.is_signaled());
    }

    #[test]
    fn reset_clears_and_rearms_the_next_set() {
        let mut event = State::new();

        assert!(!event.reset());
        assert!(event.set());
        assert!(event.reset());
        assert!(!event.is_signaled());
        assert!(!event.reset());
        assert!(event.set());
    }
}
