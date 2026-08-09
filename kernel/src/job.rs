//! Bounded process membership and exit observation for one job object.
//!
//! The live capability registry owns these states. Keeping the state machine independent
//! makes its containment and lossless-completion rules host-testable before job hierarchy,
//! resource limits, or asynchronous wait sets are added.

use alloc::{collections::VecDeque, vec::Vec};

/// One immutable process-exit record retained until a waiter consumes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitRecord {
    pub process_id: u64,
    pub status: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssignError {
    InvalidProcess,
    AlreadyMember,
    Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownMember;

/// Fixed-policy state behind one job capability object.
///
/// Live members and unconsumed exit records share one bound. Consequently every accepted
/// member is guaranteed room for exactly one terminal record, and exit observation never
/// degrades into a lossy notification under pressure.
#[derive(Debug)]
pub struct State {
    capacity: usize,
    members: Vec<u64>,
    completions: VecDeque<ExitRecord>,
}

impl State {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            members: Vec::new(),
            completions: VecDeque::new(),
        }
    }

    pub fn assign(&mut self, process_id: u64) -> Result<(), AssignError> {
        if process_id == 0 {
            return Err(AssignError::InvalidProcess);
        }
        if self.members.contains(&process_id) {
            return Err(AssignError::AlreadyMember);
        }
        if self.members.len().saturating_add(self.completions.len()) >= self.capacity {
            return Err(AssignError::Full);
        }
        self.members.push(process_id);
        Ok(())
    }

    pub fn remove_unstarted(&mut self, process_id: u64) -> Result<(), UnknownMember> {
        let index = self
            .members
            .iter()
            .position(|member| *member == process_id)
            .ok_or(UnknownMember)?;
        self.members.remove(index);
        Ok(())
    }

    pub fn complete(&mut self, record: ExitRecord) -> Result<(), UnknownMember> {
        let index = self
            .members
            .iter()
            .position(|member| *member == record.process_id)
            .ok_or(UnknownMember)?;
        self.members.remove(index);
        debug_assert!(self.members.len().saturating_add(self.completions.len()) < self.capacity);
        self.completions.push_back(record);
        Ok(())
    }

    pub fn take_completion(&mut self) -> Option<ExitRecord> {
        self.completions.pop_front()
    }

    pub fn active_members(&self) -> usize {
        self.members.len()
    }

    pub fn pending_completions(&self) -> usize {
        self.completions.len()
    }

    pub fn members(&self) -> impl Iterator<Item = u64> + '_ {
        self.members.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_are_fifo_and_independent_per_process() {
        let mut state = State::new(4);
        state.assign(11).unwrap();
        state.assign(12).unwrap();

        state
            .complete(ExitRecord {
                process_id: 12,
                status: 7,
            })
            .unwrap();
        state
            .complete(ExitRecord {
                process_id: 11,
                status: 9,
            })
            .unwrap();

        assert_eq!(state.active_members(), 0);
        assert_eq!(state.pending_completions(), 2);
        assert_eq!(state.take_completion().unwrap().process_id, 12);
        assert_eq!(state.take_completion().unwrap().process_id, 11);
        assert_eq!(state.take_completion(), None);
    }

    #[test]
    fn undrained_completions_reserve_their_bounded_storage() {
        let mut state = State::new(2);
        state.assign(1).unwrap();
        state
            .complete(ExitRecord {
                process_id: 1,
                status: 0,
            })
            .unwrap();
        state.assign(2).unwrap();

        assert_eq!(state.assign(3), Err(AssignError::Full));
        assert_eq!(state.take_completion().unwrap().process_id, 1);
        assert_eq!(state.assign(3), Ok(()));
    }

    #[test]
    fn membership_is_unique_and_unstarted_members_can_roll_back() {
        let mut state = State::new(2);
        state.assign(41).unwrap();

        assert_eq!(state.assign(41), Err(AssignError::AlreadyMember));
        assert_eq!(state.remove_unstarted(41), Ok(()));
        assert_eq!(state.remove_unstarted(41), Err(UnknownMember));
        assert_eq!(state.active_members(), 0);
    }
}
