# Service management and command-line direction

## Status

A small PID 1 bootstrap supervisor, a separate declarative system service manager, a
native `sv` management client, and a practical native utility set are **accepted
direction**. The initial bounded service-definition syntax is implemented and documented
in [NullStar service-definition format](../service-definition-format.md). Process names,
the packaging of a multicall utility, later definition fields, and eventual GNU
compatibility remain **tentative design**.

The complete process, job, activation, restart, session, application, login, and recovery
model is specified in
[Service, session, and application lifecycle](service-and-session-lifecycle.md). The current
[`NSVC` v1 milestone](../service-control-protocol.md) supplies a host-testable, allocation-free
64-byte control codec plus capability-separated native observation and mutation through PID 1 and
`sv`. Mutation is not restart-only: restart is generic, while logging is the first live
`start`/`stop` target using bounded convergence and generation-isolated route replacement.
Filesystem `Start` and `Stop` remain exactly `Unsupported`. The allocation-free version 1
definition parser and one policy-pinned PID 1 migration pilot are implemented. After VFS and
NullFS readiness, PID 1 loads the exact definition through canonical `/System/services`, launches
its NullFS-only executable with manager-owned generation and readiness capabilities, and applies
bounded `on-failure` restart policy. This is not general discovery or enablement, and there is no
separate manager.

## PID 1 and the system service manager

The kernel launches PID 1 as a minimal root userspace process. PID 1 establishes the
root job hierarchy, starts and supervises the system service manager, retains emergency
recovery authority, and coordinates final failure or shutdown. It should not become the
ordinary implementation of dependency resolution, device policy, networking, logging,
identity, package management, or user-session management.

The separately restartable system service manager owns machine-wide lifecycle policy:

- load and validate versioned service definitions;
- resolve dependencies, ordering, and capability requirements;
- launch service jobs with structured arguments and explicit startup handles;
- route capabilities and restricted service namespaces according to policy;
- track activation, readiness, health, degradation, failure, and shutdown;
- apply restart limits, bounded backoff, quarantine, and escalation;
- assign CPU, memory, handle, IPC, I/O, and realtime budgets;
- connect structured logging and optional standard-stream capture;
- expose versioned control and observation protocols;
- create top-level login-session jobs and delegate each to a session manager.

During migration, some bootstrap and manager code may share a package or binary, but
their authority and lifecycle roles remain distinct. Ordering is never authority: a
service that starts first does not automatically gain access to later services or
hardware.

## Definition and override locations

Packaged system definitions should live under:

```text
/System/services/
```

Machine enablement, local policy, and drop-in overrides should live under:

```text
/System/config/services/
```

User definitions and preferences should use:

```text
/Users/<name>/Profile/config/services/
```

Mutable system service state belongs under `/System/var`, while user service state
belongs under `Profile/state/services`. Packaged definitions are not modified to record
whether a service is enabled.

The first definition format describes ordinary immediate or notify-ready service units. Channel,
timer, path, device, and other activation classes should be added only after their queue,
failure, and authorization semantics are defined.

The implemented version 1 format intentionally covers only identity, description,
executable and structured arguments, readiness notification, and bounded restart fields. The
current migration pilot accepts no definition arguments because the launch ABI is still
command-line based, grants no definition-selected capabilities, and treats its exact definition
path as fixed enablement policy. A later definition version should eventually include:

- stable canonical service identity and service class;
- executable identity, argument vector, environment data, and working-directory
  capability;
- capabilities consumed, provided, and offered to children;
- dependencies, ordering, readiness, and health contracts;
- activation mode and bounded queued-connection policy;
- restart condition, restart limit, backoff, startup timeout, and shutdown deadline;
- filesystem bindings, devices, and brokered service endpoints;
- process and job isolation policy;
- CPU, memory, handle, IPC, I/O, and realtime limits;
- logging, audit, configuration, and crash-report policy.

The parser must be versioned, bounded, deterministic, and reject unknown mandatory
fields. Service commands are structured argument arrays, not shell strings. A requested
capability or privileged service class is validated against package, signing, and system
policy; writing it into a definition is not a grant.

## Service identity and generation

Canonical service identities are stable and globally unambiguous, for example:

```text
filesystem.vfs
filesystem.nullfs
device.manager
network.stack
network.policy
logging
media.graph
desktop.login
```

A process ID identifies one process. A service generation identifies one supervised
incarnation. Clients connect through a brokered stable identity and must not cache a PID
as the service contract.

Logs, resource accounting, crash reports, activation endpoints, and failure records
include both stable service identity and generation so a restarted process is never
confused with its predecessor.

## Startup and readiness

The manager constructs a service before it runs:

1. validate the definition, package, profile, executable, and capability routes;
2. create the service job and suspended process;
3. install one bootstrap channel in the known initial handle slot;
4. send a versioned startup message carrying explicit handles and launch data;
5. start the initial thread;
6. wait for the declared readiness contract;
7. release dependent and queued client connections only when policy permits.

Process existence is not readiness. A service may report `READY` through its lifecycle
channel only after required recovery, validation, device acquisition, and provider
registration complete.

The control protocol should distinguish desired state from observed state and represent
at least:

```text
defined
disabled
stopped
activating
starting
running-not-ready
ready
degraded
stopping
terminating
failed
restarting
quarantined
```

`start` changes desired state; it does not report success merely because a process was
created.

## Dependencies and activation

Definitions distinguish:

- `requires`: cannot operate without the dependency;
- `wants`: useful but optional;
- `after`: ordering only;
- capability requirement: a protocol or resource route that an authorized provider may
  satisfy.

Dependency graphs are cycle checked. Capability requirements are preferred where a
consumer needs a service class or device resource rather than one named process.

Native demand activation uses pre-created channel endpoints. A broker may queue a
bounded connection while the provider starts and then deliver the peer endpoint in the
startup message. Services do not bind arbitrary privileged global pathnames.

Queued activation state belongs to the manager or broker so it can survive process
replacement. Non-idempotent application operations must not be replayed automatically.

## Restart and health policy

Recommended restart conditions are:

```text
never
on-failure
on-abnormal-exit
on-watchdog
always
```

Every automatic policy has a rate limit, sliding window, and bounded backoff. Repeated
failure leads to a visible failed or quarantined state, not an unbounded restart storm.

Health checks should traverse the service's real event loop or dispatch path. A detached
heartbeat thread alone cannot prove that the service can process work.

Escalation may restart one service, a dependent subtree, a driver family, or one user
session; enter recovery mode; or reboot only when recovery is impossible. Stateful
services must define uncertain-operation behavior and may require fail-closed recovery
rather than automatic replay.

## Shutdown

The service manager coordinates absolute shutdown deadlines in dependency order:

1. stop new login and application launches;
2. stop application and user-session jobs;
3. stop nonessential machine services;
4. flush filesystems and storage;
5. stop networking and devices where safe;
6. persist final generation, failure, and log state;
7. return final power-transition control to PID 1 and the kernel.

A service may request a bounded extension, but an unresponsive job is eventually
terminated. Storage and package services must define when an uncertain outcome requires
recovery rather than continued shutdown.

## System and user scopes

The system service manager owns machine-wide services and top-level session creation. A
per-login session manager owns that user's compositor, desktop shell, session services,
and application jobs.

The same lifecycle concepts apply in both scopes, but authority does not. A session
manager receives capabilities for one user and session and cannot control unrelated
system services or another login session.

The `sv` client selects or is given an explicit scope. A user may manage services in the
user's own session; machine changes require administrative authorization.

## Current control-contract milestone

`NSVC` v1 fixes the request/response encoding for one-record paginated `list`, `status`, `start`,
`stop`, and `restart`, including nonzero request-ID correlation, optional service generation, and
separate observed and desired lifecycle states. The native transport keeps each wire record at 64
bytes and transfers one exact-`SEND` private reply endpoint with a request; responses transfer no
capability. Possession of an exact-`SEND` observation endpoint, rather than knowledge of an ID or
pathname, authorizes list and status.

PID 1 currently serves a fixed registry for `logging`, `nullfs`, `tmpfs`, and `vfs`. `/sv list`, `/sv
status SERVICE`, `/sv restart SERVICE`, `/sv start logging`, `/sv stop logging`, and trusted `ush`
builtins are implemented. Observation and mutation use separate endpoint objects. Mutation on the
observation ingress receives `AccessDenied`; filesystem `Start` and `Stop` on the mutation ingress
receive `Unsupported`.

A successful restart commits intent and reports the old generation as terminating; it does not wait
for replacement readiness. Controlled restart does not charge failure backoff or budget, and the
replacement receives the next manager-owned generation. Restart intent remains pending until the
replacement is ready and queued duplicate requests have received `Busy`. An unconfirmed sent mutation
is outcome unknown and is not retried. The trusted shell receives separate `SEND | DUPLICATE` grants
without `TRANSFER`; standalone `/sv` expects observation at handle `1` and mutation at handle `2`.

The supervisor retains desired state across controlled exits within PID 1, classifies controlled stop
separately from failure, permits stop to suppress a pending restart, re-arms bounded policy on
explicit start, and rolls back a stop whose signal was not delivered. Logging now exercises those
transitions through `/sv`. Its steady-state startup, readiness, child-status, mutation, and backoff
work is bounded; stop withdraws published routes before later resolutions are serviced, and start
publishes fresh producer and observer objects only after accepted readiness. Restart intent remains
fenced through replacement startup so queued duplicates receive `Busy`. PID 1 escalates an ignored
cooperative termination request to uncatchable, unblockable signal 9 after a bounded grace period.
A distinct readiness deadline force-terminates a live starting generation that never becomes ready;
its final status then follows the ordinary bounded restart/backoff and failure policy.

A successful logging start or stop response commits desired state; it does not wait for readiness or
final exit. Controlled NullFS restart is also asynchronous. PID 1 queues private `NFLC` v1 `QUIESCE`
behind earlier work on the existing FIFO request endpoint. Exact `QUIESCED` lets it offline that exact
generation, waking tail work with `EIO`, before it queues `UNMOUNT`. The service closes core open
handles, syncs and publishes a clean superblock through `try_unmount`, emits exact
`CLEAN_UNMOUNTED`, and exits `0`. PID 1 requires both the exact clean event and final exit `0`, then
uses a fresh endpoint and strictly newer generation before completing the restart fence.

Timeout, malformed or mismatched events, capability-bearing events, lifecycle failure, or early or
nonzero exit cannot prove durability. PID 1 exact-generation offlines, terminates and drains the whole
generation job, and lets the replacement perform dirty recovery. Controlled restart charges no failure backoff or
budget. Filesystem `Start` and `Stop` remain exactly `Unsupported`; `NSVC` v1 and the public
filesystem version 1 request/reply operation set are unchanged. This milestone adds no manager
process, activation, definitions, or cross-reboot persistence; ABI 1.13 remains the narrow PID-1
provider-offlining syscall.

## The `sv` command

`sv` is the native service control and inspection client. It talks over the versioned `NSVC`
protocol; it does not scan PIDs, edit packaged files, or send arbitrary signals as its primary
mechanism.

The implemented commands are:

```text
sv list
sv status SERVICE
sv start SERVICE
sv stop SERVICE
sv restart SERVICE
```

The next management commands should be:

```text
sv enable <service>
sv disable <service>
sv logs <service>
```

Later commands may include:

```text
sv reload <service>
sv watch [service]
sv describe <service>
sv dependencies <service>
sv dependents <service>
sv failures <service>
sv reset-failed <service>
sv resources <service>
sv capabilities <service>
sv generations <service>
```

The default output should be concise and scriptable. Detailed output may show desired
and observed state, generation, readiness, uptime, restart policy, recent failure,
delegated capability classes, job limits, and health state.

Short aliases may be provided for interactive use, but definitions and scripts should
prefer canonical service identities.

## Authorization

Service observation and control are separate rights.

- ordinary users may inspect public machine status;
- users may manage services in their own login session;
- administrative operations pass through the authorization broker;
- identity alone does not manufacture service-control authority;
- sensitive environment, capability, crash, or failure details require stronger
  inspection rights.

`sv stop` requests a semantic managed transition. The manager performs dependency,
authorization, deadline, logging, and job-cleanup policy.

Administrative approval should authorize the exact target and action through a narrow,
expiring or single-use ticket rather than placing the entire `sv` process in an ambient
root state.

## Logging integration

Every managed service receives a structured logging endpoint and optional standard-
stream capture. `sv logs` is a convenience view over the logging service filtered by
stable service identity and generation.

Examples:

```text
sv logs network.policy
sv logs network.policy --follow
sv logs network.policy --previous
sv logs network.policy --since 10m
```

The manager records administrative actions, state transitions, readiness, restart and
quarantine decisions, watchdog failures, and resource-limit violations as structured
events.

## Native command-line utilities

NullStar should provide a useful command-line environment before a full libc or GNU port
exists. Essential boot and recovery commands remain native, small, and usable in
statically linked images.

The initial utility set should grow from the existing userspace programs to include:

```text
cat       cp        mv        rm        mkdir     rmdir
ls        pwd       echo      printf    head      tail
wc        sort      uniq      cut       tr        tee
touch     stat      find      env       true      false
sleep     date
```

Shared argument parsing, diagnostics, exit-status conventions, filesystem wrappers, and
tests should live in reusable Rust crates. Individual commands may remain separate
binaries initially.

A later multicall binary is attractive for recovery and small installations. The
working name `nscore` remains tentative. A launcher or package manifest may map several
command names to applets without requiring symbolic links.

## Native administration commands

Native concepts should receive native clients rather than being forced into traditional
Unix interfaces:

- `sv` for managed services;
- `volume` for physical and logical volume inspection;
- `namespace` for authorized VFS-binding inspection and changes;
- `netctl` for network policy and connection attribution;
- `logctl` for structured logs;
- future package, process/job, driver, permission, and authorization clients.

Compatibility commands such as `mount`, `df`, `ps`, `kill`, `syslog`, `sudo`, and
`systemctl` may be added later, but they translate into native services rather than
defining NullStar's internal model.

## POSIX and GNU compatibility

NullStar should document three levels:

1. **Native semantics**: canonical NullStar APIs and administration.
2. **Portable/POSIX behavior**: common interfaces, options, and exit codes needed by
   portable software and scripts.
3. **GNU extensions**: selected widely used behavior where compatibility value justifies
   the implementation.

A native `ls` or `cp` need not implement every GNU extension before it is useful.
Intentional differences should be documented rather than accidental.

Porting GNU coreutils is a later compatibility milestone. It depends on a mature libc,
process and terminal behavior, links, file metadata, users and groups, locales, wide
characters, build tools, and gnulib. GNU coreutils becomes a demanding compatibility
workload, not a boot dependency.

BusyBox, Toybox, or smaller portable suites may be intermediate tests after the basic
libc surface exists, but they do not replace the native recovery utilities.

## Shell evolution

`ush` should continue as the native interactive environment. Useful scripting
milestones include:

- quoting and escaping;
- exit status and `&&`/`||`;
- script files;
- globbing;
- command substitution;
- loops, conditionals, and functions;
- richer pipelines and job control.

A future `sh` compatibility mode or separate shell may make a stronger POSIX promise.
The name `ush` should not silently imply complete POSIX shell behavior.

## Recommended implementation stages

1. Add job containment, process-exit observation, and a versioned control protocol to the
   current supervisor. **Foundation delivered:** ABI 1.15 supplies capability-backed flat jobs,
   inherited descendant containment, independent FIFO exit records, and bounded termination; the
   ABI 1.16 child-creation extension adds immutable hierarchy plus recursive subtree observation and
   termination; ABI 1.17 adds tightening-only hierarchy-scoped process ceilings; ABI 1.18 adds
   permanent drained-leaf retirement for bounded generation reuse; ABI 1.19 adds read-only local
   process-ceiling inspection. The
   allocation-free `NSVC` v1 codec and PID 1 registry supply service observation. Policy-pinned
   definition-backed service attempts and every logging, NullFS, tmpfs, and VFS generation now receive
   fresh jobs before barrier release; PID 1 retains only `SIGNAL | WAIT` and drains each complete generation to
   `ECHILD` before replacement. NullFS preserves exact quiesce and clean-unmount evidence before
   clean drainage and uses whole-job termination before dirty recovery. Current service generations
   remain flat jobs until a separate manager introduces service subtrees.
2. Implement `sv list`, `status`, `start`, `stop`, and `restart` against that protocol. **Partly
   delivered:** native `list`, `status`, and restart use separately authorized IPC, logging has live
   in-memory `start`/`stop`, and NullFS restart has exact-generation quiesce, clean unmount, forced
   dirty-recovery fallback, and provider replacement; filesystem live `start`/`stop` and cross-reboot
   desired state remain future work.
3. Introduce the one-bootstrap-channel startup contract and stable service generations.
4. Extract ordinary service policy into a separately restartable system service manager
   while PID 1 retains bootstrap and recovery supervision.
5. Add definitions, dependency validation, readiness, channel activation, restart
   budgets, and resource limits.
6. Integrate structured logs, configuration handles, `sv logs`, health checks, and
   operation-specific authorization.
7. Add enablement and local override storage without modifying packaged definitions.
8. Add login-session managers and explicit system/user scopes to `sv`.
9. Expand the native utility set and recovery-image coverage.
10. Grow shell scripting and use external utility suites as compatibility tests.

## Open questions

- The schema-evolution rules and fields added after service-definition version 1.
- The exact binary and process names for PID 1 and the system service manager.
- Whether the service broker is part of the service manager or a separate early service.
- The first channel-activation queue and failure limits.
- Exact machine-readable output formats for `sv`.
- Whether native utilities remain separate binaries or move into a multicall executable.
- The explicit POSIX and GNU compatibility targets for each command.
