# Userspace architecture direction

## Status

A native capability-oriented userspace API, service-oriented system architecture,
hierarchical jobs, the synthetic logical namespace, the `Profile` directory layout, a
64-bit-native executable environment, and Magnetar generation-based deployment are
**accepted direction**. Complete POSIX behavior, exact `.nspkg` encoding, dynamic-linker
ABI details, and external compatibility profiles remain **tentative design**.

Detailed contracts are split among:

- [IPC, kernel object, and handle model](ipc-and-object-model.md);
- [Service, session, and application lifecycle](service-and-session-lifecycle.md);
- [Capability-based application sandboxing](application-sandboxing.md).

## Layering and ABI

Applications should depend on stable userspace libraries and versioned service protocols
rather than unstable kernel implementation details.

The direct syscall ABI remains focused on mechanism:

- processes, threads, address spaces, and jobs;
- handles, channels, shared memory, and object signals;
- synchronization, clocks, timers, and waiting;
- low-level descriptor, mapping, exception, and compatibility operations.

Policy-heavy features such as filesystems, drivers, networking, graphics, media,
identity, packages, notifications, logging, secrets, and permissions belong in
supervised userspace services.

The Rust-facing platform should separate:

- raw unsafe ABI bindings;
- safe owned and borrowed handle types;
- process startup, allocator, and asynchronous runtime support;
- typed versioned service client and server libraries.

Handle transfer consumes ownership by default, duplication requires the appropriate
right, and ordinary application code should rarely invoke a raw syscall directly.

## Process startup

Native process startup should use one bootstrap channel installed in a known initial
handle slot. The launcher constructs the process while suspended, installs the channel,
sends a versioned startup message containing explicit handles and launch data, and only
then starts the initial thread.

The startup message may contain:

- arguments and environment data;
- standard streams;
- working-directory or rooted-directory authority;
- process-self and job handles with reduced rights;
- package, executable, application, service, component, user, and session identity;
- a restricted service namespace;
- logging and lifecycle endpoints;
- launch-specific resource capabilities.

Arguments, environment variables, paths, and names remain data rather than authority.
Native child processes do not inherit every parent handle implicitly. The current
direct-child grant and deterministic child handle are transitional bootstrap mechanisms,
not the final process-start contract.

## Service discovery

Applications and services discover stable protocols through a restricted namespace or
broker capability rather than fixed process IDs or a globally enumerable bus.

A service lookup:

1. identifies a protocol and compatible version;
2. checks the caller's namespace route and policy;
3. activates the provider when necessary;
4. returns a fresh connected channel endpoint.

Lookup does not manufacture authority. A sandboxed application receives only the service
routes selected for its verified identity, profile, user, and session.

Illustrative stable service identities include:

```text
filesystem.vfs
device.manager
network.stack
network.policy
logging
media.graph
display.session
identity.authentication
packages.magnetar
notifications
portal.desktop
```

Canonical configuration should use unambiguous names even when interactive tools offer
short aliases. Exact names remain tentative until each protocol is specified.

## Protocols and interface bindings

Stable service protocols should define major and minor versions, feature negotiation,
bounded request and collection sizes, handle types and rights, cancellation, retries,
idempotence, and behavior across provider replacement.

The wire format must not depend on native pointers, Rust struct layout, `usize`, compiler
padding, or compiler-private enum representation. A future IDL compiler may generate
Rust bindings, validation, tracing metadata, tests, mocks, and documentation after the
wire rules have been proven by real services.

## Threads and event handling

The platform ABI must not assume applications remain single-threaded. Planned support
includes thread creation, thread-local storage, join and detach, names, affinity, and a
futex-like wait/wake primitive.

A unified object-waiting and event-port model now covers IPC and one-shot timers; it should extend to process
exit, file and network completion, display events, device notifications, and media
events. Subsystems should not invent incompatible polling mechanisms.

## Native API and compatibility

NullStar defines a native capability-aware API first. POSIX and libc behavior is a
compatibility layer over that API so software can be ported without making ambient Unix
authority the native model.

A musl-based libc port is a plausible later path once virtual memory, threading, signals,
filesystem semantics, and dynamic linking are sufficiently stable. Static programs
remain the simpler early target.

Native execution is x86-64-only through the early and medium-term milestones. Public
protocols and file formats remain pointer-size independent so an isolated 32-bit
compatibility environment can be added later if worthwhile.

Freedesktop formats and selected Wayland and D-Bus protocols are compatibility
contracts, not native IPC or authorization. Privileged external interfaces translate
through native services and portals. See
[Freedesktop compatibility](freedesktop-compatibility.md) and
[Graphics stack and compositor](graphics-stack.md).

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
`Applications`, and `Users` trees projected into canonical paths through namespace
bindings rather than symbolic links:

```text
/Volumes/NullStar/System        => /System
/Volumes/NullStar/Applications  => /Applications
/Volumes/NullStar/Users         => /Users
```

Applications use canonical logical paths or, preferably, rooted directory capabilities.
Stable volume and node identities allow the backing layout to move to separate,
encrypted, or read-only volumes later. The complete transition is described in
[Filesystem namespace and boot](filesystem-namespace.md).

A user home uses familiar visible directories and one system-managed profile directory:

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

`Profile` is an accepted name. Its lowercase children are functional categories:

- `config`: user preferences and durable configuration;
- `cache`: regenerable content that may be removed automatically;
- `state`: persistent operational state such as sessions and histories;
- `data`: durable application-managed data not presented as ordinary documents;
- `logs`: user-session and application diagnostic records;
- `runtime`: sockets, locks, and ephemeral per-login state.

Applications obtain role-specific directory capabilities from the runtime using their
verified application identity rather than constructing paths. The backing layout may be
category-first, for example `Profile/cache/org.nullstar.Player/`, without making that
physical path the access-control token.

`Profile` should not rely on dot-prefix hiding. Filesystem or desktop metadata marks it
system-managed and hidden by default in graphical tools. Terminal visibility remains a
shell policy. Backup and cleanup tools may use category metadata to include durable data
while excluding cache and runtime contents.

For ported applications, XDG configuration, cache, state, and data homes map to the
corresponding `Profile` categories. XDG runtime compatibility uses a private
session-scoped projection below `Profile/runtime` and is invalidated when the login
session ends.

## Application model

An application is a verified bundle containing a manifest, executables, component roles,
private libraries, resources, localization, icons, requested capabilities, and optional
exported services or plugins. Its stable signed application identity names private
storage and permission policy.

Every application-runtime launch is sandboxed regardless of whether the bundle is under
`/System/Applications`, `/Applications`, `/Users/<user>/Applications`, or another
location. Location controls installation scope and update policy, not privilege.

Each application instance receives a job that controls lifecycle, process membership,
resource limits, scheduling policy, executable-memory policy, and capability routing.
Helper processes receive explicit reduced handle sets rather than ambient inheritance.

Sensitive operations use portal and provider protocols for selected files, microphone,
camera, screen capture, clipboard reads, secrets, local-network discovery, global
shortcuts, devices, sharing, and URL opening. The result is a narrow capability or
stream, not broad filesystem, device, input, desktop, or network authority.

## System and session services

PID 1 remains a minimal bootstrap and recovery supervisor. A separately restartable
system service manager owns ordinary machine-service definitions, dependencies,
activation, readiness, health, restart policy, limits, logging integration, and shutdown.

A successful login creates a dedicated session job. A per-session manager owns that
user's compositor, desktop shell, session services, application jobs, logout, lock, and
session restoration. Machine and user-session scopes use the same lifecycle concepts but
different capability boundaries.

The native `sv` client inspects and requests transitions through versioned manager
protocols. It does not manage services by searching for PIDs. See
[Service management and command-line direction](service-management-and-cli.md).

Userspace also needs common services for structured logs, crash records, configuration,
service health, network attribution, device supervision, authorization, permissions, and
secrets. Native services, applets, drivers, compatibility processes, and plugin hosts
should be restartable without destabilizing unrelated jobs.

The userspace-first hardware boundary is described in
[Driver model](driver-model.md); application-aware firewall policy in
[Network policy and firewall](network-policy.md); and logging in
[Logging, journal, and rotation](logging.md).

## Identity and authorization

Authenticated user and service identity complements rather than replaces capabilities.
A broker may use verified identity, session, signing, and administrator policy when
deciding whether to issue a capability. Identity alone cannot create a kernel object or
amplify rights.

Native administrative tools request narrow semantic operations through an
authorization service. A successful decision returns an operation-bound, expiring or
single-use ticket or causes the privileged service to perform the action. Graphical
applications are not relaunched with ambient root authority.

## Command-line environment

Essential boot and recovery utilities should be native Rust programs that do not depend
on a complete libc, dynamic linker, or GNU installation. Common POSIX behavior and
selected GNU extensions can be added deliberately.

GNU coreutils are a later compatibility milestone and test workload. They must not
become required to boot, repair NullFS, inspect services, read logs, or restore the
system.

`ush` remains the native shell while its scripting support evolves. A future `sh`
compatibility target should make an explicit promise rather than silently redefining
`ush` behavior.

## Desktop userspace

The compositor is the trusted boundary for surfaces, input, capture, clipboard,
window-level effects, and secure UI. Applications cannot inspect other clients' buffers
or input. Panels and docks may occupy each screen edge and run as nested compositors
whose applets are separate supervised jobs.

The native Rust UI stack should combine a backend-independent vector/raster renderer,
SVG-first assets, accessible widgets, and a constrained CSS-derived style system. These
are specified in [Graphics stack and compositor](graphics-stack.md) and
[Native graphics renderer and UI toolkit](graphics-renderer-and-toolkit.md).

## Executables and linking

NullStar is a 64-bit-native platform. The kernel, system services, userspace drivers,
recovery environment, native applications, and native machine-code libraries use the
x86-64 ABI. Architecture-independent assets may be packaged as `any`; native 32-bit
execution is deferred to an optional compatibility milestone.

Bootstrap, recovery, and early services should remain statically linked. Dynamic linking
is introduced only after ELF loading, virtual memory, threads, stable library ABI rules,
immutable deployments, and rollback are reliable. A process may load only libraries
built for its own architecture and ABI. Rust's compiler-private ABI is not a stable
shared-library contract.

Executable profiles, PIE/ASLR, dynamic-loader policy, TLS, RELRO, library search,
NullStar ELF notes, build IDs, and deferred 32-bit compatibility are specified in
[Executable loading and linking](executable-loading.md).

## Packages and deployments

The native package and deployment manager is **Magnetar**, invoked as `mag`. Native
archives use the `.nspkg` suffix.

Magnetar verifies immutable package objects, resolves dependencies, constructs a
complete generation, commits it durably, atomically selects it, performs bounded health
confirmation, and retains a previous healthy generation for rollback.

System and application deployments may have different activation scopes. Mutable
configuration, state, logs, caches, and user data remain outside immutable package
payloads and require explicit migration and rollback rules.

Package identity and signing provide inputs to service, driver, application-profile, and
entitlement policy; installing a package does not itself grant ambient runtime authority.
See [Magnetar package and deployment management](package-management.md).

## Open questions

- Exact capitalization and localization rules for ordinary visible home directories.
- The canonical `.nspkg` container, manifest encoding, and version-comparison rules.
- Whether the service broker is part of the system service manager or a separate early
  service.
- The initial libc scope and POSIX compatibility target.
- The stable platform shared-library ABI and first dynamic-loader relocation subset.
- Filesystem metadata representation for system-managed and backup-policy attributes.
- Exact service and protocol names exposed to compatibility environments.
