//! Bounded job hierarchy, process containment, and resource accounting.
//!
//! `job::State` owns one job's membership and exit-observation queue.  This
//! module owns the hierarchy-wide policy around it: every reservation is
//! charged to the owning job and all ancestors, limits can only tighten, and
//! subtree termination releases resources without leaving accounting debt.

use alloc::{vec, vec::Vec};

use crate::process_model::ProcessId;

pub const MAX_CONTAINMENT_JOBS: usize = 64;
pub const MAX_CHILDREN_PER_JOB: usize = 16;
pub const MAX_PROCESSES_PER_JOB: usize = 128;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct JobId(u64);

impl JobId {
    pub const fn from_raw(raw: u64) -> Option<Self> {
        if raw == 0 { None } else { Some(Self(raw)) }
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceUsage {
    pub processes: u64,
    pub threads: u64,
    pub memory_bytes: u64,
    pub cpu_ticks: u64,
    pub handles: u64,
}

impl ResourceUsage {
    pub const ZERO: Self = Self {
        processes: 0,
        threads: 0,
        memory_bytes: 0,
        cpu_ticks: 0,
        handles: 0,
    };

    pub const fn process(threads: u64, memory_bytes: u64, handles: u64) -> Self {
        Self {
            processes: 1,
            threads,
            memory_bytes,
            cpu_ticks: 0,
            handles,
        }
    }

    fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            processes: self.processes.checked_add(other.processes)?,
            threads: self.threads.checked_add(other.threads)?,
            memory_bytes: self.memory_bytes.checked_add(other.memory_bytes)?,
            cpu_ticks: self.cpu_ticks.checked_add(other.cpu_ticks)?,
            handles: self.handles.checked_add(other.handles)?,
        })
    }

    fn checked_sub(self, other: Self) -> Option<Self> {
        Some(Self {
            processes: self.processes.checked_sub(other.processes)?,
            threads: self.threads.checked_sub(other.threads)?,
            memory_bytes: self.memory_bytes.checked_sub(other.memory_bytes)?,
            cpu_ticks: self.cpu_ticks.checked_sub(other.cpu_ticks)?,
            handles: self.handles.checked_sub(other.handles)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceLimits {
    pub processes: u64,
    pub threads: u64,
    pub memory_bytes: u64,
    pub cpu_ticks: u64,
    pub handles: u64,
}

impl ResourceLimits {
    pub const UNLIMITED: Self = Self {
        processes: u64::MAX,
        threads: u64::MAX,
        memory_bytes: u64::MAX,
        cpu_ticks: u64::MAX,
        handles: u64::MAX,
    };

    pub const fn allows(self, usage: ResourceUsage) -> bool {
        usage.processes <= self.processes
            && usage.threads <= self.threads
            && usage.memory_bytes <= self.memory_bytes
            && usage.cpu_ticks <= self.cpu_ticks
            && usage.handles <= self.handles
    }

    const fn no_relaxation(self, replacement: Self) -> bool {
        replacement.processes <= self.processes
            && replacement.threads <= self.threads
            && replacement.memory_bytes <= self.memory_bytes
            && replacement.cpu_ticks <= self.cpu_ticks
            && replacement.handles <= self.handles
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobSnapshot {
    pub id: JobId,
    pub parent: Option<JobId>,
    pub limits: ResourceLimits,
    pub usage: ResourceUsage,
    pub child_count: usize,
    pub process_count: usize,
    pub retired: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainmentError {
    JobLimit,
    ChildLimit,
    ProcessLimit,
    InvalidJob,
    InvalidProcess,
    DuplicateProcess,
    Retired,
    Root,
    LimitExceeded,
    LimitRelaxation,
    UsageOverflow,
    UsageUnderflow,
    ProcessNotFound,
    SubtreeNotEmpty,
}

#[derive(Clone, Copy)]
struct ProcessReservation {
    process: ProcessId,
    usage: ResourceUsage,
}

struct JobNode {
    id: JobId,
    parent: Option<JobId>,
    limits: ResourceLimits,
    usage: ResourceUsage,
    children: Vec<JobId>,
    processes: Vec<ProcessReservation>,
    retired: bool,
}

/// Bounded containment tree with ancestor-inclusive resource accounting.
pub struct ContainmentTree {
    next_job_id: u64,
    jobs: Vec<JobNode>,
}

impl Default for ContainmentTree {
    fn default() -> Self {
        Self::new()
    }
}

impl ContainmentTree {
    pub const fn new() -> Self {
        Self {
            next_job_id: 1,
            jobs: Vec::new(),
        }
    }

    pub fn job_count(&self) -> usize {
        self.jobs.len()
    }

    pub fn create_root(&mut self, limits: ResourceLimits) -> Result<JobId, ContainmentError> {
        if !self.jobs.is_empty() {
            return Err(ContainmentError::JobLimit);
        }
        self.create_node(None, limits)
    }

    pub fn create_child(
        &mut self,
        parent: JobId,
        limits: ResourceLimits,
    ) -> Result<JobId, ContainmentError> {
        let parent_index = self.job_index(parent)?;
        if self.jobs[parent_index].retired {
            return Err(ContainmentError::Retired);
        }
        if self.jobs[parent_index].children.len() >= MAX_CHILDREN_PER_JOB {
            return Err(ContainmentError::ChildLimit);
        }
        let child = self.create_node(Some(parent), limits)?;
        self.jobs[parent_index].children.push(child);
        Ok(child)
    }

    pub fn set_limits(
        &mut self,
        job: JobId,
        limits: ResourceLimits,
    ) -> Result<(), ContainmentError> {
        let index = self.job_index(job)?;
        let node = &mut self.jobs[index];
        if node.retired {
            return Err(ContainmentError::Retired);
        }
        if !node.limits.no_relaxation(limits) {
            return Err(ContainmentError::LimitRelaxation);
        }
        if !limits.allows(node.usage) {
            return Err(ContainmentError::LimitExceeded);
        }
        node.limits = limits;
        Ok(())
    }

    pub fn admit_process(
        &mut self,
        job: JobId,
        process: ProcessId,
        usage: ResourceUsage,
    ) -> Result<(), ContainmentError> {
        if process.raw() == 0 || usage.processes != 1 {
            return Err(ContainmentError::InvalidProcess);
        }
        let job_index = self.job_index(job)?;
        if self.jobs[job_index].retired {
            return Err(ContainmentError::Retired);
        }
        if self
            .jobs
            .iter()
            .any(|node| node.processes.iter().any(|entry| entry.process == process))
        {
            return Err(ContainmentError::DuplicateProcess);
        }
        if self.jobs[job_index].processes.len() >= MAX_PROCESSES_PER_JOB {
            return Err(ContainmentError::ProcessLimit);
        }
        self.check_chain_capacity(job, usage)?;
        self.apply_chain_delta(job, usage, true)?;
        self.jobs[job_index]
            .processes
            .push(ProcessReservation { process, usage });
        Ok(())
    }

    pub fn set_process_usage(
        &mut self,
        job: JobId,
        process: ProcessId,
        replacement: ResourceUsage,
    ) -> Result<(), ContainmentError> {
        let job_index = self.job_index(job)?;
        let reservation_index = self.process_index(job_index, process)?;
        let current = self.jobs[job_index].processes[reservation_index].usage;
        if replacement.processes == 0 {
            return Err(ContainmentError::InvalidProcess);
        }
        if replacement.processes != current.processes {
            return Err(ContainmentError::InvalidProcess);
        }
        if replacement.memory_bytes >= current.memory_bytes
            || replacement.threads >= current.threads
            || replacement.cpu_ticks >= current.cpu_ticks
            || replacement.handles >= current.handles
        {
            let delta = ResourceUsage {
                processes: replacement.processes.saturating_sub(current.processes),
                threads: replacement.threads.saturating_sub(current.threads),
                memory_bytes: replacement
                    .memory_bytes
                    .saturating_sub(current.memory_bytes),
                cpu_ticks: replacement.cpu_ticks.saturating_sub(current.cpu_ticks),
                handles: replacement.handles.saturating_sub(current.handles),
            };
            self.check_chain_capacity(job, delta)?;
            self.apply_chain_delta(job, delta, true)?;
        }
        let decrease = ResourceUsage {
            processes: current.processes.saturating_sub(replacement.processes),
            threads: current.threads.saturating_sub(replacement.threads),
            memory_bytes: current
                .memory_bytes
                .saturating_sub(replacement.memory_bytes),
            cpu_ticks: current.cpu_ticks.saturating_sub(replacement.cpu_ticks),
            handles: current.handles.saturating_sub(replacement.handles),
        };
        self.apply_chain_delta(job, decrease, false)?;
        self.jobs[job_index].processes[reservation_index].usage = replacement;
        Ok(())
    }

    pub fn release_process(
        &mut self,
        job: JobId,
        process: ProcessId,
    ) -> Result<ResourceUsage, ContainmentError> {
        let job_index = self.job_index(job)?;
        let reservation_index = self.process_index(job_index, process)?;
        let reservation = self.jobs[job_index]
            .processes
            .swap_remove(reservation_index);
        self.apply_chain_delta(job, reservation.usage, false)?;
        Ok(reservation.usage)
    }

    pub fn snapshot(&self, job: JobId) -> Result<JobSnapshot, ContainmentError> {
        let node = &self.jobs[self.job_index(job)?];
        Ok(JobSnapshot {
            id: node.id,
            parent: node.parent,
            limits: node.limits,
            usage: node.usage,
            child_count: node.children.len(),
            process_count: node.processes.len(),
            retired: node.retired,
        })
    }

    /// Force-stop every process in a non-root subtree and retire its nodes.
    pub fn terminate_subtree(&mut self, root: JobId) -> Result<usize, ContainmentError> {
        let root_index = self.job_index(root)?;
        if self.jobs[root_index].parent.is_none() {
            return Err(ContainmentError::Root);
        }
        if self.jobs[root_index].retired {
            return Err(ContainmentError::Retired);
        }
        let descendants = self.collect_subtree(root)?;
        let mut released = 0usize;
        for job in descendants.iter().rev().copied() {
            let index = self.job_index(job)?;
            let reservations = core::mem::take(&mut self.jobs[index].processes);
            released = released.saturating_add(reservations.len());
            for reservation in reservations {
                self.apply_chain_delta(job, reservation.usage, false)?;
            }
        }
        let parent = self.jobs[root_index].parent.ok_or(ContainmentError::Root)?;
        let parent_index = self.job_index(parent)?;
        self.jobs[parent_index]
            .children
            .retain(|child| *child != root);
        for job in descendants {
            let index = self.job_index(job)?;
            self.jobs[index].children.clear();
            self.jobs[index].parent = None;
            self.jobs[index].retired = true;
        }
        Ok(released)
    }

    fn create_node(
        &mut self,
        parent: Option<JobId>,
        limits: ResourceLimits,
    ) -> Result<JobId, ContainmentError> {
        if self.jobs.len() >= MAX_CONTAINMENT_JOBS {
            return Err(ContainmentError::JobLimit);
        }
        let id = JobId::from_raw(self.next_job_id).ok_or(ContainmentError::JobLimit)?;
        self.next_job_id = self
            .next_job_id
            .checked_add(1)
            .ok_or(ContainmentError::JobLimit)?;
        self.jobs.push(JobNode {
            id,
            parent,
            limits,
            usage: ResourceUsage::ZERO,
            children: Vec::new(),
            processes: Vec::new(),
            retired: false,
        });
        Ok(id)
    }

    fn check_chain_capacity(
        &self,
        job: JobId,
        delta: ResourceUsage,
    ) -> Result<(), ContainmentError> {
        let mut current = Some(job);
        while let Some(id) = current {
            let index = self.job_index(id)?;
            let node = &self.jobs[index];
            if node.retired {
                return Err(ContainmentError::Retired);
            }
            let projected = node
                .usage
                .checked_add(delta)
                .ok_or(ContainmentError::UsageOverflow)?;
            if !node.limits.allows(projected) {
                return Err(ContainmentError::LimitExceeded);
            }
            current = node.parent;
        }
        Ok(())
    }

    fn apply_chain_delta(
        &mut self,
        job: JobId,
        delta: ResourceUsage,
        add: bool,
    ) -> Result<(), ContainmentError> {
        let mut chain = Vec::new();
        let mut current = Some(job);
        while let Some(id) = current {
            chain.push(id);
            let index = self.job_index(id)?;
            current = self.jobs[index].parent;
        }
        for id in chain {
            let index = self.job_index(id)?;
            self.jobs[index].usage = if add {
                self.jobs[index]
                    .usage
                    .checked_add(delta)
                    .ok_or(ContainmentError::UsageOverflow)?
            } else {
                self.jobs[index]
                    .usage
                    .checked_sub(delta)
                    .ok_or(ContainmentError::UsageUnderflow)?
            };
        }
        Ok(())
    }

    fn collect_subtree(&self, root: JobId) -> Result<Vec<JobId>, ContainmentError> {
        let mut result = Vec::new();
        let mut pending = vec![root];
        while let Some(job) = pending.pop() {
            let index = self.job_index(job)?;
            result.push(job);
            pending.extend(self.jobs[index].children.iter().copied());
        }
        Ok(result)
    }

    fn job_index(&self, job: JobId) -> Result<usize, ContainmentError> {
        self.jobs
            .iter()
            .position(|node| node.id == job)
            .ok_or(ContainmentError::InvalidJob)
    }

    fn process_index(
        &self,
        job_index: usize,
        process: ProcessId,
    ) -> Result<usize, ContainmentError> {
        self.jobs[job_index]
            .processes
            .iter()
            .position(|entry| entry.process == process)
            .ok_or(ContainmentError::ProcessNotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(raw: u64) -> ProcessId {
        ProcessId::from_raw(raw).unwrap()
    }

    fn limits(processes: u64, memory_bytes: u64) -> ResourceLimits {
        ResourceLimits {
            processes,
            threads: 16,
            memory_bytes,
            cpu_ticks: 100,
            handles: 32,
        }
    }

    #[test]
    fn reservations_charge_every_ancestor_and_release_cleanly() {
        let mut tree = ContainmentTree::new();
        let root = tree.create_root(limits(4, 4096)).unwrap();
        let child = tree.create_child(root, limits(2, 2048)).unwrap();
        tree.admit_process(child, process(1), ResourceUsage::process(2, 1024, 3))
            .unwrap();

        assert_eq!(tree.snapshot(child).unwrap().usage.processes, 1);
        assert_eq!(tree.snapshot(root).unwrap().usage.memory_bytes, 1024);
        assert_eq!(
            tree.release_process(child, process(1))
                .unwrap()
                .memory_bytes,
            1024
        );
        assert_eq!(tree.snapshot(root).unwrap().usage, ResourceUsage::ZERO);
    }

    #[test]
    fn effective_ancestor_limits_reject_admission_and_usage_growth() {
        let mut tree = ContainmentTree::new();
        let root = tree.create_root(limits(1, 1024)).unwrap();
        let child = tree.create_child(root, ResourceLimits::UNLIMITED).unwrap();
        tree.admit_process(child, process(1), ResourceUsage::process(1, 1024, 0))
            .unwrap();
        assert_eq!(
            tree.admit_process(child, process(2), ResourceUsage::process(1, 1, 0)),
            Err(ContainmentError::LimitExceeded)
        );
        assert_eq!(
            tree.set_process_usage(child, process(1), ResourceUsage::process(1, 2048, 0)),
            Err(ContainmentError::LimitExceeded)
        );
    }

    #[test]
    fn limits_only_tighten_and_cannot_drop_below_current_usage() {
        let mut tree = ContainmentTree::new();
        let root = tree.create_root(limits(4, 4096)).unwrap();
        tree.admit_process(root, process(1), ResourceUsage::process(1, 1024, 0))
            .unwrap();
        assert_eq!(
            tree.set_limits(root, limits(5, 4096)),
            Err(ContainmentError::LimitRelaxation)
        );
        assert_eq!(
            tree.set_limits(root, limits(1, 512)),
            Err(ContainmentError::LimitExceeded)
        );
        tree.set_limits(root, limits(1, 2048)).unwrap();
        assert_eq!(tree.snapshot(root).unwrap().limits.processes, 1);
    }

    #[test]
    fn subtree_termination_releases_aggregates_and_retires_descendants() {
        let mut tree = ContainmentTree::new();
        let root = tree.create_root(limits(8, 8192)).unwrap();
        let child = tree.create_child(root, limits(4, 4096)).unwrap();
        let grandchild = tree.create_child(child, limits(2, 2048)).unwrap();
        tree.admit_process(child, process(1), ResourceUsage::process(1, 512, 1))
            .unwrap();
        tree.admit_process(grandchild, process(2), ResourceUsage::process(2, 1024, 2))
            .unwrap();

        assert_eq!(tree.terminate_subtree(child).unwrap(), 2);
        assert_eq!(tree.snapshot(root).unwrap().usage, ResourceUsage::ZERO);
        assert!(tree.snapshot(child).unwrap().retired);
        assert!(tree.snapshot(grandchild).unwrap().retired);
        assert_eq!(
            tree.admit_process(child, process(3), ResourceUsage::process(1, 1, 0)),
            Err(ContainmentError::Retired)
        );
    }

    #[test]
    fn duplicate_processes_and_missing_releases_are_rejected() {
        let mut tree = ContainmentTree::new();
        let root = tree.create_root(ResourceLimits::UNLIMITED).unwrap();
        let child = tree.create_child(root, ResourceLimits::UNLIMITED).unwrap();
        tree.admit_process(root, process(1), ResourceUsage::process(1, 1, 0))
            .unwrap();
        assert_eq!(
            tree.admit_process(child, process(1), ResourceUsage::process(1, 1, 0)),
            Err(ContainmentError::DuplicateProcess)
        );
        assert_eq!(
            tree.release_process(child, process(2)),
            Err(ContainmentError::ProcessNotFound)
        );
    }
}
