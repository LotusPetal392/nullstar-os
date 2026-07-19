use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{
    arch::global_asm,
    fmt,
    mem::{align_of, size_of},
    ptr, slice, str,
};

use spin::Mutex;
use x86_64::{
    VirtAddr,
    instructions::{hlt, interrupts as cpu_interrupts},
    registers::control::Cr2,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageSize, PageTable, PageTableFlags,
        PhysFrame, Size4KiB, mapper::MapToError,
    },
};

use crate::{gdt, memory::BootInfoFrameAllocator, scheduler, vfs};

use super::{
    elf::{self, Image, ImageType, LoadSegment},
    pipe::{self, PipeId},
    terminal,
};

pub use super::{pipe::Snapshot as PipeSnapshot, terminal::Snapshot as TerminalSnapshot};

pub const SYSCALL_VECTOR: u8 = 0x80;

const PAGE_FAULT_VECTOR: u64 = 14;
const GENERAL_PROTECTION_VECTOR: u64 = 13;
const SYSCALL_WRITE: u64 = 1;
const SYSCALL_YIELD: u64 = 2;
const SYSCALL_EXIT: u64 = 3;
const SYSCALL_OPEN: u64 = 4;
const SYSCALL_READ: u64 = 5;
const SYSCALL_CLOSE: u64 = 6;
const SYSCALL_SPAWN_COMMAND: u64 = 7;
const SYSCALL_WAIT_CHILD: u64 = 8;
const SYSCALL_GETPID: u64 = 9;
const SYSCALL_PIPE_PAIR: u64 = 10;
const SYSCALL_TRY_WAIT_CHILD: u64 = 11;
const SYSCALL_SIGNAL_PROCESS_GROUP: u64 = 12;

pub const SIGNAL_INTERRUPT: u64 = 2;
pub const SIGNAL_TERMINATE: u64 = 15;

const USER_MIN_ADDRESS: u64 = 0x0001_0000;
const USER_PML4_SLOT_END: u64 = 0x0000_0080_0000_0000;
const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;
const USER_STACK_SIZE: usize = 64 * 1024;
const USER_STACK_GUARD_SIZE: usize = Size4KiB::SIZE as usize;
const KERNEL_TRANSITION_STACK_SIZE: usize = 64 * 1024;
const KERNEL_TRANSITION_STACK_WORDS: usize = KERNEL_TRANSITION_STACK_SIZE / size_of::<u128>();
const MAX_SYSCALL_WRITE_BYTES: usize = 4096;
const MAX_SYSCALL_READ_BYTES: usize = 4096;
const MAX_OPEN_FILES: usize = 16;
const MAX_ARGUMENTS: usize = 16;
const MAX_ARGUMENT_BYTES: usize = 4096;
const MAX_COMMAND_BYTES: usize = 512;
const SPAWN_FOREGROUND: u64 = 1;
const SPAWN_USE_DESCRIPTORS: u64 = 1 << 1;
const SPAWN_NEW_PROCESS_GROUP: u64 = 1 << 2;
const SPAWN_JOIN_PROCESS_GROUP: u64 = 1 << 3;
const DEFAULT_DESCRIPTOR: u64 = u64::MAX;
const DEFAULT_PROCESS_GROUP: u64 = u64::MAX;
const SHELL_PROCESS_TASK_NAME: &str = "user-shell-process";
const USER_RFLAGS: u64 = 0x202;
const PAGE_BYTES: u64 = Size4KiB::SIZE;

const ERR_NO_ENTRY: i64 = -2;
const ERR_NO_PROCESS: i64 = -3;
const ERR_IO: i64 = -5;
const ERR_ARGUMENT_TOO_LARGE: i64 = -7;
const ERR_BAD_FILE_DESCRIPTOR: i64 = -9;
const ERR_NO_CHILD: i64 = -10;
const ERR_TRY_AGAIN: i64 = -11;
const ERR_BAD_ADDRESS: i64 = -14;
const ERR_IS_DIRECTORY: i64 = -21;
const ERR_INVALID_ARGUMENT: i64 = -22;
const ERR_TOO_MANY_OPEN_FILES: i64 = -24;
const ERR_BROKEN_PIPE: i64 = -32;
const ERR_NOT_IMPLEMENTED: i64 = -38;

static PROCESS_MANAGER: Mutex<ProcessManager> = Mutex::new(ProcessManager::new());

global_asm!(
    r#"
    .section .text
    .p2align 4
    .global galactic_syscall_interrupt_entry
    .type galactic_syscall_interrupt_entry,@function
galactic_syscall_interrupt_entry:
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
    call galactic_syscall_dispatch
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
.size galactic_syscall_interrupt_entry, .-galactic_syscall_interrupt_entry

    .p2align 4
    .global galactic_page_fault_interrupt_entry
    .type galactic_page_fault_interrupt_entry,@function
galactic_page_fault_interrupt_entry:
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
    mov rsi, {page_fault_vector}
    and rsp, -16
    call galactic_user_fault_dispatch
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
.size galactic_page_fault_interrupt_entry, .-galactic_page_fault_interrupt_entry

    .p2align 4
    .global galactic_general_protection_interrupt_entry
    .type galactic_general_protection_interrupt_entry,@function
galactic_general_protection_interrupt_entry:
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
    mov rsi, {general_protection_vector}
    and rsp, -16
    call galactic_user_fault_dispatch
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
.size galactic_general_protection_interrupt_entry, .-galactic_general_protection_interrupt_entry
"#,
    page_fault_vector = const PAGE_FAULT_VECTOR,
    general_protection_vector = const GENERAL_PROTECTION_VECTOR,
);

unsafe extern "C" {
    fn galactic_syscall_interrupt_entry();
    fn galactic_page_fault_interrupt_entry();
    fn galactic_general_protection_interrupt_entry();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Runnable,
    Blocked,
    Exited,
    Faulted,
    Signaled,
}

impl fmt::Display for ProcessState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runnable => formatter.write_str("runnable"),
            Self::Blocked => formatter.write_str("blocked"),
            Self::Exited => formatter.write_str("exited"),
            Self::Faulted => formatter.write_str("faulted"),
            Self::Signaled => formatter.write_str("signaled"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultInfo {
    pub vector: u64,
    pub error_code: u64,
    pub address: u64,
    pub instruction_pointer: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminationReason {
    Exit(u64),
    Fault(FaultInfo),
    Signal(u64),
}

impl fmt::Display for TerminationReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exit(code) => write!(formatter, "exit({code})"),
            Self::Fault(fault) => write!(
                formatter,
                "fault(vector={}, address={:#018x}, rip={:#018x})",
                fault.vector, fault.address, fault.instruction_pointer
            ),
            Self::Signal(signal) => write!(formatter, "signal({signal})"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessResult {
    pub process_id: u64,
    pub parent_process_id: Option<u64>,
    pub process_group_id: u64,
    pub task_id: u64,
    pub path: String,
    pub termination: TerminationReason,
    pub entry_point: u64,
    pub page_table_address: u64,
    pub mapped_pages: usize,
    pub load_segments: usize,
    pub user_stack_bytes: usize,
    pub guard_page_address: u64,
    pub kernel_stack_bytes: usize,
    pub syscall_count: u64,
    pub write_count: u64,
    pub yield_count: u64,
    pub bytes_written: u64,
    pub open_count: u64,
    pub read_count: u64,
    pub close_count: u64,
    pub bytes_read: u64,
    pub terminal_read_count: u64,
    pub terminal_bytes_read: u64,
    pub blocked_read_count: u64,
    pub pipe_read_count: u64,
    pub pipe_write_count: u64,
    pub pipe_bytes_read: u64,
    pub pipe_bytes_written: u64,
    pub blocked_pipe_read_count: u64,
    pub blocked_pipe_write_count: u64,
    pub child_spawn_count: u64,
    pub child_wait_count: u64,
    pub child_poll_count: u64,
    pub child_poll_pending_count: u64,
    pub signal_sent_count: u64,
    pub signal_received_count: u64,
    pub pipe_pair_count: u64,
    pub pipe_descriptor_close_count: u64,
    pub pipe_descriptor_inherit_count: u64,
    pub scheduled_count: u64,
    pub runtime_ticks: u64,
    pub frames_reclaimed: usize,
}

impl ProcessResult {
    pub fn exit_code(&self) -> Option<u64> {
        match &self.termination {
            TerminationReason::Exit(code) => Some(*code),
            TerminationReason::Fault(_) | TerminationReason::Signal(_) => None,
        }
    }

    pub fn fault(&self) -> Option<FaultInfo> {
        match &self.termination {
            TerminationReason::Fault(fault) => Some(*fault),
            TerminationReason::Exit(_) | TerminationReason::Signal(_) => None,
        }
    }

    pub fn signal(&self) -> Option<u64> {
        match &self.termination {
            TerminationReason::Signal(signal) => Some(*signal),
            TerminationReason::Exit(_) | TerminationReason::Fault(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManagerSnapshot {
    pub spawned: u64,
    pub child_spawns: u64,
    pub child_waits: u64,
    pub signals_sent: u64,
    pub pipe_pairs: u64,
    pub pipe_descriptor_inherits: u64,
    pub active: usize,
    pub blocked: usize,
    pub exited: u64,
    pub faulted: u64,
    pub signaled: u64,
    pub reaped: u64,
    pub frames_reclaimed: u64,
    pub results: Vec<ProcessResult>,
}

#[derive(Debug, Clone)]
pub struct SpawnInfo {
    pub process_id: u64,
    pub process_group_id: u64,
    pub task_id: u64,
    pub path: String,
    pub entry_point: u64,
    pub page_table_address: u64,
    pub mapped_pages: usize,
    pub owned_frames: usize,
}

#[derive(Debug, Clone)]
pub struct ProcessGroupInfo {
    pub process_group_id: u64,
    pub process_ids: Vec<u64>,
}

#[derive(Debug)]
pub enum Error {
    SchedulerNotOnBootstrapTask,
    UnsupportedImageType,
    InvalidUserRange,
    KernelMappingUsesUserSlot(u64),
    AddressOverflow,
    FrameAllocationFailed,
    PageTableFrameAllocationFailed,
    ParentEntryHugePage,
    PageAlreadyMapped(u64),
    StackLayoutInvalid,
    TooManyArguments,
    ArgumentBytesTooLarge,
    InvalidArgument,
    InvalidDescriptor(u64),
    InvalidProcessGroup(u64),
    TerminalBusy,
    Pipe(pipe::Error),
    ProcessNotFound(u64),
    Scheduler(scheduler::InitError),
    Elf(elf::Error),
    Vfs(vfs::Error),
}

impl Error {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::SchedulerNotOnBootstrapTask => {
                "userspace processes must be spawned from the bootstrap scheduler task"
            }
            Self::UnsupportedImageType => "the userspace loader requires an ET_EXEC image",
            Self::InvalidUserRange => "userspace virtual-address range is invalid",
            Self::KernelMappingUsesUserSlot(_) => {
                "a required kernel mapping occupies the reserved userspace PML4 slot"
            }
            Self::AddressOverflow => "userspace address calculation overflowed",
            Self::FrameAllocationFailed => "physical-frame allocation for userspace failed",
            Self::PageTableFrameAllocationFailed => {
                "physical-frame allocation for a userspace page table failed"
            }
            Self::ParentEntryHugePage => "userspace mapping collided with an upper-level huge page",
            Self::PageAlreadyMapped(_) => "a userspace virtual page was mapped more than once",
            Self::StackLayoutInvalid => "userspace kernel-transition stack layout is invalid",
            Self::TooManyArguments => "userspace argument count exceeds the configured bound",
            Self::ArgumentBytesTooLarge => {
                "userspace argument strings exceed the configured stack bound"
            }
            Self::InvalidArgument => "userspace argument contains an invalid byte",
            Self::InvalidDescriptor(_) => "userspace file descriptor is invalid",
            Self::InvalidProcessGroup(_) => "userspace process group is invalid",
            Self::TerminalBusy => "another userspace process owns the terminal",
            Self::Pipe(_) => "kernel pipe operation failed",
            Self::ProcessNotFound(_) => "userspace process bookkeeping is missing",
            Self::Scheduler(_) => "scheduler rejected the userspace task",
            Self::Elf(_) => "userspace executable validation failed",
            Self::Vfs(_) => "userspace executable read failed",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KernelMappingUsesUserSlot(address) => write!(
                formatter,
                "required kernel address {address:#018x} uses PML4 slot zero"
            ),
            Self::PageAlreadyMapped(address) => {
                write!(
                    formatter,
                    "userspace page {address:#018x} is already mapped"
                )
            }
            Self::ProcessNotFound(process_id) => {
                write!(formatter, "userspace process {process_id} was not found")
            }
            Self::InvalidDescriptor(descriptor) => {
                write!(
                    formatter,
                    "userspace file descriptor {descriptor} is invalid"
                )
            }
            Self::InvalidProcessGroup(group) => {
                write!(formatter, "userspace process group {group} is invalid")
            }
            Self::Scheduler(error) => formatter.write_str(error.description()),
            Self::Elf(error) => write!(formatter, "ELF error: {error}"),
            Self::Pipe(error) => write!(formatter, "pipe error: {error}"),
            Self::Vfs(error) => write!(formatter, "VFS error: {error}"),
            _ => formatter.write_str(self.description()),
        }
    }
}

impl From<vfs::Error> for Error {
    fn from(error: vfs::Error) -> Self {
        Self::Vfs(error)
    }
}

impl From<elf::Error> for Error {
    fn from(error: elf::Error) -> Self {
        Self::Elf(error)
    }
}

impl From<pipe::Error> for Error {
    fn from(error: pipe::Error) -> Self {
        Self::Pipe(error)
    }
}

impl From<scheduler::InitError> for Error {
    fn from(error: scheduler::InitError) -> Self {
        Self::Scheduler(error)
    }
}

#[derive(Debug, Clone, Copy)]
struct UserRange {
    start: u64,
    end: u64,
    readable: bool,
    writable: bool,
    executable: bool,
}

impl UserRange {
    fn contains(self, address: u64, length: usize) -> bool {
        let Ok(length) = u64::try_from(length) else {
            return false;
        };
        let Some(end) = address.checked_add(length) else {
            return false;
        };
        address >= self.start && end <= self.end
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingTerminalRead {
    address: u64,
    length: usize,
    stack_pointer: usize,
}

#[derive(Debug, Clone, Copy)]
struct PendingPipeRead {
    pipe_id: PipeId,
    address: u64,
    length: usize,
    stack_pointer: usize,
}

#[derive(Debug, Clone, Copy)]
struct PendingPipeWrite {
    pipe_id: PipeId,
    address: u64,
    length: usize,
    stack_pointer: usize,
}

#[derive(Debug, Clone)]
struct PendingChildSpawn {
    path: String,
    arguments: Vec<String>,
    foreground: bool,
    stdin_descriptor: Option<u64>,
    stdout_descriptor: Option<u64>,
    new_process_group: bool,
    process_group_id: Option<u64>,
    stack_pointer: usize,
}

#[derive(Debug, Clone, Copy)]
struct PendingChildWait {
    child_process_id: u64,
    stack_pointer: usize,
}

#[derive(Debug, Clone)]
struct OpenFile {
    descriptor: u64,
    path: String,
    offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PipeDirection {
    Reader,
    Writer,
}

#[derive(Debug, Clone, Copy)]
struct PipeDescriptor {
    descriptor: u64,
    pipe_id: PipeId,
    direction: PipeDirection,
}

struct Process {
    process_id: u64,
    parent_process_id: Option<u64>,
    process_group_id: u64,
    terminal_parent: Option<u64>,
    task_id: u64,
    path: String,
    state: ProcessState,
    termination: Option<TerminationReason>,
    page_table_address: u64,
    entry_point: u64,
    mapped_pages: usize,
    load_segments: usize,
    guard_page_address: u64,
    ranges: Vec<UserRange>,
    pages: Vec<UserPage>,
    kernel_stack: Box<[u128]>,
    owned_frames: Vec<PhysFrame<Size4KiB>>,
    open_files: Vec<OpenFile>,
    pipe_descriptors: Vec<PipeDescriptor>,
    stdin_pipe: Option<PipeId>,
    stdout_pipe: Option<PipeId>,
    pending_terminal_read: Option<PendingTerminalRead>,
    pending_pipe_read: Option<PendingPipeRead>,
    pending_pipe_write: Option<PendingPipeWrite>,
    pending_child_spawn: Option<PendingChildSpawn>,
    pending_child_wait: Option<PendingChildWait>,
    syscall_count: u64,
    write_count: u64,
    yield_count: u64,
    bytes_written: u64,
    open_count: u64,
    read_count: u64,
    close_count: u64,
    bytes_read: u64,
    terminal_read_count: u64,
    terminal_bytes_read: u64,
    blocked_read_count: u64,
    pipe_read_count: u64,
    pipe_write_count: u64,
    pipe_bytes_read: u64,
    pipe_bytes_written: u64,
    blocked_pipe_read_count: u64,
    blocked_pipe_write_count: u64,
    child_spawn_count: u64,
    child_wait_count: u64,
    child_poll_count: u64,
    child_poll_pending_count: u64,
    signal_sent_count: u64,
    signal_received_count: u64,
    pipe_pair_count: u64,
    pipe_descriptor_close_count: u64,
    pipe_descriptor_inherit_count: u64,
}

impl Process {
    fn result(
        &self,
        frames_reclaimed: usize,
        scheduled_count: u64,
        runtime_ticks: u64,
    ) -> Result<ProcessResult, Error> {
        let termination = self
            .termination
            .clone()
            .ok_or(Error::ProcessNotFound(self.process_id))?;
        Ok(ProcessResult {
            process_id: self.process_id,
            parent_process_id: self.parent_process_id,
            process_group_id: self.process_group_id,
            task_id: self.task_id,
            path: self.path.clone(),
            termination,
            entry_point: self.entry_point,
            page_table_address: self.page_table_address,
            mapped_pages: self.mapped_pages,
            load_segments: self.load_segments,
            user_stack_bytes: USER_STACK_SIZE,
            guard_page_address: self.guard_page_address,
            kernel_stack_bytes: self.kernel_stack.len() * size_of::<u128>(),
            syscall_count: self.syscall_count,
            write_count: self.write_count,
            yield_count: self.yield_count,
            bytes_written: self.bytes_written,
            open_count: self.open_count,
            read_count: self.read_count,
            close_count: self.close_count,
            bytes_read: self.bytes_read,
            terminal_read_count: self.terminal_read_count,
            terminal_bytes_read: self.terminal_bytes_read,
            blocked_read_count: self.blocked_read_count,
            pipe_read_count: self.pipe_read_count,
            pipe_write_count: self.pipe_write_count,
            pipe_bytes_read: self.pipe_bytes_read,
            pipe_bytes_written: self.pipe_bytes_written,
            blocked_pipe_read_count: self.blocked_pipe_read_count,
            blocked_pipe_write_count: self.blocked_pipe_write_count,
            child_spawn_count: self.child_spawn_count,
            child_wait_count: self.child_wait_count,
            child_poll_count: self.child_poll_count,
            child_poll_pending_count: self.child_poll_pending_count,
            signal_sent_count: self.signal_sent_count,
            signal_received_count: self.signal_received_count,
            pipe_pair_count: self.pipe_pair_count,
            pipe_descriptor_close_count: self.pipe_descriptor_close_count,
            pipe_descriptor_inherit_count: self.pipe_descriptor_inherit_count,
            scheduled_count,
            runtime_ticks,
            frames_reclaimed,
        })
    }
}

struct ProcessManager {
    next_process_id: u64,
    processes: Vec<Process>,
    completed: Vec<ProcessResult>,
    spawned: u64,
    child_spawns: u64,
    child_waits: u64,
    signals_sent: u64,
    pipe_pairs: u64,
    pipe_descriptor_inherits: u64,
    exited: u64,
    faulted: u64,
    signaled: u64,
    reaped: u64,
    frames_reclaimed: u64,
}

impl ProcessManager {
    const fn new() -> Self {
        Self {
            next_process_id: 1,
            processes: Vec::new(),
            completed: Vec::new(),
            spawned: 0,
            child_spawns: 0,
            child_waits: 0,
            signals_sent: 0,
            pipe_pairs: 0,
            pipe_descriptor_inherits: 0,
            exited: 0,
            faulted: 0,
            signaled: 0,
            reaped: 0,
            frames_reclaimed: 0,
        }
    }

    fn allocate_process_id(&mut self) -> u64 {
        let process_id = self.next_process_id;
        self.next_process_id = self.next_process_id.saturating_add(1);
        process_id
    }

    fn process_mut(&mut self, process_id: u64) -> Option<&mut Process> {
        self.processes
            .iter_mut()
            .find(|process| process.process_id == process_id)
    }

    fn remove_process(&mut self, process_id: u64) -> Option<Process> {
        let index = self
            .processes
            .iter()
            .position(|process| process.process_id == process_id)?;
        Some(self.processes.remove(index))
    }

    fn snapshot(&self) -> ManagerSnapshot {
        ManagerSnapshot {
            spawned: self.spawned,
            child_spawns: self.child_spawns,
            child_waits: self.child_waits,
            signals_sent: self.signals_sent,
            pipe_pairs: self.pipe_pairs,
            pipe_descriptor_inherits: self.pipe_descriptor_inherits,
            active: self
                .processes
                .iter()
                .filter(|process| {
                    matches!(
                        process.state,
                        ProcessState::Runnable | ProcessState::Blocked
                    )
                })
                .count(),
            blocked: self
                .processes
                .iter()
                .filter(|process| process.state == ProcessState::Blocked)
                .count(),
            exited: self.exited,
            faulted: self.faulted,
            signaled: self.signaled,
            reaped: self.reaped,
            frames_reclaimed: self.frames_reclaimed,
            results: self.completed.clone(),
        }
    }
}

#[derive(Clone, Copy)]
struct UserPage {
    virtual_address: u64,
    frame: PhysFrame<Size4KiB>,
}

struct BuiltAddressSpace {
    page_table_frame: PhysFrame<Size4KiB>,
    entry_point: u64,
    stack_pointer: u64,
    guard_page_address: u64,
    pages: Vec<UserPage>,
    ranges: Vec<UserRange>,
    owned_frames: Vec<PhysFrame<Size4KiB>>,
}

struct TrackingFrameAllocator<'a> {
    inner: &'a mut BootInfoFrameAllocator,
    frames: Vec<PhysFrame<Size4KiB>>,
}

impl<'a> TrackingFrameAllocator<'a> {
    fn new(inner: &'a mut BootInfoFrameAllocator) -> Self {
        Self {
            inner,
            frames: Vec::new(),
        }
    }

    fn take_frames(&mut self) -> Vec<PhysFrame<Size4KiB>> {
        core::mem::take(&mut self.frames)
    }

    fn reclaim_all(&mut self) {
        for frame in self.frames.drain(..) {
            self.inner.deallocate_frame(frame);
        }
    }
}

unsafe impl FrameAllocator<Size4KiB> for TrackingFrameAllocator<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let frame = self.inner.allocate_frame()?;
        self.frames.push(frame);
        Some(frame)
    }
}

fn descriptor_in_use(process: &Process, descriptor: u64) -> bool {
    process
        .open_files
        .iter()
        .any(|file| file.descriptor == descriptor)
        || process
            .pipe_descriptors
            .iter()
            .any(|pipe| pipe.descriptor == descriptor)
}

fn descriptor_count(process: &Process) -> usize {
    process
        .open_files
        .len()
        .saturating_add(process.pipe_descriptors.len())
}

fn allocate_descriptor(process: &Process) -> Option<u64> {
    (3..3 + MAX_OPEN_FILES as u64).find(|descriptor| !descriptor_in_use(process, *descriptor))
}

fn allocate_descriptor_pair(process: &Process) -> Option<(u64, u64)> {
    let mut available = (3..3 + MAX_OPEN_FILES as u64)
        .filter(|descriptor| !descriptor_in_use(process, *descriptor));
    Some((available.next()?, available.next()?))
}

fn resolve_pipe_descriptor(
    process: &Process,
    descriptor: Option<u64>,
    direction: PipeDirection,
) -> Result<Option<PipeId>, Error> {
    descriptor
        .map(|descriptor| {
            process
                .pipe_descriptors
                .iter()
                .find(|pipe| pipe.descriptor == descriptor && pipe.direction == direction)
                .map(|pipe| pipe.pipe_id)
                .ok_or(Error::InvalidDescriptor(descriptor))
        })
        .transpose()
}

impl BuiltAddressSpace {
    fn build(
        path: &str,
        image: &Image,
        arguments: &[&str],
        kernel_mapper: &mut OffsetPageTable<'_>,
        frame_allocator: &mut BootInfoFrameAllocator,
        physical_memory_offset: VirtAddr,
    ) -> Result<Self, Error> {
        let mut tracking = TrackingFrameAllocator::new(frame_allocator);
        let result = Self::build_tracked(
            path,
            image,
            arguments,
            kernel_mapper,
            &mut tracking,
            physical_memory_offset,
        );
        match result {
            Ok(mut address_space) => {
                address_space.owned_frames = tracking.take_frames();
                Ok(address_space)
            }
            Err(error) => {
                tracking.reclaim_all();
                Err(error)
            }
        }
    }

    fn build_tracked(
        path: &str,
        image: &Image,
        arguments: &[&str],
        kernel_mapper: &mut OffsetPageTable<'_>,
        frame_allocator: &mut TrackingFrameAllocator<'_>,
        physical_memory_offset: VirtAddr,
    ) -> Result<Self, Error> {
        if image.image_type != ImageType::Executable {
            return Err(Error::UnsupportedImageType);
        }

        let page_table_frame = frame_allocator
            .allocate_frame()
            .ok_or(Error::FrameAllocationFailed)?;
        let table_virtual_address = physical_memory_offset
            .as_u64()
            .checked_add(page_table_frame.start_address().as_u64())
            .ok_or(Error::AddressOverflow)?;
        let table_pointer = table_virtual_address as *mut PageTable;
        unsafe { table_pointer.write(PageTable::new()) };
        let level_4_table = unsafe { &mut *table_pointer };

        for index in 1..512 {
            let source = &kernel_mapper.level_4_table()[index];
            if !source.is_unused() {
                level_4_table[index].set_addr(source.addr(), source.flags());
            }
        }

        let mut mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };
        let mut pages = Vec::new();
        let mut ranges = Vec::new();

        for segment in image.load_segments() {
            if segment.memory_size == 0 {
                continue;
            }
            let end = segment
                .virtual_address
                .checked_add(segment.memory_size)
                .ok_or(Error::AddressOverflow)?;
            validate_user_range(segment.virtual_address, end)?;

            let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
            if segment.writable {
                flags |= PageTableFlags::WRITABLE;
            }
            if !segment.executable {
                flags |= PageTableFlags::NO_EXECUTE;
            }

            map_range(
                segment.virtual_address,
                end,
                flags,
                &mut mapper,
                frame_allocator,
                physical_memory_offset,
                &mut pages,
            )?;
            copy_segment(path, segment, physical_memory_offset, &pages)?;
            ranges.push(UserRange {
                start: segment.virtual_address,
                end,
                readable: segment.readable,
                writable: segment.writable,
                executable: segment.executable,
            });
        }

        let stack_start = USER_STACK_TOP
            .checked_sub(USER_STACK_SIZE as u64)
            .ok_or(Error::AddressOverflow)?;
        let guard_page_address = stack_start
            .checked_sub(USER_STACK_GUARD_SIZE as u64)
            .ok_or(Error::AddressOverflow)?;
        validate_user_range(stack_start, USER_STACK_TOP)?;
        map_range(
            stack_start,
            USER_STACK_TOP,
            PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::USER_ACCESSIBLE
                | PageTableFlags::NO_EXECUTE,
            &mut mapper,
            frame_allocator,
            physical_memory_offset,
            &mut pages,
        )?;
        ranges.push(UserRange {
            start: stack_start,
            end: USER_STACK_TOP,
            readable: true,
            writable: true,
            executable: false,
        });

        if !ranges.iter().any(|range| {
            range.executable && image.entry_point >= range.start && image.entry_point < range.end
        }) {
            return Err(Error::InvalidUserRange);
        }

        let stack_pointer =
            build_initial_stack(arguments, physical_memory_offset, &pages, stack_start)?;

        Ok(Self {
            page_table_frame,
            entry_point: image.entry_point,
            stack_pointer,
            guard_page_address,
            pages,
            ranges,
            owned_frames: Vec::new(),
        })
    }
}

pub fn spawn(
    path: &str,
    task_name: &'static str,
    image: &Image,
    kernel_mapper: &mut OffsetPageTable<'_>,
    frame_allocator: &mut BootInfoFrameAllocator,
    physical_memory_offset: VirtAddr,
) -> Result<SpawnInfo, Error> {
    spawn_with_args(
        path,
        task_name,
        image,
        &[path],
        kernel_mapper,
        frame_allocator,
        physical_memory_offset,
    )
}

pub fn spawn_with_args(
    path: &str,
    task_name: &'static str,
    image: &Image,
    arguments: &[&str],
    kernel_mapper: &mut OffsetPageTable<'_>,
    frame_allocator: &mut BootInfoFrameAllocator,
    physical_memory_offset: VirtAddr,
) -> Result<SpawnInfo, Error> {
    spawn_with_mode(
        path,
        task_name,
        image,
        arguments,
        false,
        None,
        None,
        None,
        None,
        None,
        kernel_mapper,
        frame_allocator,
        physical_memory_offset,
    )
}

fn spawn_with_mode(
    path: &str,
    task_name: &'static str,
    image: &Image,
    arguments: &[&str],
    foreground: bool,
    stdin_pipe: Option<PipeId>,
    stdout_pipe: Option<PipeId>,
    parent_process_id: Option<u64>,
    terminal_parent: Option<u64>,
    process_group_id: Option<u64>,
    kernel_mapper: &mut OffsetPageTable<'_>,
    frame_allocator: &mut BootInfoFrameAllocator,
    physical_memory_offset: VirtAddr,
) -> Result<SpawnInfo, Error> {
    if scheduler::snapshot().current_task_kind != scheduler::TaskKind::Bootstrap {
        return Err(Error::SchedulerNotOnBootstrapTask);
    }

    let mut kernel_stack = vec![0_u128; KERNEL_TRANSITION_STACK_WORDS].into_boxed_slice();
    let kernel_stack_start = kernel_stack.as_mut_ptr() as usize;
    let kernel_stack_bytes = kernel_stack
        .len()
        .checked_mul(size_of::<u128>())
        .ok_or(Error::StackLayoutInvalid)?;
    let kernel_stack_top = kernel_stack_start
        .checked_add(kernel_stack_bytes)
        .ok_or(Error::StackLayoutInvalid)?;
    let initial_stack_pointer = kernel_stack_top
        .checked_sub(size_of::<SavedContext>())
        .ok_or(Error::StackLayoutInvalid)?;
    if kernel_stack_start % align_of::<u128>() != 0
        || kernel_stack_top % 16 != 0
        || initial_stack_pointer % 16 != 0
    {
        return Err(Error::StackLayoutInvalid);
    }

    for address in [
        galactic_syscall_interrupt_entry as usize as u64,
        galactic_page_fault_interrupt_entry as usize as u64,
        galactic_general_protection_interrupt_entry as usize as u64,
        scheduler::timer_interrupt_entry_address().as_u64(),
        kernel_stack_start as u64,
        physical_memory_offset.as_u64(),
    ] {
        if pml4_index(address) == 0 {
            return Err(Error::KernelMappingUsesUserSlot(address));
        }
    }

    let mut address_space = BuiltAddressSpace::build(
        path,
        image,
        arguments,
        kernel_mapper,
        frame_allocator,
        physical_memory_offset,
    )?;
    let page_table_address = address_space.page_table_frame.start_address().as_u64();
    let mapped_pages = address_space.pages.len();
    let owned_frame_count = address_space.owned_frames.len();

    let initial_context = SavedContext {
        r15: 0,
        r14: 0,
        r13: 0,
        r12: 0,
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
        rip: address_space.entry_point,
        cs: u64::from(gdt::user_code_selector()),
        rflags: USER_RFLAGS,
        stack_pointer: address_space.stack_pointer,
        stack_segment: u64::from(gdt::user_data_selector()),
    };
    unsafe { (initial_stack_pointer as *mut SavedContext).write(initial_context) };

    let process_id = PROCESS_MANAGER.lock().allocate_process_id();
    let process_group_id = process_group_id.unwrap_or(process_id);
    let mut pending_process = Some(Process {
        process_id,
        parent_process_id,
        process_group_id,
        terminal_parent,
        task_id: 0,
        path: path.to_string(),
        state: ProcessState::Runnable,
        termination: None,
        page_table_address,
        entry_point: address_space.entry_point,
        mapped_pages,
        load_segments: image.load_segments().len(),
        guard_page_address: address_space.guard_page_address,
        ranges: core::mem::take(&mut address_space.ranges),
        pages: core::mem::take(&mut address_space.pages),
        kernel_stack,
        owned_frames: core::mem::take(&mut address_space.owned_frames),
        open_files: Vec::new(),
        pipe_descriptors: Vec::new(),
        stdin_pipe,
        stdout_pipe,
        pending_terminal_read: None,
        pending_pipe_read: None,
        pending_pipe_write: None,
        pending_child_spawn: None,
        pending_child_wait: None,
        syscall_count: 0,
        write_count: 0,
        yield_count: 0,
        bytes_written: 0,
        open_count: 0,
        read_count: 0,
        close_count: 0,
        bytes_read: 0,
        terminal_read_count: 0,
        terminal_bytes_read: 0,
        blocked_read_count: 0,
        pipe_read_count: 0,
        pipe_write_count: 0,
        pipe_bytes_read: 0,
        pipe_bytes_written: 0,
        blocked_pipe_read_count: 0,
        blocked_pipe_write_count: 0,
        child_spawn_count: 0,
        child_wait_count: 0,
        child_poll_count: 0,
        child_poll_pending_count: 0,
        signal_sent_count: 0,
        signal_received_count: 0,
        pipe_pair_count: 0,
        pipe_descriptor_close_count: 0,
        pipe_descriptor_inherit_count: 0,
    });

    let task_result = cpu_interrupts::without_interrupts(|| -> Result<u64, Error> {
        if foreground {
            let attached = match terminal_parent {
                Some(parent_process) => terminal::transfer(parent_process, process_id),
                None => terminal::attach(process_id),
            };
            if !attached {
                return Err(Error::TerminalBusy);
            }
        }
        let task_id = scheduler::spawn_user_process(
            task_name,
            process_id,
            initial_stack_pointer,
            VirtAddr::new(kernel_stack_top as u64),
            kernel_stack_bytes,
            page_table_address,
        )?;
        let mut process = pending_process
            .take()
            .ok_or(scheduler::InitError::InvalidUserContext)?;
        process.task_id = task_id;
        let mut manager = PROCESS_MANAGER.lock();
        manager.spawned = manager.spawned.saturating_add(1);
        manager.processes.push(process);
        Ok(task_id)
    });

    let task_id = match task_result {
        Ok(task_id) => task_id,
        Err(error) => {
            if foreground {
                if let Some(parent_process) = terminal_parent {
                    let _ = terminal::transfer(process_id, parent_process);
                } else {
                    terminal::detach(process_id);
                }
            }
            if let Some(mut process) = pending_process.take() {
                for frame in process.owned_frames.drain(..) {
                    frame_allocator.deallocate_frame(frame);
                }
            }
            return Err(error);
        }
    };

    Ok(SpawnInfo {
        process_id,
        process_group_id,
        task_id,
        path: path.to_string(),
        entry_point: image.entry_point,
        page_table_address,
        mapped_pages,
        owned_frames: owned_frame_count,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryStats {
    pub usable_frames: u64,
    pub allocated_frames: u64,
    pub remaining_frames: u64,
    pub recycled_frames: usize,
    pub reclaimed_frames: u64,
    pub reused_frames: u64,
}

#[derive(Debug, Clone)]
pub struct PipelineResult {
    pub pipe_id: PipeId,
    pub producer: ProcessResult,
    pub consumer: ProcessResult,
}

pub struct Runtime {
    mapper: OffsetPageTable<'static>,
    frame_allocator: BootInfoFrameAllocator,
    physical_memory_offset: VirtAddr,
}

impl Runtime {
    pub fn new(
        mapper: OffsetPageTable<'static>,
        frame_allocator: BootInfoFrameAllocator,
        physical_memory_offset: VirtAddr,
    ) -> Self {
        Self {
            mapper,
            frame_allocator,
            physical_memory_offset,
        }
    }

    pub fn spawn(&mut self, path: &str, arguments: &[&str]) -> Result<SpawnInfo, Error> {
        self.spawn_mode(path, arguments, false)
    }

    pub fn spawn_foreground(&mut self, path: &str, arguments: &[&str]) -> Result<SpawnInfo, Error> {
        self.spawn_mode(path, arguments, true)
    }

    fn spawn_mode(
        &mut self,
        path: &str,
        arguments: &[&str],
        foreground: bool,
    ) -> Result<SpawnInfo, Error> {
        let image = elf::validate(path)?;
        let mut argv = Vec::with_capacity(arguments.len().saturating_add(1));
        argv.push(path);
        argv.extend_from_slice(arguments);
        spawn_with_mode(
            path,
            SHELL_PROCESS_TASK_NAME,
            &image,
            &argv,
            foreground,
            None,
            None,
            None,
            None,
            None,
            &mut self.mapper,
            &mut self.frame_allocator,
            self.physical_memory_offset,
        )
    }

    fn spawn_streams(
        &mut self,
        path: &str,
        arguments: &[&str],
        stdin_pipe: Option<PipeId>,
        stdout_pipe: Option<PipeId>,
    ) -> Result<SpawnInfo, Error> {
        let image = elf::validate(path)?;
        let mut argv = Vec::with_capacity(arguments.len().saturating_add(1));
        argv.push(path);
        argv.extend_from_slice(arguments);
        spawn_with_mode(
            path,
            SHELL_PROCESS_TASK_NAME,
            &image,
            &argv,
            false,
            stdin_pipe,
            stdout_pipe,
            None,
            None,
            None,
            &mut self.mapper,
            &mut self.frame_allocator,
            self.physical_memory_offset,
        )
    }

    pub fn pipeline(
        &mut self,
        producer_path: &str,
        producer_arguments: &[&str],
        consumer_path: &str,
        consumer_arguments: &[&str],
    ) -> Result<PipelineResult, Error> {
        let pipe_id = pipe::create_pair()?;
        let consumer =
            match self.spawn_streams(consumer_path, consumer_arguments, Some(pipe_id), None) {
                Ok(info) => info,
                Err(error) => {
                    let _ = pipe::discard_pair(pipe_id);
                    return Err(error);
                }
            };
        let producer =
            match self.spawn_streams(producer_path, producer_arguments, None, Some(pipe_id)) {
                Ok(info) => info,
                Err(error) => {
                    let _ = pipe::close_writer(pipe_id);
                    let _ = self.wait(consumer.process_id);
                    return Err(error);
                }
            };

        let producer_result = self.wait(producer.process_id)?;
        let consumer_result = self.wait(consumer.process_id)?;
        Ok(PipelineResult {
            pipe_id,
            producer: producer_result,
            consumer: consumer_result,
        })
    }

    fn spawn_child(
        &mut self,
        parent_process_id: u64,
        request: &PendingChildSpawn,
    ) -> Result<SpawnInfo, Error> {
        let (stdin_pipe, stdout_pipe, process_group_id) =
            cpu_interrupts::without_interrupts(|| {
                let manager = PROCESS_MANAGER.lock();
                let parent = manager
                    .processes
                    .iter()
                    .find(|process| process.process_id == parent_process_id)
                    .ok_or(Error::ProcessNotFound(parent_process_id))?;
                let stdin_pipe = resolve_pipe_descriptor(
                    parent,
                    request.stdin_descriptor,
                    PipeDirection::Reader,
                )?;
                let stdout_pipe = resolve_pipe_descriptor(
                    parent,
                    request.stdout_descriptor,
                    PipeDirection::Writer,
                )?;
                let inherited_group = parent.process_group_id;
                let process_group_id = if request.new_process_group {
                    None
                } else if let Some(group_id) = request.process_group_id {
                    let group_owned = manager.processes.iter().any(|process| {
                        process.parent_process_id == Some(parent_process_id)
                            && process.process_group_id == group_id
                            && matches!(
                                process.state,
                                ProcessState::Runnable | ProcessState::Blocked
                            )
                    });
                    if !group_owned {
                        return Err(Error::InvalidProcessGroup(group_id));
                    }
                    Some(group_id)
                } else {
                    Some(inherited_group)
                };
                Ok::<_, Error>((stdin_pipe, stdout_pipe, process_group_id))
            })?;

        if let Some(pipe_id) = stdin_pipe {
            pipe::retain_reader(pipe_id)?;
        }
        if let Some(pipe_id) = stdout_pipe {
            if let Err(error) = pipe::retain_writer(pipe_id) {
                if let Some(stdin_pipe) = stdin_pipe {
                    let _ = pipe::close_reader(stdin_pipe);
                }
                return Err(error.into());
            }
        }

        let image = match elf::validate(&request.path) {
            Ok(image) => image,
            Err(error) => {
                if let Some(pipe_id) = stdin_pipe {
                    let _ = pipe::close_reader(pipe_id);
                }
                if let Some(pipe_id) = stdout_pipe {
                    let _ = pipe::close_writer(pipe_id);
                }
                return Err(error.into());
            }
        };
        let mut argv = Vec::with_capacity(request.arguments.len().saturating_add(1));
        argv.push(request.path.as_str());
        argv.extend(request.arguments.iter().map(String::as_str));
        let result = spawn_with_mode(
            &request.path,
            SHELL_PROCESS_TASK_NAME,
            &image,
            &argv,
            request.foreground,
            stdin_pipe,
            stdout_pipe,
            Some(parent_process_id),
            request.foreground.then_some(parent_process_id),
            process_group_id,
            &mut self.mapper,
            &mut self.frame_allocator,
            self.physical_memory_offset,
        );

        if result.is_err() {
            if let Some(pipe_id) = stdin_pipe {
                let _ = pipe::close_reader(pipe_id);
            }
            if let Some(pipe_id) = stdout_pipe {
                let _ = pipe::close_writer(pipe_id);
            }
            return result;
        }

        let inherited = u64::from(stdin_pipe.is_some()) + u64::from(stdout_pipe.is_some());
        if inherited > 0 {
            cpu_interrupts::without_interrupts(|| {
                let mut manager = PROCESS_MANAGER.lock();
                let updated = if let Some(parent) = manager.process_mut(parent_process_id) {
                    parent.pipe_descriptor_inherit_count = parent
                        .pipe_descriptor_inherit_count
                        .saturating_add(inherited);
                    true
                } else {
                    false
                };
                if updated {
                    manager.pipe_descriptor_inherits =
                        manager.pipe_descriptor_inherits.saturating_add(inherited);
                }
            });
        }
        result
    }

    pub fn wait(&mut self, process_id: u64) -> Result<ProcessResult, Error> {
        loop {
            self.poll()?;
            let manager = PROCESS_MANAGER.lock();
            if let Some(result) = manager
                .completed
                .iter()
                .find(|result| result.process_id == process_id)
            {
                return Ok(result.clone());
            }
            if !manager
                .processes
                .iter()
                .any(|process| process.process_id == process_id)
            {
                return Err(Error::ProcessNotFound(process_id));
            }
            drop(manager);
            hlt();
        }
    }

    pub fn run(&mut self, path: &str, arguments: &[&str]) -> Result<ProcessResult, Error> {
        let spawned = self.spawn_foreground(path, arguments)?;
        self.wait(spawned.process_id)
    }

    pub fn poll(&mut self) -> Result<usize, Error> {
        terminal::poll_keyboard();
        service_terminal_interrupt();
        let reaped = reap(&mut self.frame_allocator)?;
        service_terminal_reads(self.physical_memory_offset)?;
        service_pipe_waiters(self.physical_memory_offset)?;
        self.service_child_requests()?;
        Ok(reaped)
    }

    fn service_child_requests(&mut self) -> Result<usize, Error> {
        let spawn_requests: Vec<(u64, PendingChildSpawn)> =
            cpu_interrupts::without_interrupts(|| {
                PROCESS_MANAGER
                    .lock()
                    .processes
                    .iter()
                    .filter_map(|process| {
                        process
                            .pending_child_spawn
                            .clone()
                            .map(|request| (process.process_id, request))
                    })
                    .collect()
            });
        let mut completed = 0usize;
        for (parent_process_id, request) in spawn_requests {
            let result = self.spawn_child(parent_process_id, &request);
            let return_value = match result {
                Ok(info) => info.process_id,
                Err(error) => error_return(process_error_number(&error)),
            };
            cpu_interrupts::without_interrupts(|| -> Result<(), Error> {
                let mut manager = PROCESS_MANAGER.lock();
                let process = manager
                    .process_mut(parent_process_id)
                    .ok_or(Error::ProcessNotFound(parent_process_id))?;
                let registers = unsafe { &mut *(request.stack_pointer as *mut SavedRegisters) };
                registers.rax = return_value;
                process.pending_child_spawn = None;
                process.state = ProcessState::Runnable;
                if (return_value as i64) >= 0 {
                    process.child_spawn_count = process.child_spawn_count.saturating_add(1);
                    manager.child_spawns = manager.child_spawns.saturating_add(1);
                }
                drop(manager);
                if !scheduler::wake_process(parent_process_id) {
                    return Err(Error::ProcessNotFound(parent_process_id));
                }
                Ok(())
            })?;
            completed = completed.saturating_add(1);
        }

        let wait_requests: Vec<(u64, PendingChildWait)> =
            cpu_interrupts::without_interrupts(|| {
                PROCESS_MANAGER
                    .lock()
                    .processes
                    .iter()
                    .filter_map(|process| {
                        process
                            .pending_child_wait
                            .map(|request| (process.process_id, request))
                    })
                    .collect()
            });
        for (parent_process_id, request) in wait_requests {
            let result = cpu_interrupts::without_interrupts(|| {
                PROCESS_MANAGER
                    .lock()
                    .completed
                    .iter()
                    .find(|result| {
                        result.process_id == request.child_process_id
                            && result.parent_process_id == Some(parent_process_id)
                    })
                    .cloned()
            });
            let Some(result) = result else {
                continue;
            };
            cpu_interrupts::without_interrupts(|| -> Result<(), Error> {
                let mut manager = PROCESS_MANAGER.lock();
                let process = manager
                    .process_mut(parent_process_id)
                    .ok_or(Error::ProcessNotFound(parent_process_id))?;
                let registers = unsafe { &mut *(request.stack_pointer as *mut SavedRegisters) };
                registers.rax = child_status(&result);
                process.pending_child_wait = None;
                process.state = ProcessState::Runnable;
                process.child_wait_count = process.child_wait_count.saturating_add(1);
                manager.child_waits = manager.child_waits.saturating_add(1);
                drop(manager);
                if !scheduler::wake_process(parent_process_id) {
                    return Err(Error::ProcessNotFound(parent_process_id));
                }
                Ok(())
            })?;
            completed = completed.saturating_add(1);
        }
        Ok(completed)
    }

    pub fn terminal_active(&self) -> bool {
        terminal::foreground_process().is_some()
    }

    pub fn handle_terminal_key(&mut self, key: pc_keyboard::DecodedKey) -> Result<bool, Error> {
        let handled = terminal::handle_key(key);
        let signaled = service_terminal_interrupt();
        if handled && signaled == 0 {
            service_terminal_reads(self.physical_memory_offset)?;
        }
        Ok(handled)
    }

    pub fn reap(&mut self) -> Result<usize, Error> {
        self.poll()
    }

    pub fn wait_until_blocked(&mut self, process_id: u64) -> Result<(), Error> {
        loop {
            self.poll()?;
            if scheduler::is_process_blocked(process_id) {
                return Ok(());
            }
            let manager = PROCESS_MANAGER.lock();
            if !manager
                .processes
                .iter()
                .any(|process| process.process_id == process_id)
            {
                return Err(Error::ProcessNotFound(process_id));
            }
            drop(manager);
            hlt();
        }
    }

    pub fn wait_until_terminal_read(&mut self, process_id: u64) -> Result<(), Error> {
        loop {
            self.poll()?;
            let ready = cpu_interrupts::without_interrupts(|| {
                PROCESS_MANAGER
                    .lock()
                    .processes
                    .iter()
                    .find(|process| process.process_id == process_id)
                    .is_some_and(|process| {
                        process.pending_terminal_read.is_some()
                            && process.state == ProcessState::Blocked
                    })
            });
            if ready && terminal::is_foreground(process_id) {
                return Ok(());
            }
            hlt();
        }
    }

    pub fn wait_for_child_path(
        &mut self,
        parent_process_id: u64,
        path: &str,
    ) -> Result<ProcessResult, Error> {
        loop {
            self.poll()?;
            if let Some(result) = cpu_interrupts::without_interrupts(|| {
                PROCESS_MANAGER
                    .lock()
                    .completed
                    .iter()
                    .find(|result| {
                        result.parent_process_id == Some(parent_process_id) && result.path == path
                    })
                    .cloned()
            }) {
                return Ok(result);
            }
            hlt();
        }
    }

    pub fn child_is_active(&mut self, parent_process_id: u64, path: &str) -> Result<bool, Error> {
        self.poll()?;
        Ok(cpu_interrupts::without_interrupts(|| {
            PROCESS_MANAGER.lock().processes.iter().any(|process| {
                process.parent_process_id == Some(parent_process_id) && process.path == path
            })
        }))
    }

    pub fn wait_until_child_group(
        &mut self,
        parent_process_id: u64,
        path: &str,
        minimum_members: usize,
    ) -> Result<ProcessGroupInfo, Error> {
        loop {
            self.poll()?;
            if let Some(info) = active_child_group(parent_process_id, path, minimum_members, false)
            {
                return Ok(info);
            }
            hlt();
        }
    }

    pub fn wait_until_foreground_child_group(
        &mut self,
        parent_process_id: u64,
        path: &str,
        minimum_members: usize,
    ) -> Result<ProcessGroupInfo, Error> {
        loop {
            self.poll()?;
            if let Some(info) = active_child_group(parent_process_id, path, minimum_members, true) {
                return Ok(info);
            }
            hlt();
        }
    }

    pub fn wait_for_child_group(
        &mut self,
        parent_process_id: u64,
        process_group_id: u64,
    ) -> Result<Vec<ProcessResult>, Error> {
        loop {
            self.poll()?;
            let (active, results) = cpu_interrupts::without_interrupts(|| {
                let manager = PROCESS_MANAGER.lock();
                let active = manager.processes.iter().any(|process| {
                    process.parent_process_id == Some(parent_process_id)
                        && process.process_group_id == process_group_id
                });
                let results = manager
                    .completed
                    .iter()
                    .filter(|result| {
                        result.parent_process_id == Some(parent_process_id)
                            && result.process_group_id == process_group_id
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                (active, results)
            });
            if !active && !results.is_empty() {
                return Ok(results);
            }
            hlt();
        }
    }

    pub fn inject_terminal_interrupt(&mut self) -> Result<usize, Error> {
        terminal::handle_key(pc_keyboard::DecodedKey::Unicode('\u{3}'));
        Ok(service_terminal_interrupt())
    }

    pub fn inject_terminal_line(&mut self, line: &str) -> Result<usize, Error> {
        terminal::inject_line(line);
        service_terminal_reads(self.physical_memory_offset)
    }

    pub fn terminal_snapshot(&self) -> TerminalSnapshot {
        terminal::snapshot()
    }

    pub fn pipe_snapshot(&self) -> PipeSnapshot {
        pipe::snapshot()
    }

    pub fn memory_stats(&self) -> MemoryStats {
        MemoryStats {
            usable_frames: self.frame_allocator.usable_frame_count(),
            allocated_frames: self.frame_allocator.allocated_frame_count(),
            remaining_frames: self.frame_allocator.remaining_frame_count(),
            recycled_frames: self.frame_allocator.recycled_frame_count(),
            reclaimed_frames: self.frame_allocator.reclaimed_frame_count(),
            reused_frames: self.frame_allocator.reused_frame_count(),
        }
    }
}

pub fn wait_for_all(
    frame_allocator: &mut BootInfoFrameAllocator,
    expected_processes: usize,
) -> ManagerSnapshot {
    loop {
        let _ = reap(frame_allocator);
        let snapshot = snapshot();
        if snapshot.results.len() >= expected_processes && snapshot.active == 0 {
            return snapshot;
        }
        hlt();
    }
}

pub fn wait_for(
    frame_allocator: &mut BootInfoFrameAllocator,
    process_id: u64,
) -> Result<ProcessResult, Error> {
    loop {
        let _ = reap(frame_allocator)?;
        let manager = PROCESS_MANAGER.lock();
        if let Some(result) = manager
            .completed
            .iter()
            .find(|result| result.process_id == process_id)
        {
            return Ok(result.clone());
        }
        if !manager
            .processes
            .iter()
            .any(|process| process.process_id == process_id)
        {
            return Err(Error::ProcessNotFound(process_id));
        }
        drop(manager);
        hlt();
    }
}

pub fn reap(frame_allocator: &mut BootInfoFrameAllocator) -> Result<usize, Error> {
    let process_ids = scheduler::reap_zombie_processes();
    let mut reaped = 0usize;

    for task in process_ids {
        let process_id = task.process_id;
        let mut process = cpu_interrupts::without_interrupts(|| {
            PROCESS_MANAGER.lock().remove_process(process_id)
        })
        .ok_or(Error::ProcessNotFound(process_id))?;
        let terminal_parent = process.terminal_parent;
        terminal::detach(process_id);
        if let Some(parent_process_id) = terminal_parent {
            let parent_exists = PROCESS_MANAGER
                .lock()
                .processes
                .iter()
                .any(|candidate| candidate.process_id == parent_process_id);
            if parent_exists {
                let _ = terminal::attach(parent_process_id);
            }
        }
        if let Some(pipe_id) = process.stdin_pipe.take() {
            let _ = pipe::close_reader(pipe_id);
        }
        if let Some(pipe_id) = process.stdout_pipe.take() {
            let _ = pipe::close_writer(pipe_id);
        }
        for descriptor in process.pipe_descriptors.drain(..) {
            match descriptor.direction {
                PipeDirection::Reader => {
                    let _ = pipe::close_reader(descriptor.pipe_id);
                }
                PipeDirection::Writer => {
                    let _ = pipe::close_writer(descriptor.pipe_id);
                }
            }
        }
        let frames_reclaimed = process.owned_frames.len();
        for frame in process.owned_frames.drain(..) {
            frame_allocator.deallocate_frame(frame);
        }
        let result = process.result(frames_reclaimed, task.scheduled_count, task.runtime_ticks)?;
        cpu_interrupts::without_interrupts(|| {
            let mut manager = PROCESS_MANAGER.lock();
            manager.reaped = manager.reaped.saturating_add(1);
            manager.frames_reclaimed = manager
                .frames_reclaimed
                .saturating_add(frames_reclaimed as u64);
            manager.completed.push(result);
        });
        reaped = reaped.saturating_add(1);
    }

    Ok(reaped)
}

pub fn snapshot() -> ManagerSnapshot {
    cpu_interrupts::without_interrupts(|| PROCESS_MANAGER.lock().snapshot())
}

pub fn last_result() -> Option<ProcessResult> {
    PROCESS_MANAGER.lock().completed.last().cloned()
}

pub fn syscall_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(galactic_syscall_interrupt_entry as usize as u64)
}

pub fn page_fault_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(galactic_page_fault_interrupt_entry as usize as u64)
}

pub fn general_protection_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(galactic_general_protection_interrupt_entry as usize as u64)
}

#[unsafe(no_mangle)]
pub extern "C" fn galactic_syscall_dispatch(current_stack_pointer: usize) -> usize {
    let registers = unsafe { &mut *(current_stack_pointer as *mut SavedRegisters) };
    let Some(process_id) = scheduler::current_process_id() else {
        registers.rax = error_return(ERR_NOT_IMPLEMENTED);
        return current_stack_pointer;
    };

    {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            registers.rax = error_return(ERR_NOT_IMPLEMENTED);
            return current_stack_pointer;
        };
        process.syscall_count = process.syscall_count.saturating_add(1);
    }

    match registers.rax {
        SYSCALL_WRITE => match syscall_write(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            current_stack_pointer,
        ) {
            WriteOutcome::Ready(result) => {
                registers.rax = result;
                current_stack_pointer
            }
            WriteOutcome::Blocked => scheduler::block_current(current_stack_pointer),
        },
        SYSCALL_YIELD => {
            {
                let mut manager = PROCESS_MANAGER.lock();
                if let Some(process) = manager.process_mut(process_id) {
                    process.yield_count = process.yield_count.saturating_add(1);
                }
            }
            registers.rax = 0;
            scheduler::on_yield(current_stack_pointer)
        }
        SYSCALL_OPEN => {
            registers.rax = syscall_open(process_id, registers.rdi, registers.rsi, registers.rdx);
            current_stack_pointer
        }
        SYSCALL_READ => match syscall_read(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            current_stack_pointer,
        ) {
            ReadOutcome::Ready(result) => {
                registers.rax = result;
                current_stack_pointer
            }
            ReadOutcome::Blocked => scheduler::block_current(current_stack_pointer),
        },
        SYSCALL_CLOSE => {
            registers.rax = syscall_close(process_id, registers.rdi);
            current_stack_pointer
        }
        SYSCALL_SPAWN_COMMAND => match syscall_spawn_command(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
            registers.r8,
            registers.rbx,
            current_stack_pointer,
        ) {
            ControlOutcome::Ready(result) => {
                registers.rax = result;
                current_stack_pointer
            }
            ControlOutcome::Blocked => scheduler::block_current(current_stack_pointer),
        },
        SYSCALL_WAIT_CHILD => {
            match syscall_wait_child(process_id, registers.rdi, current_stack_pointer) {
                ControlOutcome::Ready(result) => {
                    registers.rax = result;
                    current_stack_pointer
                }
                ControlOutcome::Blocked => scheduler::block_current(current_stack_pointer),
            }
        }
        SYSCALL_GETPID => {
            registers.rax = process_id;
            current_stack_pointer
        }
        SYSCALL_PIPE_PAIR => {
            registers.rax = syscall_pipe_pair(process_id);
            current_stack_pointer
        }
        SYSCALL_TRY_WAIT_CHILD => {
            registers.rax = syscall_try_wait_child(process_id, registers.rdi);
            current_stack_pointer
        }
        SYSCALL_SIGNAL_PROCESS_GROUP => {
            registers.rax = syscall_signal_process_group(process_id, registers.rdi, registers.rsi);
            current_stack_pointer
        }
        SYSCALL_EXIT => {
            let exit_code = registers.rdi;
            {
                let mut manager = PROCESS_MANAGER.lock();
                let marked = if let Some(process) = manager.process_mut(process_id) {
                    process.state = ProcessState::Exited;
                    process.termination = Some(TerminationReason::Exit(exit_code));
                    true
                } else {
                    false
                };
                if marked {
                    manager.exited = manager.exited.saturating_add(1);
                }
            }
            crate::serial_println!(
                "userspace process exited: pid={}, path={}, exit_code={}",
                process_id,
                process_path(process_id),
                exit_code
            );
            scheduler::terminate_current(current_stack_pointer)
        }
        _ => {
            registers.rax = error_return(ERR_NOT_IMPLEMENTED);
            current_stack_pointer
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn galactic_user_fault_dispatch(current_stack_pointer: usize, vector: u64) -> usize {
    let frame = unsafe { &*(current_stack_pointer as *const FaultStack) };
    if frame.cs & 3 != 3 {
        let address = if vector == PAGE_FAULT_VECTOR {
            Cr2::read().map(|address| address.as_u64()).unwrap_or(0)
        } else {
            0
        };
        crate::serial_println!("KERNEL EXCEPTION: vector={vector}");
        crate::serial_println!("Address: {address:#018x}");
        crate::serial_println!("Error code: {:#x}", frame.error_code);
        crate::serial_println!("Instruction pointer: {:#018x}", frame.rip);
        crate::hlt_loop();
    }

    let Some(process_id) = scheduler::current_process_id() else {
        crate::serial_println!("user exception arrived without an active process task");
        crate::hlt_loop();
    };
    let fault = FaultInfo {
        vector,
        error_code: frame.error_code,
        address: if vector == PAGE_FAULT_VECTOR {
            Cr2::read().map(|address| address.as_u64()).unwrap_or(0)
        } else {
            0
        },
        instruction_pointer: frame.rip,
    };
    let path = {
        let mut manager = PROCESS_MANAGER.lock();
        let path = {
            let Some(process) = manager.process_mut(process_id) else {
                crate::serial_println!("faulted userspace process {process_id} was not registered");
                crate::hlt_loop();
            };
            process.state = ProcessState::Faulted;
            process.termination = Some(TerminationReason::Fault(fault));
            process.path.clone()
        };
        manager.faulted = manager.faulted.saturating_add(1);
        path
    };
    crate::serial_println!(
        "userspace fault isolated: pid={}, path={}, vector={}, error={:#x}, address={:#018x}, rip={:#018x}",
        process_id,
        path,
        fault.vector,
        fault.error_code,
        fault.address,
        fault.instruction_pointer
    );
    scheduler::terminate_current(current_stack_pointer)
}

#[repr(C)]
struct SavedRegisters {
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

#[repr(C)]
struct FaultStack {
    registers: SavedRegisters,
    error_code: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    stack_pointer: u64,
    stack_segment: u64,
}

fn process_path(process_id: u64) -> String {
    PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == process_id)
        .map(|process| process.path.clone())
        .unwrap_or_else(|| String::from("<unknown>"))
}

enum ControlOutcome {
    Ready(u64),
    Blocked,
}

fn syscall_spawn_command(
    process_id: u64,
    address: u64,
    length: u64,
    flags: u64,
    stdin_descriptor: u64,
    stdout_descriptor: u64,
    process_group_argument: u64,
    current_stack_pointer: usize,
) -> ControlOutcome {
    let allowed_flags = SPAWN_FOREGROUND
        | SPAWN_USE_DESCRIPTORS
        | SPAWN_NEW_PROCESS_GROUP
        | SPAWN_JOIN_PROCESS_GROUP;
    if flags & !allowed_flags != 0 {
        return ControlOutcome::Ready(error_return(ERR_INVALID_ARGUMENT));
    }
    let new_process_group = flags & SPAWN_NEW_PROCESS_GROUP != 0;
    let join_process_group = flags & SPAWN_JOIN_PROCESS_GROUP != 0;
    if new_process_group && join_process_group {
        return ControlOutcome::Ready(error_return(ERR_INVALID_ARGUMENT));
    }
    let process_group_id = if join_process_group {
        if process_group_argument == DEFAULT_PROCESS_GROUP {
            return ControlOutcome::Ready(error_return(ERR_INVALID_ARGUMENT));
        }
        Some(process_group_argument)
    } else {
        None
    };
    let command = match user_text(process_id, address, length, MAX_COMMAND_BYTES) {
        Ok(command) => command,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    let (path, arguments) = match parse_command_line(&command) {
        Ok(command) => command,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    let use_descriptors = flags & SPAWN_USE_DESCRIPTORS != 0;
    let stdin_descriptor =
        (use_descriptors && stdin_descriptor != DEFAULT_DESCRIPTOR).then_some(stdin_descriptor);
    let stdout_descriptor =
        (use_descriptors && stdout_descriptor != DEFAULT_DESCRIPTOR).then_some(stdout_descriptor);

    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return ControlOutcome::Ready(error_return(ERR_NO_CHILD));
    };
    if resolve_pipe_descriptor(process, stdin_descriptor, PipeDirection::Reader).is_err()
        || resolve_pipe_descriptor(process, stdout_descriptor, PipeDirection::Writer).is_err()
    {
        return ControlOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
    }
    if process.pending_child_spawn.is_some() || process.pending_child_wait.is_some() {
        return ControlOutcome::Ready(error_return(ERR_IO));
    }
    process.pending_child_spawn = Some(PendingChildSpawn {
        path,
        arguments,
        foreground: flags & SPAWN_FOREGROUND != 0,
        stdin_descriptor,
        stdout_descriptor,
        new_process_group,
        process_group_id,
        stack_pointer: current_stack_pointer,
    });
    process.state = ProcessState::Blocked;
    ControlOutcome::Blocked
}

fn syscall_pipe_pair(process_id: u64) -> u64 {
    let descriptor_pair = {
        let manager = PROCESS_MANAGER.lock();
        let Some(process) = manager
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
        else {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        };
        if descriptor_count(process).saturating_add(2) > MAX_OPEN_FILES {
            return error_return(ERR_TOO_MANY_OPEN_FILES);
        }
        let Some(pair) = allocate_descriptor_pair(process) else {
            return error_return(ERR_TOO_MANY_OPEN_FILES);
        };
        pair
    };

    let pipe_id = match pipe::create_pair() {
        Ok(pipe_id) => pipe_id,
        Err(_) => return error_return(ERR_IO),
    };
    let (reader_descriptor, writer_descriptor) = descriptor_pair;
    let inserted = {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        };
        if descriptor_in_use(process, reader_descriptor)
            || descriptor_in_use(process, writer_descriptor)
        {
            false
        } else {
            process.pipe_descriptors.push(PipeDescriptor {
                descriptor: reader_descriptor,
                pipe_id,
                direction: PipeDirection::Reader,
            });
            process.pipe_descriptors.push(PipeDescriptor {
                descriptor: writer_descriptor,
                pipe_id,
                direction: PipeDirection::Writer,
            });
            process.pipe_pair_count = process.pipe_pair_count.saturating_add(1);
            true
        }
    };
    if !inserted {
        let _ = pipe::discard_pair(pipe_id);
        return error_return(ERR_TOO_MANY_OPEN_FILES);
    }
    let mut manager = PROCESS_MANAGER.lock();
    manager.pipe_pairs = manager.pipe_pairs.saturating_add(1);
    drop(manager);
    reader_descriptor | (writer_descriptor << 32)
}

fn syscall_wait_child(
    process_id: u64,
    child_process_id: u64,
    current_stack_pointer: usize,
) -> ControlOutcome {
    let mut manager = PROCESS_MANAGER.lock();
    if let Some(result) = manager
        .completed
        .iter()
        .find(|result| {
            result.process_id == child_process_id && result.parent_process_id == Some(process_id)
        })
        .cloned()
    {
        if let Some(process) = manager.process_mut(process_id) {
            process.child_wait_count = process.child_wait_count.saturating_add(1);
        }
        manager.child_waits = manager.child_waits.saturating_add(1);
        return ControlOutcome::Ready(child_status(&result));
    }
    let child_exists = manager.processes.iter().any(|child| {
        child.process_id == child_process_id && child.parent_process_id == Some(process_id)
    });
    if !child_exists {
        return ControlOutcome::Ready(error_return(ERR_NO_CHILD));
    }
    let Some(process) = manager.process_mut(process_id) else {
        return ControlOutcome::Ready(error_return(ERR_NO_CHILD));
    };
    if process.pending_child_spawn.is_some() || process.pending_child_wait.is_some() {
        return ControlOutcome::Ready(error_return(ERR_IO));
    }
    process.pending_child_wait = Some(PendingChildWait {
        child_process_id,
        stack_pointer: current_stack_pointer,
    });
    process.state = ProcessState::Blocked;
    ControlOutcome::Blocked
}

fn syscall_try_wait_child(process_id: u64, child_process_id: u64) -> u64 {
    let mut manager = PROCESS_MANAGER.lock();
    let completed = manager
        .completed
        .iter()
        .find(|result| {
            result.process_id == child_process_id && result.parent_process_id == Some(process_id)
        })
        .cloned();
    let active = manager.processes.iter().any(|child| {
        child.process_id == child_process_id && child.parent_process_id == Some(process_id)
    });
    if completed.is_none() && !active {
        return error_return(ERR_NO_CHILD);
    }

    let pending = completed.is_none();
    let Some(process) = manager.process_mut(process_id) else {
        return error_return(ERR_NO_CHILD);
    };
    process.child_poll_count = process.child_poll_count.saturating_add(1);
    if pending {
        process.child_poll_pending_count = process.child_poll_pending_count.saturating_add(1);
    }

    completed
        .as_ref()
        .map(child_status)
        .unwrap_or_else(|| error_return(ERR_TRY_AGAIN))
}

fn syscall_signal_process_group(process_id: u64, process_group_id: u64, signal: u64) -> u64 {
    match deliver_signal_group(Some(process_id), process_group_id, signal) {
        Ok(count) => count as u64,
        Err(error) => error_return(error),
    }
}

fn deliver_signal_group(
    owner_process_id: Option<u64>,
    process_group_id: u64,
    signal: u64,
) -> Result<usize, i64> {
    if signal != SIGNAL_INTERRUPT && signal != SIGNAL_TERMINATE {
        return Err(ERR_INVALID_ARGUMENT);
    }
    let target_process_ids = {
        let manager = PROCESS_MANAGER.lock();
        if let Some(owner_process_id) = owner_process_id {
            let owned = manager.processes.iter().any(|process| {
                process.parent_process_id == Some(owner_process_id)
                    && process.process_group_id == process_group_id
                    && matches!(
                        process.state,
                        ProcessState::Runnable | ProcessState::Blocked
                    )
            });
            if !owned {
                return Err(ERR_NO_CHILD);
            }
        }
        manager
            .processes
            .iter()
            .filter(|process| {
                process.process_group_id == process_group_id
                    && matches!(
                        process.state,
                        ProcessState::Runnable | ProcessState::Blocked
                    )
            })
            .map(|process| process.process_id)
            .collect::<Vec<_>>()
    };
    if target_process_ids.is_empty() {
        return Err(ERR_NO_PROCESS);
    }

    let mut terminated = Vec::new();
    for target_process_id in target_process_ids {
        if scheduler::terminate_process(target_process_id) {
            terminated.push(target_process_id);
        }
    }
    if terminated.is_empty() {
        return Err(ERR_NO_PROCESS);
    }

    let count = terminated.len();
    let mut manager = PROCESS_MANAGER.lock();
    for target_process_id in &terminated {
        if let Some(process) = manager.process_mut(*target_process_id) {
            process.state = ProcessState::Signaled;
            process.termination = Some(TerminationReason::Signal(signal));
            process.signal_received_count = process.signal_received_count.saturating_add(1);
        }
    }
    if let Some(owner_process_id) = owner_process_id {
        if let Some(owner) = manager.process_mut(owner_process_id) {
            owner.signal_sent_count = owner.signal_sent_count.saturating_add(count as u64);
        }
    }
    manager.signals_sent = manager.signals_sent.saturating_add(count as u64);
    manager.signaled = manager.signaled.saturating_add(count as u64);
    drop(manager);

    crate::serial_println!(
        "userspace process group signaled: group={}, signal={}, processes={}",
        process_group_id,
        signal,
        count
    );
    Ok(count)
}

fn service_terminal_interrupt() -> usize {
    let Some(foreground_process_id) = terminal::take_interrupt() else {
        return 0;
    };
    let target = PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == foreground_process_id)
        .map(|process| (process.process_group_id, process.path == "/ush"));
    let Some((process_group_id, is_shell)) = target else {
        return 0;
    };
    if is_shell {
        return 0;
    }
    deliver_signal_group(None, process_group_id, SIGNAL_INTERRUPT).unwrap_or(0)
}

fn active_child_group(
    parent_process_id: u64,
    path: &str,
    minimum_members: usize,
    require_foreground: bool,
) -> Option<ProcessGroupInfo> {
    let foreground_process = if require_foreground {
        terminal::foreground_process()
    } else {
        None
    };
    let manager = PROCESS_MANAGER.lock();
    let anchor = manager.processes.iter().find(|process| {
        process.parent_process_id == Some(parent_process_id)
            && process.path == path
            && matches!(
                process.state,
                ProcessState::Runnable | ProcessState::Blocked
            )
            && foreground_process.map_or(true, |foreground| process.process_id == foreground)
    })?;
    let process_group_id = anchor.process_group_id;
    let process_ids = manager
        .processes
        .iter()
        .filter(|process| {
            process.parent_process_id == Some(parent_process_id)
                && process.process_group_id == process_group_id
                && matches!(
                    process.state,
                    ProcessState::Runnable | ProcessState::Blocked
                )
        })
        .map(|process| process.process_id)
        .collect::<Vec<_>>();
    (process_ids.len() >= minimum_members).then_some(ProcessGroupInfo {
        process_group_id,
        process_ids,
    })
}

fn parse_command_line(command: &str) -> Result<(String, Vec<String>), i64> {
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return Err(ERR_INVALID_ARGUMENT);
    };
    let path = if program.starts_with('/') {
        String::from(program)
    } else {
        let mut path = String::from("/");
        path.push_str(program);
        path
    };
    let mut arguments = Vec::new();
    let mut argument_bytes = path.len().saturating_add(1);
    for argument in words {
        if arguments.len().saturating_add(1) >= MAX_ARGUMENTS {
            return Err(ERR_ARGUMENT_TOO_LARGE);
        }
        argument_bytes = argument_bytes.saturating_add(argument.len().saturating_add(1));
        if argument_bytes > MAX_ARGUMENT_BYTES {
            return Err(ERR_ARGUMENT_TOO_LARGE);
        }
        arguments.push(String::from(argument));
    }
    Ok((path, arguments))
}

fn child_status(result: &ProcessResult) -> u64 {
    match &result.termination {
        TerminationReason::Exit(code) => *code,
        TerminationReason::Fault(fault) => 128_u64.saturating_add(fault.vector),
        TerminationReason::Signal(signal) => 128_u64.saturating_add(*signal),
    }
}

fn process_error_number(error: &Error) -> i64 {
    match error {
        Error::Vfs(vfs::Error::NotFound) => ERR_NO_ENTRY,
        Error::InvalidArgument | Error::TooManyArguments | Error::ArgumentBytesTooLarge => {
            ERR_INVALID_ARGUMENT
        }
        Error::InvalidProcessGroup(_) => ERR_NO_PROCESS,
        Error::TerminalBusy => ERR_IO,
        _ => ERR_IO,
    }
}

enum WriteOutcome {
    Ready(u64),
    Blocked,
}

#[derive(Clone, Copy)]
enum WriteTarget {
    Console,
    Pipe(PipeId),
    Invalid,
}

fn syscall_write(
    process_id: u64,
    file_descriptor: u64,
    address: u64,
    length: u64,
    current_stack_pointer: usize,
) -> WriteOutcome {
    let Ok(length) = usize::try_from(length) else {
        return WriteOutcome::Ready(error_return(ERR_ARGUMENT_TOO_LARGE));
    };
    if length > MAX_SYSCALL_WRITE_BYTES {
        return WriteOutcome::Ready(error_return(ERR_ARGUMENT_TOO_LARGE));
    }
    if length == 0 {
        return WriteOutcome::Ready(0);
    }

    let (readable, target) = PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == process_id)
        .map(|process| {
            let readable = process
                .ranges
                .iter()
                .any(|range| range.readable && range.contains(address, length));
            let target = match file_descriptor {
                1 => process
                    .stdout_pipe
                    .map(WriteTarget::Pipe)
                    .unwrap_or(WriteTarget::Console),
                2 => WriteTarget::Console,
                descriptor if descriptor >= 3 => process
                    .pipe_descriptors
                    .iter()
                    .find(|pipe| pipe.descriptor == descriptor)
                    .map(|pipe| match pipe.direction {
                        PipeDirection::Writer => WriteTarget::Pipe(pipe.pipe_id),
                        PipeDirection::Reader => WriteTarget::Invalid,
                    })
                    .unwrap_or(WriteTarget::Invalid),
                _ => WriteTarget::Invalid,
            };
            (readable, target)
        })
        .unwrap_or((false, WriteTarget::Invalid));
    if !readable {
        return WriteOutcome::Ready(error_return(ERR_BAD_ADDRESS));
    }
    if matches!(target, WriteTarget::Invalid) {
        return WriteOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
    }

    let bytes = unsafe { slice::from_raw_parts(address as *const u8, length) };
    if let WriteTarget::Pipe(pipe_id) = target {
        return match pipe::write(pipe_id, bytes) {
            Ok(pipe::WriteOutcome::Written(count)) => {
                let mut manager = PROCESS_MANAGER.lock();
                if let Some(process) = manager.process_mut(process_id) {
                    process.write_count = process.write_count.saturating_add(1);
                    process.bytes_written = process.bytes_written.saturating_add(count as u64);
                    process.pipe_write_count = process.pipe_write_count.saturating_add(1);
                    process.pipe_bytes_written =
                        process.pipe_bytes_written.saturating_add(count as u64);
                }
                WriteOutcome::Ready(count as u64)
            }
            Ok(pipe::WriteOutcome::Full) => {
                let mut manager = PROCESS_MANAGER.lock();
                let Some(process) = manager.process_mut(process_id) else {
                    return WriteOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
                };
                process.pending_pipe_write = Some(PendingPipeWrite {
                    pipe_id,
                    address,
                    length,
                    stack_pointer: current_stack_pointer,
                });
                process.state = ProcessState::Blocked;
                process.blocked_pipe_write_count =
                    process.blocked_pipe_write_count.saturating_add(1);
                drop(manager);
                let _ = pipe::note_blocked_write(pipe_id);
                WriteOutcome::Blocked
            }
            Ok(pipe::WriteOutcome::NoReaders) => WriteOutcome::Ready(error_return(ERR_BROKEN_PIPE)),
            Err(_) => WriteOutcome::Ready(error_return(ERR_IO)),
        };
    }

    if let Ok(text) = str::from_utf8(bytes) {
        crate::print!("{text}");
        crate::serial_print!("{text}");
    } else {
        for byte in bytes.iter().copied() {
            let character = match byte {
                b'\n' | b'\r' | b'\t' => char::from(byte),
                0x20..=0x7e => char::from(byte),
                _ => '.',
            };
            crate::print!("{character}");
            crate::serial_print!("{character}");
        }
    }

    let mut manager = PROCESS_MANAGER.lock();
    if let Some(process) = manager.process_mut(process_id) {
        process.write_count = process.write_count.saturating_add(1);
        process.bytes_written = process.bytes_written.saturating_add(length as u64);
    }
    WriteOutcome::Ready(length as u64)
}

fn user_range_allows(process_id: u64, address: u64, length: usize, require_write: bool) -> bool {
    PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == process_id)
        .map(|process| {
            process.ranges.iter().any(|range| {
                range.readable
                    && (!require_write || range.writable)
                    && range.contains(address, length)
            })
        })
        .unwrap_or(false)
}

fn user_string(process_id: u64, address: u64, length: u64) -> Result<String, i64> {
    user_text(process_id, address, length, vfs::MAX_PATH_BYTES)
}

fn user_text(process_id: u64, address: u64, length: u64, maximum: usize) -> Result<String, i64> {
    let length = usize::try_from(length).map_err(|_| ERR_ARGUMENT_TOO_LARGE)?;
    if length == 0 || length > maximum {
        return Err(ERR_ARGUMENT_TOO_LARGE);
    }
    if !user_range_allows(process_id, address, length, false) {
        return Err(ERR_BAD_ADDRESS);
    }
    let bytes = unsafe { slice::from_raw_parts(address as *const u8, length) };
    if bytes.contains(&0) {
        return Err(ERR_INVALID_ARGUMENT);
    }
    let text = str::from_utf8(bytes).map_err(|_| ERR_INVALID_ARGUMENT)?;
    Ok(String::from(text))
}

fn syscall_open(process_id: u64, address: u64, length: u64, flags: u64) -> u64 {
    if flags != 0 {
        return error_return(ERR_INVALID_ARGUMENT);
    }
    let path = match user_string(process_id, address, length) {
        Ok(path) => path,
        Err(error) => return error_return(error),
    };
    let metadata = match vfs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => return error_return(vfs_errno(&error)),
    };
    if metadata.is_directory() {
        return error_return(ERR_IS_DIRECTORY);
    }

    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return error_return(ERR_BAD_FILE_DESCRIPTOR);
    };
    if descriptor_count(process) >= MAX_OPEN_FILES {
        return error_return(ERR_TOO_MANY_OPEN_FILES);
    }
    let Some(descriptor) = allocate_descriptor(process) else {
        return error_return(ERR_TOO_MANY_OPEN_FILES);
    };
    process.open_files.push(OpenFile {
        descriptor,
        path: metadata.path,
        offset: 0,
    });
    process.open_count = process.open_count.saturating_add(1);
    descriptor
}

enum ReadOutcome {
    Ready(u64),
    Blocked,
}

fn syscall_read(
    process_id: u64,
    descriptor: u64,
    address: u64,
    length: u64,
    current_stack_pointer: usize,
) -> ReadOutcome {
    let length = match usize::try_from(length) {
        Ok(length) if length <= MAX_SYSCALL_READ_BYTES => length,
        _ => return ReadOutcome::Ready(error_return(ERR_ARGUMENT_TOO_LARGE)),
    };
    if length == 0 {
        return ReadOutcome::Ready(0);
    }
    if !user_range_allows(process_id, address, length, true) {
        return ReadOutcome::Ready(error_return(ERR_BAD_ADDRESS));
    }
    if descriptor == 0 {
        let stdin_pipe = PROCESS_MANAGER
            .lock()
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
            .and_then(|process| process.stdin_pipe);
        if let Some(pipe_id) = stdin_pipe {
            return syscall_pipe_read(process_id, pipe_id, address, length, current_stack_pointer);
        }
        return syscall_terminal_read(process_id, address, length, current_stack_pointer);
    }
    if descriptor < 3 {
        return ReadOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
    }

    let pipe_descriptor = {
        let manager = PROCESS_MANAGER.lock();
        manager
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
            .and_then(|process| {
                process
                    .pipe_descriptors
                    .iter()
                    .find(|pipe| pipe.descriptor == descriptor)
                    .copied()
            })
    };
    if let Some(pipe_descriptor) = pipe_descriptor {
        if pipe_descriptor.direction != PipeDirection::Reader {
            return ReadOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
        }
        return syscall_pipe_read(
            process_id,
            pipe_descriptor.pipe_id,
            address,
            length,
            current_stack_pointer,
        );
    }

    let (path, offset) = {
        let manager = PROCESS_MANAGER.lock();
        let Some(process) = manager
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
        else {
            return ReadOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
        };
        let Some(file) = process
            .open_files
            .iter()
            .find(|file| file.descriptor == descriptor)
        else {
            return ReadOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
        };
        (file.path.clone(), file.offset)
    };

    let mut buffer = vec![0_u8; length];
    let count = match vfs::read_at(&path, offset, &mut buffer) {
        Ok(count) => count,
        Err(error) => return ReadOutcome::Ready(error_return(vfs_errno(&error))),
    };
    unsafe {
        ptr::copy_nonoverlapping(buffer.as_ptr(), address as *mut u8, count);
    }

    let mut manager = PROCESS_MANAGER.lock();
    if let Some(process) = manager.process_mut(process_id) {
        if let Some(file) = process
            .open_files
            .iter_mut()
            .find(|file| file.descriptor == descriptor)
        {
            file.offset = file.offset.saturating_add(count as u64);
        }
        process.read_count = process.read_count.saturating_add(1);
        process.bytes_read = process.bytes_read.saturating_add(count as u64);
    }
    ReadOutcome::Ready(count as u64)
}

fn syscall_pipe_read(
    process_id: u64,
    pipe_id: PipeId,
    address: u64,
    length: usize,
    current_stack_pointer: usize,
) -> ReadOutcome {
    match pipe::read(pipe_id, length) {
        Ok(pipe::ReadOutcome::Data(bytes)) => {
            unsafe {
                ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len());
            }
            let mut manager = PROCESS_MANAGER.lock();
            if let Some(process) = manager.process_mut(process_id) {
                process.pipe_read_count = process.pipe_read_count.saturating_add(1);
                process.pipe_bytes_read =
                    process.pipe_bytes_read.saturating_add(bytes.len() as u64);
            }
            ReadOutcome::Ready(bytes.len() as u64)
        }
        Ok(pipe::ReadOutcome::EndOfFile) => {
            let mut manager = PROCESS_MANAGER.lock();
            if let Some(process) = manager.process_mut(process_id) {
                process.pipe_read_count = process.pipe_read_count.saturating_add(1);
            }
            ReadOutcome::Ready(0)
        }
        Ok(pipe::ReadOutcome::Empty) => {
            let mut manager = PROCESS_MANAGER.lock();
            let Some(process) = manager.process_mut(process_id) else {
                return ReadOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
            };
            process.pending_pipe_read = Some(PendingPipeRead {
                pipe_id,
                address,
                length,
                stack_pointer: current_stack_pointer,
            });
            process.state = ProcessState::Blocked;
            process.blocked_pipe_read_count = process.blocked_pipe_read_count.saturating_add(1);
            drop(manager);
            let _ = pipe::note_blocked_read(pipe_id);
            ReadOutcome::Blocked
        }
        Err(_) => ReadOutcome::Ready(error_return(ERR_IO)),
    }
}

fn syscall_terminal_read(
    process_id: u64,
    address: u64,
    length: usize,
    current_stack_pointer: usize,
) -> ReadOutcome {
    if !terminal::is_foreground(process_id) {
        return ReadOutcome::Ready(error_return(ERR_IO));
    }

    if let Some(bytes) = terminal::take_committed(length) {
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len());
        }
        let mut manager = PROCESS_MANAGER.lock();
        if let Some(process) = manager.process_mut(process_id) {
            process.terminal_read_count = process.terminal_read_count.saturating_add(1);
            process.terminal_bytes_read = process
                .terminal_bytes_read
                .saturating_add(bytes.len() as u64);
        }
        return ReadOutcome::Ready(bytes.len() as u64);
    }

    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return ReadOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
    };
    if process.pending_terminal_read.is_some() {
        return ReadOutcome::Ready(error_return(ERR_IO));
    }
    process.pending_terminal_read = Some(PendingTerminalRead {
        address,
        length,
        stack_pointer: current_stack_pointer,
    });
    process.state = ProcessState::Blocked;
    process.blocked_read_count = process.blocked_read_count.saturating_add(1);
    drop(manager);
    terminal::note_blocked_read();
    ReadOutcome::Blocked
}

fn service_terminal_reads(physical_memory_offset: VirtAddr) -> Result<usize, Error> {
    let Some(process_id) = terminal::foreground_process() else {
        return Ok(0);
    };
    let pending = cpu_interrupts::without_interrupts(|| {
        PROCESS_MANAGER
            .lock()
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
            .and_then(|process| process.pending_terminal_read)
    });
    let Some(pending) = pending else {
        return Ok(0);
    };
    let Some(bytes) = terminal::take_committed(pending.length) else {
        return Ok(0);
    };

    cpu_interrupts::without_interrupts(|| -> Result<(), Error> {
        let mut manager = PROCESS_MANAGER.lock();
        let process = manager
            .process_mut(process_id)
            .ok_or(Error::ProcessNotFound(process_id))?;
        write_user_bytes(
            pending.address,
            &bytes,
            physical_memory_offset,
            &process.pages,
        )?;
        let registers = unsafe { &mut *(pending.stack_pointer as *mut SavedRegisters) };
        registers.rax = bytes.len() as u64;
        process.pending_terminal_read = None;
        process.state = ProcessState::Runnable;
        process.terminal_read_count = process.terminal_read_count.saturating_add(1);
        process.terminal_bytes_read = process
            .terminal_bytes_read
            .saturating_add(bytes.len() as u64);
        drop(manager);
        if !scheduler::wake_process(process_id) {
            return Err(Error::ProcessNotFound(process_id));
        }
        terminal::note_wakeup();
        Ok(())
    })?;
    Ok(bytes.len())
}

fn service_pipe_waiters(physical_memory_offset: VirtAddr) -> Result<usize, Error> {
    let pending_reads: Vec<(u64, PendingPipeRead)> = cpu_interrupts::without_interrupts(|| {
        PROCESS_MANAGER
            .lock()
            .processes
            .iter()
            .filter_map(|process| {
                process
                    .pending_pipe_read
                    .map(|pending| (process.process_id, pending))
            })
            .collect()
    });
    let pending_writes: Vec<(u64, PendingPipeWrite)> = cpu_interrupts::without_interrupts(|| {
        PROCESS_MANAGER
            .lock()
            .processes
            .iter()
            .filter_map(|process| {
                process
                    .pending_pipe_write
                    .map(|pending| (process.process_id, pending))
            })
            .collect()
    });

    let mut wakeups = 0usize;
    for (process_id, pending) in pending_reads {
        let result = match pipe::read(pending.pipe_id, pending.length) {
            Ok(pipe::ReadOutcome::Data(bytes)) => Some(bytes),
            Ok(pipe::ReadOutcome::EndOfFile) => Some(Vec::new()),
            Ok(pipe::ReadOutcome::Empty) => None,
            Err(_) => {
                complete_pipe_read_error(process_id, pending, ERR_IO)?;
                wakeups = wakeups.saturating_add(1);
                continue;
            }
        };
        let Some(bytes) = result else {
            continue;
        };
        cpu_interrupts::without_interrupts(|| -> Result<(), Error> {
            let mut manager = PROCESS_MANAGER.lock();
            let process = manager
                .process_mut(process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            if !bytes.is_empty() {
                write_user_bytes(
                    pending.address,
                    &bytes,
                    physical_memory_offset,
                    &process.pages,
                )?;
            }
            let registers = unsafe { &mut *(pending.stack_pointer as *mut SavedRegisters) };
            registers.rax = bytes.len() as u64;
            process.pending_pipe_read = None;
            process.state = ProcessState::Runnable;
            process.pipe_read_count = process.pipe_read_count.saturating_add(1);
            process.pipe_bytes_read = process.pipe_bytes_read.saturating_add(bytes.len() as u64);
            drop(manager);
            if !scheduler::wake_process(process_id) {
                return Err(Error::ProcessNotFound(process_id));
            }
            let _ = pipe::note_reader_wakeup(pending.pipe_id);
            Ok(())
        })?;
        wakeups = wakeups.saturating_add(1);
    }

    for (process_id, pending) in pending_writes {
        let bytes = cpu_interrupts::without_interrupts(|| -> Result<Vec<u8>, Error> {
            let manager = PROCESS_MANAGER.lock();
            let process = manager
                .processes
                .iter()
                .find(|process| process.process_id == process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            read_user_bytes(
                pending.address,
                pending.length,
                physical_memory_offset,
                &process.pages,
            )
        })?;
        let result = match pipe::write(pending.pipe_id, &bytes) {
            Ok(pipe::WriteOutcome::Written(count)) => Ok(count as u64),
            Ok(pipe::WriteOutcome::Full) => continue,
            Ok(pipe::WriteOutcome::NoReaders) => Err(ERR_BROKEN_PIPE),
            Err(_) => Err(ERR_IO),
        };
        cpu_interrupts::without_interrupts(|| -> Result<(), Error> {
            let mut manager = PROCESS_MANAGER.lock();
            let process = manager
                .process_mut(process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            let registers = unsafe { &mut *(pending.stack_pointer as *mut SavedRegisters) };
            match result {
                Ok(count) => {
                    registers.rax = count;
                    process.write_count = process.write_count.saturating_add(1);
                    process.bytes_written = process.bytes_written.saturating_add(count);
                    process.pipe_write_count = process.pipe_write_count.saturating_add(1);
                    process.pipe_bytes_written = process.pipe_bytes_written.saturating_add(count);
                }
                Err(error) => registers.rax = error_return(error),
            }
            process.pending_pipe_write = None;
            process.state = ProcessState::Runnable;
            drop(manager);
            if !scheduler::wake_process(process_id) {
                return Err(Error::ProcessNotFound(process_id));
            }
            let _ = pipe::note_writer_wakeup(pending.pipe_id);
            Ok(())
        })?;
        wakeups = wakeups.saturating_add(1);
    }

    Ok(wakeups)
}

fn complete_pipe_read_error(
    process_id: u64,
    pending: PendingPipeRead,
    error: i64,
) -> Result<(), Error> {
    cpu_interrupts::without_interrupts(|| -> Result<(), Error> {
        let mut manager = PROCESS_MANAGER.lock();
        let process = manager
            .process_mut(process_id)
            .ok_or(Error::ProcessNotFound(process_id))?;
        let registers = unsafe { &mut *(pending.stack_pointer as *mut SavedRegisters) };
        registers.rax = error_return(error);
        process.pending_pipe_read = None;
        process.state = ProcessState::Runnable;
        drop(manager);
        if !scheduler::wake_process(process_id) {
            return Err(Error::ProcessNotFound(process_id));
        }
        let _ = pipe::note_reader_wakeup(pending.pipe_id);
        Ok(())
    })
}

fn syscall_close(process_id: u64, descriptor: u64) -> u64 {
    if descriptor < 3 {
        return error_return(ERR_BAD_FILE_DESCRIPTOR);
    }

    let pipe_descriptor = {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        };
        if let Some(index) = process
            .pipe_descriptors
            .iter()
            .position(|pipe| pipe.descriptor == descriptor)
        {
            let descriptor = process.pipe_descriptors.remove(index);
            process.close_count = process.close_count.saturating_add(1);
            process.pipe_descriptor_close_count =
                process.pipe_descriptor_close_count.saturating_add(1);
            descriptor
        } else if let Some(index) = process
            .open_files
            .iter()
            .position(|file| file.descriptor == descriptor)
        {
            process.open_files.remove(index);
            process.close_count = process.close_count.saturating_add(1);
            return 0;
        } else {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        }
    };

    let result = match pipe_descriptor.direction {
        PipeDirection::Reader => pipe::close_reader(pipe_descriptor.pipe_id),
        PipeDirection::Writer => pipe::close_writer(pipe_descriptor.pipe_id),
    };
    match result {
        Ok(()) => 0,
        Err(_) => error_return(ERR_IO),
    }
}

fn vfs_errno(error: &vfs::Error) -> i64 {
    match error {
        vfs::Error::NotFound => ERR_NO_ENTRY,
        vfs::Error::IsDirectory => ERR_IS_DIRECTORY,
        vfs::Error::InvalidPath | vfs::Error::PathTooLong | vfs::Error::TooManyPathComponents => {
            ERR_INVALID_ARGUMENT
        }
        _ => ERR_IO,
    }
}

fn build_initial_stack(
    arguments: &[&str],
    physical_memory_offset: VirtAddr,
    pages: &[UserPage],
    stack_start: u64,
) -> Result<u64, Error> {
    if arguments.len() > MAX_ARGUMENTS {
        return Err(Error::TooManyArguments);
    }
    let argument_bytes = arguments.iter().try_fold(0usize, |total, argument| {
        if argument.as_bytes().contains(&0) {
            return Err(Error::InvalidArgument);
        }
        total
            .checked_add(argument.len().saturating_add(1))
            .ok_or(Error::ArgumentBytesTooLarge)
    })?;
    if argument_bytes > MAX_ARGUMENT_BYTES {
        return Err(Error::ArgumentBytesTooLarge);
    }

    let mut cursor = USER_STACK_TOP;
    let mut pointers = Vec::with_capacity(arguments.len());
    for argument in arguments.iter().rev() {
        cursor = cursor
            .checked_sub(argument.len().saturating_add(1) as u64)
            .ok_or(Error::StackLayoutInvalid)?;
        write_user_bytes(cursor, argument.as_bytes(), physical_memory_offset, pages)?;
        write_user_bytes(
            cursor + argument.len() as u64,
            &[0],
            physical_memory_offset,
            pages,
        )?;
        pointers.push(cursor);
    }
    pointers.reverse();

    let table_words = 1usize
        .checked_add(pointers.len())
        .and_then(|words| words.checked_add(2))
        .ok_or(Error::StackLayoutInvalid)?;
    let table_bytes = table_words
        .checked_mul(size_of::<u64>())
        .ok_or(Error::StackLayoutInvalid)?;
    cursor = cursor
        .checked_sub(table_bytes as u64)
        .ok_or(Error::StackLayoutInvalid)?
        & !0xf;
    if cursor < stack_start {
        return Err(Error::ArgumentBytesTooLarge);
    }

    let mut table = Vec::with_capacity(table_words);
    table.push(pointers.len() as u64);
    table.extend(pointers.iter().copied());
    table.push(0);
    table.push(0);
    for (index, value) in table.iter().copied().enumerate() {
        let address = cursor
            .checked_add((index * size_of::<u64>()) as u64)
            .ok_or(Error::StackLayoutInvalid)?;
        write_user_bytes(address, &value.to_ne_bytes(), physical_memory_offset, pages)?;
    }
    Ok(cursor)
}

fn write_user_bytes(
    mut virtual_address: u64,
    mut bytes: &[u8],
    physical_memory_offset: VirtAddr,
    pages: &[UserPage],
) -> Result<(), Error> {
    while !bytes.is_empty() {
        let page_address = align_down(virtual_address);
        let page = pages
            .iter()
            .find(|page| page.virtual_address == page_address)
            .ok_or(Error::InvalidUserRange)?;
        let within_page =
            usize::try_from(virtual_address - page_address).map_err(|_| Error::AddressOverflow)?;
        let chunk = bytes.len().min(Size4KiB::SIZE as usize - within_page);
        let destination_address = physical_memory_offset
            .as_u64()
            .checked_add(page.frame.start_address().as_u64())
            .and_then(|address| address.checked_add(within_page as u64))
            .ok_or(Error::AddressOverflow)?;
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), destination_address as *mut u8, chunk);
        }
        virtual_address = virtual_address
            .checked_add(chunk as u64)
            .ok_or(Error::AddressOverflow)?;
        bytes = &bytes[chunk..];
    }
    Ok(())
}

fn read_user_bytes(
    mut virtual_address: u64,
    mut length: usize,
    physical_memory_offset: VirtAddr,
    pages: &[UserPage],
) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::with_capacity(length);
    while length > 0 {
        let page_address = align_down(virtual_address);
        let page = pages
            .iter()
            .find(|page| page.virtual_address == page_address)
            .ok_or(Error::InvalidUserRange)?;
        let within_page =
            usize::try_from(virtual_address - page_address).map_err(|_| Error::AddressOverflow)?;
        let chunk = length.min(Size4KiB::SIZE as usize - within_page);
        let source_address = physical_memory_offset
            .as_u64()
            .checked_add(page.frame.start_address().as_u64())
            .and_then(|address| address.checked_add(within_page as u64))
            .ok_or(Error::AddressOverflow)?;
        let source = unsafe { slice::from_raw_parts(source_address as *const u8, chunk) };
        bytes.extend_from_slice(source);
        virtual_address = virtual_address
            .checked_add(chunk as u64)
            .ok_or(Error::AddressOverflow)?;
        length -= chunk;
    }
    Ok(bytes)
}

fn map_range(
    start: u64,
    end: u64,
    flags: PageTableFlags,
    mapper: &mut OffsetPageTable<'_>,
    frame_allocator: &mut TrackingFrameAllocator<'_>,
    physical_memory_offset: VirtAddr,
    pages: &mut Vec<UserPage>,
) -> Result<(), Error> {
    let mut address = align_down(start);
    let end = align_up(end).ok_or(Error::AddressOverflow)?;
    while address < end {
        if pages.iter().any(|page| page.virtual_address == address) {
            return Err(Error::PageAlreadyMapped(address));
        }

        let frame = frame_allocator
            .allocate_frame()
            .ok_or(Error::FrameAllocationFailed)?;
        zero_frame(frame, physical_memory_offset)?;
        let page = Page::containing_address(VirtAddr::new(address));
        let flush = unsafe { mapper.map_to(page, frame, flags, frame_allocator) }
            .map_err(|error| map_error(error, address))?;
        flush.ignore();
        pages.push(UserPage {
            virtual_address: address,
            frame,
        });
        address = address
            .checked_add(PAGE_BYTES)
            .ok_or(Error::AddressOverflow)?;
    }
    Ok(())
}

fn copy_segment(
    path: &str,
    segment: &LoadSegment,
    physical_memory_offset: VirtAddr,
    pages: &[UserPage],
) -> Result<(), Error> {
    let mut copied = 0_u64;
    while copied < segment.file_size {
        let virtual_address = segment
            .virtual_address
            .checked_add(copied)
            .ok_or(Error::AddressOverflow)?;
        let page_address = align_down(virtual_address);
        let page = pages
            .iter()
            .find(|page| page.virtual_address == page_address)
            .ok_or(Error::InvalidUserRange)?;
        let within_page =
            usize::try_from(virtual_address - page_address).map_err(|_| Error::AddressOverflow)?;
        let remaining_page = Size4KiB::SIZE as usize - within_page;
        let remaining_file =
            usize::try_from(segment.file_size - copied).map_err(|_| Error::AddressOverflow)?;
        let chunk = remaining_page.min(remaining_file);
        let destination_address = physical_memory_offset
            .as_u64()
            .checked_add(page.frame.start_address().as_u64())
            .and_then(|address| address.checked_add(within_page as u64))
            .ok_or(Error::AddressOverflow)?;
        let destination =
            unsafe { slice::from_raw_parts_mut(destination_address as *mut u8, chunk) };
        let file_offset = segment
            .file_offset
            .checked_add(copied)
            .ok_or(Error::AddressOverflow)?;
        vfs::read_exact_at(path, file_offset, destination)?;
        copied = copied
            .checked_add(chunk as u64)
            .ok_or(Error::AddressOverflow)?;
    }
    Ok(())
}

fn zero_frame(frame: PhysFrame<Size4KiB>, physical_memory_offset: VirtAddr) -> Result<(), Error> {
    let address = physical_memory_offset
        .as_u64()
        .checked_add(frame.start_address().as_u64())
        .ok_or(Error::AddressOverflow)?;
    unsafe { ptr::write_bytes(address as *mut u8, 0, Size4KiB::SIZE as usize) };
    Ok(())
}

fn map_error(error: MapToError<Size4KiB>, address: u64) -> Error {
    match error {
        MapToError::FrameAllocationFailed => Error::PageTableFrameAllocationFailed,
        MapToError::ParentEntryHugePage => Error::ParentEntryHugePage,
        MapToError::PageAlreadyMapped(_) => Error::PageAlreadyMapped(address),
    }
}

fn validate_user_range(start: u64, end: u64) -> Result<(), Error> {
    if start < USER_MIN_ADDRESS || start >= end || end > USER_PML4_SLOT_END {
        Err(Error::InvalidUserRange)
    } else {
        Ok(())
    }
}

const fn pml4_index(address: u64) -> u16 {
    ((address >> 39) & 0x1ff) as u16
}

const fn align_down(address: u64) -> u64 {
    address & !(PAGE_BYTES - 1)
}

fn align_up(address: u64) -> Option<u64> {
    address
        .checked_add(PAGE_BYTES - 1)
        .map(|value| value & !(PAGE_BYTES - 1))
}

const fn error_return(error: i64) -> u64 {
    error as u64
}
