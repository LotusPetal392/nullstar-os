//! Architecture-neutral single-CPU round-robin scheduling policy.
//!
//! The interrupt-facing scheduler owns register contexts and address spaces;
//! this bounded policy owns only runnable thread identity and quantum state.
//! Keeping the policy independent makes preemption and wakeup ordering
//! testable without booting a kernel or pretending to provide SMP semantics.

use alloc::collections::VecDeque;

use crate::process_model::ThreadId;

pub const DEFAULT_QUANTUM_TICKS: u64 = 5;
pub const MAX_RUNNABLE_THREADS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Switch {
    pub from: Option<ThreadId>,
    pub to: ThreadId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub current: Option<ThreadId>,
    pub runnable_count: usize,
    pub quantum_ticks: u64,
    pub ticks_in_quantum: u64,
    pub context_switches: u64,
    pub preemptions: u64,
    pub voluntary_switches: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Capacity,
    AlreadyRunnable,
    UnknownThread,
    InvalidQuantum,
}

/// Fixed-capacity round-robin policy for one CPU.
pub struct RoundRobin {
    quantum_ticks: u64,
    ticks_in_quantum: u64,
    current: Option<ThreadId>,
    runnable: VecDeque<ThreadId>,
    context_switches: u64,
    preemptions: u64,
    voluntary_switches: u64,
}

impl RoundRobin {
    pub const fn new(quantum_ticks: u64) -> Result<Self, Error> {
        if quantum_ticks == 0 {
            return Err(Error::InvalidQuantum);
        }
        Ok(Self {
            quantum_ticks,
            ticks_in_quantum: 0,
            current: None,
            runnable: VecDeque::new(),
            context_switches: 0,
            preemptions: 0,
            voluntary_switches: 0,
        })
    }

    pub const fn with_default_quantum() -> Self {
        // The constant is nonzero, so construction cannot fail.
        Self {
            quantum_ticks: DEFAULT_QUANTUM_TICKS,
            ticks_in_quantum: 0,
            current: None,
            runnable: VecDeque::new(),
            context_switches: 0,
            preemptions: 0,
            voluntary_switches: 0,
        }
    }

    pub fn admit(&mut self, thread: ThreadId) -> Result<Option<Switch>, Error> {
        if self.contains(thread) {
            return Err(Error::AlreadyRunnable);
        }
        if self.runnable.len() >= MAX_RUNNABLE_THREADS {
            return Err(Error::Capacity);
        }
        if self.current.is_none() {
            self.current = Some(thread);
            self.context_switches = self.context_switches.saturating_add(1);
            Ok(Some(Switch {
                from: None,
                to: thread,
            }))
        } else {
            self.runnable.push_back(thread);
            Ok(None)
        }
    }

    pub fn tick(&mut self) -> Option<Switch> {
        if self.current.is_none() {
            return self.dispatch_next();
        }
        self.ticks_in_quantum = self.ticks_in_quantum.saturating_add(1);
        if self.ticks_in_quantum < self.quantum_ticks {
            return None;
        }
        self.ticks_in_quantum = 0;
        let next = self.rotate_current()?;
        self.preemptions = self.preemptions.saturating_add(1);
        Some(next)
    }

    pub fn yield_current(&mut self) -> Option<Switch> {
        self.ticks_in_quantum = 0;
        let next = self.rotate_current()?;
        self.voluntary_switches = self.voluntary_switches.saturating_add(1);
        Some(next)
    }

    pub fn block_current(&mut self) -> Option<Switch> {
        self.ticks_in_quantum = 0;
        let from = self.current.take()?;
        let next = self.dispatch_next()?;
        Some(Switch {
            from: Some(from),
            to: next.to,
        })
    }

    pub fn wake(&mut self, thread: ThreadId) -> Result<Option<Switch>, Error> {
        if self.contains(thread) {
            return Err(Error::AlreadyRunnable);
        }
        if self.runnable.len() >= MAX_RUNNABLE_THREADS {
            return Err(Error::Capacity);
        }
        if self.current.is_none() {
            self.current = Some(thread);
            self.context_switches = self.context_switches.saturating_add(1);
            Ok(Some(Switch {
                from: None,
                to: thread,
            }))
        } else {
            self.runnable.push_back(thread);
            Ok(None)
        }
    }

    pub fn remove(&mut self, thread: ThreadId) -> Result<Option<Switch>, Error> {
        if self.current == Some(thread) {
            self.current = None;
            self.ticks_in_quantum = 0;
            return Ok(self.dispatch_next());
        }
        let Some(index) = self
            .runnable
            .iter()
            .position(|candidate| *candidate == thread)
        else {
            return Err(Error::UnknownThread);
        };
        self.runnable.remove(index);
        Ok(None)
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            current: self.current,
            runnable_count: self.runnable.len(),
            quantum_ticks: self.quantum_ticks,
            ticks_in_quantum: self.ticks_in_quantum,
            context_switches: self.context_switches,
            preemptions: self.preemptions,
            voluntary_switches: self.voluntary_switches,
        }
    }

    fn contains(&self, thread: ThreadId) -> bool {
        self.current == Some(thread) || self.runnable.iter().any(|candidate| *candidate == thread)
    }

    fn dispatch_next(&mut self) -> Option<Switch> {
        let next = self.runnable.pop_front()?;
        let from = self.current.replace(next);
        self.context_switches = self.context_switches.saturating_add(1);
        Some(Switch { from, to: next })
    }

    fn rotate_current(&mut self) -> Option<Switch> {
        let current = self.current?;
        let next = self.runnable.pop_front()?;
        self.runnable.push_back(current);
        self.current = Some(next);
        self.context_switches = self.context_switches.saturating_add(1);
        Some(Switch {
            from: Some(current),
            to: next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_model::ProcessTable;

    fn two_threads() -> (ProcessTable, ThreadId, ThreadId) {
        let mut processes = ProcessTable::new();
        let process = processes.create_process(None).unwrap();
        let first = processes.create_thread(process, "first").unwrap();
        let second = processes.create_thread(process, "second").unwrap();
        (processes, first, second)
    }

    #[test]
    fn timer_ticks_preempt_in_round_robin_order() {
        let (_processes, first, second) = two_threads();
        let mut scheduler = RoundRobin::new(2).unwrap();

        assert_eq!(
            scheduler.admit(first).unwrap(),
            Some(Switch {
                from: None,
                to: first
            })
        );
        assert_eq!(scheduler.admit(second).unwrap(), None);
        assert_eq!(scheduler.tick(), None);
        assert_eq!(
            scheduler.tick(),
            Some(Switch {
                from: Some(first),
                to: second
            })
        );
        assert_eq!(scheduler.tick(), None);
        assert_eq!(
            scheduler.tick(),
            Some(Switch {
                from: Some(second),
                to: first
            })
        );
        assert_eq!(scheduler.snapshot().preemptions, 2);
        assert_eq!(scheduler.snapshot().context_switches, 3);
    }

    #[test]
    fn voluntary_yield_and_block_choose_the_next_runnable_thread() {
        let (_processes, first, second) = two_threads();
        let mut scheduler = RoundRobin::with_default_quantum();
        scheduler.admit(first).unwrap();
        scheduler.admit(second).unwrap();

        assert_eq!(
            scheduler.yield_current(),
            Some(Switch {
                from: Some(first),
                to: second
            })
        );
        assert_eq!(scheduler.snapshot().voluntary_switches, 1);
        assert_eq!(
            scheduler.block_current(),
            Some(Switch {
                from: Some(second),
                to: first
            })
        );
        assert_eq!(scheduler.wake(second).unwrap(), None);
        scheduler.remove(first).unwrap();
        assert_eq!(scheduler.snapshot().current, Some(second));
    }

    #[test]
    fn duplicate_and_unknown_admission_are_rejected_without_corrupting_queue() {
        let (_processes, first, second) = two_threads();
        let mut scheduler = RoundRobin::new(1).unwrap();
        scheduler.admit(first).unwrap();
        assert_eq!(scheduler.admit(first), Err(Error::AlreadyRunnable));
        assert_eq!(scheduler.remove(second), Err(Error::UnknownThread));
        assert_eq!(scheduler.snapshot().runnable_count, 0);
        assert_eq!(scheduler.snapshot().current, Some(first));
    }
}
