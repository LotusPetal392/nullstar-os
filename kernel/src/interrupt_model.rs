//! Bounded interrupt, exception, and timer policy for the hardware-facing paths.
//!
//! The x86 IDT and APIC remain responsible for saving registers and delivering
//! vectors. This module keeps the policy that follows delivery architecture
//! neutral and host-testable: vectors are registered once, exceptions are
//! classified by execution context, and timer deadlines are advanced in a
//! deterministic bounded queue.

pub const MAX_INTERRUPT_VECTORS: usize = 256;
pub const MAX_INTERRUPT_TIMERS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct InterruptVector(u8);

impl InterruptVector {
    pub const fn new(raw: u8) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExceptionKind {
    DivideError,
    Breakpoint,
    InvalidOpcode,
    DoubleFault,
    GeneralProtection,
    PageFault,
    Other(InterruptVector),
}

impl ExceptionKind {
    pub const fn from_vector(vector: InterruptVector) -> Option<Self> {
        match vector.raw() {
            0 => Some(Self::DivideError),
            3 => Some(Self::Breakpoint),
            6 => Some(Self::InvalidOpcode),
            8 => Some(Self::DoubleFault),
            13 => Some(Self::GeneralProtection),
            14 => Some(Self::PageFault),
            raw if raw < 32 => Some(Self::Other(InterruptVector::new(raw))),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExceptionContext {
    Kernel,
    User,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExceptionDisposition {
    Resume,
    TerminateCurrent,
    Panic,
    Halt,
}

pub const fn exception_disposition(
    kind: ExceptionKind,
    context: ExceptionContext,
) -> ExceptionDisposition {
    match kind {
        ExceptionKind::Breakpoint => ExceptionDisposition::Resume,
        ExceptionKind::DoubleFault => ExceptionDisposition::Halt,
        _ => match context {
            ExceptionContext::Kernel => ExceptionDisposition::Panic,
            ExceptionContext::User => ExceptionDisposition::TerminateCurrent,
        },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptRoute {
    Timer,
    External,
    Syscall,
    Exception(ExceptionKind),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptEvent {
    TimerTick {
        vector: InterruptVector,
        ticks: u32,
    },
    External {
        vector: InterruptVector,
    },
    Syscall {
        vector: InterruptVector,
    },
    Exception {
        vector: InterruptVector,
        kind: ExceptionKind,
        context: ExceptionContext,
    },
}

impl InterruptEvent {
    const fn vector(self) -> InterruptVector {
        match self {
            Self::TimerTick { vector, .. }
            | Self::External { vector }
            | Self::Syscall { vector }
            | Self::Exception { vector, .. } => vector,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchResult {
    Timer {
        ticks: u32,
    },
    External,
    Syscall,
    Exception {
        kind: ExceptionKind,
        disposition: ExceptionDisposition,
    },
    Unhandled,
    RouteMismatch,
    InvalidTimerTicks,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouterStats {
    pub dispatches: u64,
    pub unhandled: u64,
    pub route_mismatches: u64,
    pub timer_ticks: u64,
    pub exceptions: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterError {
    Occupied,
    TimerOccupied,
    SyscallOccupied,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnregisterError {
    NotRegistered,
    RouteMismatch,
}

pub struct InterruptRouter {
    routes: [Option<InterruptRoute>; MAX_INTERRUPT_VECTORS],
    timer_vector: Option<InterruptVector>,
    syscall_vector: Option<InterruptVector>,
    stats: RouterStats,
}

impl Default for InterruptRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl InterruptRouter {
    pub const fn new() -> Self {
        Self {
            routes: [const { None }; MAX_INTERRUPT_VECTORS],
            timer_vector: None,
            syscall_vector: None,
            stats: RouterStats {
                dispatches: 0,
                unhandled: 0,
                route_mismatches: 0,
                timer_ticks: 0,
                exceptions: 0,
            },
        }
    }

    pub fn register(
        &mut self,
        vector: InterruptVector,
        route: InterruptRoute,
    ) -> Result<(), RegisterError> {
        let index = usize::from(vector.raw());
        if self.routes[index].is_some() {
            return Err(RegisterError::Occupied);
        }
        match route {
            InterruptRoute::Timer if self.timer_vector.is_some() => {
                return Err(RegisterError::TimerOccupied);
            }
            InterruptRoute::Syscall if self.syscall_vector.is_some() => {
                return Err(RegisterError::SyscallOccupied);
            }
            _ => {}
        }
        self.routes[index] = Some(route);
        match route {
            InterruptRoute::Timer => self.timer_vector = Some(vector),
            InterruptRoute::Syscall => self.syscall_vector = Some(vector),
            _ => {}
        }
        Ok(())
    }

    pub fn unregister(
        &mut self,
        vector: InterruptVector,
        expected: InterruptRoute,
    ) -> Result<(), UnregisterError> {
        let index = usize::from(vector.raw());
        let Some(route) = self.routes[index] else {
            return Err(UnregisterError::NotRegistered);
        };
        if route != expected {
            return Err(UnregisterError::RouteMismatch);
        }
        self.routes[index] = None;
        if self.timer_vector == Some(vector) {
            self.timer_vector = None;
        }
        if self.syscall_vector == Some(vector) {
            self.syscall_vector = None;
        }
        Ok(())
    }

    pub const fn route(&self, vector: InterruptVector) -> Option<InterruptRoute> {
        self.routes[vector.raw() as usize]
    }

    pub const fn timer_vector(&self) -> Option<InterruptVector> {
        self.timer_vector
    }

    pub const fn syscall_vector(&self) -> Option<InterruptVector> {
        self.syscall_vector
    }

    pub fn dispatch(&mut self, event: InterruptEvent) -> DispatchResult {
        self.stats.dispatches = self.stats.dispatches.saturating_add(1);
        let vector = event.vector();
        let route = self.route(vector);
        match (route, event) {
            (Some(InterruptRoute::Timer), InterruptEvent::TimerTick { ticks, .. }) if ticks > 0 => {
                self.stats.timer_ticks = self.stats.timer_ticks.saturating_add(u64::from(ticks));
                DispatchResult::Timer { ticks }
            }
            (Some(InterruptRoute::External), InterruptEvent::External { .. }) => {
                DispatchResult::External
            }
            (Some(InterruptRoute::Syscall), InterruptEvent::Syscall { .. }) => {
                DispatchResult::Syscall
            }
            (
                Some(InterruptRoute::Exception(expected)),
                InterruptEvent::Exception { kind, context, .. },
            ) if expected == kind => {
                self.stats.exceptions = self.stats.exceptions.saturating_add(1);
                DispatchResult::Exception {
                    kind,
                    disposition: exception_disposition(kind, context),
                }
            }
            (None, _) => {
                self.stats.unhandled = self.stats.unhandled.saturating_add(1);
                DispatchResult::Unhandled
            }
            (Some(_), InterruptEvent::TimerTick { ticks: 0, .. }) => {
                self.stats.route_mismatches = self.stats.route_mismatches.saturating_add(1);
                DispatchResult::InvalidTimerTicks
            }
            (Some(_), _) => {
                self.stats.route_mismatches = self.stats.route_mismatches.saturating_add(1);
                DispatchResult::RouteMismatch
            }
        }
    }

    pub const fn stats(&self) -> RouterStats {
        self.stats
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct InterruptTimerId(u32);

impl InterruptTimerId {
    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerMode {
    OneShot,
    Periodic { period_ns: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TimerEntry {
    id: InterruptTimerId,
    deadline_ns: u64,
    mode: TimerMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerError {
    Capacity,
    InvalidDeadline,
    InvalidPeriod,
    UnknownTimer,
    ClockWentBackwards,
    OutputFull,
    DeadlineOverflow,
}

pub struct InterruptTimerQueue<const CAPACITY: usize = MAX_INTERRUPT_TIMERS> {
    entries: [Option<TimerEntry>; CAPACITY],
    next_id: u32,
    last_now_ns: u64,
}

impl<const CAPACITY: usize> Default for InterruptTimerQueue<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAPACITY: usize> InterruptTimerQueue<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            entries: [const { None }; CAPACITY],
            next_id: 0,
            last_now_ns: 0,
        }
    }

    pub fn schedule(
        &mut self,
        deadline_ns: u64,
        mode: TimerMode,
    ) -> Result<InterruptTimerId, TimerError> {
        if deadline_ns == u64::MAX {
            return Err(TimerError::InvalidDeadline);
        }
        if matches!(mode, TimerMode::Periodic { period_ns: 0 }) {
            return Err(TimerError::InvalidPeriod);
        }
        let slot = self
            .entries
            .iter()
            .position(Option::is_none)
            .ok_or(TimerError::Capacity)?;
        let id = InterruptTimerId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or(TimerError::Capacity)?;
        self.entries[slot] = Some(TimerEntry {
            id,
            deadline_ns,
            mode,
        });
        Ok(id)
    }

    pub fn cancel(&mut self, id: InterruptTimerId) -> Result<(), TimerError> {
        let Some(slot) = self.find_slot(id) else {
            return Err(TimerError::UnknownTimer);
        };
        self.entries[slot] = None;
        Ok(())
    }

    pub fn advance(
        &mut self,
        now_ns: u64,
        fired: &mut [InterruptTimerId],
    ) -> Result<usize, TimerError> {
        if now_ns < self.last_now_ns {
            return Err(TimerError::ClockWentBackwards);
        }
        let due = self
            .entries
            .iter()
            .flatten()
            .filter(|entry| entry.deadline_ns <= now_ns)
            .count();
        if due > fired.len() {
            return Err(TimerError::OutputFull);
        }

        for entry in &self.entries {
            let Some(current) = *entry else {
                continue;
            };
            if current.deadline_ns > now_ns {
                continue;
            }
            let TimerMode::Periodic { period_ns } = current.mode else {
                continue;
            };
            let elapsed = now_ns - current.deadline_ns;
            let periods = elapsed / period_ns + 1;
            let increment = period_ns
                .checked_mul(periods)
                .ok_or(TimerError::DeadlineOverflow)?;
            current
                .deadline_ns
                .checked_add(increment)
                .ok_or(TimerError::DeadlineOverflow)?;
        }

        self.last_now_ns = now_ns;
        let mut count = 0;
        let mut deadlines = [0; CAPACITY];
        for entry in &self.entries {
            let Some(current) = *entry else {
                continue;
            };
            if current.deadline_ns > now_ns {
                continue;
            }
            fired[count] = current.id;
            deadlines[count] = current.deadline_ns;
            count += 1;
        }
        for index in 1..count {
            let mut position = index;
            while position > 0
                && (deadlines[position - 1], fired[position - 1])
                    > (deadlines[position], fired[position])
            {
                deadlines.swap(position - 1, position);
                fired.swap(position - 1, position);
                position -= 1;
            }
        }

        for entry in &mut self.entries {
            let Some(current) = *entry else {
                continue;
            };
            if current.deadline_ns > now_ns {
                continue;
            }
            match current.mode {
                TimerMode::OneShot => *entry = None,
                TimerMode::Periodic { period_ns } => {
                    let elapsed = now_ns - current.deadline_ns;
                    let periods = elapsed / period_ns + 1;
                    let increment = period_ns * periods;
                    let next_deadline = current.deadline_ns + increment;
                    *entry = Some(TimerEntry {
                        deadline_ns: next_deadline,
                        ..current
                    });
                }
            }
        }
        Ok(count)
    }

    pub fn active_count(&self) -> usize {
        self.entries.iter().flatten().count()
    }

    pub fn next_deadline(&self) -> Option<u64> {
        self.entries
            .iter()
            .flatten()
            .map(|entry| entry.deadline_ns)
            .min()
    }

    fn find_slot(&self, id: InterruptTimerId) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.id == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMER: InterruptVector = InterruptVector::new(32);
    const PAGE_FAULT: InterruptVector = InterruptVector::new(14);

    #[test]
    fn router_rejects_duplicate_special_routes_and_classifies_faults() {
        let mut router = InterruptRouter::new();
        router.register(TIMER, InterruptRoute::Timer).unwrap();
        assert_eq!(
            router.register(InterruptVector::new(33), InterruptRoute::Timer),
            Err(RegisterError::TimerOccupied)
        );
        router
            .register(
                PAGE_FAULT,
                InterruptRoute::Exception(ExceptionKind::PageFault),
            )
            .unwrap();
        assert_eq!(
            router.dispatch(InterruptEvent::Exception {
                vector: PAGE_FAULT,
                kind: ExceptionKind::PageFault,
                context: ExceptionContext::User,
            }),
            DispatchResult::Exception {
                kind: ExceptionKind::PageFault,
                disposition: ExceptionDisposition::TerminateCurrent,
            }
        );
        assert_eq!(router.stats().exceptions, 1);
    }

    #[test]
    fn router_records_unhandled_and_mismatched_delivery() {
        let mut router = InterruptRouter::new();
        assert_eq!(
            router.dispatch(InterruptEvent::External { vector: TIMER }),
            DispatchResult::Unhandled
        );
        router.register(TIMER, InterruptRoute::Timer).unwrap();
        assert_eq!(
            router.dispatch(InterruptEvent::External { vector: TIMER }),
            DispatchResult::RouteMismatch
        );
        assert_eq!(router.stats().unhandled, 1);
        assert_eq!(router.stats().route_mismatches, 1);
    }

    #[test]
    fn timer_queue_orders_due_timers_and_coalesces_periodic_ticks() {
        let mut queue = InterruptTimerQueue::<3>::new();
        let late = queue.schedule(35, TimerMode::OneShot).unwrap();
        let periodic = queue
            .schedule(10, TimerMode::Periodic { period_ns: 5 })
            .unwrap();
        let early = queue.schedule(22, TimerMode::OneShot).unwrap();
        let mut fired = [InterruptTimerId(99); 3];
        assert_eq!(queue.advance(19, &mut fired).unwrap(), 1);
        assert_eq!(fired[0], periodic);
        assert_eq!(queue.advance(26, &mut fired).unwrap(), 2);
        assert_eq!(fired[0], periodic);
        assert_eq!(fired[1], early);
        assert_eq!(queue.advance(31, &mut fired).unwrap(), 1);
        assert_eq!(fired[0], periodic);
        assert_eq!(queue.advance(36, &mut fired).unwrap(), 2);
        assert_eq!(fired[0], late);
        assert_eq!(fired[1], periodic);
        assert_eq!(queue.active_count(), 1);
        assert_eq!(queue.next_deadline(), Some(40));
    }

    #[test]
    fn timer_queue_preserves_state_when_output_is_too_small() {
        let mut queue = InterruptTimerQueue::<2>::new();
        let id = queue.schedule(10, TimerMode::OneShot).unwrap();
        queue.schedule(10, TimerMode::OneShot).unwrap();
        let mut fired = [InterruptTimerId(0); 1];
        assert_eq!(queue.advance(10, &mut fired), Err(TimerError::OutputFull));
        assert_eq!(queue.active_count(), 2);
        queue.cancel(id).unwrap();
    }

    #[test]
    fn exception_policy_panics_in_kernel_but_terminates_user_faults() {
        assert_eq!(
            exception_disposition(ExceptionKind::GeneralProtection, ExceptionContext::Kernel),
            ExceptionDisposition::Panic
        );
        assert_eq!(
            exception_disposition(ExceptionKind::GeneralProtection, ExceptionContext::User),
            ExceptionDisposition::TerminateCurrent
        );
        assert_eq!(
            exception_disposition(ExceptionKind::DoubleFault, ExceptionContext::User),
            ExceptionDisposition::Halt
        );
    }
}
