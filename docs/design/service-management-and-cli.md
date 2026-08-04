# Service management and command-line direction

## Status

A small PID 1 bootstrap supervisor, a separate declarative system service manager, a
native `sv` management client, and a practical native utility set are **accepted
direction**. Exact definition syntax, process names, the packaging of a multicall
utility, and eventual GNU compatibility remain **tentative design**.

The complete process, job, activation, restart, session, application, login, and recovery
model is specified in
[Service, session, and application lifecycle](service-and-session-lifecycle.md). The current
[`NSVC` v1 milestone](../service-control-protocol.md) supplies a host-testable, allocation-free
64-byte control codec plus native read-only observation through PID 1 and `sv`. It does not yet
supply a separate manager or mutation authority.

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

The first definition format needs ordinary service units and channel activation. Timer,
path, device, and other activation classes should be added only after their queue,
failure, and authorization semantics are defined.

A definition should eventually include:

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
status SERVICE`, and trusted `ush` builtins are implemented. Valid mutation requests on the observation
ingress receive `AccessDenied`; there is no mutation endpoint. The trusted shell receives only
`SEND | DUPLICATE`, while standalone `/sv` works only when an authorized launcher installs exact
`SEND` authority at handle `1`. This milestone adds no manager process, activation, definitions, or
kernel changes.

## The `sv` command

`sv` is the native service control and inspection client. It talks over the versioned `NSVC`
protocol; it does not scan PIDs, edit packaged files, or send arbitrary signals as its primary
mechanism.

The implemented read-only commands are:

```text
sv list
sv status SERVICE
```

The next management commands should be:

```text
sv start <service>
sv stop <service>
sv restart <service>
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
   current supervisor. **Partly delivered:** the allocation-free, host-testable `NSVC` v1 codec and
   PID 1 read-only registry fix the observation contract, but job containment remains.
2. Implement `sv list`, `status`, `start`, `stop`, and `restart` against that protocol. **Partly
   delivered:** native `list` and `status` use capability-authorized IPC; mutation packets are
   canonically denied and live mutation remains future work.
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

- The service-definition syntax and schema-evolution rules.
- The exact binary and process names for PID 1 and the system service manager.
- Whether the service broker is part of the service manager or a separate early service.
- The first channel-activation queue and failure limits.
- Exact machine-readable output formats for `sv`.
- Whether native utilities remain separate binaries or move into a multicall executable.
- The explicit POSIX and GNU compatibility targets for each command.
