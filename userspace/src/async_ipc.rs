//! Allocation-free asynchronous endpoint operations over bounded object waits.
//!
//! [`Reactor`] drives one scoped future tree. Endpoint futures attempt their
//! non-blocking operation first, register level-triggered readiness on
//! [`crate::ipc::Error::TRY_AGAIN`], and let the runner sleep in the kernel's
//! bounded `wait_many` syscall. This is the initial single-threaded runtime
//! layer; persistent event ports and independently spawned task scheduling
//! remain future work.

use core::{
    array,
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use crate::{
    handle::{BorrowedHandle, Endpoint, ObjectType, OwnedHandle, ReceivedMessage, SendMoveError},
    ipc::{self, CapabilityHandle, Deadline, Rights, Signals, WaitItem},
};

/// Failure from the scoped reactor itself rather than from the driven future.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunError {
    /// A kernel object wait failed, including expiry of the supplied deadline.
    Wait(ipc::Error),
    /// A future returned `Pending` without registering any waitable object.
    UnregisteredPending,
    /// The same reactor was entered recursively.
    AlreadyRunning,
}

struct Registration {
    item: WaitItem,
    waker: Waker,
}

/// A bounded, allocation-free reactor for one scoped future tree.
///
/// `N` bounds wait registrations in one poll cycle. It may not
/// exceed the kernel's `MAX_OBJECT_WAIT_ITEMS`; excess registrations fail with
/// [`ipc::Error::NO_SPACE`] through the operation future.
pub struct Reactor<const N: usize> {
    registrations: RefCell<[Option<Registration>; N]>,
    running: Cell<bool>,
}

impl<const N: usize> Reactor<N> {
    pub fn new() -> Self {
        Self {
            registrations: RefCell::new(array::from_fn(|_| None)),
            running: Cell::new(false),
        }
    }

    /// Drives an `Unpin` future with no deadline.
    pub fn run<F: Future + Unpin>(&self, future: &mut F) -> Result<F::Output, RunError> {
        self.run_until(future, Deadline::INFINITE)
    }

    /// Drives an `Unpin` future until the absolute monotonic `deadline`.
    pub fn run_until<F: Future + Unpin>(
        &self,
        future: &mut F,
        deadline: Deadline,
    ) -> Result<F::Output, RunError> {
        self.run_pinned_until(Pin::new(future), deadline)
    }

    /// Drives a pinned future with no deadline.
    pub fn run_pinned<F: Future + ?Sized>(
        &self,
        future: Pin<&mut F>,
    ) -> Result<F::Output, RunError> {
        self.run_pinned_until(future, Deadline::INFINITE)
    }

    /// Drives a pinned future until the absolute monotonic `deadline`.
    pub fn run_pinned_until<F: Future + ?Sized>(
        &self,
        future: Pin<&mut F>,
        deadline: Deadline,
    ) -> Result<F::Output, RunError> {
        if self.running.replace(true) {
            return Err(RunError::AlreadyRunning);
        }
        let _running = RunningGuard(&self.running);
        self.run_with_wait(future, deadline, ipc::wait_many)
    }

    pub fn send<'reactor, 'handle, 'bytes>(
        &'reactor self,
        endpoint: BorrowedHandle<'handle, Endpoint>,
        bytes: &'bytes [u8],
    ) -> Send<'reactor, 'handle, 'bytes, N> {
        Send {
            reactor: self,
            endpoint,
            bytes,
            complete: false,
        }
    }

    pub fn receive<'reactor, 'handle, 'buffer>(
        &'reactor self,
        endpoint: BorrowedHandle<'handle, Endpoint>,
        buffer: &'buffer mut [u8],
    ) -> Receive<'reactor, 'handle, 'buffer, N> {
        Receive {
            reactor: self,
            endpoint,
            buffer,
            complete: false,
        }
    }

    pub fn send_move<'reactor, 'handle, 'bytes, T: ObjectType>(
        &'reactor self,
        endpoint: BorrowedHandle<'handle, Endpoint>,
        bytes: &'bytes [u8],
        handle: OwnedHandle<T>,
        rights: Rights,
    ) -> SendMove<'reactor, 'handle, 'bytes, T, N> {
        SendMove {
            reactor: self,
            endpoint,
            bytes,
            handle: Some(handle),
            rights,
        }
    }

    fn register(&self, item: WaitItem, waker: &Waker) -> ipc::Result<()> {
        if item.handle() == 0 || item.requested() == Signals::EMPTY {
            return Err(ipc::Error::INVALID_ARGUMENT);
        }
        let mut registrations = self.registrations.borrow_mut();
        for registration in registrations.iter_mut().flatten() {
            if registration.item.handle() == item.handle() && registration.waker.will_wake(waker) {
                registration.item = WaitItem::new(
                    item.handle(),
                    registration.item.requested() | item.requested(),
                );
                return Ok(());
            }
        }
        let occupied = registrations.iter().filter(|entry| entry.is_some()).count();
        if occupied >= crate::abi::limits::MAX_OBJECT_WAIT_ITEMS {
            return Err(ipc::Error::NO_SPACE);
        }
        let Some(slot) = registrations.iter_mut().find(|entry| entry.is_none()) else {
            return Err(ipc::Error::NO_SPACE);
        };
        *slot = Some(Registration {
            item,
            waker: waker.clone(),
        });
        Ok(())
    }

    fn clear_registrations(&self) {
        for registration in self.registrations.borrow_mut().iter_mut() {
            *registration = None;
        }
    }

    fn snapshot(&self, output: &mut [WaitItem; N]) -> usize {
        let registrations = self.registrations.borrow();
        let mut count = 0;
        for registration in registrations.iter().flatten() {
            output[count] = registration.item;
            count += 1;
        }
        count
    }

    fn wake_handle(&self, handle: CapabilityHandle) {
        for registration in self.registrations.borrow().iter().flatten() {
            if registration.item.handle() == handle {
                registration.waker.wake_by_ref();
            }
        }
    }

    fn run_with_wait<F, W>(
        &self,
        mut future: Pin<&mut F>,
        deadline: Deadline,
        mut wait: W,
    ) -> Result<F::Output, RunError>
    where
        F: Future + ?Sized,
        W: FnMut(&[WaitItem], Deadline) -> ipc::Result<usize>,
    {
        let waker = noop_waker();
        let mut context = Context::from_waker(&waker);
        loop {
            self.clear_registrations();
            if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return Ok(output);
            }

            let mut items = [WaitItem::new(0, Signals::EMPTY); N];
            let count = self.snapshot(&mut items);
            if count == 0 {
                return Err(RunError::UnregisteredPending);
            }
            let ready = wait(&items[..count], deadline).map_err(RunError::Wait)?;
            let Some(item) = items.get(ready) else {
                return Err(RunError::Wait(ipc::Error::IO));
            };
            self.wake_handle(item.handle());
        }
    }
}

impl<const N: usize> Default for Reactor<N> {
    fn default() -> Self {
        Self::new()
    }
}

struct RunningGuard<'a>(&'a Cell<bool>);

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// A readiness-driven endpoint byte send.
pub struct Send<'reactor, 'handle, 'bytes, const N: usize> {
    reactor: &'reactor Reactor<N>,
    endpoint: BorrowedHandle<'handle, Endpoint>,
    bytes: &'bytes [u8],
    complete: bool,
}

impl<const N: usize> Future for Send<'_, '_, '_, N> {
    type Output = ipc::Result<()>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(
            !self.complete,
            "async endpoint send polled after completion"
        );
        match self.endpoint.send(self.bytes) {
            Ok(()) => {
                self.complete = true;
                Poll::Ready(Ok(()))
            }
            Err(error) if error == ipc::Error::TRY_AGAIN => {
                match self.reactor.register(
                    self.endpoint
                        .wait_item(Signals::WRITABLE | Signals::PEER_CLOSED),
                    context.waker(),
                ) {
                    Ok(()) => Poll::Pending,
                    Err(error) => {
                        self.complete = true;
                        Poll::Ready(Err(error))
                    }
                }
            }
            Err(error) => {
                self.complete = true;
                Poll::Ready(Err(error))
            }
        }
    }
}

/// A readiness-driven typed endpoint receive.
pub struct Receive<'reactor, 'handle, 'buffer, const N: usize> {
    reactor: &'reactor Reactor<N>,
    endpoint: BorrowedHandle<'handle, Endpoint>,
    buffer: &'buffer mut [u8],
    complete: bool,
}

impl<const N: usize> Future for Receive<'_, '_, '_, N> {
    type Output = ipc::Result<ReceivedMessage>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(
            !self.complete,
            "async endpoint receive polled after completion"
        );
        let endpoint = self.endpoint;
        match endpoint.try_receive(self.buffer) {
            Ok(message) => {
                self.complete = true;
                Poll::Ready(Ok(message))
            }
            Err(error) if error == ipc::Error::TRY_AGAIN => {
                match self.reactor.register(
                    endpoint.wait_item(Signals::READABLE | Signals::PEER_CLOSED),
                    context.waker(),
                ) {
                    Ok(()) => Poll::Pending,
                    Err(error) => {
                        self.complete = true;
                        Poll::Ready(Err(error))
                    }
                }
            }
            Err(error) => {
                self.complete = true;
                Poll::Ready(Err(error))
            }
        }
    }
}

/// A readiness-driven ownership-consuming endpoint move send.
pub struct SendMove<'reactor, 'handle, 'bytes, T: ObjectType, const N: usize> {
    reactor: &'reactor Reactor<N>,
    endpoint: BorrowedHandle<'handle, Endpoint>,
    bytes: &'bytes [u8],
    handle: Option<OwnedHandle<T>>,
    rights: Rights,
}

impl<T: ObjectType, const N: usize> Unpin for SendMove<'_, '_, '_, T, N> {}

impl<T: ObjectType, const N: usize> Future for SendMove<'_, '_, '_, T, N> {
    type Output = Result<(), SendMoveError<T>>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().get_mut();
        let handle = this
            .handle
            .take()
            .expect("async endpoint move send polled after completion");
        match this.endpoint.send_move(this.bytes, handle, this.rights) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(error) if error.error() == ipc::Error::TRY_AGAIN => {
                let handle = error.into_handle();
                match this.reactor.register(
                    this.endpoint
                        .wait_item(Signals::WRITABLE | Signals::PEER_CLOSED),
                    context.waker(),
                ) {
                    Ok(()) => {
                        this.handle = Some(handle);
                        Poll::Pending
                    }
                    Err(error) => Poll::Ready(Err(SendMoveError::new(error, handle))),
                }
            }
            Err(error) => Poll::Ready(Err(error)),
        }
    }
}

fn noop_waker() -> Waker {
    // SAFETY: the vtable never dereferences or frees the null data pointer.
    unsafe { Waker::from_raw(noop_raw_waker()) }
}

fn noop_raw_waker() -> RawWaker {
    RawWaker::new(core::ptr::null(), &NOOP_WAKER_VTABLE)
}

unsafe fn noop_waker_clone(_: *const ()) -> RawWaker {
    noop_raw_waker()
}

unsafe fn noop_waker_wake(_: *const ()) {}

unsafe fn noop_waker_wake_by_ref(_: *const ()) {}

unsafe fn noop_waker_drop(_: *const ()) {}

static NOOP_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
    noop_waker_clone,
    noop_waker_wake,
    noop_waker_wake_by_ref,
    noop_waker_drop,
);

#[cfg(test)]
mod tests {
    use core::{
        future,
        pin::Pin,
        task::{Poll, RawWaker, RawWakerVTable, Waker},
    };

    use super::{Reactor, RunError};
    use crate::ipc::{self, Deadline, Signals, WaitItem};

    #[test]
    fn registrations_merge_only_for_the_same_waker_and_enforce_capacity() {
        let reactor = Reactor::<3>::new();
        let waker = tagged_waker(1);
        let other_waker = tagged_waker(2);
        reactor
            .register(WaitItem::new(10, Signals::READABLE), &waker)
            .unwrap();
        reactor
            .register(WaitItem::new(10, Signals::WRITABLE), &waker)
            .unwrap();
        reactor
            .register(WaitItem::new(10, Signals::SIGNALED), &other_waker)
            .unwrap();
        reactor
            .register(WaitItem::new(11, Signals::SIGNALED), &waker)
            .unwrap();
        assert_eq!(
            reactor.register(WaitItem::new(12, Signals::READABLE), &waker),
            Err(ipc::Error::NO_SPACE)
        );

        let mut items = [WaitItem::new(0, Signals::EMPTY); 3];
        assert_eq!(reactor.snapshot(&mut items), 3);
        assert_eq!(items[0].handle(), 10);
        assert_eq!(items[0].requested(), Signals::READABLE | Signals::WRITABLE);
        assert_eq!(items[1], WaitItem::new(10, Signals::SIGNALED));
        assert_eq!(items[2], WaitItem::new(11, Signals::SIGNALED));
    }

    #[test]
    fn runner_waits_for_registered_pending_future() {
        struct PendingOnce<'a> {
            reactor: &'a Reactor<1>,
            pending: bool,
        }

        impl core::future::Future for PendingOnce<'_> {
            type Output = u64;

            fn poll(
                mut self: Pin<&mut Self>,
                context: &mut core::task::Context<'_>,
            ) -> Poll<Self::Output> {
                if self.pending {
                    self.pending = false;
                    self.reactor
                        .register(WaitItem::new(42, Signals::READABLE), context.waker())
                        .unwrap();
                    Poll::Pending
                } else {
                    Poll::Ready(7)
                }
            }
        }

        let reactor = Reactor::<1>::new();
        let mut future = PendingOnce {
            reactor: &reactor,
            pending: true,
        };
        let mut waits = 0;
        let output = reactor
            .run_with_wait(
                Pin::new(&mut future),
                Deadline::from_monotonic_ns(99),
                |items, deadline| {
                    waits += 1;
                    assert_eq!(items, &[WaitItem::new(42, Signals::READABLE)]);
                    assert_eq!(deadline, Deadline::from_monotonic_ns(99));
                    Ok(0)
                },
            )
            .unwrap();
        assert_eq!(output, 7);
        assert_eq!(waits, 1);
    }

    #[test]
    fn runner_rejects_unregistered_pending_future() {
        let reactor = Reactor::<1>::new();
        let mut future = future::pending::<()>();
        assert_eq!(
            reactor.run_with_wait(
                Pin::new(&mut future),
                Deadline::INFINITE,
                |_, _| unreachable!(),
            ),
            Err(RunError::UnregisteredPending)
        );
    }

    fn tagged_waker(tag: usize) -> Waker {
        // SAFETY: the test vtable preserves but never dereferences the tag.
        unsafe { Waker::from_raw(RawWaker::new(tag as *const (), &TAGGED_WAKER_VTABLE)) }
    }

    unsafe fn tagged_waker_clone(data: *const ()) -> RawWaker {
        RawWaker::new(data, &TAGGED_WAKER_VTABLE)
    }

    unsafe fn tagged_waker_noop(_: *const ()) {}

    static TAGGED_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
        tagged_waker_clone,
        tagged_waker_noop,
        tagged_waker_noop,
        tagged_waker_noop,
    );
}
