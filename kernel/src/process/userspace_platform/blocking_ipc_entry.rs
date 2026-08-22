// Scheduler-integrated object waits layered over the bounded capability
// data-movement syscalls.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EndpointWaiter {
    object: CapabilityObjectRef,
    target: scheduler::UserExecutionTarget,
}

static ENDPOINT_WAITERS: PreemptMutex<Vec<EndpointWaiter>> = PreemptMutex::new(Vec::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObjectWaitRegistration {
    object: CapabilityObjectRef,
    requested: kernel::object::Signals,
    key: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectWaitReturn {
    Signals,
    Index,
    WaitSetEvent,
    EventPortEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObjectWaiter {
    registrations: Vec<ObjectWaitRegistration>,
    return_kind: ObjectWaitReturn,
    target: scheduler::UserExecutionTarget,
    deadline_ns: u64,
    registers_pointer: usize,
}

static OBJECT_WAITERS: PreemptMutex<Vec<ObjectWaiter>> = PreemptMutex::new(Vec::new());

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

    if matches!(
        syscall_number,
        abi::syscall::OPEN_KERNEL_EARLY_LOG_READER | abi::syscall::KERNEL_EARLY_LOG_READ
    ) {
        let Some(process_id) = scheduler::current_process_id() else {
            unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
            return current_stack_pointer;
        };
        let registers = unsafe { &mut *registers_pointer };
        registers.rax = if syscall_number == abi::syscall::OPEN_KERNEL_EARLY_LOG_READER {
            open_kernel_early_log_reader(process_id)
        } else {
            read_kernel_early_log(
                process_id,
                registers.rdi,
                registers.rsi,
                registers.rdx,
                registers.r10,
            )
        };
        return current_stack_pointer;
    }

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

    if syscall_number == abi::syscall::MONOTONIC_TIME {
        unsafe { (*registers_pointer).rax = crate::interrupts::monotonic_time_ns() };
        return current_stack_pointer;
    }

    if syscall_number == abi::syscall::OBJECT_WAIT_ONE {
        return object_wait_one(current_stack_pointer, registers_pointer);
    }

    if syscall_number == abi::syscall::OBJECT_WAIT_MANY {
        return object_wait_many(current_stack_pointer, registers_pointer);
    }

    if syscall_number == abi::syscall::WAIT_SET_WAIT {
        return wait_set_wait(current_stack_pointer, registers_pointer);
    }

    if syscall_number == abi::syscall::EVENT_PORT_WAIT {
        return event_port_wait(current_stack_pointer, registers_pointer);
    }

    if syscall_number == abi::syscall::ENDPOINT_WAIT {
        return blocking_endpoint_wait(current_stack_pointer, registers_pointer);
    }

    if matches!(
        syscall_number,
        abi::syscall::ENDPOINT_SEND
            | abi::syscall::ENDPOINT_SEND_MOVE
            | abi::syscall::ENDPOINT_SEND_MOVE_MANY
    ) {
        let endpoint_object = scheduler::current_process_id().and_then(|process_id| {
            let endpoint_handle = unsafe { (*registers_pointer).rdi };
            endpoint_send_target_for_handle(
                process_id,
                endpoint_handle,
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

    if matches!(
        syscall_number,
        abi::syscall::ENDPOINT_RECEIVE
            | abi::syscall::ENDPOINT_RECEIVE_MANY
            | abi::syscall::NOTIFICATION_SIGNAL
            | abi::syscall::NOTIFICATION_TRY_WAIT
            | abi::syscall::CAPABILITY_CLOSE
            | abi::syscall::EVENT_PORT_ADD
            | abi::syscall::TIMER_ARM
            | abi::syscall::TIMER_CANCEL
            | abi::syscall::EVENT_SET
            | abi::syscall::EVENT_RESET
    ) {
        let next_stack_pointer =
            nullstar_capability_grant_syscall_dispatch(current_stack_pointer);
        if next_stack_pointer == current_stack_pointer
            && unsafe { ((*registers_pointer).rax as i64) >= 0 }
        {
            wake_satisfied_object_waiters();
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

fn endpoint_send_target_for_handle(
    process_id: u64,
    endpoint_handle: u64,
) -> Result<CapabilityObjectRef, i64> {
    let registry = CAPABILITY_REGISTRY.lock();
    let entry = registry
        .entry(process_id, endpoint_handle)
        .ok_or(abi::errno::BAD_FILE_DESCRIPTOR)?;
    capability_has_right(entry, abi::capability::RIGHT_SEND)?;
    if entry.object.kind != abi::capability::KIND_ENDPOINT {
        return Err(abi::errno::INVALID_ARGUMENT);
    }
    endpoint_destination(&registry, entry.object)
}

fn endpoint_has_message(object: CapabilityObjectRef) -> Result<bool, i64> {
    let registry = CAPABILITY_REGISTRY.lock();
    let object_index = registry.object_index(object).ok_or(abi::errno::IO)?;
    match &registry.objects[object_index].data {
        CapabilityObjectData::Endpoint(endpoint) => {
            if !endpoint.queue.is_empty() {
                return Ok(true);
            }
            if let EndpointPeer::Connected(peer) = endpoint.peer
                && registry.object_index(peer).is_none()
            {
                return Err(abi::errno::BROKEN_PIPE);
            }
            Ok(false)
        }
        CapabilityObjectData::Notification(_)
        | CapabilityObjectData::SharedMemory(_)
        | CapabilityObjectData::KernelEarlyLogReader(_)
        | CapabilityObjectData::Job(_)
        | CapabilityObjectData::WaitSet(_)
        | CapabilityObjectData::EventPort(_)
        | CapabilityObjectData::Timer(_)
        | CapabilityObjectData::Event(_) => Err(abi::errno::INVALID_ARGUMENT),
    }
}

fn object_wait_one(
    current_stack_pointer: usize,
    registers_pointer: *mut SavedRegisters,
) -> usize {
    let Some(target) = scheduler::current_user_execution_target() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };
    let process_id = target.process_id;
    let handle = unsafe { (*registers_pointer).rdi };
    let requested_bits = unsafe { (*registers_pointer).rsi };
    let deadline_ns = unsafe { (*registers_pointer).rdx };
    // This lock is held through state inspection, waiter registration, and
    // scheduler blocking. Mutation paths publish their state before taking
    // this lock, which prevents a readiness transition from being lost.
    let mut waiters = OBJECT_WAITERS.lock();
    waiters.retain(|waiter| waiter.target.execution.thread != target.execution.thread);
    let registry = CAPABILITY_REGISTRY.lock();
    let registration = match resolve_object_wait_registration(
        &registry,
        process_id,
        handle,
        requested_bits,
        0,
    ) {
        Ok(registration) => registration,
        Err(error) => {
            unsafe { (*registers_pointer).rax = error_return(error) };
            return current_stack_pointer;
        }
    };
    let current = match capability_object_signal_state(&registry, registration.object) {
        Ok(signals) => signals,
        Err(error) => {
            unsafe { (*registers_pointer).rax = error_return(error) };
            return current_stack_pointer;
        }
    };
    let asserted = current.bits() & registration.requested.bits();
    if asserted != 0 {
        unsafe { (*registers_pointer).rax = asserted };
        return current_stack_pointer;
    }
    if deadline_ns == abi::deadline::IMMEDIATE
        || (deadline_ns != abi::deadline::INFINITE
            && crate::interrupts::monotonic_time_ns() >= deadline_ns)
    {
        unsafe { (*registers_pointer).rax = error_return(abi::errno::TIMED_OUT) };
        return current_stack_pointer;
    }

    waiters.push(ObjectWaiter {
        registrations: vec![registration],
        return_kind: ObjectWaitReturn::Signals,
        target,
        deadline_ns,
        registers_pointer: registers_pointer as usize,
    });
    unsafe { (*registers_pointer).rax = 0 };
    drop(registry);
    let next_stack_pointer = scheduler::block_current(current_stack_pointer);
    drop(waiters);
    next_stack_pointer
}

fn object_wait_many(
    current_stack_pointer: usize,
    registers_pointer: *mut SavedRegisters,
) -> usize {
    let Some(target) = scheduler::current_user_execution_target() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };
    let process_id = target.process_id;
    let items_address = unsafe { (*registers_pointer).rdi };
    let item_count = match usize::try_from(unsafe { (*registers_pointer).rsi }) {
        Ok(0) => {
            unsafe { (*registers_pointer).rax = error_return(abi::errno::INVALID_ARGUMENT) };
            return current_stack_pointer;
        }
        Ok(count) if count <= abi::limits::MAX_OBJECT_WAIT_ITEMS => count,
        Ok(_) | Err(_) => {
            unsafe { (*registers_pointer).rax = error_return(abi::errno::ARGUMENT_TOO_LARGE) };
            return current_stack_pointer;
        }
    };
    let deadline_ns = unsafe { (*registers_pointer).rdx };
    let byte_length = item_count.saturating_mul(core::mem::size_of::<abi::ObjectWaitItem>());
    if !capability_user_range(process_id, items_address, byte_length, false) {
        unsafe { (*registers_pointer).rax = error_return(abi::errno::BAD_ADDRESS) };
        return current_stack_pointer;
    }
    let item_bytes = unsafe { slice::from_raw_parts(items_address as *const u8, byte_length) };
    let mut items = Vec::with_capacity(item_count);
    for bytes in item_bytes.chunks_exact(core::mem::size_of::<abi::ObjectWaitItem>()) {
        items.push(abi::ObjectWaitItem {
            handle: u64::from_ne_bytes(bytes[..8].try_into().expect("wait handle width")),
            requested_signals: u64::from_ne_bytes(
                bytes[8..16].try_into().expect("wait signal width"),
            ),
        });
    }

    let mut waiters = OBJECT_WAITERS.lock();
    waiters.retain(|waiter| waiter.target.execution.thread != target.execution.thread);
    let registry = CAPABILITY_REGISTRY.lock();
    let mut registrations = Vec::with_capacity(item_count);
    for (index, item) in items.into_iter().enumerate() {
        match resolve_object_wait_registration(
            &registry,
            process_id,
            item.handle,
            item.requested_signals,
            index as u64,
        ) {
            Ok(registration) => registrations.push(registration),
            Err(error) => {
                unsafe { (*registers_pointer).rax = error_return(error) };
                return current_stack_pointer;
            }
        }
    }
    match first_satisfied_object_wait(&registry, &registrations) {
        Ok(Some((index, _))) => {
            unsafe { (*registers_pointer).rax = index as u64 };
            return current_stack_pointer;
        }
        Ok(None) => {}
        Err(error) => {
            unsafe { (*registers_pointer).rax = error_return(error) };
            return current_stack_pointer;
        }
    }
    if deadline_ns == abi::deadline::IMMEDIATE
        || (deadline_ns != abi::deadline::INFINITE
            && crate::interrupts::monotonic_time_ns() >= deadline_ns)
    {
        unsafe { (*registers_pointer).rax = error_return(abi::errno::TIMED_OUT) };
        return current_stack_pointer;
    }

    waiters.push(ObjectWaiter {
        registrations,
        return_kind: ObjectWaitReturn::Index,
        target,
        deadline_ns,
        registers_pointer: registers_pointer as usize,
    });
    unsafe { (*registers_pointer).rax = 0 };
    drop(registry);
    let next_stack_pointer = scheduler::block_current(current_stack_pointer);
    drop(waiters);
    next_stack_pointer
}

fn wait_set_wait(
    current_stack_pointer: usize,
    registers_pointer: *mut SavedRegisters,
) -> usize {
    let Some(target) = scheduler::current_user_execution_target() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };
    let process_id = target.process_id;
    let wait_set_handle = unsafe { (*registers_pointer).rdi };
    let deadline_ns = unsafe { (*registers_pointer).rsi };

    let mut waiters = OBJECT_WAITERS.lock();
    waiters.retain(|waiter| waiter.target.execution.thread != target.execution.thread);
    let registry = CAPABILITY_REGISTRY.lock();
    let entry = match registry.entry(process_id, wait_set_handle) {
        Some(entry) => entry,
        None => {
            unsafe { (*registers_pointer).rax = error_return(abi::errno::BAD_FILE_DESCRIPTOR) };
            return current_stack_pointer;
        }
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_WAIT) {
        unsafe { (*registers_pointer).rax = error_return(error) };
        return current_stack_pointer;
    }
    if entry.object.kind != abi::capability::KIND_WAIT_SET {
        unsafe { (*registers_pointer).rax = error_return(abi::errno::INVALID_ARGUMENT) };
        return current_stack_pointer;
    }
    let Some(object_index) = registry.object_index(entry.object) else {
        unsafe { (*registers_pointer).rax = error_return(abi::errno::IO) };
        return current_stack_pointer;
    };
    let CapabilityObjectData::WaitSet(wait_set) = &registry.objects[object_index].data else {
        unsafe { (*registers_pointer).rax = error_return(abi::errno::INVALID_ARGUMENT) };
        return current_stack_pointer;
    };
    if wait_set.is_empty() {
        unsafe { (*registers_pointer).rax = error_return(abi::errno::INVALID_ARGUMENT) };
        return current_stack_pointer;
    }
    let registrations = wait_set
        .registrations()
        .map(|registration| ObjectWaitRegistration {
            object: CapabilityObjectRef {
                id: registration.target.object_id,
                kind: registration.target.object_kind,
            },
            requested: registration.requested,
            key: registration.key,
        })
        .collect::<Vec<_>>();

    match first_satisfied_object_wait(&registry, &registrations) {
        Ok(Some((index, asserted))) => {
            let result = abi::wait_set::pack_event(registrations[index].key, asserted)
                .unwrap_or_else(|| error_return(abi::errno::IO));
            unsafe { (*registers_pointer).rax = result };
            return current_stack_pointer;
        }
        Ok(None) => {}
        Err(error) => {
            unsafe { (*registers_pointer).rax = error_return(error) };
            return current_stack_pointer;
        }
    }
    if deadline_ns == abi::deadline::IMMEDIATE
        || (deadline_ns != abi::deadline::INFINITE
            && crate::interrupts::monotonic_time_ns() >= deadline_ns)
    {
        unsafe { (*registers_pointer).rax = error_return(abi::errno::TIMED_OUT) };
        return current_stack_pointer;
    }

    waiters.push(ObjectWaiter {
        registrations,
        return_kind: ObjectWaitReturn::WaitSetEvent,
        target,
        deadline_ns,
        registers_pointer: registers_pointer as usize,
    });
    unsafe { (*registers_pointer).rax = 0 };
    drop(registry);
    let next_stack_pointer = scheduler::block_current(current_stack_pointer);
    drop(waiters);
    next_stack_pointer
}

fn event_port_wait(
    current_stack_pointer: usize,
    registers_pointer: *mut SavedRegisters,
) -> usize {
    let Some(target) = scheduler::current_user_execution_target() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };
    let process_id = target.process_id;
    let event_port_handle = unsafe { (*registers_pointer).rdi };
    let deadline_ns = unsafe { (*registers_pointer).rsi };

    let mut waiters = OBJECT_WAITERS.lock();
    waiters.retain(|waiter| waiter.target.execution.thread != target.execution.thread);
    let mut registry = CAPABILITY_REGISTRY.lock();
    if let Err(error) = refresh_event_ports(&mut registry) {
        unsafe { (*registers_pointer).rax = error_return(error) };
        return current_stack_pointer;
    }
    let entry = match registry.entry(process_id, event_port_handle) {
        Some(entry) => entry,
        None => {
            unsafe { (*registers_pointer).rax = error_return(abi::errno::BAD_FILE_DESCRIPTOR) };
            return current_stack_pointer;
        }
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_WAIT) {
        unsafe { (*registers_pointer).rax = error_return(error) };
        return current_stack_pointer;
    }
    if entry.object.kind != abi::capability::KIND_EVENT_PORT {
        unsafe { (*registers_pointer).rax = error_return(abi::errno::INVALID_ARGUMENT) };
        return current_stack_pointer;
    }
    let Some(object_index) = registry.object_index(entry.object) else {
        unsafe { (*registers_pointer).rax = error_return(abi::errno::IO) };
        return current_stack_pointer;
    };
    let CapabilityObjectData::EventPort(event_port) = &mut registry.objects[object_index].data
    else {
        unsafe { (*registers_pointer).rax = error_return(abi::errno::INVALID_ARGUMENT) };
        return current_stack_pointer;
    };
    if let Some(event) = event_port.pop_event() {
        let result = abi::event_port::pack_event(event.key, event.signals.bits())
            .unwrap_or_else(|| error_return(abi::errno::IO));
        unsafe { (*registers_pointer).rax = result };
        return current_stack_pointer;
    }
    if deadline_ns == abi::deadline::IMMEDIATE
        || (deadline_ns != abi::deadline::INFINITE
            && crate::interrupts::monotonic_time_ns() >= deadline_ns)
    {
        unsafe { (*registers_pointer).rax = error_return(abi::errno::TIMED_OUT) };
        return current_stack_pointer;
    }

    waiters.push(ObjectWaiter {
        registrations: vec![ObjectWaitRegistration {
            object: entry.object,
            requested: kernel::object::Signals::READABLE,
            key: 0,
        }],
        return_kind: ObjectWaitReturn::EventPortEvent,
        target,
        deadline_ns,
        registers_pointer: registers_pointer as usize,
    });
    unsafe { (*registers_pointer).rax = 0 };
    drop(registry);
    let next_stack_pointer = scheduler::block_current(current_stack_pointer);
    drop(waiters);
    next_stack_pointer
}

fn resolve_object_wait_registration(
    registry: &CapabilityRegistry,
    process_id: u64,
    handle: u64,
    requested_bits: u64,
    key: u64,
) -> Result<ObjectWaitRegistration, i64> {
    if requested_bits == 0 || requested_bits & !abi::object_signal::ALL != 0 {
        return Err(abi::errno::INVALID_ARGUMENT);
    }
    let requested = kernel::object::Signals::from_bits(requested_bits);
    let entry = registry
        .entry(process_id, handle)
        .ok_or(abi::errno::BAD_FILE_DESCRIPTOR)?;
    capability_has_right(entry, abi::capability::RIGHT_WAIT)?;
    let supported = capability_object_supported_signals(entry.object.kind);
    if requested.bits() & !supported.bits() != 0 {
        return Err(abi::errno::INVALID_ARGUMENT);
    }
    Ok(ObjectWaitRegistration {
        object: entry.object,
        requested,
        key,
    })
}

fn first_satisfied_object_wait(
    registry: &CapabilityRegistry,
    registrations: &[ObjectWaitRegistration],
) -> Result<Option<(usize, u64)>, i64> {
    for (index, registration) in registrations.iter().enumerate() {
        let current = capability_object_signal_state(registry, registration.object)?;
        let asserted = current.bits() & registration.requested.bits();
        if asserted != 0 {
            return Ok(Some((index, asserted)));
        }
    }
    Ok(None)
}

fn object_waiter_result(
    registry: &mut CapabilityRegistry,
    waiter: &ObjectWaiter,
) -> Result<Option<u64>, i64> {
    if waiter.return_kind == ObjectWaitReturn::EventPortEvent {
        let registration = waiter.registrations.first().ok_or(abi::errno::IO)?;
        let object_index = registry
            .object_index(registration.object)
            .ok_or(abi::errno::IO)?;
        let CapabilityObjectData::EventPort(event_port) =
            &mut registry.objects[object_index].data
        else {
            return Err(abi::errno::INVALID_ARGUMENT);
        };
        let Some(event) = event_port.pop_event() else {
            return Ok(None);
        };
        return abi::event_port::pack_event(event.key, event.signals.bits())
            .map(Some)
            .ok_or(abi::errno::IO);
    }
    let Some((index, asserted)) = first_satisfied_object_wait(registry, &waiter.registrations)? else {
        return Ok(None);
    };
    Ok(Some(match waiter.return_kind {
        ObjectWaitReturn::Signals => asserted,
        ObjectWaitReturn::Index => index as u64,
        ObjectWaitReturn::WaitSetEvent => abi::wait_set::pack_event(
            waiter.registrations[index].key,
            asserted,
        )
        .ok_or(abi::errno::IO)?,
        ObjectWaitReturn::EventPortEvent => unreachable!("event-port waits return above"),
    }))
}

fn wake_satisfied_object_waiters() {
    let mut waiters = OBJECT_WAITERS.lock();
    let mut registry = CAPABILITY_REGISTRY.lock();
    let refresh_error = refresh_event_ports(&mut registry).err();
    if waiters.is_empty() {
        return;
    }
    let mut wakeups = Vec::new();
    let mut index = 0usize;
    while index < waiters.len() {
        let return_value = match refresh_error
            .map(Err)
            .unwrap_or_else(|| object_waiter_result(&mut registry, &waiters[index]))
        {
            Ok(None) => {
                index = index.saturating_add(1);
                continue;
            }
            Ok(Some(result)) => result,
            Err(error) => error_return(error),
        };
        let waiter = waiters.remove(index);
        let registers = unsafe { &mut *(waiter.registers_pointer as *mut SavedRegisters) };
        registers.rax = return_value;
        wakeups.push(waiter.target.execution);
    }
    drop(registry);
    drop(waiters);
    for execution in wakeups {
        let _ = scheduler::wake_execution(execution);
    }
}

pub fn service_object_wait_deadlines(now_ns: u64) {
    if advance_timers(now_ns) {
        wake_satisfied_object_waiters();
    }
    let mut waiters = OBJECT_WAITERS.lock();
    let mut wakeups = Vec::new();
    let mut index = 0usize;
    while index < waiters.len() {
        if waiters[index].deadline_ns == abi::deadline::INFINITE
            || now_ns < waiters[index].deadline_ns
        {
            index = index.saturating_add(1);
            continue;
        }
        let waiter = waiters.remove(index);
        let registers = unsafe { &mut *(waiter.registers_pointer as *mut SavedRegisters) };
        registers.rax = error_return(abi::errno::TIMED_OUT);
        wakeups.push(waiter.target.execution);
    }
    drop(waiters);
    for execution in wakeups {
        let _ = scheduler::wake_execution(execution);
    }
}

fn remove_blocking_waiters(process_id: u64) {
    OBJECT_WAITERS
        .lock()
        .retain(|waiter| waiter.target.process_id != process_id);
    ENDPOINT_WAITERS
        .lock()
        .retain(|waiter| waiter.target.process_id != process_id);
}

fn retain_live_blocking_waiters(live_processes: &[u64]) {
    OBJECT_WAITERS
        .lock()
        .retain(|waiter| live_processes.contains(&waiter.target.process_id));
    ENDPOINT_WAITERS
        .lock()
        .retain(|waiter| live_processes.contains(&waiter.target.process_id));
}

fn blocking_endpoint_wait(
    current_stack_pointer: usize,
    registers_pointer: *mut SavedRegisters,
) -> usize {
    let Some(target) = scheduler::current_user_execution_target() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };
    let process_id = target.process_id;
    let endpoint_handle = unsafe { (*registers_pointer).rdi };

    // This lock is held through waiter registration and scheduler blocking.
    // A successful sender acquires it only after its message has been queued,
    // so it cannot miss the transition from runnable to blocked.
    let mut waiters = ENDPOINT_WAITERS.lock();
    waiters.retain(|waiter| waiter.target.execution.thread != target.execution.thread);

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
                target,
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
    {
        let mut waiters = ENDPOINT_WAITERS.lock();
        while let Some(index) = waiters.iter().position(|waiter| waiter.object == object) {
            let waiter = waiters.remove(index);
            if scheduler::wake_execution(waiter.target.execution) {
                break;
            }
        }
    }
    wake_satisfied_object_waiters();
}
