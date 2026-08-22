#!/usr/bin/env python3
from pathlib import Path
import re


def read(path: str) -> str:
    return Path(path).read_text()


def write(path: str, text: str) -> None:
    Path(path).write_text(text)


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected exactly one match, found {count}")
    return text.replace(old, new, 1)


def replace_all_checked(text: str, old: str, new: str, minimum: int, label: str) -> str:
    count = text.count(old)
    if count < minimum:
        raise RuntimeError(f"{label}: expected at least {minimum} matches, found {count}")
    return text.replace(old, new)


# ABI 1.31: opaque generation-checked handles plus bounded same-process slot lookup.
path = "shared/userspace_abi.rs"
text = read(path)
text = replace_once(text, "pub const ABI_VERSION_MINOR: u64 = 30;", "pub const ABI_VERSION_MINOR: u64 = 31;", "ABI minor")
text = replace_once(
    text,
    "    pub const EVENT_RESET: u64 = 90;\n",
    "    pub const EVENT_RESET: u64 = 90;\n    pub const CAPABILITY_HANDLE_AT_SLOT: u64 = 91;\n",
    "slot lookup syscall",
)
text = replace_once(
    text,
    "    pub const EVENT_OBJECTS: u64 = 1 << 28;\n",
    "    pub const EVENT_OBJECTS: u64 = 1 << 28;\n    pub const GENERATION_CHECKED_HANDLES: u64 = 1 << 29;\n",
    "generation feature bit",
)
text = replace_once(
    text,
    "        | TIMER_OBJECTS\n        | EVENT_OBJECTS;",
    "        | TIMER_OBJECTS\n        | EVENT_OBJECTS\n        | GENERATION_CHECKED_HANDLES;",
    "protection feature set",
)
write(path, text)

path = "shared/protection_abi.rs"
text = read(path)
text = replace_once(
    text,
    "    /// Arguments: target PID, source handle, rights mask, requested child\n    /// handle. A requested handle of zero asks the kernel to allocate a slot.\n",
    "    /// Arguments: target PID, source handle, rights mask, requested child\n    /// slot. A requested slot of zero asks the kernel to allocate any free slot.\n    /// The return value is the child's opaque generation-checked handle.\n",
    "grant-child slot documentation",
)
write(path, text)

# The host-testable generic registry already uses slot+generation handles. Make
# generation exhaustion fail closed rather than wrapping back to generation 1.
path = "kernel/src/capability.rs"
text = read(path)
text = replace_once(
    text,
    "        slot.generation = slot.generation.checked_add(1).unwrap_or(1);",
    "        slot.generation = slot.generation.checked_add(1).unwrap_or(0);",
    "generic close generation exhaustion",
)
text = replace_once(
    text,
    ".find(|(_, slot)| slot.entry.is_none())",
    ".find(|(_, slot)| slot.entry.is_none() && slot.generation != 0)",
    "generic exhausted-slot allocator",
)
marker = "\n    #[test]\n    fn invalid_rights_and_object_limits_are_rejected() {"
test = r'''
    #[test]
    fn exhausted_generation_is_never_reused() {
        let mut registry = CapabilityRegistry::new();
        let original = registry
            .create_object(process(1), ObjectType::Channel, channel_rights())
            .unwrap();
        let process_index = registry.process_index(process(1)).unwrap();
        let slot_index = original.handle.slot();
        registry.processes[process_index].slots[slot_index].generation = u32::MAX;
        let maximal = CapabilityHandle {
            slot: slot_index as u16,
            generation: u32::MAX,
        };

        registry.close(process(1), maximal).unwrap();
        assert_eq!(registry.processes[process_index].slots[slot_index].generation, 0);
        assert_eq!(
            registry.lookup(process(1), maximal),
            Err(CapabilityError::StaleHandle)
        );

        let replacement = registry
            .create_object(process(1), ObjectType::Event, Rights::BASIC)
            .unwrap();
        assert_ne!(replacement.handle.slot(), slot_index);
    }
'''
if test.strip() not in text:
    text = replace_once(text, marker, "\n" + test + marker, "generic exhaustion test insertion")
write(path, text)

# Live userspace capability table: the low 16 bits identify a bounded table slot;
# a registry-global nonzero u32 generation is allocated exactly once for every
# newly installed handle. Generations never wrap; exhaustion fails closed.
path = "kernel/src/process/userspace_platform/capability_entry.rs"
text = read(path)
text = replace_once(
    text,
    "static CAPABILITY_REGISTRY: PreemptMutex<CapabilityRegistry> =\n    PreemptMutex::new(CapabilityRegistry::new());\n",
    "static CAPABILITY_REGISTRY: PreemptMutex<CapabilityRegistry> =\n    PreemptMutex::new(CapabilityRegistry::new());\n\nconst CAPABILITY_HANDLE_SLOT_BITS: u32 = 16;\nconst CAPABILITY_HANDLE_SLOT_MASK: u64 = (1_u64 << CAPABILITY_HANDLE_SLOT_BITS) - 1;\nconst _: () = assert!(abi::limits::MAX_CAPABILITIES_PER_PROCESS <= u16::MAX as usize);\n\nfn capability_handle(slot: u16, generation: u32) -> Option<u64> {\n    if slot == 0\n        || usize::from(slot) > abi::limits::MAX_CAPABILITIES_PER_PROCESS\n        || generation == 0\n    {\n        return None;\n    }\n    Some((u64::from(generation) << CAPABILITY_HANDLE_SLOT_BITS) | u64::from(slot))\n}\n\nfn capability_handle_slot(handle: u64) -> Option<u16> {\n    let slot = (handle & CAPABILITY_HANDLE_SLOT_MASK) as u16;\n    let generation = handle >> CAPABILITY_HANDLE_SLOT_BITS;\n    if slot == 0\n        || usize::from(slot) > abi::limits::MAX_CAPABILITIES_PER_PROCESS\n        || generation == 0\n        || generation > u64::from(u32::MAX)\n    {\n        None\n    } else {\n        Some(slot)\n    }\n}\n",
    "live handle codec",
)
old_impl = '''impl ProcessCapabilityTable {
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
'''
new_impl = '''impl ProcessCapabilityTable {
    fn new(process_id: u64) -> Self {
        Self {
            process_id,
            entries: Vec::new(),
        }
    }

    fn slot_in_use(&self, slot: u16) -> bool {
        self.entries
            .iter()
            .any(|entry| capability_handle_slot(entry.handle) == Some(slot))
    }

    fn free_slots(&self, count: usize) -> Vec<u16> {
        (1..=abi::limits::MAX_CAPABILITIES_PER_PROCESS as u16)
            .filter(|slot| !self.slot_in_use(*slot))
            .take(count)
            .collect()
    }

    fn handle_at_slot(&self, slot: u16) -> Option<u64> {
        self.entries
            .iter()
            .find(|entry| capability_handle_slot(entry.handle) == Some(slot))
            .map(|entry| entry.handle)
    }
}
'''
text = replace_once(text, old_impl, new_impl, "live process capability table")
text = replace_once(
    text,
    "struct CapabilityRegistry {\n    next_object_id: u64,\n    tables: Vec<ProcessCapabilityTable>,\n",
    "struct CapabilityRegistry {\n    next_object_id: u64,\n    next_handle_generation: u32,\n    tables: Vec<ProcessCapabilityTable>,\n",
    "live registry generation field",
)
text = replace_once(
    text,
    "        Self {\n            next_object_id: 1,\n            tables: Vec::new(),\n",
    "        Self {\n            next_object_id: 1,\n            next_handle_generation: 1,\n            tables: Vec::new(),\n",
    "live registry generation init",
)
needle = '''    fn entry(&self, process_id: u64, handle: u64) -> Option<CapabilityEntry> {
        self.table_index(process_id).and_then(|index| {
            self.tables[index]
                .entries
                .iter()
                .find(|entry| entry.handle == handle)
                .copied()
        })
    }
'''
replacement = needle + '''
    fn handle_at_slot(&self, process_id: u64, slot: u16) -> Option<u64> {
        self.table_index(process_id)
            .and_then(|index| self.tables[index].handle_at_slot(slot))
    }

    fn take_handle_generations(&mut self, count: usize) -> Result<Vec<u32>, i64> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let count = u32::try_from(count).map_err(|_| abi::errno::NO_SPACE)?;
        let start = self.next_handle_generation;
        if start == 0 {
            return Err(abi::errno::NO_SPACE);
        }
        let last = start
            .checked_add(count.saturating_sub(1))
            .ok_or(abi::errno::NO_SPACE)?;
        self.next_handle_generation = last.checked_add(1).unwrap_or(0);
        Ok((start..=last).collect())
    }
'''
text = replace_once(text, needle, replacement, "live registry slot/generation helpers")
old_insert = '''        let table_index = self.ensure_table(process_id)?;
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
'''
new_insert = '''        let table_index = self.ensure_table(process_id)?;
        if self.tables[table_index].entries.len() >= abi::limits::MAX_CAPABILITIES_PER_PROCESS {
            return Err(abi::errno::NO_SPACE);
        }
        let slot = self.tables[table_index]
            .free_slots(1)
            .into_iter()
            .next()
            .ok_or(abi::errno::NO_SPACE)?;
        let generation = self.take_handle_generations(1)?[0];
        let handle = capability_handle(slot, generation).ok_or(abi::errno::NO_SPACE)?;
        self.tables[table_index].entries.push(CapabilityEntry {
            handle,
            object,
            rights,
        });
        Ok(handle)
'''
text = replace_once(text, old_insert, new_insert, "live single insert")
old_many = '''        let handles = (1..=abi::limits::MAX_CAPABILITIES_PER_PROCESS as u64)
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
'''
new_many = '''        let slots = self.tables[table_index].free_slots(capabilities.len());
        if slots.len() != capabilities.len() {
            return Err(abi::errno::NO_SPACE);
        }
        let generations = self.take_handle_generations(capabilities.len())?;
        let handles = slots
            .into_iter()
            .zip(generations)
            .map(|(slot, generation)| capability_handle(slot, generation).ok_or(abi::errno::NO_SPACE))
            .collect::<Result<Vec<_>, _>>()?;
        for (handle, capability) in handles.iter().copied().zip(capabilities.iter().copied()) {
'''
text = replace_once(text, old_many, new_many, "live multi insert")
old_pair = '''        let handles = (1..=abi::limits::MAX_CAPABILITIES_PER_PROCESS as u64)
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
'''
new_pair = '''        let slots = self.tables[table_index].free_slots(2);
        if slots.len() != 2 {
            return Err(abi::errno::NO_SPACE);
        }
        let generations = self.take_handle_generations(2)?;
        let handles = slots
            .into_iter()
            .zip(generations)
            .map(|(slot, generation)| capability_handle(slot, generation).ok_or(abi::errno::NO_SPACE))
            .collect::<Result<Vec<_>, _>>()?;

        let first_id = self.next_object_id;
'''
text = replace_once(text, old_pair, new_pair, "live endpoint pair handles")
text = replace_once(
    text,
    "            | abi::syscall::CAPABILITY_SIGNAL_STATE\n",
    "            | abi::syscall::CAPABILITY_SIGNAL_STATE\n            | abi::syscall::CAPABILITY_HANDLE_AT_SLOT\n",
    "capability syscall classification",
)
text = replace_once(
    text,
    "        abi::syscall::CAPABILITY_INFO => {\n            capability_info(process_id, registers.rdi, registers.rsi, registers.rdx)\n        }\n",
    "        abi::syscall::CAPABILITY_INFO => {\n            capability_info(process_id, registers.rdi, registers.rsi, registers.rdx)\n        }\n        abi::syscall::CAPABILITY_HANDLE_AT_SLOT => {\n            capability_handle_at_slot(process_id, registers.rdi)\n        }\n",
    "slot lookup dispatch",
)
info_marker = '''fn capability_info(process_id: u64, handle: u64, address: u64, length: u64) -> u64 {
'''
slot_function = '''fn capability_handle_at_slot(process_id: u64, slot: u64) -> u64 {
    let Ok(slot) = u16::try_from(slot) else {
        return error_return(abi::errno::INVALID_ARGUMENT);
    };
    if slot == 0 || usize::from(slot) > abi::limits::MAX_CAPABILITIES_PER_PROCESS {
        return error_return(abi::errno::INVALID_ARGUMENT);
    }
    let registry = CAPABILITY_REGISTRY.lock();
    match registry.handle_at_slot(process_id, slot) {
        Some(handle) => handle,
        None => error_return(abi::errno::NO_ENTRY),
    }
}

'''
if slot_function not in text:
    text = replace_once(text, info_marker, slot_function + info_marker, "slot lookup function")
write(path, text)

# Direct-child bootstrap now requests a deterministic slot and receives the
# child's actual generation-checked opaque handle.
path = "kernel/src/process/userspace_platform/capability_grant_entry.rs"
text = read(path)
old = '''    fn insert_entry_at(
        &mut self,
        process_id: u64,
        object: CapabilityObjectRef,
        rights: u64,
        requested_handle: u64,
    ) -> Result<u64, i64> {
        if requested_handle == abi::capability::INVALID_HANDLE {
            return self.insert_entry(process_id, object, rights);
        }
        if rights == 0 || rights & !capability_allowed_rights(object.kind) != 0 {
            return Err(abi::errno::INVALID_ARGUMENT);
        }
        if self.object_index(object).is_none() {
            return Err(abi::errno::NO_ENTRY);
        }
        if requested_handle > abi::limits::MAX_CAPABILITIES_PER_PROCESS as u64 {
            return Err(abi::errno::INVALID_ARGUMENT);
        }
        let table_index = self.ensure_table(process_id)?;
        let table = &mut self.tables[table_index];
        if table.entries.len() >= abi::limits::MAX_CAPABILITIES_PER_PROCESS {
            return Err(abi::errno::NO_SPACE);
        }
        if table
            .entries
            .iter()
            .any(|entry| entry.handle == requested_handle)
        {
            return Err(abi::errno::NO_SPACE);
        }
        table.entries.push(CapabilityEntry {
            handle: requested_handle,
            object,
            rights,
        });
        Ok(requested_handle)
    }
'''
new = '''    fn insert_entry_at(
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
'''
text = replace_once(text, old, new, "grant requested slot")
text = text.replace("requested_child_handle", "requested_child_slot")
write(path, text)

# Raw userspace facade gains bounded slot lookup; safe handles can adopt the
# current handle installed at a well-known slot without decoding the raw value.
path = "userspace/src/ipc.rs"
text = read(path)
text = text.replace("requested_child_handle: CapabilityHandle", "requested_child_slot: u64")
text = text.replace("in(\"r10\") requested_child_handle,", "in(\"r10\") requested_child_slot,")
close_marker = '''pub fn close(handle: CapabilityHandle) -> Result<()> {
'''
slot_helpers = '''pub fn handle_at_slot(slot: u64) -> Result<CapabilityHandle> {
    let mut result = syscall::CAPABILITY_HANDLE_AT_SLOT;
    unsafe {
        asm!("int 0x80", inlateout("rax") result, in("rdi") slot);
    }
    decode(result)
}

pub fn info_at_slot(slot: u64) -> Result<CapabilityInfo> {
    info(handle_at_slot(slot)?)
}

pub fn close_at_slot(slot: u64) -> Result<()> {
    close(handle_at_slot(slot)?)
}

'''
if slot_helpers not in text:
    text = replace_once(text, close_marker, slot_helpers + close_marker, "userspace slot helpers")
write(path, text)

path = "userspace/src/handle.rs"
text = read(path)
needle = '''    pub unsafe fn from_raw(raw: CapabilityHandle) -> ipc::Result<Self> {
        let raw = NonZeroU64::new(raw).ok_or(ipc::Error::INVALID_ARGUMENT)?;
        Ok(Self::from_nonzero(raw))
    }
'''
replacement = needle + '''
    /// Adopts the current capability installed in one process-local table slot.
    ///
    /// The slot number is a discovery coordinate, not authority. The kernel
    /// returns the current opaque generation-checked handle, which remains the
    /// value used for all subsequent operations.
    ///
    /// # Safety
    ///
    /// The caller must own the capability installed at `slot`, must not adopt it
    /// more than once, and for a typed marker must know the object's kind.
    pub unsafe fn from_slot(slot: u64) -> ipc::Result<Self> {
        let raw = ipc::handle_at_slot(slot)?;
        unsafe { Self::from_raw(raw) }
    }
'''
text = replace_once(text, needle, replacement, "owned handle slot adoption")
write(path, text)

# Rename the managed-startup constant to what it actually represents.
path = "userspace/src/process_start.rs"
text = read(path)
text = replace_once(
    text,
    "/// Well-known handle used by the managed-launch bootstrap pilot.\npub const PROCESS_START_BOOTSTRAP_HANDLE: u64 = 1;",
    "/// Well-known capability-table slot used by managed-launch bootstrap.\n///\n/// The slot is stable; the opaque handle installed there carries a generation\n/// and must be resolved with `ipc::handle_at_slot` or `OwnedHandle::from_slot`.\npub const PROCESS_START_BOOTSTRAP_SLOT: u64 = 1;",
    "bootstrap slot constant",
)
write(path, text)

# Rename imports/usages in userspace, then fix the known semantics that formerly
# treated a slot number as a raw handle value.
for rust_path in Path("userspace/src").rglob("*.rs"):
    text = rust_path.read_text()
    if "PROCESS_START_BOOTSTRAP_HANDLE" in text:
        text = text.replace("PROCESS_START_BOOTSTRAP_HANDLE", "PROCESS_START_BOOTSTRAP_SLOT")
    text = re.sub(
        r"OwnedHandle::<([^>]+)>::from_raw\(PROCESS_START_BOOTSTRAP_SLOT\)",
        r"OwnedHandle::<\1>::from_slot(PROCESS_START_BOOTSTRAP_SLOT)",
        text,
    )
    text = text.replace(
        ".any(|handle| ipc::info(handle).is_ok())",
        ".any(|slot| ipc::info_at_slot(slot).is_ok())",
    )
    text = text.replace(
        "ipc::info(PROCESS_START_BOOTSTRAP_SLOT)",
        "ipc::info_at_slot(PROCESS_START_BOOTSTRAP_SLOT)",
    )
    rust_path.write_text(text)

path = "userspace/src/syscall_facade.rs"
text = read(path)
old_exec = '''    if ipc::info_at_slot(PROCESS_START_BOOTSTRAP_SLOT).is_ok() {
        return Err(Errno::INVALID_ARGUMENT);
    }
    let (receiver, sender) = ipc::endpoint_create_pair().map_err(|_| Errno::IO)?;
    let receiver_ready = receiver == PROCESS_START_BOOTSTRAP_SLOT
        && ipc::replace(receiver, Rights::RECEIVE).ok() == Some(receiver);
'''
new_exec = '''    if ipc::info_at_slot(PROCESS_START_BOOTSTRAP_SLOT).is_ok() {
        return Err(Errno::INVALID_ARGUMENT);
    }
    let (receiver, sender) = ipc::endpoint_create_pair().map_err(|_| Errno::IO)?;
    let receiver_ready = ipc::handle_at_slot(PROCESS_START_BOOTSTRAP_SLOT).ok() == Some(receiver)
        && ipc::replace(receiver, Rights::RECEIVE).ok() == Some(receiver);
'''
text = replace_once(text, old_exec, new_exec, "managed exec bootstrap slot")
old_close_all = '''pub fn close_all_capabilities() -> Result<()> {
    for handle in 1..=limits::MAX_CAPABILITIES_PER_PROCESS as u64 {
        if ipc::info(handle).is_ok() && ipc::close(handle).is_err() {
            return Err(Errno::IO);
        }
    }
    Ok(())
}
'''
new_close_all = '''pub fn close_all_capabilities() -> Result<()> {
    for slot in 1..=limits::MAX_CAPABILITIES_PER_PROCESS as u64 {
        match ipc::handle_at_slot(slot) {
            Ok(handle) if ipc::close(handle).is_err() => return Err(Errno::IO),
            Ok(_) | Err(ipc::Error::NO_ENTRY) => {}
            Err(_) => return Err(Errno::IO),
        }
    }
    Ok(())
}
'''
text = replace_once(text, old_close_all, new_close_all, "generation-safe close all")
old_grant = '''    let granted = ipc::grant_child(
        process_id,
        receiver,
        Rights::RECEIVE,
        PROCESS_START_BOOTSTRAP_SLOT,
    )
    .ok()
        == Some(PROCESS_START_BOOTSTRAP_SLOT);
'''
new_grant = '''    let granted = ipc::grant_child(
        process_id,
        receiver,
        Rights::RECEIVE,
        PROCESS_START_BOOTSTRAP_SLOT,
    )
    .is_ok();
'''
text = replace_once(text, old_grant, new_grant, "managed child slot grant")
text = text.replace("bootstrap handle 1", "bootstrap slot 1")
write(path, text)

# Ensure all startup code has stopped treating the well-known slot as a raw handle.
for rust_path in Path("userspace/src").rglob("*.rs"):
    text = rust_path.read_text()
    if "from_raw(PROCESS_START_BOOTSTRAP_SLOT)" in text:
        raise RuntimeError(f"raw bootstrap-slot adoption remains in {rust_path}")
    if "ipc::info(PROCESS_START_BOOTSTRAP_SLOT)" in text:
        raise RuntimeError(f"raw bootstrap-slot info remains in {rust_path}")

print("handle-generation source patch applied")
