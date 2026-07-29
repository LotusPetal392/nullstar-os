# Architecture design roadmap

This roadmap separates implemented work from future architectural direction. It does
not replace milestone-specific plans elsewhere in the repository.

## Near-term foundations

- Formalize kernel object ownership and typed handle usage.
- Separate process, thread, address-space, and future job abstractions.
- Evolve shared-memory objects from bounded copies to mapped pages.
- Add authoritative virtual-memory region tracking and guarded stacks.
- Define channel transfer, cancellation, and multi-object waiting semantics.
- Add scheduler tracing for wakeup latency and priority inversion.
- Stabilize the userspace startup block and safe platform wrappers.
- Introduce a named, versioned service-broker contract.

## Namespace and persistent storage

- Preserve the VFS as owner of a synthetic logical root.
- Give the primary NullFS volume a stable UUID and human-facing `/Volumes/NullStar`
  identity.
- Populate `System`, `Applications`, and `Users` trees on the primary volume.
- Define namespace bindings that project those trees into canonical `/System`,
  `/Applications`, and `/Users` paths without symbolic links.
- Preserve canonical file identity across logical bindings and administrative backing
  views.
- Add writable NullStar filesystem-service authority, shutdown ordering, recovery, and
  administrative tooling.
- Load non-bootstrap programs and service definitions through the bound `/System` tree.
- Treat `/System/boot` as the canonical source of boot generations and initially mirror
  the selected generation to a firmware-readable bootstrap partition.
- Retain an independent bootstrap and recovery environment whenever persistent bindings
  are unavailable.

## Driver and service evolution

- Define common device identity, ownership generation, reset, and discovery records.
- Add verified driver manifests, deterministic matching, and a supervised device
  manager.
- Add dynamic devfs provider registration and generation-scoped sessions.
- Introduce constrained PCI configuration, MMIO, IRQ, pinned DMA, and later IOMMU-domain
  capabilities.
- Move a queue-oriented virtual block or network driver to userspace first.
- Separate controller drivers from block, network, input, audio, and display class
  policy.
- Add hotplug, firmware brokerage, suspend/resume, crash recovery, quarantine, and
  driver rollback.
- Define the service-manager control protocol and state machine.
- Implement `sv list`, `status`, `start`, `stop`, `restart`, and `logs` against the
  evolving supervisor.
- Add versioned service definitions, readiness, dependency validation, restart budgets,
  resource limits, capability grants, enablement, and local overrides.

## Desktop kernel evolution

- Add multilevel feedback scheduling with interactive wakeup preemption.
- Add priority inheritance for locks and bounded synchronous IPC.
- Introduce restricted, budgeted realtime scheduling.
- Move scheduler state and preemption accounting to per-CPU structures.
- Add SMP, per-CPU run queues, affinity, and bounded load balancing.
- Evolve timers toward deadline-driven tickless operation.
- Add job-level resource accounting and limits.

## Memory evolution

- Introduce anonymous, shared, executable-image, device, and COW memory objects.
- Add lazy zero-fill, mapping protection changes, and W^X enforcement.
- Add page ownership and commitment accounting.
- Add slab caches and bounded pools for latency-sensitive work.
- Define pager-backed file mappings and unified page-cache behavior.
- Add memory-pressure notification and job-level OOM containment.
- Defer compressed memory, swap, huge pages, NUMA, and hibernation until reclaim and
  failure semantics are reliable.

## Userspace and command-line evolution

- Build raw ABI, safe handle, runtime, and service-client layers.
- Define application, service, driver, and package manifests.
- Adopt `/Users/<name>/Profile/{config,cache,state,data,logs,runtime}`.
- Add system-managed filesystem metadata so graphical tools may hide `Profile` without
  dot-prefix naming.
- Add application jobs, sandbox policy, and portal-style brokers.
- Add threads and futex-like synchronization.
- Expand the native Rust utility set for boot, recovery, and ordinary shell use.
- Grow `ush` scripting while documenting its native behavior separately from future
  POSIX `sh` compatibility.
- Map XDG base-directory compatibility to `Profile` without making Unix paths native.
- Add libc and POSIX compatibility after native contracts stabilize.
- Use external utility suites and eventually GNU coreutils as compatibility workloads,
  not boot dependencies.
- Add transactional packages and dynamic linking later.

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
- Add syslog, legacy text-file rotation, and remote forwarding only as compatibility
  or optional services.

## Networking and policy

- Route sockets through a userspace network service and attach immutable caller
  identity.
- Record connection and listener address, port, protocol, state, interface, and byte
  counts.
- Add per-application connect and listen rules plus `netctl connections` and rule
  explanations.
- Add a native resolver with domain-to-connection attribution and domain policy.
- Distinguish internet, local-network, discovery, listener, raw, VPN, and DNS-provider
  capabilities.
- Add user-scoped history, graphical policy controls, application profiles, and
  explicit privacy retention.
- Add verified malware and tracker lists with isolated parsing, immutable compiled
  snapshots, exceptions, explanations, and rollback.
- Add VPN awareness, per-application routing, rate limits, and a bounded packet-layer
  enforcement path later.

## Graphics and desktop evolution

1. Define native surface, buffer, role, commit, damage, input, and presentation objects.
2. Build a software-composited single-output session over the existing framebuffer.
3. Add isolated toplevels, dialogs, popups, focus, clipboard offers, and accessibility
   foundations.
4. Add a nested panel compositor with process-isolated applets and service-manager
   supervision.
5. Support panel, dock, or neither on every screen edge, including work-area reservation,
   auto-hide, and popup forwarding.
6. Add capture, file-transfer, global-shortcut, sensitive-input, and permission portals.
7. Add compositor-owned backdrop blur and materials without exposing underlying pixels.
8. Add accelerated buffers, explicit synchronization, modesetting, color management,
   multiple outputs, and protected-content handling.
9. Add a Wayland compatibility frontend beginning with core protocol and `xdg-shell`.
10. Add freedesktop application metadata, MIME, icon, notification, portal, settings,
    Secret Service, D-Bus session, and tray compatibility in measured stages.

## Native renderer and toolkit evolution

1. Implement a software vector/raster renderer for paths, images, clipping, alpha,
   rounded geometry, gradients, shadows, and basic text.
2. Define a safe SVG asset profile, symbolic icon roles, and scalable cursor metadata.
3. Implement a Rust widget toolkit with row, column, stack, scroll, common controls,
   input, focus, and accessibility identities.
4. Specify NullStar Style Sheets with variables, type/class/ID selectors, stable widget
   parts, pseudo-states, borders, radii, spacing, gradients, text, and shadows.
5. Add live themes, high contrast, reduced motion, animation, and compositor backdrop
   material requests.
6. Add damage tracking, retained scene fragments, glyph and image caches, and a GPU
   backend.
7. Add complex text, input methods, color management, richer SVG, and document or print
   backends later.

## Media graph evolution

1. Fixed-format playback through one output, software mixing, per-stream volume, and
   shared-memory transport.
2. Capture permissions, hotplug, multiple devices, sample-rate and sample-format
   conversion, and channel mapping.
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
