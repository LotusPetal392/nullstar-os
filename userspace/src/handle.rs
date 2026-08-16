//! Ownership-safe wrappers for process-local capability handles.
//!
//! [`crate::ipc`] remains the raw, compatibility-facing syscall layer. This module
//! makes handle ownership explicit: an [`OwnedHandle`] closes its capability on drop,
//! a [`BorrowedHandle`] cannot outlive its owner, and object marker types retain the
//! validated kernel-object kind across ordinary operations.

use core::{marker::PhantomData, num::NonZeroU64};

use crate::ipc::{
    self, CapabilityHandle, CapabilityInfo, Deadline, EventPortEvent, ObjectKind, Rights, Signals,
    WaitSetEvent,
};

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

/// A legacy mailbox or paired channel-endpoint capability.
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

/// A bounded persistent set of tagged object-signal registrations.
#[derive(Debug)]
pub enum WaitSet {}

/// A bounded FIFO of coalesced object-signal edges.
#[derive(Debug)]
pub enum EventPort {}

/// A one-shot absolute-monotonic timer capability.
#[derive(Debug)]
pub enum Timer {}

/// A persistent user-controlled manual-reset event capability.
#[derive(Debug)]
pub enum Event {}

impl sealed::Sealed for AnyObject {}
impl sealed::Sealed for Endpoint {}
impl sealed::Sealed for Notification {}
impl sealed::Sealed for SharedMemory {}
impl sealed::Sealed for KernelEarlyLogReader {}
impl sealed::Sealed for Job {}
impl sealed::Sealed for WaitSet {}
impl sealed::Sealed for EventPort {}
impl sealed::Sealed for Timer {}
impl sealed::Sealed for Event {}

impl ObjectType for AnyObject {}
impl ObjectType for Endpoint {}
impl ObjectType for Notification {}
impl ObjectType for SharedMemory {}
impl ObjectType for KernelEarlyLogReader {}
impl ObjectType for Job {}
impl ObjectType for WaitSet {}
impl ObjectType for EventPort {}
impl ObjectType for Timer {}
impl ObjectType for Event {}

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

impl KnownObjectType for WaitSet {
    const KIND: ObjectKind = ObjectKind::WaitSet;
}

impl KnownObjectType for EventPort {
    const KIND: ObjectKind = ObjectKind::EventPort;
}

impl KnownObjectType for Timer {
    const KIND: ObjectKind = ObjectKind::Timer;
}

impl KnownObjectType for Event {
    const KIND: ObjectKind = ObjectKind::Event;
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

/// One ownership-consuming attachment for a multi-handle endpoint send.
#[derive(Debug)]
pub struct MoveHandle {
    handle: OwnedHandle<AnyObject>,
    rights: Rights,
}

impl MoveHandle {
    pub fn new<T: ObjectType>(handle: OwnedHandle<T>, rights: Rights) -> Self {
        Self {
            handle: handle.erase(),
            rights,
        }
    }

    pub fn handle(&self) -> &OwnedHandle<AnyObject> {
        &self.handle
    }

    pub const fn rights(&self) -> Rights {
        self.rights
    }

    pub fn into_handle(self) -> OwnedHandle<AnyObject> {
        self.handle
    }
}

/// Typed metadata and owned attachments from one multi-handle receive.
#[derive(Debug)]
pub struct ReceivedMessageMany<const N: usize> {
    pub sender_process_id: u64,
    pub bytes: usize,
    pub capabilities: [Option<ReceivedCapability>; N],
    pub capability_count: usize,
}

/// A failed ownership-consuming endpoint send.
///
/// The kernel leaves a moved capability installed when enqueueing fails, so this
/// error returns the still-owned handle for retry, inspection, or cleanup.
#[derive(Debug)]
pub struct SendMoveError<T: ObjectType> {
    error: ipc::Error,
    handle: OwnedHandle<T>,
}

/// A failed ownership-consuming multi-handle endpoint send.
#[derive(Debug)]
pub struct SendMoveManyError<const N: usize> {
    error: ipc::Error,
    handles: [MoveHandle; N],
}

impl<const N: usize> SendMoveManyError<N> {
    pub(crate) fn new(error: ipc::Error, handles: [MoveHandle; N]) -> Self {
        Self { error, handles }
    }

    pub const fn error(&self) -> ipc::Error {
        self.error
    }

    pub fn into_handles(self) -> [MoveHandle; N] {
        self.handles
    }

    pub fn into_parts(self) -> (ipc::Error, [MoveHandle; N]) {
        (self.error, self.handles)
    }
}

impl<T: ObjectType> SendMoveError<T> {
    pub(crate) fn new(error: ipc::Error, handle: OwnedHandle<T>) -> Self {
        Self { error, handle }
    }

    pub const fn error(&self) -> ipc::Error {
        self.error
    }

    pub fn into_handle(self) -> OwnedHandle<T> {
        self.handle
    }

    pub fn into_parts(self) -> (ipc::Error, OwnedHandle<T>) {
        (self.error, self.handle)
    }
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

    pub fn create_pair() -> ipc::Result<(Self, Self)> {
        let (first, second) = ipc::endpoint_create_pair()?;
        let first = Self::adopt(first)?;
        match Self::adopt(second) {
            Ok(second) => Ok((first, second)),
            Err(error) => {
                drop(first);
                Err(error)
            }
        }
    }

    pub fn send(&self, bytes: &[u8]) -> ipc::Result<()> {
        self.borrow().send(bytes)
    }

    /// Atomically sends a message and moves `handle` into its attachment.
    ///
    /// Success consumes the source capability. Failure returns it inside
    /// [`SendMoveError`], preserving ownership for an explicit retry or drop.
    pub fn send_move<T: ObjectType>(
        &self,
        bytes: &[u8],
        handle: OwnedHandle<T>,
        rights: Rights,
    ) -> Result<(), SendMoveError<T>> {
        self.borrow().send_move(bytes, handle, rights)
    }

    /// Atomically sends a message and moves every supplied handle.
    ///
    /// Success consumes all sources. Failure returns every source in its
    /// original order inside [`SendMoveManyError`].
    pub fn send_move_many<const N: usize>(
        &self,
        bytes: &[u8],
        handles: [MoveHandle; N],
    ) -> Result<(), SendMoveManyError<N>> {
        self.borrow().send_move_many(bytes, handles)
    }

    pub fn try_receive(&self, output: &mut [u8]) -> ipc::Result<ReceivedMessage> {
        self.borrow().try_receive(output)
    }

    pub fn try_receive_many<const N: usize>(
        &self,
        output: &mut [u8],
    ) -> Result<ReceivedMessageMany<N>, ipc::ReceiveManyError> {
        self.borrow().try_receive_many(output)
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

impl OwnedHandle<WaitSet> {
    pub fn create() -> ipc::Result<Self> {
        Self::adopt(ipc::wait_set_create()?)
    }

    pub fn add<T: ObjectType>(
        &self,
        target: BorrowedHandle<'_, T>,
        requested: Signals,
        key: u64,
    ) -> ipc::Result<()> {
        ipc::wait_set_add(self.as_raw(), target.as_raw(), requested, key)
    }

    pub fn remove(&self, key: u64) -> ipc::Result<()> {
        ipc::wait_set_remove(self.as_raw(), key)
    }

    pub fn wait_next(&self, deadline: Deadline) -> ipc::Result<WaitSetEvent> {
        ipc::wait_set_wait(self.as_raw(), deadline)
    }
}

impl OwnedHandle<EventPort> {
    pub fn create() -> ipc::Result<Self> {
        Self::adopt(ipc::event_port_create()?)
    }

    pub fn add<T: ObjectType>(
        &self,
        target: BorrowedHandle<'_, T>,
        requested: Signals,
        key: u64,
    ) -> ipc::Result<()> {
        ipc::event_port_add(self.as_raw(), target.as_raw(), requested, key)
    }

    pub fn remove(&self, key: u64) -> ipc::Result<()> {
        ipc::event_port_remove(self.as_raw(), key)
    }

    pub fn wait_next(&self, deadline: Deadline) -> ipc::Result<EventPortEvent> {
        ipc::event_port_wait(self.as_raw(), deadline)
    }
}

impl OwnedHandle<Timer> {
    pub fn create() -> ipc::Result<Self> {
        Self::adopt(ipc::timer_create()?)
    }

    pub fn arm(&self, deadline: Deadline) -> ipc::Result<()> {
        ipc::timer_arm(self.as_raw(), deadline)
    }

    pub fn cancel(&self) -> ipc::Result<()> {
        ipc::timer_cancel(self.as_raw())
    }
}

impl OwnedHandle<Event> {
    pub fn create() -> ipc::Result<Self> {
        Self::adopt(ipc::event_create()?)
    }

    pub fn set(&self) -> ipc::Result<()> {
        ipc::event_set(self.as_raw())
    }

    pub fn reset(&self) -> ipc::Result<()> {
        ipc::event_reset(self.as_raw())
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

    /// Atomically sends a message and moves `handle` into its attachment.
    ///
    /// Success consumes the source capability. Failure returns it inside
    /// [`SendMoveError`], preserving ownership for an explicit retry or drop.
    pub fn send_move<T: ObjectType>(
        self,
        bytes: &[u8],
        handle: OwnedHandle<T>,
        rights: Rights,
    ) -> Result<(), SendMoveError<T>> {
        send_move_with(self.as_raw(), bytes, handle, rights, ipc::send_move)
    }

    pub fn send_move_many<const N: usize>(
        self,
        bytes: &[u8],
        handles: [MoveHandle; N],
    ) -> Result<(), SendMoveManyError<N>> {
        send_move_many_with(self.as_raw(), bytes, handles, ipc::send_move_many)
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

    pub fn try_receive_many<const N: usize>(
        self,
        output: &mut [u8],
    ) -> Result<ReceivedMessageMany<N>, ipc::ReceiveManyError> {
        let mut raw: [Option<ipc::ReceivedCapability>; N] = core::array::from_fn(|_| None);
        let message = ipc::try_receive_many(self.as_raw(), output, &mut raw)?;
        let mut capabilities: [Option<ReceivedCapability>; N] = core::array::from_fn(|_| None);
        for index in 0..message.capabilities {
            let Some(capability) = raw[index].take() else {
                for capability in raw.into_iter().flatten() {
                    let _ = ipc::close(capability.handle);
                }
                return Err(ipc::ReceiveManyError::from_error(ipc::Error::IO));
            };
            let handle = match OwnedHandle::adopt(capability.handle) {
                Ok(handle) => handle,
                Err(_) => {
                    for capability in raw.into_iter().flatten() {
                        let _ = ipc::close(capability.handle);
                    }
                    return Err(ipc::ReceiveManyError::from_error(ipc::Error::IO));
                }
            };
            capabilities[index] = Some(ReceivedCapability {
                handle,
                rights: capability.rights,
            });
        }
        Ok(ReceivedMessageMany {
            sender_process_id: message.sender_process_id,
            bytes: message.bytes,
            capabilities,
            capability_count: message.capabilities,
        })
    }
}

fn send_move_with<T, F>(
    endpoint: CapabilityHandle,
    bytes: &[u8],
    handle: OwnedHandle<T>,
    rights: Rights,
    send: F,
) -> Result<(), SendMoveError<T>>
where
    T: ObjectType,
    F: FnOnce(CapabilityHandle, &[u8], ipc::Transfer) -> ipc::Result<()>,
{
    let transfer = ipc::Transfer {
        handle: handle.as_raw(),
        rights,
    };
    match send(endpoint, bytes, transfer) {
        Ok(()) => {
            // The successful syscall removed the process-local source entry.
            // Disarm Drop without attempting to close that now-invalid value.
            let _ = handle.into_raw();
            Ok(())
        }
        Err(error) => Err(SendMoveError { error, handle }),
    }
}

fn send_move_many_with<const N: usize, F>(
    endpoint: CapabilityHandle,
    bytes: &[u8],
    handles: [MoveHandle; N],
    send: F,
) -> Result<(), SendMoveManyError<N>>
where
    F: FnOnce(CapabilityHandle, &[u8], &[ipc::Transfer]) -> ipc::Result<()>,
{
    let transfers: [ipc::Transfer; N] = core::array::from_fn(|index| ipc::Transfer {
        handle: handles[index].handle.as_raw(),
        rights: handles[index].rights,
    });
    match send(endpoint, bytes, &transfers) {
        Ok(()) => {
            for handle in handles {
                let _ = handle.handle.into_raw();
            }
            Ok(())
        }
        Err(error) => Err(SendMoveManyError { error, handles }),
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

    use super::{AnyObject, MoveHandle, OwnedHandle, send_move_many_with, send_move_with};
    use crate::ipc::{self, Rights};

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
    fn owned_handles_close_once_and_transfers_preserve_ownership() {
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

        let owned = unsafe { OwnedHandle::<AnyObject>::from_raw(44) }.unwrap();
        let failed = send_move_with(
            7,
            b"retry",
            owned,
            Rights::WAIT,
            |endpoint, bytes, transfer| {
                assert_eq!(endpoint, 7);
                assert_eq!(bytes, b"retry");
                assert_eq!(transfer.handle, 44);
                assert_eq!(transfer.rights, Rights::WAIT);
                Err(ipc::Error::TRY_AGAIN)
            },
        )
        .unwrap_err();
        assert_eq!(failed.error(), ipc::Error::TRY_AGAIN);
        let (error, owned) = failed.into_parts();
        assert_eq!(error, ipc::Error::TRY_AGAIN);
        assert_eq!(owned.as_raw(), 44);
        drop(owned);
        assert_eq!(CLOSED_COUNT.load(Ordering::SeqCst), 3);
        assert_eq!(LAST_CLOSED.load(Ordering::SeqCst), 44);

        let owned = unsafe { OwnedHandle::<AnyObject>::from_raw(45) }.unwrap();
        assert!(send_move_with(7, b"sent", owned, Rights::WAIT, |_, _, _| Ok(())).is_ok());
        assert_eq!(CLOSED_COUNT.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn zero_cannot_be_adopted() {
        assert_eq!(
            unsafe { OwnedHandle::<AnyObject>::from_raw(0) }.unwrap_err(),
            ipc::Error::INVALID_ARGUMENT
        );
    }

    #[test]
    fn multi_handle_move_is_all_or_nothing_for_ownership() {
        reset_closes();
        let first = unsafe { OwnedHandle::<AnyObject>::from_raw(51) }.unwrap();
        let second = unsafe { OwnedHandle::<AnyObject>::from_raw(52) }.unwrap();
        let failed = send_move_many_with(
            9,
            b"retry-many",
            [
                MoveHandle::new(first, Rights::WAIT),
                MoveHandle::new(second, Rights::READ),
            ],
            |endpoint, bytes, transfers| {
                assert_eq!(endpoint, 9);
                assert_eq!(bytes, b"retry-many");
                assert_eq!(transfers.len(), 2);
                assert_eq!(transfers[0].handle, 51);
                assert_eq!(transfers[0].rights, Rights::WAIT);
                assert_eq!(transfers[1].handle, 52);
                assert_eq!(transfers[1].rights, Rights::READ);
                Err(ipc::Error::TRY_AGAIN)
            },
        )
        .unwrap_err();
        assert_eq!(failed.error(), ipc::Error::TRY_AGAIN);
        let handles = failed.into_handles();
        assert_eq!(handles[0].handle().as_raw(), 51);
        assert_eq!(handles[1].handle().as_raw(), 52);
        drop(handles);
        assert_eq!(CLOSED_COUNT.load(Ordering::SeqCst), 2);

        let first = unsafe { OwnedHandle::<AnyObject>::from_raw(53) }.unwrap();
        let second = unsafe { OwnedHandle::<AnyObject>::from_raw(54) }.unwrap();
        assert!(
            send_move_many_with(
                9,
                b"sent-many",
                [
                    MoveHandle::new(first, Rights::WAIT),
                    MoveHandle::new(second, Rights::READ),
                ],
                |_, _, transfers| {
                    assert_eq!(transfers.len(), 2);
                    Ok(())
                },
            )
            .is_ok()
        );
        assert_eq!(CLOSED_COUNT.load(Ordering::SeqCst), 2);
    }
}
