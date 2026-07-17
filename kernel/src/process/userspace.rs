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

use super::elf::{Image, ImageType, LoadSegment};

pub const SYSCALL_VECTOR: u8 = 0x80;

const PAGE_FAULT_VECTOR: u64 = 14;
const GENERAL_PROTECTION_VECTOR: u64 = 13;
const SYSCALL_WRITE: u64 = 1;
const SYSCALL_YIELD: u64 = 2;
const SYSCALL_EXIT: u64 = 3;

const USER_MIN_ADDRESS: u64 = 0x0001_0000;
const USER_PML4_SLOT_END: u64 = 0x0000_0080_0000_0000;
const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;
const USER_STACK_SIZE: usize = 64 * 1024;
const USER_STACK_GUARD_SIZE: usize = Size4KiB::SIZE as usize;
const KERNEL_TRANSITION_STACK_SIZE: usize = 64 * 1024;
const KERNEL_TRANSITION_STACK_WORDS: usize = KERNEL_TRANSITION_STACK_SIZE / size_of::<u128>();
const MAX_SYSCALL_WRITE_BYTES: usize = 4096;
const USER_RFLAGS: u64 = 0x202;
const PAGE_BYTES: u64 = Size4KiB::SIZE;

const ERR_BAD_FILE_DESCRIPTOR: i64 = -9;
const ERR_BAD_ADDRESS: i64 = -14;
const ERR_ARGUMENT_TOO_LARGE: i64 = -7;
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
    Exited,
    Faulted,
}

impl fmt::Display for ProcessState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runnable => formatter.write_str("runnable"),
            Self::Exited => formatter.write_str("exited"),
            Self::Faulted => formatter.write_str("faulted"),
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
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessResult {
    pub process_id: u64,
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
    pub scheduled_count: u64,
    pub runtime_ticks: u64,
    pub frames_reclaimed: usize,
}

impl ProcessResult {
    pub fn exit_code(&self) -> Option<u64> {
        match &self.termination {
            TerminationReason::Exit(code) => Some(*code),
            TerminationReason::Fault(_) => None,
        }
    }

    pub fn fault(&self) -> Option<FaultInfo> {
        match &self.termination {
            TerminationReason::Fault(fault) => Some(*fault),
            TerminationReason::Exit(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManagerSnapshot {
    pub spawned: u64,
    pub active: usize,
    pub exited: u64,
    pub faulted: u64,
    pub reaped: u64,
    pub frames_reclaimed: u64,
    pub results: Vec<ProcessResult>,
}

#[derive(Debug, Clone)]
pub struct SpawnInfo {
    pub process_id: u64,
    pub task_id: u64,
    pub path: String,
    pub entry_point: u64,
    pub page_table_address: u64,
    pub mapped_pages: usize,
    pub owned_frames: usize,
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
    ProcessNotFound(u64),
    Scheduler(scheduler::InitError),
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
            Self::ProcessNotFound(_) => "userspace process bookkeeping is missing",
            Self::Scheduler(_) => "scheduler rejected the userspace task",
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
            Self::Scheduler(error) => formatter.write_str(error.description()),
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

struct Process {
    process_id: u64,
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
    kernel_stack: Box<[u128]>,
    owned_frames: Vec<PhysFrame<Size4KiB>>,
    syscall_count: u64,
    write_count: u64,
    yield_count: u64,
    bytes_written: u64,
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
    exited: u64,
    faulted: u64,
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
            exited: 0,
            faulted: 0,
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
            active: self
                .processes
                .iter()
                .filter(|process| process.state == ProcessState::Runnable)
                .count(),
            exited: self.exited,
            faulted: self.faulted,
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

impl BuiltAddressSpace {
    fn build(
        path: &str,
        image: &Image,
        kernel_mapper: &mut OffsetPageTable<'_>,
        frame_allocator: &mut BootInfoFrameAllocator,
        physical_memory_offset: VirtAddr,
    ) -> Result<Self, Error> {
        let mut tracking = TrackingFrameAllocator::new(frame_allocator);
        let result = Self::build_tracked(
            path,
            image,
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

        Ok(Self {
            page_table_frame,
            entry_point: image.entry_point,
            stack_pointer: USER_STACK_TOP - 16,
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
    let mut pending_process = Some(Process {
        process_id,
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
        kernel_stack,
        owned_frames: core::mem::take(&mut address_space.owned_frames),
        syscall_count: 0,
        write_count: 0,
        yield_count: 0,
        bytes_written: 0,
    });

    let task_result = cpu_interrupts::without_interrupts(|| {
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
        Ok::<u64, scheduler::InitError>(task_id)
    });

    let task_id = match task_result {
        Ok(task_id) => task_id,
        Err(error) => {
            if let Some(mut process) = pending_process.take() {
                for frame in process.owned_frames.drain(..) {
                    frame_allocator.deallocate_frame(frame);
                }
            }
            return Err(Error::Scheduler(error));
        }
    };

    Ok(SpawnInfo {
        process_id,
        task_id,
        path: path.to_string(),
        entry_point: image.entry_point,
        page_table_address,
        mapped_pages,
        owned_frames: owned_frame_count,
    })
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

pub fn reap(frame_allocator: &mut BootInfoFrameAllocator) -> Result<usize, Error> {
    let process_ids = scheduler::reap_zombie_processes();
    let mut reaped = 0usize;

    for task in process_ids {
        let process_id = task.process_id;
        let mut process = cpu_interrupts::without_interrupts(|| {
            PROCESS_MANAGER.lock().remove_process(process_id)
        })
        .ok_or(Error::ProcessNotFound(process_id))?;
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
        SYSCALL_WRITE => {
            let result = syscall_write(process_id, registers.rdi, registers.rsi, registers.rdx);
            registers.rax = result;
            current_stack_pointer
        }
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

fn syscall_write(process_id: u64, file_descriptor: u64, address: u64, length: u64) -> u64 {
    if file_descriptor != 1 && file_descriptor != 2 {
        return error_return(ERR_BAD_FILE_DESCRIPTOR);
    }
    let Ok(length) = usize::try_from(length) else {
        return error_return(ERR_ARGUMENT_TOO_LARGE);
    };
    if length > MAX_SYSCALL_WRITE_BYTES {
        return error_return(ERR_ARGUMENT_TOO_LARGE);
    }

    let readable = PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == process_id)
        .map(|process| {
            process
                .ranges
                .iter()
                .any(|range| range.readable && range.contains(address, length))
        })
        .unwrap_or(false);
    if !readable {
        return error_return(ERR_BAD_ADDRESS);
    }

    let bytes = unsafe { slice::from_raw_parts(address as *const u8, length) };
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
    length as u64
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
