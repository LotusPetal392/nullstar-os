# Formal security model

NullStar uses formal modeling to constrain security-relevant kernel semantics
without making whole-kernel formal verification a prerequisite for development.
The model is intentionally much smaller than the Rust implementation: it records
only the state required to answer who has authority over which object and which
transitions may change that authority or containment.

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
checks rights monotonicity, stable object identity, bounded authority, and failure
atomicity over a deliberately small state machine.

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
independently of job termination and exit-observation machinery. It checks
immutable acyclic ancestry, one-way assignment, inherited containment on fork,
historical non-escape, ancestor-scoped admission, and tightening-only process
limits.

The process limit is deliberately modeled as an **admission limit**, matching the
live implementation. Tightening a limit below the number of processes already in
the subtree does not retroactively eject them; instead it prevents later
assignment or fork until all ancestor checks have capacity again.

## Phase 5: job lifecycle

`formal/JobLifecycle.tla` starts from a fixed root/child hierarchy whose
containment properties are supplied by phase 4 and adds lifetime, observation,
and cleanup behavior.

The model checks the following properties:

- active members and unconsumed exit records share a fixed capacity, so pressure
  prevents later admission instead of dropping an exit record;
- each active member contributes one modeled kernel lifetime root and process
  exit removes that root only in the same abstract transition that appends the
  retained completion record;
- closing the final userspace handle does not alter membership, completion state,
  or member roots, matching the kernel's explicit cleanup rather than
  kill-on-close semantics;
- every exited process is represented exactly once as either a pending completion
  or an already drained completion, so no modeled exit can disappear or be
  duplicated;
- each individual job's completion queue is FIFO. Selection between separate
  descendant jobs during subtree drainage is intentionally abstract because no
  global cross-job completion ordering is part of this layer;
- `JOB_TERMINATE` is modeled as a request against the current subtree member
  snapshot. It does not create a sticky terminating state or automatically cover
  later admissions;
- retirement is permitted only for the empty child leaf, permanently detaches its
  hierarchy edge, and leaves the retired object inert;
- reclamation of that child requires prior retirement and final handle closure,
  after membership and completion state are already empty.

This phase directly strengthens the practical consequences of constitution
invariants 9, 11, and 13: handle closure cannot dissolve active containment,
process completion is kernel-owned state, and bounded pressure cannot be resolved
by silently discarding exit information.

The model intentionally leaves signal-9 delivery mechanics, scheduler wakeups,
completion status payloads, process-ID reuse, generic collection of inaccessible
non-retired root jobs, and PID 1 cleanup budgets/retry loops outside its state
space. Those details are implementation behavior rather than necessary state for
the lifecycle safety properties above.

The live implementation aligns with this abstraction. `kernel::job::State`
reserves one bounded slot across each live member or unconsumed completion,
records completions FIFO, restricts retirement to an empty child leaf, and makes
retirement permanent. The capability registry adds a kernel object root when a
process becomes a member and removes the matching root after recording that
process's completion. Subtree wait drains retained records, while
`JOB_TERMINATE` signals the current subtree membership snapshot. These mappings
are implementation-alignment evidence, not a proof that the Rust implementation
formally refines the TLA+ module.

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
JobLifecycle.Admit                  accept already-authorized job membership
JobLifecycle.Exit                   record completion, then release the member root
JobLifecycle.TerminateSnapshot      request termination of the current subtree snapshot
JobLifecycle.DrainSubtree           consume one retained subtree completion
JobLifecycle.CloseFinalHandle       close authority without implicit member termination
JobLifecycle.RetireChild            retire and detach an empty child leaf
JobLifecycle.ReclaimRetiredChild    reclaim a detached retired child after final close
```

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
5. job lifecycle, termination snapshots, drainage, and retirement;
6. service-generation isolation;
7. application sandbox containment.

A later endpoint refinement can separately add peer closure and blocking/wakeup
lifecycle semantics. MMIO, IRQ, DMA, mapped shared memory, and richer driver
authority should be added only after the lower-level capability and containment
rules have stable machine-checked models.
