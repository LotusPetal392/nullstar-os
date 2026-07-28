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

## Documents

- [Kernel, IPC, and scheduling](kernel-ipc-scheduling.md)
- [Memory management](memory-management.md)
- [Userspace architecture](userspace-architecture.md)
- [Media graph](media-graph.md)
- [LV2 hosting](lv2-hosting.md)
- [Architecture roadmap](roadmap.md)

## Guiding principles

- Keep mechanism in the kernel and policy in userspace where practical.
- Prefer explicit capabilities over ambient authority.
- Preserve a practical compatibility path without making Unix conventions the
  foundation of the native system.
- Optimize desktop behavior for predictable latency rather than unlimited priority.
- Use descriptive filesystem and service names that do not require historical Unix
  knowledge.
- Keep current implementation documentation separate from future architectural intent.
