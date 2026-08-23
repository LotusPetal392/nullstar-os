// Direct-child capability bootstrap layered over the general capability entry.
// Endpoint transfer is the normal delegation mechanism once processes share a
// channel; this syscall establishes that first channel without granting a
// process authority over unrelated peers.

mod phase1_protection_abi {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/protection_abi.rs"
    ));
}

impl CapabilityRegistry {
    fn insert_entry_at(
        &mut self,
        process_id: u64,
        object: CapabilityObjectRef,
        rights: u64,
        requested_slot: u64,
    ) -> Result<u64, i64> {
        if requested_slot == abi::capability::INVALID_HANDLE {
            return self.insert_entry(process_id, object, rights);
        }
        if rights == 0 || rights & !capability_allowed_rights(object.kind) != 0 {
            return Err(abi::errno::INVALID_ARGUMENT);
        }
        if self.object_index(object).is_none() {
            return Err(abi::errno::NO_ENTRY);
        }
        let slot = u16::try_from(requested_slot).map_err(|_| abi::errno::INVALID_ARGUMENT)?;
        if slot == 0 || usize::from(slot) > abi::limits::MAX_CAPABILITIES_PER_PROCESS {
            return Err(abi::errno::INVALID_ARGUMENT);
        }
        let table_index = self.ensure_table(process_id)?;
        if self.tables[table_index].entries.len() >= abi::limits::MAX_CAPABILITIES_PER_PROCESS {
            return Err(abi::errno::NO_SPACE);
        }
        if self.tables[table_index].slot_in_use(slot) {
            return Err(abi::errno::NO_SPACE);
        }
        let generation = self.take_handle_generations(1)?[0];
        let handle = capability_handle(slot, generation).ok_or(abi::errno::NO_SPACE)?;
        self.tables[table_index].entries.push(CapabilityEntry {
            handle,
            object,
            rights,
        });
        Ok(handle)
    }
}

global_asm!(
    r#"
    .section .text
    .p2align 4
    .global nullstar_capability_grant_syscall_interrupt_entry
    .type nullstar_capability_grant_syscall_interrupt_entry,@function
nullstar_capability_grant_syscall_interrupt_entry:
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
    call nullstar_capability_grant_syscall_dispatch
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
.size nullstar_capability_grant_syscall_interrupt_entry, .-nullstar_capability_grant_syscall_interrupt_entry
"#
);

unsafe extern "C" {
    fn nullstar_capability_grant_syscall_interrupt_entry();
}

pub fn capability_grant_syscall_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(nullstar_capability_grant_syscall_interrupt_entry as *const () as usize as u64)
}

#[unsafe(no_mangle)]
pub extern "C" fn nullstar_capability_grant_syscall_dispatch(
    current_stack_pointer: usize,
) -> usize {
    let registers_pointer = current_stack_pointer as *mut SavedRegisters;
    let syscall_number = unsafe { (*registers_pointer).rax };
    if syscall_number != phase1_protection_abi::syscall::GRANT_CHILD {
        return nullstar_capability_syscall_dispatch(current_stack_pointer);
    }

    let Some(process_id) = scheduler::current_process_id() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };
    capability_reap_dead_processes();

    let registers = unsafe { &mut *registers_pointer };
    registers.rax = capability_grant_child(
        process_id,
        registers.rdi,
        registers.rsi,
        registers.rdx,
        registers.r10,
    );
    current_stack_pointer
}

fn capability_grant_child(
    owner_process_id: u64,
    child_process_id: u64,
    source_handle: u64,
    rights: u64,
    requested_child_slot: u64,
) -> u64 {
    let direct_live_child = {
        let manager = PROCESS_MANAGER.lock();
        manager.processes.iter().any(|process| {
            process.process_id == child_process_id
                && process.parent_process_id == Some(owner_process_id)
                && process.is_live()
        })
    };
    if !direct_live_child {
        return error_return(abi::errno::NO_CHILD);
    }

    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(source) = registry.entry(owner_process_id, source_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(source, abi::capability::RIGHT_TRANSFER) {
        return error_return(error);
    }
    if rights == 0
        || rights & !source.rights != 0
        || rights & !capability_allowed_rights(source.object.kind) != 0
    {
        return error_return(abi::errno::PERMISSION);
    }

    match registry.insert_entry_at(
        child_process_id,
        source.object,
        rights,
        requested_child_slot,
    ) {
        Ok(handle) => handle,
        Err(error) => error_return(error),
    }
}
