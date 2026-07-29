# Driver-model direction

## Status

Userspace-first, capability-controlled, supervised drivers are **accepted direction**.
The kernel mechanisms required to make that safe are also accepted direction. The
first hardware driver moved from the kernel, process-granularity defaults, and exact
queue ABIs remain **tentative design**.

The device namespace and client-brokering rules are defined separately in
[the device-filesystem design](../devfs.md).

## Architectural split

The kernel should retain mechanisms that cannot safely be delegated as ordinary
policy:

- interrupt routing, masking, acknowledgement, and waitable IRQ objects;
- ownership and reset of discovered hardware functions;
- checked PCI configuration access;
- constrained MMIO and I/O-port mappings with explicit cache attributes;
- pinned DMA memory and IOMMU-domain management;
- shared memory, notifications, clocks, and capability enforcement;
- the minimal bootstrap drivers required before userspace is available.

Userspace should own most device-specific policy and protocol implementations:

- storage-controller, network, USB, audio, input, and other hardware drivers;
- device matching and driver launch;
- class services such as block, network, media, input, and display providers;
- hotplug policy, firmware selection, power policy, supervision, and publication.

Moving drivers to userspace is incremental. Existing kernel drivers remain valid
bootstrap providers until the required hardware capabilities and class protocols are
proven.

## Device manager

A supervised device manager should receive bounded discovery records from the kernel,
match them against verified driver manifests, launch driver jobs, and delegate only
the capabilities needed for each claimed device.

```text
kernel discovers hardware
        |
        v
device manager matches a manifest
        |
        v
service manager launches a driver job
        |
        v
MMIO, IRQ, DMA, reset, and configuration capabilities are delegated
        |
        v
driver publishes a class-specific provider through devfs or another broker
```

The device manager coordinates lifecycle and ownership. It should not become a
monolithic process that handles normal device I/O for every class.

## Driver manifests

A packaged driver manifest should describe:

- stable driver and package identity;
- executable and supported architecture;
- bus and match records, including vendor, device, class, and compatible identifiers;
- required MMIO, I/O-port, IRQ, DMA, firmware, and reset facilities;
- class protocols and versions provided;
- supported process-isolation modes;
- restart, failure, and power-management policy;
- whether the driver is eligible for the bootstrap image.

A manifest is a request, not authority. The kernel validates actual hardware ranges,
and the device manager applies system policy before delegating capabilities.

Driver matching should be deterministic. Conflicts, ambiguous equal-priority matches,
and unsupported required features must produce actionable diagnostics instead of
silently choosing an arbitrary provider.

## Hardware capabilities

### Device ownership

A physical function has one active owner generation unless its bus and class protocol
explicitly support safe sharing. Ownership includes the set of MMIO, IRQ, DMA, reset,
and configuration capabilities created for that generation.

When an owner terminates, the kernel should revoke or invalidate its capabilities,
mask interrupts, detach DMA mappings, and place the device into a reset or quarantined
state before a new owner is admitted.

### MMIO and I/O ports

Drivers receive mapping objects constrained to validated device resources. They do not
receive generic physical-memory mapping authority. Mapping objects record allowed
ranges, access rights, cache policy, ordering rules, and owner generation.

### Interrupts

Interrupts should be represented as waitable objects integrated with the normal event
system. A driver waits for an IRQ notification, drains bounded device work, then
completes or rearms the interrupt according to the object contract.

Hard-interrupt kernel code remains minimal. Device-specific parsing and completion
processing runs in scheduled context.

### DMA

Drivers request DMA buffers or registered memory through explicit objects. The kernel
owns pinning and device-address mappings. A driver receives a CPU mapping, a
handle-scoped device-visible address, and defined ownership transitions.

IOMMU isolation is the intended security boundary. Before IOMMU support, userspace
placement still improves crash containment but must be documented as a trusted-driver
phase because a bus-mastering device may reach unrelated physical memory.

## Driver-to-client protocols

Applications should not communicate with hardware-specific implementations. Drivers
publish stable class protocols consumed by higher services:

```text
AHCI or NVMe driver -> block-device protocol -> partition service -> filesystem
NIC driver          -> network-device protocol -> network stack -> socket service
Audio driver        -> audio-device protocol -> media graph
Input driver        -> input-device protocol -> input service -> compositor
GPU/display driver  -> display-device protocol -> compositor and graphics service
```

Class protocols must include:

- protocol and feature negotiation;
- provider generation and stable device identity;
- bounded request and completion queues;
- cancellation, timeout, removal, and reset behavior;
- explicit buffer ownership transitions;
- precise completion status for operations that cannot be safely replayed.

Control and discovery use ordinary IPC. High-throughput data uses registered shared
memory, submission rings, completion rings, and bounded notifications.

## Devfs relationship

`devfs-service` is a registry and connection broker, not the driver manager and not the
implementation of each device. It exposes metadata through paths and creates an
attenuated, provider-generation-scoped client session after policy approval.

Listing a device does not grant access. Opening `/dev/disk/...`, `/dev/input/...`, or
another entry must never expose the driver's MMIO, IRQ, DMA, or global provider
endpoint.

The same class protocol should work behind a kernel-backed bootstrap adapter or a
userspace hardware driver so application paths do not change during migration.

## Process isolation

The service manager and device manager should support:

- **per-device processes** for strong isolation and simple ownership;
- **per-driver-family processes** when sharing state materially reduces overhead;
- **in-kernel bootstrap providers** where early boot or incomplete mechanisms require
  them.

Per-device isolation is the preferred safety baseline for complex third-party drivers.
Policy may group trusted devices after measuring memory, context-switch, and queue
costs.

Driver jobs may contain separate control, interrupt, and bounded realtime workers.
Only a worker with a demonstrated latency requirement should receive restricted
realtime scheduling, and that worker remains subject to a CPU budget.

## Failure and replacement

Driver failure follows the generation model used by other NullStar services:

1. hide the failed provider from new connections;
2. cancel or fail unresolved old-generation operations with a precise status;
3. revoke hardware capabilities and DMA mappings;
4. mask interrupts and reset or quarantine the device;
5. notify dependent class services;
6. launch a replacement under a new generation;
7. require clients to reconnect unless the class protocol explicitly defines safe
   reconnection.

Block writes must not be replayed when completion is unknown. Network, audio, input,
and display classes may define different continuity behavior, but stale sessions must
never silently bind to a replacement.

Repeated crashes should trigger backoff, quarantine, rollback to a known driver
version, or a recovery provider. All transitions should be visible through structured
logs and service status.

## Firmware and power

Drivers request firmware by stable identifier from a firmware service. The service
verifies package ownership and returns an immutable buffer. Drivers do not gain broad
filesystem access merely to load firmware.

Power transitions should be coordinated in dependency order:

```text
applications and class services quiesce
        -> drivers quiesce devices
        -> platform enters the power state
```

Driver protocols need bounded prepare, suspend, resume, removal, and failure events.
A nonresponsive driver must time out into an explicit recovery policy rather than
blocking suspend forever.

## Graphics exception

GPU drivers may require additional kernel enforcement for memory ownership, command
validation, scheduling, and reset. NullStar should still keep vendor and policy code in
userspace where practical, but it must not treat arbitrary GPU command streams as
ordinary trusted shared-memory messages.

The first graphical path may remain a boot framebuffer with software rendering.
Modesetting, accelerated buffers, explicit synchronization, and GPU command submission
are later class-protocol milestones.

## Recommended implementation stages

1. Formalize the current internal device object, stable identity, ownership, and reset
   state.
2. Publish discovery records to a userspace device manager and define verified driver
   manifests.
3. Add dynamic devfs provider registration and generation-scoped client sessions.
4. Implement constrained PCI configuration, MMIO, IRQ, and DMA objects.
5. Choose a queue-oriented virtual device as the first userspace hardware driver,
   preferably a simple virtio block or network device.
6. Split partitioning and other class policy from controller drivers.
7. Add hotplug, suspend and resume, firmware brokerage, crash recovery, and driver
   rollback.
8. Add IOMMU domains and tighten untrusted-driver policy.
9. Tackle audio and display devices after timing and buffer-ownership contracts are
   measurable.

## Open questions

- The first driver selected for userspace migration.
- Whether driver matching belongs entirely in the device manager or partly in the
  package service.
- Queue-memory layout and notification batching for each class protocol.
- How much command validation and scheduling the kernel must retain for GPUs.
- Driver-signing and third-party trust policy.
- Whether live handoff from a bootstrap kernel driver is worth supporting initially.
