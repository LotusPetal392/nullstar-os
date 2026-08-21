//! Kernel-side observability contract for Nova and the system monitor.
//!
//! The execution subsystems intentionally keep their state private. This module
//! provides a stable, read-only shape for publishing that state without making
//! the monitor depend on scheduler or process-manager internals.

use alloc::vec::Vec;

use crate::address_space::AddressSpaceSnapshot;
use crate::containment::JobSnapshot;
use crate::process_model::{ProcessSnapshot, ThreadSnapshot};
use crate::scheduling::CpuSnapshot;

/// Maximum number of records a single monitor snapshot is intended to expose.
pub const MAX_SNAPSHOT_RECORDS: usize = 128;

/// Memory counters supplied by the physical-memory/frame allocator.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MemorySummary {
    /// Total physical memory visible to the kernel, in bytes.
    pub total_bytes: u64,
    /// Memory currently allocated by the kernel allocator, in bytes.
    pub allocated_bytes: u64,
    /// Bytes represented by mapped user pages.
    pub mapped_bytes: u64,
    /// Bytes currently represented by copy-on-write mappings.
    pub copy_on_write_bytes: u64,
}

impl MemorySummary {
    pub const ZERO: Self = Self {
        total_bytes: 0,
        allocated_bytes: 0,
        mapped_bytes: 0,
        copy_on_write_bytes: 0,
    };

    pub const fn available_bytes(self) -> u64 {
        self.total_bytes.saturating_sub(self.allocated_bytes)
    }

    pub const fn utilization_basis_points(self) -> u16 {
        if self.total_bytes == 0 {
            return 0;
        }
        let ratio = self.allocated_bytes.saturating_mul(10_000) / self.total_bytes;
        if ratio > 10_000 { 10_000 } else { ratio as u16 }
    }
}

/// Aggregate CPU state currently available from the scheduler.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuSummary {
    pub online_cpus: usize,
    pub busy_cpus: usize,
    pub runnable_threads: usize,
}

impl CpuSummary {
    pub const ZERO: Self = Self {
        online_cpus: 0,
        busy_cpus: 0,
        runnable_threads: 0,
    };
}

/// Per-process monitor record. CPU and memory accounting are supplied by the
/// execution/accounting layer once those counters are attached to processes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessRecord {
    pub process: ProcessSnapshot,
    pub address_space: Option<AddressSpaceSnapshot>,
    pub cpu_ticks: u64,
    pub memory_bytes: u64,
}

/// Complete read-only kernel view consumed by system-monitor code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelSnapshot {
    pub sequence: u64,
    pub cpu: CpuSummary,
    pub memory: MemorySummary,
    pub processes: Vec<ProcessRecord>,
    pub threads: Vec<ThreadSnapshot>,
    pub cpus: Vec<CpuSnapshot>,
    pub address_spaces: Vec<AddressSpaceSnapshot>,
    pub jobs: Vec<JobSnapshot>,
}

impl KernelSnapshot {
    /// Build a monitor snapshot from subsystem-owned read-only snapshots.
    ///
    /// Callers may provide more records than the monitor ABI can reasonably
    /// display; the snapshot is bounded deterministically by
    /// `MAX_SNAPSHOT_RECORDS`.
    pub fn from_parts(
        sequence: u64,
        cpu_records: &[CpuSnapshot],
        memory: MemorySummary,
        process_records: &[ProcessRecord],
        threads: &[ThreadSnapshot],
        address_spaces: &[AddressSpaceSnapshot],
        jobs: &[JobSnapshot],
    ) -> Self {
        let cpus = bounded_copy(cpu_records);
        let processes = bounded_copy(process_records);
        let threads = bounded_copy(threads);
        let address_spaces = bounded_copy(address_spaces);
        let jobs = bounded_copy(jobs);

        let mut cpu = CpuSummary::ZERO;
        cpu.online_cpus = cpus.len();
        for record in &cpus {
            if record.current.is_some() {
                cpu.busy_cpus = cpu.busy_cpus.saturating_add(1);
            }
            cpu.runnable_threads = cpu.runnable_threads.saturating_add(record.runnable_count);
        }

        Self {
            sequence,
            cpu,
            memory,
            processes,
            threads,
            cpus,
            address_spaces,
            jobs,
        }
    }

    pub const fn process_count(&self) -> usize {
        self.processes.len()
    }

    pub const fn thread_count(&self) -> usize {
        self.threads.len()
    }

    pub const fn job_count(&self) -> usize {
        self.jobs.len()
    }

    pub const fn address_space_count(&self) -> usize {
        self.address_spaces.len()
    }
}

fn bounded_copy<T: Copy>(records: &[T]) -> Vec<T> {
    let count = core::cmp::min(records.len(), MAX_SNAPSHOT_RECORDS);
    records[..count].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address_space::{AddressSpaceId, AddressSpaceSnapshot};
    use crate::containment::{JobId, ResourceLimits, ResourceUsage};
    use crate::process_model::{ProcessId, ProcessState, ThreadId, ThreadState};
    use crate::scheduling::{CpuId, CpuSnapshot};

    fn process(raw: u64) -> ProcessId {
        ProcessId::from_raw(raw).unwrap()
    }

    fn thread(raw: u64, process_id: ProcessId) -> ThreadSnapshot {
        ThreadSnapshot {
            id: ThreadId::from_raw(raw).unwrap(),
            process_id,
            name: "worker",
            state: ThreadState::Runnable,
            detached: false,
            exit: None,
        }
    }

    #[test]
    fn aggregates_monitor_state() {
        let cpu = [
            CpuSnapshot {
                cpu: CpuId::from_raw(0).unwrap(),
                current: Some(ThreadId::from_raw(1).unwrap()),
                runnable_count: 2,
            },
            CpuSnapshot {
                cpu: CpuId::from_raw(1).unwrap(),
                current: None,
                runnable_count: 1,
            },
        ];
        let pid = process(1);
        let process = ProcessRecord {
            process: ProcessSnapshot {
                id: pid,
                parent: None,
                state: ProcessState::Running,
                thread_count: 1,
                live_thread_count: 1,
                child_count: 0,
                exit: None,
            },
            address_space: Some(AddressSpaceSnapshot {
                id: AddressSpaceId::from_raw(1).unwrap(),
                owner: pid,
                generation: 1,
                mapping_count: 4,
                copy_on_write_count: 1,
            }),
            cpu_ticks: 10,
            memory_bytes: 16_384,
        };
        let job = JobSnapshot {
            id: JobId::from_raw(1).unwrap(),
            parent: None,
            limits: ResourceLimits::UNLIMITED,
            usage: ResourceUsage::process(1, 16_384, 2),
            child_count: 0,
            process_count: 1,
            retired: false,
        };

        let snapshot = KernelSnapshot::from_parts(
            9,
            &cpu,
            MemorySummary {
                total_bytes: 1_000_000,
                allocated_bytes: 250_000,
                mapped_bytes: 16_384,
                copy_on_write_bytes: 4_096,
            },
            &[process],
            &[thread(1, pid)],
            &[process.address_space.unwrap()],
            &[job],
        );

        assert_eq!(snapshot.sequence, 9);
        assert_eq!(snapshot.cpu.online_cpus, 2);
        assert_eq!(snapshot.cpu.busy_cpus, 1);
        assert_eq!(snapshot.cpu.runnable_threads, 3);
        assert_eq!(snapshot.memory.available_bytes(), 750_000);
        assert_eq!(snapshot.memory.utilization_basis_points(), 2_500);
        assert_eq!(snapshot.process_count(), 1);
        assert_eq!(snapshot.thread_count(), 1);
        assert_eq!(snapshot.job_count(), 1);
    }

    #[test]
    fn snapshot_is_bounded() {
        let mut records = Vec::new();
        for raw in 1..=(MAX_SNAPSHOT_RECORDS as u64 + 8) {
            records.push(thread(raw, process(1)));
        }

        let snapshot =
            KernelSnapshot::from_parts(1, &[], MemorySummary::ZERO, &[], &records, &[], &[]);

        assert_eq!(snapshot.thread_count(), MAX_SNAPSHOT_RECORDS);
    }
}
