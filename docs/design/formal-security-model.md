# Formal security model

NullStar uses formal modeling to constrain security-relevant kernel semantics
without making whole-kernel formal verification a prerequisite for development.
The model is intentionally much smaller than the Rust implementation: it records
only the state required to answer who has authority over which object and which
transitions may change that authority.

The executable specifications live under [`formal/`](../../formal/).

## Security constitution

The following invariants define the intended long-term security contract.  A
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

## Phase 1 boundary

The first executable model, `formal/CapabilityCore.tla`, covers only close,
rights-reduced duplication, atomic rights replacement, and the authority effect
of successful move-transfer.  It tracks each token's originating object and
rights so TLC can exhaustively check the phase-one monotonicity and identity
invariants over a deliberately small finite state space.

This phase does not model endpoint queues, wait state, jobs, service generation,
or the numeric representation of live syscall handles.  Those are separate
refinement layers so a counterexample remains small enough to understand.

## Relationship to implementation

Formal actions should map to narrow implementation operations rather than whole
syscall handlers.  The preferred direction is to consolidate common capability
table and rights logic so the correspondence remains obvious:

```text
formal action                 implementation concept
-------------                 ----------------------
Close                         close one handle
Duplicate                     duplicate with attenuated rights
Replace                       atomically replace with attenuated rights
MoveTransfer                  commit an ownership-consuming transfer
```

The first known refinement obligation is stale-handle safety.  The generic
`kernel::capability` registry already represents a handle as a slot plus
generation.  The live userspace-platform capability table currently allocates
reusable small integer handle values.  The formal model therefore does not claim
invariant 8 for the live ABI yet.  A follow-up should unify or adapt the live
representation so slot reuse advances a generation while preserving the opaque
`u64` ABI.

## Verification levels

NullStar should distinguish three different forms of assurance:

1. **Architecture model checking:** TLA+/TLC explores permitted state transitions
   and checks the security constitution for the modeled layer.
2. **Implementation-level checking:** selected small Rust routines may later use
   tools such as Kani or Verus for local preconditions, postconditions, and
   invariant preservation.
3. **Conformance testing:** host or QEMU tests exercise generated or hand-selected
   operation traces and compare the implementation's abstract security state with
   the formal transition model.

Passing one level is not described as passing another.  In particular, a TLC
success means the finite formal model satisfied its configured invariants; it is
not a proof that the complete NullStar kernel implementation refines that model.

## Expansion order

The intended expansion order is capability core, generation-checked handle reuse,
endpoint IPC, jobs, service generations, and application sandboxing.  MMIO, IRQ,
DMA, mapped shared memory, and richer driver authority should be added only after
the lower-level capability and containment rules have stable machine-checked
models.
