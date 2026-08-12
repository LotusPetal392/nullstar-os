# Service, session, and application lifecycle

## Status

A small root userspace bootstrap process, a separate declarative system service
manager, hierarchical jobs, stable service identities, readiness-based supervision,
per-login session managers, and per-application jobs are **accepted direction**.

The initial bounded service-definition syntax is implemented and documented in
[NullStar service-definition format](../service-definition-format.md). Later definition
fields, process names, watchdog intervals, production restart budgets, activation queue
limits, and whether some bootstrap and service-manager code initially share one binary
remain **tentative design**.

This document defines the lifecycle architecture that uses the native
[IPC and object model](ipc-and-object-model.md). Command-line administration and
service-definition locations are described in
[Service management and command-line direction](service-management-and-cli.md).

It describes future architecture. The implemented system still uses a hard-coded PID 1 that directly
launches the current services and shell. PID 1 also acts as the temporary broker for the first generic
logging routes and as the temporary [`NSVC` v1 service-control
server](../service-control-protocol.md); see [NullStar OS architecture](../architecture.md) and the
[current service route protocol](../service-route-protocol.md). Native `/sv list`, `/sv status
SERVICE`, `/sv restart SERVICE`, and trusted shell builtins use separate exact endpoint authorities.
Controlled restart is implemented without charging failure policy. Logging now also supports live
`start` and `stop` through bounded PID 1 convergence: stop withdraws routes and converges without
charging failure policy, while start creates and publishes only a fresh ready generation. Restart
intent remains fenced through replacement startup, queued duplicates receive `Busy`, and PID 1
escalates an uncooperative generation to uncatchable signal 9 after a bounded grace period.
Policy-pinned definition-backed service attempts and every logging, NullFS, tmpfs, and VFS generation are
assigned to fresh flat jobs before launch-barrier release; PID 1 retains only `SIGNAL | WAIT` and
drains all generation exits to `ECHILD` before closing endpoints or starting a replacement. The
ABI also supports immutable child jobs with recursive subtree inspection, drainage, and termination;
ABI 1.17 adds tightening-only process ceilings enforced across every ancestor subtree. Service
generations remain flat roots until a separate manager introduces policy subtrees. ABI 1.18 lets
that future long-lived manager retire a drained generation leaf without relaxing or reparenting it. The
logging-lifecycle QEMU gate injects tmpfs/VFS descendants that escape their leaders' process groups
and requires descendant termination, whole-job drainage to `ECHILD`, and generation replacement.
The NullFS restart, crash-recovery, and block-device-loss gates inject the same escaped descendant;
clean restart preserves exact `QUIESCED`, `CLEAN_UNMOUNTED`, and final exit `0` before drainage,
while dirty paths terminate the entire generation job before recovery.
Starting logging generations also have a bounded
readiness deadline whose expiry enters normal restart and backoff policy rather than leaving the
runtime in `Starting` indefinitely. Controlled NullFS restart
now queues a private `NFLC` v1 `QUIESCE` marker behind earlier FIFO requests. The service finishes
those requests and processes no later public operations after exact `QUIESCED`; PID 1 then offlines the
exact generation, waking tail work with `EIO`, and sends `UNMOUNT`. The service closes core handles,
uses `try_unmount` to sync and publish a clean superblock, emits exact `CLEAN_UNMOUNTED`, and exits
`0`. PID 1 requires both the exact event and final exit `0` before fresh-endpoint, newer-generation
replacement. Invalid lifecycle traffic, timeout, failure, or early/nonzero exit uses exact-generation
offlining, whole-generation-job termination and drainage, and dirty recovery. Controlled restart does
not charge failure policy. Filesystem
`Start` and `Stop` remain exactly `Unsupported`; `NSVC` v1 and public filesystem version 1 operations
are unchanged. One policy-pinned migration pilot now loads
`/System/services/definition-probe.service` through the canonical VFS/NullFS path, launches its
NullFS-only executable, and applies generation-scoped readiness and bounded `on-failure` restart
policy. General discovery, enablement, dependency resolution, a separate manager, and cross-reboot
desired-state persistence remain future work.

## Design goals

The lifecycle model should:

- keep PID 1 small enough to audit and recover;
- make services restartable without making process IDs stable identities;
- construct every process with explicit capabilities before its first instruction;
- contain failures and resource exhaustion in jobs;
- support demand activation without global pathname or socket namespaces;
- separate machine services, login sessions, and application instances;
- provide deterministic startup, readiness, shutdown, and failure behavior;
- keep login, lock, authentication, drivers, and privileged brokers isolated;
- preserve an independently usable recovery path.

## Userspace hierarchy

The intended hierarchy is:

```text
Kernel
└── Root job
    ├── Root bootstrap process (PID 1)
    ├── Core system job
    │   └── System service manager
    │       ├── Storage and filesystem services
    │       ├── Device manager and driver jobs
    │       ├── Network, logging, policy, and package services
    │       ├── Login and authentication services
    │       └── Other machine-wide services
    ├── Login-session jobs
    │   └── Per-user session manager
    │       ├── Compositor and desktop shell
    │       ├── User-session services
    │       └── Per-application jobs
    └── Recovery job
        └── Minimal diagnostics and repair environment
```

The kernel understands jobs, processes, threads, handles, address spaces, scheduling,
and resource policy. It does not understand desktop sessions, application bundles,
service dependencies, login screens, or package activation.

## Root bootstrap process

The kernel launches exactly one initial userspace process. It is PID 1, but it should
not become the permanent implementation of every machine policy.

Its accepted responsibilities are deliberately narrow:

- establish the root job and initial child-job hierarchy;
- create the bootstrap service namespace and emergency logging path;
- start the system service manager with the capabilities required to manage its subtree;
- supervise and, within bounded policy, restart the system service manager;
- retain only the emergency authorities needed for recovery or final shutdown;
- enter a minimal recovery path when normal userspace cannot be established;
- reap or delegate ownership of orphaned processes according to kernel rules.

PID 1 should not directly implement networking, device policy, package management,
identity databases, desktop sessions, logging storage, or ordinary service dependency
resolution.

During migration, PID 1 and the service manager may be built from one package or share
some code, but their authority and lifecycle roles should remain conceptually separate.
The long-term system must be able to replace or restart the service-manager process
without replacing the kernel or silently transferring unrelated authority.

## Job hierarchy and containment

A job is a hierarchical lifecycle, policy, and resource container. It owns processes and
child jobs, and policy becomes no more permissive down the tree.

Useful top-level jobs include:

```text
Root job
├── Bootstrap job
├── Core-system job
├── Driver job
├── Session root job
└── Recovery job
```

A job may impose:

- process, thread, child-job, and handle limits;
- committed-memory and mapped-memory limits;
- IPC queue-memory limits;
- CPU scheduling class and budget;
- I/O priority and bandwidth policy;
- executable-memory and debugging policy;
- device, interrupt, DMA, and namespace restrictions;
- crash, restart, and descendant-termination policy.

Job policy is defense in depth. Normal authority still comes from handles. A job may
forbid a class of resource even if a buggy service attempts to transfer an otherwise
valid handle into the subtree.

Terminating a job eventually terminates its complete descendant process tree and closes
its handles. This makes logout, application cleanup, driver quarantine, and recovery
transitions deterministic.

## System service manager

The system service manager owns machine-wide lifecycle policy beneath the core-system
job. Its responsibilities are:

- load and validate versioned declarative service definitions;
- resolve dependency and activation graphs;
- create service jobs and processes;
- assemble startup capabilities and restricted service namespaces;
- track activation, startup, readiness, health, degradation, and shutdown;
- apply restart policy, rate limits, backoff, and escalation;
- account CPU, memory, handles, IPC, and other resources;
- coordinate logging, crash records, configuration, and administrative control;
- supervise demand-activated and device-activated services;
- coordinate orderly machine shutdown and recovery escalation.

The service manager must not absorb unrelated service implementations. DNS, logging,
timers, device management, package management, and network configuration remain
separate protocols and services even when the manager activates them.

## Stable service identity

A service has a stable canonical identity independent of process ID or incarnation, for
example:

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

A process ID identifies one implementation process. A **service generation** identifies
one supervised incarnation. Neither should be cached as the durable way to reconnect.

The service manager and broker own the mapping:

```text
stable service identity
        -> current service generation
        -> activation and connection endpoints
```

Logs, crash records, resource usage, and client failures should include both stable
identity and generation so a replacement is never confused with its predecessor.

The implemented route substrate represents a stable identity as a UUIDv4 `ServiceId` plus a
nonzero role ID. A role matters because one service can expose independently authorized authorities;
the logging producer and observer are separate routes under one service ID. The current PID 1 pilot
owns allocation-free monotonic provider-generation sequences for logging, NullFS, tmpfs, and VFS,
and every startup attempt consumes a generation independent of process IDs. PID 1 hands it to the
service in a strict one-use `NSGN` v1 record over a private receive-only bootstrap endpoint; the
service validates PID 1, exact rights, no capability attachment, and canonical encoding before
closing the handle and declaring readiness. The generation binds filesystem sessions and proxy
registrations, while logging also binds its collector, `NSLS`, NSWP, and route publications. For a
filesystem replacement, the kernel preserves the exact old generation as an offline tombstone and
accepts registration only under a strictly newer generation with a fresh endpoint object. A
restartable service manager must eventually own these sequences and receive their current state
across replacement. The current contract provides no durable cross-boot persistence.

## Service definitions

The allocation-free version 1 parser currently covers stable identity, description,
executable and structured arguments, readiness notification, and bounded restart fields.
One fixed PID 1 migration pilot loads and validates an exact definition from the canonical
`/System/services` binding, grants only manager-owned generation and readiness capabilities,
and applies its bounded restart policy. It deliberately does not implement discovery,
enablement, dependency resolution, definition-selected capabilities, or a separate service
manager. A later definition version should eventually include:

- canonical service identity and description;
- executable identity, argument array, environment, and working-directory authority;
- service class and job policy;
- capabilities consumed, provided, and offered to children;
- required service protocols and device-class resources;
- dependencies and ordering constraints;
- activation mode and queued-endpoint limits;
- startup, readiness, health, and shutdown contracts;
- restart condition, rate limit, backoff, and escalation;
- CPU, memory, handle, IPC, I/O, and realtime budgets;
- logging, configuration, audit, and crash-report policy.

Commands are structured argument arrays, not shell strings. Unknown mandatory fields
must fail validation. Merely writing a manifest does not grant capability routes,
restricted identities, or a privileged service class.

## Process construction and startup

A managed service is constructed before it becomes runnable:

1. validate its definition, package identity, executable, and allowed service class;
2. create a service job with non-relaxable resource and security policy;
3. create the process and address space in a suspended state;
4. map its verified executable and runtime;
5. create one bootstrap channel in the known initial handle slot;
6. assemble the explicit startup-capability set;
7. send a versioned startup message through the bootstrap channel;
8. start the initial thread;
9. wait for the declared readiness contract;
10. publish or release activation endpoints only as policy permits.

A startup message may include:

- service identity and generation;
- configuration and schema handles;
- structured logging and lifecycle channels;
- a restricted service namespace;
- job and process-self handles with reduced rights;
- device, storage, network, or other resource capabilities;
- pre-created activation endpoints;
- launch arguments and environment data.

Pathnames and environment variables remain data. They are never substitutes for the
resource handles needed to use a path, service, device, or namespace.

## Lifecycle states

The lifecycle protocol should distinguish desired state from observed state. A useful
observed state machine is:

```text
DEFINED
   |
   v
ACTIVATING
   |
   v
STARTING ---- failure or timeout ----> FAILED
   |
   v
READY <-----> DEGRADED
   |
   v
STOPPING ---- timeout ----> TERMINATING
   |
   v
STOPPED
```

`FAILED` may transition back to `ACTIVATING` under restart policy. A separate
`QUARANTINED` state is appropriate when repeated failures or a security violation make
automatic restart unsafe. The codec milestone represents the bounded control view as `Defined`,
`Activating`, `Starting`, `Ready`, `Degraded`, `Stopping`, `Terminating`, `Stopped`, `Failed`, or
`Quarantined`, independently from desired `Stopped` or `Running`; it does not implement the
transitions. See the
[service control protocol](../service-control-protocol.md).

A process existing is not readiness. A service must satisfy an explicit readiness
contract, such as:

- sending `READY` on its lifecycle channel;
- completing a provider registration handshake;
- proving that required recovery or validation has completed;
- accepting a manager-created health request through its normal dispatch loop.

Dependent capabilities should not be released until required providers are ready.

## Dependency semantics

The definition language should distinguish:

- **requires**: the service cannot operate without the dependency;
- **wants**: the dependency is useful but optional;
- **after**: ordering only, with no runtime authority implication;
- **capability requirement**: a protocol or resource route that any authorized provider
  may satisfy.

Capability dependencies are preferable when the consumer needs a service class or
device resource rather than one named process. Ordering never grants authority.

Dependency graphs must be cycle checked. Synchronous runtime dependency chains should
remain shallow even when startup dependencies are deeper.

## Channel-based activation

NullStar should use channel activation rather than a global pathname or listening-socket
namespace as its native mechanism.

A broker or service manager can create a channel pair before starting the provider:

```text
client endpoint <------------------------> provider endpoint
      |                                      |
returned after policy                delivered at startup
```

Requests may queue within strict limits while the service starts. This provides
race-free demand activation and avoids requiring the provider to bind a privileged
global name.

The implemented `NSRT` stepping stone is userspace-managed and allocation-free. A stable exact-`SEND`
route grant reaches a broker ingress bound to one service-and-role key. The client transfers exactly
one fresh reply capability with its 40-byte request; an accepted reply transfers exactly one
send-only capability for the current provider ingress. The broker authorizes before checking
availability and never parses the service protocol carried on the returned endpoint.

A provider replacement publishes fresh ingress endpoint objects for the new generation. This keeps
old clients isolated from the replacement without pretending to revoke every old delegated handle:
the current kernel has no global capability-revocation primitive. Old ingress handles can remain
live until their holders close them, and the current 32-object endpoint limit makes prompt cleanup
and bounded activation important.

Activation state belongs to the manager or broker, not the disposable service process.
It may preserve:

- the stable service identity;
- queued connection requests;
- restart counters and backoff state;
- definition and configuration references;
- client-visible restarting or unavailable state.

It must not blindly replay non-idempotent application operations after a crash.

## Restart and failure policy

Recommended restart conditions are:

```text
never
on-failure
on-abnormal-exit
on-watchdog
always
```

Every automatic restart policy requires a rate limit, sliding window, and bounded
backoff. A service that repeatedly fails must enter a visible failed or quarantined state
rather than creating a restart storm.

Escalation may include:

```text
restart one service
restart a dependent service subtree
restart one driver family
restart one user session
enter recovery mode
reboot only when recovery is impossible
```

Examples of intended containment are:

- compositor crash: restart the affected graphical session;
- network service crash: restart networking and notify clients;
- ordinary application crash: terminate that application job only;
- noncritical driver crash: mark the device offline, restart, and re-enumerate;
- storage-manager failure with uncertain writes: fail closed and enter recovery policy;
- system service-manager crash: PID 1 applies bounded restart or recovery policy.

## Client behavior across failure

A client must observe distinct transport outcomes such as:

```text
PEER_CLOSED
SERVICE_UNAVAILABLE
SERVICE_RESTARTING
TIMED_OUT
CANCELED
```

The userspace runtime may automatically reconnect only for protocols that explicitly
permit it. Reconnection is reasonable for settings notifications, clipboard, logging, or other
restart-safe services, but reconnection does not imply replay. In particular, an uncertain one-way
logging `Emit` is not replayed on a new generation because the current protocol cannot prove whether
the old provider processed it. Automatic replay is unsafe by default for file mutations, package
installation, firmware update, authentication, and other non-repeatable work.

Protocol definitions should mark requests as idempotent, retry-safe, or non-repeatable
and define the fate of in-flight operations across a provider generation change.

## Health checks and watchdogs

A watchdog should exercise the service's real dispatch path. A detached heartbeat
thread can remain responsive while the primary event loop is deadlocked and is
therefore insufficient by itself.

Health contracts may include:

- lifecycle-channel round trips;
- provider-specific invariants;
- bounded queue progress;
- deadline and latency observations;
- device or storage recovery state;
- memory or handle-pressure conditions.

A health failure can produce `DEGRADED` before restart. Watchdog authority and intervals
are service policy, not application-controlled realtime privilege.

## Graceful shutdown

Machine shutdown is coordinated in stages:

1. stop accepting new login and application launches;
2. notify applications and user services;
3. stop session managers and compositor sessions;
4. stop nonessential machine services;
5. flush filesystem and storage services in dependency order;
6. stop networking and device services where safe;
7. preserve final logs and record final lifecycle state;
8. request final kernel power transition.

Each service receives a stop request with an absolute deadline. It may report readiness
or request a bounded extension, but the manager eventually terminates an unresponsive
job. Critical storage operations must define whether shutdown may proceed, enter
recovery, or fail closed after an uncertain outcome.

## User login sessions

Successful authentication creates a dedicated login-session job:

```text
User-session job
├── Session manager
├── Compositor
├── Desktop shell
├── Audio-session service
├── Clipboard service
├── Notification service
├── User settings and secret brokers
└── Application jobs
```

The session job carries immutable user, login-session, seat, and policy identity. It owns
`Profile/runtime`, session-scoped service routes, and session capabilities. Logout
performs orderly shutdown and then terminates the complete job subtree.

The system service manager creates and supervises the top-level session job, but a
per-session manager owns the desktop and application lifecycle within it.

## Session manager

A session manager is responsible for one logged-in environment:

- start the compositor, desktop shell, and user-session services;
- construct the session-scoped service namespace;
- launch and supervise application jobs;
- coordinate screen locking, logout, and session restoration;
- broker application permission requests through trusted policy services;
- track foreground, background, and user-visible application state;
- contain and recover from session-service failures.

The session manager is not a second unrestricted root service manager. Its capabilities
are bounded to one user and session.

## Application jobs and launch

Each application instance receives its own job:

```text
Application job
├── Main component
├── Declared helper components
├── Renderer or decoder workers
├── Plugin hosts
└── Application crash helper
```

The job carries process, memory, handle, IPC, CPU, background, executable-memory,
debugging, and sandbox policy. Helpers receive only explicitly selected handles. When
the application exits or is terminated, its entire subtree is cleaned up.

A native application launch is:

1. desktop shell asks the application manager to launch a verified application identity;
2. application manager validates bundle, signature, manifest, and requested profile;
3. current user and administrator policy are loaded;
4. an application job is created;
5. baseline and restored capabilities are assembled;
6. the process is created but not started;
7. executable and runtime are mapped;
8. one bootstrap channel and startup message are installed;
9. the initial thread starts;
10. the application reports readiness and may create visible surfaces.

The capability and permission rules are specified in
[Capability-based application sandboxing](application-sandboxing.md).

## Login, authentication, and lock separation

The login UI must not run as an ordinary user application. A trusted login environment
uses narrowly scoped compositor surfaces and a private authentication-service channel.
The UI never receives password databases, verifier secrets, or unrestricted session
authority.

The authentication service owns credential verification, rate limiting, token handling,
and security audit events. On success, a separate session-creation operation issues the
identity and capabilities needed to construct the user-session job.

The lock screen is a trusted session component with compositor support. Ordinary
applications must not draw above it, capture it, synthesize unlock input, or inspect its
authentication traffic. Unlocking restores an existing session; it does not hand
application processes authentication secrets.

## Driver lifecycle

Userspace drivers follow the same supervision model with stronger job restrictions. A
driver host should receive only:

- its matched device capability;
- required MMIO, interrupt, and DMA objects;
- a device-manager control channel;
- logging, lifecycle, configuration, and firmware-broker endpoints;
- class-protocol activation endpoints.

A driver must not enumerate all PCI devices, map arbitrary physical memory, or gain
unrelated device authority. On crash, the device manager advances the provider
generation, marks old sessions stale, applies reset policy, restarts or quarantines the
driver, and notifies clients.

Storage and other stateful drivers require stricter handling because an interrupted
operation may have an uncertain durable outcome and cannot be replayed blindly.

## Logging and configuration

Managed processes receive structured logging endpoints at startup. Records include
stable service or application identity, process and thread identity, generation,
severity, category, trace identity, and bounded structured fields. Sensitive payloads
and secrets are not logged by default.

Configuration should be supplied through scoped, typed configuration handles or
services. A service definition names the configuration it may read or modify; it does
not gain ambient write access to every configuration file.

Broker-owned lifecycle state, persistent service configuration, and disposable process
state remain separate so a crash does not erase policy or silently preserve unsafe
in-process state.

## Recovery environment

PID 1 must retain a path to an independently available recovery environment requiring
as little normal userspace as practical. It should provide:

```text
service and job inspection
crash and boot-log viewing
service disable or quarantine
system-generation rollback
filesystem inspection and repair
driver disablement
log export
controlled reboot or shutdown
```

Recovery tools must not depend on the active dynamic linker, ordinary desktop session,
network service, or writable primary system namespace merely to diagnose a failed boot.

## Administration

The native `sv` client communicates with the appropriate system or session manager over
a versioned protocol. It requests semantic lifecycle transitions rather than searching
for PIDs or sending arbitrary signals.

Observation and control are separate rights. Administrative operations pass through the
authorization broker and result in a narrowly scoped request or one-shot authorization
ticket, not an ambient “become root” state.

## Implementation sequence

The accepted model can be introduced incrementally:

1. **Implemented foundation and bounded pilot:** provide flat capability-backed job
   containment, non-relaxable descendant inheritance, independent process-exit records,
   and bounded whole-job termination. PID 1 assigns policy-pinned definition-backed
   service attempts and every logging, NullFS, tmpfs, and VFS generation while its leader remains
   behind the launch barrier, retains only `SIGNAL | WAIT`, and drains the complete generation to `ECHILD`
   before replacement. NullFS clean replacement preserves the exact quiesce and clean-unmount
   proof before drainage; forced dirty recovery terminates and drains the complete job. Hierarchy
   remains future work.
2. introduce the one-bootstrap-channel startup contract and explicit startup handles;
3. define stable service identity, generation, lifecycle state, readiness, and control
   protocols;
4. extract dependency, restart, and resource policy into a separate system service
   manager while retaining minimal PID 1 recovery supervision;
5. add declarative definitions, bounded activation queues, service-broker integration,
   logging, and configuration handles;
6. add restart backoff, watchdogs, failure escalation, and recovery controls;
7. create login-session jobs and per-session managers;
8. create per-application jobs, mandatory application launch mediation, and permission
   brokers;
9. move userspace drivers into restricted driver jobs with generation-safe recovery;
10. add richer session restoration, background policy, and multi-seat support after the
    lifecycle boundaries are reliable.

## Required invariants

> PID 1 remains a minimal bootstrap and recovery supervisor. Ordinary dependency,
> activation, and service policy belongs to a separately restartable system service
> manager.

> A service identity is stable; a process ID and service generation identify disposable
> implementations.

> Every managed process receives its authority through explicit startup handles before
> it runs. Startup order, pathname, package location, identity, and process parentage do
> not manufacture capabilities.

> Jobs provide hierarchical lifecycle and resource containment. Session logout,
> application termination, driver quarantine, and recovery can clean up complete
> subtrees deterministically.

> Readiness, timeout, cancellation, restart, and uncertain outcomes are explicit parts
> of service protocols rather than inferred from process existence.
