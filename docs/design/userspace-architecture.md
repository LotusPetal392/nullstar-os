# Userspace architecture direction

## Status

A native capability-oriented userspace API, service-oriented system architecture,
application jobs, the synthetic logical namespace, the `Profile` directory layout,
a 64-bit-native executable environment, and Magnetar generation-based deployment are
**accepted direction**. Complete POSIX behavior, exact `.nspkg` encoding, dynamic-
linker ABI details, and external compatibility profiles remain **tentative design**.

## Layering and ABI

Applications should depend on stable userspace libraries and versioned service
protocols rather than unstable kernel implementation details.

The direct syscall ABI should remain focused on mechanism:

- processes, threads, address spaces, and jobs;
- handles, capabilities, IPC, and shared memory;
- synchronization, clocks, timers, and waiting;
- low-level descriptor and exception operations.

Policy-heavy features such as filesystems, drivers, networking, graphics, media,
identity, packages, notifications, logging, and secrets belong in userspace services.

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
- `device.manager`;
- `system.network`;
- `network.policy`;
- `system.logging`;
- `system.media`;
- `system.display`;
- `system.input`;
- `system.identity`;
- `system.packages`;
- `system.notifications`;
- `system.portal`.

Names are illustrative until each protocol is specified. Canonical configuration
should use unambiguous names even when interactive tools provide shorter aliases.

## Threads and event handling

The platform ABI should not assume applications remain single-threaded. Planned
support includes thread creation, thread-local storage, join and detach, thread names,
affinity, and a futex-like wait/wake primitive.

A unified completion or event-waiting model should cover IPC, timers, process exit,
file and network completion, display events, device notifications, and media events.
Subsystems should not invent incompatible polling mechanisms.

## Native API and compatibility

NullStar should define a native capability-aware API first. POSIX and libc behavior
should be implemented as a compatibility layer over that API so software can be
ported without making ambient Unix authority the native system model.

A musl-based libc port is a plausible later path once virtual memory, threading,
signals, filesystem semantics, and dynamic linking are sufficiently stable. Static
programs remain the simpler early target. Native execution is x86-64-only through the
early and medium-term milestones; public protocols and file formats remain pointer-
size independent so an isolated 32-bit compatibility environment can be added later.

Freedesktop formats and selected Wayland and D-Bus protocols are desktop compatibility
contracts, not native IPC or authorization. Privileged external interfaces translate
through native services and portals. See
[Freedesktop compatibility](freedesktop-compatibility.md) and
[the graphics-stack design](graphics-stack.md).

## Filesystem namespace

The major user-visible namespace uses short descriptive names:

```text
/System
/Users
/Volumes
/Applications
```

The long-term root is a synthetic VFS namespace rather than the root directory of one
disk filesystem. The primary NullFS volume is expected to provide `System`,
`Applications`, and `Users` trees that the VFS projects into their canonical paths
through namespace bindings, not symbolic links.

```text
/Volumes/NullStar/System        => /System
/Volumes/NullStar/Applications  => /Applications
/Volumes/NullStar/Users         => /Users
```

Applications use only the canonical logical paths. Stable volume and node identities
allow the backing layout to move to separate, encrypted, or read-only volumes later.
The complete transition and boot model is in
[Filesystem namespace and boot](filesystem-namespace.md).

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
identifier instead of constructing paths. The resulting layout is category-first, for
example `Profile/cache/org.nullstar.Player/`.

`Profile` should not rely on dot-prefix hiding. The filesystem or desktop metadata
marks it system-managed and hidden by default in a graphical file manager. Terminal
visibility remains a shell policy. Backup and cleanup tools may use category metadata
to include configuration and data while excluding cache and runtime contents.

For ported applications, the XDG configuration, cache, state, and data homes map to the
corresponding `Profile` categories. XDG runtime compatibility must use a private
session-scoped binding below `Profile/runtime` and be cleared with the login session.

## Application model and sandboxing

An application is a bundle containing a manifest, executables, private libraries,
resources, localization, icons, requested capabilities, and optional service, driver,
or plugin declarations. The stable application identifier names profile storage and
sandbox policy.

Applications should launch inside job objects that control lifecycle, process
membership, memory and CPU limits, I/O priority, scheduling class, and capability
inheritance.

Desktop applications eventually run sandboxed by default. Sensitive operations use
portal-like brokers for user-selected files, microphone and camera access, screen
sharing, clipboard transfer, secrets, local-network discovery, global shortcuts, and
URL opening. The result is a narrow capability or stream to the approved resource
rather than broad filesystem, device, input, or desktop access.

## System services

PID 1 should evolve into a declarative service manager with named units, dependencies,
readiness, restart policy, backoff, capability grants, service identities, limits, and
structured logging. Startup commands remain structured argument arrays rather than
shell strings.

The native `sv` client should inspect and request state transitions through the
service-manager protocol. It does not manage services by searching for PIDs. See
[Service management and command line](service-management-and-cli.md).

Userspace also needs common infrastructure for structured logs, crash records,
configuration layering, service health, network attribution, device supervision, and
authorization. Native services, applets, drivers, compatibility processes, and plugin
hosts should be restartable without destabilizing the desktop session.

The logging contract, retention, rotation, and syslog compatibility are described in
[Logging, journal, and rotation](logging.md). The userspace-first hardware boundary is
described in [Driver model](driver-model.md), and application-aware firewall policy is
described in [Network policy and firewall](network-policy.md).

## Command-line environment

Essential boot and recovery utilities should be native Rust programs that do not
depend on a complete libc, dynamic linker, or GNU installation. Common POSIX behavior
and selected GNU extensions can be added deliberately.

Actual GNU coreutils are a later compatibility milestone and test workload. They must
not become required to boot, repair NullFS, inspect services, read logs, or restore the
system.

`ush` remains the native shell while its scripting support evolves. A future `sh`
compatibility target should make an explicit promise rather than silently redefining
`ush` behavior.

## Desktop userspace

The compositor is the trusted boundary for surfaces, input, capture, clipboard,
window-level effects, and secure UI. Applications cannot inspect other clients'
buffers or input. Panels and docks may occupy each screen edge and run as nested
compositors whose applets are separate supervised jobs.

The native Rust UI stack should combine a backend-independent vector/raster renderer,
SVG-first assets, accessible widgets, and a constrained CSS-derived style system.
These are specified in [Graphics stack and compositor](graphics-stack.md) and
[Native graphics renderer and UI toolkit](graphics-renderer-and-toolkit.md).

## Executables and linking

NullStar is a 64-bit-native platform. The kernel, system services, userspace drivers,
recovery environment, native applications, and native machine-code libraries use the
x86-64 ABI. Architecture-independent assets may be packaged as `any`; native 32-bit
execution is deferred to an optional compatibility milestone.

Bootstrap, recovery, and early services should remain statically linked. Dynamic
linking is introduced only after ELF loading, virtual memory, threads, stable library
ABI rules, immutable deployments, and rollback are reliable. A process may load only
libraries built for its own architecture and ABI. Rust's compiler-private ABI is not a
stable shared-library contract.

The executable profile, PIE/ASLR path, dynamic-loader policy, TLS, RELRO, library search,
NullStar ELF notes, build IDs, and deferred 32-bit compatibility are specified in
[Executable loading and linking](executable-loading.md).

## Packages and deployments

The native package and deployment manager is **Magnetar**, invoked as `mag`. Native
archives use the `.nspkg` suffix.

Magnetar is a reliable deployment system rather than a tool that overwrites the active
system file by file. It verifies immutable package objects, resolves dependencies,
constructs a complete generation, commits it durably, atomically selects it, performs
bounded health confirmation, and retains a previous healthy generation for rollback.

System and application deployments may have different activation scopes. Mutable
configuration, state, logs, caches, and user data remain outside immutable package
payloads and require explicit migration and rollback compatibility rules.

The archive model, manifests, repositories and trust, mirror ranking, dependency
semantics, manual and automatic package tracking, pruning, generations, boot fallback,
configuration handling, garbage collection, recovery, and CLI are specified in
[Magnetar package and deployment management](package-management.md).

## Open questions

- Exact capitalization and localization rules for ordinary visible home directories.
- The canonical `.nspkg` container, manifest encoding, and version comparison rules.
- Whether the service broker is part of PID 1 or a separate supervised service.
- The initial libc scope and POSIX compatibility target.
- The stable platform shared-library ABI and first dynamic-loader relocation subset.
- Filesystem metadata representation for system-managed and backup-policy attributes.
- Exact service names and the compatibility environment projected for unbundled ports.
