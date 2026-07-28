use core::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

use spin::{Mutex, MutexGuard};
use x86_64::instructions::interrupts;

static PREEMPTION_DEPTH: AtomicUsize = AtomicUsize::new(0);

pub struct Guard;

impl Guard {
    pub fn enter() -> Self {
        interrupts::without_interrupts(|| {
            let previous = PREEMPTION_DEPTH.fetch_add(1, Ordering::AcqRel);
            assert!(previous != usize::MAX, "kernel preemption depth overflowed");
        });
        Self
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        interrupts::without_interrupts(|| {
            let previous = PREEMPTION_DEPTH.fetch_sub(1, Ordering::AcqRel);
            assert!(previous != 0, "kernel preemption depth underflowed");
        });
    }
}

pub fn is_disabled() -> bool {
    PREEMPTION_DEPTH.load(Ordering::Acquire) != 0
}

pub struct PreemptMutex<T> {
    inner: Mutex<T>,
}

impl<T> PreemptMutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
        }
    }

    pub fn lock(&self) -> PreemptMutexGuard<'_, T> {
        let preemption = Guard::enter();
        let guard = self.inner.lock();
        PreemptMutexGuard {
            guard: Some(guard),
            preemption: Some(preemption),
        }
    }
}

pub struct PreemptMutexGuard<'a, T> {
    guard: Option<MutexGuard<'a, T>>,
    preemption: Option<Guard>,
}

impl<T> Deref for PreemptMutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.guard
            .as_deref()
            .expect("preemption mutex guard missing")
    }
}

impl<T> DerefMut for PreemptMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard
            .as_deref_mut()
            .expect("preemption mutex guard missing")
    }
}

impl<T> Drop for PreemptMutexGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.guard.take());
        drop(self.preemption.take());
    }
}
