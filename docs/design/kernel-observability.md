# Kernel observability

## Purpose

The kernel observability layer provides a read-only, bounded snapshot contract for
Nova and the system monitor. The monitor should not depend on scheduler,
process-table, address-space, or job-manager internals. Those subsystems publish
snapshots; `kernel::observability::KernelSnapshot` combines them into one monitor
view.

## Exposed state

A snapshot contains:

- online CPU count and which CPUs currently have a running thread;
- runnable-thread count;
- per-CPU scheduler placement records;
- process identity, parent, lifecycle state, thread counts, and exit state;
- per-thread identity, owning process, name, and scheduler state;
- address-space identity, owner, mapping count, generation, and COW mapping count;
- job identity, hierarchy, limits, usage, process count, and retirement state;
- total, allocated, mapped, and COW memory counters;
- a monotonically increasing snapshot sequence number.

The process record also reserves CPU-time and memory-usage fields so the monitor
contract does not need to change when execution/resource accounting becomes
attached directly to process state.

## Snapshot rules

Snapshots are observational only. Taking one must not mutate scheduler state,
process state, capabilities, jobs, or address spaces.

Each record collection is bounded by `MAX_SNAPSHOT_RECORDS`. If a caller supplies
more records than the monitor contract accepts, the earliest bounded prefix is
retained. The sequence number lets Nova detect whether two samples came from the
same publication point.

The kernel reports raw counters and state rather than UI-derived percentages.
Nova should calculate display values such as CPU percentage from consecutive
samples where a time-based counter is available.

## Current CPU-accounting boundary

The scheduler already exposes per-CPU placement and runnable state. Full runtime
accounting (user time, kernel time, per-thread runtime, process aggregation, and
per-core utilization over time) remains part of the CPU-accounting work described
in `smp-and-threading.md`. This observability layer deliberately provides the
shape for those counters without inventing measurements that the scheduler does
not yet collect.

Likewise, `MemorySummary.total_bytes` and `allocated_bytes` are supplied by the
physical-memory/allocator layer. The observability contract does not infer total
RAM from the bounded address-space model.

## Intended monitor views

The contract is sufficient to support the initial Nova/system-monitor views:

1. **CPU overview** — online cores, currently busy cores, and runnable work;
2. **memory overview** — total, allocated, available, mapped, and COW memory;
3. **process list** — PID, parent, state, thread count, address-space association,
   CPU-time field, and memory field;
4. **thread inspection** — TID, PID, name, and scheduler state;
5. **job inspection** — containment hierarchy and resource usage/limits;
6. **per-core view** — CPU placement and runnable queue depth.

Future accounting fields can be added to the record structures while preserving
this separation between kernel state and monitor presentation.
