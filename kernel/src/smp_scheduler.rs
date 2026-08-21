//! Application-processor kernel scheduling for the first live SMP workload.
//!
//! The bootstrap processor still owns the existing userspace-capable scheduler.
//! Each online AP gets an independent kernel-only round-robin lane here. This
//! lets secondary CPUs perform real context switches without sharing the BSP's
//! single `current_task`, address-space, or privilege-stack state prematurely.

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

use crate::{arch::x86_64::smp_runtime, preemption};

const MAX_CPUS: usize = 64;
const AP_KERNEL_STACK_SIZE: usize = 64 * 1024;
const AP_KERNEL_STACK_WORDS: usize = AP_KERNEL_STACK_SIZE / size_of::<u128>();
const DEFAULT_QUANTUM_TICKS: u64 = 5;
const INITIAL_RFLAGS: u64 = 0x202;

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
    pub probe_a_heartbeats: u64,
    pub probe_b_heartbeats: u64,
}

struct ApTask {
    stack_pointer: usize,
    runnable: bool,
    _stack: Option<Box<[u128]>>,
    runtime_ticks: u64,
}

impl ApTask {
    const fn bootstrap() -> Self {
        Self {
            stack_pointer: 0,
            runnable: true,
            _stack: None,
            runtime_ticks: 0,
        }
    }

    fn kernel(entry: ThreadEntry) -> Result<Self, InitError> {
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
            stack_pointer,
            runnable: true,
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
    ticks_in_quantum: u64,
    timer_ticks: u64,
    context_switches: u64,
}

impl ApScheduler {
    const fn new() -> Self {
        Self {
            tasks: Vec::new(),
            current_task: 0,
            running: false,
            armed: false,
            ticks_in_quantum: 0,
            timer_ticks: 0,
            context_switches: 0,
        }
    }

    fn initialize(&mut self) -> Result<(), InitError> {
        if self.running || !self.tasks.is_empty() {
            return Err(InitError::AlreadyInitialized);
        }
        self.tasks.push(ApTask::bootstrap());
        self.tasks.push(ApTask::kernel(ap_probe_a)?);
        self.tasks.push(ApTask::kernel(ap_probe_b)?);
        self.running = true;
        Ok(())
    }

    fn arm(&mut self) {
        self.armed = true;
        self.ticks_in_quantum = 0;
    }

    fn on_timer_interrupt(&mut self, current_stack_pointer: usize) -> usize {
        if !self.running || !self.armed || self.tasks.len() < 2 {
            return current_stack_pointer;
        }

        let current = self.current_task;
        self.tasks[current].stack_pointer = current_stack_pointer;
        self.tasks[current].runtime_ticks = self.tasks[current].runtime_ticks.saturating_add(1);
        self.timer_ticks = self.timer_ticks.saturating_add(1);
        self.ticks_in_quantum = self.ticks_in_quantum.saturating_add(1);

        if self.ticks_in_quantum < DEFAULT_QUANTUM_TICKS {
            return current_stack_pointer;
        }
        self.ticks_in_quantum = 0;

        let Some(next) = self.next_runnable_after(current) else {
            return current_stack_pointer;
        };
        if next == current {
            return current_stack_pointer;
        }

        // Once the AP has entered its first scheduled kernel thread, retire the
        // bootstrap rendezvous loop. The lane then alternates only among real
        // schedulable tasks.
        if current == 0 {
            self.tasks[0].runnable = false;
        }

        self.current_task = next;
        self.context_switches = self.context_switches.saturating_add(1);
        self.tasks[next].stack_pointer
    }

    fn next_runnable_after(&self, current: usize) -> Option<usize> {
        if self.tasks.is_empty() {
            return None;
        }
        for offset in 1..=self.tasks.len() {
            let index = (current + offset) % self.tasks.len();
            let task = &self.tasks[index];
            if task.runnable && task.stack_pointer != 0 {
                return Some(index);
            }
        }
        None
    }

    fn snapshot(&self, cpu_index: usize) -> Snapshot {
        Snapshot {
            cpu_index,
            running: self.running,
            armed: self.armed,
            task_count: self.tasks.len(),
            current_task: self.current_task,
            timer_ticks: self.timer_ticks,
            context_switches: self.context_switches,
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
    scheduler.initialize()?;
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
