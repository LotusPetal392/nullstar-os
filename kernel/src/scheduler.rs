use alloc::{boxed::Box, vec, vec::Vec};
use core::{
    arch::global_asm,
    fmt,
    mem::{align_of, size_of},
    sync::atomic::{AtomicU64, Ordering},
};

use spin::Mutex;
use x86_64::{
    PhysAddr, VirtAddr,
    instructions::{
        hlt, interrupts as cpu_interrupts,
        segmentation::{CS, SS, Segment},
    },
    registers::control::{Cr3, Cr3Flags},
    structures::paging::{PhysFrame, Size4KiB},
};

use crate::gdt;

const KERNEL_STACK_SIZE: usize = 64 * 1024;
const KERNEL_STACK_WORDS: usize = KERNEL_STACK_SIZE / size_of::<u128>();
const DEFAULT_QUANTUM_TICKS: u64 = 5;
pub const MAX_SNAPSHOT_TASKS: usize = 16;
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
    NotInitialized,
    StackLayoutInvalid,
    InvalidUserContext,
}

impl InitError {
    pub const fn description(self) -> &'static str {
        match self {
            Self::AlreadyInitialized => "scheduler is already initialized",
            Self::NotInitialized => "scheduler is not initialized",
            Self::StackLayoutInvalid => "kernel-thread stack layout is invalid",
            Self::InvalidUserContext => "userspace task context is invalid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Bootstrap,
    KernelThread,
    UserProcess,
}

impl fmt::Display for TaskKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bootstrap => formatter.write_str("bootstrap"),
            Self::KernelThread => formatter.write_str("kernel"),
            Self::UserProcess => formatter.write_str("user"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Runnable,
    Blocked,
    Stopped,
    Zombie,
}

impl fmt::Display for TaskState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runnable => formatter.write_str("runnable"),
            Self::Blocked => formatter.write_str("blocked"),
            Self::Stopped => formatter.write_str("stopped"),
            Self::Zombie => formatter.write_str("zombie"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ReapedProcessTask {
    pub process_id: u64,
    pub task_id: u64,
    pub scheduled_count: u64,
    pub runtime_ticks: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct TaskSnapshot {
    pub id: u64,
    pub name: &'static str,
    pub kind: TaskKind,
    pub state: TaskState,
    pub process_id: Option<u64>,
    pub stack_bytes: usize,
    pub page_table_address: u64,
    pub scheduled_count: u64,
    pub runtime_ticks: u64,
}

impl TaskSnapshot {
    const EMPTY: Self = Self {
        id: 0,
        name: "",
        kind: TaskKind::KernelThread,
        state: TaskState::Zombie,
        process_id: None,
        stack_bytes: 0,
        page_table_address: 0,
        scheduled_count: 0,
        runtime_ticks: 0,
    };
}

#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    pub running: bool,
    pub task_count: usize,
    pub runnable_task_count: usize,
    pub blocked_task_count: usize,
    pub stopped_task_count: usize,
    pub zombie_task_count: usize,
    pub user_task_count: usize,
    pub current_task_id: u64,
    pub current_task_name: &'static str,
    pub current_task_kind: TaskKind,
    pub current_process_id: Option<u64>,
    pub quantum_ticks: u64,
    pub context_switches: u64,
    pub preemptions: u64,
    pub voluntary_switches: u64,
    pub address_space_switches: u64,
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct AddressSpace {
    frame: PhysFrame<Size4KiB>,
    flags: Cr3Flags,
}

impl AddressSpace {
    fn current() -> Self {
        let (frame, flags) = Cr3::read();
        Self { frame, flags }
    }

    fn user(frame: PhysFrame<Size4KiB>) -> Self {
        Self {
            frame,
            flags: Cr3Flags::empty(),
        }
    }

    fn address(self) -> u64 {
        self.frame.start_address().as_u64()
    }
}

struct Task {
    id: u64,
    name: &'static str,
    kind: TaskKind,
    state: TaskState,
    stopped_resume_state: Option<TaskState>,
    process_id: Option<u64>,
    stack_pointer: usize,
    stack: Option<Box<[u128]>>,
    stack_bytes: usize,
    address_space: AddressSpace,
    privilege_stack_top: Option<VirtAddr>,
    scheduled_count: u64,
    runtime_ticks: u64,
}

impl Task {
    fn bootstrap(id: u64, address_space: AddressSpace) -> Self {
        Self {
            id,
            name: "bootstrap-shell",
            kind: TaskKind::Bootstrap,
            state: TaskState::Runnable,
            stopped_resume_state: None,
            process_id: None,
            stack_pointer: 0,
            stack: None,
            stack_bytes: 0,
            address_space,
            privilege_stack_top: None,
            scheduled_count: 1,
            runtime_ticks: 0,
        }
    }

    fn kernel_thread(
        id: u64,
        name: &'static str,
        entry: ThreadEntry,
        address_space: AddressSpace,
    ) -> Result<Self, InitError> {
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
            kind: TaskKind::KernelThread,
            state: TaskState::Runnable,
            stopped_resume_state: None,
            process_id: None,
            stack_pointer,
            stack: Some(stack),
            stack_bytes,
            address_space,
            privilege_stack_top: None,
            scheduled_count: 0,
            runtime_ticks: 0,
        })
    }

    fn user_process(
        id: u64,
        name: &'static str,
        process_id: u64,
        stack_pointer: usize,
        kernel_stack_top: VirtAddr,
        kernel_stack_bytes: usize,
        page_table_frame: PhysFrame<Size4KiB>,
    ) -> Result<Self, InitError> {
        if stack_pointer == 0
            || stack_pointer % 16 != 0
            || kernel_stack_top.as_u64() == 0
            || kernel_stack_bytes == 0
        {
            return Err(InitError::InvalidUserContext);
        }

        Ok(Self {
            id,
            name,
            kind: TaskKind::UserProcess,
            state: TaskState::Runnable,
            stopped_resume_state: None,
            process_id: Some(process_id),
            stack_pointer,
            stack: None,
            stack_bytes: kernel_stack_bytes,
            address_space: AddressSpace::user(page_table_frame),
            privilege_stack_top: Some(kernel_stack_top),
            scheduled_count: 0,
            runtime_ticks: 0,
        })
    }

    fn is_runnable(&self) -> bool {
        self.state == TaskState::Runnable && self.stack_pointer != 0
    }

    fn snapshot(&self) -> TaskSnapshot {
        TaskSnapshot {
            id: self.id,
            name: self.name,
            kind: self.kind,
            state: self.state,
            process_id: self.process_id,
            stack_bytes: self.stack_bytes,
            page_table_address: self.address_space.address(),
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
    kernel_address_space: Option<AddressSpace>,
    quantum_ticks: u64,
    ticks_in_quantum: u64,
    context_switches: u64,
    preemptions: u64,
    voluntary_switches: u64,
    address_space_switches: u64,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            tasks: Vec::new(),
            current_task: 0,
            next_task_id: 0,
            running: false,
            kernel_address_space: None,
            quantum_ticks: DEFAULT_QUANTUM_TICKS,
            ticks_in_quantum: 0,
            context_switches: 0,
            preemptions: 0,
            voluntary_switches: 0,
            address_space_switches: 0,
        }
    }

    fn initialize(&mut self) -> Result<(), InitError> {
        if self.running || !self.tasks.is_empty() {
            return Err(InitError::AlreadyInitialized);
        }

        let kernel_address_space = AddressSpace::current();
        self.kernel_address_space = Some(kernel_address_space);
        let bootstrap_id = self.allocate_task_id();
        self.tasks
            .push(Task::bootstrap(bootstrap_id, kernel_address_space));
        self.spawn_kernel("scheduler-probe-a", scheduler_probe_a)?;
        self.spawn_kernel("scheduler-probe-b", scheduler_probe_b)?;
        self.running = true;
        gdt::reset_privilege_stack();
        Ok(())
    }

    fn spawn_kernel(&mut self, name: &'static str, entry: ThreadEntry) -> Result<u64, InitError> {
        let address_space = self.kernel_address_space.ok_or(InitError::NotInitialized)?;
        let id = self.allocate_task_id();
        self.tasks
            .push(Task::kernel_thread(id, name, entry, address_space)?);
        Ok(id)
    }

    fn spawn_user(
        &mut self,
        name: &'static str,
        process_id: u64,
        stack_pointer: usize,
        kernel_stack_top: VirtAddr,
        kernel_stack_bytes: usize,
        page_table_frame: PhysFrame<Size4KiB>,
    ) -> Result<u64, InitError> {
        if !self.running {
            return Err(InitError::NotInitialized);
        }
        let id = self.allocate_task_id();
        self.tasks.push(Task::user_process(
            id,
            name,
            process_id,
            stack_pointer,
            kernel_stack_top,
            kernel_stack_bytes,
            page_table_frame,
        )?);
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

        self.ticks_in_quantum = self.ticks_in_quantum.saturating_add(1);
        if self.ticks_in_quantum < self.quantum_ticks {
            return current_stack_pointer;
        }
        self.ticks_in_quantum = 0;

        self.switch_to_next(current_stack_pointer, SwitchReason::Preempt)
    }

    fn yield_now(&mut self, current_stack_pointer: usize) -> usize {
        if !self.running || self.tasks.is_empty() {
            return current_stack_pointer;
        }

        self.tasks[self.current_task].stack_pointer = current_stack_pointer;
        self.ticks_in_quantum = 0;
        self.switch_to_next(current_stack_pointer, SwitchReason::Voluntary)
    }

    fn block_current(&mut self, current_stack_pointer: usize) -> usize {
        if !self.running || self.tasks.is_empty() {
            return current_stack_pointer;
        }

        let current = self.current_task;
        if self.tasks[current].kind != TaskKind::UserProcess {
            return current_stack_pointer;
        }
        self.tasks[current].stack_pointer = current_stack_pointer;
        self.tasks[current].state = TaskState::Blocked;
        self.ticks_in_quantum = 0;

        let next = self
            .next_runnable_after(current)
            .expect("scheduler has no runnable task after userspace blocking");
        self.switch_to(next, SwitchReason::Block)
    }

    fn wake_process(&mut self, process_id: u64) -> bool {
        let Some(task) = self
            .tasks
            .iter_mut()
            .find(|task| task.process_id == Some(process_id))
        else {
            return false;
        };
        match task.state {
            TaskState::Runnable => true,
            TaskState::Blocked => {
                task.state = TaskState::Runnable;
                true
            }
            TaskState::Stopped if task.stopped_resume_state == Some(TaskState::Blocked) => {
                task.stopped_resume_state = Some(TaskState::Runnable);
                true
            }
            _ => false,
        }
    }

    fn process_stack_pointer(&self, process_id: u64) -> Option<usize> {
        self.tasks
            .iter()
            .enumerate()
            .find(|(index, task)| {
                *index != self.current_task
                    && task.kind == TaskKind::UserProcess
                    && task.state != TaskState::Zombie
                    && task.process_id == Some(process_id)
            })
            .map(|(_, task)| task.stack_pointer)
            .filter(|stack_pointer| *stack_pointer != 0)
    }

    fn replace_process_image(
        &mut self,
        process_id: u64,
        stack_pointer: usize,
        page_table_frame: PhysFrame<Size4KiB>,
    ) -> bool {
        if stack_pointer == 0 || stack_pointer % 16 != 0 {
            return false;
        }
        let current = self.current_task;
        let Some((index, task)) = self
            .tasks
            .iter_mut()
            .enumerate()
            .find(|(_, task)| task.process_id == Some(process_id))
        else {
            return false;
        };
        if index == current
            || task.kind != TaskKind::UserProcess
            || task.state != TaskState::Blocked
        {
            return false;
        }
        task.stack_pointer = stack_pointer;
        task.address_space = AddressSpace::user(page_table_frame);
        task.stopped_resume_state = None;
        true
    }

    fn is_process_blocked(&self, process_id: u64) -> bool {
        self.tasks.iter().any(|task| {
            task.process_id == Some(process_id)
                && (task.state == TaskState::Blocked
                    || (task.state == TaskState::Stopped
                        && task.stopped_resume_state == Some(TaskState::Blocked)))
        })
    }

    fn is_process_stopped(&self, process_id: u64) -> bool {
        self.tasks
            .iter()
            .any(|task| task.process_id == Some(process_id) && task.state == TaskState::Stopped)
    }

    fn stop_process(&mut self, process_id: u64) -> bool {
        let current = self.current_task;
        let Some((index, task)) = self
            .tasks
            .iter_mut()
            .enumerate()
            .find(|(_, task)| task.process_id == Some(process_id))
        else {
            return false;
        };
        if index == current || task.kind != TaskKind::UserProcess {
            return false;
        }
        match task.state {
            TaskState::Runnable | TaskState::Blocked => {
                task.stopped_resume_state = Some(task.state);
                task.state = TaskState::Stopped;
                true
            }
            TaskState::Stopped | TaskState::Zombie => false,
        }
    }

    fn continue_process(&mut self, process_id: u64) -> bool {
        let Some(task) = self
            .tasks
            .iter_mut()
            .find(|task| task.process_id == Some(process_id))
        else {
            return false;
        };
        if task.state != TaskState::Stopped {
            return false;
        }
        task.state = task
            .stopped_resume_state
            .take()
            .unwrap_or(TaskState::Runnable);
        true
    }

    fn terminate_process(&mut self, process_id: u64) -> bool {
        let current = self.current_task;
        let Some((index, task)) = self
            .tasks
            .iter_mut()
            .enumerate()
            .find(|(_, task)| task.process_id == Some(process_id))
        else {
            return false;
        };
        if index == current || task.state == TaskState::Zombie {
            return false;
        }
        task.state = TaskState::Zombie;
        task.stopped_resume_state = None;
        true
    }

    fn terminate_current(&mut self, current_stack_pointer: usize) -> usize {
        if !self.running || self.tasks.is_empty() {
            return current_stack_pointer;
        }

        let current = self.current_task;
        self.tasks[current].stack_pointer = current_stack_pointer;
        self.tasks[current].state = TaskState::Zombie;
        self.tasks[current].stopped_resume_state = None;
        self.ticks_in_quantum = 0;

        let next = self
            .next_runnable_after(current)
            .expect("scheduler has no runnable task after process termination");
        self.switch_to(next, SwitchReason::Termination)
    }

    fn switch_to_next(&mut self, current_stack_pointer: usize, reason: SwitchReason) -> usize {
        let Some(next) = self.next_runnable_after(self.current_task) else {
            return current_stack_pointer;
        };
        if next == self.current_task {
            return current_stack_pointer;
        }
        self.switch_to(next, reason)
    }

    fn next_runnable_after(&self, current: usize) -> Option<usize> {
        if self.tasks.is_empty() {
            return None;
        }
        for offset in 1..=self.tasks.len() {
            let index = (current + offset) % self.tasks.len();
            if self.tasks[index].is_runnable() {
                return Some(index);
            }
        }
        None
    }

    fn switch_to(&mut self, next: usize, reason: SwitchReason) -> usize {
        let current = self.current_task;
        let current_address_space = self.tasks[current].address_space;
        let next_address_space = self.tasks[next].address_space;
        let next_stack_pointer = self.tasks[next].stack_pointer;

        self.current_task = next;
        self.context_switches = self.context_switches.saturating_add(1);
        match reason {
            SwitchReason::Preempt => self.preemptions = self.preemptions.saturating_add(1),
            SwitchReason::Voluntary => {
                self.voluntary_switches = self.voluntary_switches.saturating_add(1)
            }
            SwitchReason::Block | SwitchReason::Termination => {}
        }
        self.tasks[next].scheduled_count = self.tasks[next].scheduled_count.saturating_add(1);

        if current_address_space != next_address_space {
            unsafe { Cr3::write(next_address_space.frame, next_address_space.flags) };
            self.address_space_switches = self.address_space_switches.saturating_add(1);
        }
        match self.tasks[next].privilege_stack_top {
            Some(stack_top) => gdt::set_privilege_stack(stack_top),
            None => gdt::reset_privilege_stack(),
        }

        next_stack_pointer
    }

    fn current_process_id(&self) -> Option<u64> {
        self.tasks
            .get(self.current_task)
            .and_then(|task| task.process_id)
    }

    fn current_task_kind(&self) -> TaskKind {
        self.tasks
            .get(self.current_task)
            .map(|task| task.kind)
            .unwrap_or(TaskKind::KernelThread)
    }

    fn reap_zombies(&mut self) -> Vec<ReapedProcessTask> {
        let current_id = self.tasks.get(self.current_task).map(|task| task.id);
        let mut process_tasks = Vec::new();
        self.tasks.retain(|task| {
            if task.state == TaskState::Zombie {
                if let Some(process_id) = task.process_id {
                    process_tasks.push(ReapedProcessTask {
                        process_id,
                        task_id: task.id,
                        scheduled_count: task.scheduled_count,
                        runtime_ticks: task.runtime_ticks,
                    });
                }
                false
            } else {
                true
            }
        });
        if let Some(current_id) = current_id {
            self.current_task = self
                .tasks
                .iter()
                .position(|task| task.id == current_id)
                .unwrap_or(0);
        } else {
            self.current_task = 0;
        }
        process_tasks
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
            runnable_task_count: self
                .tasks
                .iter()
                .filter(|task| task.state == TaskState::Runnable)
                .count(),
            blocked_task_count: self
                .tasks
                .iter()
                .filter(|task| task.state == TaskState::Blocked)
                .count(),
            stopped_task_count: self
                .tasks
                .iter()
                .filter(|task| task.state == TaskState::Stopped)
                .count(),
            zombie_task_count: self
                .tasks
                .iter()
                .filter(|task| task.state == TaskState::Zombie)
                .count(),
            user_task_count: self
                .tasks
                .iter()
                .filter(|task| task.kind == TaskKind::UserProcess)
                .count(),
            current_task_id: current_task.map(|task| task.id).unwrap_or(0),
            current_task_name: current_task.map(|task| task.name).unwrap_or("none"),
            current_task_kind: current_task
                .map(|task| task.kind)
                .unwrap_or(TaskKind::KernelThread),
            current_process_id: current_task.and_then(|task| task.process_id),
            quantum_ticks: self.quantum_ticks,
            context_switches: self.context_switches,
            preemptions: self.preemptions,
            voluntary_switches: self.voluntary_switches,
            address_space_switches: self.address_space_switches,
            probe_a_heartbeats: PROBE_A_HEARTBEATS.load(Ordering::Relaxed),
            probe_b_heartbeats: PROBE_B_HEARTBEATS.load(Ordering::Relaxed),
            truncated: self.tasks.len() > recorded_task_count,
            tasks,
            recorded_task_count,
        }
    }
}

#[derive(Clone, Copy)]
enum SwitchReason {
    Preempt,
    Voluntary,
    Block,
    Termination,
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

pub fn current_process_id() -> Option<u64> {
    SCHEDULER.lock().current_process_id()
}

pub fn current_task_kind() -> TaskKind {
    SCHEDULER.lock().current_task_kind()
}

pub fn spawn_user_process(
    name: &'static str,
    process_id: u64,
    stack_pointer: usize,
    kernel_stack_top: VirtAddr,
    kernel_stack_bytes: usize,
    page_table_address: u64,
) -> Result<u64, InitError> {
    let page_table_frame = PhysFrame::from_start_address(PhysAddr::new(page_table_address))
        .map_err(|_| InitError::InvalidUserContext)?;
    cpu_interrupts::without_interrupts(|| {
        SCHEDULER.lock().spawn_user(
            name,
            process_id,
            stack_pointer,
            kernel_stack_top,
            kernel_stack_bytes,
            page_table_frame,
        )
    })
}

pub fn on_timer_interrupt(current_stack_pointer: usize) -> usize {
    SCHEDULER.lock().schedule(current_stack_pointer)
}

pub fn on_yield(current_stack_pointer: usize) -> usize {
    SCHEDULER.lock().yield_now(current_stack_pointer)
}

pub fn block_current(current_stack_pointer: usize) -> usize {
    SCHEDULER.lock().block_current(current_stack_pointer)
}

pub fn wake_process(process_id: u64) -> bool {
    cpu_interrupts::without_interrupts(|| SCHEDULER.lock().wake_process(process_id))
}

pub fn process_stack_pointer(process_id: u64) -> Option<usize> {
    cpu_interrupts::without_interrupts(|| SCHEDULER.lock().process_stack_pointer(process_id))
}

pub fn replace_process_image(
    process_id: u64,
    stack_pointer: usize,
    page_table_address: u64,
) -> bool {
    let Ok(page_table_frame) = PhysFrame::from_start_address(PhysAddr::new(page_table_address))
    else {
        return false;
    };
    cpu_interrupts::without_interrupts(|| {
        SCHEDULER
            .lock()
            .replace_process_image(process_id, stack_pointer, page_table_frame)
    })
}

pub fn is_process_blocked(process_id: u64) -> bool {
    cpu_interrupts::without_interrupts(|| SCHEDULER.lock().is_process_blocked(process_id))
}

pub fn is_process_stopped(process_id: u64) -> bool {
    cpu_interrupts::without_interrupts(|| SCHEDULER.lock().is_process_stopped(process_id))
}

pub fn stop_process(process_id: u64) -> bool {
    cpu_interrupts::without_interrupts(|| SCHEDULER.lock().stop_process(process_id))
}

pub fn continue_process(process_id: u64) -> bool {
    cpu_interrupts::without_interrupts(|| SCHEDULER.lock().continue_process(process_id))
}

pub fn terminate_process(process_id: u64) -> bool {
    cpu_interrupts::without_interrupts(|| SCHEDULER.lock().terminate_process(process_id))
}

pub fn terminate_current(current_stack_pointer: usize) -> usize {
    SCHEDULER.lock().terminate_current(current_stack_pointer)
}

pub fn reap_zombie_processes() -> Vec<ReapedProcessTask> {
    cpu_interrupts::without_interrupts(|| SCHEDULER.lock().reap_zombies())
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
