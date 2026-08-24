# NullStar formal security models

This directory contains small executable specifications for security-relevant
NullStar kernel, userspace-service, and target application-policy semantics. These
models are intentionally not a formal model of the whole operating system. They
isolate authority and lifecycle transitions so important invariants can be
machine-checked before those semantics spread across more kernel objects and
services.

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

## Phase 6: service-generation isolation

`ServiceGeneration.tla` models the boundary between stable route authority and
provider-generation-specific authority:

- provider publication generations only move forward;
- withdrawal retains a generation tombstone, so an equal or older generation
  cannot later become current again;
- every provider generation consumes a fresh ingress object;
- stable route requests do not acquire provider authority when they are queued;
- a request may therefore cross a provider replacement and resolve to whichever
  generation is current when the broker completes it;
- a successful response binds one provider grant to the exact current generation
  and ingress object;
- later replacement cannot mutate that grant into authority for the new ingress;
- closing a grant removes only that holder's live authority and does not alter the
  historical generation/object binding used by the model.

The central phase-six invariant is `OldAuthorityNeverRebinds`: if an issued live
provider grant belongs to a generation different from the current provider, its
ingress object must also differ from the current provider's object.

The model deliberately does not claim global revocation. Old-generation endpoint
objects can remain alive while clients or queued transfers retain them. It also
leaves application-protocol session semantics, authorization policy, broker PID
identity, endpoint queue contents, process restart mechanics, and cross-boot
generation persistence outside this layer.

## Phase 7: application sandbox target state

`ApplicationSandbox.tla` composes the earlier authority and containment rules at
the accepted future application-manager boundary. Unlike phases 1 through 6, it
is explicitly a **target-state architecture model**, not an implementation-
alignment claim about the current launcher.

The finite scenario contains two application identities, one reduced child
component, a narrow trusted launch baseline, one sensitive broker-issued resource,
one portal-selected document class, and one directly delegable private resource.
It checks that:

- application identity is descriptive and never creates authority by itself;
- trusted root launch installs only a fixed baseline allowlist;
- manifest/runtime declaration and policy approval remain separate from actual
  broker issuance;
- every live capability is inside the immutable process/profile ceiling;
- every non-baseline authority has explicit broker, portal, or parent-delegation
  provenance;
- a reduced child starts from an explicit allowlist instead of cloning the main
  component's capability table;
- direct parent delegation is same-application and restricted to resources whose
  transfer policy permits it;
- selected documents reach another component or application only through the
  portal-mediated path;
- the reduced child cannot inherit the main component's portal or sensitive
  authority merely because it belongs to the same application;
- an application whose fixed policy ceiling excludes a sensitive resource cannot
  self-grant it through identity, manifest declarations, or approvals.

This model assumes the non-escape properties already checked by
`JobContainment.tla` and does not remodel the job tree. It also does not model the
future package verifier, signing lineage, permission database, trusted prompt UI,
lease revocation, concrete filesystem/network/device service protocols, or the
current raw `fork` ABI. Those will require implementation milestones and more
specific refinements before conformance claims are appropriate.

These are architecture properties and, where stated, implementation-alignment
checks. They are not a claim that the complete kernel, service manager, or future
application runtime has been formally verified.

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

java -XX:+UseParallelGC -cp /path/to/tla2tools.jar \
  tlc2.TLC -deadlock \
  -config formal/ServiceGeneration.cfg formal/ServiceGeneration.tla

java -XX:+UseParallelGC -cp /path/to/tla2tools.jar \
  tlc2.TLC -deadlock \
  -config formal/ApplicationSandbox.cfg formal/ApplicationSandbox.tla
```

`-deadlock` disables TLC's deadlock error because intentional terminal states are
allowed in these bounded safety models. The configured safety invariants remain
checked over the complete reachable state graph.

The repository CI pins TLA+ Tools 1.7.4 for repeatability and checks all modules.

## Refinement plan

The first formal-security sequence is intentionally layered:

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
   Implemented and machine-checked.
6. **ServiceGeneration** — monotonic publication, fresh provider ingress,
   generation-bound issuance, and non-rebinding old authority. Implemented and
   machine-checked.
7. **ApplicationSandbox** — explicit bootstrap authority, broker-issued grants,
   portal mediation, delegation ceilings, and sandbox containment. Target-state
   architecture model; implementation milestones remain future work.

After this sequence, formal work should follow concrete implementation milestones
rather than grow into a monolithic policy model. Useful next refinements include
endpoint peer-closure/wakeup lifecycle, mapped shared-memory protection/W^X,
application-manager conformance traces once that runtime exists, and later
MMIO/IRQ/DMA authority when userspace drivers arrive.

## Implementation relationship

The models specify permitted security transitions rather than literal Rust data
structures. The intended mapping remains small and explicit:

| Formal action | Kernel/userspace concept |
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
| `ServiceGeneration.Publish` | publish a strictly newer generation with a fresh ingress source |
| `ServiceGeneration.Withdraw` | withdraw the exact active generation while retaining its tombstone |
| `ServiceGeneration.BeginRequest` | queue a generation-neutral request through stable route authority |
| `ServiceGeneration.CompleteRequestSuccess` | issue exact provider authority for the current generation |
| `ServiceGeneration.CompleteRequestUnavailable` | return unavailable without provider authority |
| `ServiceGeneration.CloseGrant` | release one old or current provider handle without rebinding it |
| `ApplicationSandbox.LaunchRoot` | trusted application manager installs a narrow baseline allowlist |
| `ApplicationSandbox.SpawnChild` | create a reduced declared component from an explicit handle allowlist |
| `ApplicationSandbox.DeclareSensitive` | record permission/request metadata without granting authority |
| `ApplicationSandbox.ApproveSensitive` | record policy/user approval without granting authority |
| `ApplicationSandbox.BrokerGrant` | issue an eligible approved resource through an authorized provider |
| `ApplicationSandbox.PortalAcquire` | issue one exact user-selected resource through trusted UI mediation |
| `ApplicationSandbox.PortalTransfer` | mediate a scoped resource transfer to another component/application |
| `ApplicationSandbox.DelegateSameApplication` | directly delegate only a transfer-policy-approved reduced resource |
| `ApplicationSandbox.DropAuthority` | close one live grant without changing identity or policy metadata |

The live service-route implementation matches the phase-six boundary.
`RouteTable::publish` requires a generation strictly greater than the retained
generation and keeps the latest generation after withdrawal.
`RouteBroker::connect` authorizes first, resolves the currently published route,
and passes that exact generation into the issuer. The native adapter duplicates
the current stable provider source and returns exact `SEND` authority plus the
same generation in the accepted response. Each replacement provider generation
uses fresh ingress endpoint objects, so old exact-`SEND` handles retain old-object
identity rather than reaching the replacement.

For phase seven, the implementation mapping above is architectural intent. The
current kernel already supplies process-local capabilities, rights attenuation,
direct-child bootstrap, jobs, and service-route primitives, but the full
application manager, signed application identity, portal suite, permission store,
and component allowlist launcher are not yet implemented.

The guiding rule is that the formal model should stay smaller than the
implementation. When a new feature cannot be described without pulling large
amounts of unrelated kernel, service-manager, or desktop-policy state into a
security model, that is a signal to reconsider the abstraction boundary rather
than model the entire system.
