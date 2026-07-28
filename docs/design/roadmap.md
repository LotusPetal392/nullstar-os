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

## Userspace evolution

- Build raw ABI, safe handle, runtime, and service-client layers.
- Define application and service manifests.
- Adopt `/Users/<name>/Profile/{config,cache,state,data,logs,runtime}`.
- Add system-managed filesystem metadata so graphical tools may hide `Profile` without
  dot-prefix naming.
- Add application jobs, sandbox policy, and portal-style brokers.
- Add structured logging, crash reporting, and service health.
- Add threads and futex-like synchronization.
- Add libc and POSIX compatibility after native contracts stabilize.
- Add transactional packages and dynamic linking later.

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
