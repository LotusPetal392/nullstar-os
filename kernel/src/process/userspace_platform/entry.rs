// Additional syscall entry and handlers for the documented userspace platform
// ABI. This file is included after `userspace.rs` so it deliberately shares the
// process manager's private invariants instead of duplicating them.

const PLATFORM_PAGE_SIZE: u64 = Size4KiB::SIZE;
const PLATFORM_WORKING_DIRECTORY_NAME: &str = "PWD";
const PLATFORM_WORKING_DIRECTORY_PREFIX: &str = "PWD=";

global_asm!(
    r#"
    .section .text
    .p2align 4
    .global galactic_platform_syscall_interrupt_entry
    .type galactic_platform_syscall_interrupt_entry,@function
galactic_platform_syscall_interrupt_entry:
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
    call galactic_platform_syscall_dispatch
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
.size galactic_platform_syscall_interrupt_entry, .-galactic_platform_syscall_interrupt_entry
"#
);

unsafe extern "C" {
    fn galactic_platform_syscall_interrupt_entry();
}

pub fn platform_syscall_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(galactic_platform_syscall_interrupt_entry as *const () as usize as u64)
}

#[unsafe(no_mangle)]
pub extern "C" fn galactic_platform_syscall_dispatch(current_stack_pointer: usize) -> usize {
    let registers_pointer = current_stack_pointer as *mut SavedRegisters;
    let syscall_number = unsafe { (*registers_pointer).rax };

    if !platform_syscall_number(syscall_number)
        && !platform_reserved_environment_call(syscall_number, registers_pointer)
    {
        return galactic_syscall_dispatch(current_stack_pointer);
    }

    let Some(process_id) = scheduler::current_process_id() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };

    // Keep the existing exact smoke-test counter stable: the legacy dispatcher
    // continues to account for calls 1 through 22, while platform ABI calls
    // are deliberately excluded until versioned syscall metrics are added.
    let registers = unsafe { &mut *registers_pointer };
    registers.rax = match syscall_number {
        abi::syscall::SYSTEM_INFO => platform_system_info(process_id, registers.rdi, registers.rsi),
        abi::syscall::STAT => platform_stat(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
        ),
        abi::syscall::FSTAT => {
            platform_fstat(process_id, registers.rdi, registers.rsi, registers.rdx)
        }
        abi::syscall::READ_DIRECTORY => platform_read_directory(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
            registers.r8,
        ),
        abi::syscall::CHDIR => platform_chdir(process_id, registers.rdi, registers.rsi),
        abi::syscall::GETCWD => platform_getcwd(process_id, registers.rdi, registers.rsi),
        abi::syscall::DUP => platform_dup(process_id, registers.rdi),
        abi::syscall::DUP2 => platform_dup2(process_id, registers.rdi, registers.rsi),
        abi::syscall::GETPPID => platform_getppid(process_id),
        abi::syscall::KILL => platform_kill(process_id, registers.rdi, registers.rsi),
        abi::syscall::ENVIRONMENT_SET | abi::syscall::ENVIRONMENT_UNSET => {
            error_return(ERR_INVALID_ARGUMENT)
        }
        _ => error_return(ERR_NOT_IMPLEMENTED),
    };
    current_stack_pointer
}

fn platform_syscall_number(number: u64) -> bool {
    matches!(
        number,
        abi::syscall::SYSTEM_INFO
            | abi::syscall::STAT
            | abi::syscall::FSTAT
            | abi::syscall::READ_DIRECTORY
            | abi::syscall::CHDIR
            | abi::syscall::GETCWD
            | abi::syscall::DUP
            | abi::syscall::DUP2
            | abi::syscall::GETPPID
            | abi::syscall::KILL
    )
}

fn platform_reserved_environment_call(
    syscall_number: u64,
    registers_pointer: *const SavedRegisters,
) -> bool {
    if !matches!(
        syscall_number,
        abi::syscall::ENVIRONMENT_SET | abi::syscall::ENVIRONMENT_UNSET
    ) {
        return false;
    }
    let Some(process_id) = scheduler::current_process_id() else {
        return false;
    };
    let registers = unsafe { &*registers_pointer };
    user_text(
        process_id,
        registers.rdi,
        registers.rsi,
        MAX_ENVIRONMENT_NAME_BYTES,
    )
    .is_ok_and(|name| name == PLATFORM_WORKING_DIRECTORY_NAME)
}

fn platform_system_info(process_id: u64, address: u64, length: u64) -> u64 {
    let info = abi::SystemInfo {
        abi_major: abi::ABI_VERSION_MAJOR,
        abi_minor: abi::ABI_VERSION_MINOR,
        capabilities: abi::capability::PLATFORM_V1,
        page_size: PLATFORM_PAGE_SIZE,
        maximum_open_files: MAX_OPEN_FILES as u64,
        maximum_path_bytes: abi::limits::MAX_PATH_BYTES as u64,
        maximum_directory_entries: abi::limits::MAX_DIRECTORY_ENTRIES_PER_CALL as u64,
        init_process_id: INIT_PROCESS_ID,
    };
    platform_write_value(process_id, address, length, info)
}
