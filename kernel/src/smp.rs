//! SMP-safe synchronization and bounded cross-CPU IPC primitives.
//!
//! These primitives deliberately do not perform interrupt or APIC work.  They
//! provide the ownership and ordering rules that per-CPU scheduling and the
//! later hardware bring-up layer can rely on without allocating or sharing
//! unprotected queues.

use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{
    process_model::{ProcessId, ThreadId},
    scheduling::{CpuId, CpuMask, MAX_CPUS},
};

/// A non-poisoning SMP mutex with an explicit nonblocking acquisition path.
///
/// The lock uses acquire/release ordering around the protected value.  It is
/// suitable for short kernel critical sections; callers that cannot spin must
/// use [`SmpMutex::try_lock`] instead of [`SmpMutex::lock`].
pub struct SmpMutex<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// The atomic lock establishes the happens-before relationship for access to T.
unsafe impl<T: Send> Sync for SmpMutex<T> {}
unsafe impl<T: Send> Send for SmpMutex<T> {}

impl<T> SmpMutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> SmpMutexGuard<'_, T> {
        loop {
            if let Some(guard) = self.try_lock() {
                return guard;
            }
            spin_loop();
        }
    }

    pub fn try_lock(&self) -> Option<SmpMutexGuard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| SmpMutexGuard { mutex: self })
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}

pub struct SmpMutexGuard<'a, T> {
    mutex: &'a SmpMutex<T>,
}

impl<T> Deref for SmpMutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // The guard can only be constructed while the lock is held.
        unsafe { &*self.mutex.value.get() }
    }
}

impl<T> DerefMut for SmpMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // The guard is the unique owner of the lock for its lifetime.
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<T> Drop for SmpMutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.locked.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SendError<T> {
    Full(T),
    Closed(T),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrySendError<T> {
    Busy(T),
    Full(T),
    Closed(T),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveError {
    Empty,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TryReceiveError {
    Busy,
    Empty,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelSnapshot {
    pub capacity: usize,
    pub queued: usize,
    pub closed: bool,
}

struct ChannelState<T, const CAPACITY: usize> {
    slots: [Option<T>; CAPACITY],
    head: usize,
    tail: usize,
    queued: usize,
    closed: bool,
}

/// A bounded FIFO channel protected by an SMP mutex.
pub struct SmpChannel<T, const CAPACITY: usize> {
    state: SmpMutex<ChannelState<T, CAPACITY>>,
}

impl<T, const CAPACITY: usize> SmpChannel<T, CAPACITY> {
    pub fn new() -> Self {
        Self {
            state: SmpMutex::new(ChannelState {
                slots: [const { None }; CAPACITY],
                head: 0,
                tail: 0,
                queued: 0,
                closed: false,
            }),
        }
    }

    pub const fn capacity(&self) -> usize {
        CAPACITY
    }

    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        let mut state = self.state.lock();
        Self::enqueue(&mut state, value)
    }

    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        let Some(mut state) = self.state.try_lock() else {
            return Err(TrySendError::Busy(value));
        };
        Self::enqueue(&mut state, value).map_err(|error| match error {
            SendError::Full(value) => TrySendError::Full(value),
            SendError::Closed(value) => TrySendError::Closed(value),
        })
    }

    pub fn receive(&self) -> Result<T, ReceiveError> {
        let mut state = self.state.lock();
        Self::dequeue(&mut state)
    }

    pub fn try_receive(&self) -> Result<T, TryReceiveError> {
        let Some(mut state) = self.state.try_lock() else {
            return Err(TryReceiveError::Busy);
        };
        Self::dequeue(&mut state).map_err(|error| match error {
            ReceiveError::Empty => TryReceiveError::Empty,
            ReceiveError::Closed => TryReceiveError::Closed,
        })
    }

    /// Close the channel and reject future sends.  Queued values remain
    /// available to the receiver until drained.
    pub fn close(&self) -> bool {
        let mut state = self.state.lock();
        let changed = !state.closed;
        state.closed = true;
        changed
    }

    pub fn snapshot(&self) -> ChannelSnapshot {
        let state = self.state.lock();
        ChannelSnapshot {
            capacity: CAPACITY,
            queued: state.queued,
            closed: state.closed,
        }
    }

    fn enqueue(state: &mut ChannelState<T, CAPACITY>, value: T) -> Result<(), SendError<T>> {
        if state.closed {
            return Err(SendError::Closed(value));
        }
        if state.queued == CAPACITY {
            return Err(SendError::Full(value));
        }
        // CAPACITY == 0 is always full, so this index is only reached for a
        // nonzero capacity.
        state.slots[state.tail] = Some(value);
        state.tail = (state.tail + 1) % CAPACITY;
        state.queued += 1;
        Ok(())
    }

    fn dequeue(state: &mut ChannelState<T, CAPACITY>) -> Result<T, ReceiveError> {
        if state.queued == 0 {
            return if state.closed {
                Err(ReceiveError::Closed)
            } else {
                Err(ReceiveError::Empty)
            };
        }
        let value = state.slots[state.head]
            .take()
            .expect("queued channel slot must contain a value");
        state.head = (state.head + 1) % CAPACITY;
        state.queued -= 1;
        Ok(value)
    }
}

impl<T, const CAPACITY: usize> Default for SmpChannel<T, CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpcMessage {
    Reschedule,
    WakeThread(ThreadId),
    InvalidateAddressSpace { address_space: u64, generation: u64 },
    SignalProcess { process: ProcessId, signal: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IpcEnvelope {
    pub source: CpuId,
    pub target: CpuId,
    pub message: IpcMessage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpcSendError {
    InvalidCpu(IpcEnvelope),
    OfflineCpu(IpcEnvelope),
    Busy(IpcEnvelope),
    Full(IpcEnvelope),
    Closed(IpcEnvelope),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpcReceiveError {
    InvalidCpu,
    OfflineCpu,
    Busy,
    Empty,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpcTopologyError {
    EmptyOnlineMask,
}

/// Fixed per-CPU mailboxes for scheduler wakeups and other cross-CPU events.
pub struct SmpIpc<const CAPACITY: usize = 8> {
    online_mask: CpuMask,
    mailboxes: [SmpChannel<IpcEnvelope, CAPACITY>; MAX_CPUS],
}

impl<const CAPACITY: usize> SmpIpc<CAPACITY> {
    pub fn new(online_mask: CpuMask) -> Result<Self, IpcTopologyError> {
        if online_mask.is_empty() {
            return Err(IpcTopologyError::EmptyOnlineMask);
        }
        Ok(Self {
            online_mask,
            mailboxes: core::array::from_fn(|_| SmpChannel::new()),
        })
    }

    pub const fn online_mask(&self) -> CpuMask {
        self.online_mask
    }

    pub fn send(
        &self,
        source: CpuId,
        target: CpuId,
        message: IpcMessage,
    ) -> Result<(), IpcSendError> {
        let envelope = IpcEnvelope {
            source,
            target,
            message,
        };
        self.validate_route(envelope)?;
        self.mailboxes[target.raw()]
            .send(envelope)
            .map_err(|error| match error {
                SendError::Full(envelope) => IpcSendError::Full(envelope),
                SendError::Closed(envelope) => IpcSendError::Closed(envelope),
            })
    }

    pub fn try_send(
        &self,
        source: CpuId,
        target: CpuId,
        message: IpcMessage,
    ) -> Result<(), IpcSendError> {
        let envelope = IpcEnvelope {
            source,
            target,
            message,
        };
        self.validate_route(envelope)?;
        self.mailboxes[target.raw()]
            .try_send(envelope)
            .map_err(|error| match error {
                TrySendError::Busy(envelope) => IpcSendError::Busy(envelope),
                TrySendError::Full(envelope) => IpcSendError::Full(envelope),
                TrySendError::Closed(envelope) => IpcSendError::Closed(envelope),
            })
    }

    pub fn receive(&self, target: CpuId) -> Result<IpcEnvelope, IpcReceiveError> {
        self.validate_cpu(target)?;
        self.mailboxes[target.raw()]
            .receive()
            .map_err(|error| match error {
                ReceiveError::Empty => IpcReceiveError::Empty,
                ReceiveError::Closed => IpcReceiveError::Closed,
            })
    }

    pub fn try_receive(&self, target: CpuId) -> Result<IpcEnvelope, IpcReceiveError> {
        self.validate_cpu(target)?;
        self.mailboxes[target.raw()]
            .try_receive()
            .map_err(|error| match error {
                TryReceiveError::Busy => IpcReceiveError::Busy,
                TryReceiveError::Empty => IpcReceiveError::Empty,
                TryReceiveError::Closed => IpcReceiveError::Closed,
            })
    }

    pub fn close_cpu(&self, target: CpuId) -> Result<bool, IpcReceiveError> {
        self.validate_cpu(target)?;
        Ok(self.mailboxes[target.raw()].close())
    }

    pub fn mailbox_snapshot(&self, target: CpuId) -> Result<ChannelSnapshot, IpcReceiveError> {
        self.validate_cpu(target)?;
        Ok(self.mailboxes[target.raw()].snapshot())
    }

    fn validate_route(&self, envelope: IpcEnvelope) -> Result<(), IpcSendError> {
        if envelope.source.raw() >= MAX_CPUS || envelope.target.raw() >= MAX_CPUS {
            return Err(IpcSendError::InvalidCpu(envelope));
        }
        if !self.online_mask.contains(envelope.source)
            || !self.online_mask.contains(envelope.target)
        {
            return Err(IpcSendError::OfflineCpu(envelope));
        }
        Ok(())
    }

    fn validate_cpu(&self, cpu: CpuId) -> Result<(), IpcReceiveError> {
        if cpu.raw() >= MAX_CPUS {
            return Err(IpcReceiveError::InvalidCpu);
        }
        if !self.online_mask.contains(cpu) {
            return Err(IpcReceiveError::OfflineCpu);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_model::ThreadId;

    #[test]
    fn mutex_guard_serializes_mutation_and_try_lock_reports_contention() {
        let mutex = SmpMutex::new(41_u64);
        let guard = mutex.lock();
        assert!(mutex.try_lock().is_none());
        drop(guard);
        *mutex.lock() += 1;
        assert_eq!(*mutex.lock(), 42);
    }

    #[test]
    fn channel_is_fifo_bounded_and_drains_before_closed() {
        let channel = SmpChannel::<u8, 2>::new();
        channel.send(1).unwrap();
        channel.send(2).unwrap();
        assert_eq!(channel.send(3), Err(SendError::Full(3)));
        assert_eq!(channel.receive(), Ok(1));
        assert_eq!(channel.receive(), Ok(2));
        assert_eq!(channel.receive(), Err(ReceiveError::Empty));
        assert!(channel.close());
        assert!(!channel.close());
        assert_eq!(channel.receive(), Err(ReceiveError::Closed));
        assert_eq!(channel.send(4), Err(SendError::Closed(4)));
        assert!(channel.snapshot().closed);
    }

    #[test]
    fn try_channel_operations_preserve_values_on_lock_contention() {
        let channel = SmpChannel::<u8, 1>::new();
        let guard = channel.state.lock();
        assert_eq!(channel.try_send(7), Err(TrySendError::Busy(7)));
        assert_eq!(channel.try_receive(), Err(TryReceiveError::Busy));
        drop(guard);
        channel.try_send(7).unwrap();
        assert_eq!(channel.try_receive(), Ok(7));
    }

    #[test]
    fn ipc_routes_envelopes_and_rejects_offline_cpus() {
        let cpu0 = CpuId::from_raw(0).unwrap();
        let cpu1 = CpuId::from_raw(1).unwrap();
        let cpu2 = CpuId::from_raw(2).unwrap();
        let ipc = SmpIpc::<2>::new(CpuMask::first(2).unwrap()).unwrap();
        let thread = ThreadId::from_raw(9).unwrap();

        ipc.send(cpu0, cpu1, IpcMessage::WakeThread(thread))
            .unwrap();
        assert_eq!(
            ipc.receive(cpu1),
            Ok(IpcEnvelope {
                source: cpu0,
                target: cpu1,
                message: IpcMessage::WakeThread(thread),
            })
        );
        assert_eq!(
            ipc.try_send(cpu0, cpu2, IpcMessage::Reschedule),
            Err(IpcSendError::OfflineCpu(IpcEnvelope {
                source: cpu0,
                target: cpu2,
                message: IpcMessage::Reschedule,
            }))
        );
    }

    #[test]
    fn ipc_mailboxes_are_bounded_and_close_deterministically() {
        let cpu0 = CpuId::from_raw(0).unwrap();
        let cpu1 = CpuId::from_raw(1).unwrap();
        let ipc = SmpIpc::<1>::new(CpuMask::first(2).unwrap()).unwrap();

        ipc.try_send(cpu0, cpu1, IpcMessage::Reschedule).unwrap();
        assert!(matches!(
            ipc.try_send(cpu0, cpu1, IpcMessage::Reschedule),
            Err(IpcSendError::Full(_))
        ));
        assert!(ipc.close_cpu(cpu1).unwrap());
        assert_eq!(ipc.receive(cpu1).unwrap().message, IpcMessage::Reschedule);
        assert_eq!(ipc.receive(cpu1), Err(IpcReceiveError::Closed));
        assert!(matches!(
            ipc.try_send(cpu0, cpu1, IpcMessage::Reschedule),
            Err(IpcSendError::Closed(_))
        ));
    }

    #[test]
    fn ipc_requires_at_least_one_online_cpu() {
        assert!(matches!(
            SmpIpc::<1>::new(CpuMask::empty()),
            Err(IpcTopologyError::EmptyOnlineMask)
        ));
    }
}
