# NullStar formal security models

This directory contains small executable specifications for security-relevant
NullStar kernel semantics.  These models are intentionally not a formal model of
the whole operating system.  They isolate authority and lifecycle transitions so
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

These are architecture properties, not claims that the complete kernel has been
formally verified.

## Running TLC

The model is compatible with the command-line TLA+ tools.  With Java 11 or newer
and `tla2tools.jar` available:

```sh
java -XX:+UseParallelGC -cp /path/to/tla2tools.jar \
  tlc2.TLC -deadlock \
  -config formal/CapabilityCore.cfg formal/CapabilityCore.tla
```

`-deadlock` disables TLC's deadlock error because closing the final modeled
capability is an intentional terminal state.  The safety invariants remain
checked over the complete reachable state graph.

The repository CI pins TLA+ Tools 1.7.4 for repeatability.

## Refinement plan

The model is intentionally layered.  Later modules should add semantics only when
the lower layer is stable:

1. **CapabilityCore** — authority ownership, attenuation, replacement, close, and
   move-transfer.
2. **HandleGeneration** — slot reuse, generation checks, and the guarantee that a
   stale userspace handle cannot become authority over a later object.
3. **EndpointIPC** — bounded queues, atomic move-send, all-or-nothing receive,
   peer closure, and failure atomicity.
4. **Jobs** — immutable hierarchy, non-relaxable membership, fork inheritance,
   subtree termination, and tightening-only policy.
5. **ServiceGeneration** — fresh provider ingress and the guarantee that authority
   for generation N never silently rebinds to generation N+1.
6. **ApplicationSandbox** — explicit bootstrap authority, broker-issued grants,
   delegation limits, and sandbox containment.

## Implementation relationship

The model is a specification of permitted authority transitions, not a literal
translation of Rust data structures.  The intended mapping is small and explicit:

| Formal action | Kernel concept |
| --- | --- |
| `Close` | capability close |
| `Duplicate` | rights-reduced duplicate |
| `Replace` | atomic rights replacement |
| `MoveTransfer` | successful ownership-consuming transfer |

The current generic `kernel::capability` model already uses generation-checked
slot handles, while the live userspace-platform capability table still uses
reusable small integer handles.  Phase 1 does **not** silently claim those are
already equivalent.  `HandleGeneration` is the next refinement step and should
be completed before NullStar claims the stale-handle invariant for the live ABI.

The guiding rule is that the formal model should stay smaller than the
implementation.  When a new feature cannot be described without pulling large
amounts of unrelated kernel state into a security model, that is a signal to
reconsider the abstraction boundary rather than model the entire kernel.
