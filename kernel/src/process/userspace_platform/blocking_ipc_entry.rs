// Scheduler-integrated endpoint readiness waits layered over the bounded
// capability data-movement syscalls.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EndpointWaiter {
    object: CapabilityObjectRef,
    process_id: u64,
}

static ENDPOINT_WAITERS: Mutex<Vec<EndpointWaiter>> = Mutex::new(Vec::new());

global_asm!(
    r#"
    .section .text
    .p2align 4
    .global nullstar_blocking_ipc_syscall_interrupt_entry
    .type nullstar_blocking_ipc_syscall_interrupt_entry,@function
nullstar_blocking_ipc_syscall_interrupt_entry:
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
    call nullstar_blocking_ipc_syscall_dispatch
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
.size nullstar_blocking_ipc_syscall_interrupt_entry, .-nullstar_blocking_ipc_syscall_interrupt_entry
"#
);

unsafe extern "C" {
    fn nullstar_blocking_ipc_syscall_interrupt_entry();
}

pub fn blocking_ipc_syscall_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(nullstar_blocking_ipc_syscall_interrupt_entry as *const () as usize as u64)
}

#[unsafe(no_mangle)]
pub extern "C" fn nullstar_blocking_ipc_syscall_dispatch(
    current_stack_pointer: usize,
) -> usize {
    let registers_pointer = current_stack_pointer as *mut SavedRegisters;
    let syscall_number = unsafe { (*registers_pointer).rax };

    if syscall_number == abi::syscall::SYSTEM_INFO {
        let Some(process_id) = scheduler::current_process_id() else {
            unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
            return current_stack_pointer;
        };
        let address = unsafe { (*registers_pointer).rdi };
        let length = unsafe { (*registers_pointer).rsi };
        unsafe {
            (*registers_pointer).rax = blocking_ipc_system_info(process_id, address, length)
        };
        return current_stack_pointer;
    }

    if syscall_number == abi::syscall::ENDPOINT_WAIT {
        return blocking_endpoint_wait(current_stack_pointer, registers_pointer);
    }

    if syscall_number == abi::syscall::ENDPOINT_SEND {
        let endpoint_object = scheduler::current_process_id().and_then(|process_id| {
            let endpoint_handle = unsafe { (*registers_pointer).rdi };
            endpoint_object_for_handle(
                process_id,
                endpoint_handle,
                abi::capability::RIGHT_SEND,
            )
            .ok()
        });
        let next_stack_pointer =
            nullstar_capability_grant_syscall_dispatch(current_stack_pointer);
        if next_stack_pointer == current_stack_pointer
            && unsafe { ((*registers_pointer).rax as i64) >= 0 }
            && let Some(endpoint_object) = endpoint_object
        {
            wake_endpoint_waiter(endpoint_object);
        }
        return next_stack_pointer;
    }

    nullstar_capability_grant_syscall_dispatch(current_stack_pointer)
}

fn blocking_ipc_system_info(process_id: u64, address: u64, length: u64) -> u64 {
    let info = abi::SystemInfo {
        abi_major: abi::ABI_VERSION_MAJOR,
        abi_minor: abi::ABI_VERSION_MINOR,
        capabilities: abi::capability::PLATFORM_V1
            | abi::capability::PROTECTION_V1
            | abi::capability::BLOCKING_ENDPOINT_WAIT,
        page_size: PLATFORM_PAGE_SIZE,
        maximum_open_files: MAX_OPEN_FILES as u64,
        maximum_path_bytes: abi::limits::MAX_PATH_BYTES as u64,
        maximum_directory_entries: abi::limits::MAX_DIRECTORY_ENTRIES_PER_CALL as u64,
        init_process_id: INIT_PROCESS_ID,
    };
    platform_write_value(process_id, address, length, info)
}

fn endpoint_object_for_handle(
    process_id: u64,
    endpoint_handle: u64,
    required_right: u64,
) -> Result<CapabilityObjectRef, i64> {
    let registry = CAPABILITY_REGISTRY.lock();
    let entry = registry
        .entry(process_id, endpoint_handle)
        .ok_or(abi::errno::BAD_FILE_DESCRIPTOR)?;
    capability_has_right(entry, required_right)?;
    if entry.object.kind != abi::capability::KIND_ENDPOINT {
        return Err(abi::errno::INVALID_ARGUMENT);
    }
    let object_index = registry.object_index(entry.object).ok_or(abi::errno::IO)?;
    if !matches!(
        registry.objects[object_index].data,
        CapabilityObjectData::Endpoint(_)
    ) {
        return Err(abi::errno::INVALID_ARGUMENT);
    }
    Ok(entry.object)
}

fn endpoint_has_message(object: CapabilityObjectRef) -> Result<bool, i64> {
    let registry = CAPABILITY_REGISTRY.lock();
    let object_index = registry.object_index(object).ok_or(abi::errno::IO)?;
    match &registry.objects[object_index].data {
        CapabilityObjectData::Endpoint(endpoint) => Ok(!endpoint.queue.is_empty()),
        CapabilityObjectData::Notification(_) | CapabilityObjectData::SharedMemory(_) => {
            Err(abi::errno::INVALID_ARGUMENT)
        }
    }
}

fn blocking_endpoint_wait(
    current_stack_pointer: usize,
    registers_pointer: *mut SavedRegisters,
) -> usize {
    let Some(process_id) = scheduler::current_process_id() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };
    let endpoint_handle = unsafe { (*registers_pointer).rdi };

    // This lock is held through waiter registration and scheduler blocking.
    // A successful sender acquires it only after its message has been queued,
    // so it cannot miss the transition from runnable to blocked.
    let mut waiters = ENDPOINT_WAITERS.lock();
    waiters.retain(|waiter| waiter.process_id != process_id);

    let endpoint_object = match endpoint_object_for_handle(
        process_id,
        endpoint_handle,
        abi::capability::RIGHT_RECEIVE,
    ) {
        Ok(object) => object,
        Err(error) => {
            unsafe { (*registers_pointer).rax = error_return(error) };
            return current_stack_pointer;
        }
    };
    match endpoint_has_message(endpoint_object) {
        Ok(true) => {
            unsafe { (*registers_pointer).rax = 0 };
            current_stack_pointer
        }
        Ok(false) => {
            waiters.push(EndpointWaiter {
                object: endpoint_object,
                process_id,
            });
            unsafe { (*registers_pointer).rax = 0 };
            let next_stack_pointer = scheduler::block_current(current_stack_pointer);
            drop(waiters);
            next_stack_pointer
        }
        Err(error) => {
            unsafe { (*registers_pointer).rax = error_return(error) };
            current_stack_pointer
        }
    }
}

fn wake_endpoint_waiter(object: CapabilityObjectRef) {
    let mut waiters = ENDPOINT_WAITERS.lock();
    loop {
        let Some(index) = waiters.iter().position(|waiter| waiter.object == object) else {
            return;
        };
        let waiter = waiters.remove(index);
        if scheduler::wake_process(waiter.process_id) {
            return;
        }
    }
}
