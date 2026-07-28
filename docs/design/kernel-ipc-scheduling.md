# Kernel, IPC, and scheduling direction

## Status

The hybrid kernel direction, capability-oriented IPC, and distinct realtime and
interactive scheduling classes are **accepted direction**. Exact algorithms, queue
sizes, and ABI details remain **tentative design** until measured in implementation.

## Kernel boundary

NullStar should use a pragmatic hybrid-microkernel structure. The kernel retains
latency-critical mechanisms and the authority needed to isolate processes:

- threads, processes, address spaces, and job objects;
- virtual memory and page-fault handling;
- scheduling, synchronization, timers, and interrupt delivery;
- capability tables, IPC primitives, and shared-memory mappings;
- the minimum boot, diagnostic, and compatibility infrastructure required while
  services migrate to userspace.

Policy-oriented components should move to supervised userspace services as their
protocols mature, including filesystem policy, device discovery, networking, media,
graphics, identity, and package management. Migration should be incremental rather
than requiring a pure-microkernel rewrite.

Kernel objects must have explicit ownership, lifetime, rights, and destruction rules.
Processes should refer to objects through typed handles rather than global identifiers
or kernel pointers. Rights may be attenuated during duplication or transfer but not
amplified without an independently held authority.

## Process model

The architecture should distinguish:

- a **process** as a security and resource container;
- a **thread** as a schedulable execution context;
- an **address space** as a first-class virtual-memory object;
- a **job** as a hierarchy for lifecycle, limits, and collective policy.

The ABI should not assume one thread per process even while early userspace remains
mostly single-threaded.

## IPC model

The primary IPC primitive should be a pair of bidirectional channel endpoints.
Channels carry bounded control messages and may transfer rights-reduced handles.
Large or continuous data must use mapped shared memory rather than repeated message
copies.

Planned channel features include:

- asynchronous messages;
- transaction identifiers and a call/reply library abstraction;
- cancellation and deadlines;
- handle transfer with explicit rights;
- bounded queues and deterministic failure behavior;
- events, counted notifications, and waiting on multiple objects;
- priority donation across bounded synchronous dependencies.

Long-running work should be asynchronous. Services must not hold unrelated locks
while making synchronous calls, and protocol design should avoid deep synchronous
call chains. A future completion-port abstraction should unify IPC, timers, process
exit, file completion, sockets, display events, and media events.

## Scheduler policy

NullStar should optimize for **predictable latency rather than absolute priority**.
Realtime and interactive work have different contracts:

- **Realtime** work must complete a bounded operation before a deadline.
- **Interactive** work should wake quickly in response to human input and display
  deadlines, but may occasionally miss a frame without compromising system safety.

The intended scheduling classes are:

1. **Critical kernel work**: minimal interrupt and scheduler mechanisms.
2. **Realtime**: trusted, budgeted media or driver workers.
3. **Interactive**: compositor, input, window management, and foreground UI work.
4. **Normal**: ordinary applications and services.
5. **Background**: indexing, updates, maintenance, and explicitly reduced-priority work.
6. **Idle**: work that must never delay useful activity.

The media graph may briefly preempt the compositor when needed to meet an audio
quantum deadline. The compositor receives strong wakeup preference and low latency,
but should not receive unbounded realtime authority. Realtime execution must always
be budgeted; a failing media processor should be bypassed, muted, or restarted rather
than monopolizing a CPU.

## Early scheduler

The preferred early desktop scheduler is a preemptive multilevel feedback queue with:

- short quanta and wakeup preemption for interactive threads;
- demotion of CPU-bound threads that repeatedly consume complete quanta;
- periodic starvation prevention;
- a separate restricted fixed-priority realtime class;
- priority inheritance for kernel locks and bounded synchronous IPC;
- scheduler tracing for wakeup latency, runtime, deadline misses, and inversion.

Exact quantum lengths are implementation parameters, not ABI commitments.

## SMP evolution

Before SMP, scheduler and preemption state must become per-CPU. The SMP design should
use per-CPU run queues, cache-aware placement, idle-CPU work stealing, and bounded
load balancing rather than a permanently contended global queue. Later work may add
CPU topology, heterogeneous-core, NUMA, PCID, and affinity policy.

Timers should be represented as deadlines so the implementation can evolve from a
periodic tick toward tickless idle operation.

## Interrupt and synchronization rules

Hardware interrupt handlers should acknowledge hardware, record minimal state, and
wake a scheduled worker. Complex driver work does not belong in hard-interrupt
context.

Spinlocks are limited to short sections where sleeping is impossible. Longer waits
use mutexes, events, conditions, or wait queues. Userspace synchronization should use
a futex-like wait/wake primitive so uncontended locking remains outside the kernel.

## Open questions

- Whether the normal scheduler should remain MLFQ or later evolve toward a virtual
  runtime or eligible-deadline model.
- The exact realtime admission and CPU-budget policy.
- The completion-port ABI and its relationship to existing endpoint waits.
- Limits on synchronous IPC depth and priority-donation chains.
