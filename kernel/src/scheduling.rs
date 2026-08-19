//! Architecture-neutral single-CPU round-robin scheduling policy.
//!
//! The interrupt-facing scheduler owns register contexts and address spaces;
//! this bounded policy owns only runnable thread identity and quantum state.
//! Keeping the policy independent makes preemption and wakeup ordering
//! testable without booting a kernel or pretending to provide SMP semantics.

use alloc::{collections::VecDeque, vec::Vec};

use crate::process_model::ThreadId;

pub const DEFAULT_QUANTUM_TICKS: u64 = 5;
pub const MAX_RUNNABLE_THREADS: usize = 128;
pub const MAX_CPUS: usize = 64;

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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CpuId(u8);

impl CpuId {
    pub const fn from_raw(raw: usize) -> Option<Self> {
        if raw < MAX_CPUS {
            Some(Self(raw as u8))
        } else {
            None
        }
    }

    pub const fn raw(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuMask(u64);

impl CpuMask {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn single(cpu: CpuId) -> Self {
        Self(1_u64 << cpu.0)
    }

    pub const fn first(count: usize) -> Option<Self> {
        if count == 0 || count > MAX_CPUS {
            return None;
        }
        Some(Self(if count == MAX_CPUS {
            u64::MAX
        } else {
            (1_u64 << count) - 1
        }))
    }

    pub const fn contains(self, cpu: CpuId) -> bool {
        self.0 & (1_u64 << cpu.0) != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn bits(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Placement {
    pub thread: ThreadId,
    pub cpu: CpuId,
    pub migrated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuSnapshot {
    pub cpu: CpuId,
    pub current: Option<ThreadId>,
    pub runnable_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmpSnapshot {
    pub cpu_count: usize,
    pub assigned_thread_count: usize,
    pub online_mask: CpuMask,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmpError {
    InvalidCpuCount,
    InvalidQuantum,
    InvalidCpu,
    EmptyAffinity,
    AffinityOutsideTopology,
    DuplicateThread,
    Capacity,
    UnknownThread,
    AffinityViolation,
}

struct Assignment {
    thread: ThreadId,
    affinity: CpuMask,
    cpu: CpuId,
}

/// Affinity-aware per-CPU round-robin queues.
///
/// This is the SMP scheduling substrate: it owns placement, migration, and
/// CPU-local queue policy. AP startup, interrupt delivery, and synchronization
/// remain architecture work above this policy layer.
pub struct SmpRoundRobin {
    queues: Vec<RoundRobin>,
    assignments: Vec<Assignment>,
    online_mask: CpuMask,
}

impl SmpRoundRobin {
    pub fn new(cpu_count: usize, quantum_ticks: u64) -> Result<Self, SmpError> {
        if cpu_count == 0 || cpu_count > MAX_CPUS {
            return Err(SmpError::InvalidCpuCount);
        }
        let online_mask = CpuMask::first(cpu_count).ok_or(SmpError::InvalidCpuCount)?;
        let mut queues = Vec::with_capacity(cpu_count);
        for _ in 0..cpu_count {
            queues.push(RoundRobin::new(quantum_ticks).map_err(|_| SmpError::InvalidQuantum)?);
        }
        Ok(Self {
            queues,
            assignments: Vec::new(),
            online_mask,
        })
    }

    pub fn admit(&mut self, thread: ThreadId, affinity: CpuMask) -> Result<Placement, SmpError> {
        self.validate_affinity(affinity)?;
        if self.assignment_index(thread).is_some() {
            return Err(SmpError::DuplicateThread);
        }
        if self.assignments.len() >= MAX_RUNNABLE_THREADS {
            return Err(SmpError::Capacity);
        }
        let cpu = self.least_loaded_cpu(affinity)?;
        self.queues[cpu.raw()]
            .admit(thread)
            .map_err(|_| SmpError::Capacity)?;
        self.assignments.push(Assignment {
            thread,
            affinity,
            cpu,
        });
        Ok(Placement {
            thread,
            cpu,
            migrated: false,
        })
    }

    pub fn set_affinity(
        &mut self,
        thread: ThreadId,
        affinity: CpuMask,
    ) -> Result<Placement, SmpError> {
        self.validate_affinity(affinity)?;
        let index = self
            .assignment_index(thread)
            .ok_or(SmpError::UnknownThread)?;
        let current_cpu = self.assignments[index].cpu;
        self.assignments[index].affinity = affinity;
        if affinity.contains(current_cpu) {
            return Ok(Placement {
                thread,
                cpu: current_cpu,
                migrated: false,
            });
        }

        let destination = self.least_loaded_cpu(affinity)?;
        self.queues[current_cpu.raw()]
            .remove(thread)
            .map_err(|_| SmpError::UnknownThread)?;
        self.queues[destination.raw()]
            .admit(thread)
            .map_err(|_| SmpError::Capacity)?;
        self.assignments[index].cpu = destination;
        Ok(Placement {
            thread,
            cpu: destination,
            migrated: true,
        })
    }

    pub fn migrate(&mut self, thread: ThreadId, destination: CpuId) -> Result<Placement, SmpError> {
        if !self.online_mask.contains(destination) {
            return Err(SmpError::InvalidCpu);
        }
        let index = self
            .assignment_index(thread)
            .ok_or(SmpError::UnknownThread)?;
        if !self.assignments[index].affinity.contains(destination) {
            return Err(SmpError::AffinityViolation);
        }
        let current_cpu = self.assignments[index].cpu;
        if current_cpu == destination {
            return Ok(Placement {
                thread,
                cpu: destination,
                migrated: false,
            });
        }
        self.queues[current_cpu.raw()]
            .remove(thread)
            .map_err(|_| SmpError::UnknownThread)?;
        self.queues[destination.raw()]
            .admit(thread)
            .map_err(|_| SmpError::Capacity)?;
        self.assignments[index].cpu = destination;
        Ok(Placement {
            thread,
            cpu: destination,
            migrated: true,
        })
    }

    pub fn tick(&mut self, cpu: CpuId) -> Result<Option<Switch>, SmpError> {
        let queue = self.queue_mut(cpu)?;
        Ok(queue.tick())
    }

    pub fn cpu_snapshot(&self, cpu: CpuId) -> Result<CpuSnapshot, SmpError> {
        let queue = self.queue(cpu)?;
        let snapshot = queue.snapshot();
        Ok(CpuSnapshot {
            cpu,
            current: snapshot.current,
            runnable_count: snapshot.runnable_count,
        })
    }

    pub fn snapshot(&self) -> SmpSnapshot {
        SmpSnapshot {
            cpu_count: self.queues.len(),
            assigned_thread_count: self.assignments.len(),
            online_mask: self.online_mask,
        }
    }

    pub fn placement(&self, thread: ThreadId) -> Result<Placement, SmpError> {
        let assignment = self
            .assignment_index(thread)
            .map(|index| &self.assignments[index])
            .ok_or(SmpError::UnknownThread)?;
        Ok(Placement {
            thread,
            cpu: assignment.cpu,
            migrated: false,
        })
    }

    fn validate_affinity(&self, affinity: CpuMask) -> Result<(), SmpError> {
        if affinity.is_empty() {
            return Err(SmpError::EmptyAffinity);
        }
        if affinity.bits() & !self.online_mask.bits() != 0 {
            return Err(SmpError::AffinityOutsideTopology);
        }
        Ok(())
    }

    fn least_loaded_cpu(&self, affinity: CpuMask) -> Result<CpuId, SmpError> {
        let mut selected = None;
        let mut selected_load = usize::MAX;
        for raw in 0..self.queues.len() {
            let cpu = CpuId::from_raw(raw).expect("queue CPU is within the CPU bound");
            if !affinity.contains(cpu) {
                continue;
            }
            let snapshot = self.queues[raw].snapshot();
            let load = snapshot.runnable_count + if snapshot.current.is_some() { 1 } else { 0 };
            if load < selected_load {
                selected = Some(cpu);
                selected_load = load;
            }
        }
        selected.ok_or(SmpError::EmptyAffinity)
    }

    fn assignment_index(&self, thread: ThreadId) -> Option<usize> {
        self.assignments
            .iter()
            .position(|assignment| assignment.thread == thread)
    }

    fn queue(&self, cpu: CpuId) -> Result<&RoundRobin, SmpError> {
        self.queues.get(cpu.raw()).ok_or(SmpError::InvalidCpu)
    }

    fn queue_mut(&mut self, cpu: CpuId) -> Result<&mut RoundRobin, SmpError> {
        self.queues.get_mut(cpu.raw()).ok_or(SmpError::InvalidCpu)
    }
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

    #[test]
    fn affinity_places_threads_and_migrates_when_a_cpu_is_removed() {
        let (_processes, first, second) = two_threads();
        let cpu0 = CpuId::from_raw(0).unwrap();
        let cpu1 = CpuId::from_raw(1).unwrap();
        let mut scheduler = SmpRoundRobin::new(2, 2).unwrap();

        assert_eq!(
            scheduler.admit(first, CpuMask::single(cpu1)).unwrap(),
            Placement {
                thread: first,
                cpu: cpu1,
                migrated: false
            }
        );
        assert_eq!(
            scheduler.admit(second, CpuMask::first(2).unwrap()).unwrap(),
            Placement {
                thread: second,
                cpu: cpu0,
                migrated: false
            }
        );
        assert_eq!(
            scheduler
                .set_affinity(second, CpuMask::single(cpu1))
                .unwrap(),
            Placement {
                thread: second,
                cpu: cpu1,
                migrated: true
            }
        );
        assert_eq!(scheduler.placement(second).unwrap().cpu, cpu1);
    }

    #[test]
    fn unrestricted_threads_balance_across_online_cpus() {
        let mut processes = ProcessTable::new();
        let process = processes.create_process(None).unwrap();
        let first = processes.create_thread(process, "first").unwrap();
        let second = processes.create_thread(process, "second").unwrap();
        let third = processes.create_thread(process, "third").unwrap();
        let all = CpuMask::first(2).unwrap();
        let mut scheduler = SmpRoundRobin::new(2, 1).unwrap();

        assert_eq!(scheduler.admit(first, all).unwrap().cpu.raw(), 0);
        assert_eq!(scheduler.admit(second, all).unwrap().cpu.raw(), 1);
        assert_eq!(scheduler.admit(third, all).unwrap().cpu.raw(), 0);
        assert_eq!(scheduler.snapshot().assigned_thread_count, 3);
    }

    #[test]
    fn affinity_rejects_empty_and_offline_cpu_masks() {
        let (_processes, first, _second) = two_threads();
        let cpu2 = CpuId::from_raw(2).unwrap();
        let mut scheduler = SmpRoundRobin::new(2, 1).unwrap();

        assert_eq!(
            scheduler.admit(first, CpuMask::empty()),
            Err(SmpError::EmptyAffinity)
        );
        assert_eq!(
            scheduler.admit(first, CpuMask::single(cpu2)),
            Err(SmpError::AffinityOutsideTopology)
        );
    }

    #[test]
    fn smp_constructor_preserves_invalid_topology_and_quantum_errors() {
        assert!(matches!(
            SmpRoundRobin::new(0, 1),
            Err(SmpError::InvalidCpuCount)
        ));
        assert!(matches!(
            SmpRoundRobin::new(1, 0),
            Err(SmpError::InvalidQuantum)
        ));
        assert_eq!(CpuMask::first(MAX_CPUS).unwrap().bits(), u64::MAX);
        assert_eq!(CpuMask::first(0), None);
        assert_eq!(CpuMask::first(MAX_CPUS + 1), None);
    }
}
