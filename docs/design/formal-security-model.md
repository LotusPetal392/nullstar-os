# Formal security model

NullStar uses formal modeling to constrain security-relevant kernel, service, and
selected target application-policy semantics without making whole-system formal
verification a prerequisite for development. The model is intentionally much
smaller than the implementation: it records only the state required to answer who
has authority over which object, which containment applies, and which transitions
may change those relationships.

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

`formal/JobContainment.tla` models immutable acyclic ancestry, one-way assignment,
inherited containment on fork, historical non-escape, ancestor-scoped admission,
and tightening-only process limits.

The process limit is an **admission limit**, matching the live implementation.
Tightening below the current subtree population does not eject existing members;
it prevents later assignment or fork until ancestor admission checks have
capacity again.

## Phase 5: job lifecycle

`formal/JobLifecycle.tla` starts from the containment properties supplied by
phase 4 and adds lifetime, observation, and cleanup behavior. It checks bounded
lossless completion retention, one kernel lifetime root per active member,
non-kill-on-close semantics, snapshot termination, per-job FIFO completion
drainage, empty-child-leaf retirement, and reclamation only after retirement plus
final handle closure.

Every exited process is represented exactly once as either pending or already
drained, so no modeled lifecycle transition may silently drop or duplicate an
exit record. Signal delivery mechanics, scheduler wakeups, status payloads,
process-ID reuse, and PID 1 retry policy remain outside that model.

## Phase 6: service-generation isolation

`formal/ServiceGeneration.tla` directly checks security-constitution invariant 12
and strengthens invariant 14 for service routing. It models one stable route over
multiple provider incarnations while keeping stable route authority separate from
provider authority.

The model establishes these rules:

- the retained publication generation is monotonic: a new publication must be
  strictly newer than the route's retained generation;
- withdrawal removes active availability but keeps the generation tombstone, so
  an equal or older incarnation can never become current later;
- every published provider generation is associated with a fresh ingress object;
- a stable route request is generation-neutral while it is pending. It may be
  queued under generation N and legitimately resolve after replacement to
  generation N+1 because no provider authority existed at request enqueue time;
- successful resolution binds one never-reused abstract grant to the exact
  generation and ingress object that are current when the broker completes the
  request;
- every issued grant retains provenance through the immutable
  generation-to-ingress mapping;
- if a live grant's generation differs from the currently active generation, its
  ingress object must differ from the current provider's ingress. This is the
  `OldAuthorityNeverRebinds` invariant;
- closing a provider grant releases that holder's authority but does not mutate
  its historical binding.

This model intentionally does **not** claim global revocation. An old-generation
endpoint object may remain reachable while delegated handles or queued transfers
retain it. Replacement prevents future route resolution from selecting the old
provider and prevents the new provider from receiving packets sent to the old
ingress; it does not make every old handle disappear.

Authorization policy is also outside this phase. The live broker authorizes
before consulting availability, but provider-generation isolation is independent
of which callers policy admits. Likewise, application-protocol sessions, replay
semantics, endpoint message contents, broker process identity, process restart
mechanics, and durable cross-boot generation storage are left to their own
protocol or lifecycle layers.

The live implementation already matches this abstraction. The fixed-capacity
`RouteTable` permanently associates each slot with its first route key, requires a
strictly newer `ProviderGeneration` on publish, and retains the latest generation
as a tombstone after withdrawal. `RouteBroker::connect` resolves the current
publication and passes its exact generation to the issuer. The native
`userspace::service_route` adapter duplicates a stable provider source and returns
an exact-`SEND` provider ingress handle together with that same generation. Each
provider generation creates fresh ingress endpoint objects.

These mappings are implementation-alignment evidence, not a proof that the Rust
implementation formally refines the TLA+ model.

## Phase 7: application sandbox target state

`formal/ApplicationSandbox.tla` checks the accepted application sandbox contract
at a deliberately small policy boundary. This phase is different from phases 1
through 6: it is explicitly a **target-state architecture model**. The current
kernel has many of the mechanisms the future sandbox will build on, but the full
application manager, stable signed application principal, permission database,
portal suite, and component allowlist launcher are not yet implemented.

The model uses two application identities and three process roles: one main
component and reduced child for application A, plus one main component for
application B. Four abstract resource classes are enough to exercise the security
rules: private baseline storage, a trusted portal endpoint, a broker-issued
sensitive resource, and a portal-selected document.

The model checks the following properties:

- **identity remains descriptive:** fixed application identity never creates a
  resource capability by itself;
- **bootstrap is explicit and narrow:** only trusted root launch installs the
  configured baseline, while the reduced child starts from an explicit allowlist;
- **policy input is not authority:** manifest/runtime declaration and independent
  approval may make a sensitive request eligible, but authority appears only
  after the modeled broker issues it;
- **profile ceilings do not relax:** every acquisition path is intersected with a
  fixed per-process ceiling. The reduced child and the second application's main
  component cannot self-expand those ceilings;
- **new authority has provenance:** every non-baseline live resource is traced to
  broker issuance, portal mediation, or explicit same-application delegation;
- **child creation does not clone ambient authority:** direct delegation is
  restricted to a transfer-policy-approved private resource, so the reduced child
  does not automatically inherit portal or sensitive handles from the main
  component;
- **documents are portal-mediated:** acquisition and cross-component or
  cross-application transfer of the selected document class always produce portal
  provenance. There is no direct arbitrary-process transfer action for that
  resource;
- **restricted authority cannot be self-granted:** application B's ceiling excludes
  the sensitive class, so identity, declaration, approval, or possession by some
  other process cannot manufacture that authority for it.

This phase composes rather than duplicates the lower models. It assumes the
non-escape and fork/job properties already checked by `JobContainment.tla`, the
capability-transfer properties checked by `EndpointIPC.tla`, and the broker
provenance pattern checked by `ServiceGeneration.tla`. It therefore models
mediated application component creation instead of attempting to restate the
current raw `fork` ABI.

The model does not yet include package-signing lineage, installation provenance,
permission persistence, trusted prompt UI, lease expiration/revocation, concrete
filesystem subtree semantics, network socket factories, device sessions,
background leases, or administrative entitlements. Those should become separate
implementation-backed refinements as the application runtime and desktop services
are built.

## Relationship to implementation

Formal actions should map to narrow implementation operations rather than whole
syscall handlers or complete service-manager/application-manager loops:

```text
formal action                            implementation concept
-------------                            ----------------------
Close                                    close one capability handle
Duplicate                                duplicate with attenuated rights
Replace                                  atomically replace with attenuated rights
MoveTransfer                             commit an ownership-consuming transfer
HandleGeneration.Open                    install an opaque handle in a free slot
HandleGeneration.Close                   retire the opaque handle before slot reuse
EndpointIPC.PlainSend                    append a message without authority
EndpointIPC.CopySend                     append a rights-checked copied capability
EndpointIPC.MoveSend                     consume validated sources and append one message
EndpointIPC.Receive                      install all attachments and remove the FIFO head
JobContainment.CreateChildJob            create one immutable child-parent edge
JobContainment.AssignDirectChild         assign an uncontained live direct child
JobContainment.ForkProcess               inherit the parent's existing job containment
JobContainment.TightenLimit              lower or retain subtree admission policy
JobLifecycle.Admit                       accept already-authorized job membership
JobLifecycle.Exit                        record completion, then release the member root
JobLifecycle.TerminateSnapshot           request termination of the current subtree snapshot
JobLifecycle.DrainSubtree                consume one retained subtree completion
JobLifecycle.CloseFinalHandle            close authority without implicit member termination
JobLifecycle.RetireChild                 retire and detach an empty child leaf
JobLifecycle.ReclaimRetiredChild         reclaim a detached retired child after final close
ServiceGeneration.Publish                publish a newer generation with fresh ingress authority
ServiceGeneration.Withdraw               remove active availability but retain its tombstone
ServiceGeneration.BeginRequest           queue a generation-neutral stable-route request
ServiceGeneration.CompleteRequestSuccess issue exact current-generation provider authority
ServiceGeneration.CompleteRequestUnavailable complete without provider authority
ServiceGeneration.CloseGrant             close one provider grant without rebinding it
ApplicationSandbox.LaunchRoot            install trusted narrow launch baseline
ApplicationSandbox.SpawnChild            construct reduced component from explicit allowlist
ApplicationSandbox.DeclareSensitive      record permission metadata without authority
ApplicationSandbox.ApproveSensitive      record policy/user approval without authority
ApplicationSandbox.BrokerGrant           issue eligible approved provider authority
ApplicationSandbox.PortalAcquire         issue one exact user-selected scoped resource
ApplicationSandbox.PortalTransfer        mediate scoped transfer to another component/application
ApplicationSandbox.DelegateSameApplication directly delegate only an allowed reduced resource
ApplicationSandbox.DropAuthority         close live authority while identity/policy remain
```

For phase seven, these mappings describe the accepted future application-manager
architecture rather than a current end-to-end implementation. Existing process-
local capabilities, direct-child bootstrap, jobs, rights attenuation, and service
routing provide the lower-level mechanisms that architecture will use.

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
not a proof that the complete NullStar kernel, service stack, or future
application runtime refines that model.

## Expansion order

The initial formal-security sequence is:

1. capability core;
2. generation-checked handles;
3. endpoint transfer atomicity;
4. job containment and non-escape;
5. job lifecycle, termination snapshots, drainage, and retirement;
6. service-generation isolation;
7. application sandbox target-state containment.

After phase seven, formalization should track implementation milestones rather
than expanding this sequence indefinitely. Candidate follow-ons include endpoint
peer-closure and wakeup lifecycle, mapped shared-memory protection and W^X,
application-manager trace conformance after that runtime exists, and later
MMIO/IRQ/DMA authority when userspace driver work reaches those primitives.
