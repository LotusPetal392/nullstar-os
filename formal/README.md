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

The current runtime probe exercises the corresponding live-kernel edge cases:
queue-full move-send failure retains sources, successful move invalidates sources,
duplicate move sources are rejected, and insufficient receive handle capacity
leaves the queued message available for retry.

## Phase 4: job containment

`JobContainment.tla` models the security-sensitive containment rules that apply
before job lifecycle and exit-observation details:

- a child job is created once beneath an already-live parent and cannot be
  reparented;
- each live job carries an immutable full ancestor closure;
- a live direct child may move only from no job into one job;
- a contained `fork` inherits the parent's exact current job;
- historical containment requirements never disappear;
- process admission is checked against the target job and every ancestor's
  subtree process limit;
- process limits may stay equal or tighten, but cannot be relaxed.

A process limit is an admission policy, not retroactive eviction. The model
therefore permits a limit to be tightened below the current subtree population;
subsequent assignment or fork is blocked until every ancestor again has capacity.

Termination, completion records, drainage, retirement, last-handle lifetime,
scheduler behavior, and service cleanup are deliberately deferred to a later
`JobLifecycle` refinement.

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
   ancestor-scoped admission, and tightening-only process limits.
5. **JobLifecycle** — subtree termination, completion retention/drainage,
   retirement, and containment-preserving object lifetime.
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

The live job implementation keeps hierarchy and membership state in
`kernel::job::State`. Parent assignment is one-shot, process limits reject
relaxation, and the capability registry checks subtree population against every
ancestor before accepting new membership. The formal model captures those
security semantics without pulling completion queues or termination machinery
into this layer.

The guiding rule is that the formal model should stay smaller than the
implementation. When a new feature cannot be described without pulling large
amounts of unrelated kernel state into a security model, that is a signal to
reconsider the abstraction boundary rather than model the entire kernel.
