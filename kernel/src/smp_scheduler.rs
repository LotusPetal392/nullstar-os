//! Application-processor kernel scheduling for the live SMP workload.
//!
//! Secondary CPUs own independent architecture contexts and stacks, while the
//! queue/quantum semantics come from the same architecture-neutral round-robin
//! policy used by the execution foundation. This removes the duplicate AP-only
//! scheduling algorithm and gives later affinity/migration work one canonical
//! run-queue implementation to extend.

use alloc::{boxed::Box, vec, vec::Vec};
use core::{
    arch::global_asm,
    mem::{align_of, size_of},
    sync::atomic::{AtomicU64, Ordering},
};

use spin::Mutex;
use x86_64::instructions::{
    hlt,
    segmentation::{CS, SS, Segment},
};

use crate::{
    arch::x86_64::smp_runtime,
    preemption,
    process_model::ThreadId,
    scheduling::{self, RoundRobin},
};

const MAX_CPUS: usize = 64;
const AP_KERNEL_STACK_SIZE: usize = 64 * 1024;
const AP_KERNEL_STACK_WORDS: usize = AP_KERNEL_STACK_SIZE / size_of::<u128>();
const INITIAL_RFLAGS: u64 = 0x202;
// Probe-only identities live in a reserved high range until live kernel threads
// are backed directly by ProcessTable thread identities.
const AP_PROBE_THREAD_ID_BASE: u64 = 1_u64 << 63;

static AP_SCHEDULERS: [Mutex<ApScheduler>; MAX_CPUS] =
    [const { Mutex::new(ApScheduler::new()) }; MAX_CPUS];
static PROBE_A_HEARTBEATS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static PROBE_B_HEARTBEATS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

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
    Policy(scheduling::Error),
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
    policy: RoundRobin,
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
            policy: RoundRobin::with_default_quantum(),
        }
    }

    fn initialize(&mut self, cpu_index: usize) -> Result<(), InitError> {
        if self.running || !self.tasks.is_empty() {
            return Err(InitError::AlreadyInitialized);
        }

        let probe_a = probe_thread_id(cpu_index, 0);
        let probe_b = probe_thread_id(cpu_index, 1);
        self.tasks.push(ApTask::bootstrap());
        self.tasks.push(ApTask::kernel(probe_a, ap_probe_a)?);
        self.tasks.push(ApTask::kernel(probe_b, ap_probe_b)?);
        self.policy.admit(probe_a).map_err(InitError::Policy)?;
        self.policy.admit(probe_b).map_err(InitError::Policy)?;
        self.running = true;
        Ok(())
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn on_timer_interrupt(&mut self, current_stack_pointer: usize) -> usize {
        if !self.running || !self.armed || self.tasks.len() < 2 {
            return current_stack_pointer;
        }

        let current = self.current_task;
        self.tasks[current].stack_pointer = current_stack_pointer;
        self.tasks[current].runtime_ticks = self.tasks[current].runtime_ticks.saturating_add(1);
        self.timer_ticks = self.timer_ticks.saturating_add(1);

        if current == 0 {
            let Some(thread) = self.policy.snapshot().current else {
                return current_stack_pointer;
            };
            return self.switch_to_thread(thread, current_stack_pointer);
        }

        let Some(switch) = self.policy.tick() else {
            return current_stack_pointer;
        };
        self.switch_to_thread(switch.to, current_stack_pointer)
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
        let policy = self.policy.snapshot();
        Snapshot {
            cpu_index,
            running: self.running,
            armed: self.armed,
            task_count: self.tasks.len(),
            current_task: self.current_task,
            timer_ticks: self.timer_ticks,
            context_switches: self.context_switches,
            policy_current_thread: policy.current.map(ThreadId::raw),
            policy_runnable_count: policy.runnable_count,
            policy_preemptions: policy.preemptions,
            probe_a_heartbeats: PROBE_A_HEARTBEATS[cpu_index].load(Ordering::Acquire),
            probe_b_heartbeats: PROBE_B_HEARTBEATS[cpu_index].load(Ordering::Acquire),
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
    let mut scheduler = AP_SCHEDULERS[cpu_index].lock();
    scheduler.initialize(cpu_index)?;
    Ok(scheduler.snapshot(cpu_index))
}

pub fn arm(cpu_index: usize) -> Result<(), InitError> {
    if cpu_index == 0 || cpu_index >= MAX_CPUS {
        return Err(InitError::InvalidCpu);
    }
    let mut scheduler = AP_SCHEDULERS[cpu_index].lock();
    if !scheduler.running {
        return Err(InitError::InvalidCpu);
    }
    scheduler.arm();
    Ok(())
}

pub fn on_timer_interrupt(cpu_index: usize, current_stack_pointer: usize) -> usize {
    if cpu_index == 0 || cpu_index >= MAX_CPUS || preemption::is_disabled() {
        return current_stack_pointer;
    }
    AP_SCHEDULERS[cpu_index]
        .lock()
        .on_timer_interrupt(current_stack_pointer)
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
    fn architecture_neutral_policy_rotates_live_ap_threads() {
        let mut policy = RoundRobin::with_default_quantum();
        let a = probe_thread_id(1, 0);
        let b = probe_thread_id(1, 1);
        policy.admit(a).unwrap();
        policy.admit(b).unwrap();
        assert_eq!(policy.snapshot().current, Some(a));
        for _ in 0..scheduling::DEFAULT_QUANTUM_TICKS - 1 {
            assert_eq!(policy.tick(), None);
        }
        let switch = policy.tick().expect("quantum should rotate the queue");
        assert_eq!(switch.from, Some(a));
        assert_eq!(switch.to, b);
    }
}
