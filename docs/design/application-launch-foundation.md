# Application launch foundation

## Status

This document describes the **implemented launch, verified-identity admission, reduced-component,
and lifecycle-supervision foundation** for the
application-sandbox architecture. It is intentionally narrower than the complete design in
[`application-sandboxing.md`](application-sandboxing.md).

The current implementation provides a native mediated launch primitive with:

- a fresh job assigned before application execution is released;
- complete inherited userspace file-descriptor scrubbing;
- complete inherited capability-table scrubbing;
- one receive-only bootstrap capability installed at process-local slot 1;
- typed `NSPC` startup capabilities with receiver-side kind and rights ceilings;
- authenticated `NSPD` descriptive application identity and launch metadata;
- a stable application principal bound to publisher lineage, trust class, and installation
  provenance before launch;
- verified-manifest authorization of the entry component, executable, user scope, and sandbox
  profile;
- distinct `desktop`, `desktop-child`, and `worker` profile identities;
- manager-mediated `desktop-child` and `worker` spawning from explicit, rights-monotonic
  capability allowlists;
- root-process-pinned readiness with bounded startup deadlines and relaunch backoff;
- whole-job termination and completion drainage before relaunch or teardown; and
- a mandatory application entry wrapper that validates startup before application code executes.

It does **not** yet provide a complete desktop application manager, cryptographic package verifier,
application registry service, private storage builder, standalone restricted-namespace broker,
portal suite, permission database, or compatibility namespace.

## Why both authority tables are scrubbed

NullStar is transitioning from early POSIX-like process facilities toward capability-mediated native
applications. At this stage a process can hold authority in two different forms:

1. the capability table used by the native IPC/protection model; and
2. the legacy userspace file-descriptor table used by filesystem, terminal, and pipe operations.

Scrubbing only the capability table would therefore not establish a clean application launch
boundary. An inherited descriptor could still grant access to a file, directory-facing service,
terminal, or pipe even though the capability table was empty.

`application_launch::spawn_application` consequently uses a two-phase child barrier. After `fork`,
the child retains only two private `CLOSE_ON_EXEC` pipe descriptors used for launch coordination. It
closes every other descriptor in the bounded descriptor table and then closes every inherited
capability. Only after both operations succeed does it acknowledge isolation to the parent.

```text
application manager                         fork child
        |                                       |
        |---------------- fork ---------------->|
        |                                       |
        |                         close inherited descriptors
        |                         close inherited capabilities
        |                                       |
        |<----------- isolation ack -------------|
        |                                       |
        | assign fresh application job           |
        | grant bootstrap RECEIVE at slot 1      |
        | send typed NSPC capability envelope    |
        | send authenticated NSPD launch data    |
        |                                       |
        |------------- release ----------------->|
        |                                       |
        |                                  exec application
```

The two private launch descriptors are themselves closed before or by successful `execve`, so the
application image begins with an empty legacy descriptor table. The bootstrap capability is installed
only after the child has acknowledged that its inherited capability table is empty.

## Job containment

The manager creates a fresh root job for each application launch, applies a bounded subtree process
limit, and assigns the still-blocked direct child before releasing execution. Job membership is
non-relaxable under the existing kernel job model, and later `fork` descendants inherit that
containment.

The returned `ApplicationInstance` retains the manager's job authority together with the process ID.
Reduced components are assigned to that same job before release, so a component request cannot
relax the root application's containment or process limit. Each component still repeats the complete
descriptor/capability scrub and receives a fresh bootstrap endpoint rather than inheriting the root
process's tables.

Dropping the job handle is not an implicit application kill: the kernel roots a job while members are
alive. `SupervisedApplication` therefore retains the handle for explicit whole-job termination,
completion drainage, and relaunch supervision.

The current default application subtree process limit is 16 and callers may tighten it to any
nonzero value no greater than the kernel `MAX_JOB_PROCESSES` bound.

## Lifecycle supervision

`application_lifecycle` separates lifecycle policy from kernel effects. Its allocation-free state
machine covers `Starting`, `Running`, `Draining`, bounded `Backoff`, `RelaunchPending`, and terminal
`Completed`, `Stopped`, or `Failed` states. Startup and backoff budgets are finite cooperative-yield
counts; the policy permits at most eight relaunches and rejects values above the shared lifecycle
yield ceiling.

The job-backed `SupervisedApplication` accepts only the exact `application-ready:v1` record without
an attached capability, stamped by the current root process. A readiness timeout, malformed or
foreign readiness, pre-readiness exit, or unsuccessful running root moves the generation through
whole-job termination and complete job-exit drainage. A replacement may be installed only after the
old job reaches `NO_CHILD`, and must preserve the root's application identity, desktop profile,
stable publisher principal, installation provenance, and manager generation. A clean root exit is
terminal and is not charged to relaunch policy.

User termination, session teardown, and manager shutdown are explicit stop reasons. They override a
pending drain or backoff, terminate the current job when one exists, and suppress relaunch. Every job
completion observed by the adapter is reaped, including reduced components and descendants visible
only through the job completion stream.

The QEMU launch gate exercises a timed-out first generation, verifies complete drainage, installs an
identity-pinned second generation, accepts its root readiness, and then proves session teardown drains
that generation without a third launch. The application manager remains responsible for constructing
the replacement launch and for scheduling `poll` calls against a real clock or event loop.

## Bootstrap and authority construction

Process-local capability slot 1 is a **discovery coordinate**, not authority. The parent creates a
fresh endpoint pair and grants the application only exact `RECEIVE` authority for the bootstrap end.
The actual handle remains opaque and generation checked.

Every additional startup capability travels in one `NSPC` application-role envelope. The launcher
creates temporary transfer-only duplicates so its source authorities remain owned by the manager.
The application runtime validates the received envelope with `StartupCapabilityPolicy` before
application entry:

- the runtime role must be `Application`;
- each known role must have the expected kernel object kind;
- delivered rights must contain the policy minimum;
- rights are reduced to the receiver policy maximum before the capability enters the process
  context;
- missing required roles fail startup; and
- unknown optional roles are discarded rather than becoming ambient authority.

This makes the receiver's policy ceiling authoritative. Sender-controlled role metadata cannot cause
an application to adopt more rights than its runtime contract permits.

The kernel IPC message limit currently bounds one startup envelope to four attached capabilities.
Larger application baselines will require either multiple authenticated startup records or a
restricted namespace/provider endpoint rather than increasing ambient launch authority without a
protocol.

## Reduced component spawning

The manager-owned `ApplicationInstance` retains an immutable description of the root launch's
authority ceiling plus rights-bounded duplicates of sources opted into component delegation. The
duplicates keep those source objects stable even if the caller later closes its original handle.
Each root capability is non-delegable by default. The manager must opt it into the `desktop-child`,
`worker`, or both reduced profiles, and each component launch supplies an explicit allowlist.

Before creating a process, `spawn_component` verifies that:

- the existing root uses the `desktop` profile and the requested profile is `desktop-child` or
  `worker`;
- package, application, user, session, package-generation, and manager-generation identity remain
  fixed while the component identifier changes;
- each requested role names the exact manager-owned source capability retained in the root ceiling;
- the source capability was explicitly marked delegable to the requested component profile;
- requested rights are equal to or narrower than the root's rights;
- roles are unique and all handles and rights descriptions are nonempty; and
- the component does not reproduce the complete root authority set unchanged.

Successful components run in the existing application job. The manager duplicates only the listed
sources into the component's typed startup envelope, and the component runtime applies its own policy
ceiling before application code runs. A rejected request creates no process. A failure after creation
terminates and reaps only that candidate component; it does not implicitly terminate healthy members
of the existing application job.

## Verified identity admission is not authority

The launcher no longer accepts freely assembled application identity and profile fields. A trusted
manager first passes a package-verifier result and selected installation record through
`authorize_application_launch`. The admission check requires exact agreement on package generation,
application identifier, publisher identity, accepted signing lineage, trust class, and system-app
designation. It also enforces installation ownership, one declared entry component, the declared
executable identity, and the ordinary `desktop` profile. A mismatch produces no launch
authorization.

The resulting `AuthorizedApplication` is opaque. It carries a fixed copy of at most eight verified
component declarations, so later `desktop-child` and `worker` requests must also match a declared
component, executable, and profile before process creation. This check is independent of the
rights-monotonic capability allowlist; both policies must pass.

The authenticated process-start stream carries an `ApplicationIdentity` containing package,
package-generation, application, component, user, and session identifiers plus a mandatory stable
identity record containing publisher lineage, package trust class, system designation, installation
record, and installation scope. The launch record also carries a nonzero manager generation and one
of the application namespace/profile identifiers:

| Profile | Current descriptive namespace/profile ID |
| --- | ---: |
| `desktop` | 2 |
| `desktop-child` | 3 |
| `worker` | 4 |

These numeric values are manager/package-service identifiers and remain descriptive metadata. They
do not grant filesystem, device, service, or other authority. The stable security principal is the
tuple of application identifier, publisher identity, accepted signing lineage, trust class, and
system designation; package generation and installation provenance select the immutable deployment
used for this launch.

`PackageVerification` is explicitly the bounded output of a trusted package verifier and carries the
component declarations authenticated with that package manifest. Constructing the Rust record does
not verify a signature. Authentication of a future package-verifier service, canonical bundle
parsing, content hashing, signature algorithms, revocation, and persistent registry storage remain
outside this launch-layer implementation.

The application runtime pins the startup sender to its direct parent, validates its own PID and
executable identity, requires the service identity to be zero, verifies application identity fields,
and rejects an unknown namespace/profile value before returning control to application code.

## Private storage and restricted namespace authority

A desktop root launch now requires an opaque `ApplicationNamespace` constructed against the exact
`AuthorizedApplication`. Construction validates two distinct manager-owned endpoints with the rights
needed to duplicate a send-only client view:

- `PRIVATE_STORAGE` names an application-private broker bound to the stable application, user, and
  session identity; and
- `SERVICE_NAMESPACE` names the restricted service router selected for that application profile.

The private-storage broker contract exposes logical `bundle`, `data`, `cache`, `temporary`, and
`runtime` roots. Requests select one root and a bounded canonical relative path. Absolute paths,
empty components, `.`, `..`, repeated separators, and embedded NUL bytes are rejected before broker
routing. The bundle role is read-only; write, create, and remove requests against it fail closed.
Physical NullFS/tmpfs layout is deliberately not an authority token and remains hidden behind the
broker endpoint.

Namespace roles are reserved by the launcher and cannot be injected through an ordinary capability
list. Reduced components receive neither endpoint automatically; later profile policy must delegate
an explicitly reduced route if a worker needs storage or service access.

The service-namespace endpoint now speaks the existing `NSRT` v1 route protocol through a
multi-route `ServiceNamespaceIngress`. Its immutable desktop allowlist contains display,
application-lifecycle, settings, logging-producer, audio-playback, and portal client routes. Route
knowledge remains descriptive: a requested key outside the allowlist receives `Unauthorized`
before publication lookup, an allowed route without a current provider receives `Unavailable`, and
an allowed published route returns the exact send-only endpoint for its current nonzero generation.
The normal broker authorizer still pins the kernel-stamped caller before availability is consulted.

Before the managed image is loaded, the launch shim asks the kernel to seal ambient path authority on
the next successful `execve`. The seal becomes irreversible when that image is committed, is inherited
by `fork`, and denies global-path `open`, `stat`, directory-read, `chdir`, `unlink`, `execve`, and legacy
spawn operations. Capability IPC remains available, so filesystem and service access must cross one
of the supplied endpoints. The QEMU launch probe checks the complete send-only endpoint set, rejects
aliased namespace endpoints, and verifies ambient-path denial in the root and both reduced profiles.

Provider-backed directory provisioning and a standalone application-manager broker process remain
integration work. The current QEMU manager probe owns the restricted ingress and proves accepted,
allowed-but-unavailable, and denied route behavior without giving the application a global route
grant.

## Executable and environment boundary

The initial implementation accepts only an absolute canonical executable path in its command. The
path cannot contain empty, `.` or `..` components. This prevents the launch helper itself from
silently changing the executable identity through relative path traversal.

The application startup description currently publishes an empty compatibility environment, but the
forked process still inherits the underlying legacy process environment until a dedicated
environment-construction step is implemented. Environment values are therefore **not yet part of the
sandbox confidentiality boundary**. The future application manager should construct an explicit
minimal environment rather than inheriting manager state.

Global path operations are sealed by the kernel for every managed application image. Compatibility
applications that require pathname projection will therefore need a separately authorized broker or
compatibility profile rather than silently regaining the machine namespace.

## Failure behavior

The launch path fails closed around the security boundary:

- descriptor or capability scrubbing failure terminates the child before acknowledgment;
- the parent does not grant bootstrap authority before receiving the isolation acknowledgment;
- job-assignment failure terminates and reaps the child;
- startup-message, process-start-data, or release-barrier failure terminates and reaps the candidate
  child;
- malformed bootstrap authority, unexpected initial capability slots, invalid typed authority, or
  invalid descriptive launch data causes the application entry wrapper to exit before application
  code runs.

Partial startup streams are not retried into the same child. A failed attempt is terminated so a
receiver cannot continue from a partially observed authority or descriptive state.

## Relationship to the formal model

`formal/ApplicationSandbox.tla` is the target-state architecture model. This implementation begins
refining several of its assumptions:

- `Launch` -> descriptor/capability scrub, job assignment, and one bootstrap channel;
- `SpawnChild` -> explicit same-application component allowlists intersected with a retained root
  authority ceiling and profile-specific delegation policy;
- `BootstrapGrant` -> explicit typed `NSPC` capability attachments;
- `AuthorityWithinCeiling` -> receiver-side `StartupCapabilityPolicy` rights reduction;
- private storage and namespace authority -> identity-bound send-only broker endpoints plus
  canonical relative-root requests;
- ambient path exclusion -> a one-way kernel seal applied when the managed image is committed;
- `IdentityIsNotAuthority` -> separate `NSPD` descriptive identity and capability-bearing `NSPC`
  records, with verified principal/provenance metadata still unable to create authority; and
- containment assumptions -> existing non-relaxable job membership and inherited fork containment.

This is implementation-alignment evidence, not a proof that the Rust implementation formally
refines the TLA+ module.

## Next implementation steps

The launch foundation is intentionally small enough to become the common mechanism beneath a future
application manager. The next useful layers are:

1. **Portal mediation and grant-backed authority** — implement the compositor and portal transports,
   stored-identity restoration, and rights-reduced file/directory broker endpoints around the
   implemented gesture admission, live resource resolver, and permission-store records.
2. **Production package and registry services** — cryptographic bundle verification, authenticated
   verifier routing, immutable generation selection, revocation, and durable installation records.
3. **Standalone application-manager integration** — event-driven launch ownership, namespace and
   storage providers, session wiring, and durable relaunch policy.
