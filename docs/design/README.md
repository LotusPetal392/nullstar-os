# NullStar OS design direction

This directory records long-term architectural direction that is not yet fully
implemented. The current-system description remains in
[`../architecture.md`](../architecture.md).

Each design document distinguishes among:

- **Accepted direction**: a project-level design decision that should guide future work.
- **Tentative design**: the current preferred approach, subject to implementation
  experience.
- **Open question**: an area that still needs investigation or measurement.
- **Distant goal**: intentionally outside the early-system implementation sequence.

The documents here are not stable ABI specifications. Implemented behavior remains
authoritative when it differs from future design.

## System foundations

- [Kernel, IPC, and scheduling](kernel-ipc-scheduling.md)
- [IPC, kernel object, and handle model](ipc-and-object-model.md)
- [Memory management](memory-management.md)
- [Memory hardening](memory-hardening.md)
- [Userspace architecture](userspace-architecture.md)
- [Native application runtime, SDK, and service IDL](application-runtime-sdk-and-idl.md)
- [Nova Foundation application context and profile storage](nova-foundation-profile-storage.md)
- [NSIDL and the NullStar Wire Protocol](nsidl-and-wire-protocol.md)
- [NSWP packet header and protocol identifiers](nswp-header-and-protocol-identifiers.md)
- [Service, session, and application lifecycle](service-and-session-lifecycle.md)
- [Service management and command line](service-management-and-cli.md)
- [Capability-based application sandboxing](application-sandboxing.md)
- [Application bundles, signing, and deployment](application-bundles-and-deployment.md)
- [Executable loading and linking](executable-loading.md)
- [Magnetar package and deployment management](package-management.md)
- [Filesystem namespace and boot](filesystem-namespace.md)
- [Driver model](driver-model.md)
- [Logging, journal, and rotation](logging.md)
- [Network management, diagnostics, and local sockets](network-management-and-local-sockets.md)
- [Network policy and firewall](network-policy.md)

## Desktop and media

- [Graphics stack and compositor](graphics-stack.md)
- [Native graphics renderer and UI toolkit](graphics-renderer-and-toolkit.md)
- [Appearance, theming, and display adaptation](appearance-and-theming.md)
- [Freedesktop compatibility](freedesktop-compatibility.md)
- [Media graph](media-graph.md)
- [Control surfaces and application controllers](control-surfaces-and-controllers.md)
- [LV2 hosting](lv2-hosting.md)

## Consolidated planning

- [Architecture roadmap](roadmap.md)

## Guiding principles

- Keep mechanism in the kernel and policy in userspace where practical.
- Prefer explicit capabilities over ambient authority.
- Keep PID 1 small and move ordinary service policy into restartable userspace.
- Treat stable service identity separately from disposable process incarnations.
- Construct every managed process with explicit startup handles before it runs.
- Use versioned, language-neutral service protocols as the durable userspace
  compatibility boundary rather than Rust's compiler-private ABI.
- Bind one negotiated protocol to each service channel and use explicit UUIDv4 protocol
  family identities without treating identifiers as authority.
- Keep every wire message, dynamic value, handle set, and decode allocation explicitly
  bounded and validate complete messages before dispatch.
- Sandbox every application bundle regardless of installation location.
- Bind application profile storage to verified identity and expose role-specific
  capabilities rather than ambient paths.
- Deploy applications as verified immutable generations and keep mutable data outside
  their bundles.
- Preserve a practical compatibility path without making Unix conventions the
  foundation of the native system.
- Translate privileged compatibility interfaces through native policy and portals.
- Optimize desktop behavior for predictable latency rather than unlimited priority.
- Keep the logical namespace independent from physical storage layout.
- Prevent one application, applet, plugin, driver, session, or service failure from
  exposing or destabilizing unrelated jobs.
- Coordinate independently selectable appearance components through semantic tokens
  rather than coupling them into one monolithic theme.
- Let widgets own structure and semantics while declarative themes style stable
  surfaces, keep display color adaptation separate, and make accessibility overrides
  authoritative.
- Use descriptive filesystem and service names that do not require historical Unix
  knowledge.
- Keep current implementation documentation separate from future architectural intent.
