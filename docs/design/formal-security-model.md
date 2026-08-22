# Formal security model

NullStar uses formal modeling to constrain security-relevant kernel semantics
without making whole-kernel formal verification a prerequisite for development.
The model is intentionally much smaller than the Rust implementation: it records
only the state required to answer who has authority over which object and which
transitions may change that authority.

The executable specifications live under [`formal/`](../../formal/).

## Security constitution

The following invariants define the intended long-term security contract. A
formal module may cover only a subset until its dependencies are modeled, but a
later feature must not intentionally weaken an established invariant without an
explicit architecture decision.

1. **Identity is not authority.** Knowing an object ID, PID, service ID, path,
   device identity, application identity, or similar descriptive value does not
   grant access to the described resource.
2. **Object operations require authority.** A kernel-managed resource may be
   operated on only through a capability carrying the required right.
3. **Rights are monotonic.** Duplicate, replacement, and delegation may preserve
   or reduce rights but may not increase them.
4. **Independent authority survives close.** Closing one handle removes only that
   handle's authority; it does not invalidate unrelated legitimate references.
5. **Move-transfer is ownership consuming.** A successful move does not leave the
   moved authority simultaneously installed at the source.
6. **Security-sensitive failure is atomic.** An operation documented as atomic
   either commits its complete authority transition or leaves authority unchanged.
7. **Receive is all-or-nothing.** A message carrying capabilities is not consumed
   if all attached authority cannot be installed together.
8. **Stale handles do not revive.** Reusing a table slot must not make an old
   userspace handle value refer to newly installed authority.
9. **Containment only tightens.** Job membership and hierarchy-scoped policy may
   become more restrictive but may not be relaxed through handle closure,
   reparenting, fork, or delegation.
10. **Creation preserves containment.** Child processes and jobs cannot use
    creation or inheritance to escape an ancestor containment boundary.
11. **Kernel-maintained state cannot be forged.** Userspace cannot directly assert
    conditions such as peer closure, process termination, or other kernel-owned
    lifecycle signals.
12. **Provider replacement does not rebind old authority.** A capability reaching
    service generation N never silently begins reaching generation N+1.
13. **Exhaustion is explicit.** Bounded resource exhaustion returns a defined
    failure rather than dropping authority, records, or atomicity guarantees.
14. **New authority has provenance.** Except for explicit trusted creation, every
    authority grant must derive from an already authorized capability or an
    authorized broker/provider decision.

## Phase 1: capability core

`formal/CapabilityCore.tla` covers close, rights-reduced duplication, atomic
rights replacement, and the authority effect of successful move-transfer. It
tracks each token's originating object and rights so TLC can exhaustively check
phase-one monotonicity and identity invariants over a deliberately small finite
state space.

Endpoint queues, wait state, jobs, service generation, and concrete syscall
encoding remain outside this module so counterexamples stay small enough to
understand.

## Phase 2: handle generation

`formal/HandleGeneration.tla` adds the lifecycle of an opaque userspace handle
across capability-table slot reuse. A modeled handle is `<<slot, generation>>`.
Closing it records that exact handle as retired and advances the slot generation.
The final representable generation transitions to an exhausted state rather than
wrapping to an earlier value.

The model checks security-constitution invariant 8 directly: no retired handle
may ever resolve as the current live handle for any slot. It also checks that
retired generations remain strictly behind a slot's current generation, that an
exhausted slot stays closed, and that every live handle is well formed.

The live ABI now aligns with that contract while keeping the `u64` representation
opaque. The kernel combines a bounded process-local slot with a nonzero generation
allocated once for each new live handle. A same-process bounded slot-lookup
operation supports managed bootstrap and capability cleanup without making the
slot number itself authority or exposing the bit layout. Direct-child bootstrap
therefore requests a deterministic child **slot** and receives the child's actual
opaque handle.

The host-testable `kernel::capability` registry retains its per-slot generation
scheme. Generation exhaustion there now makes the slot permanently unavailable
instead of wrapping back to generation 1. The live table uses a registry-wide
monotonic allocation generation and likewise fails closed when that generation
space is exhausted. These are implementation choices beneath the same formal
non-revival property.

## Relationship to implementation

Formal actions should map to narrow implementation operations rather than whole
syscall handlers. The preferred direction is to consolidate common capability
table and rights logic so the correspondence remains obvious:

```text
formal action                 implementation concept
-------------                 ----------------------
Close                         close one handle
Duplicate                     duplicate with attenuated rights
Replace                       atomically replace with attenuated rights
MoveTransfer                  commit an ownership-consuming transfer
HandleGeneration.Open         install an opaque handle in a free slot
HandleGeneration.Close        retire that handle before slot reuse
```

The phase-two model does not claim that the concrete Rust implementation has been
formally proved to refine the TLA+ specification. The implementation is instead
aligned by construction, unit and integration tested, and kept behind the same
machine-checked architectural invariant. A later conformance layer can compare
abstract implementation snapshots against model-generated traces.

## Verification levels

NullStar distinguishes three different forms of assurance:

1. **Architecture model checking:** TLA+/TLC explores permitted state transitions
   and checks the security constitution for the modeled layer.
2. **Implementation-level checking:** selected small Rust routines may later use
   tools such as Kani or Verus for local preconditions, postconditions, and
   invariant preservation.
3. **Conformance testing:** host or QEMU tests exercise generated or hand-selected
   operation traces and compare the implementation's abstract security state with
   the formal transition model.

Passing one level is not described as passing another. In particular, a TLC
success means the finite formal model satisfied its configured invariants; it is
not a proof that the complete NullStar kernel implementation refines that model.

## Expansion order

Capability core and generation-checked handle reuse are the first two formal
layers. The next intended layer is endpoint IPC: bounded queues, move-send commit,
all-or-nothing receive, peer closure, and failure atomicity. Jobs, service
generations, and application sandboxing follow after that. MMIO, IRQ, DMA, mapped
shared memory, and richer driver authority should be added only after the
lower-level capability and containment rules have stable machine-checked models.
