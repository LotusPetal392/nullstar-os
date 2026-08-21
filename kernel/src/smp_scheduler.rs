//! Application-processor kernel scheduling for the live SMP workload.
//!
//! Secondary CPUs own independent architecture contexts and stacks, while a
//! shared architecture-neutral SMP policy owns CPU placement, affinity, and
//! per-CPU round-robin queues. Quiescent and live migration both transfer the
//! owned saved context only at scheduler-safe boundaries.

use alloc::{boxed::Box, vec, vec::Vec};
use core::{
    arch::global_asm,
    mem::{align_of, size_of},
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

use spin::Mutex;
use x86_64::instructions::{
    hlt,
    interrupts as cpu_interrupts,
    segmentation::{CS, SS, Segment},
};

use crate::{
    arch::x86_64::smp_runtime,
    preemption,
    process_model::ThreadId,
    scheduling::{self, CpuId, CpuMask, SmpRoundRobin},
    serial_println,
};

const MAX_CPUS: usize = 64;
const AP_KERNEL_STACK_SIZE: usize = 64 * 1024;
const AP_KERNEL_STACK_WORDS: usize = AP_KERNEL_STACK_SIZE / size_of::<u128>();
const INITIAL_RFLAGS: u64 = 0x202;
const NO_MIGRATION_CPU: u8 = u8::MAX;
const LIVE_MIGRATION_IDLE: u8 = 0;
const LIVE_MIGRATION_PENDING: u8 = 1;
const LIVE_MIGRATION_TRANSFERRED: u8 = 2;
const LIVE_MIGRATION_VERIFIED: u8 = 3;
// Probe-only identities live in a reserved high range until live kernel threads
// are backed directly by ProcessTable thread identities.
const AP_PROBE_THREAD_ID_BASE: u64 = 1_u64 << 63;

static AP_SCHEDULERS: [Mutex<ApScheduler>; MAX_CPUS] =
    [const { Mutex::new(ApScheduler::new()) }; MAX_CPUS];
static SMP_POLICY: Mutex<Option<SmpRoundRobin>> = Mutex::new(None);
static PROBE_A_HEARTBEATS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static PROBE_B_HEARTBEATS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static MIGRATION_PROBE_HEARTBEATS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static MIGRATION_THREAD_ID: AtomicU64 = AtomicU64::new(0);
static MIGRATION_FROM_CPU: AtomicU8 = AtomicU8::new(NO_MIGRATION_CPU);
static MIGRATION_TO_CPU: AtomicU8 = AtomicU8::new(NO_MIGRATION_CPU);
static MIGRATION_VERIFIED: AtomicBool = AtomicBool::new(false);
static LIVE_MIGRATION_THREAD_ID: AtomicU64 = AtomicU64::new(0);
static LIVE_MIGRATION_FROM_CPU: AtomicU8 = AtomicU8::new(NO_MIGRATION_CPU);
static LIVE_MIGRATION_TO_CPU: AtomicU8 = AtomicU8::new(NO_MIGRATION_CPU);
static LIVE_MIGRATION_STATE: AtomicU8 = AtomicU8::new(LIVE_MIGRATION_IDLE);
static LIVE_MIGRATION_RESCHEDULE_BASELINE: AtomicU64 = AtomicU64::new(0);

global_asm!(
    r#"
    .section .text
    .p2align 4
    .global nullstar_ap_thread_entry_trampoline
    .type nullstar_ap_thread_entry_trampoline,@function
nullstar_ap_thread_entry_trampoline:
    call r12
    ud2
.size nullstar_ap_thread_entry_trampoline, .-nullstar_ap_thread_entry_trampoline
"#,
);

unsafe extern "C" {
    fn nullstar_ap_thread_entry_trampoline();
}

type ThreadEntry = extern "C" fn() -> !;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitError {
    InvalidCpu,
    AlreadyInitialized,
    StackLayoutInvalid,
    Policy(scheduling::SmpError),
    Migration(MigrationError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MigrationError {
    InvalidCpu,
    OfflineDestination,
    CpuArmed,
    CpuNotArmed,
    MigrationPending,
    PolicyUnavailable,
    Policy(scheduling::SmpError),
    ContextNotFound,
    DestinationConflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigrationSnapshot {
    pub thread_id: ThreadId,
    pub from_cpu: usize,
    pub to_cpu: usize,
    pub source_task_count: usize,
    pub destination_task_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LiveMigrationRequest {
    pub thread_id: ThreadId,
    pub from_cpu: usize,
    pub to_cpu: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    pub cpu_index: usize,
    pub running: bool,
    pub armed: bool,
    pub task_count: usize,
    pub current_task: usize,
    pub timer_ticks: u64,
    pub context_switches: u64,
    pub policy_current_thread: Option<u64>,
    pub policy_runnable_count: usize,
    pub policy_preemptions: u64,
    pub probe_a_heartbeats: u64,
    pub probe_b_heartbeats: u64,
    pub migration_probe_heartbeats: u64,
    pub live_migrations_in: u64,
    pub live_migrations_out: u64,
}

struct ApTask {
    thread_id: Option<ThreadId>,
    stack_pointer: usize,
    _stack: Option<Box<[u128]>>,
    runtime_ticks: u64,
}

impl ApTask {
    const fn bootstrap() -> Self {
        Self {
            thread_id: None,
            stack_pointer: 0,
            _stack: None,
            runtime_ticks: 0,
        }
    }

    fn kernel(thread_id: ThreadId, entry: ThreadEntry) -> Result<Self, InitError> {
        let mut stack = vec![0_u128; AP_KERNEL_STACK_WORDS].into_boxed_slice();
        let stack_start = stack.as_mut_ptr() as usize;
        let stack_bytes = stack
            .len()
            .checked_mul(size_of::<u128>())
            .ok_or(InitError::StackLayoutInvalid)?;
        let stack_end = stack_start
            .checked_add(stack_bytes)
            .ok_or(InitError::StackLayoutInvalid)?;
        let stack_pointer = stack_end
            .checked_sub(size_of::<SavedContext>())
            .ok_or(InitError::StackLayoutInvalid)?;

        if !stack_start.is_multiple_of(align_of::<u128>())
            || !stack_end.is_multiple_of(16)
            || !stack_pointer.is_multiple_of(16)
        {
            return Err(InitError::StackLayoutInvalid);
        }

        let context = SavedContext {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: entry as *const () as usize as u64,
            r11: 0,
            r10: 0,
            r9: 0,
            r8: 0,
            rbp: 0,
            rdi: 0,
            rsi: 0,
            rdx: 0,
            rcx: 0,
            rbx: 0,
            rax: 0,
            rip: thread_entry_trampoline_address(),
            cs: u64::from(CS::get_reg().0),
            rflags: INITIAL_RFLAGS,
            stack_pointer: stack_end as u64,
            stack_segment: u64::from(SS::get_reg().0),
        };
        unsafe { (stack_pointer as *mut SavedContext).write(context) };

        Ok(Self {
            thread_id: Some(thread_id),
            stack_pointer,
            _stack: Some(stack),
            runtime_ticks: 0,
        })
    }
}

struct ApScheduler {
    tasks: Vec<ApTask>,
    current_task: usize,
    running: bool,
    armed: bool,
    timer_ticks: u64,
    context_switches: u64,
    policy_preemptions: u64,
    live_migrations_in: u64,
    live_migrations_out: u64,
}

impl ApScheduler {
    const fn new() -> Self {
        Self {
            tasks: Vec::new(),
            current_task: 0,
            running: false,
            armed: false,
            timer_ticks: 0,
            context_switches: 0,
            policy_preemptions: 0,
            live_migrations_in: 0,
            live_migrations_out: 0,
        }
    }

    fn initialize(&mut self, cpu_index: usize) -> Result<(), InitError> {
        if self.running || !self.tasks.is_empty() {
            return Err(InitError::AlreadyInitialized);
        }
        let cpu = CpuId::from_raw(cpu_index).ok_or(InitError::InvalidCpu)?;
        let probe_a = probe_thread_id(cpu_index, 0);
        let probe_b = probe_thread_id(cpu_index, 1);

        self.tasks.push(ApTask::bootstrap());
        self.tasks.push(ApTask::kernel(probe_a, ap_probe_a)?);
        self.tasks.push(ApTask::kernel(probe_b, ap_probe_b)?);
        if cpu_index == 1 {
            self.tasks.push(ApTask::kernel(
                probe_thread_id(cpu_index, 2),
                ap_migration_probe,
            )?);
        }

        let mut policy_guard = SMP_POLICY.lock();
        if policy_guard.is_none() {
            *policy_guard = Some(
                SmpRoundRobin::new(MAX_CPUS, scheduling::DEFAULT_QUANTUM_TICKS)
                    .map_err(InitError::Policy)?,
            );
        }
        let policy = policy_guard
            .as_mut()
            .expect("SMP policy must exist after initialization");
        policy
            .admit(probe_a, CpuMask::single(cpu))
            .map_err(InitError::Policy)?;
        policy
            .admit(probe_b, CpuMask::single(cpu))
            .map_err(InitError::Policy)?;
        if cpu_index == 1 {
            policy
                .admit(probe_thread_id(cpu_index, 2), CpuMask::single(cpu))
                .map_err(InitError::Policy)?;
        }

        self.running = true;
        Ok(())
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn on_timer_interrupt(&mut self, cpu_index: usize, current_stack_pointer: usize) -> usize {
        if !self.running || !self.armed || self.tasks.len() < 2 {
            return current_stack_pointer;
        }

        let current = self.current_task;
        self.tasks[current].stack_pointer = current_stack_pointer;
        self.tasks[current].runtime_ticks = self.tasks[current].runtime_ticks.saturating_add(1);
        self.timer_ticks = self.timer_ticks.saturating_add(1);

        if let Some(next_stack_pointer) =
            try_live_migrate_current(cpu_index, self, current_stack_pointer)
        {
            return next_stack_pointer;
        }

        let Some(cpu) = CpuId::from_raw(cpu_index) else {
            return current_stack_pointer;
        };
        let selected = {
            let mut policy_guard = SMP_POLICY.lock();
            let Some(policy) = policy_guard.as_mut() else {
                return current_stack_pointer;
            };
            if current == 0 {
                policy
                    .cpu_snapshot(cpu)
                    .ok()
                    .and_then(|snapshot| snapshot.current)
            } else {
                match policy.tick(cpu) {
                    Ok(Some(switch)) => {
                        self.policy_preemptions = self.policy_preemptions.saturating_add(1);
                        Some(switch.to)
                    }
                    Ok(None) | Err(_) => None,
                }
            }
        };
        let Some(thread) = selected else {
            return current_stack_pointer;
        };
        self.switch_to_thread(thread, current_stack_pointer)
    }

    fn switch_to_thread(&mut self, thread: ThreadId, current_stack_pointer: usize) -> usize {
        let Some(next) = self
            .tasks
            .iter()
            .position(|task| task.thread_id == Some(thread))
        else {
            return current_stack_pointer;
        };
        if next == self.current_task || self.tasks[next].stack_pointer == 0 {
            return current_stack_pointer;
        }

        self.current_task = next;
        self.context_switches = self.context_switches.saturating_add(1);
        self.tasks[next].stack_pointer
    }

    fn snapshot(&self, cpu_index: usize) -> Snapshot {
        let policy_snapshot = CpuId::from_raw(cpu_index).and_then(|cpu| {
            SMP_POLICY
                .lock()
                .as_ref()
                .and_then(|policy| policy.cpu_snapshot(cpu).ok())
        });
        let migration_heartbeats = MIGRATION_PROBE_HEARTBEATS[cpu_index].load(Ordering::Acquire);
        let migration_destination = MIGRATION_TO_CPU.load(Ordering::Acquire);
        let migration_pending_here = migration_destination != NO_MIGRATION_CPU
            && usize::from(migration_destination) == cpu_index;
        let probe_b_heartbeats = if migration_pending_here && migration_heartbeats == 0 {
            0
        } else {
            PROBE_B_HEARTBEATS[cpu_index].load(Ordering::Acquire)
        };

        if migration_pending_here
            && migration_heartbeats > 0
            && MIGRATION_VERIFIED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            serial_println!(
                "application processor kernel migration verified: thread={}, from_cpu={}, to_cpu={}, heartbeats={}",
                MIGRATION_THREAD_ID.load(Ordering::Acquire),
                MIGRATION_FROM_CPU.load(Ordering::Acquire),
                migration_destination,
                migration_heartbeats
            );
        }

        Snapshot {
            cpu_index,
            running: self.running,
            armed: self.armed,
            task_count: self.tasks.len(),
            current_task: self.current_task,
            timer_ticks: self.timer_ticks,
            context_switches: self.context_switches,
            policy_current_thread: policy_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.current)
                .map(ThreadId::raw),
            policy_runnable_count: policy_snapshot
                .as_ref()
                .map(|snapshot| snapshot.runnable_count)
                .unwrap_or(0),
            policy_preemptions: self.policy_preemptions,
            probe_a_heartbeats: PROBE_A_HEARTBEATS[cpu_index].load(Ordering::Acquire),
            probe_b_heartbeats,
            migration_probe_heartbeats: migration_heartbeats,
            live_migrations_in: self.live_migrations_in,
            live_migrations_out: self.live_migrations_out,
        }
    }
}

#[repr(C)]
struct SavedContext {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    stack_pointer: u64,
    stack_segment: u64,
}

pub fn init_application_processor(cpu_index: usize) -> Result<Snapshot, InitError> {
    if cpu_index == 0 || cpu_index >= MAX_CPUS {
        return Err(InitError::InvalidCpu);
    }
    PROBE_A_HEARTBEATS[cpu_index].store(0, Ordering::Release);
    PROBE_B_HEARTBEATS[cpu_index].store(0, Ordering::Release);
    MIGRATION_PROBE_HEARTBEATS[cpu_index].store(0, Ordering::Release);
    let mut scheduler = AP_SCHEDULERS[cpu_index].lock();
    scheduler.initialize(cpu_index)?;
    Ok(scheduler.snapshot(cpu_index))
}

pub fn arm(cpu_index: usize) -> Result<(), InitError> {
    if cpu_index == 0 || cpu_index >= MAX_CPUS {
        return Err(InitError::InvalidCpu);
    }

    // A three-CPU acceptance boot gives us two AP scheduling lanes. Move a
    // never-run context from CPU 1 to CPU 2 before either lane is armed. This
    // proves affinity and ownership transfer without racing a running context.
    if cpu_index == 1
        && smp_runtime::is_online(2)
        && MIGRATION_TO_CPU.load(Ordering::Acquire) == NO_MIGRATION_CPU
    {
        migrate_unstarted(probe_thread_id(1, 2), 2).map_err(InitError::Migration)?;
    }

    let mut scheduler = AP_SCHEDULERS[cpu_index].lock();
    if !scheduler.running {
        return Err(InitError::InvalidCpu);
    }
    scheduler.arm();
    Ok(())
}

pub fn migrate_unstarted(
    thread_id: ThreadId,
    destination_cpu: usize,
) -> Result<MigrationSnapshot, MigrationError> {
    let destination = CpuId::from_raw(destination_cpu).ok_or(MigrationError::InvalidCpu)?;
    if destination_cpu == 0 {
        return Err(MigrationError::InvalidCpu);
    }
    if !smp_runtime::is_online(destination_cpu) {
        return Err(MigrationError::OfflineDestination);
    }

    let source_cpu = {
        let policy_guard = SMP_POLICY.lock();
        let policy = policy_guard
            .as_ref()
            .ok_or(MigrationError::PolicyUnavailable)?;
        policy
            .placement(thread_id)
            .map_err(MigrationError::Policy)?
            .cpu
            .raw()
    };
    if source_cpu == 0 || source_cpu >= MAX_CPUS || source_cpu == destination_cpu {
        return Err(MigrationError::InvalidCpu);
    }

    let snapshot = if source_cpu < destination_cpu {
        let mut source = AP_SCHEDULERS[source_cpu].lock();
        let mut target = AP_SCHEDULERS[destination_cpu].lock();
        migrate_locked(&mut source, &mut target, source_cpu, destination, thread_id)?
    } else {
        let mut target = AP_SCHEDULERS[destination_cpu].lock();
        let mut source = AP_SCHEDULERS[source_cpu].lock();
        migrate_locked(&mut source, &mut target, source_cpu, destination, thread_id)?
    };

    MIGRATION_THREAD_ID.store(thread_id.raw(), Ordering::Release);
    MIGRATION_FROM_CPU.store(source_cpu as u8, Ordering::Release);
    MIGRATION_TO_CPU.store(destination_cpu as u8, Ordering::Release);
    MIGRATION_VERIFIED.store(false, Ordering::Release);
    serial_println!(
        "application processor kernel migration prepared: thread={}, from_cpu={}, to_cpu={}, source_tasks={}, destination_tasks={}",
        thread_id.raw(),
        source_cpu,
        destination_cpu,
        snapshot.source_task_count,
        snapshot.destination_task_count
    );
    Ok(snapshot)
}

fn migrate_locked(
    source: &mut ApScheduler,
    target: &mut ApScheduler,
    source_cpu: usize,
    destination: CpuId,
    thread_id: ThreadId,
) -> Result<MigrationSnapshot, MigrationError> {
    if !source.running || !target.running {
        return Err(MigrationError::InvalidCpu);
    }
    if source.armed || target.armed || source.current_task != 0 || target.current_task != 0 {
        return Err(MigrationError::CpuArmed);
    }
    let source_index = source
        .tasks
        .iter()
        .position(|task| task.thread_id == Some(thread_id))
        .ok_or(MigrationError::ContextNotFound)?;
    if source_index == 0 {
        return Err(MigrationError::ContextNotFound);
    }
    if target
        .tasks
        .iter()
        .any(|task| task.thread_id == Some(thread_id))
    {
        return Err(MigrationError::DestinationConflict);
    }

    {
        let mut policy_guard = SMP_POLICY.lock();
        let policy = policy_guard
            .as_mut()
            .ok_or(MigrationError::PolicyUnavailable)?;
        let placement = policy
            .set_affinity(thread_id, CpuMask::single(destination))
            .map_err(MigrationError::Policy)?;
        if !placement.migrated || placement.cpu != destination {
            return Err(MigrationError::Policy(
                scheduling::SmpError::AffinityViolation,
            ));
        }
    }

    let task = source.tasks.remove(source_index);
    target.tasks.push(task);
    Ok(MigrationSnapshot {
        thread_id,
        from_cpu: source_cpu,
        to_cpu: destination.raw(),
        source_task_count: source.tasks.len(),
        destination_task_count: target.tasks.len(),
    })
}

pub fn migration_probe_thread_id() -> ThreadId {
    probe_thread_id(1, 2)
}

pub fn request_live_migration(
    thread_id: ThreadId,
    destination_cpu: usize,
) -> Result<LiveMigrationRequest, MigrationError> {
    if LIVE_MIGRATION_STATE.load(Ordering::Acquire) == LIVE_MIGRATION_PENDING
        || LIVE_MIGRATION_STATE.load(Ordering::Acquire) == LIVE_MIGRATION_TRANSFERRED
    {
        return Err(MigrationError::MigrationPending);
    }
    let destination = CpuId::from_raw(destination_cpu).ok_or(MigrationError::InvalidCpu)?;
    if destination_cpu == 0 || !smp_runtime::is_online(destination_cpu) {
        return Err(MigrationError::OfflineDestination);
    }

    let source_cpu = {
        let policy_guard = SMP_POLICY.lock();
        let policy = policy_guard
            .as_ref()
            .ok_or(MigrationError::PolicyUnavailable)?;
        policy
            .placement(thread_id)
            .map_err(MigrationError::Policy)?
            .cpu
            .raw()
    };
    if source_cpu == 0 || source_cpu >= MAX_CPUS || source_cpu == destination_cpu {
        return Err(MigrationError::InvalidCpu);
    }

    {
        let source = AP_SCHEDULERS[source_cpu].lock();
        let target = AP_SCHEDULERS[destination_cpu].lock();
        if !source.running || !target.running || !source.armed || !target.armed {
            return Err(MigrationError::CpuNotArmed);
        }
        if !source
            .tasks
            .iter()
            .any(|task| task.thread_id == Some(thread_id))
        {
            return Err(MigrationError::ContextNotFound);
        }
        if target
            .tasks
            .iter()
            .any(|task| task.thread_id == Some(thread_id))
        {
            return Err(MigrationError::DestinationConflict);
        }
    }

    LIVE_MIGRATION_THREAD_ID.store(thread_id.raw(), Ordering::Release);
    LIVE_MIGRATION_FROM_CPU.store(source_cpu as u8, Ordering::Release);
    LIVE_MIGRATION_TO_CPU.store(destination.raw() as u8, Ordering::Release);
    LIVE_MIGRATION_RESCHEDULE_BASELINE
        .store(smp_runtime::reschedule_ipis(source_cpu), Ordering::Release);
    LIVE_MIGRATION_STATE.store(LIVE_MIGRATION_PENDING, Ordering::Release);
    Ok(LiveMigrationRequest {
        thread_id,
        from_cpu: source_cpu,
        to_cpu: destination.raw(),
    })
}

fn try_live_migrate_current(
    source_cpu: usize,
    source: &mut ApScheduler,
    current_stack_pointer: usize,
) -> Option<usize> {
    if LIVE_MIGRATION_STATE.load(Ordering::Acquire) != LIVE_MIGRATION_PENDING
        || usize::from(LIVE_MIGRATION_FROM_CPU.load(Ordering::Acquire)) != source_cpu
        || smp_runtime::reschedule_ipis(source_cpu)
            <= LIVE_MIGRATION_RESCHEDULE_BASELINE.load(Ordering::Acquire)
    {
        return None;
    }
    let thread_id = ThreadId::from_raw(LIVE_MIGRATION_THREAD_ID.load(Ordering::Acquire))?;
    if source.tasks.get(source.current_task)?.thread_id != Some(thread_id) {
        return None;
    }
    let destination_cpu = usize::from(LIVE_MIGRATION_TO_CPU.load(Ordering::Acquire));
    if destination_cpu == 0 || destination_cpu >= MAX_CPUS || destination_cpu == source_cpu {
        return None;
    }
    let destination = CpuId::from_raw(destination_cpu)?;
    let mut target = AP_SCHEDULERS[destination_cpu].try_lock()?;
    if !target.running || !target.armed {
        return None;
    }
    if target
        .tasks
        .iter()
        .any(|task| task.thread_id == Some(thread_id))
    {
        return None;
    }

    let source_index = source.current_task;
    source.tasks[source_index].stack_pointer = current_stack_pointer;

    let replacement = {
        let mut policy_guard = SMP_POLICY.try_lock()?;
        let policy = policy_guard.as_mut()?;
        let source_id = CpuId::from_raw(source_cpu)?;
        let placement = policy
            .set_affinity(thread_id, CpuMask::single(destination))
            .ok()?;
        if !placement.migrated || placement.cpu != destination {
            return None;
        }
        let Some(replacement) = policy
            .cpu_snapshot(source_id)
            .ok()
            .and_then(|snapshot| snapshot.current)
        else {
            let _ = policy.set_affinity(thread_id, CpuMask::single(source_id));
            return None;
        };
        let replacement_ready = source
            .tasks
            .iter()
            .any(|task| task.thread_id == Some(replacement) && task.stack_pointer != 0);
        if !replacement_ready {
            let _ = policy.set_affinity(thread_id, CpuMask::single(source_id));
            return None;
        }
        replacement
    };

    let replacement_index = source
        .tasks
        .iter()
        .position(|task| task.thread_id == Some(replacement))?;
    let replacement_stack_pointer = source.tasks[replacement_index].stack_pointer;

    let task = source.tasks.remove(source_index);
    target.tasks.push(task);
    let replacement_index = source
        .tasks
        .iter()
        .position(|task| task.thread_id == Some(replacement))?;
    source.current_task = replacement_index;
    source.context_switches = source.context_switches.saturating_add(1);
    source.live_migrations_out = source.live_migrations_out.saturating_add(1);
    target.live_migrations_in = target.live_migrations_in.saturating_add(1);
    LIVE_MIGRATION_STATE.store(LIVE_MIGRATION_TRANSFERRED, Ordering::Release);
    Some(replacement_stack_pointer)
}

pub fn on_timer_interrupt(cpu_index: usize, current_stack_pointer: usize) -> usize {
    if cpu_index == 0 || cpu_index >= MAX_CPUS || preemption::is_disabled() {
        return current_stack_pointer;
    }
    AP_SCHEDULERS[cpu_index]
        .lock()
        .on_timer_interrupt(cpu_index, current_stack_pointer)
}

pub fn snapshot(cpu_index: usize) -> Snapshot {
    if cpu_index >= MAX_CPUS {
        return Snapshot::default();
    }
    AP_SCHEDULERS[cpu_index].lock().snapshot(cpu_index)
}

fn probe_thread_id(cpu_index: usize, slot: u8) -> ThreadId {
    let raw = AP_PROBE_THREAD_ID_BASE | ((cpu_index as u64) << 8) | u64::from(slot) + 1;
    ThreadId::from_raw(raw).expect("AP probe thread identity must be nonzero")
}

fn thread_entry_trampoline_address() -> u64 {
    nullstar_ap_thread_entry_trampoline as *const () as usize as u64
}

extern "C" fn ap_probe_a() -> ! {
    let cpu_index = smp_runtime::current_cpu_index().min(MAX_CPUS - 1);
    loop {
        PROBE_A_HEARTBEATS[cpu_index].fetch_add(1, Ordering::Relaxed);
        hlt();
    }
}

extern "C" fn ap_probe_b() -> ! {
    let cpu_index = smp_runtime::current_cpu_index().min(MAX_CPUS - 1);
    loop {
        PROBE_B_HEARTBEATS[cpu_index].fetch_add(1, Ordering::Relaxed);
        hlt();
    }
}

extern "C" fn ap_migration_probe() -> ! {
    loop {
        let cpu_index = smp_runtime::current_cpu_index().min(MAX_CPUS - 1);
        let heartbeats = MIGRATION_PROBE_HEARTBEATS[cpu_index]
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);

        let quiescent_destination = MIGRATION_TO_CPU.load(Ordering::Acquire);
        if quiescent_destination != NO_MIGRATION_CPU
            && usize::from(quiescent_destination) == cpu_index
            && MIGRATION_VERIFIED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            serial_println!(
                "application processor kernel migration verified: thread={}, from_cpu={}, to_cpu={}, heartbeats={}",
                MIGRATION_THREAD_ID.load(Ordering::Acquire),
                MIGRATION_FROM_CPU.load(Ordering::Acquire),
                quiescent_destination,
                heartbeats
            );
        }

        if cpu_index == 2
            && smp_runtime::is_online(1)
            && LIVE_MIGRATION_STATE.load(Ordering::Acquire) == LIVE_MIGRATION_IDLE
        {
            cpu_interrupts::without_interrupts(|| {
                if LIVE_MIGRATION_STATE.load(Ordering::Acquire) != LIVE_MIGRATION_IDLE {
                    return;
                }
                let thread_id = migration_probe_thread_id();
                if let Ok(request) = request_live_migration(thread_id, 1) {
                    if smp_runtime::send_fixed_ipi(
                        request.from_cpu,
                        crate::interrupts::RESCHEDULE_VECTOR,
                    )
                    .is_ok()
                    {
                        serial_println!(
                            "application processor live migration requested: thread={}, from_cpu={}, to_cpu={}",
                            request.thread_id.raw(),
                            request.from_cpu,
                            request.to_cpu
                        );
                    } else {
                        LIVE_MIGRATION_STATE.store(LIVE_MIGRATION_IDLE, Ordering::Release);
                    }
                }
            });
        }

        if LIVE_MIGRATION_STATE.load(Ordering::Acquire) == LIVE_MIGRATION_TRANSFERRED
            && LIVE_MIGRATION_TO_CPU.load(Ordering::Acquire) != NO_MIGRATION_CPU
            && usize::from(LIVE_MIGRATION_TO_CPU.load(Ordering::Acquire)) == cpu_index
            && LIVE_MIGRATION_STATE
                .compare_exchange(
                    LIVE_MIGRATION_TRANSFERRED,
                    LIVE_MIGRATION_VERIFIED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        {
            serial_println!(
                "application processor live migration verified: thread={}, from_cpu={}, to_cpu={}, destination_heartbeats={}",
                LIVE_MIGRATION_THREAD_ID.load(Ordering::Acquire),
                LIVE_MIGRATION_FROM_CPU.load(Ordering::Acquire),
                LIVE_MIGRATION_TO_CPU.load(Ordering::Acquire),
                MIGRATION_PROBE_HEARTBEATS[cpu_index].load(Ordering::Acquire)
            );
        }
        hlt();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_thread_ids_are_cpu_local_and_nonzero() {
        let cpu1_a = probe_thread_id(1, 0);
        let cpu1_b = probe_thread_id(1, 1);
        let cpu2_a = probe_thread_id(2, 0);
        assert_ne!(cpu1_a, cpu1_b);
        assert_ne!(cpu1_a, cpu2_a);
        assert_ne!(cpu1_a.raw(), 0);
    }

    #[test]
    fn smp_policy_honors_pinned_placement_and_affinity_migration() {
        let mut policy = SmpRoundRobin::new(3, scheduling::DEFAULT_QUANTUM_TICKS).unwrap();
        let cpu1 = CpuId::from_raw(1).unwrap();
        let cpu2 = CpuId::from_raw(2).unwrap();
        let thread = probe_thread_id(1, 2);
        let placement = policy.admit(thread, CpuMask::single(cpu1)).unwrap();
        assert_eq!(placement.cpu, cpu1);
        let migrated = policy.set_affinity(thread, CpuMask::single(cpu2)).unwrap();
        assert!(migrated.migrated);
        assert_eq!(migrated.cpu, cpu2);
        assert_eq!(policy.placement(thread).unwrap().cpu, cpu2);
    }

    #[test]
    fn live_migration_state_codes_are_monotonic() {
        assert!(LIVE_MIGRATION_IDLE < LIVE_MIGRATION_PENDING);
        assert!(LIVE_MIGRATION_PENDING < LIVE_MIGRATION_TRANSFERRED);
        assert!(LIVE_MIGRATION_TRANSFERRED < LIVE_MIGRATION_VERIFIED);
    }
}
