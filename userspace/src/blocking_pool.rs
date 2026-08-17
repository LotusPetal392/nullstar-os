//! Fixed-capacity coordination for work that must run outside async polling.
//!
//! The coordinator owns no threads and performs no allocation. A runtime
//! supplies up to `WORKERS` execution contexts and calls [`BlockingPool::run_next`]
//! from them. Admission is FIFO and bounded by `JOBS`; structured task-group
//! cancellation and deadlines are checked before work begins. Once a callback
//! has begun, cancellation is cooperative with the callback: this layer cannot
//! preempt arbitrary synchronous code without the future thread/address-space
//! substrate.

use core::array;

use crate::{
    async_ipc::{TaskAttribution, TaskGroup},
    ipc::{self, Deadline},
};

/// Policy ceiling for caller-provisioned blocking execution contexts.
pub const MAX_BLOCKING_WORKERS: usize = 8;
/// Policy ceiling for admitted or terminal-but-unreaped blocking jobs.
pub const MAX_BLOCKING_JOBS: usize = 64;
/// Number of recent blocking lifecycle transitions retained in memory.
pub const MAX_BLOCKING_TRACE_EVENTS: usize = 64;

/// A synchronous operation suitable for a blocking execution context.
pub trait BlockingWork {
    fn run(&mut self) -> ipc::Result<()>;
}

impl<F> BlockingWork for F
where
    F: FnMut() -> ipc::Result<()>,
{
    fn run(&mut self) -> ipc::Result<()> {
        self()
    }
}

/// Stable identity for one fixed blocking-job slot generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockingJobId {
    slot: usize,
    generation: u32,
}

impl BlockingJobId {
    pub const fn slot(self) -> usize {
        self.slot
    }

    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Terminal result retained until the submitter reaps the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockingOutcome {
    Completed(ipc::Result<()>),
    Cancelled,
    TimedOut,
    Shutdown,
}

/// Result of one worker dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockingRun {
    pub job: BlockingJobId,
    pub outcome: BlockingOutcome,
}

/// Current bounded-capacity usage.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BlockingPoolStats {
    pub queued: usize,
    pub running: usize,
    pub terminal: usize,
    pub free: usize,
    pub shutdown: bool,
}

/// Jobs converted to terminal outcomes by a shutdown request.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BlockingShutdownReport {
    pub cancelled_queued: usize,
    pub already_terminal: usize,
}

/// One retained coordinator lifecycle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockingTraceKind {
    Submitted,
    Started { worker: usize },
    Completed(ipc::Result<()>),
    Cancelled,
    TimedOut,
    Shutdown,
    Reaped,
}

/// Stable, attribution-carrying trace record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockingTraceEvent {
    pub sequence: u64,
    pub job: BlockingJobId,
    pub attribution: TaskAttribution,
    pub kind: BlockingTraceKind,
}

/// Result of copying one bounded trace page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockingTraceRead {
    pub events: usize,
    pub next_cursor: u64,
    pub missed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobState {
    Queued,
    Running { worker: usize },
    Terminal(BlockingOutcome),
}

struct JobSlot<'work, 'group> {
    id: BlockingJobId,
    work: &'work mut dyn BlockingWork,
    group: &'group TaskGroup,
    deadline: Deadline,
    attribution: TaskAttribution,
    state: JobState,
}

struct TraceBuffer {
    events: [Option<BlockingTraceEvent>; MAX_BLOCKING_TRACE_EVENTS],
    next_sequence: u64,
    len: usize,
}

impl TraceBuffer {
    fn new() -> Self {
        Self {
            events: [None; MAX_BLOCKING_TRACE_EVENTS],
            next_sequence: 1,
            len: 0,
        }
    }

    fn record(
        &mut self,
        job: BlockingJobId,
        attribution: TaskAttribution,
        kind: BlockingTraceKind,
    ) {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let index = usize::try_from(sequence.saturating_sub(1)).unwrap_or(usize::MAX)
            % MAX_BLOCKING_TRACE_EVENTS;
        self.events[index] = Some(BlockingTraceEvent {
            sequence,
            job,
            attribution,
            kind,
        });
        self.len = self.len.saturating_add(1).min(MAX_BLOCKING_TRACE_EVENTS);
    }

    fn read(&self, after: u64, output: &mut [Option<BlockingTraceEvent>]) -> BlockingTraceRead {
        output.fill(None);
        if output.is_empty() || self.len == 0 {
            return BlockingTraceRead {
                events: 0,
                next_cursor: after,
                missed: 0,
            };
        }
        let oldest = self.next_sequence.saturating_sub(self.len as u64);
        let requested = after.saturating_add(1);
        let start = requested.max(oldest);
        let missed = start.saturating_sub(requested);
        let newest = self.next_sequence.saturating_sub(1);
        let mut copied = 0;
        let mut cursor = after;
        for sequence in start..=newest {
            if copied == output.len() {
                break;
            }
            let index = usize::try_from(sequence.saturating_sub(1)).unwrap_or(usize::MAX)
                % MAX_BLOCKING_TRACE_EVENTS;
            let Some(event) = self.events[index].filter(|event| event.sequence == sequence) else {
                continue;
            };
            output[copied] = Some(event);
            copied += 1;
            cursor = sequence;
        }
        BlockingTraceRead {
            events: copied,
            next_cursor: cursor,
            missed,
        }
    }
}

/// Allocation-free FIFO admission and lifecycle coordination for blocking work.
pub struct BlockingPool<'work, 'group, const WORKERS: usize, const JOBS: usize> {
    jobs: [Option<JobSlot<'work, 'group>>; JOBS],
    generations: [u32; JOBS],
    queue: [Option<BlockingJobId>; JOBS],
    queued: usize,
    shutdown: bool,
    trace: TraceBuffer,
}

impl<'work, 'group, const WORKERS: usize, const JOBS: usize>
    BlockingPool<'work, 'group, WORKERS, JOBS>
{
    pub fn new() -> ipc::Result<Self> {
        if WORKERS == 0 || WORKERS > MAX_BLOCKING_WORKERS || JOBS == 0 || JOBS > MAX_BLOCKING_JOBS {
            return Err(ipc::Error::INVALID_ARGUMENT);
        }
        Ok(Self {
            jobs: array::from_fn(|_| None),
            generations: [0; JOBS],
            queue: [None; JOBS],
            queued: 0,
            shutdown: false,
            trace: TraceBuffer::new(),
        })
    }

    /// Admits one callback and retains its task-group attribution and lifecycle.
    pub fn submit(
        &mut self,
        work: &'work mut dyn BlockingWork,
        group: &'group TaskGroup,
        deadline: Deadline,
    ) -> ipc::Result<BlockingJobId> {
        if self.shutdown {
            return Err(ipc::Error::BROKEN_PIPE);
        }
        if self.queued == JOBS {
            return Err(ipc::Error::NO_SPACE);
        }
        let Some(slot) = self.jobs.iter().position(Option::is_none) else {
            return Err(ipc::Error::NO_SPACE);
        };
        let generation = self.generations[slot].wrapping_add(1).max(1);
        self.generations[slot] = generation;
        let id = BlockingJobId { slot, generation };
        let deadline = earlier_deadline(group.deadline(), deadline);
        let attribution = group.attribution();
        self.jobs[slot] = Some(JobSlot {
            id,
            work,
            group,
            deadline,
            attribution,
            state: JobState::Queued,
        });
        self.queue[self.queued] = Some(id);
        self.queued += 1;
        self.trace
            .record(id, attribution, BlockingTraceKind::Submitted);
        Ok(id)
    }

    /// Runs at most one FIFO callback from the indicated execution context.
    pub fn run_next(&mut self, worker: usize, now: Deadline) -> ipc::Result<Option<BlockingRun>> {
        if worker >= WORKERS {
            return Err(ipc::Error::INVALID_ARGUMENT);
        }
        let Some(id) = self.pop_queue() else {
            return Ok(None);
        };
        let (attribution, cancelled, deadline) = {
            let slot = self.slot(id)?;
            (slot.attribution, slot.group.is_cancelled(), slot.deadline)
        };
        let outcome = match cancelled {
            Ok(true) => BlockingOutcome::Cancelled,
            Err(error) => BlockingOutcome::Completed(Err(error)),
            Ok(false) if deadline_expired(deadline, now) => BlockingOutcome::TimedOut,
            Ok(false) => {
                self.slot_mut(id)?.state = JobState::Running { worker };
                self.trace
                    .record(id, attribution, BlockingTraceKind::Started { worker });
                BlockingOutcome::Completed(self.slot_mut(id)?.work.run())
            }
        };
        self.slot_mut(id)?.state = JobState::Terminal(outcome);
        self.record_outcome(id, attribution, outcome);
        Ok(Some(BlockingRun { job: id, outcome }))
    }

    /// Cancels work that has not started. Executing callbacks are not preempted.
    pub fn cancel(&mut self, id: BlockingJobId) -> ipc::Result<()> {
        let state = self.slot(id)?.state;
        if state != JobState::Queued {
            return Err(ipc::Error::TRY_AGAIN);
        }
        self.remove_from_queue(id);
        let attribution = self.slot(id)?.attribution;
        self.slot_mut(id)?.state = JobState::Terminal(BlockingOutcome::Cancelled);
        self.trace
            .record(id, attribution, BlockingTraceKind::Cancelled);
        Ok(())
    }

    /// Stops admission and terminalizes every callback that has not started.
    pub fn shutdown(&mut self) -> BlockingShutdownReport {
        self.shutdown = true;
        let mut report = BlockingShutdownReport::default();
        while let Some(id) = self.pop_queue() {
            if let Ok(slot) = self.slot_mut(id) {
                let attribution = slot.attribution;
                slot.state = JobState::Terminal(BlockingOutcome::Shutdown);
                self.trace
                    .record(id, attribution, BlockingTraceKind::Shutdown);
                report.cancelled_queued += 1;
            }
        }
        report.already_terminal = self
            .jobs
            .iter()
            .flatten()
            .filter(|slot| matches!(slot.state, JobState::Terminal(_)))
            .count()
            .saturating_sub(report.cancelled_queued);
        report
    }

    pub fn outcome(&self, id: BlockingJobId) -> Option<BlockingOutcome> {
        match self.slot(id).ok()?.state {
            JobState::Terminal(outcome) => Some(outcome),
            JobState::Queued | JobState::Running { .. } => None,
        }
    }

    pub fn attribution(&self, id: BlockingJobId) -> Option<TaskAttribution> {
        self.slot(id).ok().map(|slot| slot.attribution)
    }

    /// Releases one terminal slot and the callback borrow held by it.
    pub fn reap(&mut self, id: BlockingJobId) -> ipc::Result<BlockingOutcome> {
        let slot = self.slot(id)?;
        let JobState::Terminal(outcome) = slot.state else {
            return Err(ipc::Error::TRY_AGAIN);
        };
        let attribution = slot.attribution;
        self.jobs[id.slot] = None;
        self.trace
            .record(id, attribution, BlockingTraceKind::Reaped);
        Ok(outcome)
    }

    pub fn stats(&self) -> BlockingPoolStats {
        let mut stats = BlockingPoolStats {
            queued: 0,
            running: 0,
            terminal: 0,
            free: 0,
            shutdown: self.shutdown,
        };
        for slot in &self.jobs {
            match slot.as_ref().map(|slot| slot.state) {
                Some(JobState::Queued) => stats.queued += 1,
                Some(JobState::Running { .. }) => stats.running += 1,
                Some(JobState::Terminal(_)) => stats.terminal += 1,
                None => stats.free += 1,
            }
        }
        stats
    }

    pub fn read_trace(
        &self,
        after: u64,
        output: &mut [Option<BlockingTraceEvent>],
    ) -> BlockingTraceRead {
        self.trace.read(after, output)
    }

    fn slot(&self, id: BlockingJobId) -> ipc::Result<&JobSlot<'work, 'group>> {
        self.jobs
            .get(id.slot)
            .and_then(Option::as_ref)
            .filter(|slot| slot.id == id)
            .ok_or(ipc::Error::NO_ENTRY)
    }

    fn slot_mut(&mut self, id: BlockingJobId) -> ipc::Result<&mut JobSlot<'work, 'group>> {
        self.jobs
            .get_mut(id.slot)
            .and_then(Option::as_mut)
            .filter(|slot| slot.id == id)
            .ok_or(ipc::Error::NO_ENTRY)
    }

    fn pop_queue(&mut self) -> Option<BlockingJobId> {
        let id = self.queue[0]?;
        for index in 1..self.queued {
            self.queue[index - 1] = self.queue[index];
        }
        self.queued -= 1;
        self.queue[self.queued] = None;
        Some(id)
    }

    fn remove_from_queue(&mut self, id: BlockingJobId) {
        let Some(position) = self.queue[..self.queued]
            .iter()
            .position(|queued| *queued == Some(id))
        else {
            return;
        };
        for index in position + 1..self.queued {
            self.queue[index - 1] = self.queue[index];
        }
        self.queued -= 1;
        self.queue[self.queued] = None;
    }

    fn record_outcome(
        &mut self,
        id: BlockingJobId,
        attribution: TaskAttribution,
        outcome: BlockingOutcome,
    ) {
        let kind = match outcome {
            BlockingOutcome::Completed(result) => BlockingTraceKind::Completed(result),
            BlockingOutcome::Cancelled => BlockingTraceKind::Cancelled,
            BlockingOutcome::TimedOut => BlockingTraceKind::TimedOut,
            BlockingOutcome::Shutdown => BlockingTraceKind::Shutdown,
        };
        self.trace.record(id, attribution, kind);
    }
}

const fn earlier_deadline(left: Deadline, right: Deadline) -> Deadline {
    if left.as_monotonic_ns() <= right.as_monotonic_ns() {
        left
    } else {
        right
    }
}

const fn deadline_expired(deadline: Deadline, now: Deadline) -> bool {
    deadline.as_monotonic_ns() != Deadline::INFINITE.as_monotonic_ns()
        && now.as_monotonic_ns() >= deadline.as_monotonic_ns()
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::{BlockingOutcome, BlockingPool, BlockingTraceKind, MAX_BLOCKING_TRACE_EVENTS};
    use crate::{
        async_ipc::{TaskGroup, TaskRole},
        ipc::{self, Deadline},
    };

    #[test]
    fn fifo_dispatch_retains_outcomes_and_attribution() {
        let group = TaskGroup::root(TaskRole::Background, Deadline::INFINITE).unwrap();
        let order = Cell::new(0);
        let mut first = || {
            assert_eq!(order.replace(1), 0);
            Ok(())
        };
        let mut second = || {
            assert_eq!(order.replace(2), 1);
            Err(ipc::Error::IO)
        };
        let mut pool = BlockingPool::<2, 2>::new().unwrap();
        let first_id = pool.submit(&mut first, &group, Deadline::INFINITE).unwrap();
        let second_id = pool
            .submit(&mut second, &group, Deadline::INFINITE)
            .unwrap();

        assert_eq!(
            pool.run_next(1, Deadline::IMMEDIATE).unwrap().unwrap().job,
            first_id
        );
        assert_eq!(
            pool.run_next(0, Deadline::IMMEDIATE)
                .unwrap()
                .unwrap()
                .outcome,
            BlockingOutcome::Completed(Err(ipc::Error::IO))
        );
        assert_eq!(
            pool.outcome(first_id),
            Some(BlockingOutcome::Completed(Ok(())))
        );
        assert_eq!(pool.attribution(second_id), Some(group.attribution()));
        assert_eq!(pool.stats().terminal, 2);
        assert_eq!(pool.reap(first_id), Ok(BlockingOutcome::Completed(Ok(()))));
    }

    #[test]
    fn cancellation_deadline_capacity_and_shutdown_are_bounded() {
        let cancelled = TaskGroup::new(Deadline::INFINITE).unwrap();
        cancelled.cancel().unwrap();
        let active = TaskGroup::new(Deadline::INFINITE).unwrap();
        let calls = Cell::new(0);
        let mut one = || {
            calls.set(calls.get() + 1);
            Ok(())
        };
        let mut two = || {
            calls.set(calls.get() + 1);
            Ok(())
        };
        let mut rejected = || Ok(());
        let mut pool = BlockingPool::<1, 2>::new().unwrap();
        let cancelled_id = pool
            .submit(&mut one, &cancelled, Deadline::INFINITE)
            .unwrap();
        let timed_id = pool
            .submit(&mut two, &active, Deadline::from_monotonic_ns(10))
            .unwrap();
        assert_eq!(
            pool.submit(&mut rejected, &active, Deadline::INFINITE),
            Err(ipc::Error::NO_SPACE)
        );
        assert_eq!(
            pool.run_next(0, Deadline::from_monotonic_ns(1))
                .unwrap()
                .unwrap()
                .outcome,
            BlockingOutcome::Cancelled
        );
        assert_eq!(
            pool.run_next(0, Deadline::from_monotonic_ns(10))
                .unwrap()
                .unwrap()
                .outcome,
            BlockingOutcome::TimedOut
        );
        assert_eq!(calls.get(), 0);
        assert_eq!(pool.outcome(cancelled_id), Some(BlockingOutcome::Cancelled));
        assert_eq!(pool.outcome(timed_id), Some(BlockingOutcome::TimedOut));

        pool.reap(cancelled_id).unwrap();
        let mut queued_work = || Ok(());
        let queued = pool
            .submit(&mut queued_work, &active, Deadline::INFINITE)
            .unwrap();
        assert_eq!(pool.shutdown().cancelled_queued, 1);
        assert_eq!(pool.outcome(queued), Some(BlockingOutcome::Shutdown));
        let mut after_shutdown = || Ok(());
        assert_eq!(
            pool.submit(&mut after_shutdown, &active, Deadline::INFINITE),
            Err(ipc::Error::BROKEN_PIPE)
        );
    }

    #[test]
    fn explicit_cancel_reuses_generation_and_trace_reports_overwrite() {
        let group = TaskGroup::new(Deadline::INFINITE).unwrap();
        let mut first_work = || Ok(());
        let mut second_work = || Ok(());
        let mut pool = BlockingPool::<1, 1>::new().unwrap();
        let first = pool
            .submit(&mut first_work, &group, Deadline::INFINITE)
            .unwrap();
        pool.cancel(first).unwrap();
        assert_eq!(pool.reap(first), Ok(BlockingOutcome::Cancelled));
        let second = pool
            .submit(&mut second_work, &group, Deadline::INFINITE)
            .unwrap();
        assert_eq!(first.slot(), second.slot());
        assert_ne!(first.generation(), second.generation());
        pool.cancel(second).unwrap();

        let mut trace = super::TraceBuffer::new();
        for _ in 0..MAX_BLOCKING_TRACE_EVENTS + 4 {
            trace.record(second, group.attribution(), BlockingTraceKind::Submitted);
        }
        let mut page = [None; 4];
        let read = trace.read(0, &mut page);
        assert_eq!(read.events, 4);
        assert!(read.missed > 0);
        assert!(matches!(
            page[0].unwrap().kind,
            BlockingTraceKind::Submitted | BlockingTraceKind::Cancelled | BlockingTraceKind::Reaped
        ));
    }

    #[test]
    fn invalid_dimensions_and_worker_are_rejected() {
        assert!(matches!(
            BlockingPool::<0, 1>::new(),
            Err(ipc::Error::INVALID_ARGUMENT)
        ));
        assert!(matches!(
            BlockingPool::<1, 0>::new(),
            Err(ipc::Error::INVALID_ARGUMENT)
        ));
        let mut pool = BlockingPool::<1, 1>::new().unwrap();
        assert_eq!(
            pool.run_next(1, Deadline::IMMEDIATE),
            Err(ipc::Error::INVALID_ARGUMENT)
        );
    }
}
