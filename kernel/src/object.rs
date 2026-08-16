//! Common kernel-object identity, rights, signal, and handle metadata.
//!
//! This module is deliberately independent from the existing process-local capability
//! implementation. It establishes the vocabulary and invariants that capability entries,
//! IPC endpoints, notifications, shared memory, processes, and future jobs can migrate to
//! incrementally without changing their current ABI in one step.

use core::fmt;
use core::num::{NonZeroU32, NonZeroU64};
use core::sync::atomic::{AtomicU64, Ordering};

/// Stable diagnostic identity for one kernel object during the current boot.
///
/// Object IDs are not handles, capabilities, or persistent identifiers. They are never
/// reused by an allocator and exist so diagnostics can correlate multiple handles that
/// refer to the same object.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ObjectId(NonZeroU64);

impl ObjectId {
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Monotonic object-ID allocator.
///
/// Exhaustion is reported rather than wrapping and reusing an identity.
pub struct ObjectIdAllocator {
    next: AtomicU64,
}

impl ObjectIdAllocator {
    pub const fn new(first: NonZeroU64) -> Self {
        Self {
            next: AtomicU64::new(first.get()),
        }
    }

    pub fn allocate(&self) -> Result<ObjectId, ObjectIdExhausted> {
        let value = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| ObjectIdExhausted)?;

        let value = NonZeroU64::new(value).ok_or(ObjectIdExhausted)?;
        Ok(ObjectId::new(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectIdExhausted;

/// Runtime type of a kernel object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ObjectType {
    Process = 1,
    Thread = 2,
    AddressSpace = 3,
    Job = 4,
    Channel = 5,
    Notification = 6,
    SharedMemory = 7,
    Timer = 8,
    EventPort = 9,
    Device = 10,
    Event = 11,
}

/// Immutable rights attached to one handle.
///
/// Rights can only be preserved or reduced when a handle is duplicated or transferred.
#[derive(Clone, Copy, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct Rights(u64);

impl Rights {
    pub const NONE: Self = Self(0);
    pub const INSPECT: Self = Self(1 << 0);
    pub const DUPLICATE: Self = Self(1 << 1);
    pub const TRANSFER: Self = Self(1 << 2);
    pub const WAIT: Self = Self(1 << 3);
    pub const SIGNAL: Self = Self(1 << 4);
    pub const READ: Self = Self(1 << 5);
    pub const WRITE: Self = Self(1 << 6);
    pub const MAP: Self = Self(1 << 7);
    pub const GET_PROPERTY: Self = Self(1 << 8);
    pub const SET_PROPERTY: Self = Self(1 << 9);
    pub const MANAGE: Self = Self(1 << 10);

    pub const BASIC: Self =
        Self(Self::INSPECT.0 | Self::DUPLICATE.0 | Self::TRANSFER.0 | Self::WAIT.0);

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Validate a requested rights reduction.
    pub const fn reduce_to(self, requested: Self) -> Result<Self, RightsEscalation> {
        if self.contains(requested) {
            Ok(requested)
        } else {
            Err(RightsEscalation)
        }
    }
}

impl fmt::Debug for Rights {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Rights({:#x})", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RightsEscalation;

/// Level-triggered object signals consumed by wait operations.
#[derive(Clone, Copy, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct Signals(u64);

impl Signals {
    pub const NONE: Self = Self(0);
    pub const READABLE: Self = Self(1 << 0);
    pub const WRITABLE: Self = Self(1 << 1);
    pub const PEER_CLOSED: Self = Self(1 << 2);
    pub const SIGNALED: Self = Self(1 << 3);
    pub const TERMINATED: Self = Self(1 << 4);
    pub const TIMER_FIRED: Self = Self(1 << 5);

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl fmt::Debug for Signals {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Signals({:#x})", self.0)
    }
}

/// Common header embedded by concrete kernel objects.
pub struct ObjectHeader {
    id: ObjectId,
    object_type: ObjectType,
    signals: AtomicU64,
}

impl ObjectHeader {
    pub const fn new(id: ObjectId, object_type: ObjectType) -> Self {
        Self {
            id,
            object_type,
            signals: AtomicU64::new(0),
        }
    }

    pub const fn id(&self) -> ObjectId {
        self.id
    }

    pub const fn object_type(&self) -> ObjectType {
        self.object_type
    }

    pub fn signals(&self) -> Signals {
        Signals::from_bits(self.signals.load(Ordering::Acquire))
    }

    /// Atomically clear and set signals, returning both the old and new state.
    pub fn update_signals(&self, clear: Signals, set: Signals) -> SignalChange {
        let old = self
            .signals
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some((current & !clear.bits()) | set.bits())
            })
            .expect("signal update closure always returns a value");
        let new = (old & !clear.bits()) | set.bits();

        SignalChange {
            old: Signals::from_bits(old),
            new: Signals::from_bits(new),
        }
    }
}

impl fmt::Debug for ObjectHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObjectHeader")
            .field("id", &self.id)
            .field("object_type", &self.object_type)
            .field("signals", &self.signals())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalChange {
    pub old: Signals,
    pub new: Signals,
}

impl SignalChange {
    pub const fn newly_asserted(self) -> Signals {
        Signals::from_bits(self.new.bits() & !self.old.bits())
    }

    pub const fn newly_cleared(self) -> Signals {
        Signals::from_bits(self.old.bits() & !self.new.bits())
    }
}

/// Process-local handle-table key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct HandleId(NonZeroU32);

impl HandleId {
    pub const fn new(value: NonZeroU32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// Object metadata stored by a handle-table entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HandleMetadata {
    object_id: ObjectId,
    object_type: ObjectType,
    rights: Rights,
}

impl HandleMetadata {
    pub const fn new(object_id: ObjectId, object_type: ObjectType, rights: Rights) -> Self {
        Self {
            object_id,
            object_type,
            rights,
        }
    }

    pub const fn object_id(self) -> ObjectId {
        self.object_id
    }

    pub const fn object_type(self) -> ObjectType {
        self.object_type
    }

    pub const fn rights(self) -> Rights {
        self.rights
    }

    pub const fn reduced(self, requested: Rights) -> Result<Self, RightsEscalation> {
        match self.rights.reduce_to(requested) {
            Ok(rights) => Ok(Self::new(self.object_id, self.object_type, rights)),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;

    use super::*;

    #[test]
    fn object_ids_are_monotonic_and_nonzero() {
        let allocator = ObjectIdAllocator::new(NonZeroU64::new(41).unwrap());

        assert_eq!(allocator.allocate().unwrap().get(), 41);
        assert_eq!(allocator.allocate().unwrap().get(), 42);
    }

    #[test]
    fn rights_can_only_be_reduced() {
        let original = Rights::BASIC.union(Rights::READ).union(Rights::WRITE);
        let reduced = original
            .reduce_to(Rights::BASIC.union(Rights::READ))
            .unwrap();

        assert!(reduced.contains(Rights::READ));
        assert!(!reduced.contains(Rights::WRITE));
        assert_eq!(
            original.reduce_to(original.union(Rights::MANAGE)),
            Err(RightsEscalation)
        );
    }

    #[test]
    fn handle_metadata_preserves_identity_and_type_during_reduction() {
        let id = ObjectId::new(NonZeroU64::new(7).unwrap());
        let metadata = HandleMetadata::new(
            id,
            ObjectType::Channel,
            Rights::BASIC.union(Rights::READ).union(Rights::WRITE),
        );
        let reduced = metadata.reduced(Rights::BASIC.union(Rights::READ)).unwrap();

        assert_eq!(reduced.object_id(), id);
        assert_eq!(reduced.object_type(), ObjectType::Channel);
        assert!(!reduced.rights().contains(Rights::WRITE));
    }

    #[test]
    fn signal_updates_report_edges_without_changing_level_semantics() {
        let id = ObjectId::new(NonZeroU64::new(1).unwrap());
        let header = ObjectHeader::new(id, ObjectType::Notification);

        let asserted = header.update_signals(Signals::NONE, Signals::SIGNALED);
        assert_eq!(asserted.newly_asserted(), Signals::SIGNALED);
        assert!(header.signals().contains(Signals::SIGNALED));

        let replaced = header.update_signals(Signals::SIGNALED, Signals::TERMINATED);
        assert_eq!(replaced.newly_cleared(), Signals::SIGNALED);
        assert_eq!(replaced.newly_asserted(), Signals::TERMINATED);
        assert!(header.signals().contains(Signals::TERMINATED));
    }
}
