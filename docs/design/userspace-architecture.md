# Userspace architecture direction

## Status

A native capability-oriented userspace API, service-oriented system architecture,
application jobs, and the `Profile` directory layout are **accepted direction**.
Complete POSIX behavior, package details, and dynamic-linker policy remain
**tentative design**.

## Layering and ABI

Applications should depend on stable userspace libraries and versioned service
protocols rather than unstable kernel implementation details.

The direct syscall ABI should remain focused on mechanism:

- processes, threads, address spaces, and jobs;
- handles, capabilities, IPC, and shared memory;
- synchronization, clocks, timers, and waiting;
- low-level descriptor and exception operations.

Policy-heavy features such as filesystems, networking, graphics, media, identity,
packages, notifications, and secrets belong in userspace services.

The Rust-facing platform should separate:

- raw unsafe ABI bindings;
- safe owned and borrowed handle types;
- process startup and allocator support;
- versioned service client libraries.

Handle transfer should consume ownership, duplication should require the appropriate
right, and ordinary application code should rarely invoke a raw syscall directly.

## Startup and service discovery

A versioned startup block should provide arguments, environment, standard streams,
initial handles, job membership, executable identity, working-directory state, and a
service-broker endpoint.

Applications discover named services through a broker rather than fixed process IDs.
A service lookup is subject to launch policy and does not manufacture authority.
Protocols advertise a name, major version, minor version, and optional feature set.

Examples of stable service names include:

- `system.filesystem`;
- `system.network`;
- `system.media`;
- `system.display`;
- `system.input`;
- `system.identity`;
- `system.notifications`.

## Threads and event handling

The platform ABI should not assume applications remain single-threaded. Planned
support includes thread creation, thread-local storage, join and detach, thread names,
affinity, and a futex-like wait/wake primitive.

A unified completion or event-waiting model should cover IPC, timers, process exit,
file and network completion, display events, and media notifications. Subsystems
should not invent incompatible polling mechanisms.

## Native API and compatibility

NullStar should define a native capability-aware API first. POSIX and libc behavior
should be implemented as a compatibility layer over that API so software can be
ported without making ambient Unix authority the native system model.

A musl-based libc port is a plausible later path once virtual memory, threading,
signals, filesystem semantics, and dynamic linking are sufficiently stable. Static
programs remain the simpler early target.

## Filesystem namespace

The major user-visible namespace uses short descriptive names:

```text
/System
/Users
/Volumes
/Applications
```

A user home should use familiar visible content directories and one system-managed
profile directory:

```text
/Users/<username>/
├── Desktop/
├── Documents/
├── Downloads/
├── Music/
├── Pictures/
├── Videos/
├── Public/
└── Profile/
    ├── config/
    ├── cache/
    ├── state/
    ├── data/
    ├── logs/
    └── runtime/
```

`Profile` is an accepted name. Its lowercase children are functional categories,
while the capitalized directory marks a major user-facing namespace.

Directory contracts are:

- `config`: user preferences and durable configuration;
- `cache`: regenerable content that may be removed automatically;
- `state`: persistent operational state such as sessions and histories;
- `data`: durable application-managed data not presented as ordinary documents;
- `logs`: user-session and application diagnostic records;
- `runtime`: sockets, locks, and ephemeral per-login state.

Applications should obtain these paths from the runtime using their stable application
identifier instead of constructing paths. The resulting layout is category-first,
for example `Profile/cache/org.nullstar.Player/`.

`Profile` should not rely on dot-prefix hiding. The filesystem or desktop metadata
marks it system-managed and hidden by default in a graphical file manager. Terminal
visibility remains a shell policy. Backup and cleanup tools may use category metadata
to include configuration and data while excluding cache and runtime contents.

## Application model and sandboxing

An application is a bundle containing a manifest, executables, private libraries,
resources, localization, icons, requested capabilities, and optional service or
plugin declarations. The stable application identifier names profile storage and
sandbox policy.

Applications should launch inside job objects that control lifecycle, process
membership, memory and CPU limits, I/O priority, and capability inheritance.

Desktop applications eventually run sandboxed by default. Sensitive operations use
portal-like brokers for user-selected files, microphone access, screen sharing,
clipboard transfer, secret storage, and URL opening. The result is a narrow capability
to the approved resource rather than broad filesystem or device access.

## System services

PID 1 should evolve into a declarative service manager with named units, dependencies,
readiness, restart policy, backoff, capability grants, service identities, limits,
and structured logging. Startup commands remain structured argument arrays rather
than shell strings.

Userspace also needs early common infrastructure for structured logs, crash records,
configuration layering, and service health. Native services and plugin hosts should
be restartable without destabilizing the desktop session.

## Packages and dynamic linking

Packages should be verified, staged, and atomically activated. Manifests eventually
cover signatures, hashes, dependencies, architecture, ABI requirements, ownership,
and rollback. Installed applications must not write into their own bundle during
normal operation.

Dynamic linking is deferred but should eventually support ELF shared objects, TLS,
ASLR, RELRO, versioned symbols, controlled search paths, and application-private
libraries. Current-directory library loading should not be part of the default policy.

## Open questions

- Exact capitalization and localization rules for ordinary visible home directories.
- Application bundle and package manifest formats.
- Whether the service broker is part of PID 1 or a separate supervised service.
- The initial libc scope and POSIX compatibility target.
- Filesystem metadata representation for system-managed and backup-policy attributes.
