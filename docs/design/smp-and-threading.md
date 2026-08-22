# SMP and multithreading evolution

## Status

**Accepted direction:** NullStar treats threads as the schedulable execution context and
SMP as a kernel foundation that should be established before the service-manager,
networking, and desktop layers depend on parallel execution.

The exact scheduler policy, queue sizing, CPU-budget rules, and realtime admission
parameters remain **tentative** until they are measured on real workloads.

This document turns the existing scheduler direction into an explicit implementation
milestone. It complements [Kernel, IPC, and scheduling direction](kernel-ipc-scheduling.md)
and the [Architecture roadmap](roadmap.md).

## Why this is a near-term milestone

The current kernel brings application processors online with CPU-local timer interrupts,
preemption depth, kernel contexts, and round-robin scheduler lanes. Affinity-safe live
migration and a bounded periodic coordinator can rebalance kernel probe workloads between
APs. The bootstrap userspace scheduler remains separate, so the next integration step is
to back these live contexts with ordinary process/thread identities and lifecycle state.

The process model already distinguishes a process from a thread: a process is a security
and resource container, while a thread is a schedulable execution context. The ABI must
continue to support multiple threads per process even while early userspace remains mostly
single-threaded.

The allocation-free blocking-work coordinator already models bounded worker admission,
but actual parallel, preemptible workers are intentionally deferred until the thread,
address-space, and job-resource substrate exists.

Consequently, SMP and multithreading are not a late desktop feature. They are a kernel
foundation that should land before the general service-manager and desktop milestones.

## Milestone sequence

### 1. Thread foundation

Establish an explicit kernel thread object and lifecycle.

- Define thread identity, ownership, and lifetime rules.
- Separate process/address-space ownership from schedulable thread state.
- Add kernel and user execution contexts with independently managed stacks.
- Define runnable, running, blocked, sleeping, stopping, and exited states.
- Add thread creation, exit, join/detach, and safe reclamation semantics.
- Preserve capability and job containment rules across thread creation.
- Define thread-local storage support for userspace runtimes.
- Ensure `fork`/`exec` semantics remain explicit about which thread survives an image replacement.
- Keep the ABI independent of a one-thread-per-process assumption.

### 2. SMP bring-up

Bring secondary processors online without changing scheduler policy yet.

- Enumerate available CPUs from the platform's ACPI topology.
- Start application processors and establish per-CPU kernel state.
- Give each CPU its own interrupt, scheduler, and idle context.
- Make GDT/TSS and interrupt state safe for concurrent CPUs.
- Establish local-APIC/interrupt routing and inter-processor interrupts.
- Provide a safe CPU startup, shutdown, and failure path.
- Add deterministic QEMU coverage with at least two active CPUs.

### 3. SMP scheduler

Move scheduling from a single global execution context to scalable per-CPU scheduling.

- Create per-CPU run queues.
- Track the current thread and preemption state per CPU.
- Implement CPU affinity and an explicit default placement policy.
- Add bounded idle-CPU work stealing and load balancing.
- Keep interactive wakeup preference and existing scheduling classes intact.
- Make timer/preemption decisions CPU-local where possible.
- Define migration rules for runnable and blocked threads.
- Avoid a permanently contended global scheduler queue.

The initial design should favor predictable behavior and bounded synchronization over
maximum throughput. Cache-aware placement can be refined after the basic SMP scheduler
is correct and measurable.

The implemented AP scheduler now has per-CPU run queues, explicit affinity, live migration,
deterministic rebalance planning, and a single timer-driven coordinator that evaluates the
policy at a fixed interval. Migration planning runs outside the triggering CPU's scheduler
lock, only one migration may be active, and completion is recorded when the destination
scheduler dispatches the transferred context. Repeated passes converge larger imbalances
one affinity-safe move at a time, with checks, requests, completions, and delivery failures
retained as bounded counters. Blocking removes a thread from runnable load while preserving its
bounded assignment, ownership, and affinity state. Wakeup reuses the previous CPU unless an
eligible CPU is meaningfully less loaded, and affinity changes may move blocked ownership without
making the thread runnable. Host tests cover these transitions and rebalance exclusion; the
three-CPU QEMU path blocks a live context on one AP, transfers it during wakeup, and verifies its
dispatch on another AP. An architecture-neutral `SmpExecution` coordinator now binds
`ProcessTable` lifecycle transitions to SMP admission, dispatch, blocking, wakeup, migration,
rebalance, and exit effects. It owns both models, prevalidates fallible operations, and retains
enough source/destination transition detail to keep running state synchronized across CPUs. The
live AP context store now registers its reserved probe identities in one bounded lifecycle process
per AP and routes timer, migration, rebalance, block, and wake operations through the coordinator.
A kernel-wide execution runtime now owns that coordinator independently of the x86 AP context store,
initializes CPU 0 first, distinguishes 64-slot topology capacity from the actual online mask, and
publishes each CPU's context group only after atomic admission succeeds. The shared bounds are 128
processes and 256 threads/assignments; host coverage admits the full 64-CPU topology, all 131 AP
probe contexts, three CPU 0 smoke-test contexts, and 64 low-range userspace identities for a total
of 128 processes and 198 threads without colliding with the reserved architecture namespace. Boot
snapshots expose the online mask, registered CPU count, and lifecycle process/thread/running counts,
while dispatch verification checks the authoritative thread state.
The CPU 0 context store now registers its bootstrap and smoke-test kernel tasks as one architecture
context group, transactionally admits each userspace compatibility process/thread with shared
parentage, and delegates timer, yield, block, wake, stop, continue, exit, and reap transitions to
the same coordinator. A stopped blocked thread retains its deferred resume state, so an event can
make it ready without dispatching it before a continue operation. Register contexts and address
spaces remain architecture-owned. General event/wait-queue objects still need to invoke the shared
wake path directly, and the userspace ABI still needs true multi-thread creation.

### 4. Synchronization and concurrent execution

Make the kernel and userspace synchronization model safe under true parallel execution.

- Audit every shared scheduler/kernel structure for concurrent access.
- Restrict spinlocks to short, non-sleeping critical sections.
- Establish mutexes, wait queues, events, and condition-style wakeups for longer waits.
- Add the planned futex-like userspace wait/wake primitive.
- Integrate bounded priority inheritance for kernel locks and synchronous IPC.
- Verify cancellation, timeout, and wakeup races under concurrent execution.
- Ensure capability-table, job, process, and IPC operations remain race-safe.

### 5. CPU accounting and resource policy

Expose useful processor accounting and establish the substrate for future CPU limits.

- Track runtime per thread.
- Aggregate runtime to processes and jobs.
- Track user/kernel CPU time separately where practical.
- Track per-CPU utilization for diagnostics and the future system monitor.
- Add CPU affinity and scheduling-class inspection.
- Add job-level CPU budgets and limits after the basic accounting model is stable.
- Trace wakeup latency, runtime, migration, budget exhaustion, and deadline misses.

This is also the foundation for the system-monitor view of per-core utilization discussed
elsewhere in the project.

### 6. Acceptance and stress testing

SMP must be treated as a concurrency milestone, not merely a boot-time CPU-count feature.

Acceptance should include:

- booting and scheduling correctly with one and multiple CPUs;
- multiple runnable threads executing concurrently in one process;
- multiple processes executing concurrently;
- thread creation and teardown under load;
- CPU affinity and migration tests;
- blocking/wakeup races across different CPUs;
- concurrent IPC and capability operations;
- job termination while multiple descendant threads are active;
- timer and preemption behavior across CPUs;
- scheduler/accounting consistency under sustained load;
- deterministic QEMU regression coverage for the core SMP paths.

Race-oriented host tests should cover policy and state-machine logic wherever possible,
while QEMU tests cover the real interrupt, context-switch, and multi-CPU behavior.

## Relationship to the broader roadmap

The intended high-level order is:

1. NullFS job containment and current kernel-object/process foundations.
2. **Thread foundation and SMP/multithreading.**
3. Service-manager and early-userspace lifecycle work.
4. VFS and persistent system services.
5. Device, networking, firewall, and `netctl` infrastructure.
6. Magnetar package/deployment infrastructure.
7. Nova, compositor, and desktop services.
8. Live ISO and graphical installer.

Service and desktop components may use cooperative or logically bounded worker abstractions
before this milestone is complete, but production parallel/preemptible worker execution
should depend on the thread/SMP substrate rather than creating an independent concurrency
model.

## Non-goals for the initial SMP milestone

The first implementation does not need to solve every advanced CPU-topology problem.
NUMA, heterogeneous-core scheduling, CPU hotplug, advanced power management, and highly
specialized realtime placement can remain later work. The initial goal is a correct,
testable, capability-safe multi-threaded kernel that can make effective use of multiple
homogeneous CPUs.
