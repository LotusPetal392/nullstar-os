# Kernel, IPC, and scheduling direction

## Status

The pragmatic hybrid-microkernel boundary, process-local capability handles,
asynchronous-first channel IPC, hierarchical jobs, and distinct realtime and
interactive scheduling classes are **accepted direction**. Exact scheduler algorithms,
queue limits, syscall layouts, and realtime budgets remain **tentative design** until
measured in implementation.

The detailed object, rights, channel, waiting, shared-memory, call/reply, and process-
bootstrap contracts are specified in
[IPC, kernel object, and handle model](ipc-and-object-model.md). Current implemented
behavior remains documented in
[Capability and IPC protection model](../protection-model.md).

## Kernel boundary

NullStar should use a pragmatic hybrid-microkernel structure. The kernel retains
latency-critical mechanisms and the authority needed to isolate processes:

- threads, processes, address spaces, and job objects;
- virtual memory and page-fault handling;
- scheduling, synchronization, timers, and interrupt delivery;
- process-local handle tables, IPC primitives, and shared-memory mappings;
- the minimum boot, diagnostic, and compatibility infrastructure required while
  services migrate to userspace.

Policy-oriented components should move to supervised userspace services as their
protocols mature, including filesystem policy, device discovery, networking, media,
graphics, identity, packages, and desktop services. Migration is incremental rather
than a requirement to rewrite the early system as a pure microkernel immediately.

## Process and object model

The architecture distinguishes:

- a **process** as a security and resource container;
- a **thread** as a schedulable execution context;
- an **address space** as a first-class virtual-memory object;
- a **job** as a hierarchy for lifecycle, limits, and non-relaxable policy.

Kernel objects have explicit type, ownership, lifetime, rights, signals, and destruction
rules. Processes refer to them through opaque process-local handles rather than global
identifiers or kernel pointers. Rights may be preserved or attenuated during duplication
and transfer but never amplified without independently held authority.

The ABI must not assume one thread per process even while early userspace remains mostly
single-threaded.

## IPC model

The primary native IPC primitive is a pair of bidirectional channel endpoints. Channels
carry bounded control messages and may atomically move rights-reduced handles. Large or
continuous data uses mapped shared memory or specialized buffer objects rather than
repeated message copies.

The native IPC stack should provide:

- asynchronous messages and message boundaries;
- peer closure and level-triggered object signals;
- bounded queues, resource accounting, and deterministic backpressure;
- handle transfer with explicit rights and object types;
- absolute monotonic deadlines and cancellation;
- waiting on one or many objects, persistent tagged wait sets, and bounded queued event ports;
- a synchronous call/reply abstraction for small bounded requests;
- bounded priority donation across synchronous dependencies;
- typed versioned userspace protocol bindings and tracing.

NullStar is asynchronous-first, not asynchronous-only. Long-running work remains
asynchronous. Services must not hold unrelated locks while making synchronous calls, and
protocols should avoid deep call chains.

Pipes and byte streams remain for shell pipelines, standard streams, and POSIX
compatibility. They do not replace structured channels as the native service mechanism.

## Scheduler policy

NullStar should optimize for **predictable latency rather than absolute priority**.
Realtime and interactive work have different contracts:

- **Realtime** work must complete a bounded operation before a deadline.
- **Interactive** work should wake quickly for human input and display deadlines but may
  occasionally miss a frame without compromising system safety.

The intended scheduling classes are:

1. **Critical kernel work**: minimal interrupt and scheduler mechanisms.
2. **Realtime**: trusted, admitted, and budgeted media or driver workers.
3. **Interactive**: compositor, input, window management, and foreground UI work.
4. **Normal**: ordinary applications and services.
5. **Background**: indexing, updates, maintenance, and explicitly reduced-priority work.
6. **Idle**: work that must never delay useful activity.

A realtime media worker may briefly preempt compositor work to meet a declared audio
quantum. The compositor receives strong wakeup preference and low latency but not
unbounded realtime authority. A failing media processor should be bypassed, muted, or
restarted rather than monopolizing a CPU.

## Early desktop scheduler

The preferred early scheduler is a preemptive multilevel feedback queue with:

- short quanta and wakeup preemption for interactive threads;
- demotion of CPU-bound threads that repeatedly consume full quanta;
- periodic starvation prevention;
- a separate restricted fixed-priority realtime class;
- priority inheritance for kernel locks and bounded synchronous IPC;
- tracing for wakeup latency, runtime, deadline misses, and inversion.

Exact quantum lengths are implementation parameters, not ABI commitments.

## Synchronous dependency and priority donation

Priority donation is tied to a specific bounded wait dependency. It should:

- raise a server only enough to serve the blocked caller under policy limits;
- propagate through a strictly limited nested call chain;
- respect realtime admission and job CPU budgets;
- end on reply, timeout, cancellation, or peer failure;
- remain visible to tracing and wait-chain diagnostics.

A client must not manufacture unbudgeted realtime execution by repeatedly invoking a
service. Protocols whose work is unbounded or user-controlled remain asynchronous.

## SMP evolution

Before SMP, scheduler and preemption state must become per-CPU. The SMP design should use
per-CPU run queues, cache-aware placement, idle-CPU work stealing, and bounded load
balancing rather than a permanently contended global queue. Later work may add CPU
topology, heterogeneous-core, NUMA, PCID, and affinity policy.

Timers should be represented as monotonic deadlines so the implementation can evolve
from a periodic tick toward tickless idle operation.

## Interrupt and synchronization rules

Hardware interrupt handlers should acknowledge hardware, record minimal state, and wake
a scheduled worker. Complex driver work does not belong in hard-interrupt context.

Spinlocks are limited to short sections where sleeping is impossible. Longer waits use
mutexes, events, conditions, or wait queues. Userspace synchronization should use a
futex-like wait/wake primitive so uncontended locking remains outside the kernel.

## Open questions

- Whether the normal scheduler should remain MLFQ or later evolve toward a virtual-
  runtime or eligible-deadline model.
- The exact realtime admission and CPU-budget policy.
- The event-port ABI and registration model.
- Exact limits on synchronous IPC depth and priority-donation chains.
- Which compatibility descriptor operations remain temporarily kernel-resident during
  userspace migration.
