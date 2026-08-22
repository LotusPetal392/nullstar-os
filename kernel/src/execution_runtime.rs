//! Kernel-wide ownership of live process/thread lifecycle and SMP queue state.
//!
//! Architecture schedulers retain saved register contexts and address spaces,
//! while this runtime is the single shared owner of lifecycle identities,
//! affinity, CPU placement, and queue transitions.

use spin::Mutex;

use crate::{
    process_model::{
        JoinError, LookupError, ProcessExit, ProcessId, ProcessState, ReapError, ThreadId,
        ThreadState,
    },
    scheduling::{CpuId, CpuMask, DEFAULT_QUANTUM_TICKS, MAX_CPUS, SmpError, SmpSnapshot},
    smp_execution::{ExecutionError, ExitResult, SmpExecution},
};

const KERNEL_THREAD_ID_BASE: u64 = 1_u64 << 63;

static EXECUTION_RUNTIME: Mutex<Option<ExecutionRuntime>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    AlreadyInitialized,
    Unavailable,
    InvalidContextSlot,
    InvalidContextIdentity,
    CpuAlreadyRegistered,
    EmptyContextSet,
    Scheduling(SmpError),
    Execution(ExecutionError),
    Lookup(LookupError),
    Join(JoinError),
    Reap(ReapError),
    Rollback(ExecutionError),
}

impl From<SmpError> for RuntimeError {
    fn from(error: SmpError) -> Self {
        Self::Scheduling(error)
    }
}

impl From<ExecutionError> for RuntimeError {
    fn from(error: ExecutionError) -> Self {
        Self::Execution(error)
    }
}

impl From<LookupError> for RuntimeError {
    fn from(error: LookupError) -> Self {
        Self::Lookup(error)
    }
}

impl From<JoinError> for RuntimeError {
    fn from(error: JoinError) -> Self {
        Self::Join(error)
    }
}

impl From<ReapError> for RuntimeError {
    fn from(error: ReapError) -> Self {
        Self::Reap(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KernelContextRegistration {
    slot: u8,
    name: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionContext {
    pub process: ProcessId,
    pub thread: ThreadId,
    pub cpu: CpuId,
}

impl KernelContextRegistration {
    pub fn new(slot: u8, name: &'static str) -> Result<Self, RuntimeError> {
        kernel_thread_id(CpuId::from_raw(0).expect("CPU 0 is valid"), slot)
            .ok_or(RuntimeError::InvalidContextSlot)?;
        Ok(Self { slot, name })
    }

    fn thread_id(self, cpu: CpuId) -> ThreadId {
        kernel_thread_id(cpu, self.slot).expect("registration slot was validated")
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuLifecycleSnapshot {
    pub process_id: Option<ProcessId>,
    pub thread_count: usize,
    pub running_thread_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    pub scheduler: SmpSnapshot,
    pub registered_cpu_count: usize,
    pub process_count: usize,
    pub thread_count: usize,
}

/// Shared execution state used by every architecture context store.
pub struct ExecutionRuntime {
    execution: SmpExecution,
    cpu_processes: [Option<ProcessId>; MAX_CPUS],
}

impl ExecutionRuntime {
    pub fn new(
        cpu_capacity: usize,
        initial_online_mask: CpuMask,
        quantum_ticks: u64,
    ) -> Result<Self, RuntimeError> {
        Ok(Self {
            execution: SmpExecution::with_online_mask(
                cpu_capacity,
                quantum_ticks,
                initial_online_mask,
            )?,
            cpu_processes: [None; MAX_CPUS],
        })
    }

    /// Publish one online CPU and atomically admit its architecture contexts.
    pub fn register_cpu_contexts(
        &mut self,
        cpu: CpuId,
        contexts: &[KernelContextRegistration],
    ) -> Result<ProcessId, RuntimeError> {
        if contexts.is_empty() {
            return Err(RuntimeError::EmptyContextSet);
        }
        let process_slot = self
            .cpu_processes
            .get_mut(cpu.raw())
            .ok_or(RuntimeError::Scheduling(SmpError::InvalidCpu))?;
        if process_slot.is_some() {
            return Err(RuntimeError::CpuAlreadyRegistered);
        }

        self.execution.online_cpu(cpu)?;
        let process = self
            .execution
            .create_process(None)
            .map_err(ExecutionError::from)?;
        for context in contexts {
            if let Err(error) = self.execution.create_thread_with_id(
                process,
                context.thread_id(cpu),
                context.name,
                CpuMask::single(cpu),
            ) {
                if let Err(rollback) = self.execution.rollback_process_creation(process) {
                    return Err(RuntimeError::Rollback(rollback));
                }
                return Err(error.into());
            }
        }
        *process_slot = Some(process);
        Ok(process)
    }

    pub fn cpu_lifecycle_snapshot(&self, cpu: CpuId) -> CpuLifecycleSnapshot {
        let Some(process) = self.cpu_processes.get(cpu.raw()).copied().flatten() else {
            return CpuLifecycleSnapshot::default();
        };
        let thread_count = self
            .execution
            .process_snapshot(process)
            .map(|snapshot| snapshot.thread_count)
            .unwrap_or(0);
        let running_thread_count = self
            .execution
            .thread_state_count(process, ThreadState::Running)
            .unwrap_or(0);
        CpuLifecycleSnapshot {
            process_id: Some(process),
            thread_count,
            running_thread_count,
        }
    }

    /// Atomically admit one userspace compatibility task as a lifecycle
    /// process with one schedulable thread.
    pub fn register_userspace_context(
        &mut self,
        parent: Option<ProcessId>,
        name: &'static str,
        cpu: CpuId,
    ) -> Result<ExecutionContext, RuntimeError> {
        let process = self
            .execution
            .create_process(parent)
            .map_err(ExecutionError::from)?;
        let admission = match self
            .execution
            .create_thread(process, name, CpuMask::single(cpu))
        {
            Ok(admission) => admission,
            Err(error) => {
                if let Err(rollback) = self.execution.rollback_process_creation(process) {
                    return Err(RuntimeError::Rollback(rollback));
                }
                return Err(error.into());
            }
        };
        Ok(ExecutionContext {
            process,
            thread: admission.placement.thread,
            cpu: admission.placement.cpu,
        })
    }

    pub fn exit_userspace_context(
        &mut self,
        context: ExecutionContext,
        status: u64,
    ) -> Result<ExitResult, RuntimeError> {
        self.validate_context_identity(context)?;
        Ok(self.execution.exit_thread(context.thread, status)?)
    }

    pub fn reap_userspace_context(
        &mut self,
        context: ExecutionContext,
    ) -> Result<ProcessExit, RuntimeError> {
        self.validate_context_identity(context)?;
        let thread = self.execution.thread_snapshot(context.thread)?;
        if thread.detached {
            return Err(RuntimeError::Join(JoinError::Detached));
        }
        if thread.state != ThreadState::Exited {
            return Err(RuntimeError::Join(JoinError::NotExited));
        }
        let process = self.execution.process_snapshot(context.process)?;
        if process.state != ProcessState::Exited {
            return Err(RuntimeError::Reap(ReapError::NotExited));
        }
        self.execution.orphan_process_children(context.process)?;
        let exit = self.execution.join_thread(context.thread)?;
        if exit.process_id != context.process {
            return Err(RuntimeError::InvalidContextIdentity);
        }
        Ok(self.execution.reap_process(context.process)?)
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            scheduler: self.execution.scheduler_snapshot(),
            registered_cpu_count: self
                .cpu_processes
                .iter()
                .filter(|process| process.is_some())
                .count(),
            process_count: self.execution.process_count(),
            thread_count: self.execution.thread_count(),
        }
    }

    pub fn execution(&self) -> &SmpExecution {
        &self.execution
    }

    pub fn execution_mut(&mut self) -> &mut SmpExecution {
        &mut self.execution
    }

    fn validate_context_identity(&self, context: ExecutionContext) -> Result<(), RuntimeError> {
        let thread = self.execution.thread_snapshot(context.thread)?;
        if thread.process_id != context.process {
            return Err(RuntimeError::InvalidContextIdentity);
        }
        Ok(())
    }
}

pub fn kernel_thread_id(cpu: CpuId, slot: u8) -> Option<ThreadId> {
    let encoded_slot = u64::from(slot).checked_add(1)?;
    if encoded_slot > u64::from(u8::MAX) {
        return None;
    }
    let raw = KERNEL_THREAD_ID_BASE | ((cpu.raw() as u64) << 8) | encoded_slot;
    ThreadId::from_raw(raw)
}

pub fn initialize_bootstrap() -> Result<(), RuntimeError> {
    without_local_interrupts(|| {
        let mut runtime = EXECUTION_RUNTIME.lock();
        if runtime.is_some() {
            return Err(RuntimeError::AlreadyInitialized);
        }
        let cpu0 = CpuId::from_raw(0).expect("CPU 0 is valid");
        *runtime = Some(ExecutionRuntime::new(
            MAX_CPUS,
            CpuMask::single(cpu0),
            DEFAULT_QUANTUM_TICKS,
        )?);
        Ok(())
    })
}

pub fn with<R>(operation: impl FnOnce(&ExecutionRuntime) -> R) -> Result<R, RuntimeError> {
    without_local_interrupts(|| {
        let runtime = EXECUTION_RUNTIME.lock();
        runtime
            .as_ref()
            .map(operation)
            .ok_or(RuntimeError::Unavailable)
    })
}

pub fn with_mut<R>(operation: impl FnOnce(&mut ExecutionRuntime) -> R) -> Result<R, RuntimeError> {
    without_local_interrupts(|| {
        let mut runtime = EXECUTION_RUNTIME.lock();
        runtime
            .as_mut()
            .map(operation)
            .ok_or(RuntimeError::Unavailable)
    })
}

pub fn try_with_mut<R>(operation: impl FnOnce(&mut ExecutionRuntime) -> R) -> Option<R> {
    without_local_interrupts(|| {
        let mut runtime = EXECUTION_RUNTIME.try_lock()?;
        runtime.as_mut().map(operation)
    })
}

/// Prevent the local timer handler from re-entering the runtime while this
/// CPU owns its global lock. Host policy tests cannot manipulate interrupt
/// state, so they execute the same critical section directly.
fn without_local_interrupts<R>(operation: impl FnOnce() -> R) -> R {
    #[cfg(target_os = "none")]
    {
        x86_64::instructions::interrupts::without_interrupts(operation)
    }
    #[cfg(not(target_os = "none"))]
    {
        operation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        process_model::{CreateError, ThreadState},
        scheduling::AssignmentState,
    };

    fn cpu(raw: usize) -> CpuId {
        CpuId::from_raw(raw).unwrap()
    }

    fn context(slot: u8, name: &'static str) -> KernelContextRegistration {
        KernelContextRegistration::new(slot, name).unwrap()
    }

    #[test]
    fn runtime_tracks_capacity_separately_from_online_cpus() {
        let cpu0 = cpu(0);
        let cpu2 = cpu(2);
        let mut runtime =
            ExecutionRuntime::new(4, CpuMask::single(cpu0), DEFAULT_QUANTUM_TICKS).unwrap();

        let initial = runtime.snapshot();
        assert_eq!(initial.scheduler.cpu_capacity, 4);
        assert_eq!(initial.scheduler.cpu_count, 1);
        assert_eq!(initial.scheduler.online_mask, CpuMask::single(cpu0));
        assert_eq!(
            runtime.execution().cpu_snapshot(cpu2),
            Err(SmpError::InvalidCpu)
        );

        let process = runtime
            .register_cpu_contexts(cpu2, &[context(0, "cpu2-a"), context(1, "cpu2-b")])
            .unwrap();
        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.scheduler.cpu_count, 2);
        assert_eq!(
            snapshot.scheduler.online_mask,
            CpuMask::single(cpu0).union(CpuMask::single(cpu2))
        );
        assert_eq!(snapshot.registered_cpu_count, 1);
        assert_eq!(snapshot.process_count, 1);
        assert_eq!(snapshot.thread_count, 2);
        assert_eq!(
            runtime.register_cpu_contexts(cpu2, &[context(2, "duplicate-cpu")]),
            Err(RuntimeError::CpuAlreadyRegistered)
        );
        assert_eq!(runtime.snapshot(), snapshot);

        let lifecycle = runtime.cpu_lifecycle_snapshot(cpu2);
        assert_eq!(lifecycle.process_id, Some(process));
        assert_eq!(lifecycle.thread_count, 2);
        assert_eq!(lifecycle.running_thread_count, 1);
        assert_eq!(
            runtime
                .execution()
                .assignment_snapshot(kernel_thread_id(cpu2, 1).unwrap())
                .unwrap()
                .state,
            AssignmentState::Runnable
        );
    }

    #[test]
    fn failed_context_batch_rolls_back_process_threads_and_assignments() {
        let cpu0 = cpu(0);
        let cpu1 = cpu(1);
        let mut runtime =
            ExecutionRuntime::new(2, CpuMask::single(cpu0), DEFAULT_QUANTUM_TICKS).unwrap();
        let duplicate = context(0, "duplicate");

        assert_eq!(
            runtime.register_cpu_contexts(cpu1, &[duplicate, duplicate]),
            Err(RuntimeError::Execution(ExecutionError::Create(
                CreateError::DuplicateThread
            )))
        );
        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.registered_cpu_count, 0);
        assert_eq!(snapshot.process_count, 0);
        assert_eq!(snapshot.thread_count, 0);
        assert_eq!(snapshot.scheduler.assigned_thread_count, 0);
        assert_eq!(snapshot.scheduler.cpu_count, 2);
        assert_eq!(
            runtime.cpu_lifecycle_snapshot(cpu1),
            CpuLifecycleSnapshot::default()
        );
    }

    #[test]
    fn context_identity_namespace_is_stable_and_rejects_overflow_slot() {
        let cpu1 = cpu(1);
        assert_eq!(
            kernel_thread_id(cpu1, 0).unwrap().raw(),
            KERNEL_THREAD_ID_BASE | (1_u64 << 8) | 1
        );
        assert_ne!(kernel_thread_id(cpu1, 0), kernel_thread_id(cpu1, 1));
        assert_eq!(kernel_thread_id(cpu1, u8::MAX), None);
        assert_eq!(
            KernelContextRegistration::new(u8::MAX, "overflow"),
            Err(RuntimeError::InvalidContextSlot)
        );
    }

    #[test]
    fn lifecycle_snapshot_follows_shared_execution_transitions() {
        let cpu0 = cpu(0);
        let mut runtime =
            ExecutionRuntime::new(1, CpuMask::single(cpu0), DEFAULT_QUANTUM_TICKS).unwrap();
        runtime
            .register_cpu_contexts(cpu0, &[context(0, "first"), context(1, "second")])
            .unwrap();
        let first = kernel_thread_id(cpu0, 0).unwrap();

        runtime.execution_mut().block_thread(first).unwrap();
        assert_eq!(
            runtime.execution().thread_snapshot(first).unwrap().state,
            ThreadState::Blocked
        );
        assert_eq!(runtime.cpu_lifecycle_snapshot(cpu0).running_thread_count, 1);
    }

    #[test]
    fn userspace_context_registration_preserves_parentage_and_reaps_atomically() {
        let cpu0 = cpu(0);
        let mut runtime =
            ExecutionRuntime::new(1, CpuMask::single(cpu0), DEFAULT_QUANTUM_TICKS).unwrap();
        let parent = runtime
            .register_userspace_context(None, "parent", cpu0)
            .unwrap();
        let child = runtime
            .register_userspace_context(Some(parent.process), "child", cpu0)
            .unwrap();

        assert_eq!(
            runtime
                .execution()
                .process_snapshot(child.process)
                .unwrap()
                .parent,
            Some(parent.process)
        );
        assert_eq!(
            runtime.reap_userspace_context(parent),
            Err(RuntimeError::Join(JoinError::NotExited))
        );
        assert_eq!(
            runtime
                .execution()
                .process_snapshot(parent.process)
                .unwrap()
                .child_count,
            1
        );
        assert_eq!(
            runtime
                .execution()
                .process_snapshot(child.process)
                .unwrap()
                .parent,
            Some(parent.process)
        );
        runtime.exit_userspace_context(parent, 17).unwrap();
        assert_eq!(runtime.reap_userspace_context(parent).unwrap().status, 17);
        assert_eq!(
            runtime
                .execution()
                .process_snapshot(child.process)
                .unwrap()
                .parent,
            None
        );
        assert_eq!(runtime.snapshot().process_count, 1);
        assert_eq!(runtime.snapshot().thread_count, 1);
    }

    #[test]
    fn failed_userspace_admission_rolls_back_the_process_identity() {
        let cpu0 = cpu(0);
        let cpu1 = cpu(1);
        let mut runtime =
            ExecutionRuntime::new(2, CpuMask::single(cpu0), DEFAULT_QUANTUM_TICKS).unwrap();

        assert!(matches!(
            runtime.register_userspace_context(None, "offline", cpu1),
            Err(RuntimeError::Execution(ExecutionError::Scheduling(
                SmpError::AffinityOutsideTopology
            )))
        ));
        assert_eq!(runtime.snapshot().process_count, 0);
        assert_eq!(runtime.snapshot().thread_count, 0);
    }

    #[test]
    fn full_topology_context_admission_stays_within_shared_capacity() {
        let cpu0 = cpu(0);
        let mut runtime =
            ExecutionRuntime::new(MAX_CPUS, CpuMask::single(cpu0), DEFAULT_QUANTUM_TICKS).unwrap();
        let standard = [context(0, "probe-a"), context(1, "probe-b")];
        let expanded = [
            context(0, "probe-a"),
            context(1, "probe-b"),
            context(2, "migration"),
            context(3, "balance-a"),
            context(4, "balance-b"),
            context(5, "block-wake"),
            context(6, "balance-c"),
        ];
        let bootstrap = [
            context(0, "bootstrap"),
            context(1, "scheduler-a"),
            context(2, "scheduler-b"),
        ];

        runtime.register_cpu_contexts(cpu0, &bootstrap).unwrap();

        for raw in 1..MAX_CPUS {
            let contexts = if raw == 1 {
                &expanded[..]
            } else {
                &standard[..]
            };
            runtime.register_cpu_contexts(cpu(raw), contexts).unwrap();
        }

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.scheduler.cpu_capacity, MAX_CPUS);
        assert_eq!(snapshot.scheduler.cpu_count, MAX_CPUS);
        assert_eq!(snapshot.scheduler.online_mask.bits(), u64::MAX);
        assert_eq!(snapshot.registered_cpu_count, MAX_CPUS);
        assert_eq!(snapshot.process_count, MAX_CPUS);
        assert_eq!(snapshot.thread_count, 134);
        assert_eq!(snapshot.scheduler.assigned_thread_count, 134);

        for _ in 0..64 {
            let userspace = runtime
                .register_userspace_context(None, "userspace", cpu0)
                .unwrap();
            assert_eq!(userspace.thread.raw() & KERNEL_THREAD_ID_BASE, 0);
        }
        assert_eq!(runtime.snapshot().process_count, 128);
        assert_eq!(runtime.snapshot().thread_count, 198);
        assert_eq!(runtime.snapshot().scheduler.assigned_thread_count, 198);
    }
}
