# Application launch foundation

## Status

This document describes the **implemented first step** toward the application-sandbox architecture.
It is intentionally narrower than the complete design in
[`application-sandboxing.md`](application-sandboxing.md).

The current implementation provides a native mediated launch primitive with:

- a fresh job assigned before application execution is released;
- complete inherited userspace file-descriptor scrubbing;
- complete inherited capability-table scrubbing;
- one receive-only bootstrap capability installed at process-local slot 1;
- typed `NSPC` startup capabilities with receiver-side kind and rights ceilings;
- authenticated `NSPD` descriptive application identity and launch metadata;
- distinct `desktop`, `desktop-child`, and `worker` profile identities; and
- a mandatory application entry wrapper that validates startup before application code executes.

It does **not** yet provide a complete desktop application manager, package verifier, private storage
builder, restricted service namespace, portal suite, permission database, or compatibility namespace.

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
Dropping the job handle is not an implicit application kill: the kernel roots a job while members are
alive. A future application-manager lifecycle layer must retain the handle when it needs explicit
termination, completion drainage, and supervision.

The current default application subtree process limit is 16 and callers may tighten it to any
nonzero value no greater than the kernel `MAX_JOB_PROCESSES` bound.

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

## Descriptive identity is not authority

The process-start stream carries an `ApplicationIdentity` containing provisional package,
package-generation, application, component, user, and session identifiers. The launch record also
carries a nonzero manager generation and one of the application namespace/profile identifiers:

| Profile | Current descriptive namespace/profile ID |
| --- | ---: |
| `desktop` | 2 |
| `desktop-child` | 3 |
| `worker` | 4 |

These numeric values are provisional descriptive metadata. They do not grant filesystem, device,
service, or other authority. The accepted long-term stable identity remains the signed application
principal described by the sandbox architecture.

The application runtime pins the startup sender to its direct parent, validates its own PID and
executable identity, requires the service identity to be zero, verifies application identity fields,
and rejects an unknown namespace/profile value before returning control to application code.

## Executable and environment boundary

The initial implementation accepts only an absolute canonical executable path in its command. The
path cannot contain empty, `.` or `..` components. This prevents the launch helper itself from
silently changing the executable identity through relative path traversal.

The application startup description currently publishes an empty compatibility environment, but the
forked process still inherits the underlying legacy process environment until a dedicated
environment-construction step is implemented. Environment values are therefore **not yet part of the
sandbox confidentiality boundary**. The future application manager should construct an explicit
minimal environment rather than inheriting manager state.

Likewise, this launch primitive does not yet disable or virtualize legacy global-path syscalls. The
application-sandbox design still requires private directory capabilities and a restricted filesystem
namespace/service path before ordinary applications can be described as fully filesystem sandboxed.

## Failure behavior

The launch path fails closed around the security boundary:

- descriptor or capability scrubbing failure terminates the child before acknowledgment;
- the parent does not grant bootstrap authority before receiving the isolation acknowledgment;
- job-assignment failure terminates and reaps the child;
- startup-message or process-start-data failure terminates the application job and child;
- release-barrier failure terminates the application job and child;
- malformed bootstrap authority, unexpected initial capability slots, invalid typed authority, or
  invalid descriptive launch data causes the application entry wrapper to exit before application
  code runs.

Partial startup streams are not retried into the same child. A failed attempt is terminated so a
receiver cannot continue from a partially observed authority or descriptive state.

## Relationship to the formal model

`formal/ApplicationSandbox.tla` is the target-state architecture model. This implementation begins
refining several of its assumptions:

- `Launch` -> descriptor/capability scrub, job assignment, and one bootstrap channel;
- `BootstrapGrant` -> explicit typed `NSPC` capability attachments;
- `AuthorityWithinCeiling` -> receiver-side `StartupCapabilityPolicy` rights reduction;
- `IdentityIsNotAuthority` -> separate `NSPD` descriptive identity and capability-bearing `NSPC`
  records; and
- containment assumptions -> existing non-relaxable job membership and inherited fork containment.

This is implementation-alignment evidence, not a proof that the Rust implementation formally
refines the TLA+ module.

## Next implementation steps

The launch foundation is intentionally small enough to become the common mechanism beneath a future
application manager. The next useful layers are:

1. **Reduced component spawning** — construct `desktop-child` and `worker` children from explicit
   capability allowlists instead of inheriting the main component's complete context.
2. **Stable verified application identity** — package/application identity, signing lineage,
   installation provenance, and authorized profile selection.
3. **Private storage and restricted namespace construction** — bundle/data/cache/temp/runtime roots
   and removal of ambient global-path authority for native applications.
4. **Baseline service routing** — restricted display, lifecycle, settings, logging, audio playback,
   portal, and service-namespace endpoints.
5. **Application lifecycle supervision** — readiness, termination, completion drainage, restart or
   relaunch policy, and user/session teardown.
6. **Portals and persistent grants** — user-selected file/directory authority followed by sensitive
   resource and device policy.
