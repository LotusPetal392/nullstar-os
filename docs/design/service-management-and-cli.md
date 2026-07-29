# Service management and command-line direction

## Status

A declarative PID 1 service manager, a native `sv` management client, and a practical
native utility set are **accepted direction**. Exact unit syntax, the name and packaging
of a multicall utility binary, and the eventual GNU compatibility level remain
**tentative design**.

This document expands the userspace principles in
[the userspace architecture](userspace-architecture.md).

## Service-manager role

PID 1 should evolve from the current hard-coded supervisor into the authority that:

- loads versioned service definitions;
- resolves dependencies and startup ordering;
- launches service jobs with structured arguments and environments;
- delegates capabilities and service endpoints according to policy;
- tracks startup, readiness, health, failure, and shutdown state;
- applies restart limits and exponential or bounded backoff;
- assigns CPU, memory, handle, I/O, and realtime budgets;
- connects standard output, standard error, and structured logging;
- exposes a versioned control and observation protocol.

Ordering is not authority. A service that starts earlier does not automatically gain
access to services or hardware that start later. Every grant remains explicit.

The manager should support system services first. User-session services use the same
lifecycle model under a session-scoped manager or delegated PID 1 namespace later.

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
belongs under `Profile/state/services`. Packaged definitions are not modified to
record whether a service is enabled.

The first definition format only needs ordinary service units. Timer, socket,
path-triggered, and target-style units should be added only after their activation and
failure semantics are defined.

A definition should eventually include:

- stable canonical service name;
- executable, argument vector, working directory, and environment entries;
- dependency, ordering, and readiness relationships;
- restart condition, restart limit, backoff, startup timeout, and shutdown timeout;
- requested capabilities, filesystem bindings, devices, and brokered service endpoints;
- process and job isolation policy;
- resource limits and scheduling class;
- logging, audit, and health-check policy.

The parser must be versioned, bounded, deterministic, and reject unknown mandatory
fields. Service commands are argument arrays, not shell strings.

## Service state model

The control protocol should distinguish at least:

```text
disabled
stopped
starting
running-not-ready
ready
stopping
failed
restarting
quarantined
```

The manager should expose both desired state and observed state. `start` changes the
desired state; it does not claim success until launch and any required readiness
contract have completed.

Each process incarnation receives a service generation. Logs, resource accounting,
client endpoints, and failure records identify the generation so a restarted service
is not confused with its predecessor.

## The `sv` command

`sv` is the accepted native command-line client for service control and inspection.
It talks to the service manager over a versioned IPC protocol; it does not scan process
IDs, edit packaged files, or send arbitrary signals as its primary mechanism.

Initial commands should be:

```text
sv list
sv status [service]
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
```

The default status display should be concise and scriptable. A detailed mode should
show generation, readiness, uptime, restart policy, recent failure, delegated
capability classes, and resource budgets.

Canonical service identities should be globally unambiguous, for example:

```text
filesystem.vfs
filesystem.nullfs
device.manager
network.stack
network.policy
logging
media.graph
desktop.compositor
```

Short aliases such as `media` may be provided for interactive use, but scripts and
configuration should prefer canonical names.

## Authorization

Service observation and service control are separate rights.

- ordinary users may inspect public system status;
- a user may manage services belonging to that user session;
- administrative operations pass through the authorization broker;
- identity alone does not manufacture the capability to control a service;
- sensitive environment, capability, and failure details may require stronger
  inspection rights.

`sv stop` requests a managed transition. The service manager performs dependency,
authorization, timeout, logging, and cleanup policy.

## Logging integration

Every managed service should receive a structured logging endpoint and optional
standard-stream capture automatically. `sv logs` is a convenience view over the
central logging service, filtered by stable service identity and generation.

Examples:

```text
sv logs network.policy
sv logs network.policy --follow
sv logs network.policy --previous
sv logs network.policy --since 10m
```

The service manager records administrative actions, state transitions, readiness,
restart decisions, and resource-limit violations as structured events.

## Native command-line utilities

NullStar should provide a useful command-line environment before a full libc or GNU
port exists. Essential boot and recovery commands must remain native, small, and
usable in statically linked images.

The initial utility set should grow from the existing userspace programs to include:

```text
cat       cp        mv        rm        mkdir     rmdir
ls        pwd       echo      printf    head      tail
wc        sort      uniq      cut       tr        tee
touch     stat      find      env       true      false
sleep     date
```

Shared argument parsing, diagnostics, exit-status conventions, filesystem wrappers,
and tests should live in reusable Rust crates. Individual commands may be separate
binaries initially.

A later multicall binary is attractive for recovery and small installations. The
working name `nscore` is tentative. The launcher or package manifest may map multiple
command names to applets without requiring filesystem symbolic links.

## Native administration commands

Some system concepts should receive native commands rather than being forced into a
traditional Unix interface:

- `sv` for managed services;
- `volume` for physical and logical volume inspection;
- `namespace` for authorized VFS binding inspection and changes;
- `netctl` for network policy and connection attribution;
- `logctl` for structured logs;
- future package, process/job, driver, and authorization clients.

Compatibility commands such as `mount`, `df`, `ps`, `kill`, `syslog`, and `sudo` may
be implemented later, but they should translate into native services rather than
defining NullStar's internal model.

## POSIX and GNU compatibility

NullStar should pursue three explicitly documented levels:

1. **Native semantics**: the canonical NullStar API and administration model.
2. **Portable/POSIX behavior**: common options, exit codes, and interfaces needed by
   portable software and scripts.
3. **GNU extensions**: selected widely used behavior where the implementation and
   compatibility value justify it.

A native `ls` or `cp` does not need every GNU extension before it becomes useful.
Conversely, intentional differences should be documented rather than left accidental.

Porting actual GNU core utilities is a later compatibility milestone. It depends on a
mature libc, process and terminal behavior, links, file metadata, users and groups,
locales, wide characters, build tools, and the gnulib portability environment. GNU
coreutils should become a demanding compatibility workload, not a boot dependency.

BusyBox, Toybox, or smaller portable packages may be useful intermediate tests after
the basic libc surface exists, but they do not replace the native recovery suite.

## Shell evolution

`ush` should continue to provide the native interactive environment. Useful scripting
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

1. Define the service-manager control protocol and explicit service-state machine.
2. Implement `sv list`, `status`, `start`, `stop`, and `restart` against the current
   supervisor before the full definition loader exists.
3. Add versioned service definitions, dependency validation, readiness, and restart
   budgets.
4. Integrate structured logs, `sv logs`, resource accounting, and authorization.
5. Add enablement and local override storage without modifying packaged definitions.
6. Expand the native utility set with shared Rust infrastructure and recovery-image
   coverage.
7. Add user-session services and explicit system/user scopes to `sv`.
8. Grow shell scripting and POSIX-compatible utility behavior.
9. Use external utility suites and eventually GNU coreutils as compatibility tests.

## Open questions

- The unit-file syntax and schema-evolution rules.
- Whether the service broker is implemented by PID 1 or a separate early service.
- How user-session service managers relate to PID 1 and login sessions.
- Exact output and machine-readable formats for `sv`.
- Whether the first native utilities remain separate binaries or move into a multicall
  executable.
- The explicit POSIX and GNU compatibility targets for each command.
