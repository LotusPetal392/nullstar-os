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

## Phase 2: handle generation

`formal/HandleGeneration.tla` adds the lifecycle of an opaque userspace handle
across capability-table slot reuse. The model checks that retired handles cannot
revive, generation exhaustion fails closed, and live handles remain well formed.

## Phase 3: endpoint IPC atomicity

`formal/EndpointIPC.tla` adds bounded FIFO authority transitions for plain send,
copy-send, move-send, and all-or-nothing receive. Full-queue and insufficient
receive-capacity attempts are modeled as stuttering failures so they cannot
partially mutate authority or queue state.

## Phase 4: job containment

`formal/JobContainment.tla` models the containment rules that must remain true
independently of job termination and exit-observation machinery. It directly
refines security-constitution invariants 9 and 10.

The modeled rules are:

- the root job remains rooted and every child job is created once beneath an
  already-live parent;
- a live job's full ancestor closure is immutable and acyclic;
- a live direct child may transition only from no job to one job; there is no
  modeled move or unassign operation after containment is established;
- a contained fork inherits the parent's exact current job and therefore its
  complete ancestor containment;
- historical containment requirements are remembered explicitly, so any later
  state that provides less containment violates `ContainmentNeverRelaxes`;
- every new assignment or contained fork must pass the target job's process
  admission check and every ancestor's check over its complete subtree;
- a job's process limit may stay equal or decrease, never increase.

The process limit is deliberately modeled as an **admission limit**, matching the
live implementation. Tightening a limit below the number of processes already in
the subtree does not retroactively eject them; instead it prevents later
assignment or fork until all ancestor checks have capacity again.

This phase does not model process exit, completion-record retention, job
termination, drainage, retirement, final-handle closure, or kernel-root lifetime.
Those are the boundary of a later `JobLifecycle` refinement. Keeping them separate
means a containment counterexample does not need to include unrelated cleanup and
observation state.

The live implementation already has the corresponding structural rules:
`kernel::job::State` accepts its parent only once and rejects process-limit
relaxation, while the capability registry checks admission through the complete
ancestor chain. Existing host and runtime tests exercise hierarchy, tightening,
assignment, and fork/job behavior; this phase formalizes the architecture rather
than changing the syscall ABI.

## Relationship to implementation

Formal actions should map to narrow implementation operations rather than whole
syscall handlers:

```text
formal action                       implementation concept
-------------                       ----------------------
Close                               close one capability handle
Duplicate                           duplicate with attenuated rights
Replace                             atomically replace with attenuated rights
MoveTransfer                        commit an ownership-consuming transfer
HandleGeneration.Open               install an opaque handle in a free slot
HandleGeneration.Close              retire the opaque handle before slot reuse
EndpointIPC.PlainSend               append a message without authority
EndpointIPC.CopySend                append a rights-checked copied capability
EndpointIPC.MoveSend                consume validated sources and append one message
EndpointIPC.Receive                 install all attachments and remove the FIFO head
JobContainment.CreateChildJob       create one immutable child-parent edge
JobContainment.AssignDirectChild    assign an uncontained live direct child
JobContainment.ForkProcess          inherit the parent's existing job containment
JobContainment.TightenLimit         lower or retain subtree admission policy
```

The formal models do not claim that the concrete Rust implementation has been
formally proved to refine the TLA+ specifications. The implementation is instead
aligned by construction, unit and integration tested, and kept behind the same
machine-checked architectural invariants. A later conformance layer can compare
abstract implementation snapshots against model-generated traces.

## Verification levels

NullStar distinguishes three forms of assurance:

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

The formal layers are intentionally incremental:

1. capability core;
2. generation-checked handles;
3. endpoint transfer atomicity;
4. job containment and non-escape;
5. job lifecycle, termination, and drainage;
6. service-generation isolation;
7. application sandbox containment.

A later endpoint refinement can separately add peer closure and blocking/wakeup
lifecycle semantics. MMIO, IRQ, DMA, mapped shared memory, and richer driver
authority should be added only after the lower-level capability and containment
rules have stable machine-checked models.
