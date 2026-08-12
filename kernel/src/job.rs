//! Bounded process membership, child hierarchy, and exit observation for one job object.
//!
//! The live capability registry owns these states. Keeping the state machine independent
//! makes its containment, resource-policy, and lossless-completion rules host-testable before
//! asynchronous wait sets are added.

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
    Retired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildError {
    InvalidJob,
    AlreadyChild,
    Full,
    Retired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitError {
    OutOfRange,
    Relaxation,
    Retired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetireError {
    Root,
    NotEmpty,
    Retired,
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
    member_capacity: usize,
    process_limit: usize,
    child_capacity: usize,
    parent: Option<u64>,
    retired: bool,
    members: Vec<u64>,
    completions: VecDeque<ExitRecord>,
    children: Vec<u64>,
}

impl State {
    pub fn new(member_capacity: usize, child_capacity: usize) -> Self {
        Self {
            member_capacity,
            process_limit: member_capacity,
            child_capacity,
            parent: None,
            retired: false,
            members: Vec::new(),
            completions: VecDeque::new(),
            children: Vec::new(),
        }
    }

    pub fn set_parent(&mut self, job_id: u64) -> Result<(), ChildError> {
        if job_id == 0 {
            return Err(ChildError::InvalidJob);
        }
        if self.parent.is_some() {
            return Err(ChildError::AlreadyChild);
        }
        self.parent = Some(job_id);
        Ok(())
    }

    pub fn parent(&self) -> Option<u64> {
        self.parent
    }

    pub fn is_retired(&self) -> bool {
        self.retired
    }

    pub fn retirement_parent(&self) -> Result<u64, RetireError> {
        if self.retired {
            return Err(RetireError::Retired);
        }
        let parent = self.parent.ok_or(RetireError::Root)?;
        if !self.members.is_empty() || !self.completions.is_empty() || !self.children.is_empty() {
            return Err(RetireError::NotEmpty);
        }
        Ok(parent)
    }

    pub fn retire(&mut self) -> Result<u64, RetireError> {
        let parent = self.retirement_parent()?;
        self.parent = None;
        self.retired = true;
        Ok(parent)
    }

    pub fn set_process_limit(&mut self, limit: usize) -> Result<(), LimitError> {
        if self.retired {
            return Err(LimitError::Retired);
        }
        if limit > self.member_capacity {
            return Err(LimitError::OutOfRange);
        }
        if limit > self.process_limit {
            return Err(LimitError::Relaxation);
        }
        self.process_limit = limit;
        Ok(())
    }

    pub fn process_limit(&self) -> usize {
        self.process_limit
    }

    pub fn attach_child(&mut self, job_id: u64) -> Result<(), ChildError> {
        if self.retired {
            return Err(ChildError::Retired);
        }
        if job_id == 0 {
            return Err(ChildError::InvalidJob);
        }
        if self.children.contains(&job_id) {
            return Err(ChildError::AlreadyChild);
        }
        if self.children.len() >= self.child_capacity {
            return Err(ChildError::Full);
        }
        self.children.push(job_id);
        Ok(())
    }

    pub fn remove_child(&mut self, job_id: u64) -> Result<(), UnknownMember> {
        let index = self
            .children
            .iter()
            .position(|child| *child == job_id)
            .ok_or(UnknownMember)?;
        self.children.remove(index);
        Ok(())
    }

    pub fn assign(&mut self, process_id: u64) -> Result<(), AssignError> {
        if self.retired {
            return Err(AssignError::Retired);
        }
        if process_id == 0 {
            return Err(AssignError::InvalidProcess);
        }
        if self.members.contains(&process_id) {
            return Err(AssignError::AlreadyMember);
        }
        if self.members.len().saturating_add(self.completions.len()) >= self.member_capacity {
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
        debug_assert!(
            self.members.len().saturating_add(self.completions.len()) < self.member_capacity
        );
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

    pub fn children(&self) -> impl Iterator<Item = u64> + '_ {
        self.children.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn completions_are_fifo_and_independent_per_process() {
        let mut state = State::new(4, 4);
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
        let mut state = State::new(2, 2);
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
        let mut state = State::new(2, 2);
        state.assign(41).unwrap();

        assert_eq!(state.assign(41), Err(AssignError::AlreadyMember));
        assert_eq!(state.remove_unstarted(41), Ok(()));
        assert_eq!(state.remove_unstarted(41), Err(UnknownMember));
        assert_eq!(state.active_members(), 0);
    }

    #[test]
    fn child_jobs_are_bounded_ordered_and_removable_for_rollback() {
        let mut state = State::new(2, 2);

        assert_eq!(state.set_parent(0), Err(ChildError::InvalidJob));
        assert_eq!(state.set_parent(70), Ok(()));
        assert_eq!(state.set_parent(71), Err(ChildError::AlreadyChild));
        assert_eq!(state.parent(), Some(70));
        assert_eq!(state.attach_child(0), Err(ChildError::InvalidJob));
        assert_eq!(state.attach_child(71), Ok(()));
        assert_eq!(state.attach_child(71), Err(ChildError::AlreadyChild));
        assert_eq!(state.attach_child(72), Ok(()));
        assert_eq!(state.attach_child(73), Err(ChildError::Full));
        assert_eq!(state.children().collect::<Vec<_>>(), vec![71, 72]);
        assert_eq!(state.remove_child(71), Ok(()));
        assert_eq!(state.remove_child(71), Err(UnknownMember));
        assert_eq!(state.attach_child(73), Ok(()));
        assert_eq!(state.children().collect::<Vec<_>>(), vec![72, 73]);
    }

    #[test]
    fn process_limit_can_only_tighten_within_the_membership_bound() {
        let mut state = State::new(4, 2);

        assert_eq!(state.process_limit(), 4);
        assert_eq!(state.set_process_limit(5), Err(LimitError::OutOfRange));
        assert_eq!(state.set_process_limit(2), Ok(()));
        assert_eq!(state.process_limit(), 2);
        assert_eq!(state.set_process_limit(2), Ok(()));
        assert_eq!(state.set_process_limit(3), Err(LimitError::Relaxation));
        assert_eq!(state.set_process_limit(0), Ok(()));
        assert_eq!(state.process_limit(), 0);
    }

    #[test]
    fn only_an_empty_child_leaf_can_retire_and_retirement_is_permanent() {
        let root = State::new(4, 2);
        assert_eq!(root.retirement_parent(), Err(RetireError::Root));

        let mut child = State::new(4, 2);
        child.set_parent(70).unwrap();
        child.attach_child(72).unwrap();
        assert_eq!(child.retirement_parent(), Err(RetireError::NotEmpty));
        child.remove_child(72).unwrap();
        child.assign(11).unwrap();
        assert_eq!(child.retirement_parent(), Err(RetireError::NotEmpty));
        child
            .complete(ExitRecord {
                process_id: 11,
                status: 0,
            })
            .unwrap();
        assert_eq!(child.retirement_parent(), Err(RetireError::NotEmpty));
        assert!(child.take_completion().is_some());

        assert_eq!(child.retire(), Ok(70));
        assert!(child.is_retired());
        assert_eq!(child.parent(), None);
        assert_eq!(child.retire(), Err(RetireError::Retired));
        assert_eq!(child.assign(12), Err(AssignError::Retired));
        assert_eq!(child.attach_child(73), Err(ChildError::Retired));
        assert_eq!(child.set_process_limit(1), Err(LimitError::Retired));
    }
}
