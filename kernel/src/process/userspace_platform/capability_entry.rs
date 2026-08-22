// Outermost syscall entry for the phase-one capability and IPC primitives.
// Existing kernel services remain the fallback path, so this layer can be
// introduced without moving the VFS, drivers, pipes, or shell out of ring 0.

static CAPABILITY_REGISTRY: PreemptMutex<CapabilityRegistry> =
    PreemptMutex::new(CapabilityRegistry::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CapabilityObjectRef {
    id: u64,
    kind: u64,
}

#[derive(Debug, Clone, Copy)]
struct CapabilityEntry {
    handle: u64,
    object: CapabilityObjectRef,
    rights: u64,
}

#[derive(Debug, Clone, Copy)]
struct TransferredCapability {
    object: CapabilityObjectRef,
    rights: u64,
}

#[derive(Debug)]
struct EndpointMessage {
    sender_process_id: u64,
    bytes: Vec<u8>,
    capabilities: Vec<TransferredCapability>,
}

#[derive(Debug)]
struct EndpointObject {
    queue: alloc::collections::VecDeque<EndpointMessage>,
    peer: EndpointPeer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointPeer {
    Loopback,
    Connected(CapabilityObjectRef),
}

#[derive(Debug)]
struct NotificationObject {
    pending: u64,
}

#[derive(Debug)]
struct SharedMemoryObject {
    bytes: Vec<u8>,
}

#[derive(Debug)]
struct KernelEarlyLogReaderObject;

#[derive(Debug)]
enum CapabilityObjectData {
    Endpoint(EndpointObject),
    Notification(NotificationObject),
    SharedMemory(SharedMemoryObject),
    KernelEarlyLogReader(KernelEarlyLogReaderObject),
    Job(kernel::job::State),
    WaitSet(kernel::wait_set::State),
    EventPort(kernel::event_port::State),
    Timer(kernel::timer::State),
    Event(kernel::event::State),
}

#[derive(Debug)]
struct CapabilityObjectRecord {
    reference: CapabilityObjectRef,
    data: CapabilityObjectData,
}

#[derive(Debug)]
struct ProcessCapabilityTable {
    process_id: u64,
    entries: Vec<CapabilityEntry>,
}

impl ProcessCapabilityTable {
    fn new(process_id: u64) -> Self {
        Self {
            process_id,
            entries: Vec::new(),
        }
    }

    fn allocate_handle(&self) -> Option<u64> {
        (1..=abi::limits::MAX_CAPABILITIES_PER_PROCESS as u64)
            .find(|candidate| !self.entries.iter().any(|entry| entry.handle == *candidate))
    }
}

#[derive(Debug)]
struct CapabilityRegistry {
    next_object_id: u64,
    tables: Vec<ProcessCapabilityTable>,
    objects: Vec<CapabilityObjectRecord>,
}

impl CapabilityRegistry {
    const fn new() -> Self {
        Self {
            next_object_id: 1,
            tables: Vec::new(),
            objects: Vec::new(),
        }
    }

    fn table_index(&self, process_id: u64) -> Option<usize> {
        self.tables
            .iter()
            .position(|table| table.process_id == process_id)
    }

    fn ensure_table(&mut self, process_id: u64) -> Result<usize, i64> {
        if let Some(index) = self.table_index(process_id) {
            return Ok(index);
        }
        if self.tables.len() >= MAX_PROCESS_SLOTS {
            return Err(abi::errno::NO_SPACE);
        }
        self.tables.push(ProcessCapabilityTable::new(process_id));
        Ok(self.tables.len() - 1)
    }

    fn entry(&self, process_id: u64, handle: u64) -> Option<CapabilityEntry> {
        self.table_index(process_id).and_then(|index| {
            self.tables[index]
                .entries
                .iter()
                .find(|entry| entry.handle == handle)
                .copied()
        })
    }

    fn insert_entry(
        &mut self,
        process_id: u64,
        object: CapabilityObjectRef,
        rights: u64,
    ) -> Result<u64, i64> {
        if rights == 0 || rights & !capability_allowed_rights(object.kind) != 0 {
            return Err(abi::errno::INVALID_ARGUMENT);
        }
        if self.object_index(object).is_none() {
            return Err(abi::errno::NO_ENTRY);
        }
        let table_index = self.ensure_table(process_id)?;
        let table = &mut self.tables[table_index];
        if table.entries.len() >= abi::limits::MAX_CAPABILITIES_PER_PROCESS {
            return Err(abi::errno::NO_SPACE);
        }
        let handle = table.allocate_handle().ok_or(abi::errno::NO_SPACE)?;
        table.entries.push(CapabilityEntry {
            handle,
            object,
            rights,
        });
        Ok(handle)
    }

    fn remove_entry(&mut self, process_id: u64, handle: u64) -> bool {
        let Some(table_index) = self.table_index(process_id) else {
            return false;
        };
        let Some(entry_index) = self.tables[table_index]
            .entries
            .iter()
            .position(|entry| entry.handle == handle)
        else {
            return false;
        };
        self.tables[table_index].entries.remove(entry_index);
        true
    }

    fn remove_entries(&mut self, process_id: u64, handles: &[u64]) -> bool {
        let Some(table_index) = self.table_index(process_id) else {
            return false;
        };
        if handles.iter().enumerate().any(|(index, handle)| {
            handles[..index].contains(handle)
                || !self.tables[table_index]
                    .entries
                    .iter()
                    .any(|entry| entry.handle == *handle)
        }) {
            return false;
        }
        self.tables[table_index]
            .entries
            .retain(|entry| !handles.contains(&entry.handle));
        true
    }

    fn insert_entries(
        &mut self,
        process_id: u64,
        capabilities: &[TransferredCapability],
    ) -> Result<Vec<u64>, i64> {
        for capability in capabilities {
            if capability.rights == 0
                || capability.rights & !capability_allowed_rights(capability.object.kind) != 0
            {
                return Err(abi::errno::INVALID_ARGUMENT);
            }
            if self.object_index(capability.object).is_none() {
                return Err(abi::errno::NO_ENTRY);
            }
        }

        let table_index = self.ensure_table(process_id)?;
        if self.tables[table_index]
            .entries
            .len()
            .saturating_add(capabilities.len())
            > abi::limits::MAX_CAPABILITIES_PER_PROCESS
        {
            return Err(abi::errno::NO_SPACE);
        }
        let handles = (1..=abi::limits::MAX_CAPABILITIES_PER_PROCESS as u64)
            .filter(|candidate| {
                !self.tables[table_index]
                    .entries
                    .iter()
                    .any(|entry| entry.handle == *candidate)
            })
            .take(capabilities.len())
            .collect::<Vec<_>>();
        if handles.len() != capabilities.len() {
            return Err(abi::errno::NO_SPACE);
        }
        for (handle, capability) in handles.iter().copied().zip(capabilities.iter().copied()) {
            self.tables[table_index].entries.push(CapabilityEntry {
                handle,
                object: capability.object,
                rights: capability.rights,
            });
        }
        Ok(handles)
    }

    fn object_index(&self, reference: CapabilityObjectRef) -> Option<usize> {
        self.objects
            .iter()
            .position(|record| record.reference == reference)
    }

    fn object_kind_count(&self, kind: u64) -> usize {
        self.objects
            .iter()
            .filter(|record| record.reference.kind == kind)
            .count()
    }

    fn shared_memory_bytes(&self) -> usize {
        self.objects
            .iter()
            .filter_map(|record| match &record.data {
                CapabilityObjectData::SharedMemory(memory) => Some(memory.bytes.len()),
                CapabilityObjectData::Endpoint(_)
                | CapabilityObjectData::Notification(_)
                | CapabilityObjectData::KernelEarlyLogReader(_)
                | CapabilityObjectData::Job(_)
                | CapabilityObjectData::WaitSet(_)
                | CapabilityObjectData::EventPort(_)
                | CapabilityObjectData::Timer(_)
                | CapabilityObjectData::Event(_) => None,
            })
            .sum()
    }

    fn create_object(
        &mut self,
        kind: u64,
        data: CapabilityObjectData,
    ) -> Result<CapabilityObjectRef, i64> {
        self.collect_garbage();
        let limit = match kind {
            abi::capability::KIND_ENDPOINT => abi::limits::MAX_ENDPOINT_OBJECTS,
            abi::capability::KIND_NOTIFICATION => abi::limits::MAX_NOTIFICATION_OBJECTS,
            abi::capability::KIND_SHARED_MEMORY => abi::limits::MAX_SHARED_MEMORY_OBJECTS,
            abi::capability::KIND_KERNEL_EARLY_LOG_READER => 1,
            abi::capability::KIND_JOB => abi::limits::MAX_JOB_OBJECTS,
            abi::capability::KIND_WAIT_SET => abi::limits::MAX_WAIT_SET_OBJECTS,
            abi::capability::KIND_EVENT_PORT => abi::limits::MAX_EVENT_PORT_OBJECTS,
            abi::capability::KIND_TIMER => abi::limits::MAX_TIMER_OBJECTS,
            abi::capability::KIND_EVENT => abi::limits::MAX_EVENT_OBJECTS,
            _ => return Err(abi::errno::INVALID_ARGUMENT),
        };
        if self.object_kind_count(kind) >= limit {
            return Err(abi::errno::NO_SPACE);
        }
        let object_id = self.next_object_id;
        self.next_object_id = self
            .next_object_id
            .checked_add(1)
            .ok_or(abi::errno::NO_SPACE)?;
        let reference = CapabilityObjectRef {
            id: object_id,
            kind,
        };
        self.objects
            .push(CapabilityObjectRecord { reference, data });
        Ok(reference)
    }

    fn create_endpoint_pair(
        &mut self,
        process_id: u64,
    ) -> Result<abi::capability::EndpointPair, i64> {
        self.collect_garbage();
        if self.object_kind_count(abi::capability::KIND_ENDPOINT).saturating_add(2)
            > abi::limits::MAX_ENDPOINT_OBJECTS
        {
            return Err(abi::errno::NO_SPACE);
        }

        let table_index = self.ensure_table(process_id)?;
        if self.tables[table_index].entries.len().saturating_add(2)
            > abi::limits::MAX_CAPABILITIES_PER_PROCESS
        {
            return Err(abi::errno::NO_SPACE);
        }
        let handles = (1..=abi::limits::MAX_CAPABILITIES_PER_PROCESS as u64)
            .filter(|candidate| {
                !self.tables[table_index]
                    .entries
                    .iter()
                    .any(|entry| entry.handle == *candidate)
            })
            .take(2)
            .collect::<Vec<_>>();
        if handles.len() != 2 {
            return Err(abi::errno::NO_SPACE);
        }

        let first_id = self.next_object_id;
        let second_id = first_id.checked_add(1).ok_or(abi::errno::NO_SPACE)?;
        self.next_object_id = first_id.checked_add(2).ok_or(abi::errno::NO_SPACE)?;
        let first_object = CapabilityObjectRef {
            id: first_id,
            kind: abi::capability::KIND_ENDPOINT,
        };
        let second_object = CapabilityObjectRef {
            id: second_id,
            kind: abi::capability::KIND_ENDPOINT,
        };
        self.objects.push(CapabilityObjectRecord {
            reference: first_object,
            data: CapabilityObjectData::Endpoint(EndpointObject {
                queue: alloc::collections::VecDeque::with_capacity(
                    abi::limits::MAX_ENDPOINT_MESSAGES,
                ),
                peer: EndpointPeer::Connected(second_object),
            }),
        });
        self.objects.push(CapabilityObjectRecord {
            reference: second_object,
            data: CapabilityObjectData::Endpoint(EndpointObject {
                queue: alloc::collections::VecDeque::with_capacity(
                    abi::limits::MAX_ENDPOINT_MESSAGES,
                ),
                peer: EndpointPeer::Connected(first_object),
            }),
        });
        self.tables[table_index].entries.push(CapabilityEntry {
            handle: handles[0],
            object: first_object,
            rights: abi::capability::ENDPOINT_RIGHTS,
        });
        self.tables[table_index].entries.push(CapabilityEntry {
            handle: handles[1],
            object: second_object,
            rights: abi::capability::ENDPOINT_RIGHTS,
        });
        Ok(abi::capability::EndpointPair {
            first: handles[0],
            second: handles[1],
        })
    }

    fn collect_garbage(&mut self) {
        let mut reachable = Vec::<CapabilityObjectRef>::new();
        for table in &self.tables {
            for entry in &table.entries {
                push_unique_object(&mut reachable, entry.object);
            }
        }
        for object in kernel_capability_roots_snapshot() {
            push_unique_object(&mut reachable, object);
        }

        let mut cursor = 0usize;
        while cursor < reachable.len() {
            let reference = reachable[cursor];
            let linked = self
                .object_index(reference)
                .and_then(|index| match &self.objects[index].data {
                    CapabilityObjectData::Endpoint(endpoint) => Some(
                        endpoint
                            .queue
                            .iter()
                            .flat_map(|message| {
                                message
                                    .capabilities
                                    .iter()
                                    .map(|capability| capability.object)
                            })
                            .collect::<Vec<_>>(),
                    ),
                    CapabilityObjectData::Job(job) => Some(
                        job.parent()
                            .into_iter()
                            .chain(job.children())
                            .map(|id| CapabilityObjectRef {
                                id,
                                kind: abi::capability::KIND_JOB,
                            })
                            .collect::<Vec<_>>(),
                    ),
                    CapabilityObjectData::WaitSet(wait_set) => Some(
                        wait_set
                            .registrations()
                            .map(|registration| CapabilityObjectRef {
                                id: registration.target.object_id,
                                kind: registration.target.object_kind,
                            })
                            .collect::<Vec<_>>(),
                    ),
                    CapabilityObjectData::EventPort(event_port) => Some(
                        event_port
                            .registrations()
                            .map(|registration| CapabilityObjectRef {
                                id: registration.target.object_id,
                                kind: registration.target.object_kind,
                            })
                            .collect::<Vec<_>>(),
                    ),
                    CapabilityObjectData::Notification(_)
                    | CapabilityObjectData::SharedMemory(_)
                    | CapabilityObjectData::KernelEarlyLogReader(_)
                    | CapabilityObjectData::Timer(_)
                    | CapabilityObjectData::Event(_) => None,
                })
                .unwrap_or_default();
            for object in linked {
                push_unique_object(&mut reachable, object);
            }
            cursor = cursor.saturating_add(1);
        }

        self.objects
            .retain(|record| reachable.contains(&record.reference));
    }

    fn job_subtree(&self, root: CapabilityObjectRef) -> Result<Vec<CapabilityObjectRef>, i64> {
        if root.kind != abi::capability::KIND_JOB {
            return Err(abi::errno::INVALID_ARGUMENT);
        }
        let mut jobs = Vec::new();
        jobs.push(root);
        let mut cursor = 0usize;
        while cursor < jobs.len() {
            let current = jobs[cursor];
            let Some(index) = self.object_index(current) else {
                return Err(abi::errno::IO);
            };
            let CapabilityObjectData::Job(job) = &self.objects[index].data else {
                return Err(abi::errno::INVALID_ARGUMENT);
            };
            for id in job.children() {
                let child = CapabilityObjectRef {
                    id,
                    kind: abi::capability::KIND_JOB,
                };
                if jobs.contains(&child) {
                    return Err(abi::errno::IO);
                }
                jobs.push(child);
            }
            cursor = cursor.saturating_add(1);
        }
        Ok(jobs)
    }

    fn job_subtree_active_members(&self, root: CapabilityObjectRef) -> Result<usize, i64> {
        let mut active = 0usize;
        for job in self.job_subtree(root)? {
            let Some(index) = self.object_index(job) else {
                return Err(abi::errno::IO);
            };
            let CapabilityObjectData::Job(state) = &self.objects[index].data else {
                return Err(abi::errno::INVALID_ARGUMENT);
            };
            active = active
                .checked_add(state.active_members())
                .ok_or(abi::errno::RANGE)?;
        }
        Ok(active)
    }

    fn job_ancestors_inclusive(
        &self,
        leaf: CapabilityObjectRef,
    ) -> Result<Vec<CapabilityObjectRef>, i64> {
        if leaf.kind != abi::capability::KIND_JOB {
            return Err(abi::errno::INVALID_ARGUMENT);
        }
        let mut jobs = Vec::new();
        let mut current = Some(leaf);
        while let Some(job) = current {
            if jobs.contains(&job) {
                return Err(abi::errno::IO);
            }
            jobs.push(job);
            let Some(index) = self.object_index(job) else {
                return Err(abi::errno::IO);
            };
            let CapabilityObjectData::Job(state) = &self.objects[index].data else {
                return Err(abi::errno::INVALID_ARGUMENT);
            };
            current = state.parent().map(|id| CapabilityObjectRef {
                id,
                kind: abi::capability::KIND_JOB,
            });
        }
        Ok(jobs)
    }

    fn job_admits_process(&self, leaf: CapabilityObjectRef) -> Result<bool, i64> {
        for job in self.job_ancestors_inclusive(leaf)? {
            let Some(index) = self.object_index(job) else {
                return Err(abi::errno::IO);
            };
            let CapabilityObjectData::Job(state) = &self.objects[index].data else {
                return Err(abi::errno::INVALID_ARGUMENT);
            };
            if state.is_retired() {
                return Ok(false);
            }
            let limit = state.process_limit();
            if self.job_subtree_active_members(job)? >= limit {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn push_unique_object(objects: &mut Vec<CapabilityObjectRef>, object: CapabilityObjectRef) {
    if !objects.contains(&object) {
        objects.push(object);
    }
}

fn capability_allowed_rights(kind: u64) -> u64 {
    match kind {
        abi::capability::KIND_ENDPOINT => abi::capability::ENDPOINT_RIGHTS,
        abi::capability::KIND_NOTIFICATION => abi::capability::NOTIFICATION_RIGHTS,
        abi::capability::KIND_SHARED_MEMORY => abi::capability::SHARED_MEMORY_RIGHTS,
        abi::capability::KIND_KERNEL_EARLY_LOG_READER => {
            abi::capability::KERNEL_EARLY_LOG_READER_RIGHTS
        }
        abi::capability::KIND_JOB => abi::capability::JOB_RIGHTS,
        abi::capability::KIND_WAIT_SET => abi::capability::WAIT_SET_RIGHTS,
        abi::capability::KIND_EVENT_PORT => abi::capability::EVENT_PORT_RIGHTS,
        abi::capability::KIND_TIMER => abi::capability::TIMER_RIGHTS,
        abi::capability::KIND_EVENT => abi::capability::EVENT_RIGHTS,
        _ => 0,
    }
}

fn capability_has_right(entry: CapabilityEntry, right: u64) -> Result<(), i64> {
    if entry.rights & right == right {
        Ok(())
    } else {
        Err(abi::errno::PERMISSION)
    }
}

fn capability_reap_dead_processes() {
    let live_processes = {
        let manager = PROCESS_MANAGER.lock();
        manager
            .processes
            .iter()
            .filter(|process| process.is_live())
            .map(|process| process.process_id)
            .collect::<Vec<_>>()
    };
    retain_live_blocking_waiters(&live_processes);
    let mut registry = CAPABILITY_REGISTRY.lock();
    registry
        .tables
        .retain(|table| live_processes.contains(&table.process_id));
    registry.collect_garbage();
    let peer_closed = peer_closed_endpoint_objects(&registry);
    drop(registry);
    for object in peer_closed {
        wake_endpoint_waiter(object);
    }
}

fn capability_remove_process(process_id: u64) {
    let peer_closed = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        registry
            .tables
            .retain(|table| table.process_id != process_id);
        registry.collect_garbage();
        peer_closed_endpoint_objects(&registry)
    };
    for object in peer_closed {
        wake_endpoint_waiter(object);
    }
}

fn peer_closed_endpoint_objects(registry: &CapabilityRegistry) -> Vec<CapabilityObjectRef> {
    registry
        .objects
        .iter()
        .filter_map(|record| match &record.data {
            CapabilityObjectData::Endpoint(EndpointObject {
                peer: EndpointPeer::Connected(peer),
                ..
            }) if registry.object_index(*peer).is_none() => Some(record.reference),
            _ => None,
        })
        .collect()
}

fn endpoint_destination(
    registry: &CapabilityRegistry,
    endpoint: CapabilityObjectRef,
) -> Result<CapabilityObjectRef, i64> {
    let index = registry.object_index(endpoint).ok_or(abi::errno::IO)?;
    let CapabilityObjectData::Endpoint(endpoint_data) = &registry.objects[index].data else {
        return Err(abi::errno::INVALID_ARGUMENT);
    };
    let destination = match endpoint_data.peer {
        EndpointPeer::Loopback => endpoint,
        EndpointPeer::Connected(peer) => peer,
    };
    let Some(destination_index) = registry.object_index(destination) else {
        return Err(abi::errno::BROKEN_PIPE);
    };
    if matches!(
        registry.objects[destination_index].data,
        CapabilityObjectData::Endpoint(_)
    ) {
        Ok(destination)
    } else {
        Err(abi::errno::IO)
    }
}

fn capability_user_length(length: u64, maximum: usize) -> Result<usize, i64> {
    let length = usize::try_from(length).map_err(|_| abi::errno::RANGE)?;
    if length > maximum {
        Err(abi::errno::ARGUMENT_TOO_LARGE)
    } else {
        Ok(length)
    }
}

fn capability_user_range(process_id: u64, address: u64, length: usize, writable: bool) -> bool {
    length == 0 || user_range_allows(process_id, address, length, writable)
}

fn capability_read_message(process_id: u64, address: u64, length: u64) -> Result<Vec<u8>, i64> {
    let length = capability_user_length(length, abi::limits::MAX_IPC_MESSAGE_BYTES)?;
    if !capability_user_range(process_id, address, length, false) {
        return Err(abi::errno::BAD_ADDRESS);
    }
    if length == 0 {
        return Ok(Vec::new());
    }
    Ok(unsafe { slice::from_raw_parts(address as *const u8, length) }.to_vec())
}

fn capability_object_size(
    registry: &CapabilityRegistry,
    record: &CapabilityObjectRecord,
) -> Result<u64, i64> {
    match &record.data {
        CapabilityObjectData::Endpoint(endpoint) => Ok(endpoint.queue.len() as u64),
        CapabilityObjectData::Notification(notification) => Ok(notification.pending),
        CapabilityObjectData::SharedMemory(memory) => Ok(memory.bytes.len() as u64),
        CapabilityObjectData::KernelEarlyLogReader(_) => {
            Ok(crate::early_log::KERNEL_EARLY_LOG_CAPACITY as u64)
        }
        CapabilityObjectData::Job(_) => registry
            .job_subtree_active_members(record.reference)
            .and_then(|members| u64::try_from(members).map_err(|_| abi::errno::RANGE)),
        CapabilityObjectData::WaitSet(wait_set) => Ok(wait_set.len() as u64),
        CapabilityObjectData::EventPort(event_port) => Ok(event_port.queued_len() as u64),
        CapabilityObjectData::Timer(timer) => Ok(u64::from(timer.is_armed())),
        CapabilityObjectData::Event(event) => Ok(u64::from(event.is_signaled())),
    }
}

global_asm!(
    r#"
    .section .text
    .p2align 4
    .global nullstar_capability_syscall_interrupt_entry
    .type nullstar_capability_syscall_interrupt_entry,@function
nullstar_capability_syscall_interrupt_entry:
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
    call nullstar_capability_syscall_dispatch
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
.size nullstar_capability_syscall_interrupt_entry, .-nullstar_capability_syscall_interrupt_entry
"#
);

unsafe extern "C" {
    fn nullstar_capability_syscall_interrupt_entry();
}

pub fn capability_syscall_interrupt_entry_address() -> VirtAddr {
    VirtAddr::new(nullstar_capability_syscall_interrupt_entry as *const () as usize as u64)
}

fn capability_syscall_number(number: u64) -> bool {
    matches!(
        number,
        abi::syscall::CAPABILITY_DUPLICATE
            | abi::syscall::CAPABILITY_CLOSE
            | abi::syscall::CAPABILITY_INFO
            | abi::syscall::CAPABILITY_REPLACE
            | abi::syscall::CAPABILITY_SIGNAL_STATE
            | abi::syscall::ENDPOINT_CREATE
            | abi::syscall::ENDPOINT_CREATE_PAIR
            | abi::syscall::ENDPOINT_SEND
            | abi::syscall::ENDPOINT_SEND_MOVE
            | abi::syscall::ENDPOINT_SEND_MOVE_MANY
            | abi::syscall::ENDPOINT_RECEIVE
            | abi::syscall::ENDPOINT_RECEIVE_MANY
            | abi::syscall::NOTIFICATION_CREATE
            | abi::syscall::NOTIFICATION_SIGNAL
            | abi::syscall::NOTIFICATION_TRY_WAIT
            | abi::syscall::SHARED_MEMORY_CREATE
            | abi::syscall::SHARED_MEMORY_READ
            | abi::syscall::SHARED_MEMORY_WRITE
            | abi::syscall::OPEN_BLOCK_DEVICE_ENDPOINT
            | abi::syscall::OPEN_WRITABLE_BLOCK_DEVICE_ENDPOINT
            | abi::syscall::OPEN_WRITABLE_NULLFS_BLOCK_DEVICE_ENDPOINT
            | abi::syscall::OFFLINE_WRITABLE_NULLFS_BLOCK_DEVICE_ENDPOINT
            | abi::syscall::JOB_CREATE
            | abi::syscall::JOB_ASSIGN
            | abi::syscall::JOB_TRY_WAIT
            | abi::syscall::JOB_TERMINATE
            | abi::syscall::JOB_CREATE_CHILD
            | abi::syscall::JOB_SET_PROCESS_LIMIT
            | abi::syscall::JOB_RETIRE
            | abi::syscall::JOB_GET_PROCESS_LIMIT
            | abi::syscall::WAIT_SET_CREATE
            | abi::syscall::WAIT_SET_ADD
            | abi::syscall::WAIT_SET_REMOVE
            | abi::syscall::EVENT_PORT_CREATE
            | abi::syscall::EVENT_PORT_ADD
            | abi::syscall::EVENT_PORT_REMOVE
            | abi::syscall::TIMER_CREATE
            | abi::syscall::TIMER_ARM
            | abi::syscall::TIMER_CANCEL
            | abi::syscall::EVENT_CREATE
            | abi::syscall::EVENT_SET
            | abi::syscall::EVENT_RESET
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn nullstar_capability_syscall_dispatch(current_stack_pointer: usize) -> usize {
    let registers_pointer = current_stack_pointer as *mut SavedRegisters;
    let syscall_number = unsafe { (*registers_pointer).rax };
    if syscall_number != abi::syscall::SYSTEM_INFO && !capability_syscall_number(syscall_number) {
        return galactic_process_group_syscall_dispatch(current_stack_pointer);
    }

    let Some(process_id) = scheduler::current_process_id() else {
        unsafe { (*registers_pointer).rax = error_return(ERR_NOT_IMPLEMENTED) };
        return current_stack_pointer;
    };

    if syscall_number != abi::syscall::SYSTEM_INFO {
        capability_reap_dead_processes();
    }

    let registers = unsafe { &mut *registers_pointer };
    registers.rax = match syscall_number {
        abi::syscall::SYSTEM_INFO => {
            capability_system_info(process_id, registers.rdi, registers.rsi)
        }
        abi::syscall::CAPABILITY_DUPLICATE => {
            capability_duplicate(process_id, registers.rdi, registers.rsi)
        }
        abi::syscall::CAPABILITY_CLOSE => capability_close(process_id, registers.rdi),
        abi::syscall::CAPABILITY_INFO => {
            capability_info(process_id, registers.rdi, registers.rsi, registers.rdx)
        }
        abi::syscall::CAPABILITY_REPLACE => {
            capability_replace(process_id, registers.rdi, registers.rsi)
        }
        abi::syscall::CAPABILITY_SIGNAL_STATE => {
            capability_signal_state(process_id, registers.rdi)
        }
        abi::syscall::ENDPOINT_CREATE => endpoint_create(process_id),
        abi::syscall::ENDPOINT_CREATE_PAIR => {
            endpoint_create_pair(process_id, registers.rdi, registers.rsi)
        }
        abi::syscall::ENDPOINT_SEND => endpoint_send(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
            registers.r8,
        ),
        abi::syscall::ENDPOINT_SEND_MOVE => endpoint_send_move(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
            registers.r8,
        ),
        abi::syscall::ENDPOINT_SEND_MOVE_MANY => endpoint_send_move_many(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
            registers.r8,
        ),
        abi::syscall::ENDPOINT_RECEIVE => endpoint_receive(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
        ),
        abi::syscall::ENDPOINT_RECEIVE_MANY => endpoint_receive_many(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
            registers.r8,
            registers.r9,
        ),
        abi::syscall::NOTIFICATION_CREATE => notification_create(process_id),
        abi::syscall::NOTIFICATION_SIGNAL => {
            notification_signal(process_id, registers.rdi, registers.rsi)
        }
        abi::syscall::NOTIFICATION_TRY_WAIT => notification_try_wait(process_id, registers.rdi),
        abi::syscall::SHARED_MEMORY_CREATE => shared_memory_create(process_id, registers.rdi),
        abi::syscall::SHARED_MEMORY_READ => shared_memory_read(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
        ),
        abi::syscall::SHARED_MEMORY_WRITE => shared_memory_write(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
        ),
        abi::syscall::OPEN_BLOCK_DEVICE_ENDPOINT => {
            open_block_device_endpoint(process_id, registers.rdi, BlockDeviceAccess::ReadOnly)
        }
        abi::syscall::OPEN_WRITABLE_BLOCK_DEVICE_ENDPOINT => {
            open_block_device_endpoint(process_id, registers.rdi, BlockDeviceAccess::Writable)
        }
        abi::syscall::OPEN_WRITABLE_NULLFS_BLOCK_DEVICE_ENDPOINT => {
            open_writable_nullfs_block_device_endpoint(process_id, registers.rdi, registers.rsi)
        }
        abi::syscall::OFFLINE_WRITABLE_NULLFS_BLOCK_DEVICE_ENDPOINT => {
            offline_writable_nullfs_block_device_endpoint(
                process_id,
                registers.rdi,
                registers.rsi,
                registers.rdx,
            )
        }
        abi::syscall::JOB_CREATE => job_create(process_id),
        abi::syscall::JOB_ASSIGN => job_assign(process_id, registers.rdi, registers.rsi),
        abi::syscall::JOB_TRY_WAIT => {
            job_try_wait(process_id, registers.rdi, registers.rsi, registers.rdx)
        }
        abi::syscall::JOB_TERMINATE => job_terminate(process_id, registers.rdi),
        abi::syscall::JOB_CREATE_CHILD => job_create_child(process_id, registers.rdi),
        abi::syscall::JOB_SET_PROCESS_LIMIT => {
            job_set_process_limit(process_id, registers.rdi, registers.rsi)
        }
        abi::syscall::JOB_RETIRE => job_retire(process_id, registers.rdi),
        abi::syscall::JOB_GET_PROCESS_LIMIT => job_get_process_limit(process_id, registers.rdi),
        abi::syscall::WAIT_SET_CREATE => wait_set_create(process_id),
        abi::syscall::WAIT_SET_ADD => wait_set_add(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
        ),
        abi::syscall::WAIT_SET_REMOVE => {
            wait_set_remove(process_id, registers.rdi, registers.rsi)
        }
        abi::syscall::EVENT_PORT_CREATE => event_port_create(process_id),
        abi::syscall::EVENT_PORT_ADD => event_port_add(
            process_id,
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
        ),
        abi::syscall::EVENT_PORT_REMOVE => {
            event_port_remove(process_id, registers.rdi, registers.rsi)
        }
        abi::syscall::TIMER_CREATE => timer_create(process_id),
        abi::syscall::TIMER_ARM => timer_arm(process_id, registers.rdi, registers.rsi),
        abi::syscall::TIMER_CANCEL => timer_cancel(process_id, registers.rdi),
        abi::syscall::EVENT_CREATE => event_create(process_id),
        abi::syscall::EVENT_SET => event_set(process_id, registers.rdi),
        abi::syscall::EVENT_RESET => event_reset(process_id, registers.rdi),
        _ => error_return(ERR_NOT_IMPLEMENTED),
    };
    current_stack_pointer
}

fn capability_system_info(process_id: u64, address: u64, length: u64) -> u64 {
    let info = abi::SystemInfo {
        abi_major: abi::ABI_VERSION_MAJOR,
        abi_minor: abi::ABI_VERSION_MINOR,
        capabilities: abi::capability::PLATFORM_V1 | abi::capability::PROTECTION_V1,
        page_size: PLATFORM_PAGE_SIZE,
        maximum_open_files: MAX_OPEN_FILES as u64,
        maximum_path_bytes: abi::limits::MAX_PATH_BYTES as u64,
        maximum_directory_entries: abi::limits::MAX_DIRECTORY_ENTRIES_PER_CALL as u64,
        init_process_id: INIT_PROCESS_ID,
    };
    platform_write_value(process_id, address, length, info)
}

fn capability_duplicate(process_id: u64, handle: u64, rights: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(source) = registry.entry(process_id, handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(source, abi::capability::RIGHT_DUPLICATE) {
        return error_return(error);
    }
    if rights == 0
        || rights & !source.rights != 0
        || rights & !capability_allowed_rights(source.object.kind) != 0
    {
        return error_return(abi::errno::PERMISSION);
    }
    match registry.insert_entry(process_id, source.object, rights) {
        Ok(handle) => handle,
        Err(error) => error_return(error),
    }
}

fn capability_replace(process_id: u64, handle: u64, rights: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(source) = registry.entry(process_id, handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(source, abi::capability::RIGHT_DUPLICATE) {
        return error_return(error);
    }
    if rights == 0
        || rights & !source.rights != 0
        || rights & !capability_allowed_rights(source.object.kind) != 0
    {
        return error_return(abi::errno::PERMISSION);
    }
    let Some(table_index) = registry.table_index(process_id) else {
        return error_return(abi::errno::IO);
    };
    let Some(replacement) = registry.tables[table_index]
        .entries
        .iter_mut()
        .find(|entry| entry.handle == handle)
    else {
        return error_return(abi::errno::IO);
    };
    replacement.rights = rights;
    handle
}

fn capability_close(process_id: u64, handle: u64) -> u64 {
    if handle == abi::capability::INVALID_HANDLE {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    }
    let peer_closed = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        if !registry.remove_entry(process_id, handle) {
            return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
        }
        registry.collect_garbage();
        peer_closed_endpoint_objects(&registry)
    };
    for object in peer_closed {
        wake_endpoint_waiter(object);
    }
    0
}

fn capability_info(process_id: u64, handle: u64, address: u64, length: u64) -> u64 {
    let info = {
        let registry = CAPABILITY_REGISTRY.lock();
        let Some(entry) = registry.entry(process_id, handle) else {
            return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
        };
        let Some(index) = registry.object_index(entry.object) else {
            return error_return(abi::errno::IO);
        };
        let record = &registry.objects[index];
        let size = match capability_object_size(&registry, record) {
            Ok(size) => size,
            Err(error) => return error_return(error),
        };
        abi::capability::Info {
            object_id: entry.object.id,
            kind: entry.object.kind,
            rights: entry.rights,
            size,
        }
    };
    platform_write_value(process_id, address, length, info)
}

fn capability_signal_state(process_id: u64, handle: u64) -> u64 {
    let registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_WAIT) {
        return error_return(error);
    }
    match capability_object_signal_state(&registry, entry.object) {
        Ok(signals) => signals.bits(),
        Err(error) => error_return(error),
    }
}

fn capability_object_supported_signals(kind: u64) -> kernel::object::Signals {
    match kind {
        abi::capability::KIND_ENDPOINT => kernel::object::Signals::READABLE
            .union(kernel::object::Signals::WRITABLE)
            .union(kernel::object::Signals::PEER_CLOSED),
        abi::capability::KIND_NOTIFICATION => kernel::object::Signals::SIGNALED,
        abi::capability::KIND_JOB => kernel::object::Signals::READABLE
            .union(kernel::object::Signals::TERMINATED),
        abi::capability::KIND_EVENT_PORT => kernel::object::Signals::READABLE,
        abi::capability::KIND_TIMER => kernel::object::Signals::TIMER_FIRED,
        abi::capability::KIND_EVENT => kernel::object::Signals::SIGNALED,
        _ => kernel::object::Signals::NONE,
    }
}

fn capability_object_signal_state(
    registry: &CapabilityRegistry,
    object: CapabilityObjectRef,
) -> Result<kernel::object::Signals, i64> {
    let Some(index) = registry.object_index(object) else {
        return Err(abi::errno::IO);
    };

    match &registry.objects[index].data {
        CapabilityObjectData::Endpoint(endpoint) => {
            let mut signals = kernel::object::Signals::NONE;
            if !endpoint.queue.is_empty() {
                signals = signals.union(kernel::object::Signals::READABLE);
            }
            match endpoint.peer {
                EndpointPeer::Loopback => {
                    if endpoint.queue.len() < abi::limits::MAX_ENDPOINT_MESSAGES {
                        signals = signals.union(kernel::object::Signals::WRITABLE);
                    }
                }
                EndpointPeer::Connected(peer) => {
                    let Some(peer_index) = registry.object_index(peer) else {
                        return Ok(signals.union(kernel::object::Signals::PEER_CLOSED));
                    };
                    let CapabilityObjectData::Endpoint(peer) = &registry.objects[peer_index].data
                    else {
                        return Err(abi::errno::IO);
                    };
                    if peer.queue.len() < abi::limits::MAX_ENDPOINT_MESSAGES {
                        signals = signals.union(kernel::object::Signals::WRITABLE);
                    }
                }
            }
            Ok(signals)
        }
        CapabilityObjectData::Notification(notification) => {
            if notification.pending == 0 {
                Ok(kernel::object::Signals::NONE)
            } else {
                Ok(kernel::object::Signals::SIGNALED)
            }
        }
        CapabilityObjectData::Job(_) => {
            let jobs = registry.job_subtree(object)?;
            let mut active_members = 0usize;
            let mut has_completion = false;
            for job in jobs {
                let Some(index) = registry.object_index(job) else {
                    return Err(abi::errno::IO);
                };
                let CapabilityObjectData::Job(state) = &registry.objects[index].data else {
                    return Err(abi::errno::INVALID_ARGUMENT);
                };
                active_members = active_members.saturating_add(state.active_members());
                has_completion |= state.pending_completions() != 0;
            }
            let mut signals = kernel::object::Signals::NONE;
            if has_completion {
                signals = signals.union(kernel::object::Signals::READABLE);
            }
            if active_members == 0 {
                signals = signals.union(kernel::object::Signals::TERMINATED);
            }
            Ok(signals)
        }
        CapabilityObjectData::EventPort(event_port) => {
            if event_port.is_readable() {
                Ok(kernel::object::Signals::READABLE)
            } else {
                Ok(kernel::object::Signals::NONE)
            }
        }
        CapabilityObjectData::Timer(timer) => {
            if timer.is_fired() {
                Ok(kernel::object::Signals::TIMER_FIRED)
            } else {
                Ok(kernel::object::Signals::NONE)
            }
        }
        CapabilityObjectData::Event(event) => {
            if event.is_signaled() {
                Ok(kernel::object::Signals::SIGNALED)
            } else {
                Ok(kernel::object::Signals::NONE)
            }
        }
        CapabilityObjectData::SharedMemory(_)
        | CapabilityObjectData::KernelEarlyLogReader(_)
        | CapabilityObjectData::WaitSet(_) => {
            Err(abi::errno::INVALID_ARGUMENT)
        }
    }
}

fn wait_set_create(process_id: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let object = match registry.create_object(
        abi::capability::KIND_WAIT_SET,
        CapabilityObjectData::WaitSet(kernel::wait_set::State::new(
            abi::limits::MAX_WAIT_SET_REGISTRATIONS,
            abi::wait_set::MAX_KEY,
        )),
    ) {
        Ok(object) => object,
        Err(error) => return error_return(error),
    };
    match registry.insert_entry(process_id, object, abi::capability::WAIT_SET_RIGHTS) {
        Ok(handle) => handle,
        Err(error) => {
            registry.collect_garbage();
            error_return(error)
        }
    }
}

fn wait_set_add(
    process_id: u64,
    wait_set_handle: u64,
    target_handle: u64,
    requested_bits: u64,
    key: u64,
) -> u64 {
    if requested_bits == 0 || requested_bits & !abi::object_signal::ALL != 0 {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let requested = kernel::object::Signals::from_bits(requested_bits);
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(wait_set_entry) = registry.entry(process_id, wait_set_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(wait_set_entry, abi::capability::RIGHT_MANAGE) {
        return error_return(error);
    }
    if wait_set_entry.object.kind != abi::capability::KIND_WAIT_SET {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(target_entry) = registry.entry(process_id, target_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(target_entry, abi::capability::RIGHT_WAIT) {
        return error_return(error);
    }
    let supported = capability_object_supported_signals(target_entry.object.kind);
    if requested.bits() & !supported.bits() != 0 {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(index) = registry.object_index(wait_set_entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::WaitSet(wait_set) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    let registration = kernel::wait_set::Registration {
        target: kernel::wait_set::Target {
            object_id: target_entry.object.id,
            object_kind: target_entry.object.kind,
        },
        requested,
        key,
    };
    match wait_set.add(registration) {
        Ok(()) => 0,
        Err(kernel::wait_set::AddError::Full) => error_return(abi::errno::NO_SPACE),
        Err(
            kernel::wait_set::AddError::InvalidTarget
            | kernel::wait_set::AddError::InvalidSignals
            | kernel::wait_set::AddError::InvalidKey
            | kernel::wait_set::AddError::DuplicateKey,
        ) => error_return(abi::errno::INVALID_ARGUMENT),
    }
}

fn wait_set_remove(process_id: u64, wait_set_handle: u64, key: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, wait_set_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_MANAGE) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_WAIT_SET {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(index) = registry.object_index(entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::WaitSet(wait_set) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    match wait_set.remove(key) {
        Ok(_) => {
            registry.collect_garbage();
            0
        }
        Err(_) => error_return(abi::errno::NO_ENTRY),
    }
}

fn event_port_create(process_id: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let object = match registry.create_object(
        abi::capability::KIND_EVENT_PORT,
        CapabilityObjectData::EventPort(kernel::event_port::State::new(
            abi::limits::MAX_EVENT_PORT_REGISTRATIONS,
            abi::limits::MAX_EVENT_PORT_EVENTS,
            abi::event_port::MAX_KEY,
        )),
    ) {
        Ok(object) => object,
        Err(error) => return error_return(error),
    };
    match registry.insert_entry(process_id, object, abi::capability::EVENT_PORT_RIGHTS) {
        Ok(handle) => handle,
        Err(error) => {
            registry.collect_garbage();
            error_return(error)
        }
    }
}

fn event_port_add(
    process_id: u64,
    event_port_handle: u64,
    target_handle: u64,
    requested_bits: u64,
    key: u64,
) -> u64 {
    if requested_bits == 0 || requested_bits & !abi::object_signal::ALL != 0 {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let requested = kernel::object::Signals::from_bits(requested_bits);
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(event_port_entry) = registry.entry(process_id, event_port_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(event_port_entry, abi::capability::RIGHT_MANAGE) {
        return error_return(error);
    }
    if event_port_entry.object.kind != abi::capability::KIND_EVENT_PORT {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(target_entry) = registry.entry(process_id, target_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(target_entry, abi::capability::RIGHT_WAIT) {
        return error_return(error);
    }
    if target_entry.object.kind == abi::capability::KIND_EVENT_PORT {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let supported = capability_object_supported_signals(target_entry.object.kind);
    if requested.bits() & !supported.bits() != 0 {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let current = match capability_object_signal_state(&registry, target_entry.object) {
        Ok(current) => current,
        Err(error) => return error_return(error),
    };
    let Some(index) = registry.object_index(event_port_entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::EventPort(event_port) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    let target = kernel::event_port::Target {
        object_id: target_entry.object.id,
        object_kind: target_entry.object.kind,
    };
    match event_port.add(target, requested, key, current) {
        Ok(()) => 0,
        Err(kernel::event_port::AddError::Full) => error_return(abi::errno::NO_SPACE),
        Err(kernel::event_port::AddError::EventQueueFull) => error_return(abi::errno::NO_SPACE),
        Err(
            kernel::event_port::AddError::InvalidTarget
            | kernel::event_port::AddError::InvalidSignals
            | kernel::event_port::AddError::InvalidKey
            | kernel::event_port::AddError::DuplicateKey,
        ) => error_return(abi::errno::INVALID_ARGUMENT),
    }
}

fn event_port_remove(process_id: u64, event_port_handle: u64, key: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, event_port_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_MANAGE) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_EVENT_PORT {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(index) = registry.object_index(entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::EventPort(event_port) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    match event_port.remove(key) {
        Ok(_) => {
            registry.collect_garbage();
            0
        }
        Err(_) => error_return(abi::errno::NO_ENTRY),
    }
}

fn refresh_event_ports(registry: &mut CapabilityRegistry) -> Result<(), i64> {
    let ports = registry
        .objects
        .iter()
        .filter_map(|record| match &record.data {
            CapabilityObjectData::EventPort(event_port) => Some((
                record.reference,
                event_port
                    .registrations()
                    .map(|registration| registration.target)
                    .collect::<Vec<_>>(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();

    for (port, targets) in ports {
        for target in targets {
            let object = CapabilityObjectRef {
                id: target.object_id,
                kind: target.object_kind,
            };
            let current = capability_object_signal_state(registry, object)?;
            let port_index = registry.object_index(port).ok_or(abi::errno::IO)?;
            let CapabilityObjectData::EventPort(event_port) =
                &mut registry.objects[port_index].data
            else {
                return Err(abi::errno::IO);
            };
            event_port
                .observe(target, current)
                .map_err(|_| abi::errno::NO_SPACE)?;
        }
    }
    Ok(())
}

fn timer_create(process_id: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let object = match registry.create_object(
        abi::capability::KIND_TIMER,
        CapabilityObjectData::Timer(kernel::timer::State::new()),
    ) {
        Ok(object) => object,
        Err(error) => return error_return(error),
    };
    match registry.insert_entry(process_id, object, abi::capability::TIMER_RIGHTS) {
        Ok(handle) => handle,
        Err(error) => {
            registry.collect_garbage();
            error_return(error)
        }
    }
}

fn timer_arm(process_id: u64, timer_handle: u64, deadline_ns: u64) -> u64 {
    if deadline_ns == abi::deadline::INFINITE {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, timer_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_MANAGE) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_TIMER {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let object = entry.object;
    let Some(index) = registry.object_index(object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Timer(timer) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    timer.cancel();
    if let Err(error) = refresh_event_ports(&mut registry) {
        return error_return(error);
    }
    let Some(index) = registry.object_index(object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Timer(timer) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    match timer.arm(
        deadline_ns,
        crate::interrupts::monotonic_time_ns(),
    ) {
        Ok(()) => 0,
        Err(_) => error_return(abi::errno::INVALID_ARGUMENT),
    }
}

fn timer_cancel(process_id: u64, timer_handle: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, timer_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_MANAGE) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_TIMER {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(index) = registry.object_index(entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Timer(timer) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    timer.cancel();
    0
}

fn advance_timers(now_ns: u64) -> bool {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let mut fired = false;
    for record in &mut registry.objects {
        if let CapabilityObjectData::Timer(timer) = &mut record.data {
            fired |= timer.advance(now_ns);
        }
    }
    fired
}

fn event_create(process_id: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let object = match registry.create_object(
        abi::capability::KIND_EVENT,
        CapabilityObjectData::Event(kernel::event::State::new()),
    ) {
        Ok(object) => object,
        Err(error) => return error_return(error),
    };
    match registry.insert_entry(process_id, object, abi::capability::EVENT_RIGHTS) {
        Ok(handle) => handle,
        Err(error) => {
            registry.collect_garbage();
            error_return(error)
        }
    }
}

fn event_set(process_id: u64, event_handle: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, event_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_SIGNAL) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_EVENT {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(index) = registry.object_index(entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Event(event) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    event.set();
    0
}

fn event_reset(process_id: u64, event_handle: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, event_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_SIGNAL) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_EVENT {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(index) = registry.object_index(entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Event(event) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    event.reset();
    0
}

fn endpoint_create(process_id: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let object = match registry.create_object(
        abi::capability::KIND_ENDPOINT,
        CapabilityObjectData::Endpoint(EndpointObject {
            queue: alloc::collections::VecDeque::with_capacity(abi::limits::MAX_ENDPOINT_MESSAGES),
            peer: EndpointPeer::Loopback,
        }),
    ) {
        Ok(object) => object,
        Err(error) => return error_return(error),
    };
    match registry.insert_entry(process_id, object, abi::capability::ENDPOINT_RIGHTS) {
        Ok(handle) => handle,
        Err(error) => {
            registry.collect_garbage();
            error_return(error)
        }
    }
}

fn endpoint_create_pair(process_id: u64, address: u64, length: u64) -> u64 {
    let required = size_of::<abi::capability::EndpointPair>();
    let output_length = match usize::try_from(length) {
        Ok(length) => length,
        Err(_) => return error_return(abi::errno::RANGE),
    };
    if output_length < required {
        return error_return(abi::errno::RANGE);
    }
    if !user_range_allows(process_id, address, required, true) {
        return error_return(abi::errno::BAD_ADDRESS);
    }
    let pair = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        match registry.create_endpoint_pair(process_id) {
            Ok(pair) => pair,
            Err(error) => return error_return(error),
        }
    };
    unsafe { ptr::write_unaligned(address as *mut abi::capability::EndpointPair, pair) };
    0
}

fn endpoint_send(
    process_id: u64,
    endpoint_handle: u64,
    address: u64,
    length: u64,
    transfer_handle: u64,
    transfer_rights: u64,
) -> u64 {
    endpoint_send_with_disposition(
        process_id,
        endpoint_handle,
        address,
        length,
        transfer_handle,
        transfer_rights,
        false,
    )
}

fn endpoint_send_move(
    process_id: u64,
    endpoint_handle: u64,
    address: u64,
    length: u64,
    transfer_handle: u64,
    transfer_rights: u64,
) -> u64 {
    endpoint_send_with_disposition(
        process_id,
        endpoint_handle,
        address,
        length,
        transfer_handle,
        transfer_rights,
        true,
    )
}

fn endpoint_send_move_many(
    process_id: u64,
    endpoint_handle: u64,
    address: u64,
    length: u64,
    dispositions_address: u64,
    disposition_count: u64,
) -> u64 {
    let bytes = match capability_read_message(process_id, address, length) {
        Ok(bytes) => bytes,
        Err(error) => return error_return(error),
    };
    let disposition_count =
        match capability_user_length(disposition_count, abi::limits::MAX_IPC_MESSAGE_HANDLES) {
            Ok(0) => return error_return(abi::errno::INVALID_ARGUMENT),
            Ok(count) => count,
            Err(error) => return error_return(error),
        };
    let disposition_bytes = match disposition_count
        .checked_mul(size_of::<abi::capability::HandleDisposition>())
    {
        Some(bytes) => bytes,
        None => return error_return(abi::errno::RANGE),
    };
    if !capability_user_range(process_id, dispositions_address, disposition_bytes, false) {
        return error_return(abi::errno::BAD_ADDRESS);
    }
    let dispositions = (0..disposition_count)
        .map(|index| unsafe {
            ptr::read_unaligned(
                (dispositions_address as *const abi::capability::HandleDisposition).add(index),
            )
        })
        .collect::<Vec<_>>();

    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(endpoint_entry) = registry.entry(process_id, endpoint_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(endpoint_entry, abi::capability::RIGHT_SEND) {
        return error_return(error);
    }
    if endpoint_entry.object.kind != abi::capability::KIND_ENDPOINT {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }

    let mut source_handles = Vec::with_capacity(disposition_count);
    let mut transfers = Vec::with_capacity(disposition_count);
    for disposition in dispositions {
        if disposition.handle == abi::capability::INVALID_HANDLE
            || source_handles.contains(&disposition.handle)
        {
            return error_return(abi::errno::INVALID_ARGUMENT);
        }
        let Some(source) = registry.entry(process_id, disposition.handle) else {
            return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
        };
        if let Err(error) = capability_has_right(source, abi::capability::RIGHT_TRANSFER) {
            return error_return(error);
        }
        if disposition.rights == 0
            || disposition.rights & !source.rights != 0
            || disposition.rights & !capability_allowed_rights(source.object.kind) != 0
        {
            return error_return(abi::errno::PERMISSION);
        }
        source_handles.push(disposition.handle);
        transfers.push(TransferredCapability {
            object: source.object,
            rights: disposition.rights,
        });
    }

    let destination = match endpoint_destination(&registry, endpoint_entry.object) {
        Ok(destination) => destination,
        Err(error) => return error_return(error),
    };
    let Some(object_index) = registry.object_index(destination) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Endpoint(endpoint) = &registry.objects[object_index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    if endpoint.queue.len() >= abi::limits::MAX_ENDPOINT_MESSAGES {
        return error_return(abi::errno::TRY_AGAIN);
    }
    if !registry.remove_entries(process_id, &source_handles) {
        return error_return(abi::errno::IO);
    }
    let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[object_index].data else {
        return error_return(abi::errno::IO);
    };
    endpoint.queue.push_back(EndpointMessage {
        sender_process_id: process_id,
        bytes,
        capabilities: transfers,
    });
    0
}

fn endpoint_send_with_disposition(
    process_id: u64,
    endpoint_handle: u64,
    address: u64,
    length: u64,
    transfer_handle: u64,
    transfer_rights: u64,
    move_transfer: bool,
) -> u64 {
    let bytes = match capability_read_message(process_id, address, length) {
        Ok(bytes) => bytes,
        Err(error) => return error_return(error),
    };
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(endpoint_entry) = registry.entry(process_id, endpoint_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(endpoint_entry, abi::capability::RIGHT_SEND) {
        return error_return(error);
    }
    if endpoint_entry.object.kind != abi::capability::KIND_ENDPOINT {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }

    let transfer = if transfer_handle == abi::capability::INVALID_HANDLE {
        if transfer_rights != 0 || move_transfer {
            return error_return(abi::errno::INVALID_ARGUMENT);
        }
        None
    } else {
        let Some(source) = registry.entry(process_id, transfer_handle) else {
            return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
        };
        if let Err(error) = capability_has_right(source, abi::capability::RIGHT_TRANSFER) {
            return error_return(error);
        }
        if transfer_rights == 0
            || transfer_rights & !source.rights != 0
            || transfer_rights & !capability_allowed_rights(source.object.kind) != 0
        {
            return error_return(abi::errno::PERMISSION);
        }
        Some(TransferredCapability {
            object: source.object,
            rights: transfer_rights,
        })
    };

    let destination = match endpoint_destination(&registry, endpoint_entry.object) {
        Ok(destination) => destination,
        Err(error) => return error_return(error),
    };
    let Some(object_index) = registry.object_index(destination) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Endpoint(endpoint) = &registry.objects[object_index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    if endpoint.queue.len() >= abi::limits::MAX_ENDPOINT_MESSAGES {
        return error_return(abi::errno::TRY_AGAIN);
    }
    if move_transfer && !registry.remove_entry(process_id, transfer_handle) {
        return error_return(abi::errno::IO);
    }
    let CapabilityObjectData::Endpoint(endpoint) = &mut registry.objects[object_index].data else {
        return error_return(abi::errno::IO);
    };
    endpoint.queue.push_back(EndpointMessage {
        sender_process_id: process_id,
        bytes,
        capabilities: transfer.into_iter().collect(),
    });
    0
}

fn endpoint_receive(
    process_id: u64,
    endpoint_handle: u64,
    buffer_address: u64,
    buffer_length: u64,
    info_address: u64,
) -> u64 {
    let buffer_length =
        match capability_user_length(buffer_length, abi::limits::MAX_IPC_MESSAGE_BYTES) {
            Ok(length) => length,
            Err(error) => return error_return(error),
        };
    if !capability_user_range(process_id, buffer_address, buffer_length, true)
        || !user_range_allows(
            process_id,
            info_address,
            size_of::<abi::capability::MessageInfo>(),
            true,
        )
    {
        return error_return(abi::errno::BAD_ADDRESS);
    }

    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(endpoint_entry) = registry.entry(process_id, endpoint_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(endpoint_entry, abi::capability::RIGHT_RECEIVE) {
        return error_return(error);
    }
    if endpoint_entry.object.kind != abi::capability::KIND_ENDPOINT {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(object_index) = registry.object_index(endpoint_entry.object) else {
        return error_return(abi::errno::IO);
    };

    let (message_length, transfer) = match &registry.objects[object_index].data {
        CapabilityObjectData::Endpoint(endpoint) => match endpoint.queue.front() {
            Some(message) if message.capabilities.len() <= 1 => {
                (message.bytes.len(), message.capabilities.first().copied())
            }
            Some(_) => return error_return(abi::errno::RANGE),
            None => {
                if let EndpointPeer::Connected(peer) = endpoint.peer
                    && registry.object_index(peer).is_none()
                {
                    return error_return(abi::errno::BROKEN_PIPE);
                }
                return error_return(abi::errno::TRY_AGAIN);
            }
        },
        CapabilityObjectData::Notification(_)
        | CapabilityObjectData::SharedMemory(_)
        | CapabilityObjectData::KernelEarlyLogReader(_)
        | CapabilityObjectData::Job(_)
        | CapabilityObjectData::WaitSet(_)
        | CapabilityObjectData::EventPort(_)
        | CapabilityObjectData::Timer(_)
        | CapabilityObjectData::Event(_) => {
            return error_return(abi::errno::INVALID_ARGUMENT);
        }
    };
    if message_length > buffer_length {
        return error_return(abi::errno::RANGE);
    }

    let transfers = transfer.into_iter().collect::<Vec<_>>();
    let transferred_handles = match registry.insert_entries(process_id, &transfers) {
        Ok(handles) => handles,
        Err(error) => return error_return(error),
    };
    let transferred_handle = transferred_handles
        .first()
        .copied()
        .unwrap_or(abi::capability::INVALID_HANDLE);

    let message = match &mut registry.objects[object_index].data {
        CapabilityObjectData::Endpoint(endpoint) => endpoint
            .queue
            .pop_front()
            .expect("endpoint message disappeared during receive"),
        CapabilityObjectData::Notification(_)
        | CapabilityObjectData::SharedMemory(_)
        | CapabilityObjectData::KernelEarlyLogReader(_)
        | CapabilityObjectData::Job(_)
        | CapabilityObjectData::WaitSet(_)
        | CapabilityObjectData::EventPort(_)
        | CapabilityObjectData::Timer(_)
        | CapabilityObjectData::Event(_) => {
            return error_return(abi::errno::IO);
        }
    };
    if !message.bytes.is_empty() {
        unsafe {
            ptr::copy_nonoverlapping(
                message.bytes.as_ptr(),
                buffer_address as *mut u8,
                message.bytes.len(),
            )
        };
    }
    let info = abi::capability::MessageInfo {
        sender_process_id: message.sender_process_id,
        byte_count: message.bytes.len() as u64,
        transferred_handle,
        transferred_rights: message
            .capabilities
            .first()
            .copied()
            .map(|capability| capability.rights)
            .unwrap_or(0),
    };
    unsafe { ptr::write_unaligned(info_address as *mut abi::capability::MessageInfo, info) };
    registry.collect_garbage();
    message.bytes.len() as u64
}

fn endpoint_receive_many(
    process_id: u64,
    endpoint_handle: u64,
    buffer_address: u64,
    buffer_length: u64,
    handles_address: u64,
    handle_capacity: u64,
    info_address: u64,
) -> u64 {
    let buffer_length =
        match capability_user_length(buffer_length, abi::limits::MAX_IPC_MESSAGE_BYTES) {
            Ok(length) => length,
            Err(error) => return error_return(error),
        };
    let handle_capacity =
        match capability_user_length(handle_capacity, abi::limits::MAX_IPC_MESSAGE_HANDLES) {
            Ok(capacity) => capacity,
            Err(error) => return error_return(error),
        };
    let handles_length = match handle_capacity
        .checked_mul(size_of::<abi::capability::ReceivedHandle>())
    {
        Some(length) => length,
        None => return error_return(abi::errno::RANGE),
    };
    if !capability_user_range(process_id, buffer_address, buffer_length, true)
        || !capability_user_range(process_id, handles_address, handles_length, true)
        || !user_range_allows(
            process_id,
            info_address,
            size_of::<abi::capability::MessageInfoMany>(),
            true,
        )
    {
        return error_return(abi::errno::BAD_ADDRESS);
    }

    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(endpoint_entry) = registry.entry(process_id, endpoint_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(endpoint_entry, abi::capability::RIGHT_RECEIVE) {
        return error_return(error);
    }
    if endpoint_entry.object.kind != abi::capability::KIND_ENDPOINT {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(object_index) = registry.object_index(endpoint_entry.object) else {
        return error_return(abi::errno::IO);
    };

    let (message_length, transfers, sender_process_id) =
        match &registry.objects[object_index].data {
            CapabilityObjectData::Endpoint(endpoint) => match endpoint.queue.front() {
                Some(message) => (
                    message.bytes.len(),
                    message.capabilities.clone(),
                    message.sender_process_id,
                ),
                None => {
                    if let EndpointPeer::Connected(peer) = endpoint.peer
                        && registry.object_index(peer).is_none()
                    {
                        return error_return(abi::errno::BROKEN_PIPE);
                    }
                    return error_return(abi::errno::TRY_AGAIN);
                }
            },
            CapabilityObjectData::Notification(_)
            | CapabilityObjectData::SharedMemory(_)
            | CapabilityObjectData::KernelEarlyLogReader(_)
            | CapabilityObjectData::Job(_)
            | CapabilityObjectData::WaitSet(_)
            | CapabilityObjectData::EventPort(_)
            | CapabilityObjectData::Timer(_)
            | CapabilityObjectData::Event(_) => {
                return error_return(abi::errno::INVALID_ARGUMENT);
            }
        };
    let info = abi::capability::MessageInfoMany {
        sender_process_id,
        byte_count: message_length as u64,
        handle_count: transfers.len() as u64,
        reserved: 0,
    };
    unsafe { ptr::write_unaligned(info_address as *mut abi::capability::MessageInfoMany, info) };
    if message_length > buffer_length || transfers.len() > handle_capacity {
        return error_return(abi::errno::RANGE);
    }

    let transferred_handles = match registry.insert_entries(process_id, &transfers) {
        Ok(handles) => handles,
        Err(error) => return error_return(error),
    };
    let message = match &mut registry.objects[object_index].data {
        CapabilityObjectData::Endpoint(endpoint) => endpoint
            .queue
            .pop_front()
            .expect("endpoint message disappeared during receive-many"),
        CapabilityObjectData::Notification(_)
        | CapabilityObjectData::SharedMemory(_)
        | CapabilityObjectData::KernelEarlyLogReader(_)
        | CapabilityObjectData::Job(_)
        | CapabilityObjectData::WaitSet(_)
        | CapabilityObjectData::EventPort(_)
        | CapabilityObjectData::Timer(_)
        | CapabilityObjectData::Event(_) => {
            return error_return(abi::errno::IO);
        }
    };
    if !message.bytes.is_empty() {
        unsafe {
            ptr::copy_nonoverlapping(
                message.bytes.as_ptr(),
                buffer_address as *mut u8,
                message.bytes.len(),
            )
        };
    }
    for (index, (handle, capability)) in transferred_handles
        .iter()
        .copied()
        .zip(message.capabilities.iter().copied())
        .enumerate()
    {
        unsafe {
            ptr::write_unaligned(
                (handles_address as *mut abi::capability::ReceivedHandle).add(index),
                abi::capability::ReceivedHandle {
                    handle,
                    rights: capability.rights,
                },
            )
        };
    }
    registry.collect_garbage();
    message.bytes.len() as u64
}

fn notification_create(process_id: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let object = match registry.create_object(
        abi::capability::KIND_NOTIFICATION,
        CapabilityObjectData::Notification(NotificationObject { pending: 0 }),
    ) {
        Ok(object) => object,
        Err(error) => return error_return(error),
    };
    match registry.insert_entry(process_id, object, abi::capability::NOTIFICATION_RIGHTS) {
        Ok(handle) => handle,
        Err(error) => {
            registry.collect_garbage();
            error_return(error)
        }
    }
}

fn notification_signal(process_id: u64, handle: u64, amount: u64) -> u64 {
    if amount == 0 {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_SIGNAL) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_NOTIFICATION {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(index) = registry.object_index(entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Notification(notification) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    let Some(pending) = notification.pending.checked_add(amount) else {
        return error_return(abi::errno::RANGE);
    };
    notification.pending = pending;
    pending
}

fn notification_try_wait(process_id: u64, handle: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_WAIT) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_NOTIFICATION {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(index) = registry.object_index(entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Notification(notification) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    if notification.pending == 0 {
        return error_return(abi::errno::TRY_AGAIN);
    }
    notification.pending -= 1;
    notification.pending
}

fn shared_memory_create(process_id: u64, length: u64) -> u64 {
    let length = match capability_user_length(length, abi::limits::MAX_SHARED_MEMORY_BYTES) {
        Ok(0) => return error_return(abi::errno::INVALID_ARGUMENT),
        Ok(length) => length,
        Err(error) => return error_return(error),
    };
    let mut registry = CAPABILITY_REGISTRY.lock();
    registry.collect_garbage();
    if registry.shared_memory_bytes().saturating_add(length)
        > abi::limits::MAX_SHARED_MEMORY_TOTAL_BYTES
    {
        return error_return(abi::errno::NO_SPACE);
    }
    let object = match registry.create_object(
        abi::capability::KIND_SHARED_MEMORY,
        CapabilityObjectData::SharedMemory(SharedMemoryObject {
            bytes: vec![0_u8; length],
        }),
    ) {
        Ok(object) => object,
        Err(error) => return error_return(error),
    };
    match registry.insert_entry(process_id, object, abi::capability::SHARED_MEMORY_RIGHTS) {
        Ok(handle) => handle,
        Err(error) => {
            registry.collect_garbage();
            error_return(error)
        }
    }
}

fn shared_memory_read(process_id: u64, handle: u64, offset: u64, address: u64, length: u64) -> u64 {
    let length = match capability_user_length(length, abi::limits::MAX_SHARED_MEMORY_BYTES) {
        Ok(length) => length,
        Err(error) => return error_return(error),
    };
    if !capability_user_range(process_id, address, length, true) {
        return error_return(abi::errno::BAD_ADDRESS);
    }
    let offset = match usize::try_from(offset) {
        Ok(offset) => offset,
        Err(_) => return error_return(abi::errno::RANGE),
    };
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_READ) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_SHARED_MEMORY {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(index) = registry.object_index(entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::SharedMemory(memory) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    let Some(end) = offset.checked_add(length) else {
        return error_return(abi::errno::RANGE);
    };
    let Some(source) = memory.bytes.get(offset..end) else {
        return error_return(abi::errno::RANGE);
    };
    if !source.is_empty() {
        unsafe { ptr::copy_nonoverlapping(source.as_ptr(), address as *mut u8, source.len()) };
    }
    source.len() as u64
}

fn shared_memory_write(
    process_id: u64,
    handle: u64,
    offset: u64,
    address: u64,
    length: u64,
) -> u64 {
    let length = match capability_user_length(length, abi::limits::MAX_SHARED_MEMORY_BYTES) {
        Ok(length) => length,
        Err(error) => return error_return(error),
    };
    if !capability_user_range(process_id, address, length, false) {
        return error_return(abi::errno::BAD_ADDRESS);
    }
    let offset = match usize::try_from(offset) {
        Ok(offset) => offset,
        Err(_) => return error_return(abi::errno::RANGE),
    };
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_WRITE) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_SHARED_MEMORY {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(index) = registry.object_index(entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::SharedMemory(memory) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    let Some(end) = offset.checked_add(length) else {
        return error_return(abi::errno::RANGE);
    };
    let Some(destination) = memory.bytes.get_mut(offset..end) else {
        return error_return(abi::errno::RANGE);
    };
    if !destination.is_empty() {
        let source = unsafe { slice::from_raw_parts(address as *const u8, destination.len()) };
        destination.copy_from_slice(source);
    }
    destination.len() as u64
}

fn job_create(process_id: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let object = match registry.create_object(
        abi::capability::KIND_JOB,
        CapabilityObjectData::Job(kernel::job::State::new(
            MAX_PROCESS_SLOTS,
            abi::limits::MAX_JOB_OBJECTS,
        )),
    ) {
        Ok(object) => object,
        Err(error) => return error_return(error),
    };
    match registry.insert_entry(process_id, object, abi::capability::JOB_RIGHTS) {
        Ok(handle) => handle,
        Err(error) => {
            registry.collect_garbage();
            error_return(error)
        }
    }
}

fn job_create_child(process_id: u64, parent_handle: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(parent_entry) = registry.entry(process_id, parent_handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(parent_entry, abi::capability::RIGHT_MANAGE) {
        return error_return(error);
    }
    if parent_entry.object.kind != abi::capability::KIND_JOB {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let parent = parent_entry.object;
    let Some(parent_index) = registry.object_index(parent) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Job(parent_state) = &registry.objects[parent_index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    let parent_process_limit = parent_state.process_limit();
    let mut child_state =
        kernel::job::State::new(MAX_PROCESS_SLOTS, abi::limits::MAX_JOB_OBJECTS);
    if child_state.set_parent(parent.id).is_err() {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    if child_state
        .set_process_limit(parent_process_limit)
        .is_err()
    {
        return error_return(abi::errno::IO);
    }
    let child = match registry.create_object(
        abi::capability::KIND_JOB,
        CapabilityObjectData::Job(child_state),
    ) {
        Ok(object) => object,
        Err(error) => return error_return(error),
    };
    let attached = registry
        .object_index(parent)
        .and_then(|index| match &mut registry.objects[index].data {
            CapabilityObjectData::Job(state) => Some(state.attach_child(child.id)),
            _ => None,
        });
    let attach_error = match attached {
        Some(Ok(())) => None,
        Some(Err(kernel::job::ChildError::InvalidJob)) | None => {
            Some(abi::errno::INVALID_ARGUMENT)
        }
        Some(Err(kernel::job::ChildError::AlreadyChild)) => Some(abi::errno::PERMISSION),
        Some(Err(kernel::job::ChildError::Full)) => Some(abi::errno::NO_SPACE),
        Some(Err(kernel::job::ChildError::Retired)) => Some(abi::errno::PERMISSION),
    };
    if let Some(error) = attach_error {
        registry.collect_garbage();
        return error_return(error);
    }
    match registry.insert_entry(process_id, child, abi::capability::JOB_RIGHTS) {
        Ok(handle) => handle,
        Err(error) => {
            if let Some(index) = registry.object_index(parent)
                && let CapabilityObjectData::Job(state) = &mut registry.objects[index].data
            {
                let _ = state.remove_child(child.id);
            }
            registry.collect_garbage();
            error_return(error)
        }
    }
}

fn job_set_process_limit(process_id: u64, handle: u64, limit: u64) -> u64 {
    let Ok(limit) = usize::try_from(limit) else {
        return error_return(abi::errno::RANGE);
    };
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_MANAGE) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_JOB {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(index) = registry.object_index(entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Job(state) = &mut registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    match state.set_process_limit(limit) {
        Ok(()) => limit as u64,
        Err(kernel::job::LimitError::OutOfRange) => error_return(abi::errno::RANGE),
        Err(kernel::job::LimitError::Relaxation) => error_return(abi::errno::PERMISSION),
        Err(kernel::job::LimitError::Retired) => error_return(abi::errno::PERMISSION),
    }
}

fn job_get_process_limit(process_id: u64, handle: u64) -> u64 {
    let registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_WAIT) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_JOB {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(index) = registry.object_index(entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Job(state) = &registry.objects[index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    state.process_limit() as u64
}

fn job_retire(process_id: u64, handle: u64) -> u64 {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(entry) = registry.entry(process_id, handle) else {
        return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
    };
    if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_MANAGE) {
        return error_return(error);
    }
    if entry.object.kind != abi::capability::KIND_JOB {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let Some(child_index) = registry.object_index(entry.object) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Job(child_state) = &registry.objects[child_index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    let parent_id = match child_state.retirement_parent() {
        Ok(parent) => parent,
        Err(kernel::job::RetireError::Root) => {
            return error_return(abi::errno::INVALID_ARGUMENT);
        }
        Err(kernel::job::RetireError::NotEmpty) => return error_return(abi::errno::TRY_AGAIN),
        Err(kernel::job::RetireError::Retired) => return error_return(abi::errno::PERMISSION),
    };
    let parent = CapabilityObjectRef {
        id: parent_id,
        kind: abi::capability::KIND_JOB,
    };
    let Some(parent_index) = registry.object_index(parent) else {
        return error_return(abi::errno::IO);
    };
    let CapabilityObjectData::Job(parent_state) = &registry.objects[parent_index].data else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    if !parent_state.children().any(|child| child == entry.object.id) {
        return error_return(abi::errno::IO);
    }
    let retired_parent = match &mut registry.objects[child_index].data {
        CapabilityObjectData::Job(state) => match state.retire() {
            Ok(parent) => parent,
            Err(_) => return error_return(abi::errno::IO),
        },
        _ => return error_return(abi::errno::INVALID_ARGUMENT),
    };
    if retired_parent != parent_id {
        return error_return(abi::errno::IO);
    }
    let removed = match &mut registry.objects[parent_index].data {
        CapabilityObjectData::Job(state) => state.remove_child(entry.object.id).is_ok(),
        _ => false,
    };
    if !removed {
        return error_return(abi::errno::IO);
    }
    registry.collect_garbage();
    0
}

fn job_assign(process_id: u64, handle: u64, child_process_id: u64) -> u64 {
    if child_process_id == 0 || child_process_id == process_id {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let job = {
        let registry = CAPABILITY_REGISTRY.lock();
        let Some(entry) = registry.entry(process_id, handle) else {
            return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
        };
        if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_MANAGE) {
            return error_return(error);
        }
        if entry.object.kind != abi::capability::KIND_JOB {
            return error_return(abi::errno::INVALID_ARGUMENT);
        }
        entry.object
    };

    let existing_job = {
        let manager = PROCESS_MANAGER.lock();
        let Some(child) = manager.processes.iter().find(|candidate| {
            candidate.process_id == child_process_id
                && candidate.parent_process_id == Some(process_id)
                && candidate.is_live()
        }) else {
            return error_return(ERR_NO_CHILD);
        };
        child.job
    };
    if existing_job == Some(job) {
        return child_process_id;
    }
    if existing_job.is_some() {
        return error_return(abi::errno::PERMISSION);
    }

    if let Err(error) = capability_job_add_member(job, child_process_id) {
        return error_return(error);
    }
    let installed = {
        let mut manager = PROCESS_MANAGER.lock();
        manager
            .process_mut(child_process_id)
            .filter(|child| {
                child.parent_process_id == Some(process_id)
                    && child.is_live()
                    && child.job.is_none()
            })
            .map(|child| child.job = Some(job))
            .is_some()
    };
    if !installed {
        capability_job_remove_unstarted(job, child_process_id);
        return error_return(ERR_NO_CHILD);
    }
    child_process_id
}

fn job_try_wait(process_id: u64, handle: u64, address: u64, length: u64) -> u64 {
    if length != size_of::<abi::job::Exit>() as u64
        || !user_range_allows(process_id, address, size_of::<abi::job::Exit>(), true)
    {
        return error_return(if length != size_of::<abi::job::Exit>() as u64 {
            abi::errno::INVALID_ARGUMENT
        } else {
            abi::errno::BAD_ADDRESS
        });
    }
    let record = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        let Some(entry) = registry.entry(process_id, handle) else {
            return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
        };
        if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_WAIT) {
            return error_return(error);
        }
        if entry.object.kind != abi::capability::KIND_JOB {
            return error_return(abi::errno::INVALID_ARGUMENT);
        }
        let jobs = match registry.job_subtree(entry.object) {
            Ok(jobs) => jobs,
            Err(error) => return error_return(error),
        };
        let mut completion = None;
        for job in &jobs {
            let Some(index) = registry.object_index(*job) else {
                return error_return(abi::errno::IO);
            };
            let CapabilityObjectData::Job(state) = &mut registry.objects[index].data else {
                return error_return(abi::errno::INVALID_ARGUMENT);
            };
            if let Some(record) = state.take_completion() {
                completion = Some(record);
                break;
            }
        }
        match completion {
            Some(record) => record,
            None => match registry.job_subtree_active_members(entry.object) {
                Ok(0) => return error_return(ERR_NO_CHILD),
                Ok(_) => return error_return(abi::errno::TRY_AGAIN),
                Err(error) => return error_return(error),
            },
        }
    };
    let exit = abi::job::Exit {
        process_id: record.process_id,
        status: record.status,
    };
    unsafe { ptr::write_unaligned(address as *mut abi::job::Exit, exit) };
    0
}

fn job_terminate(process_id: u64, handle: u64) -> u64 {
    let members = {
        let registry = CAPABILITY_REGISTRY.lock();
        let Some(entry) = registry.entry(process_id, handle) else {
            return error_return(abi::errno::BAD_FILE_DESCRIPTOR);
        };
        if let Err(error) = capability_has_right(entry, abi::capability::RIGHT_SIGNAL) {
            return error_return(error);
        }
        if entry.object.kind != abi::capability::KIND_JOB {
            return error_return(abi::errno::INVALID_ARGUMENT);
        }
        let jobs = match registry.job_subtree(entry.object) {
            Ok(jobs) => jobs,
            Err(error) => return error_return(error),
        };
        let mut members = Vec::new();
        for job in jobs {
            let Some(index) = registry.object_index(job) else {
                return error_return(abi::errno::IO);
            };
            let CapabilityObjectData::Job(state) = &registry.objects[index].data else {
                return error_return(abi::errno::INVALID_ARGUMENT);
            };
            members.extend(state.members());
        }
        members
    };

    members
        .into_iter()
        .filter(|member| terminate_process_with_signal(*member, abi::signal::KILL, true))
        .count() as u64
}

fn capability_job_add_member(job: CapabilityObjectRef, process_id: u64) -> Result<(), i64> {
    let mut registry = CAPABILITY_REGISTRY.lock();
    let Some(index) = registry.object_index(job) else {
        return Err(abi::errno::IO);
    };
    let CapabilityObjectData::Job(state) = &registry.objects[index].data else {
        return Err(abi::errno::INVALID_ARGUMENT);
    };
    if state.is_retired() {
        return Err(abi::errno::PERMISSION);
    }
    if !registry.job_admits_process(job)? {
        return Err(abi::errno::NO_SPACE);
    }
    let CapabilityObjectData::Job(state) = &mut registry.objects[index].data else {
        return Err(abi::errno::INVALID_ARGUMENT);
    };
    state.assign(process_id).map_err(|error| match error {
        kernel::job::AssignError::InvalidProcess => abi::errno::INVALID_ARGUMENT,
        kernel::job::AssignError::AlreadyMember => abi::errno::PERMISSION,
        kernel::job::AssignError::Full => abi::errno::NO_SPACE,
        kernel::job::AssignError::Retired => abi::errno::PERMISSION,
    })?;
    kernel_capability_root_add(job);
    Ok(())
}

fn capability_job_remove_unstarted(job: CapabilityObjectRef, process_id: u64) {
    let removed = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        registry
            .object_index(job)
            .and_then(|index| match &mut registry.objects[index].data {
                CapabilityObjectData::Job(state) => Some(state.remove_unstarted(process_id)),
                _ => None,
            })
            .is_some_and(|result| result.is_ok())
    };
    if removed {
        kernel_capability_root_remove(job);
    }
}

fn capability_job_record_exit(job: CapabilityObjectRef, process_id: u64, status: u64) {
    let recorded = {
        let mut registry = CAPABILITY_REGISTRY.lock();
        registry
            .object_index(job)
            .and_then(|index| match &mut registry.objects[index].data {
                CapabilityObjectData::Job(state) => {
                    Some(state.complete(kernel::job::ExitRecord { process_id, status }))
                }
                _ => None,
            })
            .is_some_and(|result| result.is_ok())
    };
    debug_assert!(recorded, "job member disappeared before process completion");
    if recorded {
        kernel_capability_root_remove(job);
        wake_satisfied_object_waiters();
    }
}
