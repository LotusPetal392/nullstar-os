//! Allocation-free asynchronous endpoint operations over bounded object waits.
//!
//! [`Reactor`] drives scoped futures. Endpoint futures attempt their
//! non-blocking operation first, register level-triggered readiness on
//! [`crate::ipc::Error::TRY_AGAIN`], and let the runner sleep in the kernel's
//! bounded `wait_many` syscall. [`RunScope`] propagates one absolute deadline
//! and optional [`CancellationToken`] through every wait. [`PeriodicTimer`]
//! builds explicit coalescing periodic behavior over the kernel's one-shot
//! timer primitive. [`TaskExecutor`] drives independently ready tasks through a
//! queued event port, while [`TaskGroup`] supplies group cancellation and
//! deadline inheritance without heap allocation.

use core::{
    array,
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

use crate::{
    handle::{
        BorrowedHandle, Endpoint, Event, EventPort, MoveHandle, ObjectType, OwnedHandle,
        ReceivedMessage, ReceivedMessageMany, SendMoveError, SendMoveManyError, Timer,
    },
    ipc::{self, CapabilityHandle, Deadline, EventPortEvent, Rights, Signals, WaitItem},
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
    /// The run scope's cancellation token was asserted.
    Cancelled,
}

/// One absolute deadline and optional one-way cancellation authority shared by
/// a scoped future tree.
#[derive(Debug, Clone, Copy)]
pub struct RunScope<'token> {
    deadline: Deadline,
    cancellation: Option<&'token CancellationToken>,
}

impl<'token> RunScope<'token> {
    pub const fn new(deadline: Deadline) -> Self {
        Self {
            deadline,
            cancellation: None,
        }
    }

    pub const fn with_cancellation(
        deadline: Deadline,
        cancellation: &'token CancellationToken,
    ) -> Self {
        RunScope {
            deadline,
            cancellation: Some(cancellation),
        }
    }

    pub const fn deadline(self) -> Deadline {
        self.deadline
    }

    pub const fn cancellation(self) -> Option<&'token CancellationToken> {
        self.cancellation
    }

    /// Creates a nested scope without allowing its deadline to extend the
    /// parent's absolute deadline.
    pub const fn child(self, deadline: Deadline) -> Self {
        let deadline = if deadline.as_monotonic_ns() < self.deadline.as_monotonic_ns() {
            deadline
        } else {
            self.deadline
        };
        Self {
            deadline,
            cancellation: self.cancellation,
        }
    }
}

/// Signal-only authority for one structured cancellation tree.
#[derive(Debug)]
pub struct CancellationSource {
    event: OwnedHandle<Event>,
}

/// Wait-only authority for observing one structured cancellation tree.
#[derive(Debug)]
pub struct CancellationToken {
    event: OwnedHandle<Event>,
}

impl CancellationSource {
    /// Creates separated signal-only and wait-only cancellation authorities.
    pub fn new() -> ipc::Result<(Self, CancellationToken)> {
        let mut source = OwnedHandle::<Event>::create()?;
        let token = source.duplicate(CancellationToken::rights())?;
        source.replace_rights(Rights::SIGNAL)?;
        Ok((Self { event: source }, CancellationToken { event: token }))
    }

    /// Permanently cancels the associated token. Repeated calls are harmless.
    pub fn cancel(&self) -> ipc::Result<()> {
        self.event.set()
    }
}

impl CancellationToken {
    /// Observation, fan-out, and transfer authority without mutation rights.
    pub fn rights() -> Rights {
        Rights::DUPLICATE | Rights::TRANSFER | Rights::WAIT
    }

    pub fn try_clone(&self) -> ipc::Result<Self> {
        Ok(Self {
            event: self.event.duplicate(Self::rights())?,
        })
    }

    pub fn is_cancelled(&self) -> ipc::Result<bool> {
        Ok(self.event.signal_state()?.contains(Signals::SIGNALED))
    }

    pub fn into_handle(self) -> OwnedHandle<Event> {
        self.event
    }

    pub fn from_handle(mut event: OwnedHandle<Event>) -> ipc::Result<Self> {
        let rights = event.info()?.rights;
        let token_rights = Self::rights();
        if !rights.contains(token_rights) {
            return Err(ipc::Error::PERMISSION);
        }
        if rights != token_rights {
            event.replace_rights(token_rights)?;
        }
        Ok(Self { event })
    }

    fn wait_item(&self) -> WaitItem {
        self.event.borrow().wait_item(Signals::SIGNALED)
    }
}

/// A bounded lifecycle owner for independently scheduled tasks.
///
/// Every task spawned through [`TaskExecutor`] inherits this group's absolute
/// deadline and one-way cancellation token. A task may further shorten its
/// deadline, but it cannot extend the group deadline.
#[derive(Debug)]
pub struct TaskGroup {
    cancellation: CancellationSource,
    token: CancellationToken,
    deadline: Deadline,
}

impl TaskGroup {
    pub fn new(deadline: Deadline) -> ipc::Result<Self> {
        let (cancellation, token) = CancellationSource::new()?;
        Ok(Self {
            cancellation,
            token,
            deadline,
        })
    }

    pub const fn deadline(&self) -> Deadline {
        self.deadline
    }

    pub fn is_cancelled(&self) -> ipc::Result<bool> {
        self.token.is_cancelled()
    }

    /// Permanently cancels every unfinished task in this group.
    pub fn cancel(&self) -> ipc::Result<()> {
        self.cancellation.cancel()
    }

    pub fn scope(&self) -> RunScope<'_> {
        RunScope::with_cancellation(self.deadline, &self.token)
    }

    /// Creates a task scope whose deadline cannot extend the group deadline.
    pub fn task_scope(&self, deadline: Deadline) -> RunScope<'_> {
        self.scope().child(deadline)
    }
}

/// One coalesced periodic expiration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeriodicTick {
    /// The absolute deadline assigned to the earliest represented expiration.
    pub scheduled: Deadline,
    /// The monotonic time at which the fired timer was observed.
    pub observed_ns: u64,
    /// Number of elapsed periods represented by this tick.
    pub expirations: u64,
}

/// Pure periodic deadline state used by [`PeriodicTimer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeriodicSchedule {
    period_ns: u64,
    next_deadline: Deadline,
}

impl PeriodicSchedule {
    pub fn new(first_deadline: Deadline, period_ns: u64) -> ipc::Result<Self> {
        if period_ns == 0 || first_deadline == Deadline::INFINITE {
            return Err(ipc::Error::INVALID_ARGUMENT);
        }
        Ok(Self {
            period_ns,
            next_deadline: first_deadline,
        })
    }

    pub const fn period_ns(self) -> u64 {
        self.period_ns
    }

    pub const fn next_deadline(self) -> Deadline {
        self.next_deadline
    }

    /// Coalesces every elapsed period into one bounded tick and advances to the
    /// first future deadline. The schedule is unchanged on overflow.
    pub fn advance(&mut self, observed_ns: u64) -> ipc::Result<PeriodicTick> {
        let scheduled_ns = self.next_deadline.as_monotonic_ns();
        let elapsed = observed_ns.saturating_sub(scheduled_ns);
        let expirations = elapsed / self.period_ns + 1;
        let advance = self
            .period_ns
            .checked_mul(expirations)
            .ok_or(ipc::Error::RANGE)?;
        let next_ns = scheduled_ns
            .checked_add(advance)
            .filter(|deadline| *deadline != Deadline::INFINITE.as_monotonic_ns())
            .ok_or(ipc::Error::RANGE)?;
        let tick = PeriodicTick {
            scheduled: self.next_deadline,
            observed_ns,
            expirations,
        };
        self.next_deadline = Deadline::from_monotonic_ns(next_ns);
        Ok(tick)
    }
}

/// A bounded periodic timer built by rearming one kernel one-shot timer.
#[derive(Debug)]
pub struct PeriodicTimer {
    timer: OwnedHandle<Timer>,
    schedule: PeriodicSchedule,
}

impl PeriodicTimer {
    pub fn start_at(first_deadline: Deadline, period_ns: u64) -> ipc::Result<Self> {
        let schedule = PeriodicSchedule::new(first_deadline, period_ns)?;
        let timer = OwnedHandle::<Timer>::create()?;
        timer.arm(first_deadline)?;
        Ok(Self { timer, schedule })
    }

    pub fn start_after(period_ns: u64) -> ipc::Result<Self> {
        if period_ns == 0 {
            return Err(ipc::Error::INVALID_ARGUMENT);
        }
        let now = crate::platform::monotonic_time_ns().map_err(|_| ipc::Error::IO)?;
        let first = now.checked_add(period_ns).ok_or(ipc::Error::RANGE)?;
        if first == Deadline::INFINITE.as_monotonic_ns() {
            return Err(ipc::Error::RANGE);
        }
        Self::start_at(Deadline::from_monotonic_ns(first), period_ns)
    }

    pub const fn period_ns(&self) -> u64 {
        self.schedule.period_ns()
    }

    pub const fn next_deadline(&self) -> Deadline {
        self.schedule.next_deadline()
    }

    pub fn cancel(&self) -> ipc::Result<()> {
        self.timer.cancel()
    }
}

struct Registration {
    item: WaitItem,
    waker: Waker,
}

/// A bounded, allocation-free reactor for one scoped future tree.
///
/// `N` bounds total wait registrations in one poll cycle. A cancellation-aware
/// [`RunScope`] reserves one of those slots. `N` may not exceed the kernel's
/// `MAX_OBJECT_WAIT_ITEMS`; excess registrations fail with
/// [`ipc::Error::NO_SPACE`] through the operation future or [`RunError::Wait`].
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
        self.run_scoped(future, RunScope::new(deadline))
    }

    /// Drives an `Unpin` future under one propagated deadline and optional
    /// cancellation token.
    pub fn run_scoped<F: Future + Unpin>(
        &self,
        future: &mut F,
        scope: RunScope<'_>,
    ) -> Result<F::Output, RunError> {
        self.run_pinned_scoped(Pin::new(future), scope)
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
        self.run_pinned_scoped(future, RunScope::new(deadline))
    }

    /// Drives a pinned future under one propagated deadline and optional
    /// cancellation token.
    pub fn run_pinned_scoped<F: Future + ?Sized>(
        &self,
        future: Pin<&mut F>,
        scope: RunScope<'_>,
    ) -> Result<F::Output, RunError> {
        if self.running.replace(true) {
            return Err(RunError::AlreadyRunning);
        }
        let _running = RunningGuard(&self.running);
        self.run_with_wait(future, scope, ipc::wait_many)
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

    pub fn send_move_many<'reactor, 'handle, 'bytes, const M: usize>(
        &'reactor self,
        endpoint: BorrowedHandle<'handle, Endpoint>,
        bytes: &'bytes [u8],
        handles: [MoveHandle; M],
    ) -> SendMoveMany<'reactor, 'handle, 'bytes, M, N> {
        SendMoveMany {
            reactor: self,
            endpoint,
            bytes,
            handles: Some(handles),
        }
    }

    pub fn receive_many<'reactor, 'handle, 'buffer, const M: usize>(
        &'reactor self,
        endpoint: BorrowedHandle<'handle, Endpoint>,
        buffer: &'buffer mut [u8],
    ) -> ReceiveMany<'reactor, 'handle, 'buffer, M, N> {
        ReceiveMany {
            reactor: self,
            endpoint,
            buffer,
            complete: false,
        }
    }

    /// A future that completes when `token` is cancelled.
    pub fn cancelled<'reactor, 'token>(
        &'reactor self,
        token: &'token CancellationToken,
    ) -> Cancelled<'reactor, 'token, N> {
        Cancelled {
            reactor: self,
            token,
            complete: false,
        }
    }

    /// A future for the next coalesced expiration of `timer`.
    pub fn next_tick<'reactor, 'timer>(
        &'reactor self,
        timer: &'timer mut PeriodicTimer,
    ) -> NextTick<'reactor, 'timer, N> {
        NextTick {
            reactor: self,
            timer,
            complete: false,
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

    fn clear_registrations_for(&self, waker: &Waker) {
        for registration in self.registrations.borrow_mut().iter_mut() {
            if registration
                .as_ref()
                .is_some_and(|registration| registration.waker.will_wake(waker))
            {
                *registration = None;
            }
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

    fn snapshot_for(&self, waker: &Waker, output: &mut [WaitItem; N]) -> usize {
        let registrations = self.registrations.borrow();
        let mut count = 0;
        for registration in registrations.iter().flatten() {
            if registration.waker.will_wake(waker) {
                output[count] = registration.item;
                count += 1;
            }
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
        scope: RunScope<'_>,
        mut wait: W,
    ) -> Result<F::Output, RunError>
    where
        F: Future + ?Sized,
        W: FnMut(&[WaitItem], Deadline) -> ipc::Result<usize>,
    {
        let waker = noop_waker();
        let mut context = Context::from_waker(&waker);
        loop {
            if let Some(token) = scope.cancellation()
                && token.is_cancelled().map_err(RunError::Wait)?
            {
                return Err(RunError::Cancelled);
            }
            self.clear_registrations();
            if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return Ok(output);
            }

            let mut items = [WaitItem::new(0, Signals::EMPTY); N];
            let mut count = self.snapshot(&mut items);
            let cancellation_handle = if let Some(token) = scope.cancellation() {
                if count >= N || count >= crate::abi::limits::MAX_OBJECT_WAIT_ITEMS {
                    return Err(RunError::Wait(ipc::Error::NO_SPACE));
                }
                let item = token.wait_item();
                items[count] = item;
                count += 1;
                Some(item.handle())
            } else {
                None
            };
            if count == 0 {
                return Err(RunError::UnregisteredPending);
            }
            let ready = wait(&items[..count], scope.deadline()).map_err(RunError::Wait)?;
            let Some(item) = items.get(ready) else {
                return Err(RunError::Wait(ipc::Error::IO));
            };
            if cancellation_handle == Some(item.handle()) {
                return Err(RunError::Cancelled);
            }
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

/// Stable identity for one bounded executor task slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskId {
    slot: usize,
    generation: u32,
}

impl TaskId {
    pub const fn slot(self) -> usize {
        self.slot
    }

    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Terminal state retained for a task after the executor finishes driving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOutcome {
    /// The future completed, retaining its application-level result.
    Completed(ipc::Result<()>),
    /// The owning task group was cancelled before completion.
    Cancelled,
    /// The task's inherited absolute deadline expired before completion.
    TimedOut,
}

/// Failure in the task executor itself rather than in an individual task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskRunError {
    /// An event-port, cancellation-state, or monotonic-clock operation failed.
    Runtime(ipc::Error),
    /// A task returned `Pending` without registering a waitable object.
    UnregisteredPending(TaskId),
    /// The same executor was entered recursively.
    AlreadyRunning,
}

struct TaskSlot<'task, 'group> {
    id: TaskId,
    future: Pin<&'task mut (dyn Future<Output = ipc::Result<()>> + 'task)>,
    scope: RunScope<'group>,
    outcome: Option<TaskOutcome>,
    ready: bool,
}

#[derive(Debug, Clone, Copy)]
struct BoundRegistration {
    key: u64,
    task: TaskId,
}

/// Allocation-free cooperative executor over one queued kernel event port.
///
/// `TASKS` bounds independently scheduled futures and `WAITS` bounds their
/// combined reactor registrations. Every spawned task belongs to a
/// [`TaskGroup`]. Registration keys include task-slot generations so a stale
/// event can never wake a later occupant of the same slot.
pub struct TaskExecutor<'reactor, 'task, 'group, const TASKS: usize, const WAITS: usize> {
    reactor: &'reactor Reactor<WAITS>,
    event_port: Option<OwnedHandle<EventPort>>,
    tasks: [Option<TaskSlot<'task, 'group>>; TASKS],
    generations: [u32; TASKS],
    bindings: [Option<BoundRegistration>; crate::abi::limits::MAX_EVENT_PORT_REGISTRATIONS],
    running: bool,
}

impl<'reactor, 'task, 'group, const TASKS: usize, const WAITS: usize>
    TaskExecutor<'reactor, 'task, 'group, TASKS, WAITS>
{
    pub fn new(reactor: &'reactor Reactor<WAITS>) -> ipc::Result<Self> {
        if TASKS == 0
            || WAITS == 0
            || WAITS > crate::abi::limits::MAX_OBJECT_WAIT_ITEMS
            || TASKS
                .checked_add(WAITS)
                .is_none_or(|total| total > crate::abi::limits::MAX_EVENT_PORT_REGISTRATIONS)
        {
            return Err(ipc::Error::INVALID_ARGUMENT);
        }
        Ok(Self {
            reactor,
            event_port: Some(OwnedHandle::<EventPort>::create()?),
            tasks: array::from_fn(|_| None),
            generations: [0; TASKS],
            bindings: array::from_fn(|_| None),
            running: false,
        })
    }

    /// Adds an `Unpin` task to `group` with an optional shorter deadline.
    pub fn spawn<F>(
        &mut self,
        future: &'task mut F,
        group: &'group TaskGroup,
        deadline: Deadline,
    ) -> ipc::Result<TaskId>
    where
        F: Future<Output = ipc::Result<()>> + Unpin + 'task,
    {
        self.spawn_pinned(Pin::new(future), group, deadline)
    }

    /// Adds a pinned task to `group` with an optional shorter deadline.
    pub fn spawn_pinned<F>(
        &mut self,
        future: Pin<&'task mut F>,
        group: &'group TaskGroup,
        deadline: Deadline,
    ) -> ipc::Result<TaskId>
    where
        F: Future<Output = ipc::Result<()>> + 'task,
    {
        self.spawn_scoped(future, group.task_scope(deadline))
    }

    fn spawn_scoped<F>(
        &mut self,
        future: Pin<&'task mut F>,
        scope: RunScope<'group>,
    ) -> ipc::Result<TaskId>
    where
        F: Future<Output = ipc::Result<()>> + 'task,
    {
        let Some(slot) = self.tasks.iter().position(Option::is_none) else {
            return Err(ipc::Error::NO_SPACE);
        };
        let generation = self.generations[slot]
            .checked_add(1)
            .ok_or(ipc::Error::RANGE)?;
        self.generations[slot] = generation;
        let id = TaskId { slot, generation };
        self.tasks[slot] = Some(TaskSlot {
            id,
            future,
            scope,
            outcome: None,
            ready: true,
        });
        Ok(id)
    }

    pub fn outcome(&self, id: TaskId) -> Option<TaskOutcome> {
        self.tasks
            .get(id.slot)
            .and_then(Option::as_ref)
            .filter(|task| task.id == id)
            .and_then(|task| task.outcome)
    }

    /// Releases a completed slot and returns its terminal outcome.
    ///
    /// Reusing the slot advances its generation, so already-drained events
    /// cannot be confused with registrations owned by the replacement task.
    pub fn reap(&mut self, id: TaskId) -> Option<TaskOutcome> {
        if self.running {
            return None;
        }
        let task = self.tasks.get(id.slot)?.as_ref()?;
        if task.id != id {
            return None;
        }
        let outcome = task.outcome?;
        self.tasks[id.slot] = None;
        Some(outcome)
    }

    /// Drives every task to completion, cancellation, or deadline expiry.
    pub fn run(&mut self) -> Result<(), TaskRunError> {
        let Some(event_port) = self.event_port.as_ref() else {
            return Err(TaskRunError::Runtime(ipc::Error::IO));
        };
        let raw = event_port.as_raw();
        self.run_with(
            |item, key| ipc::event_port_add(raw, item.handle(), item.requested(), key),
            |key| ipc::event_port_remove(raw, key),
            |deadline| ipc::event_port_wait(raw, deadline),
            || crate::platform::monotonic_time_ns().map_err(|_| ipc::Error::IO),
        )
    }

    fn run_with<A, R, W, C>(
        &mut self,
        mut add: A,
        mut remove: R,
        mut wait: W,
        mut clock: C,
    ) -> Result<(), TaskRunError>
    where
        A: FnMut(WaitItem, u64) -> ipc::Result<()>,
        R: FnMut(u64) -> ipc::Result<()>,
        W: FnMut(Deadline) -> ipc::Result<EventPortEvent>,
        C: FnMut() -> ipc::Result<u64>,
    {
        if self.running {
            return Err(TaskRunError::AlreadyRunning);
        }
        self.running = true;
        let result = self.drive(&mut add, &mut remove, &mut wait, &mut clock);
        let cleanup = self.remove_bindings(&mut remove);
        self.reset_unfinished_tasks();
        self.running = false;
        result.and(cleanup)
    }

    fn reset_unfinished_tasks(&mut self) {
        for task in self.tasks.iter_mut().flatten() {
            let waker = task_waker(task.id);
            self.reactor.clear_registrations_for(&waker);
            if task.outcome.is_none() {
                task.ready = true;
            }
        }
    }

    fn drive<A, R, W, C>(
        &mut self,
        add: &mut A,
        remove: &mut R,
        wait: &mut W,
        clock: &mut C,
    ) -> Result<(), TaskRunError>
    where
        A: FnMut(WaitItem, u64) -> ipc::Result<()>,
        R: FnMut(u64) -> ipc::Result<()>,
        W: FnMut(Deadline) -> ipc::Result<EventPortEvent>,
        C: FnMut() -> ipc::Result<u64>,
    {
        loop {
            self.poll_ready_tasks()?;
            if self
                .tasks
                .iter()
                .flatten()
                .all(|task| task.outcome.is_some())
            {
                return Ok(());
            }
            self.sync_bindings(add, remove)?;
            let deadline = self.earliest_deadline();
            match wait(deadline) {
                Ok(event) => {
                    self.mark_ready(event);
                    loop {
                        match wait(Deadline::IMMEDIATE) {
                            Ok(event) => self.mark_ready(event),
                            Err(error) if error == ipc::Error::TIMED_OUT => break,
                            Err(error) => return Err(TaskRunError::Runtime(error)),
                        }
                    }
                }
                Err(error) if error == ipc::Error::TIMED_OUT => {
                    let now = clock().map_err(TaskRunError::Runtime)?;
                    if !self.expire_deadlines(now) {
                        return Err(TaskRunError::Runtime(ipc::Error::TIMED_OUT));
                    }
                }
                Err(error) => return Err(TaskRunError::Runtime(error)),
            }
        }
    }

    fn poll_ready_tasks(&mut self) -> Result<(), TaskRunError> {
        for slot in 0..TASKS {
            let Some(task) = self.tasks[slot].as_mut() else {
                continue;
            };
            if task.outcome.is_some() || !task.ready {
                continue;
            }
            task.ready = false;
            let waker = task_waker(task.id);
            if let Some(token) = task.scope.cancellation()
                && token.is_cancelled().map_err(TaskRunError::Runtime)?
            {
                self.reactor.clear_registrations_for(&waker);
                task.outcome = Some(TaskOutcome::Cancelled);
                continue;
            }
            self.reactor.clear_registrations_for(&waker);
            let mut context = Context::from_waker(&waker);
            match task.future.as_mut().poll(&mut context) {
                Poll::Ready(result) => task.outcome = Some(TaskOutcome::Completed(result)),
                Poll::Pending => {
                    let mut registrations = [WaitItem::new(0, Signals::EMPTY); WAITS];
                    if self.reactor.snapshot_for(&waker, &mut registrations) == 0 {
                        return Err(TaskRunError::UnregisteredPending(task.id));
                    }
                }
            }
        }
        Ok(())
    }

    fn sync_bindings<A, R>(&mut self, add: &mut A, remove: &mut R) -> Result<(), TaskRunError>
    where
        A: FnMut(WaitItem, u64) -> ipc::Result<()>,
        R: FnMut(u64) -> ipc::Result<()>,
    {
        self.remove_bindings(remove)?;
        let mut bound = 0;
        for slot in 0..TASKS {
            let Some(task) = self.tasks[slot].as_ref() else {
                continue;
            };
            if task.outcome.is_some() {
                continue;
            }
            let id = task.id;
            let scope = task.scope;
            let waker = task_waker(id);
            let mut registrations = [WaitItem::new(0, Signals::EMPTY); WAITS];
            let count = self.reactor.snapshot_for(&waker, &mut registrations);
            for (ordinal, item) in registrations[..count].iter().copied().enumerate() {
                let key = Self::registration_key(id, ordinal)?;
                add(item, key).map_err(TaskRunError::Runtime)?;
                self.bindings[bound] = Some(BoundRegistration { key, task: id });
                bound += 1;
            }
            if let Some(token) = scope.cancellation() {
                let key = Self::registration_key(id, WAITS)?;
                add(token.wait_item(), key).map_err(TaskRunError::Runtime)?;
                self.bindings[bound] = Some(BoundRegistration { key, task: id });
                bound += 1;
            }
        }
        Ok(())
    }

    fn remove_bindings<R>(&mut self, remove: &mut R) -> Result<(), TaskRunError>
    where
        R: FnMut(u64) -> ipc::Result<()>,
    {
        for binding in &mut self.bindings {
            if let Some(current) = *binding {
                remove(current.key).map_err(TaskRunError::Runtime)?;
                *binding = None;
            }
        }
        Ok(())
    }

    fn registration_key(id: TaskId, ordinal: usize) -> Result<u64, TaskRunError> {
        let stride =
            u64::try_from(WAITS + 1).map_err(|_| TaskRunError::Runtime(ipc::Error::RANGE))?;
        let task_index = u64::from(id.generation)
            .checked_mul(
                u64::try_from(TASKS).map_err(|_| TaskRunError::Runtime(ipc::Error::RANGE))?,
            )
            .and_then(|base| base.checked_add(id.slot as u64))
            .ok_or(TaskRunError::Runtime(ipc::Error::RANGE))?;
        task_index
            .checked_mul(stride)
            .and_then(|base| base.checked_add(ordinal as u64))
            .filter(|key| *key <= crate::abi::event_port::MAX_KEY)
            .ok_or(TaskRunError::Runtime(ipc::Error::RANGE))
    }

    fn mark_ready(&mut self, event: EventPortEvent) {
        let Some(binding) = self
            .bindings
            .iter()
            .flatten()
            .find(|binding| binding.key == event.key)
            .copied()
        else {
            return;
        };
        if let Some(task) = self
            .tasks
            .get_mut(binding.task.slot)
            .and_then(Option::as_mut)
            && task.id == binding.task
            && task.outcome.is_none()
        {
            task.ready = true;
        }
    }

    fn earliest_deadline(&self) -> Deadline {
        self.tasks
            .iter()
            .flatten()
            .filter(|task| task.outcome.is_none())
            .map(|task| task.scope.deadline())
            .min_by_key(|deadline| deadline.as_monotonic_ns())
            .unwrap_or(Deadline::INFINITE)
    }

    fn expire_deadlines(&mut self, now: u64) -> bool {
        let mut expired = false;
        for task in self.tasks.iter_mut().flatten() {
            if task.outcome.is_none()
                && task.scope.deadline() != Deadline::INFINITE
                && task.scope.deadline().as_monotonic_ns() <= now
            {
                let waker = task_waker(task.id);
                self.reactor.clear_registrations_for(&waker);
                task.outcome = Some(TaskOutcome::TimedOut);
                expired = true;
            }
        }
        expired
    }
}

fn task_waker(id: TaskId) -> Waker {
    let tag = ((id.generation as usize) << 8) | id.slot.saturating_add(1);
    // SAFETY: the executor's task waker vtable never dereferences the encoded tag.
    unsafe { Waker::from_raw(RawWaker::new(tag as *const (), &TASK_WAKER_VTABLE)) }
}

unsafe fn task_waker_clone(data: *const ()) -> RawWaker {
    RawWaker::new(data, &TASK_WAKER_VTABLE)
}

unsafe fn task_waker_noop(_: *const ()) {}

static TASK_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
    task_waker_clone,
    task_waker_noop,
    task_waker_noop,
    task_waker_noop,
);

/// A readiness-driven cancellation observation.
pub struct Cancelled<'reactor, 'token, const N: usize> {
    reactor: &'reactor Reactor<N>,
    token: &'token CancellationToken,
    complete: bool,
}

impl<const N: usize> Future for Cancelled<'_, '_, N> {
    type Output = ipc::Result<()>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(
            !self.complete,
            "cancellation future polled after completion"
        );
        match self.token.is_cancelled() {
            Ok(true) => {
                self.complete = true;
                Poll::Ready(Ok(()))
            }
            Ok(false) => match self
                .reactor
                .register(self.token.wait_item(), context.waker())
            {
                Ok(()) => Poll::Pending,
                Err(error) => {
                    self.complete = true;
                    Poll::Ready(Err(error))
                }
            },
            Err(error) => {
                self.complete = true;
                Poll::Ready(Err(error))
            }
        }
    }
}

/// A readiness-driven periodic timer expiration.
pub struct NextTick<'reactor, 'timer, const N: usize> {
    reactor: &'reactor Reactor<N>,
    timer: &'timer mut PeriodicTimer,
    complete: bool,
}

impl<const N: usize> Future for NextTick<'_, '_, N> {
    type Output = ipc::Result<PeriodicTick>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(!self.complete, "periodic tick polled after completion");
        match self.timer.timer.signal_state() {
            Ok(signals) if signals.contains(Signals::TIMER_FIRED) => {
                let observed = match crate::platform::monotonic_time_ns() {
                    Ok(observed) => observed,
                    Err(_) => {
                        self.complete = true;
                        return Poll::Ready(Err(ipc::Error::IO));
                    }
                };
                let mut schedule = self.timer.schedule;
                let tick = match schedule.advance(observed) {
                    Ok(tick) => tick,
                    Err(error) => {
                        self.complete = true;
                        return Poll::Ready(Err(error));
                    }
                };
                if let Err(error) = self.timer.timer.arm(schedule.next_deadline()) {
                    self.complete = true;
                    return Poll::Ready(Err(error));
                }
                self.timer.schedule = schedule;
                self.complete = true;
                Poll::Ready(Ok(tick))
            }
            Ok(_) => match self.reactor.register(
                self.timer.timer.borrow().wait_item(Signals::TIMER_FIRED),
                context.waker(),
            ) {
                Ok(()) => Poll::Pending,
                Err(error) => {
                    self.complete = true;
                    Poll::Ready(Err(error))
                }
            },
            Err(error) => {
                self.complete = true;
                Poll::Ready(Err(error))
            }
        }
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

/// A readiness-driven ownership-consuming multi-handle endpoint send.
pub struct SendMoveMany<'reactor, 'handle, 'bytes, const M: usize, const N: usize> {
    reactor: &'reactor Reactor<N>,
    endpoint: BorrowedHandle<'handle, Endpoint>,
    bytes: &'bytes [u8],
    handles: Option<[MoveHandle; M]>,
}

impl<const M: usize, const N: usize> Unpin for SendMoveMany<'_, '_, '_, M, N> {}

impl<const M: usize, const N: usize> Future for SendMoveMany<'_, '_, '_, M, N> {
    type Output = Result<(), SendMoveManyError<M>>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().get_mut();
        let handles = this
            .handles
            .take()
            .expect("async endpoint multi-handle send polled after completion");
        match this.endpoint.send_move_many(this.bytes, handles) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(error) if error.error() == ipc::Error::TRY_AGAIN => {
                let handles = error.into_handles();
                match this.reactor.register(
                    this.endpoint
                        .wait_item(Signals::WRITABLE | Signals::PEER_CLOSED),
                    context.waker(),
                ) {
                    Ok(()) => {
                        this.handles = Some(handles);
                        Poll::Pending
                    }
                    Err(error) => Poll::Ready(Err(SendMoveManyError::new(error, handles))),
                }
            }
            Err(error) => Poll::Ready(Err(error)),
        }
    }
}

/// A readiness-driven typed multi-handle endpoint receive.
pub struct ReceiveMany<'reactor, 'handle, 'buffer, const M: usize, const N: usize> {
    reactor: &'reactor Reactor<N>,
    endpoint: BorrowedHandle<'handle, Endpoint>,
    buffer: &'buffer mut [u8],
    complete: bool,
}

impl<const M: usize, const N: usize> Future for ReceiveMany<'_, '_, '_, M, N> {
    type Output = Result<ReceivedMessageMany<M>, ipc::ReceiveManyError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(
            !self.complete,
            "async endpoint multi-handle receive polled after completion"
        );
        let endpoint = self.endpoint;
        match endpoint.try_receive_many(self.buffer) {
            Ok(message) => {
                self.complete = true;
                Poll::Ready(Ok(message))
            }
            Err(error) if error.error() == ipc::Error::TRY_AGAIN => {
                match self.reactor.register(
                    endpoint.wait_item(Signals::READABLE | Signals::PEER_CLOSED),
                    context.waker(),
                ) {
                    Ok(()) => Poll::Pending,
                    Err(wait_error) => {
                        self.complete = true;
                        Poll::Ready(Err(ipc::ReceiveManyError::from_error(wait_error)))
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
        cell::{Cell, RefCell},
        future,
        pin::Pin,
        task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    };

    use super::{
        PeriodicSchedule, Reactor, RunError, RunScope, TaskExecutor, TaskOutcome, TaskRunError,
    };
    use crate::ipc::{self, Deadline, EventPortEvent, Signals, WaitItem};

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
                RunScope::new(Deadline::from_monotonic_ns(99)),
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
                RunScope::new(Deadline::INFINITE),
                |_, _| unreachable!(),
            ),
            Err(RunError::UnregisteredPending)
        );
    }

    #[test]
    fn child_scope_never_extends_parent_deadline() {
        let parent = RunScope::new(Deadline::from_monotonic_ns(100));
        assert_eq!(
            parent.child(Deadline::from_monotonic_ns(40)).deadline(),
            Deadline::from_monotonic_ns(40)
        );
        assert_eq!(
            parent.child(Deadline::from_monotonic_ns(140)).deadline(),
            Deadline::from_monotonic_ns(100)
        );
        assert_eq!(
            parent.child(Deadline::INFINITE).deadline(),
            Deadline::from_monotonic_ns(100)
        );
    }

    #[test]
    fn periodic_schedule_coalesces_missed_expirations() {
        let mut schedule = PeriodicSchedule::new(Deadline::from_monotonic_ns(100), 25).unwrap();
        let first = schedule.advance(100).unwrap();
        assert_eq!(first.scheduled, Deadline::from_monotonic_ns(100));
        assert_eq!(first.observed_ns, 100);
        assert_eq!(first.expirations, 1);
        assert_eq!(schedule.next_deadline(), Deadline::from_monotonic_ns(125));

        let coalesced = schedule.advance(181).unwrap();
        assert_eq!(coalesced.scheduled, Deadline::from_monotonic_ns(125));
        assert_eq!(coalesced.observed_ns, 181);
        assert_eq!(coalesced.expirations, 3);
        assert_eq!(schedule.next_deadline(), Deadline::from_monotonic_ns(200));
    }

    #[test]
    fn periodic_schedule_rejects_invalid_or_overflowing_deadlines() {
        assert_eq!(
            PeriodicSchedule::new(Deadline::IMMEDIATE, 0),
            Err(ipc::Error::INVALID_ARGUMENT)
        );
        assert_eq!(
            PeriodicSchedule::new(Deadline::INFINITE, 1),
            Err(ipc::Error::INVALID_ARGUMENT)
        );

        let mut schedule =
            PeriodicSchedule::new(Deadline::from_monotonic_ns(u64::MAX - 2), 2).unwrap();
        assert_eq!(schedule.advance(u64::MAX - 2), Err(ipc::Error::RANGE));
        assert_eq!(
            schedule.next_deadline(),
            Deadline::from_monotonic_ns(u64::MAX - 2)
        );
    }

    #[test]
    fn task_executor_polls_only_tasks_selected_by_event_keys() {
        struct PendingOnce<'a> {
            reactor: &'a Reactor<2>,
            handle: u64,
            polls: &'a Cell<u32>,
        }

        impl Future for PendingOnce<'_> {
            type Output = ipc::Result<()>;

            fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
                let this = self.get_mut();
                let polls = this.polls.get() + 1;
                this.polls.set(polls);
                if polls == 1 {
                    this.reactor
                        .register(
                            WaitItem::new(this.handle, Signals::READABLE),
                            context.waker(),
                        )
                        .unwrap();
                    Poll::Pending
                } else {
                    Poll::Ready(Ok(()))
                }
            }
        }

        #[derive(Default)]
        struct FakePort {
            bindings: [Option<(u64, u64)>; 2],
            waits: usize,
        }

        let reactor = Reactor::<2>::new();
        let first_polls = Cell::new(0);
        let second_polls = Cell::new(0);
        let mut first = PendingOnce {
            reactor: &reactor,
            handle: 10,
            polls: &first_polls,
        };
        let mut second = PendingOnce {
            reactor: &reactor,
            handle: 20,
            polls: &second_polls,
        };
        let mut executor = TaskExecutor::<2, 2> {
            reactor: &reactor,
            event_port: None,
            tasks: core::array::from_fn(|_| None),
            generations: [0; 2],
            bindings: core::array::from_fn(|_| None),
            running: false,
        };
        let first_id = executor
            .spawn_scoped(Pin::new(&mut first), RunScope::new(Deadline::INFINITE))
            .unwrap();
        let second_id = executor
            .spawn_scoped(Pin::new(&mut second), RunScope::new(Deadline::INFINITE))
            .unwrap();
        let port = RefCell::new(FakePort::default());

        executor
            .run_with(
                |item, key| {
                    let mut port = port.borrow_mut();
                    let binding = port
                        .bindings
                        .iter_mut()
                        .find(|binding| binding.is_none())
                        .unwrap();
                    *binding = Some((item.handle(), key));
                    Ok(())
                },
                |key| {
                    for binding in &mut port.borrow_mut().bindings {
                        if binding.is_some_and(|(_, binding_key)| binding_key == key) {
                            *binding = None;
                        }
                    }
                    Ok(())
                },
                |deadline| {
                    if deadline == Deadline::IMMEDIATE {
                        return Err(ipc::Error::TIMED_OUT);
                    }
                    let mut port = port.borrow_mut();
                    let handle = if port.waits == 0 { 20 } else { 10 };
                    if port.waits == 1 {
                        assert_eq!(first_polls.get(), 1);
                        assert_eq!(second_polls.get(), 2);
                    }
                    port.waits += 1;
                    let key = port
                        .bindings
                        .iter()
                        .flatten()
                        .find(|(bound_handle, _)| *bound_handle == handle)
                        .unwrap()
                        .1;
                    Ok(EventPortEvent {
                        key,
                        signals: Signals::READABLE,
                    })
                },
                || Ok(0),
            )
            .unwrap();

        assert_eq!(
            executor.outcome(first_id),
            Some(TaskOutcome::Completed(Ok(())))
        );
        assert_eq!(
            executor.outcome(second_id),
            Some(TaskOutcome::Completed(Ok(())))
        );
        assert_eq!(
            executor.reap(first_id),
            Some(TaskOutcome::Completed(Ok(())))
        );
        assert_eq!(executor.outcome(first_id), None);
        assert_eq!(first_polls.get(), 2);
        assert_eq!(second_polls.get(), 2);
    }

    #[test]
    fn task_registration_keys_change_when_a_slot_generation_advances() {
        let first = super::TaskId {
            slot: 0,
            generation: 1,
        };
        let replacement = super::TaskId {
            slot: 0,
            generation: 2,
        };
        assert_ne!(
            TaskExecutor::<2, 2>::registration_key(first, 0).unwrap(),
            TaskExecutor::<2, 2>::registration_key(replacement, 0).unwrap()
        );
    }

    #[test]
    fn task_executor_expires_pending_task_at_inherited_deadline() {
        struct Waiting<'a> {
            reactor: &'a Reactor<1>,
        }

        impl Future for Waiting<'_> {
            type Output = ipc::Result<()>;

            fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
                self.reactor
                    .register(WaitItem::new(30, Signals::READABLE), context.waker())
                    .unwrap();
                Poll::Pending
            }
        }

        let reactor = Reactor::<1>::new();
        let mut task = Waiting { reactor: &reactor };
        let mut executor = TaskExecutor::<1, 1> {
            reactor: &reactor,
            event_port: None,
            tasks: core::array::from_fn(|_| None),
            generations: [0; 1],
            bindings: core::array::from_fn(|_| None),
            running: false,
        };
        let id = executor
            .spawn_scoped(
                Pin::new(&mut task),
                RunScope::new(Deadline::from_monotonic_ns(50)),
            )
            .unwrap();
        let key = Cell::new(None);
        executor
            .run_with(
                |_, bound_key| {
                    key.set(Some(bound_key));
                    Ok(())
                },
                |bound_key| {
                    if key.get() == Some(bound_key) {
                        key.set(None);
                    }
                    Ok(())
                },
                |deadline| {
                    assert_eq!(deadline, Deadline::from_monotonic_ns(50));
                    Err(ipc::Error::TIMED_OUT)
                },
                || Ok(50),
            )
            .unwrap();
        assert_eq!(executor.outcome(id), Some(TaskOutcome::TimedOut));
    }

    #[test]
    fn task_executor_rejects_unregistered_pending_task() {
        let reactor = Reactor::<1>::new();
        let mut task = future::pending::<ipc::Result<()>>();
        let mut executor = TaskExecutor::<1, 1> {
            reactor: &reactor,
            event_port: None,
            tasks: core::array::from_fn(|_| None),
            generations: [0; 1],
            bindings: core::array::from_fn(|_| None),
            running: false,
        };
        let id = executor
            .spawn_scoped(Pin::new(&mut task), RunScope::new(Deadline::INFINITE))
            .unwrap();
        assert_eq!(
            executor.run_with(
                |_, _| unreachable!(),
                |_| unreachable!(),
                |_| unreachable!(),
                || unreachable!(),
            ),
            Err(TaskRunError::UnregisteredPending(id))
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
