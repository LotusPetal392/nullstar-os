use alloc::{boxed::Box, string::String, vec, vec::Vec};
use core::{
    arch::{asm, global_asm},
    fmt,
    mem::{align_of, size_of},
    ptr, slice, str,
};

use spin::Mutex;
use x86_64::{
    VirtAddr,
    instructions::interrupts as cpu_interrupts,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageSize, PageTable, PageTableFlags,
        PhysFrame, Size4KiB, mapper::MapToError,
    },
};

use crate::{gdt, scheduler, vfs};

use super::elf::{Image, ImageType, LoadSegment};

pub const SYSCALL_VECTOR: u8 = 0x80;

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

static ACTIVE_PROCESS: Mutex<Option<ActiveProcess>> = Mutex::new(None);
static LAST_RESULT: Mutex<Option<RunResult>> = Mutex::new(None);

#[unsafe(no_mangle)]
static mut GALACTIC_USER_KERNEL_CR3: u64 = 0;
#[unsafe(no_mangle)]
static mut GALACTIC_USER_RETURN_RSP: u64 = 0;
#[unsafe(no_mangle)]
static mut GALACTIC_USER_EXIT_CODE: u64 = 0;

global_asm!(
    r#"
    .section .text
    .p2align 4
    .global galactic_enter_userspace
    .type galactic_enter_userspace,@function
galactic_enter_userspace:
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15

    mov [rip + GALACTIC_USER_RETURN_RSP], rsp
    mov rax, cr3
    mov [rip + GALACTIC_USER_KERNEL_CR3], rax
    mov cr3, rdi

    push r8
    push rdx
    push {user_rflags}
    push rcx
    push rsi
    iretq
.size galactic_enter_userspace, .-galactic_enter_userspace

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
    test rax, rax
    jz .Lgalactic_userspace_exit

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

.Lgalactic_userspace_exit:
    mov rax, [rip + GALACTIC_USER_KERNEL_CR3]
    mov cr3, rax
    mov rsp, [rip + GALACTIC_USER_RETURN_RSP]
    mov rax, [rip + GALACTIC_USER_EXIT_CODE]

    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    sti
    ret
.size galactic_syscall_interrupt_entry, .-galactic_syscall_interrupt_entry
"#,
    user_rflags = const USER_RFLAGS,
);

unsafe extern "C" {
    fn galactic_enter_userspace(
        page_table_address: u64,
        entry_point: u64,
        stack_pointer: u64,
        code_selector: u64,
        data_selector: u64,
    ) -> u64;
    fn galactic_syscall_interrupt_entry();
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub path: String,
    pub exit_code: u64,
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
}

#[derive(Debug)]
pub enum Error {
    AlreadyRunning,
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
    ProcessDidNotExit,
    ProcessExitMismatch { returned: u64, recorded: u64 },
    Vfs(vfs::Error),
}

impl Error {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::AlreadyRunning => "a userspace process is already active",
            Self::SchedulerNotOnBootstrapTask => {
                "userspace launch must occur from the bootstrap scheduler task"
            }
            Self::UnsupportedImageType => "the first userspace loader requires an ET_EXEC image",
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
            Self::ProcessDidNotExit => "userspace returned without issuing the exit syscall",
            Self::ProcessExitMismatch { .. } => "userspace exit bookkeeping is inconsistent",
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
                write!(formatter, "userspace page {address:#018x} is already mapped")
            }
            Self::ProcessExitMismatch { returned, recorded } => write!(
                formatter,
                "userspace returned exit code {returned}, but syscall state recorded {recorded}"
            ),
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

struct ActiveProcess {
    path: String,
    page_table_address: u64,
    entry_point: u64,
    mapped_pages: usize,
    load_segments: usize,
    guard_page_address: u64,
    ranges: Vec<UserRange>,
    _kernel_stack: Box<[u128]>,
    syscall_count: u64,
    write_count: u64,
    yield_count: u64,
    bytes_written: u64,
    exit_code: Option<u64>,
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
}

impl BuiltAddressSpace {
    fn new(
        path: &str,
        image: &Image,
        kernel_mapper: &mut OffsetPageTable<'_>,
        frame_allocator: &mut impl FrameAllocator<Size4KiB>,
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
            copy_segment(
                path,
                segment,
                physical_memory_offset,
                &pages,
            )?;
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
            range.executable
                && image.entry_point >= range.start
                && image.entry_point < range.end
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
        })
    }
}

pub fn run(
    path: &str,
    image: &Image,
    kernel_mapper: &mut OffsetPageTable<'_>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    physical_memory_offset: VirtAddr,
) -> Result<RunResult, Error> {
    if ACTIVE_PROCESS.lock().is_some() {
        return Err(Error::AlreadyRunning);
    }
    if scheduler::snapshot().current_task_name != "bootstrap-shell" {
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
    if kernel_stack_start % align_of::<u128>() != 0 || kernel_stack_top % 16 != 0 {
        return Err(Error::StackLayoutInvalid);
    }

    let current_stack = current_stack_pointer();
    for address in [
        galactic_enter_userspace as usize as u64,
        galactic_syscall_interrupt_entry as usize as u64,
        current_stack,
        kernel_stack_start as u64,
        physical_memory_offset.as_u64(),
    ] {
        if pml4_index(address) == 0 {
            return Err(Error::KernelMappingUsesUserSlot(address));
        }
    }

    let address_space = BuiltAddressSpace::new(
        path,
        image,
        kernel_mapper,
        frame_allocator,
        physical_memory_offset,
    )?;
    let page_table_address = address_space.page_table_frame.start_address().as_u64();
    let mapped_pages = address_space.pages.len();

    *ACTIVE_PROCESS.lock() = Some(ActiveProcess {
        path: String::from(path),
        page_table_address,
        entry_point: address_space.entry_point,
        mapped_pages,
        load_segments: image.load_segments().len(),
        guard_page_address: address_space.guard_page_address,
        ranges: address_space.ranges,
        _kernel_stack: kernel_stack,
        syscall_count: 0,
        write_count: 0,
        yield_count: 0,
        bytes_written: 0,
        exit_code: None,
    });

    unsafe {
        ptr::write_volatile(&raw mut GALACTIC_USER_EXIT_CODE, u64::MAX);
    }
    gdt::set_privilege_stack(VirtAddr::new(kernel_stack_top as u64));

    cpu_interrupts::disable();
    let returned_exit_code = unsafe {
        galactic_enter_userspace(
            page_table_address,
            address_space.entry_point,
            address_space.stack_pointer,
            u64::from(gdt::user_code_selector()),
            u64::from(gdt::user_data_selector()),
        )
    };

    gdt::reset_privilege_stack();
    let active = ACTIVE_PROCESS
        .lock()
        .take()
        .ok_or(Error::ProcessDidNotExit)?;
    let recorded_exit_code = active.exit_code.ok_or(Error::ProcessDidNotExit)?;
    if returned_exit_code != recorded_exit_code {
        return Err(Error::ProcessExitMismatch {
            returned: returned_exit_code,
            recorded: recorded_exit_code,
        });
    }

    let result = RunResult {
        path: active.path,
        exit_code: returned_exit_code,
        entry_point: active.entry_point,
        page_table_address: active.page_table_address,
        mapped_pages: active.mapped_pages,
        load_segments: active.load_segments,
        user_stack_bytes: USER_STACK_SIZE,
        guard_page_address: active.guard_page_address,
        kernel_stack_bytes,
        syscall_count: active.syscall_count,
        write_count: active.write_count,
        yield_count: active.yield_count,
        bytes_written: active.bytes_written,
    };
    *LAST_RESULT.lock() = Some(result.clone());
    Ok(result)
}

pub fn last_result() -> Option<RunResult> {
    LAST_RESULT.lock().clone()
}

pub fn syscall_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(galactic_syscall_interrupt_entry as usize as u64)
}

#[unsafe(no_mangle)]
pub extern "C" fn galactic_syscall_dispatch(current_stack_pointer: usize) -> usize {
    let registers = unsafe { &mut *(current_stack_pointer as *mut SyscallRegisters) };
    {
        let mut active = ACTIVE_PROCESS.lock();
        let Some(process) = active.as_mut() else {
            registers.rax = error_return(ERR_NOT_IMPLEMENTED);
            return current_stack_pointer;
        };
        process.syscall_count = process.syscall_count.saturating_add(1);
    }

    match registers.rax {
        SYSCALL_WRITE => {
            let result = syscall_write(registers.rdi, registers.rsi, registers.rdx);
            registers.rax = result;
            current_stack_pointer
        }
        SYSCALL_YIELD => {
            {
                let mut active = ACTIVE_PROCESS.lock();
                if let Some(process) = active.as_mut() {
                    process.yield_count = process.yield_count.saturating_add(1);
                }
            }
            registers.rax = 0;
            scheduler::on_yield(current_stack_pointer)
        }
        SYSCALL_EXIT => {
            let exit_code = registers.rdi;
            {
                let mut active = ACTIVE_PROCESS.lock();
                if let Some(process) = active.as_mut() {
                    process.exit_code = Some(exit_code);
                }
            }
            unsafe {
                ptr::write_volatile(&raw mut GALACTIC_USER_EXIT_CODE, exit_code);
            }
            gdt::reset_privilege_stack();
            0
        }
        _ => {
            registers.rax = error_return(ERR_NOT_IMPLEMENTED);
            current_stack_pointer
        }
    }
}

#[repr(C)]
struct SyscallRegisters {
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

fn syscall_write(file_descriptor: u64, address: u64, length: u64) -> u64 {
    if file_descriptor != 1 && file_descriptor != 2 {
        return error_return(ERR_BAD_FILE_DESCRIPTOR);
    }
    let Ok(length) = usize::try_from(length) else {
        return error_return(ERR_ARGUMENT_TOO_LARGE);
    };
    if length > MAX_SYSCALL_WRITE_BYTES {
        return error_return(ERR_ARGUMENT_TOO_LARGE);
    }

    let readable = ACTIVE_PROCESS
        .lock()
        .as_ref()
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

    let mut active = ACTIVE_PROCESS.lock();
    if let Some(process) = active.as_mut() {
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
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
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
        let within_page = usize::try_from(virtual_address - page_address)
            .map_err(|_| Error::AddressOverflow)?;
        let remaining_page = Size4KiB::SIZE as usize - within_page;
        let remaining_file = usize::try_from(segment.file_size - copied)
            .map_err(|_| Error::AddressOverflow)?;
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

fn zero_frame(
    frame: PhysFrame<Size4KiB>,
    physical_memory_offset: VirtAddr,
) -> Result<(), Error> {
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

fn current_stack_pointer() -> u64 {
    let stack_pointer: u64;
    unsafe {
        asm!(
            "mov {}, rsp",
            out(reg) stack_pointer,
            options(nomem, nostack, preserves_flags)
        );
    }
    stack_pointer
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
