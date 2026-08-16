use alloc::{
    boxed::Box,
    collections::VecDeque,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    arch::global_asm,
    fmt,
    mem::{align_of, size_of},
    ptr, slice, str,
};

use x86_64::{
    PhysAddr, VirtAddr,
    instructions::{hlt, interrupts as cpu_interrupts},
    registers::control::Cr2,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageSize, PageTable, PageTableFlags,
        PhysFrame, Size4KiB, mapper::MapToError,
    },
};

use crate::{
    gdt, memory::BootInfoFrameAllocator, preemption::PreemptMutex,
    process_completion::CompletionQueue, scheduler, vfs,
};

use super::{
    elf::{self, Image, ImageType, LoadSegment, LoadedExecutable},
    pipe::{self, PipeId},
    terminal,
};

pub use super::{pipe::Snapshot as PipeSnapshot, terminal::Snapshot as TerminalSnapshot};

#[allow(dead_code)]
mod abi {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/userspace_abi.rs"
    ));
}

#[allow(dead_code)]
mod block_device_protocol {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/block_device_protocol.rs"
    ));
}

#[allow(dead_code)]
mod tmpfs_protocol {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/tmpfs_protocol.rs"
    ));
}

#[allow(dead_code)]
mod filesystem_protocol {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/filesystem_protocol.rs"
    ));
}

#[allow(dead_code)]
mod vfs_protocol {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/vfs_protocol.rs"
    ));
}

#[allow(dead_code)]
mod nullfs_primary_volume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/nullfs_primary_volume.rs"
    ));
}

pub const SYSCALL_VECTOR: u8 = abi::SYSCALL_VECTOR;
pub const INIT_PROCESS_ID: u64 = abi::INIT_PROCESS_ID;

const PAGE_FAULT_VECTOR: u64 = 14;
const GENERAL_PROTECTION_VECTOR: u64 = 13;
const SYSCALL_WRITE: u64 = abi::syscall::WRITE;
const SYSCALL_YIELD: u64 = abi::syscall::YIELD;
const SYSCALL_EXIT: u64 = abi::syscall::EXIT;
const SYSCALL_OPEN: u64 = abi::syscall::OPEN;
const SYSCALL_READ: u64 = abi::syscall::READ;
const SYSCALL_CLOSE: u64 = abi::syscall::CLOSE;
const SYSCALL_SPAWN_COMMAND: u64 = abi::syscall::SPAWN_COMMAND;
const SYSCALL_WAIT_CHILD: u64 = abi::syscall::WAIT_CHILD;
const SYSCALL_GETPID: u64 = abi::syscall::GETPID;
const SYSCALL_PIPE_PAIR: u64 = abi::syscall::PIPE_PAIR;
const SYSCALL_TRY_WAIT_CHILD: u64 = abi::syscall::TRY_WAIT_CHILD;
const SYSCALL_SIGNAL_PROCESS_GROUP: u64 = abi::syscall::SIGNAL_PROCESS_GROUP;
const SYSCALL_FOREGROUND_PROCESS_GROUP: u64 = abi::syscall::FOREGROUND_PROCESS_GROUP;
const SYSCALL_SEEK: u64 = abi::syscall::SEEK;
const SYSCALL_EXECVE: u64 = abi::syscall::EXECVE;
const SYSCALL_SET_DESCRIPTOR_FLAGS: u64 = abi::syscall::SET_DESCRIPTOR_FLAGS;
const SYSCALL_FORK: u64 = abi::syscall::FORK;
const SYSCALL_SIGNAL_ACTION: u64 = abi::syscall::SIGNAL_ACTION;
const SYSCALL_SIGNAL_MASK: u64 = abi::syscall::SIGNAL_MASK;
const SYSCALL_SIGNAL_RETURN: u64 = abi::syscall::SIGNAL_RETURN;
const SYSCALL_ENVIRONMENT_SET: u64 = abi::syscall::ENVIRONMENT_SET;
const SYSCALL_ENVIRONMENT_UNSET: u64 = abi::syscall::ENVIRONMENT_UNSET;

pub const SIGNAL_INTERRUPT: u64 = abi::signal::INTERRUPT;
pub const SIGNAL_KILL: u64 = abi::signal::KILL;
pub const SIGNAL_TERMINATE: u64 = abi::signal::TERMINATE;
pub const SIGNAL_CONTINUE: u64 = abi::signal::CONTINUE;
pub const SIGNAL_STOP: u64 = abi::signal::STOP;
pub const SIGNAL_TERMINAL_STOP: u64 = abi::signal::TERMINAL_STOP;

const USER_MIN_ADDRESS: u64 = 0x0001_0000;
const USER_PML4_SLOT_END: u64 = 0x0000_0080_0000_0000;
const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;
// Writable filesystem transactions use multiple block-sized frames even in unoptimized builds.
const USER_STACK_SIZE: usize = 256 * 1024;
const USER_STACK_GUARD_SIZE: usize = Size4KiB::SIZE as usize;
const KERNEL_TRANSITION_STACK_SIZE: usize = 64 * 1024;
const KERNEL_TRANSITION_STACK_WORDS: usize = KERNEL_TRANSITION_STACK_SIZE / size_of::<u128>();
const MAX_SYSCALL_WRITE_BYTES: usize = abi::limits::MAX_SYSCALL_WRITE_BYTES;
const MAX_SYSCALL_READ_BYTES: usize = abi::limits::MAX_SYSCALL_READ_BYTES;
const MAX_OPEN_FILES: usize = abi::limits::MAX_OPEN_FILES;
const MAX_ARGUMENTS: usize = abi::limits::MAX_ARGUMENTS;
const MAX_ARGUMENT_BYTES: usize = abi::limits::MAX_ARGUMENT_BYTES;
const MAX_ENVIRONMENT_VARIABLES: usize = abi::limits::MAX_ENVIRONMENT_VARIABLES;
const MAX_ENVIRONMENT_BYTES: usize = abi::limits::MAX_ENVIRONMENT_BYTES;
const MAX_ENVIRONMENT_NAME_BYTES: usize = abi::limits::MAX_ENVIRONMENT_NAME_BYTES;
const MAX_COMMAND_BYTES: usize = abi::limits::MAX_COMMAND_BYTES;
const SPAWN_FOREGROUND: u64 = abi::spawn::FOREGROUND;
const SPAWN_USE_DESCRIPTORS: u64 = abi::spawn::USE_DESCRIPTORS;
const SPAWN_NEW_PROCESS_GROUP: u64 = abi::spawn::NEW_PROCESS_GROUP;
const SPAWN_JOIN_PROCESS_GROUP: u64 = abi::spawn::JOIN_PROCESS_GROUP;
const DEFAULT_DESCRIPTOR: u64 = abi::spawn::DEFAULT_DESCRIPTOR;
const DEFAULT_PROCESS_GROUP: u64 = abi::spawn::DEFAULT_PROCESS_GROUP;
const OPEN_READ: u64 = abi::open::READ;
const OPEN_WRITE: u64 = abi::open::WRITE;
const OPEN_CREATE: u64 = abi::open::CREATE;
const OPEN_TRUNCATE: u64 = abi::open::TRUNCATE;
const OPEN_APPEND: u64 = abi::open::APPEND;
const OPEN_CLOSE_ON_EXEC: u64 = abi::open::CLOSE_ON_EXEC;
const OPEN_ALLOWED_FLAGS: u64 = abi::open::ALLOWED_FLAGS;
const DESCRIPTOR_CLOSE_ON_EXEC: u64 = abi::descriptor::CLOSE_ON_EXEC;
const DESCRIPTOR_ALLOWED_FLAGS: u64 = abi::descriptor::ALLOWED_FLAGS;
const SEEK_SET: u64 = abi::seek::SET;
const SEEK_CURRENT: u64 = abi::seek::CURRENT;
const SEEK_END: u64 = abi::seek::END;
const SHELL_PROCESS_TASK_NAME: &str = "user-shell-process";
const USER_RFLAGS: u64 = 0x202;
const RFLAGS_DIRECTION: u64 = 1 << 10;
const PAGE_BYTES: u64 = Size4KiB::SIZE;
const SIGNAL_TABLE_SIZE: usize = abi::signal::MAX as usize + 1;
const SIGNAL_SUPPORTED_MASK: u64 = abi::signal::SUPPORTED_MASK;
const SIGNAL_UNBLOCKABLE_MASK: u64 = abi::signal::UNBLOCKABLE_MASK;
const SIGNAL_RED_ZONE_BYTES: u64 = 128;
pub const MAX_PROCESS_SLOTS: usize = abi::limits::MAX_JOB_PROCESSES;
const MAX_PENDING_TMPFS_CLOSES: usize = MAX_PROCESS_SLOTS * (MAX_OPEN_FILES + 3) + 1;
const NULLFS_MOUNT_PATH: &str = nullfs_primary_volume::MOUNT_PATH;
const FILESYSTEM_PROXY_BULK_BYTES: usize = 4096;
const PROCESS_HISTORY_LIMIT: usize = 128;
// PID zero is reserved for the kernel reaper. Children assigned to it have no
// userspace owner and therefore never become waitable zombies.
const KERNEL_REAPER_PROCESS_ID: u64 = 0;

const ERR_NO_ENTRY: i64 = abi::errno::NO_ENTRY;
const ERR_INTERRUPTED: i64 = abi::errno::INTERRUPTED;
const ERR_NO_PROCESS: i64 = abi::errno::NO_PROCESS;
const ERR_IO: i64 = abi::errno::IO;
const ERR_ARGUMENT_TOO_LARGE: i64 = abi::errno::ARGUMENT_TOO_LARGE;
const ERR_BAD_FILE_DESCRIPTOR: i64 = abi::errno::BAD_FILE_DESCRIPTOR;
const ERR_NO_CHILD: i64 = abi::errno::NO_CHILD;
const ERR_TRY_AGAIN: i64 = abi::errno::TRY_AGAIN;
const ERR_BAD_ADDRESS: i64 = abi::errno::BAD_ADDRESS;
const ERR_IS_DIRECTORY: i64 = abi::errno::IS_DIRECTORY;
const ERR_INVALID_ARGUMENT: i64 = abi::errno::INVALID_ARGUMENT;
const ERR_TOO_MANY_OPEN_FILES: i64 = abi::errno::TOO_MANY_OPEN_FILES;
const ERR_NO_SPACE: i64 = abi::errno::NO_SPACE;
const ERR_READ_ONLY: i64 = abi::errno::READ_ONLY;
const ERR_BROKEN_PIPE: i64 = abi::errno::BROKEN_PIPE;
const ERR_NOT_IMPLEMENTED: i64 = abi::errno::NOT_IMPLEMENTED;

static PROCESS_MANAGER: PreemptMutex<ProcessManager> = PreemptMutex::new(ProcessManager::new());

#[derive(Debug, Clone, Copy)]
struct SharedFrameReference {
    frame: PhysFrame<Size4KiB>,
    references: usize,
}

#[derive(Debug)]
struct SharedFrameTable {
    frames: Vec<SharedFrameReference>,
    peak_frames: usize,
    peak_references: usize,
}

impl SharedFrameTable {
    const fn new() -> Self {
        Self {
            frames: Vec::new(),
            peak_frames: 0,
            peak_references: 0,
        }
    }

    fn retain(&mut self, frame: PhysFrame<Size4KiB>) {
        if let Some(reference) = self.frames.iter_mut().find(|entry| entry.frame == frame) {
            reference.references = reference.references.saturating_add(1);
        } else {
            self.frames.push(SharedFrameReference {
                frame,
                references: 2,
            });
        }
        self.peak_frames = self.peak_frames.max(self.frames.len());
        self.peak_references = self.peak_references.max(self.total_references());
    }

    fn references(&self, frame: PhysFrame<Size4KiB>) -> usize {
        self.frames
            .iter()
            .find(|entry| entry.frame == frame)
            .map(|entry| entry.references)
            .unwrap_or(1)
    }

    fn release(&mut self, frame: PhysFrame<Size4KiB>) -> bool {
        let Some(index) = self.frames.iter().position(|entry| entry.frame == frame) else {
            return true;
        };
        let references = self.frames[index].references.saturating_sub(1);
        if references <= 1 {
            self.frames.remove(index);
        } else {
            self.frames[index].references = references;
        }
        false
    }

    fn total_references(&self) -> usize {
        self.frames.iter().map(|entry| entry.references).sum()
    }
}

static SHARED_USER_FRAMES: PreemptMutex<SharedFrameTable> =
    PreemptMutex::new(SharedFrameTable::new());

fn retain_shared_frame(frame: PhysFrame<Size4KiB>) {
    SHARED_USER_FRAMES.lock().retain(frame);
}

fn shared_frame_references(frame: PhysFrame<Size4KiB>) -> usize {
    SHARED_USER_FRAMES.lock().references(frame)
}

fn release_owned_frame(
    frame: PhysFrame<Size4KiB>,
    frame_allocator: &mut BootInfoFrameAllocator,
) -> bool {
    if SHARED_USER_FRAMES.lock().release(frame) {
        frame_allocator.deallocate_frame(frame);
        true
    } else {
        false
    }
}

fn release_owned_frames(
    frames: &mut Vec<PhysFrame<Size4KiB>>,
    frame_allocator: &mut BootInfoFrameAllocator,
) -> usize {
    let mut reclaimed = 0usize;
    for frame in frames.drain(..) {
        reclaimed =
            reclaimed.saturating_add(usize::from(release_owned_frame(frame, frame_allocator)));
    }
    reclaimed
}

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
    Stopped,
    Exited,
    Faulted,
    Signaled,
}

impl fmt::Display for ProcessState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runnable => formatter.write_str("runnable"),
            Self::Blocked => formatter.write_str("blocked"),
            Self::Stopped => formatter.write_str("stopped"),
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
    pub file_write_count: u64,
    pub file_bytes_written: u64,
    pub seek_count: u64,
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
    pub exec_count: u64,
    pub exec_failure_count: u64,
    pub close_on_exec_count: u64,
    pub exec_frames_reclaimed: u64,
    pub fork_count: u64,
    pub environment_count: usize,
    pub environment_change_count: u64,
    pub cow_fault_count: u64,
    pub cow_copy_count: u64,
    pub signal_sent_count: u64,
    pub signal_received_count: u64,
    pub signal_handler_count: u64,
    pub signal_return_count: u64,
    pub signal_ignored_count: u64,
    pub signal_interrupted_syscall_count: u64,
    pub signal_frame_failure_count: u64,
    pub pending_signal_peak: u64,
    pub stop_count: u64,
    pub continue_count: u64,
    pub pipe_pair_count: u64,
    pub pipe_descriptor_close_count: u64,
    pub pipe_descriptor_inherit_count: u64,
    pub file_descriptor_inherit_count: u64,
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
    pub execs: u64,
    pub exec_failures: u64,
    pub forks: u64,
    pub fork_failures: u64,
    pub environment_changes: u64,
    pub cow_faults: u64,
    pub cow_copies: u64,
    pub shared_frames: usize,
    pub shared_references: usize,
    pub peak_shared_frames: usize,
    pub peak_shared_references: usize,
    pub signals_sent: u64,
    pub signal_handlers: u64,
    pub signal_returns: u64,
    pub signal_ignores: u64,
    pub signal_interruptions: u64,
    pub signal_frame_failures: u64,
    pub pending_signals: usize,
    pub stop_deliveries: u64,
    pub continue_deliveries: u64,
    pub pipe_pairs: u64,
    pub pipe_descriptor_inherits: u64,
    pub file_descriptor_inherits: u64,
    pub waitable_zombies: usize,
    pub process_limit: usize,
    pub active: usize,
    pub blocked: usize,
    pub stopped: usize,
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
    pub runnable: usize,
    pub blocked: usize,
    pub stopped: usize,
}

#[derive(Debug)]
pub enum Error {
    SchedulerNotOnBootstrapTask,
    UnsupportedImageType,
    InvalidExecutableBytes,
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
    TooManyEnvironmentVariables,
    EnvironmentBytesTooLarge,
    InvalidEnvironment,
    InvalidArgument,
    InvalidDescriptor(u64),
    InvalidProcessGroup(u64),
    TerminalBusy,
    ProcessLimitReached,
    JobLimitReached,
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
            Self::InvalidExecutableBytes => {
                "validated executable segment bytes are unavailable during image construction"
            }
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
            Self::TooManyEnvironmentVariables => {
                "userspace environment variable count exceeds the configured bound"
            }
            Self::EnvironmentBytesTooLarge => {
                "userspace environment strings exceed the configured stack bound"
            }
            Self::InvalidEnvironment => "userspace environment entry is invalid",
            Self::InvalidArgument => "userspace argument contains an invalid byte",
            Self::InvalidDescriptor(_) => "userspace file descriptor is invalid",
            Self::InvalidProcessGroup(_) => "userspace process group is invalid",
            Self::TerminalBusy => "another userspace process owns the terminal",
            Self::ProcessLimitReached => "userspace process limit was reached",
            Self::JobLimitReached => "userspace job membership limit was reached",
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
    stderr_descriptor: Option<u64>,
    new_process_group: bool,
    process_group_id: Option<u64>,
    stack_pointer: usize,
    claimed: bool,
}

#[derive(Debug, Clone, Copy)]
struct PendingChildWait {
    child_process_id: u64,
    stack_pointer: usize,
}

#[derive(Debug, Clone)]
struct PendingExec {
    path: String,
    arguments: Vec<String>,
    stack_pointer: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutableLoadOwner {
    ChildSpawn,
    Exec,
}

struct PendingExecutableLoad {
    owner: ExecutableLoadOwner,
    path: String,
    stack_pointer: usize,
    vfs_generation: u32,
    backend_path: Option<String>,
    provider_generation: u32,
    session_id: u64,
    session_generation: u64,
    node_id: u64,
    expected_size: usize,
    bytes: Vec<u8>,
    retry: Option<(filesystem_protocol::Request, PendingNullfsProxyOperation)>,
    result: Option<Result<LoadedExecutable, i64>>,
}

impl PendingExecutableLoad {
    fn new(owner: ExecutableLoadOwner, path: &str, stack_pointer: usize) -> Self {
        Self {
            owner,
            path: String::from(path),
            stack_pointer,
            vfs_generation: 0,
            backend_path: None,
            provider_generation: 0,
            session_id: filesystem_protocol::INVALID_ID,
            session_generation: 0,
            node_id: filesystem_protocol::INVALID_ID,
            expected_size: 0,
            bytes: Vec::new(),
            retry: None,
            result: None,
        }
    }

    fn has_open_node(&self) -> bool {
        self.provider_generation != 0
            && self.session_id != filesystem_protocol::INVALID_ID
            && self.session_generation != 0
            && self.node_id != filesystem_protocol::INVALID_ID
    }

    fn take_close_ticket(&mut self) -> Option<PendingTmpfsClose> {
        if !self.has_open_node() {
            return None;
        }
        let ticket = PendingTmpfsClose {
            generation: self.provider_generation,
            session_id: self.session_id,
            session_generation: self.session_generation,
            node_id: self.node_id,
        };
        self.node_id = filesystem_protocol::INVALID_ID;
        Some(ticket)
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingFork {
    stack_pointer: usize,
}

#[derive(Debug, Clone, Copy)]
struct PendingCowFault {
    page_address: u64,
}

#[derive(Clone, Copy)]
struct ActiveSignalFrame {
    signal: u64,
    previous_mask: u64,
    saved_context: SavedContext,
    frame_address: u64,
    cookie: u64,
    restorer: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenFileBackend {
    Vfs,
    TmpfsProxy {
        generation: u32,
        session_id: u64,
        session_generation: u64,
        node_id: u64,
    },
    NullfsProxy {
        generation: u32,
        session_id: u64,
        session_generation: u64,
        node_id: u64,
    },
}

struct OpenFileState {
    path: String,
    offset: u64,
    readable: bool,
    writable: bool,
    append: bool,
    size: u64,
    nullfs_size: Option<Arc<PreemptMutex<u64>>>,
    backend: OpenFileBackend,
}

impl Drop for OpenFileState {
    fn drop(&mut self) {
        match self.backend {
            OpenFileBackend::TmpfsProxy {
                generation,
                session_id,
                session_generation,
                node_id,
            } => tmpfs_proxy_enqueue_close(generation, session_id, session_generation, node_id),
            OpenFileBackend::NullfsProxy {
                generation,
                session_id,
                session_generation,
                node_id,
            } => nullfs_proxy_enqueue_close(generation, session_id, session_generation, node_id),
            OpenFileBackend::Vfs => {}
        }
    }
}

type OpenFileHandle = Arc<PreemptMutex<OpenFileState>>;

#[derive(Clone)]
struct OpenFile {
    descriptor: u64,
    handle: OpenFileHandle,
    close_on_exec: bool,
}

#[derive(Clone)]
struct PendingTmpfsProxyRequest {
    reply_endpoint: CapabilityObjectRef,
    request_operation: u16,
    request_generation: u32,
    generic_request_id: u64,
    operation: PendingTmpfsProxyOperation,
    stack_pointer: usize,
}

#[derive(Clone)]
enum PendingTmpfsProxyOperation {
    Open {
        path: String,
        descriptor: u64,
        readable: bool,
        writable: bool,
        append: bool,
        close_on_exec: bool,
        generation: u32,
        session_id: u64,
        session_generation: u64,
    },
    Read {
        handle: OpenFileHandle,
        address: u64,
        length: usize,
    },
    Write {
        handle: OpenFileHandle,
        offset: u64,
        initial_offset: u64,
        append: bool,
        length: usize,
    },
    Stat {
        address: u64,
        length: u64,
    },
    ReadDirectory {
        start_index: usize,
        records_address: u64,
        capacity: usize,
    },
    Unlink,
}

#[derive(Clone)]
struct PendingNullfsProxyRequest {
    reply_endpoint: CapabilityObjectRef,
    request: filesystem_protocol::Request,
    request_operation: u16,
    request_generation: u32,
    request_id: u64,
    operation: PendingNullfsProxyOperation,
    stack_pointer: usize,
}

#[derive(Clone)]
enum PendingNullfsProxyOperation {
    Lookup {
        path: String,
        components: Vec<String>,
        next_component: usize,
        purpose: NullfsPathPurpose,
    },
    Open {
        path: String,
        descriptor: u64,
        readable: bool,
        writable: bool,
        append: bool,
        close_on_exec: bool,
        generation: u32,
        session_id: u64,
        session_generation: u64,
    },
    LoadExecutableOpen {
        owner: ExecutableLoadOwner,
        vfs_generation: u32,
        generation: u32,
        session_id: u64,
        session_generation: u64,
    },
    LoadExecutableRead {
        owner: ExecutableLoadOwner,
        vfs_generation: u32,
        generation: u32,
        session_id: u64,
        session_generation: u64,
        node_id: u64,
        offset: usize,
        length: usize,
    },
    Read {
        handle: OpenFileHandle,
        address: u64,
        initial_offset: u64,
        length: usize,
    },
    Write {
        handle: OpenFileHandle,
        offset: u64,
        initial_offset: u64,
        append: bool,
        length: usize,
    },
    ReadDirectory {
        node_id: u64,
        start_index: usize,
        seen: usize,
        records_address: u64,
        capacity: usize,
        records: Vec<abi::file::DirectoryEntry>,
        cookie: u64,
        flags: u64,
    },
    Unlink,
}

struct NullfsNodeSize {
    generation: u32,
    session_id: u64,
    session_generation: u64,
    node_id: u64,
    size: Weak<PreemptMutex<u64>>,
}

#[derive(Clone)]
enum NullfsPathPurpose {
    Stat {
        address: u64,
        length: u64,
    },
    Open {
        options: vfs::OpenOptions,
        descriptor: u64,
        close_on_exec: bool,
    },
    LoadExecutable {
        owner: ExecutableLoadOwner,
        vfs_generation: u32,
    },
    ReadDirectory {
        start_index: usize,
        records_address: u64,
        capacity: usize,
    },
    Chdir,
    Unlink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingTmpfsClose {
    generation: u32,
    session_id: u64,
    session_generation: u64,
    node_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActiveTmpfsClose {
    ticket: PendingTmpfsClose,
    request_id: u64,
    reply_endpoint: CapabilityObjectRef,
    session_id: u64,
    session_generation: u64,
}

#[derive(Clone, Copy)]
struct TmpfsProxyState {
    request_endpoint: Option<CapabilityObjectRef>,
    retired_request_endpoint_id: u64,
    generation: u32,
    connect_reply_endpoint: Option<CapabilityObjectRef>,
    session_reply_endpoint: Option<CapabilityObjectRef>,
    session_id: u64,
    session_generation: u64,
    session_features: u64,
    bulk_buffer: Option<CapabilityObjectRef>,
    bulk_buffer_attached: bool,
    next_request_id: u64,
    active_request_id: u64,
    active_close: Option<ActiveTmpfsClose>,
}

impl TmpfsProxyState {
    const fn new() -> Self {
        Self {
            request_endpoint: None,
            retired_request_endpoint_id: 0,
            generation: 0,
            connect_reply_endpoint: None,
            session_reply_endpoint: None,
            session_id: filesystem_protocol::INVALID_ID,
            session_generation: 0,
            session_features: 0,
            bulk_buffer: None,
            bulk_buffer_attached: false,
            next_request_id: 3,
            active_request_id: filesystem_protocol::INVALID_ID,
            active_close: None,
        }
    }
}

#[derive(Clone, Copy)]
struct KernelCapabilityRoot {
    object: CapabilityObjectRef,
    references: usize,
}

static KERNEL_CAPABILITY_ROOTS: PreemptMutex<Vec<KernelCapabilityRoot>> =
    PreemptMutex::new(Vec::new());
static TMPFS_PROXY: PreemptMutex<TmpfsProxyState> = PreemptMutex::new(TmpfsProxyState::new());
static TMPFS_CLOSE_QUEUE: PreemptMutex<VecDeque<PendingTmpfsClose>> =
    PreemptMutex::new(VecDeque::new());
static TMPFS_ABANDONED_REQUEST: PreemptMutex<Option<PendingTmpfsProxyRequest>> =
    PreemptMutex::new(None);
static NULLFS_PROXY: PreemptMutex<TmpfsProxyState> = PreemptMutex::new(TmpfsProxyState::new());
static NULLFS_CLOSE_QUEUE: PreemptMutex<VecDeque<PendingTmpfsClose>> =
    PreemptMutex::new(VecDeque::new());
static NULLFS_ABANDONED_REQUEST: PreemptMutex<Option<PendingNullfsProxyRequest>> =
    PreemptMutex::new(None);
static NULLFS_NODE_SIZES: PreemptMutex<Vec<NullfsNodeSize>> = PreemptMutex::new(Vec::new());

#[derive(Clone, Copy)]
struct VfsRouteState {
    request_endpoint: Option<CapabilityObjectRef>,
    retired_request_endpoint_id: u64,
    reply_endpoint: Option<CapabilityObjectRef>,
    generation: u32,
    ready: bool,
    next_request_id: u32,
    active_request_id: u32,
}

impl VfsRouteState {
    const fn new() -> Self {
        Self {
            request_endpoint: None,
            retired_request_endpoint_id: 0,
            reply_endpoint: None,
            generation: 0,
            ready: false,
            next_request_id: 1,
            active_request_id: 0,
        }
    }
}

static VFS_ROUTE: PreemptMutex<VfsRouteState> = PreemptMutex::new(VfsRouteState::new());

#[derive(Clone)]
enum PendingVfsOperation {
    Stat {
        stat_address: u64,
        stat_length: u64,
    },
    LoadExecutable {
        owner: ExecutableLoadOwner,
    },
    ReadDirectory {
        start_index: usize,
        records_address: u64,
        capacity: usize,
    },
    Chdir,
    Open {
        options: vfs::OpenOptions,
        close_on_exec: bool,
        descriptor: u64,
    },
    Unlink,
}

#[derive(Clone)]
struct PendingVfsRequest {
    reply_endpoint: CapabilityObjectRef,
    request_id: u32,
    generation: u32,
    path: String,
    operation: PendingVfsOperation,
    stack_pointer: usize,
}

fn kernel_capability_root_add(object: CapabilityObjectRef) {
    let mut roots = KERNEL_CAPABILITY_ROOTS.lock();
    if let Some(root) = roots.iter_mut().find(|root| root.object == object) {
        root.references = root
            .references
            .checked_add(1)
            .expect("kernel capability root reference count overflowed");
    } else {
        roots.push(KernelCapabilityRoot {
            object,
            references: 1,
        });
    }
}

fn kernel_capability_root_remove(object: CapabilityObjectRef) {
    let mut roots = KERNEL_CAPABILITY_ROOTS.lock();
    let Some(index) = roots.iter().position(|root| root.object == object) else {
        return;
    };
    if roots[index].references > 1 {
        roots[index].references -= 1;
    } else {
        roots.remove(index);
    }
}

fn kernel_capability_roots_snapshot() -> Vec<CapabilityObjectRef> {
    KERNEL_CAPABILITY_ROOTS
        .lock()
        .iter()
        .map(|root| root.object)
        .collect()
}

#[derive(Clone)]
enum StreamTarget {
    Pipe(PipeId),
    File(OpenFileHandle),
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
    close_on_exec: bool,
}

struct Process {
    process_id: u64,
    parent_process_id: Option<u64>,
    process_group_id: u64,
    job: Option<CapabilityObjectRef>,
    terminal_parent: Option<u64>,
    task_id: u64,
    path: String,
    environment: Vec<String>,
    state: ProcessState,
    stopped_resume_state: Option<ProcessState>,
    last_stop_signal: Option<u64>,
    pending_parent_status: Option<u64>,
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
    stdin_target: Option<StreamTarget>,
    stdout_target: Option<StreamTarget>,
    stderr_target: Option<StreamTarget>,
    pending_terminal_read: Option<PendingTerminalRead>,
    pending_pipe_read: Option<PendingPipeRead>,
    pending_pipe_write: Option<PendingPipeWrite>,
    pending_tmpfs_proxy: Option<PendingTmpfsProxyRequest>,
    pending_nullfs_proxy: Option<PendingNullfsProxyRequest>,
    pending_vfs_request: Option<PendingVfsRequest>,
    pending_child_spawn: Option<PendingChildSpawn>,
    pending_child_wait: Option<PendingChildWait>,
    pending_exec: Option<PendingExec>,
    pending_executable_load: Option<PendingExecutableLoad>,
    pending_fork: Option<PendingFork>,
    pending_cow_fault: Option<PendingCowFault>,
    signal_actions: [abi::signal_action::Action; SIGNAL_TABLE_SIZE],
    signal_mask: u64,
    pending_signals: u64,
    active_signal: Option<ActiveSignalFrame>,
    pending_signal_peak: u64,
    syscall_count: u64,
    write_count: u64,
    yield_count: u64,
    bytes_written: u64,
    open_count: u64,
    read_count: u64,
    close_count: u64,
    bytes_read: u64,
    file_write_count: u64,
    file_bytes_written: u64,
    seek_count: u64,
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
    exec_count: u64,
    exec_failure_count: u64,
    close_on_exec_count: u64,
    exec_frames_reclaimed: u64,
    fork_count: u64,
    environment_change_count: u64,
    cow_fault_count: u64,
    cow_copy_count: u64,
    signal_sent_count: u64,
    signal_received_count: u64,
    signal_handler_count: u64,
    signal_return_count: u64,
    signal_ignored_count: u64,
    signal_interrupted_syscall_count: u64,
    signal_frame_failure_count: u64,
    stop_count: u64,
    continue_count: u64,
    pipe_pair_count: u64,
    pipe_descriptor_close_count: u64,
    pipe_descriptor_inherit_count: u64,
    file_descriptor_inherit_count: u64,
}

impl Process {
    fn is_live(&self) -> bool {
        matches!(
            self.state,
            ProcessState::Runnable | ProcessState::Blocked | ProcessState::Stopped
        )
    }

    fn make_runnable(&mut self) {
        match self.state {
            ProcessState::Blocked => self.state = ProcessState::Runnable,
            ProcessState::Stopped if self.stopped_resume_state == Some(ProcessState::Blocked) => {
                self.stopped_resume_state = Some(ProcessState::Runnable);
            }
            _ => {}
        }
    }

    fn stop(&mut self, signal: u64, record_received: bool) -> bool {
        if !matches!(self.state, ProcessState::Runnable | ProcessState::Blocked) {
            return false;
        }
        self.stopped_resume_state = Some(self.state);
        self.state = ProcessState::Stopped;
        self.last_stop_signal = Some(signal);
        self.pending_parent_status = Some(stopped_child_status(signal));
        if record_received {
            self.signal_received_count = self.signal_received_count.saturating_add(1);
        }
        self.stop_count = self.stop_count.saturating_add(1);
        true
    }

    fn continue_running(&mut self, record_received: bool) -> bool {
        if self.state != ProcessState::Stopped {
            return false;
        }
        self.state = self
            .stopped_resume_state
            .take()
            .unwrap_or(ProcessState::Runnable);
        self.pending_parent_status = Some(abi::child_status::CONTINUED);
        if record_received {
            self.signal_received_count = self.signal_received_count.saturating_add(1);
        }
        self.continue_count = self.continue_count.saturating_add(1);
        true
    }

    fn take_parent_status(&mut self) -> Option<u64> {
        self.pending_parent_status.take()
    }

    fn signal_action(&self, signal: u64) -> abi::signal_action::Action {
        usize::try_from(signal)
            .ok()
            .and_then(|index| self.signal_actions.get(index).copied())
            .unwrap_or(abi::signal_action::Action::DEFAULT)
    }

    fn signal_is_masked(&self, signal: u64) -> bool {
        self.signal_mask & abi::signal::bit(signal) != 0
    }

    fn queue_signal(&mut self, signal: u64) -> bool {
        let bit = abi::signal::bit(signal);
        if bit == 0 {
            return false;
        }
        let was_new = self.pending_signals & bit == 0;
        self.pending_signals |= bit;
        self.pending_signal_peak = self
            .pending_signal_peak
            .max(u64::from(self.pending_signals.count_ones()));
        was_new
    }

    fn clear_pending_signal(&mut self, signal: u64) {
        self.pending_signals &= !abi::signal::bit(signal);
    }

    fn reset_signal_actions_for_exec(&mut self) {
        for action in &mut self.signal_actions {
            if action.handler != abi::signal_action::Action::IGNORE.handler {
                *action = abi::signal_action::Action::DEFAULT;
            }
        }
        self.active_signal = None;
    }

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
            file_write_count: self.file_write_count,
            file_bytes_written: self.file_bytes_written,
            seek_count: self.seek_count,
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
            exec_count: self.exec_count,
            exec_failure_count: self.exec_failure_count,
            close_on_exec_count: self.close_on_exec_count,
            exec_frames_reclaimed: self.exec_frames_reclaimed,
            fork_count: self.fork_count,
            environment_count: self.environment.len(),
            environment_change_count: self.environment_change_count,
            cow_fault_count: self.cow_fault_count,
            cow_copy_count: self.cow_copy_count,
            signal_sent_count: self.signal_sent_count,
            signal_received_count: self.signal_received_count,
            signal_handler_count: self.signal_handler_count,
            signal_return_count: self.signal_return_count,
            signal_ignored_count: self.signal_ignored_count,
            signal_interrupted_syscall_count: self.signal_interrupted_syscall_count,
            signal_frame_failure_count: self.signal_frame_failure_count,
            pending_signal_peak: self.pending_signal_peak,
            stop_count: self.stop_count,
            continue_count: self.continue_count,
            pipe_pair_count: self.pipe_pair_count,
            pipe_descriptor_close_count: self.pipe_descriptor_close_count,
            pipe_descriptor_inherit_count: self.pipe_descriptor_inherit_count,
            file_descriptor_inherit_count: self.file_descriptor_inherit_count,
            scheduled_count,
            runtime_ticks,
            frames_reclaimed: frames_reclaimed.saturating_add(self.exec_frames_reclaimed as usize),
        })
    }
}

struct ProcessManager {
    next_process_id: u64,
    processes: Vec<Process>,
    completions: CompletionQueue<ProcessResult>,
    spawned: u64,
    child_spawns: u64,
    child_waits: u64,
    execs: u64,
    exec_failures: u64,
    forks: u64,
    fork_failures: u64,
    environment_changes: u64,
    cow_faults: u64,
    cow_copies: u64,
    signals_sent: u64,
    signal_handlers: u64,
    signal_returns: u64,
    signal_ignores: u64,
    signal_interruptions: u64,
    signal_frame_failures: u64,
    stop_deliveries: u64,
    continue_deliveries: u64,
    pipe_pairs: u64,
    pipe_descriptor_inherits: u64,
    file_descriptor_inherits: u64,
    exited: u64,
    faulted: u64,
    signaled: u64,
    reaped: u64,
    frames_reclaimed: u64,
}

impl ProcessManager {
    const fn new() -> Self {
        Self {
            next_process_id: INIT_PROCESS_ID,
            processes: Vec::new(),
            completions: CompletionQueue::new(PROCESS_HISTORY_LIMIT),
            spawned: 0,
            child_spawns: 0,
            child_waits: 0,
            execs: 0,
            exec_failures: 0,
            forks: 0,
            fork_failures: 0,
            environment_changes: 0,
            cow_faults: 0,
            cow_copies: 0,
            signals_sent: 0,
            signal_handlers: 0,
            signal_returns: 0,
            signal_ignores: 0,
            signal_interruptions: 0,
            signal_frame_failures: 0,
            stop_deliveries: 0,
            continue_deliveries: 0,
            pipe_pairs: 0,
            pipe_descriptor_inherits: 0,
            file_descriptor_inherits: 0,
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

    fn process_slots_in_use(&self) -> usize {
        self.processes
            .len()
            .saturating_add(self.completions.pending_len())
    }

    fn ensure_process_slot(&self) -> Result<(), Error> {
        if self.process_slots_in_use() >= MAX_PROCESS_SLOTS {
            Err(Error::ProcessLimitReached)
        } else {
            Ok(())
        }
    }

    fn take_result(&mut self, process_id: u64) -> Option<ProcessResult> {
        self.completions
            .take_pending_where(|result| result.process_id == process_id)
    }

    fn take_child_result(
        &mut self,
        parent_process_id: u64,
        child_process_id: u64,
    ) -> Option<ProcessResult> {
        self.completions.take_pending_where(|result| {
            result.process_id == child_process_id
                && result.parent_process_id == Some(parent_process_id)
        })
    }

    fn orphan_children_of(&mut self, parent_process_id: u64) {
        for child in &mut self.processes {
            if child.parent_process_id == Some(parent_process_id) {
                child.parent_process_id = Some(KERNEL_REAPER_PROCESS_ID);
            }
            if child.terminal_parent == Some(parent_process_id) {
                child.terminal_parent = None;
            }
        }
        self.completions
            .discard_pending_where(|result| result.parent_process_id == Some(parent_process_id));
    }

    fn record_result(&mut self, result: ProcessResult) {
        let waitable = result.parent_process_id != Some(KERNEL_REAPER_PROCESS_ID);
        self.completions.record(result, waitable);
    }

    fn snapshot(&self) -> ManagerSnapshot {
        let shared = SHARED_USER_FRAMES.lock();
        ManagerSnapshot {
            spawned: self.spawned,
            child_spawns: self.child_spawns,
            child_waits: self.child_waits,
            execs: self.execs,
            exec_failures: self.exec_failures,
            forks: self.forks,
            fork_failures: self.fork_failures,
            environment_changes: self.environment_changes,
            cow_faults: self.cow_faults,
            cow_copies: self.cow_copies,
            shared_frames: shared.frames.len(),
            shared_references: shared.total_references(),
            peak_shared_frames: shared.peak_frames,
            peak_shared_references: shared.peak_references,
            signals_sent: self.signals_sent,
            signal_handlers: self.signal_handlers,
            signal_returns: self.signal_returns,
            signal_ignores: self.signal_ignores,
            signal_interruptions: self.signal_interruptions,
            signal_frame_failures: self.signal_frame_failures,
            pending_signals: self
                .processes
                .iter()
                .map(|process| process.pending_signals.count_ones() as usize)
                .sum(),
            stop_deliveries: self.stop_deliveries,
            continue_deliveries: self.continue_deliveries,
            pipe_pairs: self.pipe_pairs,
            pipe_descriptor_inherits: self.pipe_descriptor_inherits,
            file_descriptor_inherits: self.file_descriptor_inherits,
            waitable_zombies: self.completions.pending_len(),
            process_limit: MAX_PROCESS_SLOTS,
            active: self
                .processes
                .iter()
                .filter(|process| {
                    matches!(
                        process.state,
                        ProcessState::Runnable | ProcessState::Blocked | ProcessState::Stopped
                    )
                })
                .count(),
            blocked: self
                .processes
                .iter()
                .filter(|process| process.state == ProcessState::Blocked)
                .count(),
            stopped: self
                .processes
                .iter()
                .filter(|process| process.state == ProcessState::Stopped)
                .count(),
            exited: self.exited,
            faulted: self.faulted,
            signaled: self.signaled,
            reaped: self.reaped,
            frames_reclaimed: self.frames_reclaimed,
            results: self.completions.history().iter().cloned().collect(),
        }
    }
}

#[derive(Clone, Copy)]
struct UserPage {
    virtual_address: u64,
    frame: PhysFrame<Size4KiB>,
    flags: PageTableFlags,
    copy_on_write: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamAccess {
    Read,
    Write,
}

fn resolve_stream_descriptor(
    process: &Process,
    descriptor: Option<u64>,
    access: StreamAccess,
) -> Result<Option<StreamTarget>, Error> {
    let Some(descriptor) = descriptor else {
        return Ok(None);
    };
    let direction = match access {
        StreamAccess::Read => PipeDirection::Reader,
        StreamAccess::Write => PipeDirection::Writer,
    };
    if let Some(pipe) = process
        .pipe_descriptors
        .iter()
        .find(|pipe| pipe.descriptor == descriptor && pipe.direction == direction)
    {
        return Ok(Some(StreamTarget::Pipe(pipe.pipe_id)));
    }
    if let Some(file) = process
        .open_files
        .iter()
        .find(|file| file.descriptor == descriptor)
    {
        let allowed = {
            let state = file.handle.lock();
            match access {
                StreamAccess::Read => state.readable,
                StreamAccess::Write => state.writable,
            }
        };
        if allowed {
            return Ok(Some(StreamTarget::File(file.handle.clone())));
        }
    }
    Err(Error::InvalidDescriptor(descriptor))
}

fn retain_stream_target(target: &Option<StreamTarget>, access: StreamAccess) -> Result<(), Error> {
    let Some(StreamTarget::Pipe(pipe_id)) = target else {
        return Ok(());
    };
    match access {
        StreamAccess::Read => pipe::retain_reader(*pipe_id)?,
        StreamAccess::Write => pipe::retain_writer(*pipe_id)?,
    }
    Ok(())
}

fn release_stream_target(target: Option<StreamTarget>, access: StreamAccess) {
    let Some(StreamTarget::Pipe(pipe_id)) = target else {
        return;
    };
    match access {
        StreamAccess::Read => {
            let _ = pipe::close_reader(pipe_id);
        }
        StreamAccess::Write => {
            let _ = pipe::close_writer(pipe_id);
        }
    }
}

fn stream_target_is_pipe(target: &Option<StreamTarget>) -> bool {
    matches!(target, Some(StreamTarget::Pipe(_)))
}
fn stream_target_is_file(target: &Option<StreamTarget>) -> bool {
    matches!(target, Some(StreamTarget::File(_)))
}

fn validate_kernel_context_pointer(process: &Process, context_pointer: usize) -> Result<(), Error> {
    let stack_start = process.kernel_stack.as_ptr() as usize;
    let stack_bytes = process
        .kernel_stack
        .len()
        .checked_mul(size_of::<u128>())
        .ok_or(Error::StackLayoutInvalid)?;
    let stack_top = stack_start
        .checked_add(stack_bytes)
        .ok_or(Error::StackLayoutInvalid)?;
    let expected_pointer = stack_top
        .checked_sub(size_of::<SavedContext>())
        .ok_or(Error::StackLayoutInvalid)?;
    let context_end = context_pointer
        .checked_add(size_of::<SavedContext>())
        .ok_or(Error::StackLayoutInvalid)?;
    if !stack_start.is_multiple_of(align_of::<u128>())
        || !stack_top.is_multiple_of(16)
        || !context_pointer.is_multiple_of(16)
        || context_pointer != expected_pointer
        || context_pointer < stack_start
        || context_end > stack_top
    {
        return Err(Error::StackLayoutInvalid);
    }
    Ok(())
}

impl BuiltAddressSpace {
    fn build(
        image: &Image,
        executable_bytes: &[u8],
        arguments: &[&str],
        environment: &[String],
        kernel_mapper: &mut OffsetPageTable<'_>,
        frame_allocator: &mut BootInfoFrameAllocator,
        physical_memory_offset: VirtAddr,
    ) -> Result<Self, Error> {
        let mut tracking = TrackingFrameAllocator::new(frame_allocator);
        let result = Self::build_tracked(
            image,
            executable_bytes,
            arguments,
            environment,
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
        image: &Image,
        executable_bytes: &[u8],
        arguments: &[&str],
        environment: &[String],
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
            copy_segment(executable_bytes, segment, physical_memory_offset, &pages)?;
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

        let stack_pointer = build_initial_stack(
            arguments,
            environment,
            physical_memory_offset,
            &pages,
            stack_start,
        )?;

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
    executable: &LoadedExecutable,
    kernel_mapper: &mut OffsetPageTable<'_>,
    frame_allocator: &mut BootInfoFrameAllocator,
    physical_memory_offset: VirtAddr,
) -> Result<SpawnInfo, Error> {
    spawn_with_args(
        path,
        task_name,
        executable,
        &[path],
        kernel_mapper,
        frame_allocator,
        physical_memory_offset,
    )
}

pub fn spawn_with_args(
    path: &str,
    task_name: &'static str,
    executable: &LoadedExecutable,
    arguments: &[&str],
    kernel_mapper: &mut OffsetPageTable<'_>,
    frame_allocator: &mut BootInfoFrameAllocator,
    physical_memory_offset: VirtAddr,
) -> Result<SpawnInfo, Error> {
    spawn_with_mode(
        SpawnRequest {
            path,
            task_name,
            executable,
            arguments,
            environment: &[],
            foreground: false,
            stdin_target: None,
            stdout_target: None,
            stderr_target: None,
            parent_process_id: None,
            terminal_parent: None,
            process_group_id: None,
        },
        kernel_mapper,
        frame_allocator,
        physical_memory_offset,
    )
}

struct SpawnRequest<'a> {
    path: &'a str,
    task_name: &'static str,
    executable: &'a LoadedExecutable,
    arguments: &'a [&'a str],
    environment: &'a [&'a str],
    foreground: bool,
    stdin_target: Option<StreamTarget>,
    stdout_target: Option<StreamTarget>,
    stderr_target: Option<StreamTarget>,
    parent_process_id: Option<u64>,
    terminal_parent: Option<u64>,
    process_group_id: Option<u64>,
}

fn spawn_with_mode(
    request: SpawnRequest<'_>,
    kernel_mapper: &mut OffsetPageTable<'_>,
    frame_allocator: &mut BootInfoFrameAllocator,
    physical_memory_offset: VirtAddr,
) -> Result<SpawnInfo, Error> {
    let SpawnRequest {
        path,
        task_name,
        executable,
        arguments,
        environment,
        foreground,
        stdin_target,
        stdout_target,
        stderr_target,
        parent_process_id,
        terminal_parent,
        process_group_id,
    } = request;
    if scheduler::current_task_kind() != scheduler::TaskKind::Bootstrap {
        return Err(Error::SchedulerNotOnBootstrapTask);
    }
    PROCESS_MANAGER.lock().ensure_process_slot()?;
    let inherited_job = parent_process_id.and_then(|parent_process_id| {
        PROCESS_MANAGER
            .lock()
            .processes
            .iter()
            .find(|process| process.process_id == parent_process_id)
            .and_then(|process| process.job)
    });

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
    if !kernel_stack_start.is_multiple_of(align_of::<u128>())
        || !kernel_stack_top.is_multiple_of(16)
        || !initial_stack_pointer.is_multiple_of(16)
    {
        return Err(Error::StackLayoutInvalid);
    }

    for address in [
        galactic_syscall_interrupt_entry as *const () as usize as u64,
        galactic_page_fault_interrupt_entry as *const () as usize as u64,
        galactic_general_protection_interrupt_entry as *const () as usize as u64,
        scheduler::timer_interrupt_entry_address().as_u64(),
        kernel_stack_start as u64,
        physical_memory_offset.as_u64(),
    ] {
        if pml4_index(address) == 0 {
            return Err(Error::KernelMappingUsesUserSlot(address));
        }
    }

    let environment = collect_environment(environment)?;
    let image = executable.image();
    let mut address_space = BuiltAddressSpace::build(
        image,
        executable.bytes(),
        arguments,
        &environment,
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
        job: inherited_job,
        terminal_parent,
        task_id: 0,
        path: path.to_string(),
        environment,
        state: ProcessState::Runnable,
        stopped_resume_state: None,
        last_stop_signal: None,
        pending_parent_status: None,
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
        stdin_target,
        stdout_target,
        stderr_target,
        pending_terminal_read: None,
        pending_pipe_read: None,
        pending_pipe_write: None,
        pending_tmpfs_proxy: None,
        pending_nullfs_proxy: None,
        pending_vfs_request: None,
        pending_child_spawn: None,
        pending_child_wait: None,
        pending_exec: None,
        pending_executable_load: None,
        pending_fork: None,
        pending_cow_fault: None,
        signal_actions: [abi::signal_action::Action::DEFAULT; SIGNAL_TABLE_SIZE],
        signal_mask: 0,
        pending_signals: 0,
        active_signal: None,
        pending_signal_peak: 0,
        syscall_count: 0,
        write_count: 0,
        yield_count: 0,
        bytes_written: 0,
        open_count: 0,
        read_count: 0,
        close_count: 0,
        bytes_read: 0,
        file_write_count: 0,
        file_bytes_written: 0,
        seek_count: 0,
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
        exec_count: 0,
        exec_failure_count: 0,
        close_on_exec_count: 0,
        exec_frames_reclaimed: 0,
        fork_count: 0,
        environment_change_count: 0,
        cow_fault_count: 0,
        cow_copy_count: 0,
        signal_sent_count: 0,
        signal_received_count: 0,
        signal_handler_count: 0,
        signal_return_count: 0,
        signal_ignored_count: 0,
        signal_interrupted_syscall_count: 0,
        signal_frame_failure_count: 0,
        stop_count: 0,
        continue_count: 0,
        pipe_pair_count: 0,
        pipe_descriptor_close_count: 0,
        pipe_descriptor_inherit_count: 0,
        file_descriptor_inherit_count: 0,
    });

    if let Some(job) = inherited_job
        && capability_job_add_member(job, process_id).is_err()
    {
        if let Some(mut process) = pending_process.take() {
            for frame in process.owned_frames.drain(..) {
                frame_allocator.deallocate_frame(frame);
            }
        }
        return Err(Error::JobLimitReached);
    }

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
            if let Some(job) = inherited_job {
                capability_job_remove_unstarted(job, process_id);
            }
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

#[derive(Clone)]
struct ForkSnapshot {
    path: String,
    environment: Vec<String>,
    process_group_id: u64,
    job: Option<CapabilityObjectRef>,
    entry_point: u64,
    mapped_pages: usize,
    load_segments: usize,
    guard_page_address: u64,
    ranges: Vec<UserRange>,
    pages: Vec<UserPage>,
    open_files: Vec<OpenFile>,
    pipe_descriptors: Vec<PipeDescriptor>,
    stdin_target: Option<StreamTarget>,
    stdout_target: Option<StreamTarget>,
    stderr_target: Option<StreamTarget>,
    signal_actions: [abi::signal_action::Action; SIGNAL_TABLE_SIZE],
    signal_mask: u64,
    context: SavedContext,
}

struct ForkAddressSpace {
    page_table_frame: PhysFrame<Size4KiB>,
    pages: Vec<UserPage>,
    page_table_frames: Vec<PhysFrame<Size4KiB>>,
}

fn page_table_mapper(
    page_table_address: u64,
    physical_memory_offset: VirtAddr,
) -> Result<OffsetPageTable<'static>, Error> {
    let frame: PhysFrame<Size4KiB> =
        PhysFrame::from_start_address(PhysAddr::new(page_table_address))
            .map_err(|_| Error::InvalidUserRange)?;
    let address = physical_memory_offset
        .as_u64()
        .checked_add(frame.start_address().as_u64())
        .ok_or(Error::AddressOverflow)?;
    let table = unsafe { &mut *(address as *mut PageTable) };
    Ok(unsafe { OffsetPageTable::new(table, physical_memory_offset) })
}

fn copy_frame(
    source: PhysFrame<Size4KiB>,
    destination: PhysFrame<Size4KiB>,
    physical_memory_offset: VirtAddr,
) -> Result<(), Error> {
    let source_address = physical_memory_offset
        .as_u64()
        .checked_add(source.start_address().as_u64())
        .ok_or(Error::AddressOverflow)?;
    let destination_address = physical_memory_offset
        .as_u64()
        .checked_add(destination.start_address().as_u64())
        .ok_or(Error::AddressOverflow)?;
    unsafe {
        ptr::copy_nonoverlapping(
            source_address as *const u8,
            destination_address as *mut u8,
            Size4KiB::SIZE as usize,
        );
    }
    Ok(())
}

fn clone_fork_address_space(
    parent: &ForkSnapshot,
    kernel_mapper: &mut OffsetPageTable<'_>,
    frame_allocator: &mut BootInfoFrameAllocator,
    physical_memory_offset: VirtAddr,
) -> Result<ForkAddressSpace, Error> {
    let mut tracking = TrackingFrameAllocator::new(frame_allocator);
    let result = (|| {
        let page_table_frame = tracking
            .allocate_frame()
            .ok_or(Error::FrameAllocationFailed)?;
        let table_address = physical_memory_offset
            .as_u64()
            .checked_add(page_table_frame.start_address().as_u64())
            .ok_or(Error::AddressOverflow)?;
        let table_pointer = table_address as *mut PageTable;
        unsafe { table_pointer.write(PageTable::new()) };
        let level_4_table = unsafe { &mut *table_pointer };
        for index in 1..512 {
            let source = &kernel_mapper.level_4_table()[index];
            if !source.is_unused() {
                level_4_table[index].set_addr(source.addr(), source.flags());
            }
        }
        let mut mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };
        let mut pages = Vec::with_capacity(parent.pages.len());
        for source in &parent.pages {
            let mut mapping_flags = source.flags;
            let copy_on_write =
                source.copy_on_write || source.flags.contains(PageTableFlags::WRITABLE);
            if copy_on_write {
                mapping_flags.remove(PageTableFlags::WRITABLE);
            }
            let page = Page::containing_address(VirtAddr::new(source.virtual_address));
            let flush = unsafe { mapper.map_to(page, source.frame, mapping_flags, &mut tracking) }
                .map_err(|error| map_error(error, source.virtual_address))?;
            flush.ignore();
            pages.push(UserPage {
                virtual_address: source.virtual_address,
                frame: source.frame,
                flags: source.flags,
                copy_on_write,
            });
        }
        Ok(ForkAddressSpace {
            page_table_frame,
            pages,
            page_table_frames: Vec::new(),
        })
    })();
    match result {
        Ok(mut address_space) => {
            address_space.page_table_frames = tracking.take_frames();
            Ok(address_space)
        }
        Err(error) => {
            tracking.reclaim_all();
            Err(error)
        }
    }
}

fn retain_pipe_descriptor(descriptor: PipeDescriptor) -> Result<(), Error> {
    match descriptor.direction {
        PipeDirection::Reader => pipe::retain_reader(descriptor.pipe_id)?,
        PipeDirection::Writer => pipe::retain_writer(descriptor.pipe_id)?,
    }
    Ok(())
}

fn release_pipe_descriptor(descriptor: PipeDescriptor) {
    match descriptor.direction {
        PipeDirection::Reader => {
            let _ = pipe::close_reader(descriptor.pipe_id);
        }
        PipeDirection::Writer => {
            let _ = pipe::close_writer(descriptor.pipe_id);
        }
    }
}

fn retain_fork_resources(snapshot: &ForkSnapshot) -> Result<(), Error> {
    let mut retained_descriptors = 0usize;
    for descriptor in &snapshot.pipe_descriptors {
        if let Err(error) = retain_pipe_descriptor(*descriptor) {
            for retained in &snapshot.pipe_descriptors[..retained_descriptors] {
                release_pipe_descriptor(*retained);
            }
            return Err(error);
        }
        retained_descriptors = retained_descriptors.saturating_add(1);
    }
    if let Err(error) = retain_stream_target(&snapshot.stdin_target, StreamAccess::Read) {
        for descriptor in &snapshot.pipe_descriptors {
            release_pipe_descriptor(*descriptor);
        }
        return Err(error);
    }
    if let Err(error) = retain_stream_target(&snapshot.stdout_target, StreamAccess::Write) {
        release_stream_target(snapshot.stdin_target.clone(), StreamAccess::Read);
        for descriptor in &snapshot.pipe_descriptors {
            release_pipe_descriptor(*descriptor);
        }
        return Err(error);
    }
    if let Err(error) = retain_stream_target(&snapshot.stderr_target, StreamAccess::Write) {
        release_stream_target(snapshot.stdout_target.clone(), StreamAccess::Write);
        release_stream_target(snapshot.stdin_target.clone(), StreamAccess::Read);
        for descriptor in &snapshot.pipe_descriptors {
            release_pipe_descriptor(*descriptor);
        }
        return Err(error);
    }
    Ok(())
}

fn release_fork_resources(snapshot: &ForkSnapshot) {
    release_stream_target(snapshot.stderr_target.clone(), StreamAccess::Write);
    release_stream_target(snapshot.stdout_target.clone(), StreamAccess::Write);
    release_stream_target(snapshot.stdin_target.clone(), StreamAccess::Read);
    for descriptor in &snapshot.pipe_descriptors {
        release_pipe_descriptor(*descriptor);
    }
}

fn protect_parent_pages_for_fork(
    process_id: u64,
    physical_memory_offset: VirtAddr,
) -> Result<usize, Error> {
    let (page_table_address, pages) = {
        let manager = PROCESS_MANAGER.lock();
        let process = manager
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
            .ok_or(Error::ProcessNotFound(process_id))?;
        (process.page_table_address, process.pages.clone())
    };
    let mut mapper = page_table_mapper(page_table_address, physical_memory_offset)?;
    let mut protected = 0usize;
    for page_info in &pages {
        if page_info.copy_on_write || !page_info.flags.contains(PageTableFlags::WRITABLE) {
            continue;
        }
        let mut flags = page_info.flags;
        flags.remove(PageTableFlags::WRITABLE);
        let page: Page<Size4KiB> =
            Page::containing_address(VirtAddr::new(page_info.virtual_address));
        let flush =
            unsafe { mapper.update_flags(page, flags) }.map_err(|_| Error::InvalidUserRange)?;
        flush.ignore();
        protected = protected.saturating_add(1);
    }
    if protected != 0 {
        let mut manager = PROCESS_MANAGER.lock();
        let process = manager
            .process_mut(process_id)
            .ok_or(Error::ProcessNotFound(process_id))?;
        for page in &mut process.pages {
            if page.flags.contains(PageTableFlags::WRITABLE) {
                page.copy_on_write = true;
            }
        }
    }
    Ok(protected)
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
        self.spawn_mode(path, arguments, &[], false)
    }

    pub fn spawn_foreground(&mut self, path: &str, arguments: &[&str]) -> Result<SpawnInfo, Error> {
        self.spawn_mode(path, arguments, &[], true)
    }

    pub fn spawn_foreground_with_environment(
        &mut self,
        path: &str,
        arguments: &[&str],
        environment: &[&str],
    ) -> Result<SpawnInfo, Error> {
        self.spawn_mode(path, arguments, environment, true)
    }

    fn spawn_mode(
        &mut self,
        path: &str,
        arguments: &[&str],
        environment: &[&str],
        foreground: bool,
    ) -> Result<SpawnInfo, Error> {
        let executable = elf::load(path)?;
        let mut argv = Vec::with_capacity(arguments.len().saturating_add(1));
        argv.push(path);
        argv.extend_from_slice(arguments);
        spawn_with_mode(
            SpawnRequest {
                path,
                task_name: SHELL_PROCESS_TASK_NAME,
                executable: &executable,
                arguments: &argv,
                environment,
                foreground,
                stdin_target: None,
                stdout_target: None,
                stderr_target: None,
                parent_process_id: None,
                terminal_parent: None,
                process_group_id: None,
            },
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
        let executable = elf::load(path)?;
        let mut argv = Vec::with_capacity(arguments.len().saturating_add(1));
        argv.push(path);
        argv.extend_from_slice(arguments);
        spawn_with_mode(
            SpawnRequest {
                path,
                task_name: SHELL_PROCESS_TASK_NAME,
                executable: &executable,
                arguments: &argv,
                environment: &[],
                foreground: false,
                stdin_target: stdin_pipe.map(StreamTarget::Pipe),
                stdout_target: stdout_pipe.map(StreamTarget::Pipe),
                stderr_target: None,
                parent_process_id: None,
                terminal_parent: None,
                process_group_id: None,
            },
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

    fn spawn_child_loaded(
        &mut self,
        parent_process_id: u64,
        request: &PendingChildSpawn,
        executable: &LoadedExecutable,
    ) -> Result<SpawnInfo, Error> {
        let (stdin_target, stdout_target, stderr_target, process_group_id, environment) =
            cpu_interrupts::without_interrupts(|| {
                let manager = PROCESS_MANAGER.lock();
                let parent = manager
                    .processes
                    .iter()
                    .find(|process| process.process_id == parent_process_id)
                    .ok_or(Error::ProcessNotFound(parent_process_id))?;
                if parent.state != ProcessState::Blocked
                    || parent.pending_child_spawn.as_ref().is_none_or(|pending| {
                        !pending.claimed
                            || pending.stack_pointer != request.stack_pointer
                            || pending.path != request.path
                    })
                {
                    return Err(Error::InvalidArgument);
                }
                let stdin_target = resolve_stream_descriptor(
                    parent,
                    request.stdin_descriptor,
                    StreamAccess::Read,
                )?;
                let stdout_target = resolve_stream_descriptor(
                    parent,
                    request.stdout_descriptor,
                    StreamAccess::Write,
                )?;
                let stderr_target = resolve_stream_descriptor(
                    parent,
                    request.stderr_descriptor,
                    StreamAccess::Write,
                )?;
                let inherited_group = parent.process_group_id;
                let process_group_id = if request.new_process_group {
                    None
                } else if let Some(group_id) = request.process_group_id {
                    let owned = manager.processes.iter().any(|process| {
                        process.parent_process_id == Some(parent_process_id)
                            && process.process_group_id == group_id
                            && process.is_live()
                    });
                    if !owned {
                        return Err(Error::InvalidProcessGroup(group_id));
                    }
                    Some(group_id)
                } else {
                    Some(inherited_group)
                };
                Ok::<_, Error>((
                    stdin_target,
                    stdout_target,
                    stderr_target,
                    process_group_id,
                    parent.environment.clone(),
                ))
            })?;
        retain_stream_target(&stdin_target, StreamAccess::Read)?;
        if let Err(error) = retain_stream_target(&stdout_target, StreamAccess::Write) {
            release_stream_target(stdin_target, StreamAccess::Read);
            return Err(error);
        }
        if let Err(error) = retain_stream_target(&stderr_target, StreamAccess::Write) {
            release_stream_target(stdout_target, StreamAccess::Write);
            release_stream_target(stdin_target, StreamAccess::Read);
            return Err(error);
        }
        let mut argv = Vec::with_capacity(request.arguments.len().saturating_add(1));
        argv.push(request.path.as_str());
        argv.extend(request.arguments.iter().map(String::as_str));
        let environment: Vec<&str> = environment.iter().map(String::as_str).collect();
        let result = spawn_with_mode(
            SpawnRequest {
                path: &request.path,
                task_name: SHELL_PROCESS_TASK_NAME,
                executable,
                arguments: &argv,
                environment: &environment,
                foreground: request.foreground,
                stdin_target: stdin_target.clone(),
                stdout_target: stdout_target.clone(),
                stderr_target: stderr_target.clone(),
                parent_process_id: Some(parent_process_id),
                terminal_parent: request.foreground.then_some(parent_process_id),
                process_group_id,
            },
            &mut self.mapper,
            &mut self.frame_allocator,
            self.physical_memory_offset,
        );
        if result.is_err() {
            release_stream_target(stderr_target, StreamAccess::Write);
            release_stream_target(stdout_target, StreamAccess::Write);
            release_stream_target(stdin_target, StreamAccess::Read);
            return result;
        }
        let pipe_inherited = u64::from(stream_target_is_pipe(&stdin_target))
            + u64::from(stream_target_is_pipe(&stdout_target))
            + u64::from(stream_target_is_pipe(&stderr_target));
        let file_inherited = u64::from(stream_target_is_file(&stdin_target))
            + u64::from(stream_target_is_file(&stdout_target))
            + u64::from(stream_target_is_file(&stderr_target));
        if pipe_inherited > 0 || file_inherited > 0 {
            cpu_interrupts::without_interrupts(|| {
                let mut manager = PROCESS_MANAGER.lock();
                let updated = if let Some(parent) = manager.process_mut(parent_process_id) {
                    parent.pipe_descriptor_inherit_count = parent
                        .pipe_descriptor_inherit_count
                        .saturating_add(pipe_inherited);
                    parent.file_descriptor_inherit_count = parent
                        .file_descriptor_inherit_count
                        .saturating_add(file_inherited);
                    true
                } else {
                    false
                };
                if updated {
                    manager.pipe_descriptor_inherits = manager
                        .pipe_descriptor_inherits
                        .saturating_add(pipe_inherited);
                    manager.file_descriptor_inherits = manager
                        .file_descriptor_inherits
                        .saturating_add(file_inherited);
                }
            });
        }
        result
    }

    fn fork_process(&mut self, parent_process_id: u64, request: PendingFork) -> Result<u64, Error> {
        PROCESS_MANAGER.lock().ensure_process_slot()?;
        let snapshot = {
            let manager = PROCESS_MANAGER.lock();
            let parent = manager
                .processes
                .iter()
                .find(|process| process.process_id == parent_process_id)
                .ok_or(Error::ProcessNotFound(parent_process_id))?;
            if parent.state != ProcessState::Blocked || parent.pending_fork.is_none() {
                return Err(Error::InvalidArgument);
            }
            validate_kernel_context_pointer(parent, request.stack_pointer)?;
            ForkSnapshot {
                path: parent.path.clone(),
                environment: parent.environment.clone(),
                process_group_id: parent.process_group_id,
                job: parent.job,
                entry_point: parent.entry_point,
                mapped_pages: parent.mapped_pages,
                load_segments: parent.load_segments,
                guard_page_address: parent.guard_page_address,
                ranges: parent.ranges.clone(),
                pages: parent.pages.clone(),
                open_files: parent.open_files.clone(),
                pipe_descriptors: parent.pipe_descriptors.clone(),
                stdin_target: parent.stdin_target.clone(),
                stdout_target: parent.stdout_target.clone(),
                stderr_target: parent.stderr_target.clone(),
                signal_actions: parent.signal_actions,
                signal_mask: parent.signal_mask,
                context: unsafe { *(request.stack_pointer as *const SavedContext) },
            }
        };

        let mut address_space = clone_fork_address_space(
            &snapshot,
            &mut self.mapper,
            &mut self.frame_allocator,
            self.physical_memory_offset,
        )?;
        if let Err(error) = retain_fork_resources(&snapshot) {
            for frame in address_space.page_table_frames.drain(..) {
                self.frame_allocator.deallocate_frame(frame);
            }
            return Err(error);
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
        let child_stack_pointer = kernel_stack_top
            .checked_sub(size_of::<SavedContext>())
            .ok_or(Error::StackLayoutInvalid)?;
        if !kernel_stack_start.is_multiple_of(align_of::<u128>())
            || !kernel_stack_top.is_multiple_of(16)
            || !child_stack_pointer.is_multiple_of(16)
        {
            release_fork_resources(&snapshot);
            for frame in address_space.page_table_frames.drain(..) {
                self.frame_allocator.deallocate_frame(frame);
            }
            return Err(Error::StackLayoutInvalid);
        }
        let mut child_context = snapshot.context;
        child_context.rax = 0;
        unsafe { (child_stack_pointer as *mut SavedContext).write(child_context) };

        let child_process_id = PROCESS_MANAGER.lock().allocate_process_id();
        if let Some(job) = snapshot.job
            && capability_job_add_member(job, child_process_id).is_err()
        {
            release_fork_resources(&snapshot);
            for frame in address_space.page_table_frames.drain(..) {
                self.frame_allocator.deallocate_frame(frame);
            }
            return Err(Error::JobLimitReached);
        }
        let page_table_address = address_space.page_table_frame.start_address().as_u64();
        let mut owned_frames = core::mem::take(&mut address_space.page_table_frames);
        owned_frames.extend(snapshot.pages.iter().map(|page| page.frame));

        let task_result = cpu_interrupts::without_interrupts(|| -> Result<u64, Error> {
            let _ = protect_parent_pages_for_fork(parent_process_id, self.physical_memory_offset)?;
            let task_id = scheduler::spawn_user_process(
                SHELL_PROCESS_TASK_NAME,
                child_process_id,
                child_stack_pointer,
                VirtAddr::new(kernel_stack_top as u64),
                kernel_stack_bytes,
                page_table_address,
            )?;
            for page in &snapshot.pages {
                retain_shared_frame(page.frame);
            }

            let child = Process {
                process_id: child_process_id,
                parent_process_id: Some(parent_process_id),
                process_group_id: snapshot.process_group_id,
                job: snapshot.job,
                terminal_parent: None,
                task_id,
                path: snapshot.path.clone(),
                environment: snapshot.environment.clone(),
                state: ProcessState::Runnable,
                stopped_resume_state: None,
                last_stop_signal: None,
                pending_parent_status: None,
                termination: None,
                page_table_address,
                entry_point: snapshot.entry_point,
                mapped_pages: snapshot.mapped_pages,
                load_segments: snapshot.load_segments,
                guard_page_address: snapshot.guard_page_address,
                ranges: snapshot.ranges.clone(),
                pages: core::mem::take(&mut address_space.pages),
                kernel_stack,
                owned_frames,
                open_files: snapshot.open_files.clone(),
                pipe_descriptors: snapshot.pipe_descriptors.clone(),
                stdin_target: snapshot.stdin_target.clone(),
                stdout_target: snapshot.stdout_target.clone(),
                stderr_target: snapshot.stderr_target.clone(),
                pending_terminal_read: None,
                pending_pipe_read: None,
                pending_pipe_write: None,
                pending_tmpfs_proxy: None,
                pending_nullfs_proxy: None,
                pending_vfs_request: None,
                pending_child_spawn: None,
                pending_child_wait: None,
                pending_exec: None,
                pending_executable_load: None,
                pending_fork: None,
                pending_cow_fault: None,
                signal_actions: snapshot.signal_actions,
                signal_mask: snapshot.signal_mask,
                pending_signals: 0,
                active_signal: None,
                pending_signal_peak: 0,
                syscall_count: 0,
                write_count: 0,
                yield_count: 0,
                bytes_written: 0,
                open_count: 0,
                read_count: 0,
                close_count: 0,
                bytes_read: 0,
                file_write_count: 0,
                file_bytes_written: 0,
                seek_count: 0,
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
                exec_count: 0,
                exec_failure_count: 0,
                close_on_exec_count: 0,
                exec_frames_reclaimed: 0,
                fork_count: 0,
                environment_change_count: 0,
                cow_fault_count: 0,
                cow_copy_count: 0,
                signal_sent_count: 0,
                signal_received_count: 0,
                signal_handler_count: 0,
                signal_return_count: 0,
                signal_ignored_count: 0,
                signal_interrupted_syscall_count: 0,
                signal_frame_failure_count: 0,
                stop_count: 0,
                continue_count: 0,
                pipe_pair_count: 0,
                pipe_descriptor_close_count: 0,
                pipe_descriptor_inherit_count: snapshot.pipe_descriptors.len() as u64,
                file_descriptor_inherit_count: snapshot.open_files.len() as u64,
            };

            let mut manager = PROCESS_MANAGER.lock();
            let parent = manager
                .process_mut(parent_process_id)
                .ok_or(Error::ProcessNotFound(parent_process_id))?;
            let registers = unsafe { &mut *(request.stack_pointer as *mut SavedRegisters) };
            registers.rax = child_process_id;
            parent.pending_fork = None;
            parent.make_runnable();
            parent.fork_count = parent.fork_count.saturating_add(1);
            manager.forks = manager.forks.saturating_add(1);
            manager.spawned = manager.spawned.saturating_add(1);
            manager.processes.push(child);
            Ok(task_id)
        });

        if let Err(error) = task_result {
            if let Some(job) = snapshot.job {
                capability_job_remove_unstarted(job, child_process_id);
            }
            release_fork_resources(&snapshot);
            for frame in address_space.page_table_frames.drain(..) {
                self.frame_allocator.deallocate_frame(frame);
            }
            return Err(error);
        }
        if !scheduler::wake_process(parent_process_id) {
            return Err(Error::ProcessNotFound(parent_process_id));
        }
        crate::serial_println!(
            "userspace process forked: parent={}, child={}, group={}, shared_pages={}",
            parent_process_id,
            child_process_id,
            snapshot.process_group_id,
            snapshot.pages.len()
        );
        Ok(child_process_id)
    }

    fn service_fork_requests(&mut self) -> Result<usize, Error> {
        let requests: Vec<(u64, PendingFork)> = cpu_interrupts::without_interrupts(|| {
            PROCESS_MANAGER
                .lock()
                .processes
                .iter()
                .filter_map(|process| {
                    process
                        .pending_fork
                        .map(|request| (process.process_id, request))
                })
                .collect()
        });
        let mut completed = 0usize;
        for (parent_process_id, request) in requests {
            if let Err(error) = self.fork_process(parent_process_id, request) {
                let return_value = error_return(process_error_number(&error));
                let should_wake = cpu_interrupts::without_interrupts(|| {
                    let mut manager = PROCESS_MANAGER.lock();
                    let Some(parent) = manager.process_mut(parent_process_id) else {
                        return false;
                    };
                    if parent.state != ProcessState::Blocked || parent.pending_fork.is_none() {
                        return false;
                    }
                    let registers = unsafe { &mut *(request.stack_pointer as *mut SavedRegisters) };
                    registers.rax = return_value;
                    parent.pending_fork = None;
                    parent.make_runnable();
                    manager.fork_failures = manager.fork_failures.saturating_add(1);
                    true
                });
                if should_wake && !scheduler::wake_process(parent_process_id) {
                    return Err(Error::ProcessNotFound(parent_process_id));
                }
                crate::serial_println!(
                    "userspace process fork failed: parent={}, error={}",
                    parent_process_id,
                    error
                );
            }
            completed = completed.saturating_add(1);
        }
        Ok(completed)
    }

    fn make_cow_page_private(&mut self, process_id: u64, page_address: u64) -> Result<bool, Error> {
        let (page_table_address, page_info) = {
            let manager = PROCESS_MANAGER.lock();
            let process = manager
                .processes
                .iter()
                .find(|process| process.process_id == process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            let page = process
                .pages
                .iter()
                .find(|page| page.virtual_address == page_address && page.copy_on_write)
                .copied()
                .ok_or(Error::InvalidUserRange)?;
            (process.page_table_address, page)
        };
        let references = shared_frame_references(page_info.frame);
        let page = Page::containing_address(VirtAddr::new(page_address));
        let mut mapper = page_table_mapper(page_table_address, self.physical_memory_offset)?;
        let mut copied = false;
        let replacement = if references > 1 {
            let new_frame = self
                .frame_allocator
                .allocate_frame()
                .ok_or(Error::FrameAllocationFailed)?;
            copy_frame(page_info.frame, new_frame, self.physical_memory_offset)?;
            let (old_frame, flush) = mapper.unmap(page).map_err(|_| Error::InvalidUserRange)?;
            flush.ignore();
            if old_frame != page_info.frame {
                self.frame_allocator.deallocate_frame(new_frame);
                return Err(Error::InvalidUserRange);
            }
            let flush = unsafe {
                mapper.map_to(page, new_frame, page_info.flags, &mut self.frame_allocator)
            }
            .map_err(|error| map_error(error, page_address))?;
            flush.ignore();
            copied = true;
            new_frame
        } else {
            let (old_frame, flush) = mapper.unmap(page).map_err(|_| Error::InvalidUserRange)?;
            flush.ignore();
            if old_frame != page_info.frame {
                return Err(Error::InvalidUserRange);
            }
            let flush = unsafe {
                mapper.map_to(page, old_frame, page_info.flags, &mut self.frame_allocator)
            }
            .map_err(|error| map_error(error, page_address))?;
            flush.ignore();
            old_frame
        };

        let mut manager = PROCESS_MANAGER.lock();
        {
            let process = manager
                .process_mut(process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            let page_index = process
                .pages
                .iter()
                .position(|page| page.virtual_address == page_address)
                .ok_or(Error::InvalidUserRange)?;
            if copied {
                let owned = process
                    .owned_frames
                    .iter_mut()
                    .find(|frame| **frame == page_info.frame)
                    .ok_or(Error::InvalidUserRange)?;
                *owned = replacement;
                let _ = release_owned_frame(page_info.frame, &mut self.frame_allocator);
                process.cow_copy_count = process.cow_copy_count.saturating_add(1);
            }
            process.pages[page_index].frame = replacement;
            process.pages[page_index].copy_on_write = false;
        }
        if copied {
            manager.cow_copies = manager.cow_copies.saturating_add(1);
        }
        Ok(copied)
    }

    fn resolve_cow_page(&mut self, process_id: u64, page_address: u64) -> Result<bool, Error> {
        let copied = self.make_cow_page_private(process_id, page_address)?;
        {
            let mut manager = PROCESS_MANAGER.lock();
            let process = manager
                .process_mut(process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            process.pending_cow_fault = None;
            process.make_runnable();
        }
        if !scheduler::wake_process(process_id) {
            return Err(Error::ProcessNotFound(process_id));
        }
        Ok(copied)
    }

    fn service_cow_faults(&mut self) -> Result<usize, Error> {
        let faults: Vec<(u64, PendingCowFault)> = cpu_interrupts::without_interrupts(|| {
            PROCESS_MANAGER
                .lock()
                .processes
                .iter()
                .filter_map(|process| {
                    process
                        .pending_cow_fault
                        .map(|fault| (process.process_id, fault))
                })
                .collect()
        });
        let mut completed = 0usize;
        for (process_id, fault) in faults {
            let copied = self.resolve_cow_page(process_id, fault.page_address)?;
            crate::serial_println!(
                "userspace copy-on-write resolved: pid={}, page={:#018x}, copied={}",
                process_id,
                fault.page_address,
                copied
            );
            completed = completed.saturating_add(1);
        }
        Ok(completed)
    }
    fn prepare_signal_frame_pages(
        &mut self,
        process_id: u64,
        address: u64,
        length: usize,
    ) -> Result<(), Error> {
        if length == 0 {
            return Ok(());
        }
        let end = address
            .checked_add(length as u64)
            .and_then(|end| end.checked_sub(1))
            .ok_or(Error::AddressOverflow)?;
        let first_page = align_down(address);
        let last_page = align_down(end);
        let copy_on_write_pages = {
            let manager = PROCESS_MANAGER.lock();
            let process = manager
                .processes
                .iter()
                .find(|process| process.process_id == process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            process
                .pages
                .iter()
                .filter(|page| {
                    page.copy_on_write
                        && page.virtual_address >= first_page
                        && page.virtual_address <= last_page
                })
                .map(|page| page.virtual_address)
                .collect::<Vec<_>>()
        };
        for page_address in copy_on_write_pages {
            let _ = self.make_cow_page_private(process_id, page_address)?;
        }
        Ok(())
    }

    fn interrupt_signal_wait(&mut self, process_id: u64) -> Result<bool, Error> {
        #[derive(Clone)]
        enum InterruptedWait {
            Terminal,
            PipeRead(PipeId),
            PipeWrite(PipeId),
            TmpfsProxy(PendingTmpfsProxyRequest),
            NullfsProxy(Box<PendingNullfsProxyRequest>),
            VfsRequest(PendingVfsRequest),
            Child,
        }

        let interrupted =
            cpu_interrupts::without_interrupts(|| -> Result<Option<InterruptedWait>, Error> {
                let mut manager = PROCESS_MANAGER.lock();
                let process = manager
                    .process_mut(process_id)
                    .ok_or(Error::ProcessNotFound(process_id))?;
                if process.state != ProcessState::Blocked {
                    return Ok(None);
                }
                let (stack_pointer, kind) =
                    if let Some(pending) = process.pending_terminal_read.take() {
                        (pending.stack_pointer, InterruptedWait::Terminal)
                    } else if let Some(pending) = process.pending_pipe_read.take() {
                        (
                            pending.stack_pointer,
                            InterruptedWait::PipeRead(pending.pipe_id),
                        )
                    } else if let Some(pending) = process.pending_pipe_write.take() {
                        (
                            pending.stack_pointer,
                            InterruptedWait::PipeWrite(pending.pipe_id),
                        )
                    } else if let Some(pending) = process.pending_tmpfs_proxy.take() {
                        (pending.stack_pointer, InterruptedWait::TmpfsProxy(pending))
                    } else if process
                        .pending_nullfs_proxy
                        .as_ref()
                        .is_some_and(|pending| {
                            nullfs_proxy_executable_owner(&pending.operation).is_none()
                        })
                    {
                        let pending = process
                            .pending_nullfs_proxy
                            .take()
                            .expect("checked pending NullFS request");
                        (
                            pending.stack_pointer,
                            InterruptedWait::NullfsProxy(Box::new(pending)),
                        )
                    } else if process.pending_vfs_request.as_ref().is_some_and(|pending| {
                        !matches!(
                            pending.operation,
                            PendingVfsOperation::LoadExecutable { .. }
                        )
                    }) {
                        let pending = process
                            .pending_vfs_request
                            .take()
                            .expect("checked pending VFS request");
                        (pending.stack_pointer, InterruptedWait::VfsRequest(pending))
                    } else if let Some(pending) = process.pending_child_wait.take() {
                        (pending.stack_pointer, InterruptedWait::Child)
                    } else {
                        return Ok(None);
                    };
                if !matches!(&kind, InterruptedWait::VfsRequest(_)) {
                    let error = match &kind {
                        InterruptedWait::NullfsProxy(pending)
                            if nullfs_proxy_request_is_mutating(pending) =>
                        {
                            ERR_IO
                        }
                        _ => ERR_INTERRUPTED,
                    };
                    let registers = unsafe { &mut *(stack_pointer as *mut SavedRegisters) };
                    registers.rax = error_return(error);
                }
                process.make_runnable();
                process.signal_interrupted_syscall_count =
                    process.signal_interrupted_syscall_count.saturating_add(1);
                manager.signal_interruptions = manager.signal_interruptions.saturating_add(1);
                Ok(Some(kind))
            })?;

        let Some(interrupted) = interrupted else {
            return Ok(false);
        };
        if let InterruptedWait::VfsRequest(pending) = &interrupted {
            scheduler::with_process_address_space(process_id, || {
                let registers = unsafe { &mut *(pending.stack_pointer as *mut SavedRegisters) };
                registers.rax = error_return(ERR_INTERRUPTED);
            })
            .ok_or(Error::ProcessNotFound(process_id))?;
        }
        if !scheduler::wake_process(process_id) {
            return Err(Error::ProcessNotFound(process_id));
        }
        match interrupted {
            InterruptedWait::Terminal => terminal::note_wakeup(),
            InterruptedWait::PipeRead(pipe_id) => {
                let _ = pipe::note_reader_wakeup(pipe_id);
            }
            InterruptedWait::PipeWrite(pipe_id) => {
                let _ = pipe::note_writer_wakeup(pipe_id);
            }
            InterruptedWait::TmpfsProxy(pending) => {
                tmpfs_proxy_abandon_pending(pending);
            }
            InterruptedWait::NullfsProxy(pending) => {
                nullfs_proxy_abandon_pending(*pending);
            }
            InterruptedWait::VfsRequest(pending) => vfs_request_release(&pending),
            InterruptedWait::Child => {}
        }
        Ok(true)
    }

    fn install_signal_frame(
        &mut self,
        process_id: u64,
        signal: u64,
        action: abi::signal_action::Action,
    ) -> Result<(), Error> {
        let kernel_stack_pointer = scheduler::process_stack_pointer(process_id)
            .ok_or(Error::ProcessNotFound(process_id))?;
        let saved_context = unsafe { *(kernel_stack_pointer as *const SavedContext) };
        let frame_size = size_of::<abi::signal_action::Frame>();
        let frame_limit = saved_context
            .stack_pointer
            .checked_sub(SIGNAL_RED_ZONE_BYTES)
            .ok_or(Error::StackLayoutInvalid)?;
        let unaligned = frame_limit
            .checked_sub(frame_size as u64)
            .ok_or(Error::StackLayoutInvalid)?;
        let frame_address = (unaligned & !0xf)
            .checked_sub(8)
            .ok_or(Error::StackLayoutInvalid)?;
        if frame_address & 0xf != 8
            || !user_range_allows(process_id, frame_address, frame_size, true)
        {
            return Err(Error::InvalidUserRange);
        }

        self.prepare_signal_frame_pages(process_id, frame_address, frame_size)?;
        let previous_mask = {
            let manager = PROCESS_MANAGER.lock();
            let process = manager
                .processes
                .iter()
                .find(|process| process.process_id == process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            if process.state != ProcessState::Runnable || process.active_signal.is_some() {
                return Err(Error::InvalidArgument);
            }
            process.signal_mask
        };
        let cookie = abi::signal_action::FRAME_MAGIC
            ^ process_id.rotate_left(17)
            ^ frame_address.rotate_right(7)
            ^ signal.rotate_left(3);
        let frame = abi::signal_action::Frame {
            return_address: action.restorer,
            magic: abi::signal_action::FRAME_MAGIC,
            signal,
            previous_mask,
            cookie,
        };
        let frame_bytes = unsafe {
            slice::from_raw_parts(
                (&frame as *const abi::signal_action::Frame).cast::<u8>(),
                frame_size,
            )
        };
        {
            let manager = PROCESS_MANAGER.lock();
            let process = manager
                .processes
                .iter()
                .find(|process| process.process_id == process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            write_user_bytes(
                frame_address,
                frame_bytes,
                self.physical_memory_offset,
                &process.pages,
            )?;
        }

        unsafe {
            let context = &mut *(kernel_stack_pointer as *mut SavedContext);
            context.rip = action.handler;
            context.stack_pointer = frame_address;
            context.rdi = signal;
            context.rsi = frame_address;
            context.rflags &= !RFLAGS_DIRECTION;
        }
        let mut manager = PROCESS_MANAGER.lock();
        let process = manager
            .process_mut(process_id)
            .ok_or(Error::ProcessNotFound(process_id))?;
        process.clear_pending_signal(signal);
        process.signal_mask =
            (previous_mask | action.mask | abi::signal::bit(signal)) & !SIGNAL_UNBLOCKABLE_MASK;
        process.active_signal = Some(ActiveSignalFrame {
            signal,
            previous_mask,
            saved_context,
            frame_address,
            cookie,
            restorer: action.restorer,
        });
        if action.flags & abi::signal_action::RESET_HANDLER != 0 {
            process.signal_actions[signal as usize] = abi::signal_action::Action::DEFAULT;
        }
        process.signal_handler_count = process.signal_handler_count.saturating_add(1);
        manager.signal_handlers = manager.signal_handlers.saturating_add(1);
        Ok(())
    }

    fn service_pending_signals(&mut self) -> Result<usize, Error> {
        let candidates = {
            let manager = PROCESS_MANAGER.lock();
            manager
                .processes
                .iter()
                .filter_map(|process| {
                    if !process.is_live() || process.state == ProcessState::Stopped {
                        return None;
                    }
                    let deliverable = process.pending_signals & !process.signal_mask;
                    if deliverable == 0 {
                        return None;
                    }
                    let signal = u64::from(deliverable.trailing_zeros()) + 1;
                    Some((
                        process.process_id,
                        process.state,
                        signal,
                        process.signal_action(signal),
                        process.active_signal.is_some(),
                    ))
                })
                .collect::<Vec<_>>()
        };

        let mut completed = 0usize;
        for (process_id, state, signal, action, active) in candidates {
            if action.handler == abi::signal_action::IGNORE {
                let mut manager = PROCESS_MANAGER.lock();
                if let Some(process) = manager.process_mut(process_id) {
                    process.clear_pending_signal(signal);
                    process.signal_ignored_count = process.signal_ignored_count.saturating_add(1);
                    manager.signal_ignores = manager.signal_ignores.saturating_add(1);
                    completed = completed.saturating_add(1);
                }
                continue;
            }

            if action.handler == abi::signal_action::DEFAULT {
                {
                    let mut manager = PROCESS_MANAGER.lock();
                    if let Some(process) = manager.process_mut(process_id) {
                        process.clear_pending_signal(signal);
                    }
                }
                match default_signal_action(signal) {
                    Some(DefaultSignalAction::Terminate) => {
                        let _ = terminate_process_with_signal(process_id, signal, false);
                    }
                    Some(DefaultSignalAction::Stop) => {
                        let process_group_id = PROCESS_MANAGER
                            .lock()
                            .processes
                            .iter()
                            .find(|process| process.process_id == process_id)
                            .map(|process| process.process_group_id);
                        if stop_process_with_signal(process_id, signal, false)
                            && let Some(process_group_id) = process_group_id
                        {
                            restore_group_terminal(process_group_id);
                        }
                    }
                    Some(DefaultSignalAction::Continue) => {
                        let _ = continue_process_for_signal(process_id, false);
                    }
                    None => {}
                }
                completed = completed.saturating_add(1);
                continue;
            }

            if active {
                continue;
            }
            if state == ProcessState::Blocked && !self.interrupt_signal_wait(process_id)? {
                continue;
            }
            match self.install_signal_frame(process_id, signal, action) {
                Ok(()) => {
                    crate::serial_println!(
                        "userspace signal handler entered: pid={}, signal={}, handler={:#018x}",
                        process_id,
                        signal,
                        action.handler
                    );
                }
                Err(error) => {
                    {
                        let mut manager = PROCESS_MANAGER.lock();
                        if let Some(process) = manager.process_mut(process_id) {
                            process.signal_frame_failure_count =
                                process.signal_frame_failure_count.saturating_add(1);
                        }
                        manager.signal_frame_failures =
                            manager.signal_frame_failures.saturating_add(1);
                    }
                    let _ = terminate_process_with_signal(process_id, signal, false);
                    crate::serial_println!(
                        "userspace signal frame failed: pid={}, signal={}, error={}",
                        process_id,
                        signal,
                        error
                    );
                }
            }
            completed = completed.saturating_add(1);
        }
        Ok(completed)
    }

    fn replace_process_image(
        &mut self,
        process_id: u64,
        mut request: PendingExec,
        executable: &LoadedExecutable,
    ) -> Result<(), Error> {
        let context_pointer = request.stack_pointer;
        let environment = {
            let manager = PROCESS_MANAGER.lock();
            let process = manager
                .processes
                .iter()
                .find(|process| process.process_id == process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            if process.state != ProcessState::Blocked || process.pending_exec.is_none() {
                return Err(Error::InvalidArgument);
            }
            validate_kernel_context_pointer(process, context_pointer)?;
            process.environment.clone()
        };

        let image = executable.image();
        let mut argv = Vec::with_capacity(request.arguments.len().saturating_add(1));
        argv.push(request.path.as_str());
        argv.extend(request.arguments.iter().map(String::as_str));
        let mut address_space = BuiltAddressSpace::build(
            image,
            executable.bytes(),
            &argv,
            &environment,
            &mut self.mapper,
            &mut self.frame_allocator,
            self.physical_memory_offset,
        )?;
        let page_table_address = address_space.page_table_frame.start_address().as_u64();
        let new_entry_point = address_space.entry_point;
        let new_path = core::mem::take(&mut request.path);
        let mut closed_files = Vec::with_capacity(MAX_OPEN_FILES);
        let mut closed_pipes = [None; MAX_OPEN_FILES];

        let commit = cpu_interrupts::without_interrupts(|| {
            if !scheduler::replace_process_image(process_id, context_pointer, page_table_address) {
                return Err(Error::Scheduler(scheduler::InitError::InvalidUserContext));
            }

            let context = SavedContext {
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
                rip: new_entry_point,
                cs: u64::from(gdt::user_code_selector()),
                rflags: USER_RFLAGS,
                stack_pointer: address_space.stack_pointer,
                stack_segment: u64::from(gdt::user_data_selector()),
            };
            unsafe { (context_pointer as *mut SavedContext).write(context) };

            let mut manager = PROCESS_MANAGER.lock();
            let process = manager
                .process_mut(process_id)
                .expect("exec process disappeared after scheduler replacement");
            let old_path = core::mem::replace(&mut process.path, new_path);
            let old_frames = core::mem::replace(
                &mut process.owned_frames,
                core::mem::take(&mut address_space.owned_frames),
            );
            process.page_table_address = page_table_address;
            process.entry_point = new_entry_point;
            process.mapped_pages = address_space.pages.len();
            process.load_segments = image.load_segments().len();
            process.guard_page_address = address_space.guard_page_address;
            process.ranges = core::mem::take(&mut address_space.ranges);
            process.pages = core::mem::take(&mut address_space.pages);
            process.pending_exec = None;
            process.reset_signal_actions_for_exec();
            process.exec_count = process.exec_count.saturating_add(1);

            let mut index = 0usize;
            while index < process.open_files.len() {
                if process.open_files[index].close_on_exec {
                    closed_files.push(process.open_files.remove(index));
                } else {
                    index = index.saturating_add(1);
                }
            }
            let closed_file_count = closed_files.len();
            let mut closed_pipe_count = 0usize;
            let mut index = 0usize;
            while index < process.pipe_descriptors.len() {
                if process.pipe_descriptors[index].close_on_exec {
                    closed_pipes[closed_pipe_count] = Some(process.pipe_descriptors.remove(index));
                    closed_pipe_count = closed_pipe_count.saturating_add(1);
                } else {
                    index = index.saturating_add(1);
                }
            }
            let closed_descriptors = closed_file_count.saturating_add(closed_pipe_count);
            process.close_count = process
                .close_count
                .saturating_add(closed_descriptors as u64);
            process.pipe_descriptor_close_count = process
                .pipe_descriptor_close_count
                .saturating_add(closed_pipe_count as u64);
            process.close_on_exec_count = process
                .close_on_exec_count
                .saturating_add(closed_descriptors as u64);
            manager.execs = manager.execs.saturating_add(1);
            Ok((old_path, old_frames, closed_pipe_count, closed_descriptors))
        });
        let (old_path, mut old_frames, closed_pipe_count, closed_descriptors) = match commit {
            Ok(committed) => committed,
            Err(error) => {
                for frame in address_space.owned_frames.drain(..) {
                    self.frame_allocator.deallocate_frame(frame);
                }
                return Err(error);
            }
        };

        drop(closed_files);
        for descriptor in closed_pipes.into_iter().take(closed_pipe_count).flatten() {
            match descriptor.direction {
                PipeDirection::Reader => {
                    let _ = pipe::close_reader(descriptor.pipe_id);
                }
                PipeDirection::Writer => {
                    let _ = pipe::close_writer(descriptor.pipe_id);
                }
            }
        }
        let reclaimed_frames = release_owned_frames(&mut old_frames, &mut self.frame_allocator);
        {
            let mut manager = PROCESS_MANAGER.lock();
            if let Some(process) = manager.process_mut(process_id) {
                process.exec_frames_reclaimed = process
                    .exec_frames_reclaimed
                    .saturating_add(reclaimed_frames as u64);
            }
            manager.frames_reclaimed = manager
                .frames_reclaimed
                .saturating_add(reclaimed_frames as u64);
        }
        crate::serial_println!(
            "userspace process image replaced: pid={}, old={}, new={}, entry={:#018x}, closed={}, frames_reclaimed={}",
            process_id,
            old_path,
            process_path(process_id),
            new_entry_point,
            closed_descriptors,
            reclaimed_frames
        );
        let should_wake = {
            let mut manager = PROCESS_MANAGER.lock();
            manager.process_mut(process_id).is_some_and(|process| {
                if process.state == ProcessState::Blocked {
                    process.make_runnable();
                    true
                } else {
                    false
                }
            })
        };
        if should_wake {
            assert!(
                scheduler::wake_process(process_id),
                "exec process task disappeared before wakeup"
            );
        }
        Ok(())
    }

    fn service_exec_requests(&mut self) -> Result<usize, Error> {
        let requests: Vec<(u64, PendingExec)> = cpu_interrupts::without_interrupts(|| {
            PROCESS_MANAGER
                .lock()
                .processes
                .iter()
                .filter_map(|process| {
                    process
                        .pending_exec
                        .clone()
                        .map(|request| (process.process_id, request))
                })
                .collect()
        });
        let mut completed = 0usize;
        for (process_id, request) in requests {
            let executable = if executable_path_uses_service(&request.path) {
                match poll_service_executable_load(
                    process_id,
                    ExecutableLoadOwner::Exec,
                    &request.path,
                    request.stack_pointer,
                ) {
                    ExecutableLoadPoll::Pending => continue,
                    ExecutableLoadPoll::Ready(executable) => Ok(executable),
                    ExecutableLoadPoll::Failed(error) => Err((error, None)),
                }
            } else {
                elf::load(&request.path).map_err(|error| {
                    let error = Error::Elf(error);
                    (process_error_number(&error), Some(error))
                })
            };
            let failure = match executable {
                Ok(executable) => {
                    match self.replace_process_image(process_id, request.clone(), &executable) {
                        Ok(()) => {
                            completed = completed.saturating_add(1);
                            continue;
                        }
                        Err(error) => Some((process_error_number(&error), Some(error))),
                    }
                }
                Err(failure) => Some(failure),
            };
            let Some((error_number, diagnostic)) = failure else {
                continue;
            };
            let return_value = error_return(error_number);
            let should_wake = cpu_interrupts::without_interrupts(|| {
                let mut manager = PROCESS_MANAGER.lock();
                let Some(process) = manager.process_mut(process_id) else {
                    return false;
                };
                if process.state != ProcessState::Blocked
                    || process.pending_exec.as_ref().is_none_or(|pending| {
                        pending.stack_pointer != request.stack_pointer
                            || pending.path != request.path
                    })
                {
                    return false;
                }
                let registers = unsafe { &mut *(request.stack_pointer as *mut SavedRegisters) };
                registers.rax = return_value;
                process.pending_exec = None;
                process.pending_executable_load = None;
                process.make_runnable();
                process.exec_failure_count = process.exec_failure_count.saturating_add(1);
                manager.exec_failures = manager.exec_failures.saturating_add(1);
                true
            });
            if should_wake && !scheduler::wake_process(process_id) {
                return Err(Error::ProcessNotFound(process_id));
            }
            if let Some(error) = diagnostic {
                crate::serial_println!(
                    "userspace process exec failed: pid={}, path={}, error={}",
                    process_id,
                    request.path,
                    error
                );
            } else {
                crate::serial_println!(
                    "userspace process exec load failed: pid={}, path={}, errno={}",
                    process_id,
                    request.path,
                    error_number
                );
            }
            completed = completed.saturating_add(1);
        }
        Ok(completed)
    }

    pub fn wait(&mut self, process_id: u64) -> Result<ProcessResult, Error> {
        loop {
            self.poll()?;
            let mut manager = PROCESS_MANAGER.lock();
            if let Some(result) = manager.take_result(process_id) {
                return Ok(result);
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
        service_terminal_control();
        self.service_pending_signals()?;
        let reaped = reap(&mut self.frame_allocator)?;
        service_block_device_endpoints();
        self.service_cow_faults()?;
        service_terminal_reads(self.physical_memory_offset)?;
        service_pipe_waiters(self.physical_memory_offset)?;
        self.service_tmpfs_proxy_requests()?;
        self.service_nullfs_proxy_requests()?;
        vfs_route_service_registration();
        self.service_vfs_requests()?;
        self.service_fork_requests()?;
        self.service_child_requests()?;
        self.service_exec_requests()?;
        self.service_pending_signals()?;
        wake_satisfied_object_waiters();
        Ok(reaped)
    }

    fn service_tmpfs_proxy_requests(&mut self) -> Result<usize, Error> {
        let mut completed = usize::from(tmpfs_proxy_service_connect());
        if tmpfs_proxy_service_abandoned() {
            completed = completed.saturating_add(1);
        }
        let pending_requests: Vec<(u64, PendingTmpfsProxyRequest)> =
            cpu_interrupts::without_interrupts(|| {
                PROCESS_MANAGER
                    .lock()
                    .processes
                    .iter()
                    .filter_map(|process| {
                        process
                            .pending_tmpfs_proxy
                            .clone()
                            .map(|pending| (process.process_id, pending))
                    })
                    .collect()
            });
        for (process_id, pending) in pending_requests {
            let Some(reply) = tmpfs_proxy_take_filesystem_reply(&pending) else {
                continue;
            };
            tmpfs_proxy_release_pending(&pending);
            self.complete_tmpfs_proxy_request(process_id, pending, reply)?;
            completed = completed.saturating_add(1);
        }
        if tmpfs_proxy_service_close() {
            completed = completed.saturating_add(1);
        }
        Ok(completed)
    }

    fn service_nullfs_proxy_requests(&mut self) -> Result<usize, Error> {
        let mut completed = usize::from(nullfs_proxy_service_connect());
        if nullfs_proxy_service_abandoned() {
            completed = completed.saturating_add(1);
        }
        let pending_requests: Vec<(u64, PendingNullfsProxyRequest)> =
            cpu_interrupts::without_interrupts(|| {
                PROCESS_MANAGER
                    .lock()
                    .processes
                    .iter()
                    .filter_map(|process| {
                        process
                            .pending_nullfs_proxy
                            .clone()
                            .map(|pending| (process.process_id, pending))
                    })
                    .collect()
            });
        for (process_id, pending) in pending_requests {
            let Some(reply) = nullfs_proxy_take_reply(&pending) else {
                continue;
            };
            self.complete_nullfs_proxy_request(process_id, pending, reply)?;
            completed = completed.saturating_add(1);
        }
        if nullfs_proxy_service_close() {
            completed = completed.saturating_add(1);
        }
        Ok(completed)
    }

    fn service_vfs_requests(&mut self) -> Result<usize, Error> {
        let requests: Vec<(u64, PendingVfsRequest)> = PROCESS_MANAGER
            .lock()
            .processes
            .iter()
            .filter_map(|process| {
                process
                    .pending_vfs_request
                    .clone()
                    .map(|pending| (process.process_id, pending))
            })
            .collect();
        let mut completed = 0usize;
        for (process_id, pending) in requests {
            let Some(reply) = vfs_request_take_reply(&pending) else {
                continue;
            };
            vfs_request_release(&pending);
            {
                let mut manager = PROCESS_MANAGER.lock();
                let Some(process) = manager.process_mut(process_id) else {
                    continue;
                };
                if process
                    .pending_vfs_request
                    .as_ref()
                    .is_none_or(|current| current.reply_endpoint != pending.reply_endpoint)
                {
                    continue;
                }
                process.pending_vfs_request = None;
            }
            if let PendingVfsOperation::LoadExecutable { owner } = &pending.operation {
                let owner = *owner;
                match reply {
                    Ok(reply)
                        if reply.status == vfs_protocol::status::OK
                            && reply.backend == vfs_protocol::backend::NULLFS =>
                    {
                        match vfs_nullfs_backend_path(&reply, &pending.path) {
                            Ok(backend_path) => {
                                if !executable_load_set_route(
                                    process_id,
                                    owner,
                                    pending.stack_pointer,
                                    pending.generation,
                                    backend_path,
                                ) {
                                    crate::serial_println!(
                                        "userspace executable route stale: pid={}, path={}",
                                        process_id,
                                        pending.path
                                    );
                                    executable_load_fail(
                                        process_id,
                                        owner,
                                        pending.stack_pointer,
                                        ERR_IO,
                                    );
                                }
                            }
                            Err(error) => executable_load_fail(
                                process_id,
                                owner,
                                pending.stack_pointer,
                                error,
                            ),
                        }
                    }
                    Ok(reply) if reply.status == vfs_protocol::status::NOT_FOUND => {
                        executable_load_fail(
                            process_id,
                            owner,
                            pending.stack_pointer,
                            ERR_NO_ENTRY,
                        );
                    }
                    Ok(_) | Err(_) => {
                        executable_load_fail(process_id, owner, pending.stack_pointer, ERR_IO)
                    }
                }
                completed = completed.saturating_add(1);
                continue;
            }

            let nullfs_backend_path = match &reply {
                Ok(reply)
                    if reply.status == vfs_protocol::status::OK
                        && reply.backend == vfs_protocol::backend::NULLFS =>
                {
                    Some(vfs_nullfs_backend_path(reply, &pending.path))
                }
                _ => None,
            };
            let nullfs_outcome = match (&pending.operation, &reply, &nullfs_backend_path) {
                (
                    PendingVfsOperation::Stat {
                        stat_address,
                        stat_length,
                    },
                    Ok(_),
                    Some(Ok(backend_path)),
                ) => Some(nullfs_proxy_stat(
                    process_id,
                    &pending.path,
                    backend_path,
                    *stat_address,
                    *stat_length,
                    pending.stack_pointer,
                )),
                (
                    PendingVfsOperation::ReadDirectory {
                        start_index,
                        records_address,
                        capacity,
                    },
                    Ok(_),
                    Some(Ok(backend_path)),
                ) => Some(nullfs_proxy_read_directory(
                    process_id,
                    &pending.path,
                    backend_path,
                    *start_index,
                    *records_address,
                    *capacity,
                    pending.stack_pointer,
                )),
                (PendingVfsOperation::Chdir, Ok(_), Some(Ok(backend_path))) => {
                    Some(nullfs_proxy_chdir(
                        process_id,
                        &pending.path,
                        backend_path,
                        pending.stack_pointer,
                    ))
                }
                (
                    PendingVfsOperation::Open {
                        options,
                        close_on_exec,
                        descriptor,
                    },
                    Ok(_),
                    Some(Ok(backend_path)),
                ) => Some(nullfs_proxy_open(
                    process_id,
                    &pending.path,
                    backend_path,
                    *options,
                    *close_on_exec,
                    *descriptor,
                    pending.stack_pointer,
                )),
                (PendingVfsOperation::Unlink, Ok(_), Some(Ok(backend_path))) => {
                    Some(nullfs_proxy_unlink(
                        process_id,
                        &pending.path,
                        backend_path,
                        pending.stack_pointer,
                    ))
                }
                (_, Ok(_), Some(Err(error))) => Some(ControlOutcome::Ready(error_return(*error))),
                (_, Ok(reply), _) if reply.backend == vfs_protocol::backend::NULLFS => {
                    Some(ControlOutcome::Ready(error_return(ERR_IO)))
                }
                _ => None,
            };
            let outcome = if let Some(outcome) = nullfs_outcome {
                outcome
            } else {
                scheduler::with_process_address_space(process_id, || {
                    match (&pending.operation, reply) {
                        (
                            PendingVfsOperation::Stat {
                                stat_address,
                                stat_length,
                            },
                            Ok(reply),
                        ) if reply.status == vfs_protocol::status::OK
                            && reply.backend == vfs_protocol::backend::TMPFS =>
                        {
                            tmpfs_proxy_stat(
                                process_id,
                                &pending.path,
                                *stat_address,
                                *stat_length,
                                pending.stack_pointer,
                            )
                        }

                        (
                            PendingVfsOperation::Stat {
                                stat_address,
                                stat_length,
                            },
                            Ok(reply),
                        ) if reply.status == vfs_protocol::status::OK
                            && reply.backend == vfs_protocol::backend::BOOT_FILESYSTEM =>
                        {
                            let result = match vfs::metadata(&pending.path) {
                                Ok(metadata) => platform_write_value(
                                    process_id,
                                    *stat_address,
                                    *stat_length,
                                    platform_stat_from_metadata(&metadata),
                                ),
                                Err(error) => error_return(platform_vfs_errno(&error)),
                            };
                            ControlOutcome::Ready(result)
                        }
                        (
                            PendingVfsOperation::Stat {
                                stat_address,
                                stat_length,
                            },
                            Ok(reply),
                        ) if reply.status == vfs_protocol::status::OK
                            && reply.backend == vfs_protocol::backend::NAMESPACE
                            && usize::from(reply.prefix_length) == pending.path.len() =>
                        {
                            ControlOutcome::Ready(platform_write_value(
                                process_id,
                                *stat_address,
                                *stat_length,
                                abi::file::Stat {
                                    kind: abi::file::KIND_DIRECTORY,
                                    size: 0,
                                    flags: if pending.path == "/System"
                                        || pending.path.starts_with("/System/")
                                    {
                                        abi::file::FLAG_SYSTEM
                                    } else {
                                        0
                                    },
                                },
                            ))
                        }
                        (PendingVfsOperation::Stat { .. }, Ok(reply))
                            if reply.status == vfs_protocol::status::OK
                                && reply.backend == vfs_protocol::backend::NAMESPACE =>
                        {
                            ControlOutcome::Ready(error_return(abi::errno::NO_ENTRY))
                        }
                        (
                            PendingVfsOperation::ReadDirectory {
                                start_index,
                                records_address,
                                capacity,
                            },
                            Ok(reply),
                        ) if reply.status == vfs_protocol::status::OK
                            && reply.backend == vfs_protocol::backend::TMPFS =>
                        {
                            tmpfs_proxy_read_directory(
                                process_id,
                                &pending.path,
                                *start_index,
                                *records_address,
                                *capacity,
                                pending.stack_pointer,
                            )
                        }

                        (
                            PendingVfsOperation::ReadDirectory {
                                start_index,
                                records_address,
                                capacity,
                            },
                            Ok(reply),
                        ) if reply.status == vfs_protocol::status::OK
                            && reply.backend == vfs_protocol::backend::BOOT_FILESYSTEM =>
                        {
                            vfs_routed_boot_directory(
                                &pending.path,
                                *start_index,
                                *records_address,
                                *capacity,
                            )
                        }
                        (
                            PendingVfsOperation::ReadDirectory {
                                start_index,
                                records_address,
                                capacity,
                            },
                            Ok(reply),
                        ) if reply.status == vfs_protocol::status::OK
                            && reply.backend == vfs_protocol::backend::NAMESPACE
                            && usize::from(reply.prefix_length) == pending.path.len() =>
                        {
                            vfs_routed_namespace_directory(
                                &pending.path,
                                *start_index,
                                *records_address,
                                *capacity,
                            )
                        }
                        (PendingVfsOperation::ReadDirectory { .. }, Ok(reply))
                            if reply.status == vfs_protocol::status::OK
                                && reply.backend == vfs_protocol::backend::NAMESPACE =>
                        {
                            ControlOutcome::Ready(error_return(abi::errno::NO_ENTRY))
                        }
                        (PendingVfsOperation::Chdir, Ok(reply))
                            if reply.status == vfs_protocol::status::OK
                                && reply.backend == vfs_protocol::backend::TMPFS
                                && pending.path == "/tmp" =>
                        {
                            ControlOutcome::Ready(
                                match platform_set_working_directory(process_id, &pending.path) {
                                    Ok(()) => 0,
                                    Err(error) => error_return(error),
                                },
                            )
                        }

                        (PendingVfsOperation::Chdir, Ok(reply))
                            if reply.status == vfs_protocol::status::OK
                                && reply.backend == vfs_protocol::backend::BOOT_FILESYSTEM =>
                        {
                            let result = match vfs::metadata(&pending.path) {
                                Ok(metadata) if metadata.is_directory() => {
                                    platform_set_working_directory(process_id, &metadata.path)
                                }
                                Ok(_) => Err(abi::errno::NOT_DIRECTORY),
                                Err(error) => Err(platform_vfs_errno(&error)),
                            };
                            ControlOutcome::Ready(match result {
                                Ok(()) => 0,
                                Err(error) => error_return(error),
                            })
                        }
                        (PendingVfsOperation::Chdir, Ok(reply))
                            if reply.status == vfs_protocol::status::OK
                                && reply.backend == vfs_protocol::backend::NAMESPACE
                                && usize::from(reply.prefix_length) == pending.path.len() =>
                        {
                            ControlOutcome::Ready(
                                match platform_set_working_directory(process_id, &pending.path) {
                                    Ok(()) => 0,
                                    Err(error) => error_return(error),
                                },
                            )
                        }
                        (PendingVfsOperation::Chdir, Ok(reply))
                            if reply.status == vfs_protocol::status::OK
                                && (reply.backend == vfs_protocol::backend::NAMESPACE
                                    || reply.backend == vfs_protocol::backend::TMPFS) =>
                        {
                            ControlOutcome::Ready(error_return(abi::errno::NO_ENTRY))
                        }
                        (PendingVfsOperation::Open { .. }, Ok(reply))
                            if reply.status == vfs_protocol::status::OK
                                && reply.backend == vfs_protocol::backend::NAMESPACE =>
                        {
                            ControlOutcome::Ready(
                                if usize::from(reply.prefix_length) == pending.path.len() {
                                    error_return(ERR_IS_DIRECTORY)
                                } else {
                                    error_return(abi::errno::NO_ENTRY)
                                },
                            )
                        }
                        (PendingVfsOperation::Open { .. }, Ok(reply))
                            if reply.status == vfs_protocol::status::OK
                                && reply.backend == vfs_protocol::backend::TMPFS
                                && pending.path == "/tmp" =>
                        {
                            ControlOutcome::Ready(error_return(ERR_IS_DIRECTORY))
                        }
                        (
                            PendingVfsOperation::Open {
                                options,
                                close_on_exec,
                                descriptor,
                            },
                            Ok(reply),
                        ) if reply.status == vfs_protocol::status::OK
                            && reply.backend == vfs_protocol::backend::TMPFS =>
                        {
                            tmpfs_proxy_open(
                                process_id,
                                &pending.path,
                                *options,
                                *close_on_exec,
                                *descriptor,
                                pending.stack_pointer,
                            )
                        }

                        (
                            PendingVfsOperation::Open {
                                options,
                                close_on_exec,
                                descriptor,
                            },
                            Ok(reply),
                        ) if reply.status == vfs_protocol::status::OK
                            && reply.backend == vfs_protocol::backend::BOOT_FILESYSTEM =>
                        {
                            ControlOutcome::Ready(vfs_complete_boot_open(
                                process_id,
                                &pending.path,
                                *options,
                                *close_on_exec,
                                *descriptor,
                            ))
                        }
                        (PendingVfsOperation::Unlink, Ok(reply))
                            if reply.status == vfs_protocol::status::OK
                                && reply.backend == vfs_protocol::backend::TMPFS
                                && pending.path == "/tmp" =>
                        {
                            ControlOutcome::Ready(error_return(ERR_IS_DIRECTORY))
                        }
                        (PendingVfsOperation::Unlink, Ok(reply))
                            if reply.status == vfs_protocol::status::OK
                                && reply.backend == vfs_protocol::backend::TMPFS =>
                        {
                            tmpfs_proxy_unlink(process_id, &pending.path, pending.stack_pointer)
                        }

                        (PendingVfsOperation::Unlink, Ok(reply))
                            if reply.status == vfs_protocol::status::OK
                                && reply.backend == vfs_protocol::backend::NAMESPACE =>
                        {
                            ControlOutcome::Ready(
                                if usize::from(reply.prefix_length) == pending.path.len() {
                                    error_return(ERR_IS_DIRECTORY)
                                } else {
                                    error_return(abi::errno::NO_ENTRY)
                                },
                            )
                        }
                        (PendingVfsOperation::Unlink, Ok(reply))
                            if reply.status == vfs_protocol::status::OK
                                && reply.backend == vfs_protocol::backend::BOOT_FILESYSTEM =>
                        {
                            ControlOutcome::Ready(error_return(ERR_NOT_IMPLEMENTED))
                        }
                        (_, Ok(reply)) if reply.status == vfs_protocol::status::NOT_FOUND => {
                            ControlOutcome::Ready(error_return(abi::errno::NO_ENTRY))
                        }
                        _ => ControlOutcome::Ready(error_return(ERR_IO)),
                    }
                })
                .ok_or(Error::ProcessNotFound(process_id))?
            };
            if let ControlOutcome::Ready(result) = outcome {
                scheduler::with_process_address_space(process_id, || {
                    let registers = unsafe { &mut *(pending.stack_pointer as *mut SavedRegisters) };
                    registers.rax = result;
                })
                .ok_or(Error::ProcessNotFound(process_id))?;
                let mut manager = PROCESS_MANAGER.lock();
                let process = manager
                    .process_mut(process_id)
                    .ok_or(Error::ProcessNotFound(process_id))?;
                process.make_runnable();
                drop(manager);
                if !scheduler::wake_process(process_id) {
                    return Err(Error::ProcessNotFound(process_id));
                }
            }
            completed += 1;
        }
        Ok(completed)
    }

    fn complete_tmpfs_proxy_request(
        &mut self,
        process_id: u64,
        pending: PendingTmpfsProxyRequest,
        reply: Result<tmpfs_protocol::Reply, i64>,
    ) -> Result<(), Error> {
        let physical_memory_offset = self.physical_memory_offset;
        let reply_endpoint = pending.reply_endpoint;
        let stack_pointer = pending.stack_pointer;
        let operation = pending.operation;
        cpu_interrupts::without_interrupts(|| -> Result<(), Error> {
            let mut manager = PROCESS_MANAGER.lock();
            let process = manager
                .process_mut(process_id)
                .ok_or(Error::ProcessNotFound(process_id))?;
            let still_pending = process
                .pending_tmpfs_proxy
                .as_ref()
                .is_some_and(|current| current.reply_endpoint == reply_endpoint);
            if !still_pending {
                return Ok(());
            }
            let result = match reply {
                Ok(reply) => match tmpfs_proxy_reply_status(reply.status) {
                    Ok(()) => tmpfs_proxy_complete_operation(
                        process,
                        operation,
                        reply,
                        physical_memory_offset,
                    )?,
                    Err(error) => error_return(error),
                },
                Err(error) => error_return(error),
            };
            if scheduler::with_process_address_space(process_id, || {
                let registers = unsafe { &mut *(stack_pointer as *mut SavedRegisters) };
                registers.rax = result;
            })
            .is_none()
            {
                return Err(Error::ProcessNotFound(process_id));
            }
            process.pending_tmpfs_proxy = None;
            process.make_runnable();
            drop(manager);
            if !scheduler::wake_process(process_id) {
                return Err(Error::ProcessNotFound(process_id));
            }
            Ok(())
        })
    }

    fn complete_nullfs_proxy_request(
        &mut self,
        process_id: u64,
        pending: PendingNullfsProxyRequest,
        reply: Result<filesystem_protocol::Reply, i64>,
    ) -> Result<(), Error> {
        let still_pending = {
            let mut manager = PROCESS_MANAGER.lock();
            let Some(process) = manager.process_mut(process_id) else {
                return Ok(());
            };
            if process
                .pending_nullfs_proxy
                .as_ref()
                .is_none_or(|current| current.request_id != pending.request_id)
            {
                false
            } else {
                process.pending_nullfs_proxy = None;
                true
            }
        };
        if !still_pending {
            let abandoned = {
                let mut slot = NULLFS_ABANDONED_REQUEST.lock();
                if slot
                    .as_ref()
                    .is_some_and(|current| current.request_id == pending.request_id)
                {
                    slot.take()
                } else {
                    None
                }
            };
            if let Some(abandoned) = abandoned {
                nullfs_proxy_complete_abandoned(abandoned, reply);
            }
            return Ok(());
        }
        let request_id = pending.request_id;
        let quarantine = nullfs_proxy_request_is_mutating(&pending)
            && match &reply {
                Ok(reply) => reply.status == filesystem_protocol::status::OUTCOME_UNKNOWN,
                Err(_) => true,
            };
        if quarantine {
            nullfs_proxy_quarantine(&pending);
        }
        let executable_owner = nullfs_proxy_executable_owner(&pending.operation);

        let result = match reply {
            Ok(reply) if reply.status == filesystem_protocol::status::OK => {
                nullfs_proxy_complete_success(
                    process_id,
                    pending,
                    reply,
                    self.physical_memory_offset,
                )
            }
            Ok(reply)
                if executable_owner.is_some()
                    && reply.status == filesystem_protocol::status::TRY_AGAIN =>
            {
                executable_load_set_retry(
                    process_id,
                    executable_owner.expect("checked executable load owner"),
                    pending.stack_pointer,
                    pending.request,
                    pending.operation.clone(),
                );
                Ok(())
            }
            Ok(reply) if executable_owner.is_some() => {
                executable_load_fail(
                    process_id,
                    executable_owner.expect("checked executable load owner"),
                    pending.stack_pointer,
                    nullfs_proxy_status_errno(reply.status),
                );
                Ok(())
            }
            Ok(reply) => nullfs_proxy_finish_process(
                request_id,
                process_id,
                pending.stack_pointer,
                error_return(nullfs_proxy_status_errno(reply.status)),
            ),
            Err(error) if executable_owner.is_some() => {
                executable_load_fail(
                    process_id,
                    executable_owner.expect("checked executable load owner"),
                    pending.stack_pointer,
                    error,
                );
                Ok(())
            }
            Err(error) => nullfs_proxy_finish_process(
                request_id,
                process_id,
                pending.stack_pointer,
                error_return(error),
            ),
        };
        nullfs_proxy_release_request(request_id);
        result
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
                            .as_ref()
                            .filter(|request| !request.claimed)
                            .cloned()
                            .map(|request| (process.process_id, request))
                    })
                    .collect()
            });
        let mut completed = 0usize;
        for (parent_process_id, request) in spawn_requests {
            let executable = if executable_path_uses_service(&request.path) {
                match poll_service_executable_load(
                    parent_process_id,
                    ExecutableLoadOwner::ChildSpawn,
                    &request.path,
                    request.stack_pointer,
                ) {
                    ExecutableLoadPoll::Pending => continue,
                    ExecutableLoadPoll::Ready(executable) => Ok(executable),
                    ExecutableLoadPoll::Failed(error) => Err(error),
                }
            } else {
                elf::load(&request.path).map_err(|error| process_error_number(&Error::Elf(error)))
            };
            let claimed = {
                let mut manager = PROCESS_MANAGER.lock();
                let Some(process) = manager.process_mut(parent_process_id) else {
                    continue;
                };
                if process.state != ProcessState::Blocked {
                    false
                } else if let Some(pending) = process.pending_child_spawn.as_mut() {
                    if pending.claimed
                        || pending.stack_pointer != request.stack_pointer
                        || pending.path != request.path
                    {
                        false
                    } else {
                        pending.claimed = true;
                        true
                    }
                } else {
                    false
                }
            };
            if !claimed {
                continue;
            }
            let return_value = match executable {
                Ok(executable) => {
                    match self.spawn_child_loaded(parent_process_id, &request, &executable) {
                        Ok(info) => info.process_id,
                        Err(error) => error_return(process_error_number(&error)),
                    }
                }
                Err(error) => {
                    crate::serial_println!(
                        "userspace child executable load failed: pid={}, path={}, errno={}",
                        parent_process_id,
                        request.path,
                        error
                    );
                    error_return(error)
                }
            };
            cpu_interrupts::without_interrupts(|| -> Result<(), Error> {
                let mut manager = PROCESS_MANAGER.lock();
                let process = manager
                    .process_mut(parent_process_id)
                    .ok_or(Error::ProcessNotFound(parent_process_id))?;
                if process.state != ProcessState::Blocked
                    || process.pending_child_spawn.as_ref().is_none_or(|pending| {
                        !pending.claimed
                            || pending.stack_pointer != request.stack_pointer
                            || pending.path != request.path
                    })
                {
                    return Ok(());
                }
                let registers = unsafe { &mut *(request.stack_pointer as *mut SavedRegisters) };
                registers.rax = return_value;
                process.pending_child_spawn = None;
                process.pending_executable_load = None;
                process.make_runnable();
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
            let serviced = cpu_interrupts::without_interrupts(|| -> Result<bool, Error> {
                let mut manager = PROCESS_MANAGER.lock();
                let final_status = manager
                    .take_child_result(parent_process_id, request.child_process_id)
                    .as_ref()
                    .map(child_status);
                let status = if final_status.is_some() {
                    final_status
                } else {
                    manager
                        .processes
                        .iter_mut()
                        .find(|process| {
                            process.process_id == request.child_process_id
                                && process.parent_process_id == Some(parent_process_id)
                        })
                        .and_then(Process::take_parent_status)
                };
                let Some(status) = status else {
                    return Ok(false);
                };
                let parent_index = manager
                    .processes
                    .iter()
                    .position(|process| process.process_id == parent_process_id)
                    .ok_or(Error::ProcessNotFound(parent_process_id))?;
                let registers = unsafe { &mut *(request.stack_pointer as *mut SavedRegisters) };
                registers.rax = status;
                {
                    let parent = &mut manager.processes[parent_index];
                    parent.pending_child_wait = None;
                    parent.make_runnable();
                    parent.child_wait_count = parent.child_wait_count.saturating_add(1);
                }
                manager.child_waits = manager.child_waits.saturating_add(1);
                drop(manager);
                if !scheduler::wake_process(parent_process_id) {
                    return Err(Error::ProcessNotFound(parent_process_id));
                }
                Ok(true)
            })?;
            if serviced {
                completed = completed.saturating_add(1);
            }
        }
        Ok(completed)
    }

    pub fn signal_process_group(
        &mut self,
        process_group_id: u64,
        signal: u64,
    ) -> Result<usize, Error> {
        deliver_signal_group(None, process_group_id, signal).map_err(|error| match error {
            ERR_NO_PROCESS => Error::InvalidProcessGroup(process_group_id),
            _ => Error::InvalidArgument,
        })
    }

    pub fn terminal_active(&self) -> bool {
        terminal::foreground_process().is_some()
    }

    pub fn process_is_active(&self, process_id: u64) -> bool {
        cpu_interrupts::without_interrupts(|| {
            PROCESS_MANAGER
                .lock()
                .processes
                .iter()
                .any(|process| process.process_id == process_id && process.is_live())
        })
    }

    pub fn handle_terminal_key(&mut self, key: pc_keyboard::DecodedKey) -> Result<bool, Error> {
        let handled = terminal::handle_key(key);
        let signaled = service_terminal_control();
        if handled && signaled == 0 {
            service_terminal_reads(self.physical_memory_offset)?;
        }
        Ok(handled)
    }

    pub fn wait_until_process_path(&mut self, process_id: u64, path: &str) -> Result<(), Error> {
        loop {
            self.poll()?;
            let (active_match, exists, completed_match) =
                cpu_interrupts::without_interrupts(|| {
                    let manager = PROCESS_MANAGER.lock();
                    let active = manager
                        .processes
                        .iter()
                        .find(|process| process.process_id == process_id);
                    let active_match = active.is_some_and(|process| process.path == path);
                    let completed_match = manager
                        .completions
                        .history()
                        .iter()
                        .any(|result| result.process_id == process_id && result.path == path);
                    (active_match, active.is_some(), completed_match)
                });
            if active_match || completed_match {
                return Ok(());
            }
            if !exists {
                return Err(Error::ProcessNotFound(process_id));
            }
            hlt();
        }
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
                    .completions
                    .history()
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

    pub fn wait_until_child_group_stopped(
        &mut self,
        parent_process_id: u64,
        process_group_id: u64,
        minimum_members: usize,
    ) -> Result<ProcessGroupInfo, Error> {
        loop {
            self.poll()?;
            if let Some(info) = child_group_info(parent_process_id, process_group_id)
                && info.process_ids.len() >= minimum_members
                && info.stopped == info.process_ids.len()
            {
                return Ok(info);
            }
            hlt();
        }
    }

    pub fn wait_until_child_group_resumed(
        &mut self,
        parent_process_id: u64,
        process_group_id: u64,
        minimum_members: usize,
    ) -> Result<ProcessGroupInfo, Error> {
        loop {
            self.poll()?;
            if let Some(info) = child_group_info(parent_process_id, process_group_id)
                && info.process_ids.len() >= minimum_members
                && info.stopped == 0
            {
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
                    .completions
                    .history()
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
        self.inject_terminal_control(pc_keyboard::DecodedKey::Unicode('\u{3}'))
    }

    pub fn inject_terminal_suspend(&mut self) -> Result<usize, Error> {
        self.inject_terminal_control(pc_keyboard::DecodedKey::Unicode('\u{1a}'))
    }

    fn inject_terminal_control(&mut self, key: pc_keyboard::DecodedKey) -> Result<usize, Error> {
        terminal::handle_key(key);
        let delivered = service_terminal_control();
        if delivered == 0 {
            service_terminal_reads(self.physical_memory_offset)?;
        }
        Ok(delivered)
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

fn vfs_route_begin_registration(
    request_endpoint: CapabilityObjectRef,
    generation: u32,
) -> Result<(), i64> {
    {
        let state = VFS_ROUTE.lock();
        if state.request_endpoint.is_some()
            || generation <= state.generation
            || request_endpoint.id <= state.retired_request_endpoint_id
        {
            return Err(ERR_INVALID_ARGUMENT);
        }
    }
    let reply_endpoint = tmpfs_proxy_create_reply_endpoint()?;
    let mut request = vfs_protocol::Request::EMPTY;
    request.operation = vfs_protocol::operation::RESOLVE;
    request.request_id = generation;
    request.path_length = 1;
    request.path[0] = b'/';
    let bytes = tmpfs_proxy_value_bytes(&request).to_vec();
    let push_result = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        match registry.object_index(request_endpoint) {
            Some(object_index) => match &mut registry.objects[object_index].data {
                CapabilityObjectData::Endpoint(endpoint)
                    if endpoint.queue.len() < abi::limits::MAX_ENDPOINT_MESSAGES =>
                {
                    endpoint.queue.push_back(EndpointMessage {
                        sender_process_id: 0,
                        bytes,
                        capabilities: vec![TransferredCapability {
                            object: reply_endpoint,
                            rights: abi::capability::RIGHT_SEND,
                        }],
                    });
                    Ok(())
                }
                CapabilityObjectData::Endpoint(_) => Err(ERR_TRY_AGAIN),
                CapabilityObjectData::Notification(_)
                | CapabilityObjectData::SharedMemory(_)
                | CapabilityObjectData::KernelEarlyLogReader(_)
                | CapabilityObjectData::Job(_) => Err(ERR_IO),
            },
            None => Err(ERR_IO),
        }
    };
    if let Err(error) = push_result {
        tmpfs_proxy_release_reply_endpoint(reply_endpoint);
        return Err(error);
    }

    let previous = {
        let mut state = VFS_ROUTE.lock();
        let previous = (state.request_endpoint, state.reply_endpoint);
        state.request_endpoint = Some(request_endpoint);
        state.reply_endpoint = Some(reply_endpoint);
        state.generation = generation;
        state.ready = false;
        state.active_request_id = 0;
        previous
    };
    if previous.0 != Some(request_endpoint) {
        if let Some(endpoint) = previous.0 {
            kernel_capability_root_remove(endpoint);
        }
        kernel_capability_root_add(request_endpoint);
    }
    if let Some(endpoint) = previous.1 {
        tmpfs_proxy_release_reply_endpoint(endpoint);
    }
    vfs_route_cancel_stale_requests(generation);
    wake_endpoint_waiter(request_endpoint);
    Ok(())
}

fn vfs_route_offline(generation: u32) -> Result<(), i64> {
    let (request_endpoint, reply_endpoint) = {
        let mut state = VFS_ROUTE.lock();
        if state.generation != generation {
            return Err(ERR_INVALID_ARGUMENT);
        }
        let Some(request_endpoint) = state.request_endpoint.take() else {
            return Ok(());
        };
        state.retired_request_endpoint_id = request_endpoint.id;
        let reply_endpoint = state.reply_endpoint.take();
        state.ready = false;
        state.active_request_id = 0;
        (request_endpoint, reply_endpoint)
    };
    kernel_capability_root_remove(request_endpoint);
    if let Some(reply_endpoint) = reply_endpoint {
        tmpfs_proxy_release_reply_endpoint(reply_endpoint);
    } else {
        CAPABILITY_REGISTRY.lock().collect_garbage();
    }
    vfs_route_cancel_generation(generation);
    Ok(())
}

fn vfs_route_cancel_generation(generation: u32) {
    let pending: Vec<(u64, PendingVfsRequest)> = {
        let mut manager = PROCESS_MANAGER.lock();
        manager
            .processes
            .iter_mut()
            .filter_map(|process| {
                let request = process.pending_vfs_request.as_ref()?;
                if request.generation != generation {
                    return None;
                }
                process
                    .pending_vfs_request
                    .take()
                    .map(|pending| (process.process_id, pending))
            })
            .collect()
    };
    vfs_route_fail_pending(pending);
    executable_load_cancel_vfs_generations(generation, true);
}

fn vfs_route_cancel_stale_requests(generation: u32) {
    let pending: Vec<(u64, PendingVfsRequest)> = {
        let mut manager = PROCESS_MANAGER.lock();
        manager
            .processes
            .iter_mut()
            .filter_map(|process| {
                let request = process.pending_vfs_request.as_ref()?;
                if request.generation == generation {
                    return None;
                }
                process
                    .pending_vfs_request
                    .take()
                    .map(|pending| (process.process_id, pending))
            })
            .collect()
    };
    vfs_route_fail_pending(pending);
    executable_load_cancel_vfs_generations(generation, false);
}

fn executable_load_cancel_vfs_generations(generation: u32, matching: bool) {
    let (abandoned, closes) = {
        let mut manager = PROCESS_MANAGER.lock();
        let mut abandoned = Vec::new();
        let mut closes = Vec::new();
        for process in &mut manager.processes {
            let Some(load) = process.pending_executable_load.as_mut() else {
                continue;
            };
            if load.vfs_generation == 0 || (load.vfs_generation == generation) != matching {
                continue;
            }
            if let Some(close) = load.take_close_ticket() {
                closes.push(close);
            }
            load.bytes.clear();
            load.retry = None;
            load.result = Some(Err(ERR_IO));
            if process
                .pending_nullfs_proxy
                .as_ref()
                .is_some_and(|pending| nullfs_proxy_executable_owner(&pending.operation).is_some())
                && let Some(pending) = process.pending_nullfs_proxy.take()
            {
                abandoned.push(pending);
            }
        }
        (abandoned, closes)
    };
    for close in closes {
        nullfs_proxy_enqueue_close_ticket(close);
    }
    for pending in abandoned {
        nullfs_proxy_abandon_pending(pending);
    }
}

fn vfs_route_fail_pending(pending: Vec<(u64, PendingVfsRequest)>) {
    for (process_id, pending) in pending {
        tmpfs_proxy_release_reply_endpoint(pending.reply_endpoint);
        if let PendingVfsOperation::LoadExecutable { owner } = pending.operation {
            executable_load_fail(process_id, owner, pending.stack_pointer, ERR_IO);
            continue;
        }
        if scheduler::with_process_address_space(process_id, || {
            let registers = unsafe { &mut *(pending.stack_pointer as *mut SavedRegisters) };
            registers.rax = error_return(ERR_IO);
        })
        .is_none()
        {
            continue;
        }
        let made_runnable = {
            let mut manager = PROCESS_MANAGER.lock();
            let Some(process) = manager.process_mut(process_id) else {
                continue;
            };
            process.make_runnable();
            true
        };
        if made_runnable {
            let _ = scheduler::wake_process(process_id);
        }
    }
}

fn vfs_route_service_registration() -> bool {
    let state = *VFS_ROUTE.lock();
    let Some(reply_endpoint) = state.reply_endpoint else {
        return false;
    };
    let message = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        let Some(index) = registry.object_index(reply_endpoint) else {
            return false;
        };
        let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[index].data else {
            return false;
        };
        endpoint.queue.pop_front()
    };
    let Some(message) = message else {
        return false;
    };
    let valid = if message.capabilities.is_empty()
        && message.bytes.len() == size_of::<vfs_protocol::Reply>()
    {
        let reply =
            unsafe { ptr::read_unaligned(message.bytes.as_ptr() as *const vfs_protocol::Reply) };
        reply.version == vfs_protocol::VERSION
            && reply.operation == vfs_protocol::operation::RESOLVE
            && reply.request_id == state.generation
            && reply.status == vfs_protocol::status::OK
            && reply.route_id == vfs_protocol::route::ROOT
            && reply.backend == vfs_protocol::backend::BOOT_FILESYSTEM
            && reply.prefix_length == 1
            && reply.backing_prefix_length == 0
            && reply.flags == 0
            && reply.reserved == [0; 8]
            && reply.backing_prefix == [0; vfs_protocol::MAX_PATH_BYTES]
    } else {
        false
    };
    {
        let mut route = VFS_ROUTE.lock();
        if route.reply_endpoint == Some(reply_endpoint) {
            route.reply_endpoint = None;
            route.ready = valid;
        }
    }
    tmpfs_proxy_release_reply_endpoint(reply_endpoint);
    true
}

fn vfs_route_ready() -> bool {
    let state = *VFS_ROUTE.lock();
    state.request_endpoint.is_some() && state.generation != 0 && state.ready
}

fn vfs_route_is_offline() -> bool {
    let state = *VFS_ROUTE.lock();
    state.generation != 0 && (state.request_endpoint.is_none() || !state.ready)
}

fn vfs_is_declared_namespace_directory(path: &str) -> bool {
    matches!(path, "/dev" | "/Volumes")
}

fn vfs_routed_boot_directory(
    path: &str,
    start_index: usize,
    records_address: u64,
    capacity: usize,
) -> ControlOutcome {
    let entries = match vfs::read_directory(path) {
        Ok(entries) => entries,
        Err(error) => return ControlOutcome::Ready(error_return(platform_vfs_errno(&error))),
    };
    let mut records = Vec::new();
    if path == "/" {
        for name in ["dev", "tmp", "System", "Users", "Applications", "Volumes"] {
            records.push(vfs_namespace_directory_record(name, name == "System"));
        }
    }
    for entry in &entries {
        if path == "/"
            && matches!(
                entry.name.as_str(),
                "dev" | "tmp" | "System" | "Users" | "Applications" | "Volumes"
            )
        {
            continue;
        }
        let record = match platform_directory_record(entry) {
            Ok(record) => record,
            Err(error) => return ControlOutcome::Ready(error_return(error)),
        };
        records.push(record);
    }
    vfs_write_directory_records(&records, start_index, records_address, capacity)
}

fn vfs_routed_namespace_directory(
    path: &str,
    start_index: usize,
    records_address: u64,
    capacity: usize,
) -> ControlOutcome {
    let names: &[&str] = match path {
        "/dev" => &[],
        "/Volumes" => &[nullfs_primary_volume::DISPLAY_NAME],
        _ => return ControlOutcome::Ready(error_return(abi::errno::NO_ENTRY)),
    };
    let records: Vec<_> = names
        .iter()
        .map(|name| vfs_namespace_directory_record(name, path.starts_with("/System")))
        .collect();
    vfs_write_directory_records(&records, start_index, records_address, capacity)
}

fn vfs_namespace_directory_record(name: &str, system: bool) -> abi::file::DirectoryEntry {
    let bytes = name.as_bytes();
    let mut record = abi::file::DirectoryEntry {
        kind: abi::file::KIND_DIRECTORY,
        size: 0,
        flags: if system {
            abi::file::FLAG_SYSTEM | abi::file::FLAG_READ_ONLY
        } else {
            0
        },
        name_length: bytes.len() as u64,
        name: [0; abi::file::DIRECTORY_ENTRY_NAME_CAPACITY],
    };
    record.name[..bytes.len()].copy_from_slice(bytes);
    record
}

fn vfs_write_directory_records(
    records: &[abi::file::DirectoryEntry],
    start_index: usize,
    records_address: u64,
    capacity: usize,
) -> ControlOutcome {
    let mut written = 0usize;
    for record in records.iter().skip(start_index).take(capacity) {
        let destination = (records_address as *mut abi::file::DirectoryEntry).wrapping_add(written);
        unsafe { ptr::write_unaligned(destination, *record) };
        written = written.saturating_add(1);
    }
    ControlOutcome::Ready(written as u64)
}

fn vfs_route_stat(
    process_id: u64,
    path: &str,
    stat_address: u64,
    stat_length: u64,
    stack_pointer: usize,
) -> ControlOutcome {
    vfs_route_request(
        process_id,
        path,
        PendingVfsOperation::Stat {
            stat_address,
            stat_length,
        },
        stack_pointer,
    )
}

fn vfs_route_read_directory(
    process_id: u64,
    path: &str,
    start_index: usize,
    records_address: u64,
    capacity: usize,
    stack_pointer: usize,
) -> ControlOutcome {
    vfs_route_request(
        process_id,
        path,
        PendingVfsOperation::ReadDirectory {
            start_index,
            records_address,
            capacity,
        },
        stack_pointer,
    )
}

fn vfs_route_chdir(process_id: u64, path: &str, stack_pointer: usize) -> ControlOutcome {
    vfs_route_request(process_id, path, PendingVfsOperation::Chdir, stack_pointer)
}

fn vfs_route_open(
    process_id: u64,
    path: &str,
    options: vfs::OpenOptions,
    close_on_exec: bool,
    descriptor: u64,
    stack_pointer: usize,
) -> ControlOutcome {
    vfs_route_request(
        process_id,
        path,
        PendingVfsOperation::Open {
            options,
            close_on_exec,
            descriptor,
        },
        stack_pointer,
    )
}

fn vfs_route_unlink(process_id: u64, path: &str, stack_pointer: usize) -> ControlOutcome {
    vfs_route_request(process_id, path, PendingVfsOperation::Unlink, stack_pointer)
}

enum ExecutableLoadPoll {
    Pending,
    Ready(LoadedExecutable),
    Failed(i64),
}

enum ExecutableLoadStart {
    Vfs,
    Nullfs {
        backend_path: String,
        vfs_generation: u32,
    },
    Retry {
        request: filesystem_protocol::Request,
        operation: PendingNullfsProxyOperation,
    },
    Complete {
        result: Result<LoadedExecutable, i64>,
        vfs_generation: u32,
        provider_generation: u32,
        session_id: u64,
        session_generation: u64,
    },
}

fn executable_path_uses_service(path: &str) -> bool {
    vfs_path_has_prefix(path, "/Applications") || vfs_path_has_prefix(path, "/System")
}

fn executable_load_fail(
    process_id: u64,
    owner: ExecutableLoadOwner,
    stack_pointer: usize,
    error: i64,
) {
    let close = {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            return;
        };
        let Some(load) = process.pending_executable_load.as_mut() else {
            return;
        };
        if load.owner != owner || load.stack_pointer != stack_pointer {
            return;
        }
        let close = load.take_close_ticket();
        load.bytes.clear();
        load.retry = None;
        load.result = Some(Err(error));
        close
    };
    if let Some(close) = close {
        nullfs_proxy_enqueue_close_ticket(close);
    }
}

fn executable_load_set_route(
    process_id: u64,
    owner: ExecutableLoadOwner,
    stack_pointer: usize,
    vfs_generation: u32,
    backend_path: String,
) -> bool {
    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return false;
    };
    let Some(load) = process.pending_executable_load.as_mut() else {
        return false;
    };
    if load.owner != owner || load.stack_pointer != stack_pointer || load.result.is_some() {
        return false;
    }
    load.vfs_generation = vfs_generation;
    load.backend_path = Some(backend_path);
    true
}

fn executable_load_set_retry(
    process_id: u64,
    owner: ExecutableLoadOwner,
    stack_pointer: usize,
    request: filesystem_protocol::Request,
    operation: PendingNullfsProxyOperation,
) {
    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return;
    };
    let Some(load) = process.pending_executable_load.as_mut() else {
        return;
    };
    if load.owner == owner && load.stack_pointer == stack_pointer && load.result.is_none() {
        load.retry = Some((request, operation));
    }
}

fn poll_service_executable_load(
    process_id: u64,
    owner: ExecutableLoadOwner,
    path: &str,
    stack_pointer: usize,
) -> ExecutableLoadPoll {
    let action = {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            return ExecutableLoadPoll::Failed(ERR_NO_PROCESS);
        };
        if process.pending_executable_load.is_none() {
            process.pending_executable_load =
                Some(PendingExecutableLoad::new(owner, path, stack_pointer));
        }
        let Some(load) = process.pending_executable_load.as_mut() else {
            return ExecutableLoadPoll::Failed(ERR_IO);
        };
        if load.owner != owner || load.path != path || load.stack_pointer != stack_pointer {
            let close = load.take_close_ticket();
            process.pending_executable_load = None;
            drop(manager);
            if let Some(close) = close {
                nullfs_proxy_enqueue_close_ticket(close);
            }
            return ExecutableLoadPoll::Failed(ERR_IO);
        }
        if let Some(result) = load.result.take() {
            let complete = ExecutableLoadStart::Complete {
                result,
                vfs_generation: load.vfs_generation,
                provider_generation: load.provider_generation,
                session_id: load.session_id,
                session_generation: load.session_generation,
            };
            process.pending_executable_load = None;
            complete
        } else if process.pending_vfs_request.is_some() || process.pending_nullfs_proxy.is_some() {
            return ExecutableLoadPoll::Pending;
        } else if let Some((request, operation)) = load.retry.take() {
            ExecutableLoadStart::Retry { request, operation }
        } else if load.vfs_generation == 0 {
            ExecutableLoadStart::Vfs
        } else {
            let Some(backend_path) = load.backend_path.clone() else {
                return ExecutableLoadPoll::Failed(ERR_IO);
            };
            ExecutableLoadStart::Nullfs {
                backend_path,
                vfs_generation: load.vfs_generation,
            }
        }
    };

    let outcome = match action {
        ExecutableLoadStart::Vfs => vfs_route_request(
            process_id,
            path,
            PendingVfsOperation::LoadExecutable { owner },
            stack_pointer,
        ),
        ExecutableLoadStart::Nullfs {
            backend_path,
            vfs_generation,
        } => nullfs_proxy_start_path(
            process_id,
            path,
            &backend_path,
            NullfsPathPurpose::LoadExecutable {
                owner,
                vfs_generation,
            },
            stack_pointer,
        ),
        ExecutableLoadStart::Retry { request, operation } => {
            match nullfs_proxy_begin_request(process_id, request, operation.clone(), stack_pointer)
            {
                Ok(()) => ControlOutcome::Blocked,
                Err(error) if error == ERR_TRY_AGAIN => {
                    executable_load_set_retry(process_id, owner, stack_pointer, request, operation);
                    ControlOutcome::Ready(error_return(ERR_TRY_AGAIN))
                }
                Err(error) => ControlOutcome::Ready(error_return(error)),
            }
        }
        ExecutableLoadStart::Complete {
            result,
            vfs_generation,
            provider_generation,
            session_id,
            session_generation,
        } => {
            let authority_current = vfs_route_generation_is_current(vfs_generation)
                && nullfs_proxy_backend_is_current(
                    provider_generation,
                    session_id,
                    session_generation,
                );
            return match result {
                Ok(executable) if authority_current => ExecutableLoadPoll::Ready(executable),
                Ok(_) => ExecutableLoadPoll::Failed(ERR_IO),
                Err(error) => ExecutableLoadPoll::Failed(error),
            };
        }
    };
    match outcome {
        ControlOutcome::Blocked => ExecutableLoadPoll::Pending,
        ControlOutcome::Ready(result) if result as i64 == ERR_TRY_AGAIN => {
            ExecutableLoadPoll::Pending
        }
        ControlOutcome::Ready(result) => {
            let error = result as i64;
            executable_load_fail(process_id, owner, stack_pointer, error);
            let mut manager = PROCESS_MANAGER.lock();
            if let Some(process) = manager.process_mut(process_id) {
                process.pending_executable_load = None;
            }
            ExecutableLoadPoll::Failed(error)
        }
    }
}

fn vfs_complete_boot_open(
    process_id: u64,
    path: &str,
    options: vfs::OpenOptions,
    close_on_exec: bool,
    descriptor: u64,
) -> u64 {
    let metadata = match vfs::open(path, options) {
        Ok(metadata) => metadata,
        Err(error) => return error_return(vfs_errno(&error)),
    };
    let offset = if options.append { metadata.size } else { 0 };
    let handle = Arc::new(PreemptMutex::new(OpenFileState {
        path: metadata.path,
        offset,
        readable: options.read,
        writable: options.write,
        append: options.append,
        size: metadata.size,
        nullfs_size: None,
        backend: OpenFileBackend::Vfs,
    }));
    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return error_return(ERR_BAD_FILE_DESCRIPTOR);
    };
    if descriptor_in_use(process, descriptor) {
        return error_return(ERR_TOO_MANY_OPEN_FILES);
    }
    process.open_files.push(OpenFile {
        descriptor,
        handle,
        close_on_exec,
    });
    process.open_count = process.open_count.saturating_add(1);
    descriptor
}

fn vfs_route_request(
    process_id: u64,
    path: &str,
    operation: PendingVfsOperation,
    stack_pointer: usize,
) -> ControlOutcome {
    if path.len() > vfs_protocol::MAX_PATH_BYTES {
        return ControlOutcome::Ready(error_return(abi::errno::NAME_TOO_LONG));
    }
    let (endpoint, generation, request_id) = {
        let mut state = VFS_ROUTE.lock();
        let Some(endpoint) = state.request_endpoint.filter(|_| state.ready) else {
            return ControlOutcome::Ready(error_return(ERR_IO));
        };
        if state.active_request_id != 0 {
            return ControlOutcome::Ready(error_return(ERR_TRY_AGAIN));
        }
        let request_id = state.next_request_id.max(1);
        state.next_request_id = request_id.wrapping_add(1).max(1);
        state.active_request_id = request_id;
        (endpoint, state.generation, request_id)
    };
    let reply_endpoint = match tmpfs_proxy_create_reply_endpoint() {
        Ok(endpoint) => endpoint,
        Err(error) => {
            let mut state = VFS_ROUTE.lock();
            if state.generation == generation && state.active_request_id == request_id {
                state.active_request_id = 0;
            }
            return ControlOutcome::Ready(error_return(error));
        }
    };
    let mut request = vfs_protocol::Request::EMPTY;
    request.operation = vfs_protocol::operation::RESOLVE;
    request.request_id = request_id;
    request.path_length = path.len() as u16;
    request.path[..path.len()].copy_from_slice(path.as_bytes());
    let pushed = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        match registry.object_index(endpoint) {
            Some(index) => match &mut registry.objects[index].data {
                CapabilityObjectData::Endpoint(target)
                    if target.queue.len() < abi::limits::MAX_ENDPOINT_MESSAGES =>
                {
                    target.queue.push_back(EndpointMessage {
                        sender_process_id: 0,
                        bytes: tmpfs_proxy_value_bytes(&request).to_vec(),
                        capabilities: vec![TransferredCapability {
                            object: reply_endpoint,
                            rights: abi::capability::RIGHT_SEND,
                        }],
                    });
                    true
                }
                _ => false,
            },
            None => false,
        }
    };
    let pending = PendingVfsRequest {
        reply_endpoint,
        request_id,
        generation,
        path: String::from(path),
        operation,
        stack_pointer,
    };
    if !pushed {
        vfs_request_release(&pending);
        return ControlOutcome::Ready(error_return(ERR_TRY_AGAIN));
    }
    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        drop(manager);
        vfs_request_release(&pending);
        return ControlOutcome::Ready(error_return(ERR_NO_PROCESS));
    };
    if process.pending_vfs_request.is_some()
        || process.pending_tmpfs_proxy.is_some()
        || process.pending_nullfs_proxy.is_some()
    {
        drop(manager);
        vfs_request_release(&pending);
        return ControlOutcome::Ready(error_return(ERR_IO));
    }
    process.pending_vfs_request = Some(pending);
    process.state = ProcessState::Blocked;
    drop(manager);
    wake_endpoint_waiter(endpoint);
    ControlOutcome::Blocked
}

fn vfs_request_take_reply(pending: &PendingVfsRequest) -> Option<Result<vfs_protocol::Reply, i64>> {
    let message = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        let index = registry.object_index(pending.reply_endpoint)?;
        let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[index].data else {
            return Some(Err(ERR_IO));
        };
        endpoint.queue.pop_front()
    }?;
    if !message.capabilities.is_empty() || message.bytes.len() != size_of::<vfs_protocol::Reply>() {
        return Some(Err(ERR_IO));
    }
    let reply =
        unsafe { ptr::read_unaligned(message.bytes.as_ptr() as *const vfs_protocol::Reply) };
    let state = *VFS_ROUTE.lock();
    if reply.version != vfs_protocol::VERSION
        || reply.operation != vfs_protocol::operation::RESOLVE
        || reply.request_id != pending.request_id
        || reply.reserved != [0; 8]
        || state.generation != pending.generation
        || state.active_request_id != pending.request_id
        || !vfs_stat_route_is_valid(&reply, &pending.path)
    {
        Some(Err(ERR_IO))
    } else {
        Some(Ok(reply))
    }
}

fn vfs_stat_route_is_valid(reply: &vfs_protocol::Reply, path: &str) -> bool {
    if !vfs_protocol::status::known(reply.status) {
        return false;
    }
    let binding = reply.binding_prefix();
    let empty_backing = binding == Ok(None);
    if reply.status == vfs_protocol::status::NOT_FOUND {
        return reply.route_id == 0
            && reply.backend == 0
            && reply.prefix_length == 0
            && empty_backing;
    }
    if reply.status == vfs_protocol::status::INVALID {
        return reply.route_id == 0
            && reply.backend == 0
            && reply.prefix_length == 0
            && empty_backing;
    }
    if reply.status != vfs_protocol::status::OK {
        return false;
    }
    let expected = if vfs_path_has_prefix(path, "/System/Applications") {
        (
            "/System/Applications",
            vfs_protocol::route::SYSTEM_APPLICATIONS,
            vfs_protocol::backend::NULLFS,
            Some("/System/Applications"),
        )
    } else if vfs_path_has_prefix(path, "/System/services") {
        (
            "/System/services",
            vfs_protocol::route::SYSTEM_SERVICES,
            vfs_protocol::backend::NULLFS,
            Some("/System/services"),
        )
    } else if vfs_path_has_prefix(path, "/System/drivers") {
        (
            "/System/drivers",
            vfs_protocol::route::SYSTEM_DRIVERS,
            vfs_protocol::backend::NULLFS,
            Some("/System/drivers"),
        )
    } else if vfs_path_has_prefix(path, "/System/var/log") {
        (
            "/System/var/log",
            vfs_protocol::route::SYSTEM_VAR_LOG,
            vfs_protocol::backend::NULLFS,
            Some("/System/var/log"),
        )
    } else if vfs_path_has_prefix(path, "/System/config") {
        (
            "/System/config",
            vfs_protocol::route::SYSTEM_CONFIG,
            vfs_protocol::backend::NULLFS,
            Some("/System/config"),
        )
    } else if vfs_path_has_prefix(path, "/System/bin") {
        (
            "/System/bin",
            vfs_protocol::route::SYSTEM_BIN,
            vfs_protocol::backend::NULLFS,
            Some("/System/bin"),
        )
    } else if vfs_path_has_prefix(path, "/System/lib") {
        (
            "/System/lib",
            vfs_protocol::route::SYSTEM_LIB,
            vfs_protocol::backend::NULLFS,
            Some("/System/lib"),
        )
    } else if vfs_path_has_prefix(path, "/System/var") {
        (
            "/System/var",
            vfs_protocol::route::SYSTEM_VAR,
            vfs_protocol::backend::NULLFS,
            Some("/System/var"),
        )
    } else if vfs_path_has_prefix(path, "/System") {
        (
            "/System",
            vfs_protocol::route::SYSTEM,
            vfs_protocol::backend::NULLFS,
            Some("/System"),
        )
    } else if vfs_path_has_prefix(path, "/Applications") {
        (
            "/Applications",
            vfs_protocol::route::APPLICATIONS,
            vfs_protocol::backend::NULLFS,
            Some("/Applications"),
        )
    } else if vfs_path_has_prefix(path, nullfs_primary_volume::MOUNT_PATH) {
        (
            nullfs_primary_volume::MOUNT_PATH,
            vfs_protocol::route::NULLSTAR_VOLUME,
            vfs_protocol::backend::NULLFS,
            None,
        )
    } else if vfs_path_has_prefix(path, "/Volumes") {
        (
            "/Volumes",
            vfs_protocol::route::VOLUMES,
            vfs_protocol::backend::NAMESPACE,
            None,
        )
    } else if vfs_path_has_prefix(path, "/Users") {
        (
            "/Users",
            vfs_protocol::route::USERS,
            vfs_protocol::backend::NULLFS,
            Some("/Users"),
        )
    } else if vfs_path_has_prefix(path, "/tmp") {
        (
            "/tmp",
            vfs_protocol::route::TMP,
            vfs_protocol::backend::TMPFS,
            None,
        )
    } else if vfs_path_has_prefix(path, "/dev") {
        (
            "/dev",
            vfs_protocol::route::DEV,
            vfs_protocol::backend::NAMESPACE,
            None,
        )
    } else {
        (
            "/",
            vfs_protocol::route::ROOT,
            vfs_protocol::backend::BOOT_FILESYSTEM,
            None,
        )
    };
    reply.route_id == expected.1
        && reply.backend == expected.2
        && usize::from(reply.prefix_length) == expected.0.len()
        && match expected.3 {
            Some(backing_prefix) => binding == Ok(Some(backing_prefix)),
            None => empty_backing,
        }
}

fn vfs_nullfs_backend_path(
    reply: &vfs_protocol::Reply,
    canonical_path: &str,
) -> Result<String, i64> {
    if reply.flags == vfs_protocol::reply_flags::BINDING {
        let prefix_length = usize::from(reply.prefix_length);
        let suffix = canonical_path
            .get(prefix_length..)
            .ok_or(ERR_INVALID_ARGUMENT)?;
        if !suffix.is_empty() && !suffix.starts_with('/') {
            return Err(ERR_INVALID_ARGUMENT);
        }
        let backing_prefix = reply
            .binding_prefix()
            .map_err(|_| ERR_INVALID_ARGUMENT)?
            .ok_or(ERR_INVALID_ARGUMENT)?;
        let mut backend_path = String::from(backing_prefix);
        backend_path.push_str(suffix);
        if backend_path.len() > vfs_protocol::MAX_PATH_BYTES {
            return Err(abi::errno::NAME_TOO_LONG);
        }
        Ok(backend_path)
    } else if reply.route_id == vfs_protocol::route::NULLSTAR_VOLUME {
        let suffix = canonical_path
            .strip_prefix(NULLFS_MOUNT_PATH)
            .ok_or(ERR_INVALID_ARGUMENT)?;
        Ok(if suffix.is_empty() {
            String::from("/")
        } else {
            String::from(suffix)
        })
    } else {
        Err(ERR_INVALID_ARGUMENT)
    }
}

fn vfs_path_has_prefix(path: &str, prefix: &str) -> bool {
    vfs_protocol::path_has_prefix(path, prefix)
}

fn vfs_request_release(pending: &PendingVfsRequest) {
    let mut state = VFS_ROUTE.lock();
    if state.active_request_id == pending.request_id {
        state.active_request_id = 0;
    }
    drop(state);
    tmpfs_proxy_release_reply_endpoint(pending.reply_endpoint);
}

pub fn wait_for_processes(
    frame_allocator: &mut BootInfoFrameAllocator,
    process_ids: &[u64],
) -> ManagerSnapshot {
    let mut pending = process_ids.to_vec();
    loop {
        let _ = reap(frame_allocator);
        cpu_interrupts::without_interrupts(|| {
            let mut manager = PROCESS_MANAGER.lock();
            pending.retain(|process_id| manager.take_result(*process_id).is_none());
        });
        if pending.is_empty() {
            return snapshot();
        }
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
        remove_object_waiter(process_id);
        capability_remove_process(process_id);
        let terminal_parent = process.terminal_parent;
        terminal::detach(process_id);
        if let Some(parent_process_id) = terminal_parent {
            let parent_exists = PROCESS_MANAGER
                .lock()
                .processes
                .iter()
                .any(|candidate| candidate.process_id == parent_process_id);
            if parent_exists && terminal::foreground_process() != Some(parent_process_id) {
                let _ = terminal::attach(parent_process_id);
            }
        }
        release_stream_target(process.stdin_target.take(), StreamAccess::Read);
        release_stream_target(process.stdout_target.take(), StreamAccess::Write);
        release_stream_target(process.stderr_target.take(), StreamAccess::Write);
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
        if let Some(pending) = process.pending_tmpfs_proxy.take() {
            tmpfs_proxy_abandon_pending(pending);
        }
        if let Some(pending) = process.pending_nullfs_proxy.take() {
            nullfs_proxy_abandon_pending(pending);
        }
        if let Some(pending) = process.pending_vfs_request.take() {
            vfs_request_release(&pending);
        }
        if let Some(load) = process.pending_executable_load.as_mut()
            && let Some(close) = load.take_close_ticket()
        {
            nullfs_proxy_enqueue_close_ticket(close);
        }
        let frames_reclaimed = release_owned_frames(&mut process.owned_frames, frame_allocator);
        debug_assert_eq!(process.task_id, task.task_id);
        let result = process.result(frames_reclaimed, task.scheduled_count, task.runtime_ticks)?;
        if let Some(job) = process.job {
            capability_job_record_exit(job, process_id, child_status(&result));
        }
        cpu_interrupts::without_interrupts(|| {
            let mut manager = PROCESS_MANAGER.lock();
            manager.orphan_children_of(process_id);
            manager.reaped = manager.reaped.saturating_add(1);
            manager.frames_reclaimed = manager
                .frames_reclaimed
                .saturating_add(frames_reclaimed as u64);
            manager.record_result(result);
        });
        reaped = reaped.saturating_add(1);
    }

    Ok(reaped)
}

pub fn snapshot() -> ManagerSnapshot {
    cpu_interrupts::without_interrupts(|| PROCESS_MANAGER.lock().snapshot())
}

pub fn syscall_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(galactic_syscall_interrupt_entry as *const () as usize as u64)
}

pub fn page_fault_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(galactic_page_fault_interrupt_entry as *const () as usize as u64)
}

pub fn general_protection_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(galactic_general_protection_interrupt_entry as *const () as usize as u64)
}

#[unsafe(no_mangle)]
pub extern "C" fn galactic_syscall_dispatch(current_stack_pointer: usize) -> usize {
    let registers_pointer = current_stack_pointer as *mut SavedRegisters;
    let syscall_number = unsafe { (*registers_pointer).rax };
    let Some(process_id) = scheduler::current_process_id() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };

    {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
            return current_stack_pointer;
        };
        process.syscall_count = process.syscall_count.saturating_add(1);
    }

    if syscall_number == SYSCALL_SIGNAL_RETURN {
        match syscall_signal_return(process_id, current_stack_pointer) {
            Ok(context) => unsafe { (current_stack_pointer as *mut SavedContext).write(context) },
            Err(error) => unsafe { (*registers_pointer).rax = error_return(error) },
        }
        return current_stack_pointer;
    }

    let registers = unsafe { &mut *registers_pointer };
    match syscall_number {
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
            SpawnSyscallArgs {
                address: registers.rdi,
                length: registers.rsi,
                flags: registers.rdx,
                stdin_descriptor: registers.r10,
                stdout_descriptor: registers.r8,
                stderr_descriptor: registers.r9,
                process_group_argument: registers.rbx,
            },
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
        SYSCALL_FOREGROUND_PROCESS_GROUP => {
            registers.rax = syscall_foreground_process_group(process_id, registers.rdi);
            current_stack_pointer
        }
        SYSCALL_SEEK => {
            registers.rax = syscall_seek(process_id, registers.rdi, registers.rsi, registers.rdx);
            current_stack_pointer
        }
        SYSCALL_EXECVE => {
            match syscall_execve(
                process_id,
                registers.rdi,
                registers.rsi,
                current_stack_pointer,
            ) {
                ControlOutcome::Ready(result) => {
                    registers.rax = result;
                    current_stack_pointer
                }
                ControlOutcome::Blocked => scheduler::block_current(current_stack_pointer),
            }
        }
        SYSCALL_SET_DESCRIPTOR_FLAGS => {
            registers.rax = syscall_set_descriptor_flags(process_id, registers.rdi, registers.rsi);
            current_stack_pointer
        }
        SYSCALL_FORK => match syscall_fork(process_id, current_stack_pointer) {
            ControlOutcome::Ready(result) => {
                registers.rax = result;
                current_stack_pointer
            }
            ControlOutcome::Blocked => scheduler::block_current(current_stack_pointer),
        },
        SYSCALL_SIGNAL_ACTION => {
            registers.rax =
                syscall_signal_action(process_id, registers.rdi, registers.rsi, registers.rdx);
            current_stack_pointer
        }
        SYSCALL_SIGNAL_MASK => {
            registers.rax = syscall_signal_mask(process_id, registers.rdi, registers.rsi);
            current_stack_pointer
        }
        SYSCALL_ENVIRONMENT_SET => {
            registers.rax = syscall_environment_set(
                process_id,
                registers.rdi,
                registers.rsi,
                registers.rdx,
                registers.r10,
            );
            current_stack_pointer
        }
        SYSCALL_ENVIRONMENT_UNSET => {
            registers.rax = syscall_environment_unset(process_id, registers.rdi, registers.rsi);
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
    let frame = unsafe { &mut *(current_stack_pointer as *mut FaultStack) };
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

    let fault_address = if vector == PAGE_FAULT_VECTOR {
        Cr2::read().map(|address| address.as_u64()).unwrap_or(0)
    } else {
        0
    };
    let cow_page_address = align_down(fault_address);
    let is_cow_fault = vector == PAGE_FAULT_VECTOR
        && frame.error_code & 0b111 == 0b111
        && PROCESS_MANAGER
            .lock()
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
            .is_some_and(|process| {
                process.pending_cow_fault.is_none()
                    && process
                        .pages
                        .iter()
                        .any(|page| page.virtual_address == cow_page_address && page.copy_on_write)
            });
    if is_cow_fault {
        let original_rip = frame.rip;
        let original_cs = frame.cs;
        let original_rflags = frame.rflags;
        let original_stack_pointer = frame.stack_pointer;
        let original_stack_segment = frame.stack_segment;
        frame.error_code = original_rip;
        frame.rip = original_cs;
        frame.cs = original_rflags;
        frame.rflags = original_stack_pointer;
        frame.stack_pointer = original_stack_segment;
        {
            let mut manager = PROCESS_MANAGER.lock();
            let process = manager
                .process_mut(process_id)
                .expect("copy-on-write process disappeared");
            process.pending_cow_fault = Some(PendingCowFault {
                page_address: cow_page_address,
            });
            process.state = ProcessState::Blocked;
            process.cow_fault_count = process.cow_fault_count.saturating_add(1);
            manager.cow_faults = manager.cow_faults.saturating_add(1);
        }
        return scheduler::block_current(current_stack_pointer);
    }
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
#[derive(Clone, Copy)]
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

struct SpawnSyscallArgs {
    address: u64,
    length: u64,
    flags: u64,
    stdin_descriptor: u64,
    stdout_descriptor: u64,
    stderr_descriptor: u64,
    process_group_argument: u64,
}

fn syscall_spawn_command(
    process_id: u64,
    arguments: SpawnSyscallArgs,
    current_stack_pointer: usize,
) -> ControlOutcome {
    let SpawnSyscallArgs {
        address,
        length,
        flags,
        stdin_descriptor,
        stdout_descriptor,
        stderr_descriptor,
        process_group_argument,
    } = arguments;
    let allowed_flags = abi::spawn::ALLOWED_FLAGS;
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
    let stderr_descriptor =
        (use_descriptors && stderr_descriptor != DEFAULT_DESCRIPTOR).then_some(stderr_descriptor);

    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return ControlOutcome::Ready(error_return(ERR_NO_CHILD));
    };
    if resolve_stream_descriptor(process, stdin_descriptor, StreamAccess::Read).is_err()
        || resolve_stream_descriptor(process, stdout_descriptor, StreamAccess::Write).is_err()
        || resolve_stream_descriptor(process, stderr_descriptor, StreamAccess::Write).is_err()
    {
        return ControlOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
    }
    if process.pending_child_spawn.is_some()
        || process.pending_child_wait.is_some()
        || process.pending_exec.is_some()
        || process.pending_fork.is_some()
        || process.pending_tmpfs_proxy.is_some()
        || process.pending_nullfs_proxy.is_some()
        || process.pending_vfs_request.is_some()
    {
        return ControlOutcome::Ready(error_return(ERR_IO));
    }
    process.pending_child_spawn = Some(PendingChildSpawn {
        path,
        arguments,
        foreground: flags & SPAWN_FOREGROUND != 0,
        stdin_descriptor,
        stdout_descriptor,
        stderr_descriptor,
        new_process_group,
        process_group_id,
        stack_pointer: current_stack_pointer,
        claimed: false,
    });
    process.state = ProcessState::Blocked;
    ControlOutcome::Blocked
}

fn syscall_execve(
    process_id: u64,
    address: u64,
    length: u64,
    current_stack_pointer: usize,
) -> ControlOutcome {
    let command = match user_text(process_id, address, length, MAX_COMMAND_BYTES) {
        Ok(command) => command,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    let (path, arguments) = match parse_command_line(&command) {
        Ok(command) => command,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };

    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return ControlOutcome::Ready(error_return(ERR_NO_PROCESS));
    };
    if process.pending_terminal_read.is_some()
        || process.pending_pipe_read.is_some()
        || process.pending_pipe_write.is_some()
        || process.pending_tmpfs_proxy.is_some()
        || process.pending_nullfs_proxy.is_some()
        || process.pending_vfs_request.is_some()
        || process.pending_child_spawn.is_some()
        || process.pending_child_wait.is_some()
        || process.pending_exec.is_some()
        || process.pending_fork.is_some()
    {
        return ControlOutcome::Ready(error_return(ERR_IO));
    }
    process.pending_exec = Some(PendingExec {
        path,
        arguments,
        stack_pointer: current_stack_pointer,
    });
    process.state = ProcessState::Blocked;
    ControlOutcome::Blocked
}

fn syscall_fork(process_id: u64, current_stack_pointer: usize) -> ControlOutcome {
    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return ControlOutcome::Ready(error_return(ERR_NO_PROCESS));
    };
    if process.pending_terminal_read.is_some()
        || process.pending_pipe_read.is_some()
        || process.pending_pipe_write.is_some()
        || process.pending_tmpfs_proxy.is_some()
        || process.pending_nullfs_proxy.is_some()
        || process.pending_vfs_request.is_some()
        || process.pending_child_spawn.is_some()
        || process.pending_child_wait.is_some()
        || process.pending_exec.is_some()
        || process.pending_fork.is_some()
        || process.pending_cow_fault.is_some()
        || process.active_signal.is_some()
    {
        return ControlOutcome::Ready(error_return(ERR_IO));
    }
    process.pending_fork = Some(PendingFork {
        stack_pointer: current_stack_pointer,
    });
    process.state = ProcessState::Blocked;
    ControlOutcome::Blocked
}

fn syscall_set_descriptor_flags(process_id: u64, descriptor: u64, flags: u64) -> u64 {
    if descriptor < 3 || flags & !DESCRIPTOR_ALLOWED_FLAGS != 0 {
        return error_return(ERR_INVALID_ARGUMENT);
    }
    let close_on_exec = flags & DESCRIPTOR_CLOSE_ON_EXEC != 0;
    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return error_return(ERR_BAD_FILE_DESCRIPTOR);
    };
    if let Some(file) = process
        .open_files
        .iter_mut()
        .find(|file| file.descriptor == descriptor)
    {
        file.close_on_exec = close_on_exec;
        return 0;
    }
    if let Some(pipe) = process
        .pipe_descriptors
        .iter_mut()
        .find(|pipe| pipe.descriptor == descriptor)
    {
        pipe.close_on_exec = close_on_exec;
        return 0;
    }
    error_return(ERR_BAD_FILE_DESCRIPTOR)
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
                close_on_exec: false,
            });
            process.pipe_descriptors.push(PipeDescriptor {
                descriptor: writer_descriptor,
                pipe_id,
                direction: PipeDirection::Writer,
                close_on_exec: false,
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
    let final_status = manager
        .take_child_result(process_id, child_process_id)
        .as_ref()
        .map(child_status);
    if let Some(status) = final_status {
        note_child_wait(&mut manager, process_id);
        return ControlOutcome::Ready(status);
    }

    let child_index = manager.processes.iter().position(|child| {
        child.process_id == child_process_id && child.parent_process_id == Some(process_id)
    });
    let Some(child_index) = child_index else {
        return ControlOutcome::Ready(error_return(ERR_NO_CHILD));
    };
    if let Some(status) = manager.processes[child_index].take_parent_status() {
        note_child_wait(&mut manager, process_id);
        return ControlOutcome::Ready(status);
    }

    let Some(process) = manager.process_mut(process_id) else {
        return ControlOutcome::Ready(error_return(ERR_NO_CHILD));
    };
    if process.pending_child_spawn.is_some()
        || process.pending_child_wait.is_some()
        || process.pending_exec.is_some()
        || process.pending_fork.is_some()
        || process.pending_tmpfs_proxy.is_some()
        || process.pending_nullfs_proxy.is_some()
        || process.pending_vfs_request.is_some()
    {
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
    let completed_status = manager
        .take_child_result(process_id, child_process_id)
        .as_ref()
        .map(child_status);
    let active_child_index = manager.processes.iter().position(|child| {
        child.process_id == child_process_id && child.parent_process_id == Some(process_id)
    });
    if completed_status.is_none() && active_child_index.is_none() {
        return error_return(ERR_NO_CHILD);
    }
    let child_status =
        active_child_index.and_then(|index| manager.processes[index].take_parent_status());

    let pending = completed_status.is_none() && child_status.is_none();
    {
        let Some(process) = manager.process_mut(process_id) else {
            return error_return(ERR_NO_CHILD);
        };
        process.child_poll_count = process.child_poll_count.saturating_add(1);
        if pending {
            process.child_poll_pending_count = process.child_poll_pending_count.saturating_add(1);
        }
    }
    if completed_status.is_some() {
        note_child_wait(&mut manager, process_id);
    }

    completed_status
        .or(child_status)
        .unwrap_or_else(|| error_return(ERR_TRY_AGAIN))
}

fn signal_is_supported(signal: u64) -> bool {
    abi::signal::bit(signal) & SIGNAL_SUPPORTED_MASK != 0
}

fn signal_is_catchable(signal: u64) -> bool {
    signal_is_supported(signal) && !matches!(signal, SIGNAL_KILL | SIGNAL_STOP)
}

fn user_executable_address(process_id: u64, address: u64) -> bool {
    PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == process_id)
        .is_some_and(|process| {
            process
                .ranges
                .iter()
                .any(|range| range.executable && range.contains(address, 1))
        })
}

fn read_user_signal_action(
    process_id: u64,
    address: u64,
) -> Result<abi::signal_action::Action, i64> {
    if !user_range_allows(
        process_id,
        address,
        size_of::<abi::signal_action::Action>(),
        false,
    ) {
        return Err(ERR_BAD_ADDRESS);
    }
    Ok(unsafe { ptr::read_unaligned(address as *const abi::signal_action::Action) })
}

fn write_user_signal_action(
    process_id: u64,
    address: u64,
    action: abi::signal_action::Action,
) -> Result<(), i64> {
    if !user_range_allows(
        process_id,
        address,
        size_of::<abi::signal_action::Action>(),
        true,
    ) {
        return Err(ERR_BAD_ADDRESS);
    }
    unsafe { ptr::write_unaligned(address as *mut abi::signal_action::Action, action) };
    Ok(())
}

fn validate_signal_action(
    process_id: u64,
    signal: u64,
    mut action: abi::signal_action::Action,
) -> Result<abi::signal_action::Action, i64> {
    if action.mask & !SIGNAL_SUPPORTED_MASK != 0
        || action.flags & !abi::signal_action::ALLOWED_FLAGS != 0
    {
        return Err(ERR_INVALID_ARGUMENT);
    }
    action.mask &= !SIGNAL_UNBLOCKABLE_MASK;
    match action.handler {
        abi::signal_action::DEFAULT | abi::signal_action::IGNORE => {
            if matches!(signal, SIGNAL_KILL | SIGNAL_STOP)
                && action.handler != abi::signal_action::DEFAULT
            {
                return Err(ERR_INVALID_ARGUMENT);
            }
            action.restorer = 0;
        }
        _ => {
            if !signal_is_catchable(signal)
                || !user_executable_address(process_id, action.handler)
                || !user_executable_address(process_id, action.restorer)
            {
                return Err(ERR_INVALID_ARGUMENT);
            }
        }
    }
    Ok(action)
}

fn syscall_signal_action(
    process_id: u64,
    signal: u64,
    action_address: u64,
    previous_address: u64,
) -> u64 {
    if !signal_is_supported(signal) {
        return error_return(ERR_INVALID_ARGUMENT);
    }
    if previous_address != 0
        && !user_range_allows(
            process_id,
            previous_address,
            size_of::<abi::signal_action::Action>(),
            true,
        )
    {
        return error_return(ERR_BAD_ADDRESS);
    }
    let action = if action_address == 0 {
        None
    } else {
        match read_user_signal_action(process_id, action_address)
            .and_then(|action| validate_signal_action(process_id, signal, action))
        {
            Ok(action) => Some(action),
            Err(error) => return error_return(error),
        }
    };

    let (previous, ignored_pending) = {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            return error_return(ERR_NO_PROCESS);
        };
        let previous = process.signal_action(signal);
        let mut ignored_pending = false;
        if let Some(action) = action {
            let index = signal as usize;
            process.signal_actions[index] = action;
            if action.handler == abi::signal_action::IGNORE
                && process.pending_signals & abi::signal::bit(signal) != 0
            {
                process.clear_pending_signal(signal);
                process.signal_ignored_count = process.signal_ignored_count.saturating_add(1);
                ignored_pending = true;
            }
        }
        if ignored_pending {
            manager.signal_ignores = manager.signal_ignores.saturating_add(1);
        }
        (previous, ignored_pending)
    };
    let _ = ignored_pending;

    if previous_address != 0
        && let Err(error) = write_user_signal_action(process_id, previous_address, previous)
    {
        return error_return(error);
    }
    0
}

fn syscall_signal_mask(process_id: u64, how: u64, mask: u64) -> u64 {
    if mask & !SIGNAL_SUPPORTED_MASK != 0 {
        return error_return(ERR_INVALID_ARGUMENT);
    }
    let mask = mask & !SIGNAL_UNBLOCKABLE_MASK;
    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return error_return(ERR_NO_PROCESS);
    };
    let previous = process.signal_mask;
    process.signal_mask = match how {
        abi::signal_mask::BLOCK => process.signal_mask | mask,
        abi::signal_mask::UNBLOCK => process.signal_mask & !mask,
        abi::signal_mask::SET => mask,
        _ => return error_return(ERR_INVALID_ARGUMENT),
    };
    previous
}

fn syscall_environment_set(
    process_id: u64,
    name_address: u64,
    name_length: u64,
    value_address: u64,
    value_length: u64,
) -> u64 {
    let name = match user_text(
        process_id,
        name_address,
        name_length,
        MAX_ENVIRONMENT_NAME_BYTES,
    ) {
        Ok(name) if valid_environment_name(&name) => name,
        Ok(_) => return error_return(ERR_INVALID_ARGUMENT),
        Err(error) => return error_return(error),
    };
    let value = match user_text_allow_empty(
        process_id,
        value_address,
        value_length,
        MAX_ENVIRONMENT_BYTES,
    ) {
        Ok(value) => value,
        Err(error) => return error_return(error),
    };
    let entry_bytes = match name
        .len()
        .checked_add(1)
        .and_then(|length| length.checked_add(value.len()))
        .and_then(|length| length.checked_add(1))
    {
        Some(length) => length,
        None => return error_return(ERR_ARGUMENT_TOO_LARGE),
    };
    if entry_bytes > MAX_ENVIRONMENT_BYTES {
        return error_return(ERR_ARGUMENT_TOO_LARGE);
    }
    let mut entry = String::with_capacity(entry_bytes.saturating_sub(1));
    entry.push_str(&name);
    entry.push('=');
    entry.push_str(&value);

    let mut manager = PROCESS_MANAGER.lock();
    let changed = {
        let Some(process) = manager.process_mut(process_id) else {
            return error_return(ERR_NO_PROCESS);
        };
        let existing = process
            .environment
            .iter()
            .position(|candidate| environment_name(candidate) == Some(name.as_str()));
        if existing.is_none() && process.environment.len() >= MAX_ENVIRONMENT_VARIABLES {
            return error_return(ERR_NO_SPACE);
        }
        let current_bytes = match environment_serialized_bytes(&process.environment) {
            Some(bytes) => bytes,
            None => return error_return(ERR_IO),
        };
        let old_bytes = existing
            .map(|index| process.environment[index].len().saturating_add(1))
            .unwrap_or(0);
        let total_bytes = current_bytes
            .saturating_sub(old_bytes)
            .saturating_add(entry_bytes);
        if total_bytes > MAX_ENVIRONMENT_BYTES {
            return error_return(ERR_ARGUMENT_TOO_LARGE);
        }
        match existing {
            Some(index) if process.environment[index] == entry => false,
            Some(index) => {
                process.environment[index] = entry;
                true
            }
            None => {
                process.environment.push(entry);
                true
            }
        }
    };
    if changed {
        let process = manager
            .process_mut(process_id)
            .expect("environment process disappeared during update");
        process.environment_change_count = process.environment_change_count.saturating_add(1);
        manager.environment_changes = manager.environment_changes.saturating_add(1);
    }
    0
}

fn syscall_environment_unset(process_id: u64, name_address: u64, name_length: u64) -> u64 {
    let name = match user_text(
        process_id,
        name_address,
        name_length,
        MAX_ENVIRONMENT_NAME_BYTES,
    ) {
        Ok(name) if valid_environment_name(&name) => name,
        Ok(_) => return error_return(ERR_INVALID_ARGUMENT),
        Err(error) => return error_return(error),
    };
    let mut manager = PROCESS_MANAGER.lock();
    let changed = {
        let Some(process) = manager.process_mut(process_id) else {
            return error_return(ERR_NO_PROCESS);
        };
        let Some(index) = process
            .environment
            .iter()
            .position(|candidate| environment_name(candidate) == Some(name.as_str()))
        else {
            return 0;
        };
        process.environment.remove(index);
        true
    };
    if changed {
        let process = manager
            .process_mut(process_id)
            .expect("environment process disappeared during removal");
        process.environment_change_count = process.environment_change_count.saturating_add(1);
        manager.environment_changes = manager.environment_changes.saturating_add(1);
    }
    0
}

fn syscall_signal_return(
    process_id: u64,
    current_stack_pointer: usize,
) -> Result<SavedContext, i64> {
    let (active, current_user_stack) = {
        let manager = PROCESS_MANAGER.lock();
        let process = manager
            .processes
            .iter()
            .find(|process| process.process_id == process_id)
            .ok_or(ERR_NO_PROCESS)?;
        validate_kernel_context_pointer(process, current_stack_pointer).map_err(|_| ERR_IO)?;
        let active = process.active_signal.ok_or(ERR_INVALID_ARGUMENT)?;
        let context = unsafe { &*(current_stack_pointer as *const SavedContext) };
        (active, context.stack_pointer)
    };
    let expected_stack = active
        .frame_address
        .checked_add(size_of::<u64>() as u64)
        .ok_or(ERR_BAD_ADDRESS)?;
    if current_user_stack != expected_stack
        || !user_range_allows(
            process_id,
            active.frame_address,
            size_of::<abi::signal_action::Frame>(),
            false,
        )
    {
        return Err(ERR_BAD_ADDRESS);
    }
    let frame =
        unsafe { ptr::read_unaligned(active.frame_address as *const abi::signal_action::Frame) };
    if frame.return_address != active.restorer
        || frame.magic != abi::signal_action::FRAME_MAGIC
        || frame.signal != active.signal
        || frame.previous_mask != active.previous_mask
        || frame.cookie != active.cookie
    {
        return Err(ERR_INVALID_ARGUMENT);
    }

    let mut manager = PROCESS_MANAGER.lock();
    let process = manager.process_mut(process_id).ok_or(ERR_NO_PROCESS)?;
    if process
        .active_signal
        .is_none_or(|current| current.cookie != active.cookie)
    {
        return Err(ERR_INVALID_ARGUMENT);
    }
    process.active_signal = None;
    process.signal_mask = active.previous_mask;
    process.signal_return_count = process.signal_return_count.saturating_add(1);
    manager.signal_returns = manager.signal_returns.saturating_add(1);
    Ok(active.saved_context)
}

fn syscall_signal_process_group(process_id: u64, process_group_id: u64, signal: u64) -> u64 {
    match deliver_signal_group(Some(process_id), process_group_id, signal) {
        Ok(count) => count as u64,
        Err(error) => error_return(error),
    }
}

fn syscall_foreground_process_group(process_id: u64, process_group_id: u64) -> u64 {
    match foreground_process_group(process_id, process_group_id) {
        Ok(count) => count as u64,
        Err(error) => error_return(error),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DefaultSignalAction {
    Terminate,
    Stop,
    Continue,
}

fn default_signal_action(signal: u64) -> Option<DefaultSignalAction> {
    match signal {
        SIGNAL_INTERRUPT | SIGNAL_KILL | SIGNAL_TERMINATE => Some(DefaultSignalAction::Terminate),
        SIGNAL_STOP | SIGNAL_TERMINAL_STOP => Some(DefaultSignalAction::Stop),
        SIGNAL_CONTINUE => Some(DefaultSignalAction::Continue),
        _ => None,
    }
}

fn terminate_process_with_signal(process_id: u64, signal: u64, record_received: bool) -> bool {
    if !scheduler::terminate_process(process_id) {
        return false;
    }
    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return false;
    };
    process.state = ProcessState::Signaled;
    process.stopped_resume_state = None;
    process.pending_parent_status = None;
    process.termination = Some(TerminationReason::Signal(signal));
    process.pending_signals = 0;
    process.active_signal = None;
    if record_received {
        process.signal_received_count = process.signal_received_count.saturating_add(1);
    }
    manager.signaled = manager.signaled.saturating_add(1);
    true
}

fn stop_process_with_signal(process_id: u64, signal: u64, record_received: bool) -> bool {
    if !scheduler::stop_process(process_id) {
        return false;
    }
    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return false;
    };
    if !process.stop(signal, record_received) {
        return false;
    }
    manager.stop_deliveries = manager.stop_deliveries.saturating_add(1);
    true
}

fn continue_process_for_signal(process_id: u64, record_received: bool) -> bool {
    if !scheduler::continue_process(process_id) {
        return false;
    }
    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        return false;
    };
    if !process.continue_running(record_received) {
        return false;
    }
    manager.continue_deliveries = manager.continue_deliveries.saturating_add(1);
    true
}

#[derive(Clone, Copy)]
struct SignalDelivery {
    accepted: bool,
    stopped: bool,
}

fn deliver_signal_to_process(process_id: u64, signal: u64) -> SignalDelivery {
    let (state, action, masked) = {
        let manager = PROCESS_MANAGER.lock();
        let Some(process) = manager
            .processes
            .iter()
            .find(|process| process.process_id == process_id && process.is_live())
        else {
            return SignalDelivery {
                accepted: false,
                stopped: false,
            };
        };
        (
            process.state,
            process.signal_action(signal),
            process.signal_is_masked(signal),
        )
    };

    let continued = signal == SIGNAL_CONTINUE
        && state == ProcessState::Stopped
        && continue_process_for_signal(process_id, false);

    if signal == SIGNAL_STOP {
        let stopped = stop_process_with_signal(process_id, signal, true);
        return SignalDelivery {
            accepted: stopped,
            stopped,
        };
    }

    if action.handler == abi::signal_action::IGNORE {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            return SignalDelivery {
                accepted: false,
                stopped: false,
            };
        };
        process.signal_received_count = process.signal_received_count.saturating_add(1);
        process.signal_ignored_count = process.signal_ignored_count.saturating_add(1);
        process.clear_pending_signal(signal);
        manager.signal_ignores = manager.signal_ignores.saturating_add(1);
        return SignalDelivery {
            accepted: true,
            stopped: false,
        };
    }

    if action.handler != abi::signal_action::DEFAULT || masked {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            return SignalDelivery {
                accepted: false,
                stopped: false,
            };
        };
        process.signal_received_count = process.signal_received_count.saturating_add(1);
        process.queue_signal(signal);
        return SignalDelivery {
            accepted: true,
            stopped: false,
        };
    }

    match default_signal_action(signal) {
        Some(DefaultSignalAction::Terminate) => SignalDelivery {
            accepted: terminate_process_with_signal(process_id, signal, true),
            stopped: false,
        },
        Some(DefaultSignalAction::Stop) => {
            let stopped = stop_process_with_signal(process_id, signal, true);
            SignalDelivery {
                accepted: stopped,
                stopped,
            }
        }
        Some(DefaultSignalAction::Continue) => {
            if continued || state != ProcessState::Stopped {
                let mut manager = PROCESS_MANAGER.lock();
                if let Some(process) = manager.process_mut(process_id) {
                    process.signal_received_count = process.signal_received_count.saturating_add(1);
                }
            }
            SignalDelivery {
                accepted: continued || state != ProcessState::Stopped,
                stopped: false,
            }
        }
        None => SignalDelivery {
            accepted: false,
            stopped: false,
        },
    }
}

fn deliver_signal_group(
    owner_process_id: Option<u64>,
    process_group_id: u64,
    signal: u64,
) -> Result<usize, i64> {
    if !signal_is_supported(signal) {
        return Err(ERR_INVALID_ARGUMENT);
    }
    let target_process_ids = {
        let manager = PROCESS_MANAGER.lock();
        if let Some(owner_process_id) = owner_process_id {
            let owned = manager.processes.iter().any(|process| {
                process.parent_process_id == Some(owner_process_id)
                    && process.process_group_id == process_group_id
                    && process.is_live()
            });
            if !owned {
                return Err(ERR_NO_CHILD);
            }
        }
        manager
            .processes
            .iter()
            .filter(|process| process.process_group_id == process_group_id && process.is_live())
            .map(|process| process.process_id)
            .collect::<Vec<_>>()
    };
    if target_process_ids.is_empty() {
        return Err(ERR_NO_PROCESS);
    }

    let mut count = 0usize;
    let mut stopped = false;
    for target_process_id in target_process_ids {
        let delivery = deliver_signal_to_process(target_process_id, signal);
        count = count.saturating_add(usize::from(delivery.accepted));
        stopped |= delivery.stopped;
    }
    if count == 0 {
        return Err(ERR_NO_PROCESS);
    }

    let mut manager = PROCESS_MANAGER.lock();
    if let Some(owner_process_id) = owner_process_id
        && let Some(owner) = manager.process_mut(owner_process_id)
    {
        owner.signal_sent_count = owner.signal_sent_count.saturating_add(count as u64);
    }
    manager.signals_sent = manager.signals_sent.saturating_add(count as u64);
    drop(manager);

    if stopped {
        restore_group_terminal(process_group_id);
    }

    crate::serial_println!(
        "userspace process group signaled: group={}, signal={}, processes={}",
        process_group_id,
        signal,
        count
    );
    Ok(count)
}

fn restore_group_terminal(process_group_id: u64) {
    let Some(foreground_process_id) = terminal::foreground_process() else {
        return;
    };
    let terminal_parent = PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| {
            process.process_id == foreground_process_id
                && process.process_group_id == process_group_id
        })
        .and_then(|process| process.terminal_parent);
    let Some(terminal_parent) = terminal_parent else {
        return;
    };
    let _ = terminal::transfer(foreground_process_id, terminal_parent);
}

fn foreground_process_group(owner_process_id: u64, process_group_id: u64) -> Result<usize, i64> {
    if !terminal::is_foreground(owner_process_id) {
        return Err(ERR_IO);
    }
    let (foreground_process_id, active_count, stopped) = {
        let manager = PROCESS_MANAGER.lock();
        let members = manager
            .processes
            .iter()
            .filter(|process| {
                process.parent_process_id == Some(owner_process_id)
                    && process.process_group_id == process_group_id
                    && process.is_live()
            })
            .collect::<Vec<_>>();
        if members.is_empty() {
            return Err(ERR_NO_CHILD);
        }
        let foreground_process_id = members
            .iter()
            .find(|process| process.process_id == process_group_id)
            .or_else(|| members.first())
            .map(|process| process.process_id)
            .ok_or(ERR_NO_PROCESS)?;
        let stopped = members
            .iter()
            .any(|process| process.state == ProcessState::Stopped);
        (foreground_process_id, members.len(), stopped)
    };

    if !terminal::transfer(owner_process_id, foreground_process_id) {
        return Err(ERR_IO);
    }
    {
        let mut manager = PROCESS_MANAGER.lock();
        if let Some(process) = manager.process_mut(foreground_process_id) {
            process.terminal_parent = Some(owner_process_id);
        }
    }

    if stopped
        && let Err(error) =
            deliver_signal_group(Some(owner_process_id), process_group_id, SIGNAL_CONTINUE)
    {
        let _ = terminal::transfer(foreground_process_id, owner_process_id);
        return Err(error);
    }
    Ok(active_count)
}

fn service_terminal_control() -> usize {
    let Some(event) = terminal::take_control() else {
        return 0;
    };
    let target = PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == event.foreground_process)
        .map(|process| (process.process_group_id, process.path == "/ush"));
    let Some((process_group_id, is_shell)) = target else {
        return 0;
    };
    if is_shell {
        return 0;
    }
    let signal = match event.signal {
        terminal::ControlSignal::Interrupt => SIGNAL_INTERRUPT,
        terminal::ControlSignal::Suspend => SIGNAL_TERMINAL_STOP,
    };
    deliver_signal_group(None, process_group_id, signal).unwrap_or(0)
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
            && process.is_live()
            && foreground_process.is_none_or(|foreground| process.process_id == foreground)
    })?;
    let process_group_id = anchor.process_group_id;
    let info = child_group_info_locked(&manager, parent_process_id, process_group_id)?;
    (info.process_ids.len() >= minimum_members).then_some(info)
}

fn child_group_info(parent_process_id: u64, process_group_id: u64) -> Option<ProcessGroupInfo> {
    let manager = PROCESS_MANAGER.lock();
    child_group_info_locked(&manager, parent_process_id, process_group_id)
}

fn child_group_info_locked(
    manager: &ProcessManager,
    parent_process_id: u64,
    process_group_id: u64,
) -> Option<ProcessGroupInfo> {
    let members = manager
        .processes
        .iter()
        .filter(|process| {
            process.parent_process_id == Some(parent_process_id)
                && process.process_group_id == process_group_id
                && process.is_live()
        })
        .collect::<Vec<_>>();
    if members.is_empty() {
        return None;
    }
    Some(ProcessGroupInfo {
        process_group_id,
        process_ids: members.iter().map(|process| process.process_id).collect(),
        runnable: members
            .iter()
            .filter(|process| process.state == ProcessState::Runnable)
            .count(),
        blocked: members
            .iter()
            .filter(|process| process.state == ProcessState::Blocked)
            .count(),
        stopped: members
            .iter()
            .filter(|process| process.state == ProcessState::Stopped)
            .count(),
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
        TerminationReason::Fault(fault) => {
            abi::child_status::SIGNAL_BASE.saturating_add(fault.vector)
        }
        TerminationReason::Signal(signal) => abi::child_status::SIGNAL_BASE.saturating_add(*signal),
    }
}

fn stopped_child_status(signal: u64) -> u64 {
    abi::child_status::STOPPED_BASE.saturating_add(signal)
}

fn note_child_wait(manager: &mut ProcessManager, parent_process_id: u64) {
    if let Some(parent) = manager.process_mut(parent_process_id) {
        parent.child_wait_count = parent.child_wait_count.saturating_add(1);
    }
    manager.child_waits = manager.child_waits.saturating_add(1);
}

fn process_error_number(error: &Error) -> i64 {
    match error {
        Error::Vfs(vfs::Error::NotFound) | Error::Elf(elf::Error::Vfs(vfs::Error::NotFound)) => {
            ERR_NO_ENTRY
        }
        Error::InvalidArgument
        | Error::InvalidEnvironment
        | Error::TooManyArguments
        | Error::ArgumentBytesTooLarge => ERR_INVALID_ARGUMENT,
        Error::TooManyEnvironmentVariables | Error::EnvironmentBytesTooLarge => {
            ERR_ARGUMENT_TOO_LARGE
        }
        Error::ProcessLimitReached | Error::Scheduler(scheduler::InitError::TaskLimitReached) => {
            ERR_TRY_AGAIN
        }
        Error::JobLimitReached => ERR_NO_SPACE,
        Error::InvalidProcessGroup(_) => ERR_NO_PROCESS,
        Error::TerminalBusy => ERR_IO,
        _ => ERR_IO,
    }
}

enum WriteOutcome {
    Ready(u64),
    Blocked,
}

#[derive(Clone)]
enum WriteTarget {
    Console,
    Pipe(PipeId),
    File(OpenFileHandle),
    Invalid,
}
fn stream_write_target(target: &Option<StreamTarget>) -> WriteTarget {
    match target {
        Some(StreamTarget::Pipe(id)) => WriteTarget::Pipe(*id),
        Some(StreamTarget::File(h)) => WriteTarget::File(h.clone()),
        None => WriteTarget::Console,
    }
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
        .find(|p| p.process_id == process_id)
        .map(|p| {
            let readable = p
                .ranges
                .iter()
                .any(|r| r.readable && r.contains(address, length));
            let target = match file_descriptor {
                1 => stream_write_target(&p.stdout_target),
                2 => stream_write_target(&p.stderr_target),
                d if d >= 3 => {
                    if let Some(pd) = p.pipe_descriptors.iter().find(|x| x.descriptor == d) {
                        match pd.direction {
                            PipeDirection::Writer => WriteTarget::Pipe(pd.pipe_id),
                            PipeDirection::Reader => WriteTarget::Invalid,
                        }
                    } else {
                        p.open_files
                            .iter()
                            .find(|f| f.descriptor == d)
                            .map(|f| WriteTarget::File(f.handle.clone()))
                            .unwrap_or(WriteTarget::Invalid)
                    }
                }
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
    if let WriteTarget::Pipe(pipe_id) = &target {
        let pipe_id = *pipe_id;
        return match pipe::write(pipe_id, bytes) {
            Ok(pipe::WriteOutcome::Written(count)) => {
                let mut m = PROCESS_MANAGER.lock();
                if let Some(p) = m.process_mut(process_id) {
                    p.write_count = p.write_count.saturating_add(1);
                    p.bytes_written = p.bytes_written.saturating_add(count as u64);
                    p.pipe_write_count = p.pipe_write_count.saturating_add(1);
                    p.pipe_bytes_written = p.pipe_bytes_written.saturating_add(count as u64);
                }
                WriteOutcome::Ready(count as u64)
            }
            Ok(pipe::WriteOutcome::Full) => {
                let mut m = PROCESS_MANAGER.lock();
                let Some(p) = m.process_mut(process_id) else {
                    return WriteOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
                };
                p.pending_pipe_write = Some(PendingPipeWrite {
                    pipe_id,
                    address,
                    length,
                    stack_pointer: current_stack_pointer,
                });
                p.state = ProcessState::Blocked;
                p.blocked_pipe_write_count = p.blocked_pipe_write_count.saturating_add(1);
                drop(m);
                let _ = pipe::note_blocked_write(pipe_id);
                WriteOutcome::Blocked
            }
            Ok(pipe::WriteOutcome::NoReaders) => WriteOutcome::Ready(error_return(ERR_BROKEN_PIPE)),
            Err(_) => WriteOutcome::Ready(error_return(ERR_IO)),
        };
    }
    if let WriteTarget::File(handle) = &target {
        let handle = handle.clone();
        let backend = handle.lock().backend;
        if matches!(backend, OpenFileBackend::NullfsProxy { .. }) {
            return nullfs_proxy_write(process_id, handle, bytes, current_stack_pointer);
        }
        if matches!(backend, OpenFileBackend::TmpfsProxy { .. }) {
            return tmpfs_proxy_write(process_id, handle, bytes, current_stack_pointer);
        }
        let result = {
            let mut f = handle.lock();
            if !f.writable {
                Err(ERR_BAD_FILE_DESCRIPTOR)
            } else if f.append {
                match vfs::append(&f.path, bytes) {
                    Ok((offset, count)) => {
                        f.offset = offset.saturating_add(count as u64);
                        f.size = f.size.max(f.offset);
                        Ok(count)
                    }
                    Err(e) => Err(vfs_errno(&e)),
                }
            } else {
                match vfs::write_at(&f.path, f.offset, bytes) {
                    Ok(count) => {
                        f.offset = f.offset.saturating_add(count as u64);
                        f.size = f.size.max(f.offset);
                        Ok(count)
                    }
                    Err(e) => Err(vfs_errno(&e)),
                }
            }
        };
        return match result {
            Ok(count) => {
                let mut m = PROCESS_MANAGER.lock();
                if let Some(p) = m.process_mut(process_id) {
                    p.write_count = p.write_count.saturating_add(1);
                    p.bytes_written = p.bytes_written.saturating_add(count as u64);
                    p.file_write_count = p.file_write_count.saturating_add(1);
                    p.file_bytes_written = p.file_bytes_written.saturating_add(count as u64);
                }
                WriteOutcome::Ready(count as u64)
            }
            Err(e) => WriteOutcome::Ready(error_return(e)),
        };
    }
    if let Ok(text) = str::from_utf8(bytes) {
        crate::print!("{text}");
        crate::serial_print!("{text}");
    } else {
        for byte in bytes.iter().copied() {
            let c = match byte {
                b'\n' | b'\r' | b'\t' => char::from(byte),
                0x20..=0x7e => char::from(byte),
                _ => '.',
            };
            crate::print!("{c}");
            crate::serial_print!("{c}");
        }
    }
    let mut m = PROCESS_MANAGER.lock();
    if let Some(p) = m.process_mut(process_id) {
        p.write_count = p.write_count.saturating_add(1);
        p.bytes_written = p.bytes_written.saturating_add(length as u64);
    }
    WriteOutcome::Ready(length as u64)
}

fn user_range_allows(process_id: u64, address: u64, length: usize, require_write: bool) -> bool {
    PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == process_id)
        .is_some_and(|process| process_user_range_allows(process, address, length, require_write))
}

fn process_user_range_allows(
    process: &Process,
    address: u64,
    length: usize,
    require_write: bool,
) -> bool {
    process.ranges.iter().any(|range| {
        range.readable && (!require_write || range.writable) && range.contains(address, length)
    })
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

fn user_text_allow_empty(
    process_id: u64,
    address: u64,
    length: u64,
    maximum: usize,
) -> Result<String, i64> {
    let length = usize::try_from(length).map_err(|_| ERR_ARGUMENT_TOO_LARGE)?;
    if length > maximum {
        return Err(ERR_ARGUMENT_TOO_LARGE);
    }
    if length == 0 {
        return Ok(String::new());
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

enum TmpfsProxyPath {
    Directory,
    File(String),
}

fn tmpfs_proxy_path(path: &str) -> Result<Option<TmpfsProxyPath>, i64> {
    let path = vfs::normalize_path(path).map_err(|error| vfs_errno(&error))?;
    if path == vfs::TMPFS_MOUNT_PATH {
        return Ok(Some(TmpfsProxyPath::Directory));
    }
    let Some(name) = path.strip_prefix("/tmp/") else {
        return Ok(None);
    };
    if name.is_empty() || name.contains('/') {
        return Err(ERR_INVALID_ARGUMENT);
    }
    if name.len() > tmpfs_protocol::MAX_NAME_BYTES {
        return Err(abi::errno::NAME_TOO_LONG);
    }
    Ok(Some(TmpfsProxyPath::File(String::from(name))))
}

fn tmpfs_proxy_state() -> Option<TmpfsProxyState> {
    let state = *TMPFS_PROXY.lock();
    if state.request_endpoint.is_some() && state.generation != 0 {
        Some(state)
    } else {
        None
    }
}

fn tmpfs_proxy_backend_is_current(
    generation: u32,
    session_id: u64,
    session_generation: u64,
) -> bool {
    tmpfs_proxy_state().is_some_and(|state| {
        state.generation == generation
            && state.session_id == session_id
            && state.session_generation == session_generation
    })
}

fn tmpfs_proxy_begin_connect(
    request_endpoint: CapabilityObjectRef,
    legacy_generation: u32,
) -> Result<(), i64> {
    {
        let state = TMPFS_PROXY.lock();
        if state.request_endpoint.is_some()
            || legacy_generation <= state.generation
            || request_endpoint.id <= state.retired_request_endpoint_id
        {
            return Err(ERR_INVALID_ARGUMENT);
        }
    }
    let reply_endpoint = tmpfs_proxy_create_reply_endpoint()?;
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::CONNECT;
    request.request_id = 1;
    let bytes = tmpfs_proxy_value_bytes(&request).to_vec();
    let push_result = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        match registry.object_index(request_endpoint) {
            Some(object_index) => match &mut registry.objects[object_index].data {
                CapabilityObjectData::Endpoint(endpoint)
                    if endpoint.queue.len() < abi::limits::MAX_ENDPOINT_MESSAGES =>
                {
                    endpoint.queue.push_back(EndpointMessage {
                        sender_process_id: 0,
                        bytes,
                        capabilities: vec![TransferredCapability {
                            object: reply_endpoint,
                            rights: abi::capability::RIGHT_SEND,
                        }],
                    });
                    Ok(())
                }
                CapabilityObjectData::Endpoint(_) => Err(ERR_TRY_AGAIN),
                CapabilityObjectData::Notification(_)
                | CapabilityObjectData::SharedMemory(_)
                | CapabilityObjectData::KernelEarlyLogReader(_)
                | CapabilityObjectData::Job(_) => Err(ERR_IO),
            },
            None => Err(ERR_IO),
        }
    };
    if let Err(error) = push_result {
        tmpfs_proxy_release_reply_endpoint(reply_endpoint);
        return Err(error);
    }

    let previous = {
        let mut state = TMPFS_PROXY.lock();
        let previous = (
            state.request_endpoint,
            state.connect_reply_endpoint,
            state.session_reply_endpoint,
            state.bulk_buffer,
        );
        state.request_endpoint = Some(request_endpoint);
        state.generation = legacy_generation;
        state.connect_reply_endpoint = Some(reply_endpoint);
        state.session_reply_endpoint = None;
        state.session_id = filesystem_protocol::INVALID_ID;
        state.session_generation = 0;
        state.session_features = 0;
        state.bulk_buffer = None;
        state.bulk_buffer_attached = false;
        state.active_request_id = filesystem_protocol::INVALID_ID;
        state.active_close = None;
        previous
    };
    let abandoned = TMPFS_ABANDONED_REQUEST.lock().take();
    drop(abandoned);
    if previous.0 != Some(request_endpoint) {
        if let Some(endpoint) = previous.0 {
            kernel_capability_root_remove(endpoint);
        }
        kernel_capability_root_add(request_endpoint);
    }
    if let Some(endpoint) = previous.1 {
        tmpfs_proxy_release_reply_endpoint(endpoint);
    }
    if let Some(endpoint) = previous.2 {
        tmpfs_proxy_release_reply_endpoint(endpoint);
    }
    if let Some(buffer) = previous.3 {
        kernel_capability_root_remove(buffer);
    }
    wake_endpoint_waiter(request_endpoint);
    Ok(())
}

fn tmpfs_proxy_offline(generation: u32) -> Result<(), i64> {
    let (request_endpoint, connect_reply_endpoint, session_reply_endpoint, bulk_buffer) = {
        let mut state = TMPFS_PROXY.lock();
        if state.generation != generation {
            return Err(ERR_INVALID_ARGUMENT);
        }
        let Some(request_endpoint) = state.request_endpoint.take() else {
            return Ok(());
        };
        state.retired_request_endpoint_id = request_endpoint.id;
        let resources = (
            request_endpoint,
            state.connect_reply_endpoint.take(),
            state.session_reply_endpoint.take(),
            state.bulk_buffer.take(),
        );
        state.session_id = filesystem_protocol::INVALID_ID;
        state.session_generation = 0;
        state.session_features = 0;
        state.bulk_buffer_attached = false;
        state.active_request_id = filesystem_protocol::INVALID_ID;
        state.active_close = None;
        resources
    };
    drop(TMPFS_ABANDONED_REQUEST.lock().take());
    TMPFS_CLOSE_QUEUE
        .lock()
        .retain(|ticket| ticket.generation != generation);
    kernel_capability_root_remove(request_endpoint);
    if let Some(endpoint) = connect_reply_endpoint {
        tmpfs_proxy_release_reply_endpoint(endpoint);
    }
    if let Some(endpoint) = session_reply_endpoint
        && Some(endpoint) != connect_reply_endpoint
    {
        tmpfs_proxy_release_reply_endpoint(endpoint);
    }
    if let Some(buffer) = bulk_buffer {
        kernel_capability_root_remove(buffer);
    }
    CAPABILITY_REGISTRY.lock().collect_garbage();
    tmpfs_proxy_cancel_generation(generation);
    Ok(())
}

fn tmpfs_proxy_cancel_generation(generation: u32) {
    let pending: Vec<(u64, PendingTmpfsProxyRequest)> = {
        let mut manager = PROCESS_MANAGER.lock();
        manager
            .processes
            .iter_mut()
            .filter_map(|process| {
                let request = process.pending_tmpfs_proxy.as_ref()?;
                if request.request_generation != generation {
                    return None;
                }
                process
                    .pending_tmpfs_proxy
                    .take()
                    .map(|pending| (process.process_id, pending))
            })
            .collect()
    };
    for (process_id, pending) in pending {
        if scheduler::with_process_address_space(process_id, || {
            let registers = unsafe { &mut *(pending.stack_pointer as *mut SavedRegisters) };
            registers.rax = error_return(ERR_IO);
        })
        .is_none()
        {
            continue;
        }
        let made_runnable = {
            let mut manager = PROCESS_MANAGER.lock();
            let Some(process) = manager.process_mut(process_id) else {
                continue;
            };
            process.make_runnable();
            true
        };
        if made_runnable {
            let _ = scheduler::wake_process(process_id);
        }
    }
}

fn tmpfs_proxy_service_connect() -> bool {
    let state = *TMPFS_PROXY.lock();
    if state.connect_reply_endpoint.is_none()
        && state.bulk_buffer.is_some()
        && !state.bulk_buffer_attached
    {
        return tmpfs_proxy_service_attach(state);
    };
    let Some(reply_endpoint) = state.connect_reply_endpoint else {
        return false;
    };
    let message = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        let Some(index) = registry.object_index(reply_endpoint) else {
            return false;
        };
        let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[index].data else {
            return false;
        };
        endpoint.queue.pop_front()
    };
    let Some(message) = message else {
        return false;
    };
    let valid = if message.capabilities.is_empty()
        && message.bytes.len() == size_of::<filesystem_protocol::Reply>()
    {
        let reply = unsafe {
            ptr::read_unaligned(message.bytes.as_ptr() as *const filesystem_protocol::Reply)
        };
        reply.version == filesystem_protocol::VERSION
            && reply.operation == filesystem_protocol::operation::CONNECT
            && reply.status == filesystem_protocol::status::OK
            && reply.flags == 0
            && reply.request_id == 1
            && reply.session_id != filesystem_protocol::INVALID_ID
            && reply.generation == u64::from(state.generation)
            && reply.node_id == filesystem_protocol::ROOT_NODE_ID
            && reply.node_kind == filesystem_protocol::node_kind::DIRECTORY
            && reply.value == 0
            && reply.data_length == 0
            && reply.reserved == [0; 2]
            && {
                let mut proxy = TMPFS_PROXY.lock();
                if proxy.connect_reply_endpoint == Some(reply_endpoint) {
                    proxy.connect_reply_endpoint = None;
                    proxy.session_reply_endpoint = Some(reply_endpoint);
                    proxy.session_id = reply.session_id;
                    proxy.session_generation = reply.generation;
                    proxy.session_features = reply.value;
                    true
                } else {
                    false
                }
            }
    } else {
        false
    };
    if !valid {
        let mut proxy = TMPFS_PROXY.lock();
        if proxy.connect_reply_endpoint == Some(reply_endpoint) {
            proxy.connect_reply_endpoint = None;
            proxy.session_id = filesystem_protocol::INVALID_ID;
            proxy.session_generation = 0;
            proxy.session_features = 0;
        }
    }
    if !valid {
        tmpfs_proxy_release_reply_endpoint(reply_endpoint);
    } else if tmpfs_proxy_begin_attach().is_err() {
        let mut proxy = TMPFS_PROXY.lock();
        proxy.session_features = 0;
        proxy.bulk_buffer = None;
        proxy.bulk_buffer_attached = false;
    }
    true
}

fn tmpfs_proxy_begin_attach() -> Result<(), i64> {
    const BULK_BUFFER_ID: u64 = 1;
    const BULK_BUFFER_BYTES: usize = 4096;
    let state = *TMPFS_PROXY.lock();
    let request_endpoint = state.request_endpoint.ok_or(ERR_IO)?;
    if state.session_id == filesystem_protocol::INVALID_ID || state.session_generation == 0 {
        return Err(ERR_IO);
    }
    let buffer = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        registry.collect_garbage();
        if registry
            .shared_memory_bytes()
            .saturating_add(BULK_BUFFER_BYTES)
            > abi::limits::MAX_SHARED_MEMORY_TOTAL_BYTES
        {
            return Err(ERR_NO_SPACE);
        }
        registry.create_object(
            abi::capability::KIND_SHARED_MEMORY,
            CapabilityObjectData::SharedMemory(SharedMemoryObject {
                bytes: vec![0_u8; BULK_BUFFER_BYTES],
            }),
        )?
    };
    kernel_capability_root_add(buffer);
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::ATTACH_BUFFER;
    request.request_id = 2;
    request.session_id = state.session_id;
    request.generation = state.session_generation;
    request.bulk = filesystem_protocol::BulkBuffer {
        buffer_id: BULK_BUFFER_ID,
        offset: 0,
        length: BULK_BUFFER_BYTES as u64,
    };
    let bytes = tmpfs_proxy_value_bytes(&request).to_vec();
    let pushed = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        match registry.object_index(request_endpoint) {
            Some(index) => match &mut registry.objects[index].data {
                CapabilityObjectData::Endpoint(endpoint)
                    if endpoint.queue.len() < abi::limits::MAX_ENDPOINT_MESSAGES =>
                {
                    endpoint.queue.push_back(EndpointMessage {
                        sender_process_id: 0,
                        bytes,
                        capabilities: vec![TransferredCapability {
                            object: buffer,
                            rights: abi::capability::RIGHT_READ | abi::capability::RIGHT_WRITE,
                        }],
                    });
                    Ok(())
                }
                CapabilityObjectData::Endpoint(_) => Err(ERR_TRY_AGAIN),
                CapabilityObjectData::Notification(_)
                | CapabilityObjectData::SharedMemory(_)
                | CapabilityObjectData::KernelEarlyLogReader(_)
                | CapabilityObjectData::Job(_) => Err(ERR_IO),
            },
            None => Err(ERR_IO),
        }
    };
    if let Err(error) = pushed {
        kernel_capability_root_remove(buffer);
        CAPABILITY_REGISTRY.lock().collect_garbage();
        return Err(error);
    }
    let mut proxy = TMPFS_PROXY.lock();
    if proxy.session_id != state.session_id
        || proxy.session_generation != state.session_generation
        || proxy.request_endpoint != Some(request_endpoint)
    {
        drop(proxy);
        kernel_capability_root_remove(buffer);
        return Err(ERR_IO);
    }
    proxy.bulk_buffer = Some(buffer);
    proxy.bulk_buffer_attached = false;
    drop(proxy);
    wake_endpoint_waiter(request_endpoint);
    Ok(())
}

fn tmpfs_proxy_service_attach(state: TmpfsProxyState) -> bool {
    let Some(reply_endpoint) = state.session_reply_endpoint else {
        return false;
    };
    let message = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        let Some(index) = registry.object_index(reply_endpoint) else {
            return false;
        };
        let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[index].data else {
            return false;
        };
        endpoint.queue.pop_front()
    };
    let Some(message) = message else {
        return false;
    };
    let valid = if message.capabilities.is_empty()
        && message.bytes.len() == size_of::<filesystem_protocol::Reply>()
    {
        let reply = unsafe {
            ptr::read_unaligned(message.bytes.as_ptr() as *const filesystem_protocol::Reply)
        };
        reply.version == filesystem_protocol::VERSION
            && reply.operation == filesystem_protocol::operation::ATTACH_BUFFER
            && reply.status == filesystem_protocol::status::OK
            && reply.flags == 0
            && reply.request_id == 2
            && reply.session_id == state.session_id
            && reply.generation == state.session_generation
            && reply.data_length == 0
            && reply.reserved == [0; 2]
    } else {
        false
    };
    let mut proxy = TMPFS_PROXY.lock();
    if proxy.session_id == state.session_id
        && proxy.session_generation == state.session_generation
        && proxy.bulk_buffer == state.bulk_buffer
    {
        proxy.bulk_buffer_attached = valid;
        if !valid {
            let buffer = proxy.bulk_buffer.take();
            drop(proxy);
            if let Some(buffer) = buffer {
                kernel_capability_root_remove(buffer);
            }
            return true;
        }
    }
    true
}

fn tmpfs_proxy_create_reply_endpoint() -> Result<CapabilityObjectRef, i64> {
    let reply_endpoint = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        registry.create_object(
            abi::capability::KIND_ENDPOINT,
            CapabilityObjectData::Endpoint(EndpointObject {
                queue: alloc::collections::VecDeque::with_capacity(
                    abi::limits::MAX_ENDPOINT_MESSAGES,
                ),
                peer: EndpointPeer::Loopback,
            }),
        )?
    };
    kernel_capability_root_add(reply_endpoint);
    Ok(reply_endpoint)
}

fn tmpfs_proxy_release_reply_endpoint(reply_endpoint: CapabilityObjectRef) {
    kernel_capability_root_remove(reply_endpoint);
    CAPABILITY_REGISTRY.lock().collect_garbage();
}

fn tmpfs_proxy_take_filesystem_reply(
    pending: &PendingTmpfsProxyRequest,
) -> Option<Result<tmpfs_protocol::Reply, i64>> {
    let message = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        let Some(index) = registry.object_index(pending.reply_endpoint) else {
            return Some(Err(ERR_IO));
        };
        let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[index].data else {
            return Some(Err(ERR_IO));
        };
        endpoint.queue.pop_front()
    }?;
    Some(tmpfs_proxy_decode_filesystem_reply(message, pending))
}

fn tmpfs_proxy_decode_filesystem_reply(
    message: EndpointMessage,
    pending: &PendingTmpfsProxyRequest,
) -> Result<tmpfs_protocol::Reply, i64> {
    if !message.capabilities.is_empty()
        || message.bytes.len() != size_of::<filesystem_protocol::Reply>()
    {
        return Err(ERR_IO);
    }
    let reply =
        unsafe { ptr::read_unaligned(message.bytes.as_ptr() as *const filesystem_protocol::Reply) };
    let state = *TMPFS_PROXY.lock();
    if reply.version != filesystem_protocol::VERSION
        || reply.operation != pending.request_operation
        || reply.request_id != pending.generic_request_id
        || reply.session_id != state.session_id
        || reply.generation != state.session_generation
        || reply.flags
            != if pending.request_operation == filesystem_protocol::operation::READ_DIRECTORY {
                filesystem_protocol::reply_flags::END_OF_DIRECTORY
            } else {
                0
            }
        || reply.reserved != [0; 2]
        || reply.status == filesystem_protocol::status::OK
            && matches!(
                pending.request_operation,
                filesystem_protocol::operation::OPEN | filesystem_protocol::operation::LOOKUP
            )
            && (reply.node_id == filesystem_protocol::INVALID_ID
                || reply.node_kind != filesystem_protocol::node_kind::FILE)
    {
        return Err(ERR_IO);
    }
    let write_resulting_offset = if reply.status == filesystem_protocol::status::OK
        && pending.request_operation == filesystem_protocol::operation::WRITE
    {
        Some(filesystem_protocol::decode_write_reply_offset(&reply).ok_or(ERR_IO)?)
    } else {
        if reply.data_length != 0 || reply.data != [0; filesystem_protocol::MAX_INLINE_DATA_BYTES] {
            return Err(ERR_IO);
        }
        None
    };
    let value = u32::try_from(reply.value).map_err(|_| abi::errno::RANGE)?;
    let mut compatibility = tmpfs_protocol::Reply::EMPTY;
    compatibility.operation = match pending.operation {
        PendingTmpfsProxyOperation::Open { .. } => tmpfs_protocol::operation::OPEN,
        PendingTmpfsProxyOperation::Read { .. } => tmpfs_protocol::operation::READ,
        PendingTmpfsProxyOperation::Write { .. } => tmpfs_protocol::operation::WRITE,
        PendingTmpfsProxyOperation::Stat { .. } => tmpfs_protocol::operation::STAT,
        PendingTmpfsProxyOperation::ReadDirectory { .. } => tmpfs_protocol::operation::LIST,
        PendingTmpfsProxyOperation::Unlink => tmpfs_protocol::operation::REMOVE,
    };
    compatibility.generation = pending.request_generation;
    compatibility.status = match reply.status {
        filesystem_protocol::status::OK => tmpfs_protocol::status::OK,
        filesystem_protocol::status::INVALID
        | filesystem_protocol::status::NOT_DIRECTORY
        | filesystem_protocol::status::IS_DIRECTORY
        | filesystem_protocol::status::EXISTS
        | filesystem_protocol::status::PERMISSION => tmpfs_protocol::status::INVALID,
        filesystem_protocol::status::NOT_FOUND | filesystem_protocol::status::STALE_NODE => {
            tmpfs_protocol::status::NOT_FOUND
        }
        filesystem_protocol::status::NO_SPACE => tmpfs_protocol::status::NO_SPACE,
        filesystem_protocol::status::RANGE => tmpfs_protocol::status::RANGE,
        filesystem_protocol::status::STALE_SESSION => tmpfs_protocol::status::STALE_MOUNT,
        _ => return Err(ERR_IO),
    };
    compatibility.value = value;
    match &pending.operation {
        PendingTmpfsProxyOperation::Open { .. } => {
            compatibility.data[..size_of::<u64>()].copy_from_slice(&reply.node_id.to_ne_bytes());
            compatibility.data_length = size_of::<u64>() as u16;
        }
        PendingTmpfsProxyOperation::Read { length, .. } => {
            let count = usize::try_from(reply.value).map_err(|_| abi::errno::RANGE)?;
            if count > *length || count > compatibility.data.len() {
                return Err(ERR_IO);
            }
            tmpfs_proxy_bulk_read(&mut compatibility.data[..count])?;
            compatibility.data_length = count as u16;
        }
        PendingTmpfsProxyOperation::Write {
            offset,
            append,
            length,
            ..
        } => {
            if value as usize > *length {
                return Err(ERR_IO);
            }
            if reply.status == filesystem_protocol::status::OK {
                let Some(resulting_offset) = write_resulting_offset else {
                    return Err(ERR_IO);
                };
                let valid = if *append {
                    resulting_offset.checked_sub(u64::from(value)).is_some()
                } else {
                    offset.checked_add(u64::from(value)) == Some(resulting_offset)
                };
                if !valid {
                    return Err(ERR_IO);
                }
                compatibility.data[..size_of::<u64>()]
                    .copy_from_slice(&resulting_offset.to_le_bytes());
                compatibility.data_length = size_of::<u64>() as u16;
            }
        }
        PendingTmpfsProxyOperation::ReadDirectory { .. } => {
            tmpfs_proxy_translate_directory_entries(
                usize::try_from(reply.value).map_err(|_| abi::errno::RANGE)?,
                &mut compatibility,
            )?;
        }
        PendingTmpfsProxyOperation::Unlink => {}
        _ => {}
    }
    Ok(compatibility)
}

fn tmpfs_proxy_translate_directory_entries(
    count: usize,
    compatibility: &mut tmpfs_protocol::Reply,
) -> Result<(), i64> {
    let entry_size = size_of::<filesystem_protocol::DirectoryEntry>();
    let maximum = 4096 / entry_size;
    if count > maximum {
        return Err(ERR_IO);
    }
    let mut entry_bytes = [0_u8; size_of::<filesystem_protocol::DirectoryEntry>()];
    let mut output = 0usize;
    let mut previous_cookie = 0u64;
    for index in 0..count {
        tmpfs_proxy_bulk_read_at(index * entry_size, &mut entry_bytes)?;
        let entry = unsafe {
            ptr::read_unaligned(entry_bytes.as_ptr() as *const filesystem_protocol::DirectoryEntry)
        };
        let name_length = usize::from(entry.name_length);
        if entry.node_id == filesystem_protocol::INVALID_ID
            || entry.kind != filesystem_protocol::node_kind::FILE
            || entry.reserved != 0
            || entry.next_cookie != entry.node_id
            || entry.next_cookie <= previous_cookie
            || name_length == 0
            || name_length > entry.name.len()
            || entry.name[..name_length].contains(&b'/')
            || entry.name[..name_length].contains(&0)
        {
            return Err(ERR_IO);
        }
        let separator = usize::from(output != 0);
        let Some(end) = output
            .checked_add(separator)
            .and_then(|cursor| cursor.checked_add(name_length))
        else {
            return Err(abi::errno::RANGE);
        };
        if end > compatibility.data.len() {
            break;
        }
        if separator != 0 {
            compatibility.data[output] = b'\n';
            output += 1;
        }
        compatibility.data[output..end].copy_from_slice(&entry.name[..name_length]);
        output = end;
        previous_cookie = entry.next_cookie;
    }
    compatibility.data_length = output as u16;
    compatibility.value = u32::try_from(count).map_err(|_| abi::errno::RANGE)?;
    Ok(())
}

fn tmpfs_proxy_bulk_read(destination: &mut [u8]) -> Result<(), i64> {
    tmpfs_proxy_bulk_read_at(0, destination)
}

fn tmpfs_proxy_bulk_read_at(offset: usize, destination: &mut [u8]) -> Result<(), i64> {
    let buffer = TMPFS_PROXY.lock().bulk_buffer.ok_or(ERR_IO)?;
    let registry = CAPABILITY_REGISTRY.lock();
    let index = registry.object_index(buffer).ok_or(ERR_IO)?;
    let CapabilityObjectData::SharedMemory(memory) = &registry.objects[index].data else {
        return Err(ERR_IO);
    };
    let end = offset
        .checked_add(destination.len())
        .ok_or(abi::errno::RANGE)?;
    let source = memory.bytes.get(offset..end).ok_or(abi::errno::RANGE)?;
    destination.copy_from_slice(source);
    Ok(())
}

fn tmpfs_proxy_bulk_write(source: &[u8]) -> Result<(), i64> {
    let buffer = TMPFS_PROXY.lock().bulk_buffer.ok_or(ERR_IO)?;
    let mut registry = CAPABILITY_REGISTRY.lock();
    let index = registry.object_index(buffer).ok_or(ERR_IO)?;
    let CapabilityObjectData::SharedMemory(memory) = &mut registry.objects[index].data else {
        return Err(ERR_IO);
    };
    let destination = memory
        .bytes
        .get_mut(..source.len())
        .ok_or(abi::errno::RANGE)?;
    destination.copy_from_slice(source);
    Ok(())
}

fn tmpfs_proxy_release_pending(pending: &PendingTmpfsProxyRequest) {
    let mut state = TMPFS_PROXY.lock();
    if state.active_request_id == pending.generic_request_id {
        state.active_request_id = filesystem_protocol::INVALID_ID;
    }
}

fn tmpfs_proxy_abandon_pending(pending: PendingTmpfsProxyRequest) {
    if TMPFS_PROXY.lock().active_request_id != pending.generic_request_id {
        return;
    }
    let mut abandoned = TMPFS_ABANDONED_REQUEST.lock();
    assert!(abandoned.is_none(), "multiple abandoned tmpfs requests");
    *abandoned = Some(pending);
}

fn tmpfs_proxy_service_abandoned() -> bool {
    let pending = TMPFS_ABANDONED_REQUEST.lock().clone();
    let Some(pending) = pending else {
        return false;
    };
    let Some(reply) = tmpfs_proxy_take_filesystem_reply(&pending) else {
        return false;
    };
    tmpfs_proxy_release_pending(&pending);
    let abandoned = {
        let mut slot = TMPFS_ABANDONED_REQUEST.lock();
        if slot
            .as_ref()
            .is_some_and(|current| current.generic_request_id == pending.generic_request_id)
        {
            slot.take()
        } else {
            None
        }
    };
    let Some(abandoned) = abandoned else {
        return true;
    };
    if let (
        PendingTmpfsProxyOperation::Open {
            generation,
            session_id,
            session_generation,
            ..
        },
        Ok(reply),
    ) = (&abandoned.operation, reply)
        && reply.status == tmpfs_protocol::status::OK
        && usize::from(reply.data_length) == size_of::<u64>()
    {
        let mut node_bytes = [0_u8; size_of::<u64>()];
        node_bytes.copy_from_slice(&reply.data[..size_of::<u64>()]);
        let node_id = u64::from_ne_bytes(node_bytes);
        if node_id != filesystem_protocol::INVALID_ID {
            tmpfs_proxy_enqueue_close(*generation, *session_id, *session_generation, node_id);
        }
    }
    drop(abandoned);
    true
}

fn tmpfs_proxy_enqueue_close(
    generation: u32,
    session_id: u64,
    session_generation: u64,
    node_id: u64,
) {
    debug_assert_ne!(generation, 0);
    debug_assert_ne!(session_id, filesystem_protocol::INVALID_ID);
    debug_assert_ne!(session_generation, 0);
    debug_assert_ne!(node_id, filesystem_protocol::INVALID_ID);
    let state = *TMPFS_PROXY.lock();
    if state.request_endpoint.is_none()
        || state.generation != generation
        || state.session_id != session_id
        || state.session_generation != session_generation
    {
        return;
    }
    let mut queue = TMPFS_CLOSE_QUEUE.lock();
    assert!(
        queue.len() < MAX_PENDING_TMPFS_CLOSES,
        "bounded tmpfs close queue exhausted"
    );
    queue.push_back(PendingTmpfsClose {
        generation,
        session_id,
        session_generation,
        node_id,
    });
}

fn tmpfs_proxy_service_close() -> bool {
    let active = TMPFS_PROXY.lock().active_close;
    if let Some(active) = active {
        let message = {
            let mut registry = CAPABILITY_REGISTRY.lock();
            let Some(index) = registry.object_index(active.reply_endpoint) else {
                return false;
            };
            let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[index].data else {
                return false;
            };
            endpoint.queue.pop_front()
        };
        let Some(message) = message else {
            return false;
        };
        match tmpfs_proxy_validate_close_reply(message, active) {
            Some(filesystem_protocol::status::OK)
            | Some(filesystem_protocol::status::STALE_NODE)
            | Some(filesystem_protocol::status::STALE_SESSION) => {
                tmpfs_proxy_finish_close(active);
            }
            Some(filesystem_protocol::status::TRY_AGAIN) => {
                tmpfs_proxy_finish_close(active);
                TMPFS_CLOSE_QUEUE.lock().push_front(active.ticket);
            }
            _ => return true,
        }
    }

    loop {
        let Some(ticket) = TMPFS_CLOSE_QUEUE.lock().pop_front() else {
            return active.is_some();
        };
        let start = {
            let mut state = TMPFS_PROXY.lock();
            if ticket.generation != state.generation
                || ticket.session_id != state.session_id
                || ticket.session_generation != state.session_generation
            {
                None
            } else if !state.bulk_buffer_attached
                || state.active_request_id != filesystem_protocol::INVALID_ID
            {
                drop(state);
                TMPFS_CLOSE_QUEUE.lock().push_front(ticket);
                return active.is_some();
            } else {
                let request_endpoint = match state.request_endpoint {
                    Some(endpoint) => endpoint,
                    None => {
                        drop(state);
                        TMPFS_CLOSE_QUEUE.lock().push_front(ticket);
                        return active.is_some();
                    }
                };
                let reply_endpoint = match state.session_reply_endpoint {
                    Some(endpoint) => endpoint,
                    None => {
                        drop(state);
                        TMPFS_CLOSE_QUEUE.lock().push_front(ticket);
                        return active.is_some();
                    }
                };
                if state.session_id == filesystem_protocol::INVALID_ID
                    || state.session_generation == 0
                {
                    drop(state);
                    TMPFS_CLOSE_QUEUE.lock().push_front(ticket);
                    return active.is_some();
                }
                let request_id = state.next_request_id;
                let Some(next_request_id) = state
                    .next_request_id
                    .checked_add(1)
                    .filter(|id| *id != filesystem_protocol::INVALID_ID)
                else {
                    drop(state);
                    TMPFS_CLOSE_QUEUE.lock().push_front(ticket);
                    return active.is_some();
                };
                state.next_request_id = next_request_id;
                state.active_request_id = request_id;
                let close = ActiveTmpfsClose {
                    ticket,
                    request_id,
                    reply_endpoint,
                    session_id: ticket.session_id,
                    session_generation: ticket.session_generation,
                };
                state.active_close = Some(close);
                Some((request_endpoint, close))
            }
        };
        let Some((request_endpoint, close)) = start else {
            continue;
        };
        let mut request = filesystem_protocol::Request::EMPTY;
        request.operation = filesystem_protocol::operation::CLOSE_NODE;
        request.request_id = close.request_id;
        request.session_id = close.session_id;
        request.generation = close.session_generation;
        request.node_id = close.ticket.node_id;
        let bytes = tmpfs_proxy_value_bytes(&request).to_vec();
        let pushed = {
            let mut registry = CAPABILITY_REGISTRY.lock();
            match registry.object_index(request_endpoint) {
                Some(index) => match &mut registry.objects[index].data {
                    CapabilityObjectData::Endpoint(endpoint)
                        if endpoint.queue.len() < abi::limits::MAX_ENDPOINT_MESSAGES =>
                    {
                        endpoint.queue.push_back(EndpointMessage {
                            sender_process_id: 0,
                            bytes,
                            capabilities: Vec::new(),
                        });
                        true
                    }
                    _ => false,
                },
                None => false,
            }
        };
        if !pushed {
            let mut state = TMPFS_PROXY.lock();
            if state.active_close == Some(close) {
                state.active_close = None;
                state.active_request_id = filesystem_protocol::INVALID_ID;
            }
            drop(state);
            TMPFS_CLOSE_QUEUE.lock().push_front(ticket);
            return active.is_some();
        }
        wake_endpoint_waiter(request_endpoint);
        return true;
    }
}

fn tmpfs_proxy_validate_close_reply(
    message: EndpointMessage,
    active: ActiveTmpfsClose,
) -> Option<i32> {
    if !message.capabilities.is_empty()
        || message.bytes.len() != size_of::<filesystem_protocol::Reply>()
    {
        return None;
    }
    let reply =
        unsafe { ptr::read_unaligned(message.bytes.as_ptr() as *const filesystem_protocol::Reply) };
    (reply.version == filesystem_protocol::VERSION
        && reply.operation == filesystem_protocol::operation::CLOSE_NODE
        && reply.flags == 0
        && reply.request_id == active.request_id
        && reply.session_id == active.session_id
        && reply.generation == active.session_generation
        && reply.node_id == filesystem_protocol::INVALID_ID
        && reply.value == 0
        && reply.data_length == 0
        && reply.node_kind == filesystem_protocol::node_kind::UNKNOWN
        && reply.reserved == [0; 2]
        && reply.data == [0; filesystem_protocol::MAX_INLINE_DATA_BYTES])
        .then_some(reply.status)
}

fn tmpfs_proxy_finish_close(active: ActiveTmpfsClose) -> bool {
    let mut state = TMPFS_PROXY.lock();
    if state.active_close == Some(active) {
        state.active_close = None;
        state.active_request_id = filesystem_protocol::INVALID_ID;
    }
    true
}

fn tmpfs_proxy_value_bytes<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

fn tmpfs_proxy_begin_filesystem_request(
    process_id: u64,
    mut request: filesystem_protocol::Request,
    mut operation: PendingTmpfsProxyOperation,
    stack_pointer: usize,
) -> Result<(), i64> {
    let (request_endpoint, reply_endpoint, legacy_generation, request_id) = {
        let mut state = TMPFS_PROXY.lock();
        let request_endpoint = state.request_endpoint.ok_or(ERR_IO)?;
        if !state.bulk_buffer_attached || state.active_request_id != filesystem_protocol::INVALID_ID
        {
            return Err(ERR_TRY_AGAIN);
        }
        let reply_endpoint = state.session_reply_endpoint.ok_or(ERR_IO)?;
        if state.session_id == filesystem_protocol::INVALID_ID || state.session_generation == 0 {
            return Err(ERR_IO);
        }
        let request_id = state.next_request_id;
        state.next_request_id = state
            .next_request_id
            .checked_add(1)
            .filter(|id| *id != filesystem_protocol::INVALID_ID)
            .ok_or(ERR_IO)?;
        state.active_request_id = request_id;
        request.request_id = request_id;
        request.session_id = state.session_id;
        request.generation = state.session_generation;
        if let PendingTmpfsProxyOperation::Open {
            session_id,
            session_generation,
            ..
        } = &mut operation
        {
            *session_id = state.session_id;
            *session_generation = state.session_generation;
        }
        (
            request_endpoint,
            reply_endpoint,
            state.generation,
            request_id,
        )
    };
    let bytes = tmpfs_proxy_value_bytes(&request).to_vec();
    let pushed = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        match registry.object_index(request_endpoint) {
            Some(index) => match &mut registry.objects[index].data {
                CapabilityObjectData::Endpoint(endpoint)
                    if endpoint.queue.len() < abi::limits::MAX_ENDPOINT_MESSAGES =>
                {
                    endpoint.queue.push_back(EndpointMessage {
                        sender_process_id: 0,
                        bytes,
                        capabilities: Vec::new(),
                    });
                    Ok(())
                }
                CapabilityObjectData::Endpoint(_) => Err(ERR_TRY_AGAIN),
                CapabilityObjectData::Notification(_)
                | CapabilityObjectData::SharedMemory(_)
                | CapabilityObjectData::KernelEarlyLogReader(_)
                | CapabilityObjectData::Job(_) => Err(ERR_IO),
            },
            None => Err(ERR_IO),
        }
    };
    if let Err(error) = pushed {
        let mut state = TMPFS_PROXY.lock();
        if state.active_request_id == request_id {
            state.active_request_id = filesystem_protocol::INVALID_ID;
        }
        return Err(error);
    }
    let pending = PendingTmpfsProxyRequest {
        reply_endpoint,
        request_operation: request.operation,
        request_generation: legacy_generation,
        generic_request_id: request_id,
        operation,
        stack_pointer,
    };
    let mut manager = PROCESS_MANAGER.lock();
    let Some(process) = manager.process_mut(process_id) else {
        drop(manager);
        tmpfs_proxy_abandon_pending(pending);
        return Err(ERR_NO_PROCESS);
    };
    if process.pending_tmpfs_proxy.is_some()
        || process.pending_nullfs_proxy.is_some()
        || process.pending_vfs_request.is_some()
    {
        drop(manager);
        tmpfs_proxy_abandon_pending(pending);
        return Err(ERR_IO);
    }
    process.pending_tmpfs_proxy = Some(pending);
    process.state = ProcessState::Blocked;
    drop(manager);
    wake_endpoint_waiter(request_endpoint);
    Ok(())
}

fn tmpfs_proxy_reply_status(status: i32) -> Result<(), i64> {
    match status {
        tmpfs_protocol::status::OK => Ok(()),
        tmpfs_protocol::status::INVALID => Err(ERR_INVALID_ARGUMENT),
        tmpfs_protocol::status::NOT_FOUND => Err(ERR_NO_ENTRY),
        tmpfs_protocol::status::NO_SPACE => Err(ERR_NO_SPACE),
        tmpfs_protocol::status::RANGE => Err(abi::errno::RANGE),
        tmpfs_protocol::status::STALE_MOUNT => Err(ERR_IO),
        _ => Err(ERR_IO),
    }
}

fn tmpfs_proxy_complete_operation(
    process: &mut Process,
    operation: PendingTmpfsProxyOperation,
    reply: tmpfs_protocol::Reply,
    physical_memory_offset: VirtAddr,
) -> Result<u64, Error> {
    match operation {
        PendingTmpfsProxyOperation::Open {
            path,
            descriptor,
            readable,
            writable,
            append,
            close_on_exec,
            generation,
            session_id,
            session_generation,
        } => {
            let size = reply.value as u64;
            if usize::from(reply.data_length) != size_of::<u64>() {
                return Ok(error_return(ERR_IO));
            }
            let mut node_bytes = [0_u8; size_of::<u64>()];
            node_bytes.copy_from_slice(&reply.data[..size_of::<u64>()]);
            let node_id = u64::from_ne_bytes(node_bytes);
            if node_id == filesystem_protocol::INVALID_ID {
                return Ok(error_return(ERR_IO));
            }
            let offset = if append { size } else { 0 };
            let handle = Arc::new(PreemptMutex::new(OpenFileState {
                path,
                offset,
                readable,
                writable,
                append,
                size,
                nullfs_size: None,
                backend: OpenFileBackend::TmpfsProxy {
                    generation,
                    session_id,
                    session_generation,
                    node_id,
                },
            }));
            if descriptor_in_use(process, descriptor) || descriptor_count(process) >= MAX_OPEN_FILES
            {
                drop(handle);
                return Ok(error_return(ERR_TOO_MANY_OPEN_FILES));
            }
            process.open_files.push(OpenFile {
                descriptor,
                handle,
                close_on_exec,
            });
            process.open_count = process.open_count.saturating_add(1);
            Ok(descriptor)
        }
        PendingTmpfsProxyOperation::Read {
            handle,
            address,
            length,
        } => {
            let count = usize::from(reply.data_length);
            if count > length {
                return Ok(error_return(ERR_IO));
            }
            if count != 0 {
                write_user_bytes(
                    address,
                    &reply.data[..count],
                    physical_memory_offset,
                    &process.pages,
                )?;
            }
            {
                let mut file = handle.lock();
                file.offset = file.offset.saturating_add(count as u64);
            }
            process.read_count = process.read_count.saturating_add(1);
            process.bytes_read = process.bytes_read.saturating_add(count as u64);
            Ok(count as u64)
        }
        PendingTmpfsProxyOperation::Write {
            handle,
            initial_offset,
            length,
            ..
        } => {
            let count = reply.value as usize;
            if count > length || usize::from(reply.data_length) != size_of::<u64>() {
                return Ok(error_return(ERR_IO));
            }
            let mut offset_bytes = [0_u8; size_of::<u64>()];
            offset_bytes.copy_from_slice(&reply.data[..size_of::<u64>()]);
            let resulting_offset = u64::from_le_bytes(offset_bytes);
            {
                let mut file = handle.lock();
                if file.offset == initial_offset {
                    file.offset = resulting_offset;
                }
                file.size = file.size.max(resulting_offset);
            }
            process.write_count = process.write_count.saturating_add(1);
            process.bytes_written = process.bytes_written.saturating_add(count as u64);
            process.file_write_count = process.file_write_count.saturating_add(1);
            process.file_bytes_written = process.file_bytes_written.saturating_add(count as u64);
            Ok(count as u64)
        }
        PendingTmpfsProxyOperation::Stat { address, length } => {
            let Ok(length) = usize::try_from(length) else {
                return Ok(error_return(abi::errno::RANGE));
            };
            let required = size_of::<abi::file::Stat>();
            if length < required {
                return Ok(error_return(abi::errno::RANGE));
            }
            if !process_user_range_allows(process, address, required, true) {
                return Ok(error_return(ERR_BAD_ADDRESS));
            }
            let stat = abi::file::Stat {
                kind: abi::file::KIND_FILE,
                size: reply.value as u64,
                flags: 0,
            };
            write_user_bytes(
                address,
                tmpfs_proxy_value_bytes(&stat),
                physical_memory_offset,
                &process.pages,
            )?;
            Ok(0)
        }
        PendingTmpfsProxyOperation::ReadDirectory {
            start_index,
            records_address,
            capacity,
        } => tmpfs_proxy_complete_read_directory(
            process,
            &reply,
            start_index,
            records_address,
            capacity,
            physical_memory_offset,
        ),
        PendingTmpfsProxyOperation::Unlink => Ok(0),
    }
}

fn tmpfs_proxy_complete_read_directory(
    process: &Process,
    reply: &tmpfs_protocol::Reply,
    start_index: usize,
    records_address: u64,
    capacity: usize,
    physical_memory_offset: VirtAddr,
) -> Result<u64, Error> {
    let record_size = size_of::<abi::file::DirectoryEntry>();
    let byte_length = match capacity.checked_mul(record_size) {
        Some(length) => length,
        None => return Ok(error_return(ERR_ARGUMENT_TOO_LARGE)),
    };
    if !process_user_range_allows(process, records_address, byte_length, true) {
        return Ok(error_return(ERR_BAD_ADDRESS));
    }

    let data = &reply.data[..usize::from(reply.data_length)];
    let mut entry_index = 0usize;
    let mut written = 0usize;
    let mut cursor = 0usize;
    while cursor < data.len() {
        let start = cursor;
        while cursor < data.len() && data[cursor] != b'\n' {
            cursor = cursor.saturating_add(1);
        }
        let name = &data[start..cursor];
        if name.is_empty()
            || name.len() > abi::file::MAX_DIRECTORY_ENTRY_NAME_BYTES
            || name.contains(&b'/')
        {
            return Ok(error_return(ERR_IO));
        }
        if entry_index >= start_index && written < capacity {
            let record = tmpfs_proxy_directory_record(name);
            let Some(destination) = records_address.checked_add((written * record_size) as u64)
            else {
                return Ok(error_return(abi::errno::RANGE));
            };
            write_user_bytes(
                destination,
                tmpfs_proxy_value_bytes(&record),
                physical_memory_offset,
                &process.pages,
            )?;
            written = written.saturating_add(1);
        }
        entry_index = entry_index.saturating_add(1);
        if cursor < data.len() {
            cursor = cursor.saturating_add(1);
        }
    }
    Ok(written as u64)
}

fn tmpfs_proxy_directory_record(name: &[u8]) -> abi::file::DirectoryEntry {
    let mut record = abi::file::DirectoryEntry {
        kind: abi::file::KIND_FILE,
        size: 0,
        flags: 0,
        name_length: name.len() as u64,
        name: [0; abi::file::DIRECTORY_ENTRY_NAME_CAPACITY],
    };
    record.name[..name.len()].copy_from_slice(name);
    record
}

fn tmpfs_proxy_file_name(path: &str) -> Result<String, i64> {
    match tmpfs_proxy_path(path)? {
        Some(TmpfsProxyPath::File(name)) => Ok(name),
        Some(TmpfsProxyPath::Directory) => Err(ERR_IS_DIRECTORY),
        None => Err(ERR_INVALID_ARGUMENT),
    }
}

fn tmpfs_proxy_open(
    process_id: u64,
    path: &str,
    options: vfs::OpenOptions,
    close_on_exec: bool,
    descriptor: u64,
    stack_pointer: usize,
) -> ControlOutcome {
    let generation = match tmpfs_proxy_state() {
        Some(state) => state.generation,
        None => return ControlOutcome::Ready(error_return(ERR_IO)),
    };
    let name = match tmpfs_proxy_file_name(path) {
        Ok(name) => name,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::OPEN;
    request.node_id = filesystem_protocol::ROOT_NODE_ID;
    request.name_length = name.len() as u16;
    request.name[..name.len()].copy_from_slice(name.as_bytes());
    let mut flags = 0_u32;
    if options.read {
        flags |= filesystem_protocol::request_flags::READ;
    }
    if options.write {
        flags |= filesystem_protocol::request_flags::WRITE;
    }
    if options.create {
        flags |= filesystem_protocol::request_flags::CREATE;
    }
    if options.truncate {
        flags |= filesystem_protocol::request_flags::TRUNCATE;
    }
    if options.append {
        flags |= filesystem_protocol::request_flags::APPEND;
    }
    request.flags = flags;
    let path = if path == vfs::TMPFS_MOUNT_PATH {
        String::from(vfs::TMPFS_MOUNT_PATH)
    } else {
        let mut full_path = String::from("/tmp/");
        full_path.push_str(&name);
        full_path
    };
    let operation = PendingTmpfsProxyOperation::Open {
        path,
        descriptor,
        readable: options.read,
        writable: options.write,
        append: options.append,
        close_on_exec,
        generation,
        session_id: filesystem_protocol::INVALID_ID,
        session_generation: 0,
    };
    match tmpfs_proxy_begin_filesystem_request(process_id, request, operation, stack_pointer) {
        Ok(()) => ControlOutcome::Blocked,
        Err(error) => ControlOutcome::Ready(error_return(error)),
    }
}

fn tmpfs_proxy_read(
    process_id: u64,
    handle: OpenFileHandle,
    address: u64,
    length: usize,
    stack_pointer: usize,
) -> ReadOutcome {
    let (offset, generation, session_id, session_generation, node_id) = {
        let file = handle.lock();
        if !file.readable {
            return ReadOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
        }
        let OpenFileBackend::TmpfsProxy {
            generation,
            session_id,
            session_generation,
            node_id,
        } = file.backend
        else {
            return ReadOutcome::Ready(error_return(ERR_IO));
        };
        (
            file.offset,
            generation,
            session_id,
            session_generation,
            node_id,
        )
    };
    if tmpfs_proxy_state().is_none_or(|state| {
        state.generation != generation
            || state.session_id != session_id
            || state.session_generation != session_generation
    }) {
        return ReadOutcome::Ready(error_return(ERR_IO));
    }
    let count = length.min(tmpfs_protocol::MAX_DATA_BYTES);
    if count == 0 {
        return ReadOutcome::Ready(0);
    }
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::READ;
    request.node_id = node_id;
    request.file_offset = offset;
    request.bulk = filesystem_protocol::BulkBuffer {
        buffer_id: 1,
        offset: 0,
        length: count as u64,
    };
    let operation = PendingTmpfsProxyOperation::Read {
        handle,
        address,
        length: count,
    };
    match tmpfs_proxy_begin_filesystem_request(process_id, request, operation, stack_pointer) {
        Ok(()) => ReadOutcome::Blocked,
        Err(error) => ReadOutcome::Ready(error_return(error)),
    }
}

fn tmpfs_proxy_write(
    process_id: u64,
    handle: OpenFileHandle,
    bytes: &[u8],
    stack_pointer: usize,
) -> WriteOutcome {
    let (offset, initial_offset, generation, session_id, session_generation, node_id, append) = {
        let file = handle.lock();
        if !file.writable {
            return WriteOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
        }
        let OpenFileBackend::TmpfsProxy {
            generation,
            session_id,
            session_generation,
            node_id,
        } = file.backend
        else {
            return WriteOutcome::Ready(error_return(ERR_IO));
        };
        let offset = if file.append { file.size } else { file.offset };
        (
            offset,
            file.offset,
            generation,
            session_id,
            session_generation,
            node_id,
            file.append,
        )
    };
    if tmpfs_proxy_state().is_none_or(|state| {
        state.generation != generation
            || state.session_id != session_id
            || state.session_generation != session_generation
    }) {
        return WriteOutcome::Ready(error_return(ERR_IO));
    }
    let count = bytes.len().min(tmpfs_protocol::MAX_DATA_BYTES);
    if count == 0 {
        return WriteOutcome::Ready(0);
    }
    {
        let state = TMPFS_PROXY.lock();
        if !state.bulk_buffer_attached || state.active_request_id != filesystem_protocol::INVALID_ID
        {
            return WriteOutcome::Ready(error_return(ERR_TRY_AGAIN));
        }
    }
    if let Err(error) = tmpfs_proxy_bulk_write(&bytes[..count]) {
        return WriteOutcome::Ready(error_return(error));
    }
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::WRITE;
    request.flags = if append {
        filesystem_protocol::request_flags::APPEND
    } else {
        0
    };
    request.node_id = node_id;
    request.file_offset = offset;
    request.bulk = filesystem_protocol::BulkBuffer {
        buffer_id: 1,
        offset: 0,
        length: count as u64,
    };
    let operation = PendingTmpfsProxyOperation::Write {
        handle,
        offset,
        initial_offset,
        append,
        length: count,
    };
    match tmpfs_proxy_begin_filesystem_request(process_id, request, operation, stack_pointer) {
        Ok(()) => WriteOutcome::Blocked,
        Err(error) => WriteOutcome::Ready(error_return(error)),
    }
}

fn tmpfs_proxy_stat(
    process_id: u64,
    path: &str,
    address: u64,
    length: u64,
    stack_pointer: usize,
) -> ControlOutcome {
    if matches!(tmpfs_proxy_path(path), Ok(Some(TmpfsProxyPath::Directory))) {
        let metadata = vfs::Metadata {
            path: String::from(vfs::TMPFS_MOUNT_PATH),
            kind: vfs::NodeKind::Directory,
            size: 0,
            read_only: false,
            hidden: false,
            system: false,
        };
        return ControlOutcome::Ready(platform_write_value(
            process_id,
            address,
            length,
            platform_stat_from_metadata(&metadata),
        ));
    }
    if tmpfs_proxy_state().is_none() {
        return ControlOutcome::Ready(error_return(ERR_IO));
    }
    let name = match tmpfs_proxy_file_name(path) {
        Ok(name) => name,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::LOOKUP;
    request.node_id = filesystem_protocol::ROOT_NODE_ID;
    request.name_length = name.len() as u16;
    request.name[..name.len()].copy_from_slice(name.as_bytes());
    let operation = PendingTmpfsProxyOperation::Stat { address, length };
    match tmpfs_proxy_begin_filesystem_request(process_id, request, operation, stack_pointer) {
        Ok(()) => ControlOutcome::Blocked,
        Err(error) => ControlOutcome::Ready(error_return(error)),
    }
}

fn tmpfs_proxy_unlink(process_id: u64, path: &str, stack_pointer: usize) -> ControlOutcome {
    let name = match tmpfs_proxy_file_name(path) {
        Ok(name) => name,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::UNLINK;
    request.node_id = filesystem_protocol::ROOT_NODE_ID;
    request.name_length = name.len() as u16;
    request.name[..name.len()].copy_from_slice(name.as_bytes());
    match tmpfs_proxy_begin_filesystem_request(
        process_id,
        request,
        PendingTmpfsProxyOperation::Unlink,
        stack_pointer,
    ) {
        Ok(()) => ControlOutcome::Blocked,
        Err(error) => ControlOutcome::Ready(error_return(error)),
    }
}

fn tmpfs_proxy_read_directory(
    process_id: u64,
    path: &str,
    start_index: usize,
    records_address: u64,
    capacity: usize,
    stack_pointer: usize,
) -> ControlOutcome {
    if !matches!(tmpfs_proxy_path(path), Ok(Some(TmpfsProxyPath::Directory))) {
        return ControlOutcome::Ready(error_return(abi::errno::NOT_DIRECTORY));
    }
    if tmpfs_proxy_state().is_none() {
        return ControlOutcome::Ready(error_return(ERR_IO));
    }
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::READ_DIRECTORY;
    request.node_id = filesystem_protocol::ROOT_NODE_ID;
    request.bulk = filesystem_protocol::BulkBuffer {
        buffer_id: 1,
        offset: 0,
        length: 4096,
    };
    let operation = PendingTmpfsProxyOperation::ReadDirectory {
        start_index,
        records_address,
        capacity,
    };
    match tmpfs_proxy_begin_filesystem_request(process_id, request, operation, stack_pointer) {
        Ok(()) => ControlOutcome::Blocked,
        Err(error) => ControlOutcome::Ready(error_return(error)),
    }
}

fn nullfs_proxy_backend_is_current(
    generation: u32,
    session_id: u64,
    session_generation: u64,
) -> bool {
    nullfs_proxy_state().is_some_and(|state| {
        state.generation == generation
            && state.session_id == session_id
            && state.session_generation == session_generation
    })
}

fn nullfs_proxy_state() -> Option<TmpfsProxyState> {
    let state = *NULLFS_PROXY.lock();
    if state.request_endpoint.is_some()
        && state.generation != 0
        && state.bulk_buffer_attached
        && state.session_id != filesystem_protocol::INVALID_ID
        && state.session_generation != 0
        && state.session_features == filesystem_protocol::session_features::WRITE
    {
        Some(state)
    } else {
        None
    }
}

fn nullfs_proxy_node_size(
    generation: u32,
    session_id: u64,
    session_generation: u64,
    node_id: u64,
    observed_size: u64,
) -> Arc<PreemptMutex<u64>> {
    let mut sizes = NULLFS_NODE_SIZES.lock();
    sizes.retain(|entry| entry.size.strong_count() != 0);
    if let Some(size) = sizes
        .iter()
        .find(|entry| {
            entry.generation == generation
                && entry.session_id == session_id
                && entry.session_generation == session_generation
                && entry.node_id == node_id
        })
        .and_then(|entry| entry.size.upgrade())
    {
        *size.lock() = observed_size;
        return size;
    }

    let size = Arc::new(PreemptMutex::new(observed_size));
    sizes.push(NullfsNodeSize {
        generation,
        session_id,
        session_generation,
        node_id,
        size: Arc::downgrade(&size),
    });
    size
}

fn open_file_size(file: &OpenFileState) -> u64 {
    file.nullfs_size
        .as_ref()
        .map_or(file.size, |size| *size.lock())
}

fn update_open_file_size(file: &mut OpenFileState, size: u64) {
    if let Some(shared_size) = &file.nullfs_size {
        *shared_size.lock() = size;
    } else {
        file.size = size;
    }
}

fn nullfs_proxy_push_request(
    request_endpoint: CapabilityObjectRef,
    request: &filesystem_protocol::Request,
    capability: Option<TransferredCapability>,
) -> Result<(), i64> {
    let bytes = tmpfs_proxy_value_bytes(request).to_vec();
    let mut registry = CAPABILITY_REGISTRY.lock();
    match registry.object_index(request_endpoint) {
        Some(index) => match &mut registry.objects[index].data {
            CapabilityObjectData::Endpoint(endpoint)
                if endpoint.queue.len() < abi::limits::MAX_ENDPOINT_MESSAGES =>
            {
                endpoint.queue.push_back(EndpointMessage {
                    sender_process_id: 0,
                    bytes,
                    capabilities: capability.into_iter().collect(),
                });
                Ok(())
            }
            CapabilityObjectData::Endpoint(_) => Err(ERR_TRY_AGAIN),
            CapabilityObjectData::Notification(_)
            | CapabilityObjectData::SharedMemory(_)
            | CapabilityObjectData::KernelEarlyLogReader(_)
            | CapabilityObjectData::Job(_) => Err(ERR_IO),
        },
        None => Err(ERR_IO),
    }
}

fn nullfs_proxy_begin_connect(
    request_endpoint: CapabilityObjectRef,
    generation: u32,
) -> Result<(), i64> {
    {
        let state = NULLFS_PROXY.lock();
        if state.request_endpoint.is_some()
            || generation <= state.generation
            || request_endpoint.id <= state.retired_request_endpoint_id
        {
            return Err(ERR_INVALID_ARGUMENT);
        }
    }
    let reply_endpoint = tmpfs_proxy_create_reply_endpoint()?;
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::CONNECT;
    request.flags = filesystem_protocol::connect_flags::WRITE;
    request.request_id = 1;
    if let Err(error) = nullfs_proxy_push_request(
        request_endpoint,
        &request,
        Some(TransferredCapability {
            object: reply_endpoint,
            rights: abi::capability::RIGHT_SEND,
        }),
    ) {
        tmpfs_proxy_release_reply_endpoint(reply_endpoint);
        return Err(error);
    }

    let previous = {
        let mut state = NULLFS_PROXY.lock();
        let previous = (
            state.request_endpoint,
            state.connect_reply_endpoint,
            state.session_reply_endpoint,
            state.bulk_buffer,
        );
        state.request_endpoint = Some(request_endpoint);
        state.generation = generation;
        state.connect_reply_endpoint = Some(reply_endpoint);
        state.session_reply_endpoint = None;
        state.session_id = filesystem_protocol::INVALID_ID;
        state.session_generation = 0;
        state.session_features = 0;
        state.bulk_buffer = None;
        state.bulk_buffer_attached = false;
        state.active_request_id = filesystem_protocol::INVALID_ID;
        state.active_close = None;
        previous
    };
    drop(NULLFS_ABANDONED_REQUEST.lock().take());
    if previous.0 != Some(request_endpoint) {
        if let Some(endpoint) = previous.0 {
            kernel_capability_root_remove(endpoint);
        }
        kernel_capability_root_add(request_endpoint);
    }
    if let Some(endpoint) = previous.1 {
        tmpfs_proxy_release_reply_endpoint(endpoint);
    }
    if let Some(endpoint) = previous.2 {
        tmpfs_proxy_release_reply_endpoint(endpoint);
    }
    if let Some(buffer) = previous.3 {
        kernel_capability_root_remove(buffer);
    }
    nullfs_proxy_cancel_stale_requests(generation);
    wake_endpoint_waiter(request_endpoint);
    Ok(())
}

fn nullfs_proxy_offline(generation: u32) -> Result<(), i64> {
    let (request_endpoint, connect_reply_endpoint, session_reply_endpoint, bulk_buffer) = {
        let mut state = NULLFS_PROXY.lock();
        if state.generation != generation {
            return Err(ERR_INVALID_ARGUMENT);
        }
        let Some(request_endpoint) = state.request_endpoint.take() else {
            return Ok(());
        };
        state.retired_request_endpoint_id = request_endpoint.id;
        let resources = (
            request_endpoint,
            state.connect_reply_endpoint.take(),
            state.session_reply_endpoint.take(),
            state.bulk_buffer.take(),
        );
        state.session_id = filesystem_protocol::INVALID_ID;
        state.session_generation = 0;
        state.session_features = 0;
        state.bulk_buffer_attached = false;
        state.active_request_id = filesystem_protocol::INVALID_ID;
        state.active_close = None;
        resources
    };
    drop(NULLFS_ABANDONED_REQUEST.lock().take());
    NULLFS_CLOSE_QUEUE
        .lock()
        .retain(|ticket| ticket.generation != generation);
    NULLFS_NODE_SIZES
        .lock()
        .retain(|entry| entry.generation != generation);
    kernel_capability_root_remove(request_endpoint);
    if let Some(endpoint) = connect_reply_endpoint {
        tmpfs_proxy_release_reply_endpoint(endpoint);
    }
    if let Some(endpoint) = session_reply_endpoint
        && Some(endpoint) != connect_reply_endpoint
    {
        tmpfs_proxy_release_reply_endpoint(endpoint);
    }
    if let Some(buffer) = bulk_buffer {
        kernel_capability_root_remove(buffer);
    }
    CAPABILITY_REGISTRY.lock().collect_garbage();
    nullfs_proxy_cancel_generation(generation);
    Ok(())
}

fn nullfs_proxy_cancel_generation(generation: u32) {
    let pending: Vec<(u64, PendingNullfsProxyRequest)> = {
        let mut manager = PROCESS_MANAGER.lock();
        manager
            .processes
            .iter_mut()
            .filter_map(|process| {
                let request = process.pending_nullfs_proxy.as_ref()?;
                if request.request_generation != generation {
                    return None;
                }
                process
                    .pending_nullfs_proxy
                    .take()
                    .map(|pending| (process.process_id, pending))
            })
            .collect()
    };
    nullfs_proxy_fail_pending(pending);
    executable_load_cancel_nullfs_generations(generation, true);
}

fn nullfs_proxy_cancel_stale_requests(generation: u32) {
    let pending: Vec<(u64, PendingNullfsProxyRequest)> = {
        let mut manager = PROCESS_MANAGER.lock();
        manager
            .processes
            .iter_mut()
            .filter_map(|process| {
                let request = process.pending_nullfs_proxy.as_ref()?;
                if request.request_generation == generation {
                    return None;
                }
                process
                    .pending_nullfs_proxy
                    .take()
                    .map(|pending| (process.process_id, pending))
            })
            .collect()
    };
    nullfs_proxy_fail_pending(pending);
    executable_load_cancel_nullfs_generations(generation, false);
}

fn executable_load_cancel_nullfs_generations(generation: u32, matching: bool) {
    let closes = {
        let mut manager = PROCESS_MANAGER.lock();
        let mut closes = Vec::new();
        for process in &mut manager.processes {
            let Some(load) = process.pending_executable_load.as_mut() else {
                continue;
            };
            if load.provider_generation == 0 || (load.provider_generation == generation) != matching
            {
                continue;
            }
            if let Some(close) = load.take_close_ticket() {
                closes.push(close);
            }
            load.bytes.clear();
            load.retry = None;
            load.result = Some(Err(ERR_IO));
        }
        closes
    };
    for close in closes {
        nullfs_proxy_enqueue_close_ticket(close);
    }
}

fn nullfs_proxy_fail_pending(pending: Vec<(u64, PendingNullfsProxyRequest)>) {
    for (process_id, pending) in pending {
        if let Some(owner) = nullfs_proxy_executable_owner(&pending.operation) {
            executable_load_fail(process_id, owner, pending.stack_pointer, ERR_IO);
            continue;
        }
        if scheduler::with_process_address_space(process_id, || {
            let registers = unsafe { &mut *(pending.stack_pointer as *mut SavedRegisters) };
            registers.rax = error_return(ERR_IO);
        })
        .is_none()
        {
            continue;
        }
        let made_runnable = {
            let mut manager = PROCESS_MANAGER.lock();
            let Some(process) = manager.process_mut(process_id) else {
                continue;
            };
            process.make_runnable();
            true
        };
        if made_runnable {
            let _ = scheduler::wake_process(process_id);
        }
    }
}

fn nullfs_proxy_service_connect() -> bool {
    let state = *NULLFS_PROXY.lock();
    if state.connect_reply_endpoint.is_none() && !state.bulk_buffer_attached {
        if state.bulk_buffer.is_some() {
            return nullfs_proxy_service_attach(state);
        }
        if state.session_reply_endpoint.is_some()
            && state.session_id != filesystem_protocol::INVALID_ID
            && state.session_generation != 0
            && state.session_features == filesystem_protocol::session_features::WRITE
        {
            return nullfs_proxy_begin_attach().is_ok();
        }
    }
    let Some(reply_endpoint) = state.connect_reply_endpoint else {
        return false;
    };
    let message = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        let Some(index) = registry.object_index(reply_endpoint) else {
            return false;
        };
        let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[index].data else {
            return false;
        };
        endpoint.queue.pop_front()
    };
    let Some(message) = message else {
        return false;
    };
    let valid = if message.capabilities.is_empty()
        && message.bytes.len() == size_of::<filesystem_protocol::Reply>()
    {
        let reply = unsafe {
            ptr::read_unaligned(message.bytes.as_ptr() as *const filesystem_protocol::Reply)
        };
        reply.version == filesystem_protocol::VERSION
            && reply.operation == filesystem_protocol::operation::CONNECT
            && reply.status == filesystem_protocol::status::OK
            && reply.flags == 0
            && reply.request_id == 1
            && reply.session_id != filesystem_protocol::INVALID_ID
            && reply.generation == u64::from(state.generation)
            && reply.node_id == filesystem_protocol::ROOT_NODE_ID
            && reply.node_kind == filesystem_protocol::node_kind::DIRECTORY
            && reply.value == filesystem_protocol::session_features::WRITE
            && reply.data_length == 0
            && reply.reserved == [0; 2]
            && reply.data == [0; filesystem_protocol::MAX_INLINE_DATA_BYTES]
            && {
                let mut proxy = NULLFS_PROXY.lock();
                if proxy.connect_reply_endpoint == Some(reply_endpoint) {
                    proxy.connect_reply_endpoint = None;
                    proxy.session_reply_endpoint = Some(reply_endpoint);
                    proxy.session_id = reply.session_id;
                    proxy.session_generation = reply.generation;
                    proxy.session_features = reply.value;
                    true
                } else {
                    false
                }
            }
    } else {
        false
    };
    if !valid {
        let mut proxy = NULLFS_PROXY.lock();
        if proxy.connect_reply_endpoint == Some(reply_endpoint) {
            proxy.connect_reply_endpoint = None;
            proxy.session_id = filesystem_protocol::INVALID_ID;
            proxy.session_generation = 0;
            proxy.session_features = 0;
        }
        drop(proxy);
        tmpfs_proxy_release_reply_endpoint(reply_endpoint);
    } else if nullfs_proxy_begin_attach().is_err() {
        let mut proxy = NULLFS_PROXY.lock();
        proxy.session_features = 0;
        proxy.bulk_buffer = None;
        proxy.bulk_buffer_attached = false;
    }
    true
}

fn nullfs_proxy_begin_attach() -> Result<(), i64> {
    let state = *NULLFS_PROXY.lock();
    let request_endpoint = state.request_endpoint.ok_or(ERR_IO)?;
    if state.session_id == filesystem_protocol::INVALID_ID || state.session_generation == 0 {
        return Err(ERR_IO);
    }
    let buffer = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        registry.collect_garbage();
        if registry
            .shared_memory_bytes()
            .saturating_add(FILESYSTEM_PROXY_BULK_BYTES)
            > abi::limits::MAX_SHARED_MEMORY_TOTAL_BYTES
        {
            return Err(ERR_NO_SPACE);
        }
        registry.create_object(
            abi::capability::KIND_SHARED_MEMORY,
            CapabilityObjectData::SharedMemory(SharedMemoryObject {
                bytes: vec![0_u8; FILESYSTEM_PROXY_BULK_BYTES],
            }),
        )?
    };
    kernel_capability_root_add(buffer);
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::ATTACH_BUFFER;
    request.request_id = 2;
    request.session_id = state.session_id;
    request.generation = state.session_generation;
    request.bulk = filesystem_protocol::BulkBuffer {
        buffer_id: 1,
        offset: 0,
        length: FILESYSTEM_PROXY_BULK_BYTES as u64,
    };
    if let Err(error) = nullfs_proxy_push_request(
        request_endpoint,
        &request,
        Some(TransferredCapability {
            object: buffer,
            rights: abi::capability::RIGHT_READ | abi::capability::RIGHT_WRITE,
        }),
    ) {
        kernel_capability_root_remove(buffer);
        CAPABILITY_REGISTRY.lock().collect_garbage();
        return Err(error);
    }
    let mut proxy = NULLFS_PROXY.lock();
    if proxy.session_id != state.session_id
        || proxy.session_generation != state.session_generation
        || proxy.request_endpoint != Some(request_endpoint)
    {
        drop(proxy);
        kernel_capability_root_remove(buffer);
        return Err(ERR_IO);
    }
    proxy.bulk_buffer = Some(buffer);
    proxy.bulk_buffer_attached = false;
    drop(proxy);
    wake_endpoint_waiter(request_endpoint);
    Ok(())
}

fn nullfs_proxy_service_attach(state: TmpfsProxyState) -> bool {
    let Some(reply_endpoint) = state.session_reply_endpoint else {
        return false;
    };
    let message = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        let Some(index) = registry.object_index(reply_endpoint) else {
            return false;
        };
        let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[index].data else {
            return false;
        };
        endpoint.queue.pop_front()
    };
    let Some(message) = message else {
        return false;
    };
    let valid = if message.capabilities.is_empty()
        && message.bytes.len() == size_of::<filesystem_protocol::Reply>()
    {
        let reply = unsafe {
            ptr::read_unaligned(message.bytes.as_ptr() as *const filesystem_protocol::Reply)
        };
        reply.version == filesystem_protocol::VERSION
            && reply.operation == filesystem_protocol::operation::ATTACH_BUFFER
            && reply.status == filesystem_protocol::status::OK
            && reply.flags == 0
            && reply.request_id == 2
            && reply.session_id == state.session_id
            && reply.generation == state.session_generation
            && reply.node_id == filesystem_protocol::INVALID_ID
            && reply.value == 0
            && reply.data_length == 0
            && reply.node_kind == filesystem_protocol::node_kind::UNKNOWN
            && reply.reserved == [0; 2]
            && reply.data == [0; filesystem_protocol::MAX_INLINE_DATA_BYTES]
    } else {
        false
    };
    let mut proxy = NULLFS_PROXY.lock();
    if proxy.session_id == state.session_id
        && proxy.session_generation == state.session_generation
        && proxy.bulk_buffer == state.bulk_buffer
    {
        proxy.bulk_buffer_attached = valid;
        if !valid {
            proxy.session_features = 0;
            let buffer = proxy.bulk_buffer.take();
            drop(proxy);
            if let Some(buffer) = buffer {
                kernel_capability_root_remove(buffer);
                CAPABILITY_REGISTRY.lock().collect_garbage();
            }
            return true;
        }
    }
    true
}

fn nullfs_proxy_stage_write(
    buffer: CapabilityObjectRef,
    bulk: filesystem_protocol::BulkBuffer,
    source: &[u8],
) -> Result<(), i64> {
    let offset = usize::try_from(bulk.offset).map_err(|_| abi::errno::RANGE)?;
    let length = usize::try_from(bulk.length).map_err(|_| abi::errno::RANGE)?;
    if bulk.buffer_id != 1 || length != source.len() {
        return Err(abi::errno::RANGE);
    }
    let end = offset.checked_add(length).ok_or(abi::errno::RANGE)?;
    let mut registry = CAPABILITY_REGISTRY.lock();
    let index = registry.object_index(buffer).ok_or(ERR_IO)?;
    let CapabilityObjectData::SharedMemory(memory) = &mut registry.objects[index].data else {
        return Err(ERR_IO);
    };
    let destination = memory.bytes.get_mut(offset..end).ok_or(abi::errno::RANGE)?;
    destination.copy_from_slice(source);
    Ok(())
}

fn nullfs_proxy_begin_request(
    process_id: u64,
    request: filesystem_protocol::Request,
    operation: PendingNullfsProxyOperation,
    stack_pointer: usize,
) -> Result<(), i64> {
    nullfs_proxy_submit_request(process_id, request, operation, stack_pointer, None, None)
}

fn nullfs_proxy_submit_request(
    process_id: u64,
    mut request: filesystem_protocol::Request,
    mut operation: PendingNullfsProxyOperation,
    stack_pointer: usize,
    previous_request_id: Option<u64>,
    staged_bytes: Option<&[u8]>,
) -> Result<(), i64> {
    let request_endpoint = cpu_interrupts::without_interrupts(|| -> Result<_, i64> {
        let mut state = NULLFS_PROXY.lock();
        let request_endpoint = state.request_endpoint.ok_or(ERR_IO)?;
        if !state.bulk_buffer_attached {
            return Err(ERR_TRY_AGAIN);
        }
        match previous_request_id {
            Some(previous_request_id) if state.active_request_id != previous_request_id => {
                return Err(ERR_IO);
            }
            None if state.active_request_id != filesystem_protocol::INVALID_ID => {
                return Err(ERR_TRY_AGAIN);
            }
            _ => {}
        }
        let reply_endpoint = state.session_reply_endpoint.ok_or(ERR_IO)?;
        if state.session_id == filesystem_protocol::INVALID_ID || state.session_generation == 0 {
            return Err(ERR_IO);
        }
        let request_id = state.next_request_id;
        state.next_request_id = state
            .next_request_id
            .checked_add(1)
            .filter(|id| *id != filesystem_protocol::INVALID_ID)
            .ok_or(ERR_IO)?;
        state.active_request_id = request_id;
        request.request_id = request_id;
        request.session_id = state.session_id;
        request.generation = state.session_generation;
        match &mut operation {
            PendingNullfsProxyOperation::Open {
                generation,
                session_id,
                session_generation,
                ..
            }
            | PendingNullfsProxyOperation::LoadExecutableOpen {
                generation,
                session_id,
                session_generation,
                ..
            } => {
                *generation = state.generation;
                *session_id = state.session_id;
                *session_generation = state.session_generation;
            }
            _ => {}
        }
        let pending = PendingNullfsProxyRequest {
            reply_endpoint,
            request,
            request_operation: request.operation,
            request_generation: state.generation,
            request_id,
            operation,
            stack_pointer,
        };
        let previous_process_state = {
            let mut manager = PROCESS_MANAGER.lock();
            let Some(process) = manager.process_mut(process_id) else {
                state.active_request_id = filesystem_protocol::INVALID_ID;
                return Err(ERR_NO_PROCESS);
            };
            if process.pending_nullfs_proxy.is_some()
                || process.pending_tmpfs_proxy.is_some()
                || process.pending_vfs_request.is_some()
            {
                state.active_request_id = filesystem_protocol::INVALID_ID;
                return Err(ERR_IO);
            }
            let previous_state = process.state;
            process.pending_nullfs_proxy = Some(pending);
            process.state = ProcessState::Blocked;
            previous_state
        };
        let submitted = if let Some(source) = staged_bytes {
            state
                .bulk_buffer
                .ok_or(ERR_IO)
                .and_then(|buffer| nullfs_proxy_stage_write(buffer, request.bulk, source))
                .and_then(|()| nullfs_proxy_push_request(request_endpoint, &request, None))
        } else {
            nullfs_proxy_push_request(request_endpoint, &request, None)
        };
        if let Err(error) = submitted {
            let mut manager = PROCESS_MANAGER.lock();
            if let Some(process) = manager.process_mut(process_id)
                && process
                    .pending_nullfs_proxy
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == request_id)
            {
                process.pending_nullfs_proxy = None;
                process.state = previous_process_state;
            }
            if state.active_request_id == request_id {
                state.active_request_id = filesystem_protocol::INVALID_ID;
            }
            return Err(error);
        }
        drop(state);
        Ok(request_endpoint)
    })?;
    wake_endpoint_waiter(request_endpoint);
    Ok(())
}

fn nullfs_proxy_take_reply(
    pending: &PendingNullfsProxyRequest,
) -> Option<Result<filesystem_protocol::Reply, i64>> {
    let message = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        let Some(index) = registry.object_index(pending.reply_endpoint) else {
            return Some(Err(ERR_IO));
        };
        let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[index].data else {
            return Some(Err(ERR_IO));
        };
        endpoint.queue.pop_front()
    }?;
    Some(nullfs_proxy_decode_reply(message, pending))
}

fn nullfs_proxy_decode_reply(
    message: EndpointMessage,
    pending: &PendingNullfsProxyRequest,
) -> Result<filesystem_protocol::Reply, i64> {
    if !message.capabilities.is_empty()
        || message.bytes.len() != size_of::<filesystem_protocol::Reply>()
    {
        return Err(ERR_IO);
    }
    let reply =
        unsafe { ptr::read_unaligned(message.bytes.as_ptr() as *const filesystem_protocol::Reply) };
    let state = *NULLFS_PROXY.lock();
    if reply.version != filesystem_protocol::VERSION
        || reply.operation != pending.request_operation
        || reply.request_id != pending.request_id
        || reply.session_id != state.session_id
        || reply.generation != state.session_generation
        || reply.reserved != [0; 2]
        || state.generation != pending.request_generation
        || state.active_request_id != pending.request_id
        || reply.flags & !filesystem_protocol::reply_flags::ALL != 0
    {
        return Err(ERR_IO);
    }
    if !nullfs_proxy_status_is_known(reply.status) {
        return Err(ERR_IO);
    }
    let canonical_error = reply.flags == 0
        && reply.node_id == filesystem_protocol::INVALID_ID
        && reply.value == 0
        && reply.data_length == 0
        && reply.node_kind == filesystem_protocol::node_kind::UNKNOWN
        && reply.data == [0; filesystem_protocol::MAX_INLINE_DATA_BYTES];
    if reply.status != filesystem_protocol::status::OK {
        return canonical_error.then_some(reply).ok_or(ERR_IO);
    }
    let canonical_success = match pending.request_operation {
        filesystem_protocol::operation::LOOKUP => {
            reply.flags == 0
                && reply.node_id != filesystem_protocol::INVALID_ID
                && matches!(
                    reply.node_kind,
                    filesystem_protocol::node_kind::FILE
                        | filesystem_protocol::node_kind::DIRECTORY
                )
                && reply.data_length == 0
                && reply.data == [0; filesystem_protocol::MAX_INLINE_DATA_BYTES]
        }
        filesystem_protocol::operation::OPEN => {
            let valid = matches!(
                &pending.operation,
                PendingNullfsProxyOperation::Open { .. }
                    | PendingNullfsProxyOperation::LoadExecutableOpen { .. }
            ) && reply.flags == 0
                && reply.node_id != filesystem_protocol::INVALID_ID
                && matches!(
                    reply.node_kind,
                    filesystem_protocol::node_kind::FILE
                        | filesystem_protocol::node_kind::DIRECTORY
                )
                && reply.data_length == 0
                && reply.data == [0; filesystem_protocol::MAX_INLINE_DATA_BYTES];
            if !valid
                && reply.node_id != filesystem_protocol::INVALID_ID
                && matches!(
                    reply.node_kind,
                    filesystem_protocol::node_kind::FILE
                        | filesystem_protocol::node_kind::DIRECTORY
                )
            {
                nullfs_proxy_enqueue_close(
                    pending.request_generation,
                    reply.session_id,
                    reply.generation,
                    reply.node_id,
                );
            }
            valid
        }
        filesystem_protocol::operation::READ => {
            reply.flags == 0
                && reply.node_id == filesystem_protocol::INVALID_ID
                && reply.node_kind == filesystem_protocol::node_kind::UNKNOWN
                && reply.data_length == 0
                && reply.data == [0; filesystem_protocol::MAX_INLINE_DATA_BYTES]
        }
        filesystem_protocol::operation::WRITE => {
            matches!(
                &pending.operation,
                PendingNullfsProxyOperation::Write {
                    offset,
                    append,
                    length,
                    ..
                } if reply.value <= *length as u64
                    && filesystem_protocol::decode_write_reply_offset(&reply).is_some_and(
                        |resulting_offset| if *append {
                            resulting_offset.checked_sub(reply.value).is_some()
                        } else {
                            offset.checked_add(reply.value) == Some(resulting_offset)
                        }
                    )
            ) && reply.flags == 0
                && reply.node_id == filesystem_protocol::INVALID_ID
                && reply.node_kind == filesystem_protocol::node_kind::UNKNOWN
        }
        filesystem_protocol::operation::UNLINK => {
            matches!(&pending.operation, PendingNullfsProxyOperation::Unlink) && canonical_error
        }
        filesystem_protocol::operation::READ_DIRECTORY => {
            reply.node_id == filesystem_protocol::INVALID_ID
                && reply.node_kind == filesystem_protocol::node_kind::UNKNOWN
                && reply.data_length == 0
                && reply.data == [0; filesystem_protocol::MAX_INLINE_DATA_BYTES]
        }
        _ => false,
    };
    canonical_success.then_some(reply).ok_or(ERR_IO)
}

fn nullfs_proxy_release_request(request_id: u64) {
    let mut state = NULLFS_PROXY.lock();
    if state.active_request_id == request_id {
        state.active_request_id = filesystem_protocol::INVALID_ID;
    }
}

fn nullfs_proxy_executable_owner(
    operation: &PendingNullfsProxyOperation,
) -> Option<ExecutableLoadOwner> {
    match operation {
        PendingNullfsProxyOperation::Lookup {
            purpose: NullfsPathPurpose::LoadExecutable { owner, .. },
            ..
        }
        | PendingNullfsProxyOperation::LoadExecutableOpen { owner, .. }
        | PendingNullfsProxyOperation::LoadExecutableRead { owner, .. } => Some(*owner),
        _ => None,
    }
}

fn nullfs_proxy_request_is_mutating(pending: &PendingNullfsProxyRequest) -> bool {
    matches!(
        &pending.operation,
        PendingNullfsProxyOperation::Open { writable: true, .. }
            | PendingNullfsProxyOperation::Write { .. }
            | PendingNullfsProxyOperation::Unlink
    )
}

fn nullfs_proxy_quarantine(pending: &PendingNullfsProxyRequest) {
    let mut state = NULLFS_PROXY.lock();
    if state.generation == pending.request_generation {
        state.bulk_buffer_attached = false;
        state.session_features = 0;
    }
}

fn nullfs_proxy_bulk_read_at(offset: usize, destination: &mut [u8]) -> Result<(), i64> {
    let buffer = NULLFS_PROXY.lock().bulk_buffer.ok_or(ERR_IO)?;
    let registry = CAPABILITY_REGISTRY.lock();
    let index = registry.object_index(buffer).ok_or(ERR_IO)?;
    let CapabilityObjectData::SharedMemory(memory) = &registry.objects[index].data else {
        return Err(ERR_IO);
    };
    let end = offset
        .checked_add(destination.len())
        .ok_or(abi::errno::RANGE)?;
    let source = memory.bytes.get(offset..end).ok_or(abi::errno::RANGE)?;
    destination.copy_from_slice(source);
    Ok(())
}

fn nullfs_proxy_abandon_pending(pending: PendingNullfsProxyRequest) {
    if NULLFS_PROXY.lock().active_request_id != pending.request_id {
        return;
    }
    let mut abandoned = NULLFS_ABANDONED_REQUEST.lock();
    assert!(abandoned.is_none(), "multiple abandoned nullfs requests");
    *abandoned = Some(pending);
}

fn nullfs_proxy_service_abandoned() -> bool {
    let pending = NULLFS_ABANDONED_REQUEST.lock().clone();
    let Some(pending) = pending else {
        return false;
    };
    let Some(reply) = nullfs_proxy_take_reply(&pending) else {
        return false;
    };
    nullfs_proxy_release_request(pending.request_id);
    let abandoned = {
        let mut slot = NULLFS_ABANDONED_REQUEST.lock();
        if slot
            .as_ref()
            .is_some_and(|current| current.request_id == pending.request_id)
        {
            slot.take()
        } else {
            None
        }
    };
    let Some(abandoned) = abandoned else {
        return true;
    };
    nullfs_proxy_complete_abandoned(abandoned, reply);
    true
}

fn nullfs_proxy_complete_abandoned(
    abandoned: PendingNullfsProxyRequest,
    reply: Result<filesystem_protocol::Reply, i64>,
) {
    let quarantine = nullfs_proxy_request_is_mutating(&abandoned)
        && match &reply {
            Ok(reply) => reply.status == filesystem_protocol::status::OUTCOME_UNKNOWN,
            Err(_) => true,
        };
    if quarantine {
        nullfs_proxy_quarantine(&abandoned);
    }
    match (abandoned.operation, reply) {
        (
            PendingNullfsProxyOperation::Open { .. }
            | PendingNullfsProxyOperation::LoadExecutableOpen { .. },
            Ok(reply),
        ) if reply.status == filesystem_protocol::status::OK
            && reply.node_id != filesystem_protocol::INVALID_ID =>
        {
            nullfs_proxy_enqueue_close(
                abandoned.request_generation,
                reply.session_id,
                reply.generation,
                reply.node_id,
            );
        }
        (
            PendingNullfsProxyOperation::Write {
                handle,
                initial_offset,
                length,
                ..
            },
            Ok(reply),
        ) if reply.status == filesystem_protocol::status::OK && reply.value <= length as u64 => {
            let Some(resulting_offset) = filesystem_protocol::decode_write_reply_offset(&reply)
            else {
                return;
            };
            let mut file = handle.lock();
            let size = open_file_size(&file).max(resulting_offset);
            update_open_file_size(&mut file, size);
            if file.offset == initial_offset {
                file.offset = resulting_offset;
            }
        }
        _ => {}
    }
}

fn nullfs_proxy_enqueue_close_ticket(ticket: PendingTmpfsClose) {
    nullfs_proxy_enqueue_close(
        ticket.generation,
        ticket.session_id,
        ticket.session_generation,
        ticket.node_id,
    );
}

fn nullfs_proxy_enqueue_close(
    generation: u32,
    session_id: u64,
    session_generation: u64,
    node_id: u64,
) {
    debug_assert_ne!(generation, 0);
    debug_assert_ne!(session_id, filesystem_protocol::INVALID_ID);
    debug_assert_ne!(session_generation, 0);
    debug_assert_ne!(node_id, filesystem_protocol::INVALID_ID);
    let state = *NULLFS_PROXY.lock();
    if state.request_endpoint.is_none()
        || state.generation != generation
        || state.session_id != session_id
        || state.session_generation != session_generation
    {
        return;
    }
    let mut queue = NULLFS_CLOSE_QUEUE.lock();
    assert!(
        queue.len() < MAX_PENDING_TMPFS_CLOSES,
        "bounded nullfs close queue exhausted"
    );
    queue.push_back(PendingTmpfsClose {
        generation,
        session_id,
        session_generation,
        node_id,
    });
}

fn nullfs_proxy_service_close() -> bool {
    let active = NULLFS_PROXY.lock().active_close;
    if let Some(active) = active {
        let message = {
            let mut registry = CAPABILITY_REGISTRY.lock();
            let Some(index) = registry.object_index(active.reply_endpoint) else {
                return false;
            };
            let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[index].data else {
                return false;
            };
            endpoint.queue.pop_front()
        };
        let Some(message) = message else {
            return false;
        };
        match tmpfs_proxy_validate_close_reply(message, active)
            .filter(|status| nullfs_proxy_status_is_known(*status))
        {
            Some(filesystem_protocol::status::OK)
            | Some(filesystem_protocol::status::STALE_NODE)
            | Some(filesystem_protocol::status::STALE_SESSION) => {
                nullfs_proxy_finish_close(active);
            }
            Some(filesystem_protocol::status::TRY_AGAIN) => {
                nullfs_proxy_finish_close(active);
                NULLFS_CLOSE_QUEUE.lock().push_front(active.ticket);
            }
            Some(_) | None => {
                nullfs_proxy_fail_close(active);
                return true;
            }
        }
    }

    loop {
        let Some(ticket) = NULLFS_CLOSE_QUEUE.lock().pop_front() else {
            return active.is_some();
        };
        let start = {
            let mut state = NULLFS_PROXY.lock();
            if ticket.generation != state.generation
                || ticket.session_id != state.session_id
                || ticket.session_generation != state.session_generation
            {
                None
            } else if !state.bulk_buffer_attached
                || state.active_request_id != filesystem_protocol::INVALID_ID
            {
                drop(state);
                NULLFS_CLOSE_QUEUE.lock().push_front(ticket);
                return active.is_some();
            } else {
                let Some(request_endpoint) = state.request_endpoint else {
                    drop(state);
                    NULLFS_CLOSE_QUEUE.lock().push_front(ticket);
                    return active.is_some();
                };
                let Some(reply_endpoint) = state.session_reply_endpoint else {
                    drop(state);
                    NULLFS_CLOSE_QUEUE.lock().push_front(ticket);
                    return active.is_some();
                };
                let request_id = state.next_request_id;
                let Some(next_request_id) = state
                    .next_request_id
                    .checked_add(1)
                    .filter(|id| *id != filesystem_protocol::INVALID_ID)
                else {
                    drop(state);
                    NULLFS_CLOSE_QUEUE.lock().push_front(ticket);
                    return active.is_some();
                };
                state.next_request_id = next_request_id;
                state.active_request_id = request_id;
                let close = ActiveTmpfsClose {
                    ticket,
                    request_id,
                    reply_endpoint,
                    session_id: ticket.session_id,
                    session_generation: ticket.session_generation,
                };
                state.active_close = Some(close);
                Some((request_endpoint, close))
            }
        };
        let Some((request_endpoint, close)) = start else {
            continue;
        };
        let mut request = filesystem_protocol::Request::EMPTY;
        request.operation = filesystem_protocol::operation::CLOSE_NODE;
        request.request_id = close.request_id;
        request.session_id = close.session_id;
        request.generation = close.session_generation;
        request.node_id = close.ticket.node_id;
        if nullfs_proxy_push_request(request_endpoint, &request, None).is_err() {
            let mut state = NULLFS_PROXY.lock();
            if state.active_close == Some(close) {
                state.active_close = None;
                state.active_request_id = filesystem_protocol::INVALID_ID;
            }
            drop(state);
            NULLFS_CLOSE_QUEUE.lock().push_front(ticket);
            return active.is_some();
        }
        wake_endpoint_waiter(request_endpoint);
        return true;
    }
}

fn nullfs_proxy_finish_close(active: ActiveTmpfsClose) {
    let mut state = NULLFS_PROXY.lock();
    if state.active_close == Some(active) {
        state.active_close = None;
        state.active_request_id = filesystem_protocol::INVALID_ID;
    }
}

fn nullfs_proxy_fail_close(active: ActiveTmpfsClose) {
    let mut state = NULLFS_PROXY.lock();
    if state.active_close == Some(active) {
        state.active_close = None;
        state.active_request_id = filesystem_protocol::INVALID_ID;
        state.session_features = 0;
        state.bulk_buffer_attached = false;
    }
}

fn nullfs_proxy_status_is_known(status: i32) -> bool {
    matches!(
        status,
        filesystem_protocol::status::OK
            | filesystem_protocol::status::INVALID
            | filesystem_protocol::status::NOT_FOUND
            | filesystem_protocol::status::NOT_DIRECTORY
            | filesystem_protocol::status::IS_DIRECTORY
            | filesystem_protocol::status::EXISTS
            | filesystem_protocol::status::PERMISSION
            | filesystem_protocol::status::NO_SPACE
            | filesystem_protocol::status::RANGE
            | filesystem_protocol::status::STALE_SESSION
            | filesystem_protocol::status::STALE_NODE
            | filesystem_protocol::status::STALE_BUFFER
            | filesystem_protocol::status::TRY_AGAIN
            | filesystem_protocol::status::IO
            | filesystem_protocol::status::NOT_SUPPORTED
            | filesystem_protocol::status::CANCELLED
            | filesystem_protocol::status::NOT_EMPTY
            | filesystem_protocol::status::WOULD_CYCLE
            | filesystem_protocol::status::OUTCOME_UNKNOWN
    )
}

fn nullfs_proxy_status_errno(status: i32) -> i64 {
    match status {
        filesystem_protocol::status::INVALID => ERR_INVALID_ARGUMENT,
        filesystem_protocol::status::NOT_FOUND => ERR_NO_ENTRY,
        filesystem_protocol::status::NOT_DIRECTORY => abi::errno::NOT_DIRECTORY,
        filesystem_protocol::status::IS_DIRECTORY => ERR_IS_DIRECTORY,
        filesystem_protocol::status::PERMISSION => ERR_READ_ONLY,
        filesystem_protocol::status::NO_SPACE => ERR_NO_SPACE,
        filesystem_protocol::status::RANGE => abi::errno::RANGE,
        filesystem_protocol::status::TRY_AGAIN => ERR_TRY_AGAIN,
        filesystem_protocol::status::OUTCOME_UNKNOWN => ERR_IO,
        _ => ERR_IO,
    }
}

fn nullfs_proxy_finish_process(
    request_id: u64,
    process_id: u64,
    stack_pointer: usize,
    result: u64,
) -> Result<(), Error> {
    nullfs_proxy_finish_owned_process(process_id, stack_pointer, result, request_id)
}

fn nullfs_proxy_finish_owned_process(
    process_id: u64,
    stack_pointer: usize,
    result: u64,
    request_id: u64,
) -> Result<(), Error> {
    let wrote_result = scheduler::with_process_address_space(process_id, || {
        let registers = unsafe { &mut *(stack_pointer as *mut SavedRegisters) };
        registers.rax = result;
    })
    .is_some();
    nullfs_proxy_release_request(request_id);
    if !wrote_result {
        return Ok(());
    }
    let made_runnable = {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            return Ok(());
        };
        process.make_runnable();
        true
    };
    if made_runnable && !scheduler::wake_process(process_id) {
        return Err(Error::ProcessNotFound(process_id));
    }
    Ok(())
}

fn nullfs_proxy_continue_executable(
    process_id: u64,
    previous_request_id: u64,
    request: filesystem_protocol::Request,
    operation: PendingNullfsProxyOperation,
    owner: ExecutableLoadOwner,
    stack_pointer: usize,
) -> Result<(), Error> {
    match nullfs_proxy_submit_request(
        process_id,
        request,
        operation.clone(),
        stack_pointer,
        Some(previous_request_id),
        None,
    ) {
        Ok(()) => Ok(()),
        Err(error) if error == ERR_TRY_AGAIN => {
            executable_load_set_retry(process_id, owner, stack_pointer, request, operation);
            nullfs_proxy_release_request(previous_request_id);
            Ok(())
        }
        Err(error) => {
            executable_load_fail(process_id, owner, stack_pointer, error);
            nullfs_proxy_release_request(previous_request_id);
            Ok(())
        }
    }
}

fn nullfs_proxy_continue(
    process_id: u64,
    previous_request_id: u64,
    request: filesystem_protocol::Request,
    operation: PendingNullfsProxyOperation,
    stack_pointer: usize,
) -> Result<(), Error> {
    match nullfs_proxy_submit_request(
        process_id,
        request,
        operation,
        stack_pointer,
        Some(previous_request_id),
        None,
    ) {
        Ok(()) => Ok(()),
        Err(error) => nullfs_proxy_finish_owned_process(
            process_id,
            stack_pointer,
            error_return(error),
            previous_request_id,
        ),
    }
}

fn nullfs_proxy_continue_path(
    process_id: u64,
    previous_request_id: u64,
    request: filesystem_protocol::Request,
    operation: PendingNullfsProxyOperation,
    executable_owner: Option<ExecutableLoadOwner>,
    stack_pointer: usize,
) -> Result<(), Error> {
    if let Some(owner) = executable_owner {
        nullfs_proxy_continue_executable(
            process_id,
            previous_request_id,
            request,
            operation,
            owner,
            stack_pointer,
        )
    } else {
        nullfs_proxy_continue(
            process_id,
            previous_request_id,
            request,
            operation,
            stack_pointer,
        )
    }
}

fn nullfs_proxy_finish_path_error(
    request_id: u64,
    process_id: u64,
    stack_pointer: usize,
    executable_owner: Option<ExecutableLoadOwner>,
    error: i64,
) -> Result<(), Error> {
    if let Some(owner) = executable_owner {
        executable_load_fail(process_id, owner, stack_pointer, error);
        nullfs_proxy_release_request(request_id);
        Ok(())
    } else {
        nullfs_proxy_finish_process(request_id, process_id, stack_pointer, error_return(error))
    }
}

fn vfs_route_generation_is_current(generation: u32) -> bool {
    let state = *VFS_ROUTE.lock();
    state.ready && state.generation == generation
}

fn nullfs_proxy_components(path: &str) -> Result<Vec<String>, i64> {
    if path == "/" {
        return Ok(Vec::new());
    }
    let Some(relative) = path.strip_prefix('/') else {
        return Err(ERR_INVALID_ARGUMENT);
    };
    if relative.is_empty() {
        return Err(ERR_INVALID_ARGUMENT);
    }
    let mut components = Vec::new();
    for component in relative.split('/') {
        if component.is_empty()
            || component.len() > filesystem_protocol::MAX_NAME_BYTES
            || component.as_bytes().contains(&0)
        {
            return Err(if component.len() > filesystem_protocol::MAX_NAME_BYTES {
                abi::errno::NAME_TOO_LONG
            } else {
                ERR_INVALID_ARGUMENT
            });
        }
        components.push(String::from(component));
    }
    Ok(components)
}

fn nullfs_proxy_path_flags(path: &str) -> u64 {
    if vfs_path_has_prefix(path, "/System") {
        abi::file::FLAG_SYSTEM | abi::file::FLAG_READ_ONLY
    } else if path
        .strip_prefix(NULLFS_MOUNT_PATH)
        .is_some_and(|suffix| vfs_path_has_prefix(suffix, "/System"))
    {
        abi::file::FLAG_READ_ONLY
    } else {
        0
    }
}

fn nullfs_proxy_path_is_read_only(backend_path: &str) -> bool {
    vfs_path_has_prefix(backend_path, "/System")
}

fn nullfs_proxy_lookup_request(parent: u64, name: &str) -> filesystem_protocol::Request {
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::LOOKUP;
    request.node_id = parent;
    request.name_length = name.len() as u16;
    request.name[..name.len()].copy_from_slice(name.as_bytes());
    request
}

fn nullfs_proxy_directory_request(node_id: u64, cookie: u64) -> filesystem_protocol::Request {
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::READ_DIRECTORY;
    request.node_id = node_id;
    request.file_offset = cookie;
    request.bulk = filesystem_protocol::BulkBuffer {
        buffer_id: 1,
        offset: 0,
        length: FILESYSTEM_PROXY_BULK_BYTES as u64,
    };
    request
}

fn nullfs_proxy_named_operation(
    path: &str,
    parent: u64,
    name: &str,
    purpose: NullfsPathPurpose,
) -> Result<(filesystem_protocol::Request, PendingNullfsProxyOperation), i64> {
    let mut request = filesystem_protocol::Request::EMPTY;
    request.node_id = parent;
    request.name_length = name.len() as u16;
    request.name[..name.len()].copy_from_slice(name.as_bytes());
    let operation = match purpose {
        NullfsPathPurpose::Open {
            options,
            descriptor,
            close_on_exec,
        } => {
            request.operation = filesystem_protocol::operation::OPEN;
            if options.read {
                request.flags |= filesystem_protocol::request_flags::READ;
            }
            if options.write {
                request.flags |= filesystem_protocol::request_flags::WRITE;
            }
            if options.create {
                request.flags |= filesystem_protocol::request_flags::CREATE;
            }
            if options.truncate {
                request.flags |= filesystem_protocol::request_flags::TRUNCATE;
            }
            if options.append {
                request.flags |= filesystem_protocol::request_flags::APPEND;
            }
            PendingNullfsProxyOperation::Open {
                path: String::from(path),
                descriptor,
                readable: options.read,
                writable: options.write,
                append: options.append,
                close_on_exec,
                generation: 0,
                session_id: filesystem_protocol::INVALID_ID,
                session_generation: 0,
            }
        }
        NullfsPathPurpose::LoadExecutable {
            owner,
            vfs_generation,
        } => {
            request.operation = filesystem_protocol::operation::OPEN;
            request.flags = filesystem_protocol::request_flags::READ;
            PendingNullfsProxyOperation::LoadExecutableOpen {
                owner,
                vfs_generation,
                generation: 0,
                session_id: filesystem_protocol::INVALID_ID,
                session_generation: 0,
            }
        }
        NullfsPathPurpose::Unlink => {
            request.operation = filesystem_protocol::operation::UNLINK;
            PendingNullfsProxyOperation::Unlink
        }
        _ => return Err(ERR_IO),
    };
    Ok((request, operation))
}

fn nullfs_proxy_resolves_parent(purpose: &NullfsPathPurpose) -> bool {
    matches!(
        purpose,
        NullfsPathPurpose::Open { .. }
            | NullfsPathPurpose::LoadExecutable { .. }
            | NullfsPathPurpose::Unlink
    )
}

fn nullfs_proxy_start_path(
    process_id: u64,
    path: &str,
    backend_path: &str,
    purpose: NullfsPathPurpose,
    stack_pointer: usize,
) -> ControlOutcome {
    if nullfs_proxy_state().is_none() {
        return ControlOutcome::Ready(error_return(ERR_IO));
    }
    let components = match nullfs_proxy_components(backend_path) {
        Ok(components) => components,
        Err(error) => return ControlOutcome::Ready(error_return(error)),
    };
    if components.is_empty() {
        return match purpose {
            NullfsPathPurpose::Stat { address, length } => {
                ControlOutcome::Ready(nullfs_proxy_write_stat(
                    process_id,
                    address,
                    length,
                    abi::file::Stat {
                        kind: abi::file::KIND_DIRECTORY,
                        size: 0,
                        flags: 0,
                    },
                ))
            }
            NullfsPathPurpose::Open { .. }
            | NullfsPathPurpose::LoadExecutable { .. }
            | NullfsPathPurpose::Unlink => ControlOutcome::Ready(error_return(ERR_IS_DIRECTORY)),
            NullfsPathPurpose::Chdir => {
                ControlOutcome::Ready(match platform_set_working_directory(process_id, path) {
                    Ok(()) => 0,
                    Err(error) => error_return(error),
                })
            }
            NullfsPathPurpose::ReadDirectory {
                start_index,
                records_address,
                capacity,
            } => {
                let request = nullfs_proxy_directory_request(filesystem_protocol::ROOT_NODE_ID, 0);
                let operation = PendingNullfsProxyOperation::ReadDirectory {
                    node_id: filesystem_protocol::ROOT_NODE_ID,
                    start_index,
                    seen: 0,
                    records_address,
                    capacity,
                    records: Vec::new(),
                    cookie: 0,
                    flags: nullfs_proxy_path_flags(path),
                };
                match nullfs_proxy_begin_request(process_id, request, operation, stack_pointer) {
                    Ok(()) => ControlOutcome::Blocked,
                    Err(error) => ControlOutcome::Ready(error_return(error)),
                }
            }
        };
    }
    let named_root_operation = nullfs_proxy_resolves_parent(&purpose) && components.len() == 1;
    let (request, operation) = if named_root_operation {
        match nullfs_proxy_named_operation(
            path,
            filesystem_protocol::ROOT_NODE_ID,
            &components[0],
            purpose,
        ) {
            Ok(operation) => operation,
            Err(error) => return ControlOutcome::Ready(error_return(error)),
        }
    } else {
        (
            nullfs_proxy_lookup_request(filesystem_protocol::ROOT_NODE_ID, &components[0]),
            PendingNullfsProxyOperation::Lookup {
                path: String::from(path),
                components,
                next_component: 1,
                purpose,
            },
        )
    };
    match nullfs_proxy_begin_request(process_id, request, operation, stack_pointer) {
        Ok(()) => ControlOutcome::Blocked,
        Err(error) => ControlOutcome::Ready(error_return(error)),
    }
}

fn nullfs_proxy_stat(
    process_id: u64,
    path: &str,
    backend_path: &str,
    address: u64,
    length: u64,
    stack_pointer: usize,
) -> ControlOutcome {
    nullfs_proxy_start_path(
        process_id,
        path,
        backend_path,
        NullfsPathPurpose::Stat { address, length },
        stack_pointer,
    )
}

fn nullfs_proxy_open(
    process_id: u64,
    path: &str,
    backend_path: &str,
    options: vfs::OpenOptions,
    close_on_exec: bool,
    descriptor: u64,
    stack_pointer: usize,
) -> ControlOutcome {
    if nullfs_proxy_path_is_read_only(backend_path)
        && (options.write || options.create || options.truncate || options.append)
    {
        return ControlOutcome::Ready(error_return(ERR_READ_ONLY));
    }
    nullfs_proxy_start_path(
        process_id,
        path,
        backend_path,
        NullfsPathPurpose::Open {
            options,
            descriptor,
            close_on_exec,
        },
        stack_pointer,
    )
}

fn nullfs_proxy_read_directory(
    process_id: u64,
    path: &str,
    backend_path: &str,
    start_index: usize,
    records_address: u64,
    capacity: usize,
    stack_pointer: usize,
) -> ControlOutcome {
    nullfs_proxy_start_path(
        process_id,
        path,
        backend_path,
        NullfsPathPurpose::ReadDirectory {
            start_index,
            records_address,
            capacity,
        },
        stack_pointer,
    )
}

fn nullfs_proxy_chdir(
    process_id: u64,
    path: &str,
    backend_path: &str,
    stack_pointer: usize,
) -> ControlOutcome {
    nullfs_proxy_start_path(
        process_id,
        path,
        backend_path,
        NullfsPathPurpose::Chdir,
        stack_pointer,
    )
}

fn nullfs_proxy_unlink(
    process_id: u64,
    path: &str,
    backend_path: &str,
    stack_pointer: usize,
) -> ControlOutcome {
    if nullfs_proxy_path_is_read_only(backend_path) {
        return ControlOutcome::Ready(error_return(ERR_READ_ONLY));
    }
    nullfs_proxy_start_path(
        process_id,
        path,
        backend_path,
        NullfsPathPurpose::Unlink,
        stack_pointer,
    )
}

fn nullfs_proxy_read(
    process_id: u64,
    handle: OpenFileHandle,
    address: u64,
    length: usize,
    stack_pointer: usize,
) -> ReadOutcome {
    let (offset, generation, session_id, session_generation, node_id) = {
        let file = handle.lock();
        if !file.readable {
            return ReadOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
        }
        let OpenFileBackend::NullfsProxy {
            generation,
            session_id,
            session_generation,
            node_id,
        } = file.backend
        else {
            return ReadOutcome::Ready(error_return(ERR_IO));
        };
        (
            file.offset,
            generation,
            session_id,
            session_generation,
            node_id,
        )
    };
    if nullfs_proxy_state().is_none_or(|state| {
        state.generation != generation
            || state.session_id != session_id
            || state.session_generation != session_generation
    }) {
        return ReadOutcome::Ready(error_return(ERR_IO));
    }
    let count = length.min(FILESYSTEM_PROXY_BULK_BYTES);
    if count == 0 {
        return ReadOutcome::Ready(0);
    }
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::READ;
    request.node_id = node_id;
    request.file_offset = offset;
    request.bulk = filesystem_protocol::BulkBuffer {
        buffer_id: 1,
        offset: 0,
        length: count as u64,
    };
    let operation = PendingNullfsProxyOperation::Read {
        handle,
        address,
        initial_offset: offset,
        length: count,
    };
    match nullfs_proxy_begin_request(process_id, request, operation, stack_pointer) {
        Ok(()) => ReadOutcome::Blocked,
        Err(error) => ReadOutcome::Ready(error_return(error)),
    }
}

fn nullfs_proxy_write(
    process_id: u64,
    handle: OpenFileHandle,
    bytes: &[u8],
    stack_pointer: usize,
) -> WriteOutcome {
    let (offset, initial_offset, generation, session_id, session_generation, node_id, append) = {
        let file = handle.lock();
        if !file.writable {
            return WriteOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
        }
        let OpenFileBackend::NullfsProxy {
            generation,
            session_id,
            session_generation,
            node_id,
        } = file.backend
        else {
            return WriteOutcome::Ready(error_return(ERR_IO));
        };
        (
            if file.append {
                open_file_size(&file)
            } else {
                file.offset
            },
            file.offset,
            generation,
            session_id,
            session_generation,
            node_id,
            file.append,
        )
    };
    if nullfs_proxy_state().is_none_or(|state| {
        state.generation != generation
            || state.session_id != session_id
            || state.session_generation != session_generation
    }) {
        return WriteOutcome::Ready(error_return(ERR_IO));
    }
    let count = bytes.len().min(FILESYSTEM_PROXY_BULK_BYTES);
    if count == 0 {
        return WriteOutcome::Ready(0);
    }
    let mut request = filesystem_protocol::Request::EMPTY;
    request.operation = filesystem_protocol::operation::WRITE;
    request.flags = if append {
        filesystem_protocol::request_flags::APPEND
    } else {
        0
    };
    request.node_id = node_id;
    request.file_offset = offset;
    request.bulk = filesystem_protocol::BulkBuffer {
        buffer_id: 1,
        offset: 0,
        length: count as u64,
    };
    let operation = PendingNullfsProxyOperation::Write {
        handle,
        offset,
        initial_offset,
        append,
        length: count,
    };
    match nullfs_proxy_submit_request(
        process_id,
        request,
        operation,
        stack_pointer,
        None,
        Some(&bytes[..count]),
    ) {
        Ok(()) => WriteOutcome::Blocked,
        Err(error) => WriteOutcome::Ready(error_return(error)),
    }
}

fn nullfs_proxy_write_stat(
    process_id: u64,
    address: u64,
    length: u64,
    stat: abi::file::Stat,
) -> u64 {
    let Ok(length) = usize::try_from(length) else {
        return error_return(abi::errno::RANGE);
    };
    if length < size_of::<abi::file::Stat>() {
        return error_return(abi::errno::RANGE);
    }
    if !user_range_allows(process_id, address, size_of::<abi::file::Stat>(), true) {
        return error_return(ERR_BAD_ADDRESS);
    }
    scheduler::with_process_address_space(process_id, || unsafe {
        ptr::write_unaligned(address as *mut abi::file::Stat, stat);
        0
    })
    .unwrap_or_else(|| error_return(ERR_NO_PROCESS))
}

fn nullfs_proxy_node_kind(kind: u16) -> Option<u64> {
    match kind {
        filesystem_protocol::node_kind::FILE => Some(abi::file::KIND_FILE),
        filesystem_protocol::node_kind::DIRECTORY => Some(abi::file::KIND_DIRECTORY),
        _ => None,
    }
}

fn nullfs_proxy_complete_success(
    process_id: u64,
    pending: PendingNullfsProxyRequest,
    reply: filesystem_protocol::Reply,
    physical_memory_offset: VirtAddr,
) -> Result<(), Error> {
    let stack_pointer = pending.stack_pointer;
    let request_id = pending.request_id;
    match pending.operation {
        PendingNullfsProxyOperation::Lookup {
            path,
            components,
            next_component,
            purpose,
        } => {
            let executable_owner = match &purpose {
                NullfsPathPurpose::LoadExecutable { owner, .. } => Some(*owner),
                _ => None,
            };
            if next_component > components.len() {
                return nullfs_proxy_finish_path_error(
                    request_id,
                    process_id,
                    stack_pointer,
                    executable_owner,
                    ERR_IO,
                );
            }
            if nullfs_proxy_resolves_parent(&purpose)
                && next_component == components.len().saturating_sub(1)
            {
                if reply.node_kind != filesystem_protocol::node_kind::DIRECTORY {
                    return nullfs_proxy_finish_path_error(
                        request_id,
                        process_id,
                        stack_pointer,
                        executable_owner,
                        abi::errno::NOT_DIRECTORY,
                    );
                }
                let (request, operation) = match nullfs_proxy_named_operation(
                    &path,
                    reply.node_id,
                    &components[next_component],
                    purpose,
                ) {
                    Ok(operation) => operation,
                    Err(error) => {
                        return nullfs_proxy_finish_path_error(
                            request_id,
                            process_id,
                            stack_pointer,
                            executable_owner,
                            error,
                        );
                    }
                };
                return nullfs_proxy_continue_path(
                    process_id,
                    request_id,
                    request,
                    operation,
                    executable_owner,
                    stack_pointer,
                );
            }
            if next_component < components.len() {
                if reply.node_kind != filesystem_protocol::node_kind::DIRECTORY {
                    return nullfs_proxy_finish_path_error(
                        request_id,
                        process_id,
                        stack_pointer,
                        executable_owner,
                        abi::errno::NOT_DIRECTORY,
                    );
                }
                let request =
                    nullfs_proxy_lookup_request(reply.node_id, &components[next_component]);
                return nullfs_proxy_continue_path(
                    process_id,
                    request_id,
                    request,
                    PendingNullfsProxyOperation::Lookup {
                        path,
                        components,
                        next_component: next_component + 1,
                        purpose,
                    },
                    executable_owner,
                    stack_pointer,
                );
            }
            match purpose {
                NullfsPathPurpose::Stat { address, length } => {
                    let Some(kind) = nullfs_proxy_node_kind(reply.node_kind) else {
                        return nullfs_proxy_finish_process(
                            request_id,
                            process_id,
                            stack_pointer,
                            error_return(ERR_IO),
                        );
                    };
                    let result = nullfs_proxy_write_stat(
                        process_id,
                        address,
                        length,
                        abi::file::Stat {
                            kind,
                            size: reply.value,
                            flags: nullfs_proxy_path_flags(&path),
                        },
                    );
                    nullfs_proxy_finish_process(request_id, process_id, stack_pointer, result)
                }
                NullfsPathPurpose::LoadExecutable { owner, .. } => nullfs_proxy_finish_path_error(
                    request_id,
                    process_id,
                    stack_pointer,
                    Some(owner),
                    ERR_IO,
                ),
                NullfsPathPurpose::Open { .. } | NullfsPathPurpose::Unlink => {
                    nullfs_proxy_finish_process(
                        request_id,
                        process_id,
                        stack_pointer,
                        error_return(ERR_IO),
                    )
                }
                NullfsPathPurpose::ReadDirectory {
                    start_index,
                    records_address,
                    capacity,
                } => {
                    if reply.node_kind != filesystem_protocol::node_kind::DIRECTORY {
                        return nullfs_proxy_finish_process(
                            request_id,
                            process_id,
                            stack_pointer,
                            error_return(abi::errno::NOT_DIRECTORY),
                        );
                    }
                    let request = nullfs_proxy_directory_request(reply.node_id, 0);
                    nullfs_proxy_continue(
                        process_id,
                        request_id,
                        request,
                        PendingNullfsProxyOperation::ReadDirectory {
                            node_id: reply.node_id,
                            start_index,
                            seen: 0,
                            records_address,
                            capacity,
                            records: Vec::new(),
                            cookie: 0,
                            flags: nullfs_proxy_path_flags(&path),
                        },
                        stack_pointer,
                    )
                }
                NullfsPathPurpose::Chdir => {
                    if reply.node_kind != filesystem_protocol::node_kind::DIRECTORY {
                        return nullfs_proxy_finish_process(
                            request_id,
                            process_id,
                            stack_pointer,
                            error_return(abi::errno::NOT_DIRECTORY),
                        );
                    }
                    let result = match platform_set_working_directory(process_id, &path) {
                        Ok(()) => 0,
                        Err(error) => error_return(error),
                    };
                    nullfs_proxy_finish_process(request_id, process_id, stack_pointer, result)
                }
            }
        }
        PendingNullfsProxyOperation::Open {
            path,
            descriptor,
            readable,
            writable,
            append,
            close_on_exec,
            generation,
            session_id,
            session_generation,
        } => {
            if reply.node_kind == filesystem_protocol::node_kind::DIRECTORY {
                nullfs_proxy_enqueue_close(
                    generation,
                    session_id,
                    session_generation,
                    reply.node_id,
                );
                return nullfs_proxy_finish_process(
                    request_id,
                    process_id,
                    stack_pointer,
                    error_return(ERR_IS_DIRECTORY),
                );
            }
            if reply.node_kind != filesystem_protocol::node_kind::FILE {
                return nullfs_proxy_finish_process(
                    request_id,
                    process_id,
                    stack_pointer,
                    error_return(ERR_IO),
                );
            }
            let shared_size = nullfs_proxy_node_size(
                generation,
                session_id,
                session_generation,
                reply.node_id,
                reply.value,
            );
            let handle = Arc::new(PreemptMutex::new(OpenFileState {
                path,
                offset: if append { reply.value } else { 0 },
                readable,
                writable,
                append,
                size: reply.value,
                nullfs_size: Some(shared_size),
                backend: OpenFileBackend::NullfsProxy {
                    generation,
                    session_id,
                    session_generation,
                    node_id: reply.node_id,
                },
            }));
            let result = {
                let mut manager = PROCESS_MANAGER.lock();
                let Some(process) = manager.process_mut(process_id) else {
                    drop(manager);
                    drop(handle);
                    return Ok(());
                };
                if descriptor_in_use(process, descriptor)
                    || descriptor_count(process) >= MAX_OPEN_FILES
                {
                    drop(manager);
                    drop(handle);
                    error_return(ERR_TOO_MANY_OPEN_FILES)
                } else {
                    process.open_files.push(OpenFile {
                        descriptor,
                        handle,
                        close_on_exec,
                    });
                    process.open_count = process.open_count.saturating_add(1);
                    descriptor
                }
            };
            nullfs_proxy_finish_process(request_id, process_id, stack_pointer, result)
        }
        PendingNullfsProxyOperation::LoadExecutableOpen {
            owner,
            vfs_generation,
            generation,
            session_id,
            session_generation,
        } => {
            if !vfs_route_generation_is_current(vfs_generation)
                || !nullfs_proxy_backend_is_current(generation, session_id, session_generation)
            {
                nullfs_proxy_enqueue_close(
                    generation,
                    session_id,
                    session_generation,
                    reply.node_id,
                );
                executable_load_fail(process_id, owner, stack_pointer, ERR_IO);
                return Ok(());
            }
            if reply.node_kind != filesystem_protocol::node_kind::FILE {
                nullfs_proxy_enqueue_close(
                    generation,
                    session_id,
                    session_generation,
                    reply.node_id,
                );
                executable_load_fail(
                    process_id,
                    owner,
                    stack_pointer,
                    if reply.node_kind == filesystem_protocol::node_kind::DIRECTORY {
                        ERR_IS_DIRECTORY
                    } else {
                        ERR_IO
                    },
                );
                return Ok(());
            }
            let expected_size = match usize::try_from(reply.value) {
                Ok(size) if size <= elf::MAX_EXECUTABLE_FILE_BYTES => size,
                _ => {
                    nullfs_proxy_enqueue_close(
                        generation,
                        session_id,
                        session_generation,
                        reply.node_id,
                    );
                    executable_load_fail(process_id, owner, stack_pointer, ERR_IO);
                    return Ok(());
                }
            };

            let bytes = vec![0_u8; expected_size];
            let initialized = {
                let mut manager = PROCESS_MANAGER.lock();
                if let Some(load) = manager
                    .process_mut(process_id)
                    .and_then(|process| process.pending_executable_load.as_mut())
                    && load.owner == owner
                    && load.stack_pointer == stack_pointer
                    && load.vfs_generation == vfs_generation
                    && load.result.is_none()
                {
                    load.provider_generation = generation;
                    load.session_id = session_id;
                    load.session_generation = session_generation;
                    load.node_id = reply.node_id;
                    load.expected_size = expected_size;
                    load.bytes = bytes;
                    true
                } else {
                    false
                }
            };
            if !initialized {
                nullfs_proxy_enqueue_close(
                    generation,
                    session_id,
                    session_generation,
                    reply.node_id,
                );
                executable_load_fail(process_id, owner, stack_pointer, ERR_IO);
                return Ok(());
            }
            if expected_size == 0 {
                executable_load_fail(process_id, owner, stack_pointer, ERR_IO);
                return Ok(());
            }
            let length = expected_size.min(FILESYSTEM_PROXY_BULK_BYTES);
            let mut request = filesystem_protocol::Request::EMPTY;
            request.operation = filesystem_protocol::operation::READ;
            request.node_id = reply.node_id;
            request.file_offset = 0;
            request.bulk = filesystem_protocol::BulkBuffer {
                buffer_id: 1,
                offset: 0,
                length: length as u64,
            };
            nullfs_proxy_continue_executable(
                process_id,
                request_id,
                request,
                PendingNullfsProxyOperation::LoadExecutableRead {
                    owner,
                    vfs_generation,
                    generation,
                    session_id,
                    session_generation,
                    node_id: reply.node_id,
                    offset: 0,
                    length,
                },
                owner,
                stack_pointer,
            )
        }
        PendingNullfsProxyOperation::LoadExecutableRead {
            owner,
            vfs_generation,
            generation,
            session_id,
            session_generation,
            node_id,
            offset,
            length,
        } => {
            let count = match usize::try_from(reply.value) {
                Ok(count) if count != 0 && count <= length => count,
                _ => {
                    executable_load_fail(process_id, owner, stack_pointer, ERR_IO);
                    return Ok(());
                }
            };
            if !vfs_route_generation_is_current(vfs_generation)
                || !nullfs_proxy_backend_is_current(generation, session_id, session_generation)
            {
                executable_load_fail(process_id, owner, stack_pointer, ERR_IO);
                return Ok(());
            }
            let mut chunk = vec![0_u8; count];
            if nullfs_proxy_bulk_read_at(0, &mut chunk).is_err() {
                executable_load_fail(process_id, owner, stack_pointer, ERR_IO);
                return Ok(());
            }
            let next = {
                let mut manager = PROCESS_MANAGER.lock();
                let Some(process) = manager.process_mut(process_id) else {
                    return Ok(());
                };
                let Some(load) = process.pending_executable_load.as_mut() else {
                    return Ok(());
                };
                match offset.checked_add(count) {
                    Some(end)
                        if load.owner == owner
                            && load.stack_pointer == stack_pointer
                            && load.vfs_generation == vfs_generation
                            && load.provider_generation == generation
                            && load.session_id == session_id
                            && load.session_generation == session_generation
                            && load.node_id == node_id
                            && end <= load.expected_size
                            && load.result.is_none() =>
                    {
                        load.bytes[offset..end].copy_from_slice(&chunk);
                        if end == load.expected_size {
                            load.node_id = filesystem_protocol::INVALID_ID;
                            Some((
                                end,
                                Some((load.path.clone(), core::mem::take(&mut load.bytes))),
                            ))
                        } else {
                            Some((end, None))
                        }
                    }
                    Some(_) | None => None,
                }
            };
            let Some((next_offset, completed)) = next else {
                executable_load_fail(process_id, owner, stack_pointer, ERR_IO);
                return Ok(());
            };
            if let Some((path, bytes)) = completed {
                nullfs_proxy_enqueue_close(generation, session_id, session_generation, node_id);
                let loaded = LoadedExecutable::from_bytes(&path, bytes).map_err(|error| {
                    crate::serial_println!(
                        "userspace executable validation failed: pid={}, path={}, error={}",
                        process_id,
                        path,
                        error
                    );
                    ERR_IO
                });
                if loaded.is_ok() {
                    crate::serial_println!(
                        "userspace executable materialized: pid={}, path={}, bytes={}, generation={}",
                        process_id,
                        path,
                        next_offset,
                        generation
                    );
                }
                let mut manager = PROCESS_MANAGER.lock();
                if let Some(process) = manager.process_mut(process_id)
                    && let Some(load) = process.pending_executable_load.as_mut()
                    && load.owner == owner
                    && load.stack_pointer == stack_pointer
                    && load.vfs_generation == vfs_generation
                    && load.result.is_none()
                {
                    load.result = Some(loaded);
                }
                return Ok(());
            }
            let remaining = {
                let manager = PROCESS_MANAGER.lock();
                manager
                    .processes
                    .iter()
                    .find(|process| process.process_id == process_id)
                    .and_then(|process| process.pending_executable_load.as_ref())
                    .map(|load| load.expected_size.saturating_sub(next_offset))
                    .unwrap_or(0)
            };
            if remaining == 0 {
                executable_load_fail(process_id, owner, stack_pointer, ERR_IO);
                return Ok(());
            }
            let next_length = remaining.min(FILESYSTEM_PROXY_BULK_BYTES);
            let mut request = filesystem_protocol::Request::EMPTY;
            request.operation = filesystem_protocol::operation::READ;
            request.node_id = node_id;
            request.file_offset = next_offset as u64;
            request.bulk = filesystem_protocol::BulkBuffer {
                buffer_id: 1,
                offset: 0,
                length: next_length as u64,
            };
            nullfs_proxy_continue_executable(
                process_id,
                request_id,
                request,
                PendingNullfsProxyOperation::LoadExecutableRead {
                    owner,
                    vfs_generation,
                    generation,
                    session_id,
                    session_generation,
                    node_id,
                    offset: next_offset,
                    length: next_length,
                },
                owner,
                stack_pointer,
            )
        }
        PendingNullfsProxyOperation::Read {
            handle,
            address,
            initial_offset,
            length,
        } => {
            let count = match usize::try_from(reply.value) {
                Ok(count) if count <= length => count,
                _ => {
                    return nullfs_proxy_finish_process(
                        request_id,
                        process_id,
                        stack_pointer,
                        error_return(ERR_IO),
                    );
                }
            };
            let mut bytes = vec![0_u8; count];
            if count != 0 && nullfs_proxy_bulk_read_at(0, &mut bytes).is_err() {
                return nullfs_proxy_finish_process(
                    request_id,
                    process_id,
                    stack_pointer,
                    error_return(ERR_IO),
                );
            }
            let copied = {
                let mut manager = PROCESS_MANAGER.lock();
                let Some(process) = manager.process_mut(process_id) else {
                    return Ok(());
                };
                let copied = count == 0
                    || write_user_bytes(address, &bytes, physical_memory_offset, &process.pages)
                        .is_ok();
                if copied {
                    process.read_count = process.read_count.saturating_add(1);
                    process.bytes_read = process.bytes_read.saturating_add(count as u64);
                }
                copied
            };
            if !copied {
                return nullfs_proxy_finish_process(
                    request_id,
                    process_id,
                    stack_pointer,
                    error_return(ERR_IO),
                );
            }
            let mut file = handle.lock();
            if file.offset == initial_offset {
                file.offset = initial_offset.saturating_add(count as u64);
            }
            drop(file);
            nullfs_proxy_finish_process(request_id, process_id, stack_pointer, count as u64)
        }
        PendingNullfsProxyOperation::Write {
            handle,
            initial_offset,
            length,
            ..
        } => {
            let count = match usize::try_from(reply.value) {
                Ok(count) if count <= length => count,
                _ => {
                    return nullfs_proxy_finish_process(
                        request_id,
                        process_id,
                        stack_pointer,
                        error_return(ERR_IO),
                    );
                }
            };
            let Some(resulting_offset) = filesystem_protocol::decode_write_reply_offset(&reply)
            else {
                return nullfs_proxy_finish_process(
                    request_id,
                    process_id,
                    stack_pointer,
                    error_return(ERR_IO),
                );
            };
            {
                let mut file = handle.lock();
                let size = open_file_size(&file).max(resulting_offset);
                update_open_file_size(&mut file, size);
                if file.offset == initial_offset {
                    file.offset = resulting_offset;
                }
            }
            {
                let mut manager = PROCESS_MANAGER.lock();
                if let Some(process) = manager.process_mut(process_id) {
                    process.write_count = process.write_count.saturating_add(1);
                    process.bytes_written = process.bytes_written.saturating_add(count as u64);
                    process.file_write_count = process.file_write_count.saturating_add(1);
                    process.file_bytes_written =
                        process.file_bytes_written.saturating_add(count as u64);
                }
            }
            nullfs_proxy_finish_process(request_id, process_id, stack_pointer, count as u64)
        }
        PendingNullfsProxyOperation::Unlink => {
            nullfs_proxy_finish_process(request_id, process_id, stack_pointer, 0)
        }
        PendingNullfsProxyOperation::ReadDirectory {
            node_id,
            start_index,
            mut seen,
            records_address,
            capacity,
            mut records,
            cookie,
            flags,
        } => {
            let entry_size = size_of::<filesystem_protocol::DirectoryEntry>();
            let maximum = FILESYSTEM_PROXY_BULK_BYTES / entry_size;
            let count = match usize::try_from(reply.value) {
                Ok(count) if count <= maximum => count,
                _ => {
                    return nullfs_proxy_finish_process(
                        request_id,
                        process_id,
                        stack_pointer,
                        error_return(ERR_IO),
                    );
                }
            };
            let mut previous_cookie = cookie;
            for index in 0..count {
                let mut bytes = [0_u8; size_of::<filesystem_protocol::DirectoryEntry>()];
                if nullfs_proxy_bulk_read_at(index * entry_size, &mut bytes).is_err() {
                    return nullfs_proxy_finish_process(
                        request_id,
                        process_id,
                        stack_pointer,
                        error_return(ERR_IO),
                    );
                }
                let entry = unsafe {
                    ptr::read_unaligned(bytes.as_ptr() as *const filesystem_protocol::DirectoryEntry)
                };
                let name_length = usize::from(entry.name_length);
                let Some(kind) = nullfs_proxy_node_kind(entry.kind) else {
                    return nullfs_proxy_finish_process(
                        request_id,
                        process_id,
                        stack_pointer,
                        error_return(ERR_IO),
                    );
                };
                if entry.node_id == filesystem_protocol::INVALID_ID
                    || entry.next_cookie <= previous_cookie
                    || entry.reserved != 0
                    || name_length == 0
                    || name_length > entry.name.len()
                    || entry.name[..name_length].contains(&b'/')
                    || entry.name[..name_length].contains(&0)
                    || entry.name[name_length..].iter().any(|byte| *byte != 0)
                    || name_length > abi::file::MAX_DIRECTORY_ENTRY_NAME_BYTES
                {
                    return nullfs_proxy_finish_process(
                        request_id,
                        process_id,
                        stack_pointer,
                        error_return(ERR_IO),
                    );
                }
                previous_cookie = entry.next_cookie;
                if seen >= start_index && records.len() < capacity {
                    let mut record = abi::file::DirectoryEntry {
                        kind,
                        size: 0,
                        flags,
                        name_length: name_length as u64,
                        name: [0; abi::file::DIRECTORY_ENTRY_NAME_CAPACITY],
                    };
                    record.name[..name_length].copy_from_slice(&entry.name[..name_length]);
                    records.push(record);
                }
                seen = seen.saturating_add(1);
            }
            let end = reply.flags & filesystem_protocol::reply_flags::END_OF_DIRECTORY != 0;
            if records.len() >= capacity || end {
                let destinations = records
                    .iter()
                    .enumerate()
                    .map(|(index, record)| {
                        records_address
                            .checked_add((index * size_of::<abi::file::DirectoryEntry>()) as u64)
                            .map(|destination| (destination, record))
                    })
                    .collect::<Option<Vec<_>>>();
                let Some(destinations) = destinations else {
                    return nullfs_proxy_finish_process(
                        request_id,
                        process_id,
                        stack_pointer,
                        error_return(abi::errno::RANGE),
                    );
                };
                let copied = {
                    let manager = PROCESS_MANAGER.lock();
                    let Some(process) = manager
                        .processes
                        .iter()
                        .find(|process| process.process_id == process_id)
                    else {
                        return Ok(());
                    };
                    destinations.iter().all(|(destination, record)| {
                        write_user_bytes(
                            *destination,
                            tmpfs_proxy_value_bytes(*record),
                            physical_memory_offset,
                            &process.pages,
                        )
                        .is_ok()
                    })
                };
                return nullfs_proxy_finish_process(
                    request_id,
                    process_id,
                    stack_pointer,
                    if copied {
                        records.len() as u64
                    } else {
                        error_return(ERR_IO)
                    },
                );
            }
            if count == 0 {
                return nullfs_proxy_finish_process(
                    request_id,
                    process_id,
                    stack_pointer,
                    error_return(ERR_IO),
                );
            }
            let request = nullfs_proxy_directory_request(node_id, previous_cookie);
            nullfs_proxy_continue(
                process_id,
                request_id,
                request,
                PendingNullfsProxyOperation::ReadDirectory {
                    node_id,
                    start_index,
                    seen,
                    records_address,
                    capacity,
                    records,
                    cookie: previous_cookie,
                    flags,
                },
                stack_pointer,
            )
        }
    }
}

fn decode_open_options(mut flags: u64) -> Result<(vfs::OpenOptions, bool), i64> {
    if flags == 0 {
        flags = OPEN_READ;
    }
    if flags & !OPEN_ALLOWED_FLAGS != 0 {
        return Err(ERR_INVALID_ARGUMENT);
    }
    let o = vfs::OpenOptions {
        read: flags & OPEN_READ != 0,
        write: flags & OPEN_WRITE != 0,
        create: flags & OPEN_CREATE != 0,
        truncate: flags & OPEN_TRUNCATE != 0,
        append: flags & OPEN_APPEND != 0,
    };
    if !o.write && (!o.read || o.create || o.truncate || o.append) || o.truncate && o.append {
        return Err(ERR_INVALID_ARGUMENT);
    }
    Ok((o, flags & OPEN_CLOSE_ON_EXEC != 0))
}
fn syscall_open(process_id: u64, address: u64, length: u64, flags: u64) -> u64 {
    let (options, close_on_exec) = match decode_open_options(flags) {
        Ok(decoded) => decoded,
        Err(e) => return error_return(e),
    };
    let path = match user_string(process_id, address, length) {
        Ok(p) => p,
        Err(e) => return error_return(e),
    };
    let descriptor = {
        let m = PROCESS_MANAGER.lock();
        let Some(p) = m.processes.iter().find(|p| p.process_id == process_id) else {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        };
        if descriptor_count(p) >= MAX_OPEN_FILES {
            return error_return(ERR_TOO_MANY_OPEN_FILES);
        }
        let Some(d) = allocate_descriptor(p) else {
            return error_return(ERR_TOO_MANY_OPEN_FILES);
        };
        d
    };
    let metadata = match vfs::open(&path, options) {
        Ok(m) => m,
        Err(e) => return error_return(vfs_errno(&e)),
    };
    let offset = if options.append { metadata.size } else { 0 };
    let handle = Arc::new(PreemptMutex::new(OpenFileState {
        path: metadata.path,
        offset,
        readable: options.read,
        writable: options.write,
        append: options.append,
        size: metadata.size,
        nullfs_size: None,
        backend: OpenFileBackend::Vfs,
    }));
    let mut m = PROCESS_MANAGER.lock();
    let Some(p) = m.process_mut(process_id) else {
        return error_return(ERR_BAD_FILE_DESCRIPTOR);
    };
    if descriptor_in_use(p, descriptor) {
        return error_return(ERR_TOO_MANY_OPEN_FILES);
    }
    p.open_files.push(OpenFile {
        descriptor,
        handle,
        close_on_exec,
    });
    p.open_count = p.open_count.saturating_add(1);
    descriptor
}
enum ReadOutcome {
    Ready(u64),
    Blocked,
}
#[derive(Clone)]
enum ReadTarget {
    Terminal,
    Pipe(PipeId),
    File(OpenFileHandle),
    Invalid,
}
fn syscall_read(
    process_id: u64,
    descriptor: u64,
    address: u64,
    length: u64,
    current_stack_pointer: usize,
) -> ReadOutcome {
    let length = match usize::try_from(length) {
        Ok(n) if n <= MAX_SYSCALL_READ_BYTES => n,
        _ => return ReadOutcome::Ready(error_return(ERR_ARGUMENT_TOO_LARGE)),
    };
    if length == 0 {
        return ReadOutcome::Ready(0);
    }
    if !user_range_allows(process_id, address, length, true) {
        return ReadOutcome::Ready(error_return(ERR_BAD_ADDRESS));
    }
    let target = {
        let m = PROCESS_MANAGER.lock();
        let Some(p) = m.processes.iter().find(|p| p.process_id == process_id) else {
            return ReadOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR));
        };
        match descriptor {
            0 => match &p.stdin_target {
                Some(StreamTarget::Pipe(id)) => ReadTarget::Pipe(*id),
                Some(StreamTarget::File(h)) => ReadTarget::File(h.clone()),
                None => ReadTarget::Terminal,
            },
            1 | 2 => ReadTarget::Invalid,
            d => {
                if let Some(pd) = p.pipe_descriptors.iter().find(|x| x.descriptor == d) {
                    match pd.direction {
                        PipeDirection::Reader => ReadTarget::Pipe(pd.pipe_id),
                        PipeDirection::Writer => ReadTarget::Invalid,
                    }
                } else {
                    p.open_files
                        .iter()
                        .find(|f| f.descriptor == d)
                        .map(|f| ReadTarget::File(f.handle.clone()))
                        .unwrap_or(ReadTarget::Invalid)
                }
            }
        }
    };
    match target {
        ReadTarget::Terminal => {
            syscall_terminal_read(process_id, address, length, current_stack_pointer)
        }
        ReadTarget::Pipe(id) => {
            syscall_pipe_read(process_id, id, address, length, current_stack_pointer)
        }
        ReadTarget::File(handle) => {
            let backend = handle.lock().backend;
            if matches!(backend, OpenFileBackend::NullfsProxy { .. }) {
                return nullfs_proxy_read(
                    process_id,
                    handle,
                    address,
                    length,
                    current_stack_pointer,
                );
            }
            if matches!(backend, OpenFileBackend::TmpfsProxy { .. }) {
                return tmpfs_proxy_read(
                    process_id,
                    handle,
                    address,
                    length,
                    current_stack_pointer,
                );
            }
            let mut buffer = vec![0_u8; length];
            let result = {
                let mut f = handle.lock();
                if !f.readable {
                    Err(ERR_BAD_FILE_DESCRIPTOR)
                } else {
                    match vfs::read_at(&f.path, f.offset, &mut buffer) {
                        Ok(count) => {
                            f.offset = f.offset.saturating_add(count as u64);
                            Ok(count)
                        }
                        Err(e) => Err(vfs_errno(&e)),
                    }
                }
            };
            match result {
                Ok(count) => {
                    unsafe {
                        ptr::copy_nonoverlapping(buffer.as_ptr(), address as *mut u8, count);
                    }
                    let mut m = PROCESS_MANAGER.lock();
                    if let Some(p) = m.process_mut(process_id) {
                        p.read_count = p.read_count.saturating_add(1);
                        p.bytes_read = p.bytes_read.saturating_add(count as u64);
                    }
                    ReadOutcome::Ready(count as u64)
                }
                Err(e) => ReadOutcome::Ready(error_return(e)),
            }
        }
        ReadTarget::Invalid => ReadOutcome::Ready(error_return(ERR_BAD_FILE_DESCRIPTOR)),
    }
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
        process.make_runnable();
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
            process.make_runnable();
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
            process.make_runnable();
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
        process.make_runnable();
        drop(manager);
        if !scheduler::wake_process(process_id) {
            return Err(Error::ProcessNotFound(process_id));
        }
        let _ = pipe::note_reader_wakeup(pending.pipe_id);
        Ok(())
    })
}

fn syscall_seek(process_id: u64, descriptor: u64, offset: u64, whence: u64) -> u64 {
    let handle = {
        let m = PROCESS_MANAGER.lock();
        let Some(p) = m.processes.iter().find(|p| p.process_id == process_id) else {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        };
        match descriptor {
            0 => match &p.stdin_target {
                Some(StreamTarget::File(h)) => Some(h.clone()),
                _ => None,
            },
            1 => match &p.stdout_target {
                Some(StreamTarget::File(h)) => Some(h.clone()),
                _ => None,
            },
            2 => match &p.stderr_target {
                Some(StreamTarget::File(h)) => Some(h.clone()),
                _ => None,
            },
            d => p
                .open_files
                .iter()
                .find(|f| f.descriptor == d)
                .map(|f| f.handle.clone()),
        }
    };
    let Some(handle) = handle else {
        return error_return(ERR_BAD_FILE_DESCRIPTOR);
    };
    let signed = offset as i64;
    let new_offset = {
        let mut f = handle.lock();
        match f.backend {
            OpenFileBackend::TmpfsProxy {
                generation,
                session_id,
                session_generation,
                ..
            } if !tmpfs_proxy_backend_is_current(generation, session_id, session_generation) => {
                return error_return(ERR_IO);
            }
            OpenFileBackend::NullfsProxy {
                generation,
                session_id,
                session_generation,
                ..
            } if !nullfs_proxy_backend_is_current(generation, session_id, session_generation) => {
                return error_return(ERR_IO);
            }
            _ => {}
        }
        let base = match whence {
            SEEK_SET => 0_i128,
            SEEK_CURRENT => i128::from(f.offset),
            SEEK_END => match f.backend {
                OpenFileBackend::TmpfsProxy { .. } => i128::from(f.size),
                OpenFileBackend::NullfsProxy { .. } => i128::from(open_file_size(&f)),
                OpenFileBackend::Vfs => match vfs::metadata(&f.path) {
                    Ok(md) => i128::from(md.size),
                    Err(e) => return error_return(vfs_errno(&e)),
                },
            },
            _ => return error_return(ERR_INVALID_ARGUMENT),
        };
        if whence == SEEK_SET && signed < 0 {
            return error_return(ERR_INVALID_ARGUMENT);
        }
        let Some(value) = base.checked_add(i128::from(signed)) else {
            return error_return(ERR_INVALID_ARGUMENT);
        };
        let Ok(value) = u64::try_from(value) else {
            return error_return(ERR_INVALID_ARGUMENT);
        };
        f.offset = value;
        value
    };
    let mut m = PROCESS_MANAGER.lock();
    if let Some(p) = m.process_mut(process_id) {
        p.seek_count = p.seek_count.saturating_add(1);
    }
    new_offset
}

fn syscall_close(process_id: u64, descriptor: u64) -> u64 {
    if descriptor < 3 {
        return error_return(ERR_BAD_FILE_DESCRIPTOR);
    }

    enum ClosedDescriptor {
        File(OpenFile),
        Pipe(PipeDescriptor),
    }

    let closed = {
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
            ClosedDescriptor::Pipe(descriptor)
        } else if let Some(index) = process
            .open_files
            .iter()
            .position(|file| file.descriptor == descriptor)
        {
            let file = process.open_files.remove(index);
            process.close_count = process.close_count.saturating_add(1);
            ClosedDescriptor::File(file)
        } else {
            return error_return(ERR_BAD_FILE_DESCRIPTOR);
        }
    };

    match closed {
        ClosedDescriptor::File(file) => {
            drop(file);
            0
        }
        ClosedDescriptor::Pipe(descriptor) => {
            let result = match descriptor.direction {
                PipeDirection::Reader => pipe::close_reader(descriptor.pipe_id),
                PipeDirection::Writer => pipe::close_writer(descriptor.pipe_id),
            };
            match result {
                Ok(()) => 0,
                Err(_) => error_return(ERR_IO),
            }
        }
    }
}

fn vfs_errno(error: &vfs::Error) -> i64 {
    match error {
        vfs::Error::NotFound => ERR_NO_ENTRY,
        vfs::Error::IsDirectory => ERR_IS_DIRECTORY,
        vfs::Error::ReadOnly => ERR_READ_ONLY,
        vfs::Error::NoSpace | vfs::Error::FileTooLarge | vfs::Error::TooManyFiles => ERR_NO_SPACE,
        vfs::Error::InvalidPath
        | vfs::Error::PathTooLong
        | vfs::Error::TooManyPathComponents
        | vfs::Error::NameTooLong
        | vfs::Error::InvalidOpenOptions => ERR_INVALID_ARGUMENT,
        _ => ERR_IO,
    }
}

fn valid_environment_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_ENVIRONMENT_NAME_BYTES {
        return false;
    }
    let first = bytes[0];
    if !matches!(first, b'A'..=b'Z' | b'a'..=b'z' | b'_') {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|byte| matches!(*byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

fn environment_name(entry: &str) -> Option<&str> {
    let (name, _) = entry.split_once('=')?;
    valid_environment_name(name).then_some(name)
}

fn environment_serialized_bytes(environment: &[String]) -> Option<usize> {
    environment.iter().try_fold(0usize, |total, entry| {
        total.checked_add(entry.len().checked_add(1)?)
    })
}

fn collect_environment(environment: &[&str]) -> Result<Vec<String>, Error> {
    if environment.len() > MAX_ENVIRONMENT_VARIABLES {
        return Err(Error::TooManyEnvironmentVariables);
    }
    let mut entries: Vec<String> = Vec::with_capacity(environment.len());
    let mut total_bytes = 0usize;
    for entry in environment {
        if entry.as_bytes().contains(&0) {
            return Err(Error::InvalidEnvironment);
        }
        let Some(name) = environment_name(entry) else {
            return Err(Error::InvalidEnvironment);
        };
        if entries
            .iter()
            .any(|existing| environment_name(existing) == Some(name))
        {
            return Err(Error::InvalidEnvironment);
        }
        total_bytes = total_bytes
            .checked_add(entry.len().saturating_add(1))
            .ok_or(Error::EnvironmentBytesTooLarge)?;
        if total_bytes > MAX_ENVIRONMENT_BYTES {
            return Err(Error::EnvironmentBytesTooLarge);
        }
        entries.push(String::from(*entry));
    }
    Ok(entries)
}

fn build_initial_stack(
    arguments: &[&str],
    environment: &[String],
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
    if environment.len() > MAX_ENVIRONMENT_VARIABLES {
        return Err(Error::TooManyEnvironmentVariables);
    }
    let environment_bytes =
        environment_serialized_bytes(environment).ok_or(Error::EnvironmentBytesTooLarge)?;
    if environment
        .iter()
        .any(|entry| entry.as_bytes().contains(&0) || environment_name(entry).is_none())
    {
        return Err(Error::InvalidEnvironment);
    }
    if environment_bytes > MAX_ENVIRONMENT_BYTES {
        return Err(Error::EnvironmentBytesTooLarge);
    }

    let mut cursor = USER_STACK_TOP;
    let mut argument_pointers = Vec::with_capacity(arguments.len());
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
        argument_pointers.push(cursor);
    }
    argument_pointers.reverse();

    let mut environment_pointers = Vec::with_capacity(environment.len());
    for entry in environment.iter().rev() {
        cursor = cursor
            .checked_sub(entry.len().saturating_add(1) as u64)
            .ok_or(Error::StackLayoutInvalid)?;
        write_user_bytes(cursor, entry.as_bytes(), physical_memory_offset, pages)?;
        write_user_bytes(
            cursor + entry.len() as u64,
            &[0],
            physical_memory_offset,
            pages,
        )?;
        environment_pointers.push(cursor);
    }
    environment_pointers.reverse();

    let table_words = 1usize
        .checked_add(argument_pointers.len())
        .and_then(|words| words.checked_add(1))
        .and_then(|words| words.checked_add(environment_pointers.len()))
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
    table.push(argument_pointers.len() as u64);
    table.extend(argument_pointers.iter().copied());
    table.push(0);
    table.extend(environment_pointers.iter().copied());
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
            flags,
            copy_on_write: false,
        });
        address = address
            .checked_add(PAGE_BYTES)
            .ok_or(Error::AddressOverflow)?;
    }
    Ok(())
}

fn copy_segment(
    executable_bytes: &[u8],
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
        let source_start = usize::try_from(file_offset).map_err(|_| Error::AddressOverflow)?;
        let source_end = source_start
            .checked_add(chunk)
            .ok_or(Error::AddressOverflow)?;
        let source = executable_bytes
            .get(source_start..source_end)
            .ok_or(Error::InvalidExecutableBytes)?;
        destination.copy_from_slice(source);
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
