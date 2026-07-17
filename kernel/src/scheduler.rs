use alloc::{boxed::Box, vec, vec::Vec};
use core::{
    arch::global_asm,
    mem::{align_of, size_of},
    sync::atomic::{AtomicU64, Ordering},
};

use spin::Mutex;
use x86_64::{
    VirtAddr,
    instructions::{
        hlt, interrupts as cpu_interrupts,
        segmentation::{CS, SS, Segment},
    },
};

const KERNEL_STACK_SIZE: usize = 64 * 1024;
const KERNEL_STACK_WORDS: usize = KERNEL_STACK_SIZE / size_of::<u128>();
const DEFAULT_QUANTUM_TICKS: u64 = 5;
pub const MAX_SNAPSHOT_TASKS: usize = 8;
const INITIAL_RFLAGS: u64 = 0x202;

static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());
static PROBE_A_HEARTBEATS: AtomicU64 = AtomicU64::new(0);
static PROBE_B_HEARTBEATS: AtomicU64 = AtomicU64::new(0);

global_asm!(
    r#"
    .section .text
    .p2align 4
    .global galactic_timer_interrupt_entry
    .type galactic_timer_interrupt_entry,@function
galactic_timer_interrupt_entry:
    cld
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rdi, rsp
    and rsp, -16
    call galactic_timer_interrupt_dispatch
    mov rsp, rax

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    iretq
.size galactic_timer_interrupt_entry, .-galactic_timer_interrupt_entry

    .p2align 4
    .global galactic_thread_entry_trampoline
    .type galactic_thread_entry_trampoline,@function
galactic_thread_entry_trampoline:
    call r12
    ud2
.size galactic_thread_entry_trampoline, .-galactic_thread_entry_trampoline
"#,
);

unsafe extern "C" {
    fn galactic_timer_interrupt_entry();
    fn galactic_thread_entry_trampoline();
}

pub type ThreadEntry = extern "C" fn() -> !;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    AlreadyInitialized,
    StackLayoutInvalid,
}

impl InitError {
    pub const fn description(self) -> &'static str {
        match self {
            Self::AlreadyInitialized => "scheduler is already initialized",
            Self::StackLayoutInvalid => "kernel-thread stack layout is invalid",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TaskSnapshot {
    pub id: u64,
    pub name: &'static str,
    pub stack_bytes: usize,
    pub scheduled_count: u64,
    pub runtime_ticks: u64,
}

impl TaskSnapshot {
    const EMPTY: Self = Self {
        id: 0,
        name: "",
        stack_bytes: 0,
        scheduled_count: 0,
        runtime_ticks: 0,
    };
}

#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    pub running: bool,
    pub task_count: usize,
    pub current_task_id: u64,
    pub current_task_name: &'static str,
    pub quantum_ticks: u64,
    pub context_switches: u64,
    pub preemptions: u64,
    pub probe_a_heartbeats: u64,
    pub probe_b_heartbeats: u64,
    pub truncated: bool,
    tasks: [TaskSnapshot; MAX_SNAPSHOT_TASKS],
    recorded_task_count: usize,
}

impl Snapshot {
    pub fn tasks(&self) -> &[TaskSnapshot] {
        &self.tasks[..self.recorded_task_count]
    }
}

struct Task {
    id: u64,
    name: &'static str,
    stack_pointer: usize,
    stack: Option<Box<[u128]>>,
    scheduled_count: u64,
    runtime_ticks: u64,
}

impl Task {
    fn bootstrap(id: u64) -> Self {
        Self {
            id,
            name: "bootstrap-shell",
            stack_pointer: 0,
            stack: None,
            scheduled_count: 1,
            runtime_ticks: 0,
        }
    }

    fn kernel_thread(id: u64, name: &'static str, entry: ThreadEntry) -> Result<Self, InitError> {
        let mut stack = vec![0_u128; KERNEL_STACK_WORDS].into_boxed_slice();
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

        if stack_start % align_of::<u128>() != 0 || stack_end % 16 != 0 || stack_pointer % 16 != 0 {
            return Err(InitError::StackLayoutInvalid);
        }

        let context = SavedContext {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: entry as usize as u64,
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
            rip: thread_entry_trampoline_address().as_u64(),
            cs: u64::from(CS::get_reg().0),
            rflags: INITIAL_RFLAGS,
            stack_pointer: stack_end as u64,
            stack_segment: u64::from(SS::get_reg().0),
        };

        unsafe { (stack_pointer as *mut SavedContext).write(context) };

        Ok(Self {
            id,
            name,
            stack_pointer,
            stack: Some(stack),
            scheduled_count: 0,
            runtime_ticks: 0,
        })
    }

    fn stack_bytes(&self) -> usize {
        self.stack
            .as_ref()
            .map(|stack| stack.len() * size_of::<u128>())
            .unwrap_or(0)
    }

    fn snapshot(&self) -> TaskSnapshot {
        TaskSnapshot {
            id: self.id,
            name: self.name,
            stack_bytes: self.stack_bytes(),
            scheduled_count: self.scheduled_count,
            runtime_ticks: self.runtime_ticks,
        }
    }
}

struct Scheduler {
    tasks: Vec<Task>,
    current_task: usize,
    next_task_id: u64,
    running: bool,
    quantum_ticks: u64,
    ticks_in_quantum: u64,
    context_switches: u64,
    preemptions: u64,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            tasks: Vec::new(),
            current_task: 0,
            next_task_id: 0,
            running: false,
            quantum_ticks: DEFAULT_QUANTUM_TICKS,
            ticks_in_quantum: 0,
            context_switches: 0,
            preemptions: 0,
        }
    }

    fn initialize(&mut self) -> Result<(), InitError> {
        if self.running || !self.tasks.is_empty() {
            return Err(InitError::AlreadyInitialized);
        }

        let bootstrap_id = self.allocate_task_id();
        self.tasks.push(Task::bootstrap(bootstrap_id));
        self.spawn("scheduler-probe-a", scheduler_probe_a)?;
        self.spawn("scheduler-probe-b", scheduler_probe_b)?;
        self.running = true;
        Ok(())
    }

    fn spawn(&mut self, name: &'static str, entry: ThreadEntry) -> Result<u64, InitError> {
        let id = self.allocate_task_id();
        self.tasks.push(Task::kernel_thread(id, name, entry)?);
        Ok(id)
    }

    fn allocate_task_id(&mut self) -> u64 {
        let id = self.next_task_id;
        self.next_task_id = self.next_task_id.saturating_add(1);
        id
    }

    fn schedule(&mut self, current_stack_pointer: usize) -> usize {
        if !self.running || self.tasks.is_empty() {
            return current_stack_pointer;
        }

        let current = self.current_task;
        self.tasks[current].stack_pointer = current_stack_pointer;
        self.tasks[current].runtime_ticks = self.tasks[current].runtime_ticks.saturating_add(1);

        if self.tasks.len() < 2 {
            return current_stack_pointer;
        }

        self.ticks_in_quantum = self.ticks_in_quantum.saturating_add(1);
        if self.ticks_in_quantum < self.quantum_ticks {
            return current_stack_pointer;
        }
        self.ticks_in_quantum = 0;

        let next = (current + 1) % self.tasks.len();
        let next_stack_pointer = self.tasks[next].stack_pointer;
        if next_stack_pointer == 0 {
            return current_stack_pointer;
        }

        self.current_task = next;
        self.context_switches = self.context_switches.saturating_add(1);
        self.preemptions = self.preemptions.saturating_add(1);
        self.tasks[next].scheduled_count = self.tasks[next].scheduled_count.saturating_add(1);
        next_stack_pointer
    }

    fn snapshot(&self) -> Snapshot {
        let mut tasks = [TaskSnapshot::EMPTY; MAX_SNAPSHOT_TASKS];
        let recorded_task_count = self.tasks.len().min(MAX_SNAPSHOT_TASKS);
        for (destination, task) in tasks.iter_mut().zip(self.tasks.iter()) {
            *destination = task.snapshot();
        }

        let current_task = self.tasks.get(self.current_task);
        Snapshot {
            running: self.running,
            task_count: self.tasks.len(),
            current_task_id: current_task.map(|task| task.id).unwrap_or(0),
            current_task_name: current_task.map(|task| task.name).unwrap_or("none"),
            quantum_ticks: self.quantum_ticks,
            context_switches: self.context_switches,
            preemptions: self.preemptions,
            probe_a_heartbeats: PROBE_A_HEARTBEATS.load(Ordering::Relaxed),
            probe_b_heartbeats: PROBE_B_HEARTBEATS.load(Ordering::Relaxed),
            truncated: self.tasks.len() > recorded_task_count,
            tasks,
            recorded_task_count,
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

pub fn init() -> Result<Snapshot, InitError> {
    PROBE_A_HEARTBEATS.store(0, Ordering::Relaxed);
    PROBE_B_HEARTBEATS.store(0, Ordering::Relaxed);

    cpu_interrupts::without_interrupts(|| {
        let mut scheduler = SCHEDULER.lock();
        scheduler.initialize()?;
        Ok(scheduler.snapshot())
    })
}

pub fn wait_for_self_test() -> Snapshot {
    loop {
        let snapshot = snapshot();
        if snapshot.context_switches >= 3
            && snapshot.probe_a_heartbeats > 0
            && snapshot.probe_b_heartbeats > 0
        {
            return snapshot;
        }
        hlt();
    }
}

pub fn snapshot() -> Snapshot {
    cpu_interrupts::without_interrupts(|| SCHEDULER.lock().snapshot())
}

pub fn on_timer_interrupt(current_stack_pointer: usize) -> usize {
    SCHEDULER.lock().schedule(current_stack_pointer)
}

pub fn timer_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(galactic_timer_interrupt_entry as usize as u64)
}

fn thread_entry_trampoline_address() -> VirtAddr {
    VirtAddr::new(galactic_thread_entry_trampoline as usize as u64)
}

extern "C" fn scheduler_probe_a() -> ! {
    loop {
        PROBE_A_HEARTBEATS.fetch_add(1, Ordering::Relaxed);
        hlt();
    }
}

extern "C" fn scheduler_probe_b() -> ! {
    loop {
        PROBE_B_HEARTBEATS.fetch_add(1, Ordering::Relaxed);
        hlt();
    }
}
