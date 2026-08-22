//! Synchronized process lifecycle and SMP scheduling state.
//!
//! [`ProcessTable`] is authoritative for process and thread lifecycle, while
//! [`SmpRoundRobin`] is authoritative for affinity, CPU ownership, and queue
//! order. This bounded coordinator owns both so a public operation cannot
//! update one model without applying the corresponding transition to the
//! other.

use crate::{
    process_model::{
        CreateError, JoinError, LookupError, ProcessExit, ProcessId, ProcessSnapshot, ProcessTable,
        ReapError, StateError, ThreadExit, ThreadId, ThreadSnapshot, ThreadState,
    },
    scheduling::{
        AdmissionResult, AssignmentSnapshot, CpuId, CpuMask, CpuSnapshot, MigrationResult,
        Placement, QueueTransition, RebalancePlan, SmpError, SmpRoundRobin, SmpSnapshot, Switch,
        WakeResult,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionError {
    Create(CreateError),
    State(StateError),
    Scheduling(SmpError),
}

impl From<CreateError> for ExecutionError {
    fn from(error: CreateError) -> Self {
        Self::Create(error)
    }
}

impl From<StateError> for ExecutionError {
    fn from(error: StateError) -> Self {
        Self::State(error)
    }
}

impl From<SmpError> for ExecutionError {
    fn from(error: SmpError) -> Self {
        Self::Scheduling(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitResult {
    pub queue: QueueTransition,
    pub process_exit: Option<ProcessExit>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RebalanceResult {
    pub plan: RebalancePlan,
    pub migration: MigrationResult,
}

/// Bounded owner of lifecycle identities and their SMP queue assignments.
pub struct SmpExecution {
    processes: ProcessTable,
    scheduler: SmpRoundRobin,
}

impl SmpExecution {
    pub fn new(cpu_count: usize, quantum_ticks: u64) -> Result<Self, SmpError> {
        Ok(Self {
            processes: ProcessTable::new(),
            scheduler: SmpRoundRobin::new(cpu_count, quantum_ticks)?,
        })
    }

    pub fn create_process(&mut self, parent: Option<ProcessId>) -> Result<ProcessId, CreateError> {
        self.processes.create_process(parent)
    }

    pub fn create_thread(
        &mut self,
        process: ProcessId,
        name: &'static str,
        affinity: CpuMask,
    ) -> Result<AdmissionResult, ExecutionError> {
        self.processes.validate_thread_creation(process)?;
        self.scheduler.validate_admission(affinity)?;
        let thread = self.processes.create_thread(process, name)?;
        let admission = match self.scheduler.admit_with_transition(thread, affinity) {
            Ok(admission) => admission,
            Err(error) => {
                self.processes
                    .rollback_thread_creation(thread)
                    .expect("newly created thread must be safe to roll back");
                return Err(error.into());
            }
        };
        self.activate(admission.switch);
        Ok(admission)
    }

    /// Admit an architecture-owned context with a preallocated nonzero identity.
    pub fn create_thread_with_id(
        &mut self,
        process: ProcessId,
        thread: ThreadId,
        name: &'static str,
        affinity: CpuMask,
    ) -> Result<AdmissionResult, ExecutionError> {
        self.processes.validate_thread_creation(process)?;
        self.scheduler.validate_admission(affinity)?;
        self.processes
            .create_thread_with_id(process, thread, name)?;
        let admission = match self.scheduler.admit_with_transition(thread, affinity) {
            Ok(admission) => admission,
            Err(error) => {
                self.processes
                    .rollback_thread_creation(thread)
                    .expect("newly created thread must be safe to roll back");
                return Err(error.into());
            }
        };
        self.activate(admission.switch);
        Ok(admission)
    }

    pub fn process_snapshot(&self, process: ProcessId) -> Result<ProcessSnapshot, LookupError> {
        self.processes.process_snapshot(process)
    }

    pub fn thread_snapshot(&self, thread: ThreadId) -> Result<ThreadSnapshot, LookupError> {
        self.processes.thread_snapshot(thread)
    }

    pub fn scheduler_snapshot(&self) -> SmpSnapshot {
        self.scheduler.snapshot()
    }

    pub fn cpu_snapshot(&self, cpu: CpuId) -> Result<CpuSnapshot, SmpError> {
        self.scheduler.cpu_snapshot(cpu)
    }

    pub fn assignment_snapshot(&self, thread: ThreadId) -> Result<AssignmentSnapshot, SmpError> {
        self.scheduler.assignment_snapshot(thread)
    }

    pub fn placement(&self, thread: ThreadId) -> Result<Placement, SmpError> {
        self.scheduler.placement(thread)
    }

    pub fn thread_state_count(
        &self,
        process: ProcessId,
        state: ThreadState,
    ) -> Result<usize, LookupError> {
        self.processes.thread_state_count(process, state)
    }

    pub fn rebalance_plan(
        &self,
        eligible_cpus: CpuMask,
    ) -> Result<Option<RebalancePlan>, SmpError> {
        self.scheduler.rebalance_plan(eligible_cpus)
    }

    pub fn tick(&mut self, cpu: CpuId) -> Result<Option<Switch>, ExecutionError> {
        let switch = self.scheduler.tick(cpu)?;
        self.rotate(switch);
        Ok(switch)
    }

    pub fn yield_current(&mut self, cpu: CpuId) -> Result<Option<Switch>, ExecutionError> {
        let switch = self.scheduler.yield_current(cpu)?;
        self.rotate(switch);
        Ok(switch)
    }

    pub fn block_thread(&mut self, thread: ThreadId) -> Result<QueueTransition, ExecutionError> {
        self.require_state(thread, &[ThreadState::Runnable, ThreadState::Running])?;
        let transition = self.scheduler.block(thread)?;
        self.processes
            .block_thread(thread)
            .expect("block transition was prevalidated");
        self.activate(transition.switch);
        Ok(transition)
    }

    pub fn sleep_thread(&mut self, thread: ThreadId) -> Result<QueueTransition, ExecutionError> {
        self.require_state(thread, &[ThreadState::Runnable, ThreadState::Running])?;
        let transition = self.scheduler.block(thread)?;
        self.processes
            .sleep_thread(thread)
            .expect("sleep transition was prevalidated");
        self.activate(transition.switch);
        Ok(transition)
    }

    pub fn wake_thread(&mut self, thread: ThreadId) -> Result<WakeResult, ExecutionError> {
        self.require_state(thread, &[ThreadState::Blocked, ThreadState::Sleeping])?;
        let wake = self.scheduler.wake(thread)?;
        self.processes
            .wake_thread(thread)
            .expect("wake transition was prevalidated");
        self.activate(wake.switch);
        Ok(wake)
    }

    pub fn stop_thread(
        &mut self,
        thread: ThreadId,
    ) -> Result<Option<QueueTransition>, ExecutionError> {
        let state = self.thread_state(thread)?;
        let transition = match state {
            ThreadState::Runnable | ThreadState::Running => Some(self.scheduler.block(thread)?),
            ThreadState::Blocked | ThreadState::Sleeping => None,
            ThreadState::Stopped | ThreadState::Exited => {
                return Err(StateError::InvalidTransition.into());
            }
        };
        self.processes
            .stop_thread(thread)
            .expect("stop transition was prevalidated");
        if let Some(transition) = transition {
            self.activate(transition.switch);
        }
        Ok(transition)
    }

    pub fn continue_thread(&mut self, thread: ThreadId) -> Result<WakeResult, ExecutionError> {
        self.require_state(thread, &[ThreadState::Stopped])?;
        let wake = self.scheduler.wake(thread)?;
        self.processes
            .continue_thread(thread)
            .expect("continue transition was prevalidated");
        self.activate(wake.switch);
        Ok(wake)
    }

    pub fn set_affinity(
        &mut self,
        thread: ThreadId,
        affinity: CpuMask,
    ) -> Result<MigrationResult, ExecutionError> {
        self.require_active(thread)?;
        let migration = self
            .scheduler
            .set_affinity_with_transition(thread, affinity)?;
        self.apply_migration(thread, migration);
        Ok(migration)
    }

    pub fn migrate(
        &mut self,
        thread: ThreadId,
        destination: CpuId,
    ) -> Result<MigrationResult, ExecutionError> {
        self.require_active(thread)?;
        let migration = self
            .scheduler
            .migrate_with_transition(thread, destination)?;
        self.apply_migration(thread, migration);
        Ok(migration)
    }

    pub fn rebalance_once(
        &mut self,
        eligible_cpus: CpuMask,
    ) -> Result<Option<RebalanceResult>, ExecutionError> {
        let Some(plan) = self.scheduler.rebalance_plan(eligible_cpus)? else {
            return Ok(None);
        };
        let migration = self.migrate(plan.thread, plan.to)?;
        Ok(Some(RebalanceResult { plan, migration }))
    }

    pub fn exit_thread(
        &mut self,
        thread: ThreadId,
        status: u64,
    ) -> Result<ExitResult, ExecutionError> {
        let state = self.thread_state(thread)?;
        if state == ThreadState::Exited {
            return Err(StateError::InvalidTransition.into());
        }
        let queue = self.scheduler.remove(thread)?;
        let process_exit = self
            .processes
            .exit_thread(thread, status)
            .expect("exit transition was prevalidated");
        self.activate(queue.switch);
        Ok(ExitResult {
            queue,
            process_exit,
        })
    }

    pub fn detach_thread(&mut self, thread: ThreadId) -> Result<(), StateError> {
        self.processes.detach_thread(thread)
    }

    pub fn join_thread(&mut self, thread: ThreadId) -> Result<ThreadExit, JoinError> {
        self.processes.join_thread(thread)
    }

    pub fn reap_process(&mut self, process: ProcessId) -> Result<ProcessExit, ReapError> {
        self.processes.reap_process(process)
    }

    fn thread_state(&self, thread: ThreadId) -> Result<ThreadState, ExecutionError> {
        self.processes
            .thread_snapshot(thread)
            .map(|snapshot| snapshot.state)
            .map_err(|_| StateError::InvalidThread.into())
    }

    fn require_state(
        &self,
        thread: ThreadId,
        allowed: &[ThreadState],
    ) -> Result<(), ExecutionError> {
        if allowed.contains(&self.thread_state(thread)?) {
            Ok(())
        } else {
            Err(StateError::InvalidTransition.into())
        }
    }

    fn require_active(&self, thread: ThreadId) -> Result<(), ExecutionError> {
        if self.thread_state(thread)? == ThreadState::Exited {
            Err(StateError::InvalidTransition.into())
        } else {
            Ok(())
        }
    }

    fn rotate(&mut self, switch: Option<Switch>) {
        let Some(switch) = switch else {
            return;
        };
        let from = switch
            .from
            .expect("round-robin rotation must identify its running thread");
        self.processes
            .yield_thread(from)
            .expect("scheduler current thread must be lifecycle-running");
        self.processes
            .run_thread(switch.to)
            .expect("scheduler destination thread must be lifecycle-runnable");
    }

    fn activate(&mut self, switch: Option<Switch>) {
        if let Some(switch) = switch {
            self.processes
                .run_thread(switch.to)
                .expect("scheduler destination thread must be lifecycle-runnable");
        }
    }

    fn apply_migration(&mut self, thread: ThreadId, migration: MigrationResult) {
        if migration.source_was_current {
            self.processes
                .yield_thread(thread)
                .expect("migrated scheduler current must be lifecycle-running");
            self.activate(migration.source_switch);
        }
        self.activate(migration.destination_switch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        process_model::ProcessState,
        scheduling::{AssignmentState, DEFAULT_QUANTUM_TICKS},
    };

    fn cpu(raw: usize) -> CpuId {
        CpuId::from_raw(raw).unwrap()
    }

    #[test]
    fn admission_ticks_and_yields_keep_lifecycle_states_synchronized() {
        let cpu0 = cpu(0);
        let mut execution = SmpExecution::new(1, 1).unwrap();
        let process = execution.create_process(None).unwrap();
        let first = execution
            .create_thread(process, "first", CpuMask::single(cpu0))
            .unwrap()
            .placement
            .thread;
        let second = execution
            .create_thread(process, "second", CpuMask::single(cpu0))
            .unwrap()
            .placement
            .thread;

        assert_eq!(
            execution.thread_snapshot(first).unwrap().state,
            ThreadState::Running
        );
        assert_eq!(
            execution.thread_snapshot(second).unwrap().state,
            ThreadState::Runnable
        );

        assert_eq!(execution.tick(cpu0).unwrap().unwrap().to, second);
        assert_eq!(
            execution.thread_snapshot(first).unwrap().state,
            ThreadState::Runnable
        );
        assert_eq!(
            execution.thread_snapshot(second).unwrap().state,
            ThreadState::Running
        );

        assert_eq!(execution.yield_current(cpu0).unwrap().unwrap().to, first);
        assert_eq!(
            execution.thread_snapshot(first).unwrap().state,
            ThreadState::Running
        );
        assert_eq!(
            execution.thread_snapshot(second).unwrap().state,
            ThreadState::Runnable
        );
    }

    #[test]
    fn block_sleep_wake_stop_and_continue_share_one_queue_state() {
        let cpu0 = cpu(0);
        let mut execution = SmpExecution::new(1, DEFAULT_QUANTUM_TICKS).unwrap();
        let process = execution.create_process(None).unwrap();
        let first = execution
            .create_thread(process, "first", CpuMask::single(cpu0))
            .unwrap()
            .placement
            .thread;
        let second = execution
            .create_thread(process, "second", CpuMask::single(cpu0))
            .unwrap()
            .placement
            .thread;

        assert_eq!(
            execution.block_thread(first).unwrap().switch.unwrap().to,
            second
        );
        assert_eq!(
            execution.thread_snapshot(first).unwrap().state,
            ThreadState::Blocked
        );
        assert_eq!(execution.scheduler_snapshot().blocked_thread_count, 1);
        assert_eq!(execution.wake_thread(first).unwrap().switch, None);
        assert_eq!(
            execution.thread_snapshot(first).unwrap().state,
            ThreadState::Runnable
        );

        assert_eq!(
            execution.sleep_thread(second).unwrap().switch.unwrap().to,
            first
        );
        assert_eq!(
            execution.thread_snapshot(second).unwrap().state,
            ThreadState::Sleeping
        );
        assert_eq!(execution.stop_thread(second).unwrap(), None);
        assert_eq!(
            execution.thread_snapshot(second).unwrap().state,
            ThreadState::Stopped
        );
        assert_eq!(execution.continue_thread(second).unwrap().switch, None);
        assert_eq!(
            execution.thread_snapshot(second).unwrap().state,
            ThreadState::Runnable
        );
        assert_eq!(execution.scheduler_snapshot().blocked_thread_count, 0);
    }

    #[test]
    fn current_and_blocked_migrations_preserve_running_invariants() {
        let cpu0 = cpu(0);
        let cpu1 = cpu(1);
        let mut execution = SmpExecution::new(2, DEFAULT_QUANTUM_TICKS).unwrap();
        let process = execution.create_process(None).unwrap();
        let first = execution
            .create_thread(process, "first", CpuMask::single(cpu0))
            .unwrap()
            .placement
            .thread;
        let second = execution
            .create_thread(process, "second", CpuMask::single(cpu0))
            .unwrap()
            .placement
            .thread;

        let migration = execution
            .set_affinity(first, CpuMask::single(cpu1))
            .unwrap();
        assert!(migration.source_was_current);
        assert_eq!(migration.source_switch.unwrap().to, second);
        assert_eq!(migration.destination_switch.unwrap().to, first);
        assert_eq!(
            execution.thread_snapshot(first).unwrap().state,
            ThreadState::Running
        );
        assert_eq!(
            execution.thread_snapshot(second).unwrap().state,
            ThreadState::Running
        );

        execution.block_thread(first).unwrap();
        let blocked_migration = execution
            .set_affinity(first, CpuMask::single(cpu0))
            .unwrap();
        assert!(!blocked_migration.source_was_current);
        assert_eq!(blocked_migration.source_switch, None);
        assert_eq!(blocked_migration.destination_switch, None);
        assert_eq!(
            execution.assignment_snapshot(first).unwrap().state,
            AssignmentState::Blocked
        );
        assert_eq!(
            execution.thread_snapshot(first).unwrap().state,
            ThreadState::Blocked
        );
    }

    #[test]
    fn one_rebalance_moves_a_queued_thread_and_marks_its_idle_cpu_running() {
        let cpu0 = cpu(0);
        let cpu1 = cpu(1);
        let both = CpuMask::single(cpu0).union(CpuMask::single(cpu1));
        let mut execution = SmpExecution::new(2, DEFAULT_QUANTUM_TICKS).unwrap();
        let process = execution.create_process(None).unwrap();
        for index in 0..3 {
            let admission = execution
                .create_thread(process, "worker", CpuMask::single(cpu0))
                .unwrap();
            let thread = admission.placement.thread;
            execution.set_affinity(thread, both).unwrap();
            assert_eq!(execution.assignment_snapshot(thread).unwrap().cpu, cpu0);
            if index == 0 {
                assert_eq!(
                    execution.thread_snapshot(thread).unwrap().state,
                    ThreadState::Running
                );
            }
        }

        let result = execution.rebalance_once(both).unwrap().unwrap();
        assert_eq!(result.plan.from, cpu0);
        assert_eq!(result.plan.to, cpu1);
        assert!(!result.migration.source_was_current);
        assert_eq!(
            result.migration.destination_switch.unwrap().to,
            result.plan.thread
        );
        assert_eq!(
            execution.thread_snapshot(result.plan.thread).unwrap().state,
            ThreadState::Running
        );
        assert_eq!(execution.cpu_snapshot(cpu0).unwrap().runnable_count, 1);
        assert_eq!(
            execution.cpu_snapshot(cpu1).unwrap().current,
            Some(result.plan.thread)
        );
    }

    #[test]
    fn exit_detach_join_and_reap_remove_scheduler_assignments_first() {
        let cpu0 = cpu(0);
        let mut execution = SmpExecution::new(1, DEFAULT_QUANTUM_TICKS).unwrap();
        let process = execution.create_process(None).unwrap();
        let detached = execution
            .create_thread(process, "detached", CpuMask::single(cpu0))
            .unwrap()
            .placement
            .thread;
        let joinable = execution
            .create_thread(process, "joinable", CpuMask::single(cpu0))
            .unwrap()
            .placement
            .thread;

        execution.detach_thread(detached).unwrap();
        let first_exit = execution.exit_thread(detached, 7).unwrap();
        assert_eq!(first_exit.queue.switch.unwrap().to, joinable);
        assert_eq!(first_exit.process_exit, None);
        assert_eq!(
            execution.thread_snapshot(detached),
            Err(LookupError::InvalidThread)
        );
        assert_eq!(execution.scheduler_snapshot().assigned_thread_count, 1);

        let final_exit = execution.exit_thread(joinable, 9).unwrap();
        assert_eq!(final_exit.process_exit.unwrap().status, 9);
        assert_eq!(execution.scheduler_snapshot().assigned_thread_count, 0);
        assert_eq!(
            execution.set_affinity(joinable, CpuMask::single(cpu0)),
            Err(ExecutionError::State(StateError::InvalidTransition))
        );
        assert_eq!(execution.join_thread(joinable).unwrap().status, 9);
        assert_eq!(execution.reap_process(process).unwrap().status, 9);
    }

    #[test]
    fn failed_admission_leaves_the_process_and_scheduler_unchanged() {
        let mut execution = SmpExecution::new(1, DEFAULT_QUANTUM_TICKS).unwrap();
        let process = execution.create_process(None).unwrap();

        assert_eq!(
            execution.create_thread(process, "invalid", CpuMask::empty()),
            Err(ExecutionError::Scheduling(SmpError::EmptyAffinity))
        );
        assert_eq!(execution.scheduler_snapshot().assigned_thread_count, 0);
        assert_eq!(execution.process_snapshot(process).unwrap().thread_count, 0);
        assert_eq!(
            execution.process_snapshot(process).unwrap().state,
            ProcessState::Created
        );

        let missing = ProcessId::from_raw(99).unwrap();
        assert_eq!(
            execution.create_thread(missing, "missing", CpuMask::single(cpu(0))),
            Err(ExecutionError::Create(CreateError::InvalidProcess))
        );
        assert_eq!(execution.scheduler_snapshot().assigned_thread_count, 0);
    }

    #[test]
    fn explicit_thread_identity_is_authoritative_and_does_not_collide_with_allocation() {
        let cpu0 = cpu(0);
        let mut execution = SmpExecution::new(1, DEFAULT_QUANTUM_TICKS).unwrap();
        let process = execution.create_process(None).unwrap();
        let explicit = ThreadId::from_raw(1).unwrap();

        let admission = execution
            .create_thread_with_id(
                process,
                explicit,
                "architecture-context",
                CpuMask::single(cpu0),
            )
            .unwrap();
        assert_eq!(admission.placement.thread, explicit);
        assert_eq!(
            execution.thread_snapshot(explicit).unwrap().state,
            ThreadState::Running
        );
        assert_eq!(
            execution
                .thread_state_count(process, ThreadState::Running)
                .unwrap(),
            1
        );
        assert_eq!(
            execution.create_thread_with_id(process, explicit, "duplicate", CpuMask::single(cpu0),),
            Err(ExecutionError::Create(CreateError::DuplicateThread))
        );

        let allocated = execution
            .create_thread(process, "allocated", CpuMask::single(cpu0))
            .unwrap()
            .placement
            .thread;
        assert_ne!(allocated, explicit);
        assert_eq!(allocated.raw(), 2);
        assert_eq!(execution.scheduler_snapshot().assigned_thread_count, 2);
    }
}
