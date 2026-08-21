use core::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

use spin::{Mutex, MutexGuard};
use x86_64::instructions::interrupts;

use crate::arch::x86_64::smp_runtime;

const MAX_CPUS: usize = 64;
static PREEMPTION_DEPTHS: [AtomicUsize; MAX_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_CPUS];

pub struct Guard {
    cpu_index: usize,
}

impl Guard {
    pub fn enter() -> Self {
        interrupts::without_interrupts(|| {
            let cpu_index = smp_runtime::current_cpu_index().min(MAX_CPUS - 1);
            let previous = PREEMPTION_DEPTHS[cpu_index].fetch_add(1, Ordering::AcqRel);
            assert!(previous != usize::MAX, "kernel preemption depth overflowed");
            Self { cpu_index }
        })
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        interrupts::without_interrupts(|| {
            let previous = PREEMPTION_DEPTHS[self.cpu_index].fetch_sub(1, Ordering::AcqRel);
            assert!(previous != 0, "kernel preemption depth underflowed");
        });
    }
}

pub fn is_disabled() -> bool {
    let cpu_index = smp_runtime::current_cpu_index().min(MAX_CPUS - 1);
    PREEMPTION_DEPTHS[cpu_index].load(Ordering::Acquire) != 0
}

pub fn depth_for_cpu(cpu_index: usize) -> usize {
    PREEMPTION_DEPTHS
        .get(cpu_index)
        .map(|depth| depth.load(Ordering::Acquire))
        .unwrap_or(0)
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
