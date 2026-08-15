//! Ownership-safe wrappers for process-local capability handles.
//!
//! [`crate::ipc`] remains the raw, compatibility-facing syscall layer. This module
//! makes handle ownership explicit: an [`OwnedHandle`] closes its capability on drop,
//! a [`BorrowedHandle`] cannot outlive its owner, and object marker types retain the
//! validated kernel-object kind across ordinary operations.

use core::{marker::PhantomData, num::NonZeroU64};

use crate::ipc::{self, CapabilityHandle, CapabilityInfo, Deadline, ObjectKind, Rights, Signals};

mod sealed {
    pub trait Sealed {}
}

/// Marker trait for the object kind carried by an owned or borrowed handle.
pub trait ObjectType: sealed::Sealed {}

/// Marker trait for handles whose kernel object kind is known.
pub trait KnownObjectType: ObjectType {
    const KIND: ObjectKind;
}

/// A capability whose object kind has not yet been validated.
#[derive(Debug)]
pub enum AnyObject {}

/// A mailbox endpoint capability.
#[derive(Debug)]
pub enum Endpoint {}

/// A counted notification capability.
#[derive(Debug)]
pub enum Notification {}

/// A copied shared-memory capability.
#[derive(Debug)]
pub enum SharedMemory {}

/// A kernel early-log reader capability.
#[derive(Debug)]
pub enum KernelEarlyLogReader {}

/// A hierarchical job capability.
#[derive(Debug)]
pub enum Job {}

impl sealed::Sealed for AnyObject {}
impl sealed::Sealed for Endpoint {}
impl sealed::Sealed for Notification {}
impl sealed::Sealed for SharedMemory {}
impl sealed::Sealed for KernelEarlyLogReader {}
impl sealed::Sealed for Job {}

impl ObjectType for AnyObject {}
impl ObjectType for Endpoint {}
impl ObjectType for Notification {}
impl ObjectType for SharedMemory {}
impl ObjectType for KernelEarlyLogReader {}
impl ObjectType for Job {}

impl KnownObjectType for Endpoint {
    const KIND: ObjectKind = ObjectKind::Endpoint;
}

impl KnownObjectType for Notification {
    const KIND: ObjectKind = ObjectKind::Notification;
}

impl KnownObjectType for SharedMemory {
    const KIND: ObjectKind = ObjectKind::SharedMemory;
}

impl KnownObjectType for KernelEarlyLogReader {
    const KIND: ObjectKind = ObjectKind::KernelEarlyLogReader;
}

impl KnownObjectType for Job {
    const KIND: ObjectKind = ObjectKind::Job;
}

/// Exclusive ownership of one process-local capability-table entry.
///
/// The handle is closed on drop. Use [`Self::into_raw`] only when transferring
/// ownership to code that will close or move the raw handle itself.
#[derive(Debug)]
pub struct OwnedHandle<T: ObjectType = AnyObject> {
    raw: Option<NonZeroU64>,
    object_type: PhantomData<T>,
}

/// A non-owning capability reference tied to the lifetime of its owner.
#[derive(Debug, PartialEq, Eq)]
pub struct BorrowedHandle<'a, T: ObjectType = AnyObject> {
    raw: NonZeroU64,
    owner: PhantomData<&'a OwnedHandle<T>>,
}

/// A capability attachment adopted from one received endpoint message.
#[derive(Debug)]
pub struct ReceivedCapability {
    pub handle: OwnedHandle<AnyObject>,
    pub rights: Rights,
}

/// Endpoint receive metadata whose optional attachment has explicit ownership.
#[derive(Debug)]
pub struct ReceivedMessage {
    pub sender_process_id: u64,
    pub bytes: usize,
    pub capability: Option<ReceivedCapability>,
}

impl<T: ObjectType> Copy for BorrowedHandle<'_, T> {}

impl<T: ObjectType> Clone for BorrowedHandle<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ObjectType> OwnedHandle<T> {
    /// Adopts exclusive ownership of an existing raw handle.
    ///
    /// # Safety
    ///
    /// The caller must own `raw`, must not adopt it more than once, and must not
    /// close or move it except through the returned value. For a typed marker, the
    /// caller must also know that the kernel object has the matching kind.
    pub unsafe fn from_raw(raw: CapabilityHandle) -> ipc::Result<Self> {
        let raw = NonZeroU64::new(raw).ok_or(ipc::Error::INVALID_ARGUMENT)?;
        Ok(Self::from_nonzero(raw))
    }

    fn from_nonzero(raw: NonZeroU64) -> Self {
        Self {
            raw: Some(raw),
            object_type: PhantomData,
        }
    }

    fn adopt(raw: CapabilityHandle) -> ipc::Result<Self> {
        let raw = NonZeroU64::new(raw).ok_or(ipc::Error::IO)?;
        Ok(Self::from_nonzero(raw))
    }

    pub fn as_raw(&self) -> CapabilityHandle {
        self.raw.expect("owned handle remains live").get()
    }

    pub fn borrow(&self) -> BorrowedHandle<'_, T> {
        BorrowedHandle {
            raw: self.raw.expect("owned handle remains live"),
            owner: PhantomData,
        }
    }

    /// Relinquishes ownership without closing the handle.
    #[must_use = "the returned raw handle must be closed or moved"]
    pub fn into_raw(mut self) -> CapabilityHandle {
        self.raw.take().expect("owned handle remains live").get()
    }

    /// Closes the handle now instead of waiting for drop.
    pub fn close(mut self) -> ipc::Result<()> {
        let raw = self.raw.take().expect("owned handle remains live").get();
        close_raw(raw)
    }

    pub fn info(&self) -> ipc::Result<CapabilityInfo> {
        ipc::info(self.as_raw())
    }

    pub fn signal_state(&self) -> ipc::Result<Signals> {
        ipc::signal_state(self.as_raw())
    }

    pub fn wait(&self, requested: Signals, deadline: Deadline) -> ipc::Result<Signals> {
        ipc::wait_one(self.as_raw(), requested, deadline)
    }

    /// Duplicates this capability with the requested reduced rights.
    pub fn duplicate(&self, rights: Rights) -> ipc::Result<Self> {
        Self::adopt(ipc::duplicate(self.as_raw(), rights)?)
    }

    /// Atomically replaces this table entry with a rights-reduced entry.
    ///
    /// A syscall failure leaves the original handle owned and unchanged.
    pub fn replace_rights(&mut self, rights: Rights) -> ipc::Result<()> {
        let replacement = ipc::replace(self.as_raw(), rights)?;
        let Some(replacement) = NonZeroU64::new(replacement) else {
            // A successful replacement invalidates the old entry. Do not close the
            // stale value if a broken kernel violates the nonzero-handle ABI.
            self.raw = None;
            return Err(ipc::Error::IO);
        };
        self.raw = Some(replacement);
        Ok(())
    }

    /// Erases the compile-time object kind without changing ownership.
    pub fn erase(mut self) -> OwnedHandle<AnyObject> {
        let raw = self.raw.take().expect("owned handle remains live");
        OwnedHandle::from_nonzero(raw)
    }

    /// Validates and assigns a concrete object marker.
    ///
    /// On failure the original owned handle is returned to the caller.
    pub fn try_cast<U: KnownObjectType>(
        self,
    ) -> core::result::Result<OwnedHandle<U>, (ipc::Error, Self)> {
        match self.info() {
            Ok(info) if info.kind == U::KIND => {
                let mut this = self;
                let raw = this.raw.take().expect("owned handle remains live");
                Ok(OwnedHandle::from_nonzero(raw))
            }
            Ok(_) => Err((ipc::Error::INVALID_ARGUMENT, self)),
            Err(error) => Err((error, self)),
        }
    }
}

impl OwnedHandle<Endpoint> {
    pub fn create() -> ipc::Result<Self> {
        Self::adopt(ipc::endpoint_create()?)
    }

    pub fn send(&self, bytes: &[u8]) -> ipc::Result<()> {
        self.borrow().send(bytes)
    }

    pub fn try_receive(&self, output: &mut [u8]) -> ipc::Result<ReceivedMessage> {
        self.borrow().try_receive(output)
    }
}

impl OwnedHandle<Notification> {
    pub fn create() -> ipc::Result<Self> {
        Self::adopt(ipc::notification_create()?)
    }

    pub fn signal(&self, amount: u64) -> ipc::Result<u64> {
        ipc::notification_signal(self.as_raw(), amount)
    }

    pub fn try_wait(&self) -> ipc::Result<u64> {
        ipc::notification_try_wait(self.as_raw())
    }
}

impl OwnedHandle<SharedMemory> {
    pub fn create(length: usize) -> ipc::Result<Self> {
        Self::adopt(ipc::shared_memory_create(length)?)
    }

    pub fn read(&self, offset: usize, output: &mut [u8]) -> ipc::Result<usize> {
        ipc::shared_memory_read(self.as_raw(), offset, output)
    }

    pub fn write(&self, offset: usize, bytes: &[u8]) -> ipc::Result<usize> {
        ipc::shared_memory_write(self.as_raw(), offset, bytes)
    }
}

impl OwnedHandle<Job> {
    pub fn create() -> ipc::Result<Self> {
        Self::adopt(ipc::job_create()?)
    }

    pub fn create_child(&self) -> ipc::Result<Self> {
        Self::adopt(ipc::job_create_child(self.as_raw())?)
    }
}

impl<'a, T: ObjectType> BorrowedHandle<'a, T> {
    pub const fn as_raw(self) -> CapabilityHandle {
        self.raw.get()
    }

    pub fn info(self) -> ipc::Result<CapabilityInfo> {
        ipc::info(self.as_raw())
    }

    pub fn signal_state(self) -> ipc::Result<Signals> {
        ipc::signal_state(self.as_raw())
    }

    pub fn wait(self, requested: Signals, deadline: Deadline) -> ipc::Result<Signals> {
        ipc::wait_one(self.as_raw(), requested, deadline)
    }

    pub fn duplicate(self, rights: Rights) -> ipc::Result<OwnedHandle<T>> {
        OwnedHandle::adopt(ipc::duplicate(self.as_raw(), rights)?)
    }

    pub const fn wait_item(self, requested: Signals) -> ipc::WaitItem {
        ipc::WaitItem::new(self.as_raw(), requested)
    }
}

impl BorrowedHandle<'_, Endpoint> {
    pub fn send(self, bytes: &[u8]) -> ipc::Result<()> {
        ipc::send(self.as_raw(), bytes, None)
    }

    pub fn try_receive(self, output: &mut [u8]) -> ipc::Result<ReceivedMessage> {
        let message = ipc::try_receive(self.as_raw(), output)?;
        let capability = match message.capability {
            Some(capability) => Some(ReceivedCapability {
                handle: OwnedHandle::adopt(capability.handle)?,
                rights: capability.rights,
            }),
            None => None,
        };
        Ok(ReceivedMessage {
            sender_process_id: message.sender_process_id,
            bytes: message.bytes,
            capability,
        })
    }
}

impl<T: ObjectType> Drop for OwnedHandle<T> {
    fn drop(&mut self) {
        if let Some(raw) = self.raw.take() {
            let _ = close_raw(raw.get());
        }
    }
}

#[cfg(not(test))]
fn close_raw(raw: CapabilityHandle) -> ipc::Result<()> {
    ipc::close(raw)
}

#[cfg(test)]
fn close_raw(raw: CapabilityHandle) -> ipc::Result<()> {
    tests::record_close(raw);
    Ok(())
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::{AnyObject, OwnedHandle};
    use crate::ipc;

    static CLOSED_COUNT: AtomicUsize = AtomicUsize::new(0);
    static LAST_CLOSED: AtomicU64 = AtomicU64::new(0);

    pub(super) fn record_close(raw: u64) {
        LAST_CLOSED.store(raw, Ordering::SeqCst);
        CLOSED_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    fn reset_closes() {
        LAST_CLOSED.store(0, Ordering::SeqCst);
        CLOSED_COUNT.store(0, Ordering::SeqCst);
    }

    #[test]
    fn owned_handles_close_once_and_raw_transfer_suppresses_drop() {
        reset_closes();
        {
            let owned = unsafe { OwnedHandle::<AnyObject>::from_raw(41) }.unwrap();
            assert_eq!(owned.as_raw(), 41);
            assert_eq!(owned.borrow().as_raw(), 41);
        }
        assert_eq!(CLOSED_COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(LAST_CLOSED.load(Ordering::SeqCst), 41);

        let owned = unsafe { OwnedHandle::<AnyObject>::from_raw(42) }.unwrap();
        assert_eq!(owned.into_raw(), 42);
        assert_eq!(CLOSED_COUNT.load(Ordering::SeqCst), 1);

        let owned = unsafe { OwnedHandle::<AnyObject>::from_raw(43) }.unwrap();
        assert!(owned.close().is_ok());
        assert_eq!(CLOSED_COUNT.load(Ordering::SeqCst), 2);
        assert_eq!(LAST_CLOSED.load(Ordering::SeqCst), 43);
    }

    #[test]
    fn zero_cannot_be_adopted() {
        assert_eq!(
            unsafe { OwnedHandle::<AnyObject>::from_raw(0) }.unwrap_err(),
            ipc::Error::INVALID_ARGUMENT
        );
    }
}
