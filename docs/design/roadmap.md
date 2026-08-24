# Architecture design roadmap

This roadmap separates implemented work from future architectural direction. It does
not replace milestone-specific plans elsewhere in the repository. Current implementation
documents remain authoritative for behavior that exists today.

## IPC and kernel-object foundations

The current system already has bounded process-local capability tables, rights-reduced
copying and atomic replacement, diagnostic object identity, endpoints, counted notifications, manual-reset events,
atomic one- and bounded multi-handle move-transfer, copied shared memory, level-triggered endpoint, notification, job,
timer, and event signal snapshots, absolute-deadline single- and bounded many-object waiting, bounded persistent
tagged wait sets, bounded queued edge-event ports, one-shot monotonic timers, endpoint waiting,
atomic channel pairs with final-reference peer closure,
direct-child bootstrap grants, and an initial userspace layer of non-cloneable owned, borrowed, and
kind-validated handles with automatic close and retry-safe ownership-consuming move transfer. A
bounded allocation-free scoped reactor also provides asynchronous endpoint send, receive, and move
send over the existing many-object wait ABI. It now propagates nested absolute deadlines, separates
signal-only cancellation sources from clonable wait-only tokens, and supplies coalescing periodic
schedules over one-shot timers. A fixed-capacity task executor maps generation-tagged reactor waits
onto queued event ports, schedules only ready task slots, and retains fixed-depth role attribution,
ancestor cancellation, inherited deadlines, bounded shutdown draining, and distinct terminal outcomes
without heap allocation. A bounded lifecycle trace now retains sequence-ordered task transitions with
overwrite accounting. Typed generic readiness, counted-notification consumption, and hierarchical
job-exit futures feed the same registration machinery. A fixed-capacity blocking-work coordinator adds
FIFO admission, logical worker bounds, task-group attribution, pre-dispatch cancellation and deadline
checks, shutdown conversion, retained outcomes, and bounded tracing; actual parallel/preemptible workers
still depend on thread/address-space support. Role-specific process contexts own explicitly supplied capabilities, validate
typed claims, tighten rights, and construct client/server-typed bindings from bounded protocol
descriptors whose common transport invariants have host and QEMU conformance coverage. Canonical
fragmented process-start data frames now add bounded typed identity, arguments, compatibility
environment, and launch metadata without treating any descriptive field as authority. The
definition-backed service, logging, NullFS, tmpfs, and VFS now use the live one-channel path: PID 1 queues their
role-tagged `NSPC` capabilities, required `NSPD` sections, and `NSPX` end record before
launch-barrier release. Logging, NullFS, tmpfs, and VFS additionally use a shared fail-closed receiver and carry
manager generation in PID 1-authenticated launch data instead of a separate endpoint. PID 1 also moves
generation-local transfer-only authorities for logging and NullFS, reacquiring them for replacements.
The ordinary shell loader now applies the same capability-empty contract to single commands and every
pipeline stage, using a private internal barrier before the existing process-group barrier. Converted
tool entry points pin their parent and validate canonical identity, arguments, environment, and an
otherwise empty initial capability table. Managed spawn now waits for the child to discard inherited
capabilities before installing bootstrap handle 1. Every bundled exec caller carries the managed
stream through image replacement as a same-PID handoff, including CWD-relative resolution, repeated
failure cleanup, and probes that explicitly discard inherited authority once it is no longer needed.
PID 1 now uses typed capability-bearing managed-tool startup for `sv`, `logctl`, and its foreground
`ush`, separating logging observation and service-control observation/mutation by role and exact
rights. Filesystem and fault-injection probe launchers retain transitional startup paths, and the
kernel smoke-test launch of `ush` retains one explicit mixed-launch compatibility entry. The next
architecture stages are:

1. finish formalizing common kernel-object ownership, typed handles, immutable rights, and
   inspection authorization around the implemented close, duplicate, atomic replace, inspection,
   diagnostic object identity, and signal-state operations;
2. evolve the implemented process and immutable hierarchical-job containment into distinct thread
   and address-space abstractions plus broader hierarchy-scoped resource policy beyond the
   implemented process-count ceiling;
3. evolve the implemented channel pairs, peer closure, and atomic multi-handle move-transfer with
   sender-side receiver-capacity reservation and explicit per-job backpressure accounting;
4. evolve shared-memory objects from bounded copies to mapped pages with protection,
   sealing, W^X integration, and job accounting;
5. extend the implemented scoped cancellation foundation into bounded synchronous call/reply,
   late-reply cleanup, protocol cancellation, and priority donation;
6. extend the implemented queued event ports and typed job-exit completion beyond current object signals
   to file and network completion, display, device, and media event sources;
7. integrate the implemented typed service bindings, bounded tracing, capability-bearing process
   contexts, and protocol conformance checks with the eventual startup message, generated bindings,
   privacy metadata, and real service migrations;
8. introduce an IDL only after stable wire and lifecycle conventions have survived real
   services.

The detailed contract is in
[IPC, kernel object, and handle model](ipc-and-object-model.md).

## Process startup and service lifecycle

1. **Implemented foundation and service integration:** add capability-backed job
   containment, inherited descendants, independent FIFO process-exit observation, and
   bounded whole-job termination. ABI 1.16 also adds immutable child-job creation with recursive
   inspection, drainage, and termination; ABI 1.17 adds a tightening-only subtree process ceiling.
   ABI 1.18 adds permanent empty-leaf retirement and bounded reclamation; ABI 1.19 makes the local
   process ceiling observable through read-only job authority. PID 1 assigns
   policy-pinned definition-backed
   service attempts and every logging, NullFS, tmpfs, and VFS generation to fresh jobs before
   launch-barrier release, retains only `SIGNAL | WAIT`, and drains each complete generation to `ECHILD` before
   replacement. NullFS preserves exact quiesce, clean-unmount, and final-exit evidence before clean
   drainage; forced dirty recovery terminates and drains the complete job. PID 1 service generations
   remain flat roots; session and application hierarchy integration remains future work.
2. complete the partially migrated general-loader path by covering PID 1's remaining filesystem and
   fault-injection probes and other managed processes, then remove the explicit kernel-direct `ush`
   compatibility entry;
3. define stable service identity, service generation, lifecycle state, readiness,
   control, and failure protocols;
4. keep PID 1 as a minimal bootstrap and recovery supervisor while moving ordinary
   dependency, activation, restart, and resource policy into a separately restartable
   system service manager;
5. add declarative definitions, dependency validation, capability requirements,
   channel activation, restart budgets, watchdogs, resource limits, and quarantine;
6. integrate structured logging, configuration handles, administrative authorization,
   service inspection, and recovery controls;
7. create per-login session jobs and session managers;
8. create per-application jobs and explicit component-role launches;
9. move userspace drivers into restricted driver jobs with provider-generation recovery;
10. add richer session restoration, background policy, and multi-seat support only after
    lifecycle containment is reliable.

In the service-management implementation sequence, the
[allocation-free `NSVC` v1 contract](../service-control-protocol.md) now provides a host-testable exact
64-byte codec, native endpoint transport, a temporary PID 1 registry, implemented `sv list`, `sv
status SERVICE`, and separately authorized mutation. `sv restart SERVICE` remains generic, while live
`sv start logging` and `sv stop logging` exercise bounded desired-state convergence, immediate route
withdrawal, fresh route objects, and manager-owned generation replacement without charging failure
policy. Logging restart retains a fence through replacement startup, rejects a queued duplicate with
`Busy`, and escalates from cooperative termination to uncatchable signal 9 after a bounded grace
period. A separate bounded readiness deadline prevents a live but unready logging child from holding
replacement convergence indefinitely and feeds expiry into normal restart/backoff policy. Controlled
NullFS restart now queues a private `NFLC` v1 `QUIESCE` marker behind earlier FIFO work. Exact
`QUIESCED` lets PID 1 offline the exact generation and wake tail work with `EIO`; `UNMOUNT` then closes
core handles, syncs and publishes a clean superblock, emits exact `CLEAN_UNMOUNTED`, and exits `0`.
Only that exact event plus final exit `0` proves a clean path. Timeout, invalid lifecycle traffic,
failure, or early/nonzero exit triggers exact-generation offlining, whole-generation-job termination
and drainage, and dirty recovery.
Replacement still uses a fresh endpoint and strictly newer generation, and controlled restart charges
no failure budget. Filesystem `Start` and `Stop` stay exactly `Unsupported`; `NSVC` v1 and the public
filesystem version 1 `Request`/`Reply` operations are unchanged. ABI 1.15 supplies flat jobs,
non-relaxable descendant inheritance, independent exit records, and whole-job termination; ABI 1.16
adds immutable child creation with recursive inspection, drainage, and termination; ABI 1.17 adds
hierarchy-scoped process ceilings; ABI 1.18 adds safe drained-leaf retirement; ABI 1.19 adds
read-only process-ceiling inspection. PID 1 now
assigns each policy-pinned definition-backed service attempt and every logging, NullFS, tmpfs, and VFS
generation before barrier release, retains only `SIGNAL | WAIT`, and drains the old job to `ECHILD`
before replacement.
Logging keeps cooperative process-group termination but uses whole-job KILL for escalation and escaped
descendants. The logging-lifecycle QEMU gate also injects escaped-process-group descendants into tmpfs
and VFS generations and requires descendant termination, whole-job drainage to `ECHILD`, and
replacement. The NullFS restart, crash-recovery, and provider-loss gates additionally preserve its
durability protocol while proving escaped-descendant termination and complete job drainage. A separate
manager process, general activation, and cross-reboot desired-state persistence remain future work.

See [Service, session, and application lifecycle](service-and-session-lifecycle.md) and
[Service management and command line](service-management-and-cli.md).

## Application isolation and permissions

The native launch foundation now scrubs inherited descriptors and capabilities, assigns a bounded
application job before release, delivers typed startup identity and authority, and creates
`desktop-child` and `worker` components from explicit profile-specific capability allowlists. Launch
now additionally requires package-verifier output to match an installed generation's stable
application/publisher lineage, provenance, component executable, user scope, and authorized profile.
Desktop roots now also require identity-bound private-storage and restricted service-namespace
endpoints, canonical relative-root policy keeps bundle access read-only, and a one-way inherited
kernel seal removes ambient global-path operations after managed exec. The restricted namespace now
uses one immutable multi-route NSRT ingress for the baseline display, lifecycle, settings,
logging-producer, audio-playback, and portal routes; policy denial precedes provider availability,
and published routes retain generation-bound endpoint issuance. Concrete providers, provider-backed
directory provisioning, cryptographic package and registry services, and a standalone application
manager remain future work; application lifecycle supervision is the next launch layer.

1. require every graphical bundle to launch through the application manager regardless
   of installation path;
2. establish stable signed application identity and technical sandbox profiles;
3. provide private bundle, data, cache, temporary, and runtime capabilities plus a
   restricted service namespace;
4. implement file, save, directory, drag-and-drop, and share portals with persistent
   resource identities and permission records;
5. add microphone, camera, screen-capture, contextual clipboard, and trusted active-use
   indicators;
6. add provider-controlled leases, expiration, revocation, and reduced child delegation;
7. route outbound networking, local-network discovery, listeners, and device access
   through application-attributed brokers;
8. add explicit multi-process component roles, application-exported services, isolated
   plugin hosts, and application groups;
9. add visible background leases and start-at-login controls;
10. add operation-specific administrative tickets, compatibility profiles, policy
    linting, and capability-graph inspection.

See [Capability-based application sandboxing](application-sandboxing.md).

## Namespace and persistent storage

All three primary-volume namespace bindings and the bounded NullFS Phase 5 acceptance gates
are implemented. VFS namespace-routing protocol version 2 uses a bounded 224-byte reply
with explicit binding metadata. The VFS service owns exact `/System`, `/Applications`, and
`/Users` records targeting matching nodes below the UUID-selected NullFS provider's backend
root. Raw matching paths below `/Volumes/NullStar` remain administrative aliases, while cwd
and open-file paths remain canonical. Public filesystem protocol v1 and `NSVC` v1 are
unchanged.

- **Implemented:** preserve the VFS as owner of a synthetic logical root.
- **Implemented:** give the primary NullFS volume a stable UUID and human-facing
  `/Volumes/NullStar` identity.
- **Implemented:** populate `System`, `Applications`, and `Users` trees on the primary
  volume.
- **Implemented:** project all three persistent trees through namespace bindings rather
  than symbolic links.
- **Current coverage:** preserve canonical paths and underlying node identity across raw
  and bound views; enforce read-only System policy, test system metadata flags, a static
  `/System/bin` executable, canonical application and user-profile mutation, stale
  descriptors across service restart, and bootstrap availability.
- **Implemented foundation:** writable NullStar filesystem-service authority, controlled
  shutdown ordering, clean/dirty recovery, and bounded public mutation. A fully allocated service
  image proves exact data-block and inode exhaustion, continued reads, reclamation, and subsequent
  public mutation. Exact UUID- and generation-fenced block-endpoint offlining now proves explicit
  `EIO`, uncertain-mutation fail-stop, stale filesystem-generation failure, and bootstrap continuity.
  A capability-gated post-commit/pre-reply service crash proves non-retried public `EIO`, exact
  old-generation offlining, dirty remount, stale descriptors, and single-copy durable recovery.
  A separate three-boot disposable-image gate stages a generation into the inactive firmware slot,
  selects it through canonical and mirrored checksummed records, rolls back, and verifies both
  retained generations without modifying the generated source image.
- **Partially implemented:** load non-bootstrap programs and service definitions through
  the bound `/System` tree; static executable loading, the bounded allocation-free
  definition parser, and one policy-pinned PID 1 activation pilot are implemented. General
  discovery, enablement, dependency resolution, and a separate manager remain future work.
- **Implemented acceptance foundation:** `/System/boot` contains deterministic retained
  generations and one canonical version 1 selection record. The targeted synchronizer mirrors two
  artifact slots plus the selector to firmware-readable FAT and proves selection and rollback across
  three boots. Production manifests, authenticated health, attempt policy, redundant selectors, and
  an ordinary long-running synchronization service remain future work.
- **Implemented acceptance gate:** a generated image with no primary NullFS partition proves exact
  UUID lookup failure and handoff to the independently available emergency kernel shell. Retaining
  that recovery independence remains an ongoing requirement.

## Driver and device evolution

- Define common device identity, ownership generation, reset, and discovery records.
- Add verified driver manifests, deterministic matching, and a supervised device
  manager.
- Add dynamic devfs provider registration and generation-scoped sessions.
- Introduce constrained PCI configuration, MMIO, IRQ, pinned DMA, and later IOMMU-domain
  capabilities.
- Move a queue-oriented virtual block or network driver to userspace first.
- Separate controller drivers from block, network, input, audio, display, radio, and
  other class policy.
- Add hotplug, firmware brokerage, suspend/resume, crash recovery, quarantine, and
  driver rollback.
- Keep raw device enumeration and transfer separate from higher-level device-class
  services and application portals.

## Desktop scheduler evolution

- Add multilevel feedback scheduling with interactive wakeup preemption.
- Add priority inheritance for locks and bounded synchronous IPC.
- Introduce restricted, admitted, and budgeted realtime scheduling.
- Move scheduler state and preemption accounting to per-CPU structures.
- Add SMP, per-CPU run queues, affinity, and bounded load balancing.
- Evolve timers toward deadline-driven tickless operation.
- Add job-level CPU accounting and limits.
- Trace wakeup latency, donation chains, budget exhaustion, and deadline misses.

## Memory evolution

- Introduce anonymous, shared, executable-image, device, and copy-on-write memory
  objects.
- Add lazy zero-fill, mapping-protection changes, and W^X enforcement.
- Add page ownership and commitment accounting.
- Add slab caches and bounded pools for latency-sensitive work.
- Define pager-backed file mappings and unified page-cache behavior.
- Add memory-pressure notification and job-level out-of-memory containment.
- Add replaceable shared buffers for revocable media and capture sessions.
- Keep generic shared memory separate from pinned, device-visible DMA buffers.
- Defer compressed memory, swap, huge pages, NUMA, and hibernation until reclaim and
  failure semantics are reliable.

## Userspace and command-line evolution

- Build raw ABI, safe handle, runtime, asynchronous IPC, and service-client layers.
- Define application, service, driver, package, and protocol manifests.
- Adopt `/Users/<name>/Profile/{config,cache,state,data,logs,runtime}`.
- Add system-managed filesystem metadata so graphical tools may hide `Profile` without
  dot-prefix naming.
- Add threads and futex-like synchronization.
- Expand the native Rust utility set for boot, recovery, and ordinary shell use.
- Grow `ush` scripting while documenting native behavior separately from future POSIX
  `sh` compatibility.
- Map XDG base-directory compatibility to `Profile` without making Unix paths native.
- Add libc and POSIX compatibility after native contracts stabilize.
- Use external utility suites and eventually GNU coreutils as compatibility workloads,
  not boot dependencies.
- Keep bootstrap, recovery, and early services statically linked while loader and
  deployment ABIs evolve.
- Introduce shared libraries only through versioned, verified deployments and controlled
  loader policy.

## Executable and linking evolution

1. Keep the native kernel, services, drivers, recovery environment, executables, and
   machine-code libraries x86-64-only.
2. Harden static ELF64 program-header validation, segment bounds, zero-fill, entry-point
   checks, and final W^X permissions.
3. Add build IDs, separate debug-symbol packages, non-executable stacks, and versioned
   NullStar ELF notes verified against package metadata.
4. Add static PIE, a bounded relocation subset, and ASLR for executable, stack, heap,
   and mapping bases.
5. Define a stable platform-library ABI rather than exposing Rust's compiler-private ABI.
6. Add a versioned dynamic loader, immediate relocation, shared objects, TLS,
   constructors, RELRO, application-private libraries, and controlled search policy.
7. Keep bootstrap, recovery, and critical repair utilities statically linked.
8. Defer ELF32/i386 execution, 32-bit libraries, and legacy plugin hosts to an optional
   compatibility deployment after the 64-bit platform is mature.

## Magnetar package and deployment evolution

1. Define deterministic `.nspkg` archives, canonical package and application identities,
   version comparison, architecture fields, file classes, and signed manifests.
2. Define authenticated publisher-key lineage so application updates may preserve
   identity without silently accepting an unrelated signer.
3. Implement local install and verification, repository snapshots, a trusted key store,
   mirror ranking and history, configurable parallel downloads, and package queries.
4. Add dependency solving, minimum-version and ABI requirements, conflicts, providers,
   manual/automatic tracking, removal, and generation-aware pruning.
5. Import verified immutable content into a content-addressed store and record embedded
   static components for vulnerability auditing.
6. Construct complete application generations and atomically switch application bundles
   without changing grants solely because a path changed.
7. Construct complete system and boot generations, preserve the previous healthy
   generation, and allow selection from the independent bootstrap environment.
8. Add staged, pending, healthy, failed, superseded, and pinned states plus bounded
   failed-boot fallback.
9. Separate package defaults from mutable configuration and state; define schema-aware
   merge, migration, snapshot, and rollback-compatibility rules.
10. Coordinate services, drivers, namespace bindings, health checks, and reboots through
    declarative activation rather than unrestricted package scripts.
11. Add generation-aware garbage collection, offline transaction bundles, repair,
    provenance, repository-key rotation, and recovery tooling.

## Identity, login, and authorization

- Define bounded UID/GID and immutable process credential types without treating them as
  capabilities.
- Add native ownership and mode metadata to tmpfs, NullFS, VFS, and service requests.
- Add read-only account and group lookup and isolated credential-verifier storage.
- Add service identities and checked identity/capability filtering at launch.
- Implement a dedicated authentication service and trusted login UI.
- Create login-session jobs, private runtime namespaces, compositor lock integration,
  and deterministic logout cleanup.
- Add supplementary groups and shared-file workflows.
- Add narrow semantic authorization requests and single-use administrative tickets.
- Add stronger audit records, denial-path tests, rate limiting, recovery, and credential
  upgrade policy.

## Logging and diagnostics

- Add a bounded kernel early-boot log ring and boot IDs.
- Add structured userspace records with process, application, package, service,
  generation, user, session, and subsystem attribution.
- Capture managed-service stdout/stderr and implement `logctl show`, `follow`, and
  `sv logs`.
- Persist a size-bounded append-only journal.
- Add immutable segments, indexes, compression, age and size rotation, low-disk policy,
  and configurable retention classes.
- Add rate limits, drop summaries, field-level privacy, crash-report links, and a
  stronger audit stream.
- Add IPC, object, handle, capability-route, service-generation, and wait-chain
  diagnostic tools.
- Add syslog, legacy text-file rotation, and remote forwarding only as compatibility or
  optional services.

## Networking and policy

- Route sockets through a userspace network service and attach immutable caller
  application, service, user, and session identity.
- Record connection and listener address, port, protocol, state, interface, and byte
  counts.
- Add per-application connect and listen rules plus `netctl connections` and rule
  explanations.
- Add a native resolver with domain-to-connection attribution and domain policy.
- Distinguish outbound internet, local-network discovery, listener, raw, packet-capture,
  VPN, and DNS-provider capabilities.
- Add user-scoped history, graphical policy controls, application profiles, and explicit
  privacy retention.
- Add verified malware and tracker lists with isolated parsing, immutable compiled
  snapshots, exceptions, explanations, and rollback.
- Add VPN awareness, per-application routing, rate limits, and a bounded packet-layer
  enforcement path later.

## Graphics and desktop evolution

1. Define native surface, buffer, role, commit, damage, input, and presentation objects.
2. Build a software-composited single-output session over the existing framebuffer.
3. Add isolated toplevels, dialogs, popups, focus, clipboard offers, and accessibility
   foundations.
4. Add a nested panel compositor with process-isolated applets and session-manager
   supervision.
5. Support panel, dock, or neither on every screen edge, including work-area reservation,
   auto-hide, and popup forwarding.
6. Add file-transfer, clipboard, global-shortcut, screen-capture, sensitive-input, and
   permission portals.
7. Add compositor-owned backdrop blur and materials without exposing underlying pixels.
8. Add trusted microphone, camera, and capture indicators with immediate stop actions.
9. Add accelerated buffers, explicit synchronization, modesetting, color management,
   multiple outputs, and protected-content handling.
10. Add a Wayland compatibility frontend beginning with core protocol and `xdg-shell`.
11. Add freedesktop application metadata, MIME, icons, notifications, portals, settings,
    Secret Service, D-Bus session, and tray compatibility in measured stages.

## Native renderer and toolkit evolution

1. Implement a software vector/raster renderer for paths, images, clipping, alpha,
   rounded geometry, gradients, shadows, and basic text.
2. Define a safe SVG asset profile, symbolic icon roles, and scalable cursor metadata.
3. Implement a Rust widget toolkit with row, column, stack, scroll, common controls,
   input, focus, and accessibility identities.
4. Specify NullStar Style Sheets with variables, selectors, stable widget parts,
   pseudo-states, borders, radii, spacing, gradients, text, and shadows.
5. Add live themes, high contrast, reduced motion, animation, and compositor backdrop
   material requests.
6. Add damage tracking, retained scene fragments, glyph and image caches, and a GPU
   backend.
7. Add complex text, input methods, color management, richer SVG, and document or print
   backends later.

## Media graph evolution

1. Fixed-format playback through one output, software mixing, per-stream volume, and
   shared-memory transport.
2. Capture permissions, trusted active-use indicators, hotplug, multiple devices,
   sample-rate and sample-format conversion, and channel mapping.
3. Arbitrary routing, virtual devices, processing nodes, graph inspection, and saved
   routes.
4. Multiple clock domains, adaptive resampling, latency negotiation, MIDI, automation,
   and professional low-latency policy.
5. Video, cameras, screen capture, codecs, and audio/video synchronization.

The realtime media worker has bounded precedence over interactive compositor work only
when meeting a declared audio deadline. It never receives unlimited CPU authority.

## Distant LV2 compatibility

- Complete native processing, event, latency, state, and automation semantics first.
- Add dynamic loading and required C ABI support.
- Port LV2 discovery dependencies and build a scanner.
- Add sandboxed DSP hosting for audio and control ports.
- Add Atom, MIDI, Worker, State, presets, and delay compensation.
- Add generated NullStar plugin controls and per-chain isolation.
- Investigate foreign or external plugin UIs only after DSP compatibility is useful.

## Documentation rule

Current implementation documents describe what exists. Design documents describe
accepted direction, tentative design, open questions, and distant goals. Future work
must update both sides when a design becomes implemented behavior.
