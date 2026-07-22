// Outermost syscall entry for process-group services. The platform entry remains
// the fallback so existing path and descriptor compatibility shims keep their
// established accounting and blocking behavior.

global_asm!(
    r#"
    .section .text
    .p2align 4
    .global galactic_process_group_syscall_interrupt_entry
    .type galactic_process_group_syscall_interrupt_entry,@function
galactic_process_group_syscall_interrupt_entry:
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
    call galactic_process_group_syscall_dispatch
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
.size galactic_process_group_syscall_interrupt_entry, .-galactic_process_group_syscall_interrupt_entry
"#
);

unsafe extern "C" {
    fn galactic_process_group_syscall_interrupt_entry();
}

pub fn process_group_syscall_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(galactic_process_group_syscall_interrupt_entry as *const () as usize as u64)
}

#[unsafe(no_mangle)]
pub extern "C" fn galactic_process_group_syscall_dispatch(current_stack_pointer: usize) -> usize {
    let registers_pointer = current_stack_pointer as *mut SavedRegisters;
    let syscall_number = unsafe { (*registers_pointer).rax };
    if !matches!(
        syscall_number,
        abi::syscall::FOREGROUND_PROCESS_GROUP
            | abi::syscall::GET_PROCESS_GROUP
            | abi::syscall::SET_PROCESS_GROUP
    ) {
        return galactic_platform_syscall_dispatch(current_stack_pointer);
    }

    let Some(process_id) = scheduler::current_process_id() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };

    if syscall_number == abi::syscall::FOREGROUND_PROCESS_GROUP {
        let mut manager = PROCESS_MANAGER.lock();
        let Some(process) = manager.process_mut(process_id) else {
            unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
            return current_stack_pointer;
        };
        process.syscall_count = process.syscall_count.saturating_add(1);
    }

    let registers = unsafe { &mut *registers_pointer };
    registers.rax = match syscall_number {
        abi::syscall::FOREGROUND_PROCESS_GROUP => {
            platform_foreground_process_group(process_id, registers.rdi)
        }
        abi::syscall::GET_PROCESS_GROUP => {
            platform_get_process_group(process_id, registers.rdi)
        }
        abi::syscall::SET_PROCESS_GROUP => {
            platform_set_process_group(process_id, registers.rdi, registers.rsi)
        }
        _ => error_return(ERR_NOT_IMPLEMENTED),
    };
    current_stack_pointer
}
