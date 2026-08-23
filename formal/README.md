# NullStar formal security models

This directory contains small executable specifications for security-relevant
NullStar kernel semantics. These models are intentionally not a formal model of
the whole operating system. They isolate authority and lifecycle transitions so
important invariants can be machine-checked before those semantics spread across
more kernel objects and services.

## Phase 1: capability core

`CapabilityCore.tla` models only the authority effects of:

- closing a capability;
- rights-reduced duplication;
- atomic rights replacement;
- successful move-transfer between processes.

The model deliberately abstracts away syscall numbers, numeric handle encoding,
kernel pointers, object payloads, scheduling, endpoint queues, and failure codes.
A failed operation is represented by stuttering: there is no partial state
transition.

The first checked invariants are:

- every live authority token is well typed;
- live capabilities never carry an empty rights set;
- rights never exceed the authority from which a token originated;
- capability operations never change the referenced object identity;
- live authority remains bounded by the finite handle universe.

## Phase 2: handle generation

`HandleGeneration.tla` refines capability identity across table-slot reuse. A
modeled userspace handle is the pair `<<slot, generation>>`. Closing a handle
retires that exact pair and advances the slot generation. When the final modeled
generation is consumed, the slot enters an exhausted state rather than wrapping.

The phase-two invariants check that:

- a retired opaque handle never resolves as a live handle again;
- a slot's current generation always remains beyond every retired generation for
  that slot, unless the generation space has been exhausted;
- an exhausted slot cannot become active again;
- every live handle contains a valid nonzero generation.

The live ABI follows the same security rule while keeping `u64` handles opaque.
The current implementation uses a bounded slot plus a registry-wide nonzero
generation allocated for each new live handle. Userspace may ask the kernel which
opaque handle is currently installed at one of its own bounded table slots, but
the slot number itself is not authority and the ABI does not expose the handle
bit layout.

## Phase 3: endpoint IPC atomicity

`EndpointIPC.tla` models the security-sensitive portion of bounded endpoint IPC:

- FIFO enqueue and dequeue;
- plain messages without transferred authority;
- copy-send, where the sender retains its source authority;
- move-send, where successful enqueue consumes the sender's source authority;
- all-or-nothing receive of attached capabilities;
- bounded queue and receiver handle capacity;
- explicit authority provenance across copy, move, queueing, and receive.

Byte payload contents, peer closure, scheduler blocking, wakeups, deadlines,
event ports, and concrete handle encoding remain outside this refinement. Queue
full and insufficient receive-capacity failures are modeled as stuttering: no
partial authority or queue mutation is permitted.

The phase-three invariants check that:

- the endpoint queue never exceeds its configured bound;
- successful receives preserve FIFO order;
- move-send cannot leave moved source authority installed at the sender;
- receive capability accounting is exact and therefore cannot partially install
  an attached set;
- every live unit of authority derives either from the initial source or an
  explicitly recorded successful copy-send.

The current runtime probe already exercises the corresponding live-kernel edge
cases: queue-full move-send failure retains its source handles, successful
move-send invalidates the moved source handles, duplicate move sources are
rejected, and insufficient receive handle capacity leaves the queued message
available for a later successful receive.

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
   all-or-nothing receive, and failure atomicity. This phase intentionally leaves
   peer closure and blocking/wakeup semantics for a later IPC refinement.
4. **Jobs** — immutable hierarchy, non-relaxable membership, fork inheritance,
   subtree termination, and tightening-only policy.
5. **ServiceGeneration** — fresh provider ingress and the guarantee that authority
   for generation N never silently rebinds to generation N+1.
6. **ApplicationSandbox** — explicit bootstrap authority, broker-issued grants,
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

The host-testable generic `kernel::capability` registry keeps per-slot generation
state. The live userspace-platform table uses globally unique per-allocation
generations combined with bounded process-local slots. Both implementations obey
the phase-two property: closing and later reusing a slot cannot recreate a stale
opaque handle, and generation exhaustion fails closed instead of wrapping.

For endpoint IPC, the live kernel performs source validation and queue-capacity
checks before removing move sources. Receive checks byte/handle capacity and
installs the complete attached capability set before removing the FIFO head. The
formal model captures the resulting atomic state transition rather than the
individual Rust statements used to implement it.

The guiding rule is that the formal model should stay smaller than the
implementation. When a new feature cannot be described without pulling large
amounts of unrelated kernel state into a security model, that is a signal to
reconsider the abstraction boundary rather than model the entire kernel.
