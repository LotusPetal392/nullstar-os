# NullStar formal security models

This directory contains small executable specifications for security-relevant
NullStar kernel semantics. These models are intentionally not a formal model of
the whole operating system. They isolate authority and lifecycle transitions so
important invariants can be machine-checked before those semantics spread across
more kernel objects and services.

## Phase 1: capability core

`CapabilityCore.tla` models closing, rights-reduced duplication, atomic rights
replacement, and successful move-transfer between processes. It checks type
safety, nonempty authority, rights monotonicity, stable object identity, and
bounded live authority.

## Phase 2: handle generation

`HandleGeneration.tla` refines capability identity across table-slot reuse. It
checks that retired opaque handles never revive, generations never move backward,
exhausted slots remain closed, and every live handle is well formed.

## Phase 3: endpoint IPC atomicity

`EndpointIPC.tla` models FIFO enqueue/dequeue, plain send, copy-send,
ownership-consuming move-send, all-or-nothing receive, bounded queue/receiver
capacity, and explicit authority provenance. Queue-full and insufficient receive
capacity failures are stuttering transitions with no partial authority mutation.

## Phase 4: job containment

`JobContainment.tla` models immutable job ancestry, one-way direct-child
assignment, contained fork inheritance, historical non-escape, ancestor-scoped
admission, and tightening-only process limits. Process limits are admission
policy: tightening below the current population blocks future admissions rather
than retroactively ejecting existing members.

## Phase 5: job lifecycle

`JobLifecycle.tla` refines the already-established containment model with the
security-sensitive lifetime and observation rules used by live jobs:

- active members and unconsumed completion records share one bounded capacity;
- every active member contributes one kernel lifetime root, so final userspace
  handle closure is not implicit kill-on-close and cannot release a live job;
- process exit moves one member into that job's retained completion FIFO before
  the corresponding kernel root disappears;
- subtree termination records the current member snapshot rather than creating a
  permanent terminating/sealed state;
- subtree drainage may select among descendant jobs, while each individual job's
  completion queue remains FIFO;
- every exited process is accounted for exactly once as either pending or already
  drained, so the modeled lifecycle cannot silently drop or duplicate an exit;
- only an empty child leaf can retire, retirement detaches the hierarchy edge and
  makes the child inert, and reclamation requires retirement plus final handle
  closure.

The model deliberately does not claim a global ordering between completions from
different jobs in a subtree. It also leaves scheduler signal delivery, status
payload contents, generic collection of abandoned non-retired roots, process-ID
reuse, and PID 1 retry/budget policy outside this layer.

These are architecture properties and implementation-alignment checks, not a
claim that the complete kernel has been formally verified.

## Running TLC

The models are compatible with the command-line TLA+ tools. With Java 11 or newer
and `tla2tools.jar` available:

```sh
java -XX:+UseParallelGC -cp /path/to/tla2tools.jar \
  tlc2.TLC -deadlock \
  -config formal/CapabilityCore.cfg formal/CapabilityCore.tla

java -XX:+UseParallelGC -cp /path/to/tla2tools.jar \
  tlc2.TLC -deadlock \
  -config formal/HandleGeneration.cfg formal/HandleGeneration.tla

java -XX:+UseParallelGC -cp /path/to/tla2tools.jar \
  tlc2.TLC -deadlock \
  -config formal/EndpointIPC.cfg formal/EndpointIPC.tla

java -XX:+UseParallelGC -cp /path/to/tla2tools.jar \
  tlc2.TLC -deadlock \
  -config formal/JobContainment.cfg formal/JobContainment.tla

java -XX:+UseParallelGC -cp /path/to/tla2tools.jar \
  tlc2.TLC -deadlock \
  -config formal/JobLifecycle.cfg formal/JobLifecycle.tla
```

`-deadlock` disables TLC's deadlock error because intentional terminal states are
allowed in these bounded safety models. The configured safety invariants remain
checked over the complete reachable state graph.

The repository CI pins TLA+ Tools 1.7.4 for repeatability and checks all modules.

## Refinement plan

The model is intentionally layered. Later modules should add semantics only when
the lower layer is stable:

1. **CapabilityCore** — authority ownership, attenuation, replacement, close, and
   move-transfer. Implemented and machine-checked.
2. **HandleGeneration** — slot reuse, generation checks, explicit exhaustion, and
   stale-handle non-revival. Implemented and machine-checked.
3. **EndpointIPC** — bounded FIFO queues, copy/move authority transfer,
   all-or-nothing receive, and failure atomicity. Implemented and machine-checked.
4. **JobContainment** — immutable hierarchy, one-way membership, fork inheritance,
   ancestor-scoped admission, and tightening-only process limits. Implemented and
   machine-checked.
5. **JobLifecycle** — member rooting, snapshot termination, completion retention
   and drainage, empty-leaf retirement, and post-retirement reclamation.
6. **ServiceGeneration** — fresh provider ingress and the guarantee that authority
   for generation N never silently rebinds to generation N+1.
7. **ApplicationSandbox** — explicit bootstrap authority, broker-issued grants,
   delegation limits, and sandbox containment.

## Implementation relationship

The models specify permitted security transitions rather than literal Rust data
structures. The intended mapping remains small and explicit:

| Formal action | Kernel concept |
| --- | --- |
| `Close` | capability close |
| `Duplicate` | rights-reduced duplicate |
| `Replace` | atomic rights replacement |
| `MoveTransfer` | successful ownership-consuming transfer |
| `Open` in `HandleGeneration` | install a new opaque handle in a free slot |
| `Close` in `HandleGeneration` | retire the opaque handle before slot reuse |
| `PlainSend` | append a message with no attached capability |
| `CopySend` | append transferred authority while retaining the source |
| `MoveSend` | validate capacity, consume sources, then append one message |
| `Receive` | reserve/install every attachment and dequeue exactly the FIFO head |
| `CreateChildJob` | create a child job with a permanent parent edge |
| `AssignDirectChild` | assign an uncontained live direct child once |
| `ForkProcess` | inherit the parent's current job and ancestor containment |
| `TightenLimit` | lower or retain a job's subtree process-admission limit |
| `JobLifecycle.Admit` | accepted job membership after containment checks |
| `JobLifecycle.Exit` | member-to-retained-completion transition and root release |
| `JobLifecycle.TerminateSnapshot` | signal the current selected subtree membership |
| `JobLifecycle.DrainSubtree` | consume one retained descendant completion |
| `JobLifecycle.CloseFinalHandle` | surrender userspace authority without killing members |
| `JobLifecycle.RetireChild` | retire and detach an empty child leaf |
| `JobLifecycle.ReclaimRetiredChild` | reclaim a detached retired child after final close |

The live job implementation keeps hierarchy, membership, completion, and
retirement state in `kernel::job::State`. Live membership adds kernel capability
roots, exit records completion before removing the matching root, and pending
completions share the membership bound so pressure rejects new membership instead
of dropping exit information. `JOB_TERMINATE` signals the current subtree
snapshot, while `JOB_TRY_WAIT` drains retained completion records. Retirement is
restricted to an empty child leaf and is permanent.

The guiding rule is that the formal model should stay smaller than the
implementation. When a new feature cannot be described without pulling large
amounts of unrelated kernel state into a security model, that is a signal to
reconsider the abstraction boundary rather than model the entire kernel.
