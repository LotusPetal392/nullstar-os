# NullStar OS design direction

This directory records long-term architectural direction that is not yet fully
implemented. The current-system description remains in
[`../architecture.md`](../architecture.md).

Each design document distinguishes among:

- **Accepted direction**: a project-level design decision that should guide future work.
- **Tentative design**: the current preferred approach, subject to implementation experience.
- **Open question**: an area that still needs investigation or measurement.
- **Distant goal**: intentionally outside the early-system implementation sequence.

The documents here are not stable ABI specifications. Implemented behavior remains
authoritative when it differs from a future design.

## System foundations

- [Kernel, IPC, and scheduling](kernel-ipc-scheduling.md)
- [Memory management](memory-management.md)
- [Userspace architecture](userspace-architecture.md)
- [Filesystem namespace and boot](filesystem-namespace.md)
- [Driver model](driver-model.md)
- [Service management and command line](service-management-and-cli.md)
- [Logging, journal, and rotation](logging.md)
- [Network policy and firewall](network-policy.md)

## Desktop and media

- [Graphics stack and compositor](graphics-stack.md)
- [Native graphics renderer and UI toolkit](graphics-renderer-and-toolkit.md)
- [Freedesktop compatibility](freedesktop-compatibility.md)
- [Media graph](media-graph.md)
- [LV2 hosting](lv2-hosting.md)

## Consolidated planning

- [Architecture roadmap](roadmap.md)

## Guiding principles

- Keep mechanism in the kernel and policy in userspace where practical.
- Prefer explicit capabilities over ambient authority.
- Preserve a practical compatibility path without making Unix conventions the
  foundation of the native system.
- Translate privileged compatibility interfaces through native policy and portals.
- Optimize desktop behavior for predictable latency rather than unlimited priority.
- Keep the logical namespace independent from physical storage layout.
- Prevent one application, applet, plugin, driver, or service failure from exposing or
  destabilizing unrelated processes.
- Use descriptive filesystem and service names that do not require historical Unix
  knowledge.
- Keep current implementation documentation separate from future architectural intent.
