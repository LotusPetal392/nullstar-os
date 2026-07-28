# Device filesystem design

## Status

This document describes a future userspace device filesystem for NullStar OS.
The current VFS exposes `/dev` as a synthetic namespace; it does not yet mount a
dynamic devfs or create device-backed open-file descriptions.

## Goals

The device filesystem should:

- expose discoverable devices through ordinary paths under `/dev`;
- preserve the normal `open`, descriptor, `read`, `write`, `fstat`, and close
  application model;
- keep device providers and policy outside the kernel where practical;
- ensure that listing or looking up a device does not grant authority to use it;
- support provider restart, hotplug, stable aliases, and bounded resource use;
- use typed, versioned device protocols instead of an unrestricted control ABI;
- permit kernel-backed adapters now and userspace hardware drivers later.

It is not required to reproduce Linux `devtmpfs`, major/minor allocation, or
`ioctl` encoding exactly. Compatibility layers may expose those conventions
later without making them the native internal model.

## Architecture

`devfs-service` should own the `/dev` mount through the generic VFS route. It is
a registry and connection broker, not the implementation of every device.
Device providers publish bounded records to it through a privileged registration
protocol. PID 1 or a future driver manager grants registration authority only to
supervised providers.

```text
application open("/dev/console")
    -> VFS route for /dev
    -> devfs lookup and policy check
    -> provider creates an attenuated session
    -> kernel installs a device-backed open-file description
    -> descriptor operations use the typed provider protocol
```

The initial providers may be adapters around kernel facilities such as the
terminal and AHCI block layer. Moving those drivers to userspace later should
not change application paths or descriptor semantics.

## Namespace

The initial namespace should reserve:

```text
/dev/null
/dev/zero
/dev/full
/dev/console
/dev/tty
/dev/random
/dev/urandom
/dev/input/
/dev/disk/
/dev/disk/by-label/
/dev/disk/by-uuid/
```

`random` and `urandom` must not appear until their entropy and readiness
semantics are defined. Stable aliases may initially be registry aliases rather
than filesystem symbolic links. Device entries are generated at boot and are
not persistent filesystem records.

## Device records

A published device record should contain:

- a validated path component and device class;
- provider identity and monotonically increasing provider generation;
- stable device identity where the hardware or backing service supplies one;
- owner UID, owner GID, and permission mode;
- supported protocol identifier and version range;
- capability and operation classes the provider is willing to delegate;
- optional label, UUID, and human-readable description;
- bounded flags describing seekability, event readiness, and hotplug behavior.

The ABI should add explicit character-device, block-device, terminal, and other
necessary `stat` kinds. Major and minor numbers are optional compatibility
metadata rather than authority-bearing identifiers.

## Open and authority flow

Path visibility is not authority. A successful lookup or directory listing
returns metadata only. Opening a device should perform these steps:

1. Canonicalize and route the path to devfs.
2. Evaluate identity policy using the caller's immutable process credentials,
   node owner/group, mode bits, and any class-specific policy.
3. Ask the current provider generation to create a new session.
4. Attenuate the session to the operations approved for this caller.
5. Install a generation- and session-scoped device open-file description.
6. Release exactly one provider reference when the final shared description is
   destroyed.

A device path must never expose a globally reusable provider endpoint. Raw block
write authority, MMIO, IRQ, DMA, and administrative control require explicit
capabilities even for UID 0. Root and admin identities influence policy but do
not manufacture missing capabilities.

## Descriptor protocols

The common descriptor layer should support operations only where meaningful:

- character devices: `read`, `write`, readiness, and typed control requests;
- block devices: geometry, aligned reads/writes, flush, discard, and bounds;
- terminals: stream I/O, process-group control, size, and terminal settings;
- input devices: event reads, metadata, and readiness notifications;
- display or audio devices: control IPC plus shared-memory queues.

Control operations should use device-class-specific request structures with
version, size, flags, and reserved-field validation. NullStar should not begin
with an untyped variadic `ioctl` that allows providers to interpret arbitrary
application memory.

High-throughput devices should use IPC for session setup and control, bounded
shared-memory rings for bulk transfer, and notification capabilities for
readiness. Every index, length, ownership transition, and generation must be
validated on both sides.

## Publication and hotplug

Provider registration should require a capability granted by PID 1 or the future
driver manager. The registry must reject duplicate canonical paths, invalid
classes, unsupported versions, and records exceeding namespace bounds.

Provider removal should atomically hide its entries from new lookup. Existing
descriptors remain tied to their recorded generation and either finish under an
explicit provider contract or become stale and return `IO`. A replacement may
publish the same stable path with a higher generation, but old descriptors must
not silently rebind unless that device class explicitly defines reconnectable
semantics.

## Identity policy

The future identity model is specified in
[Identity and access-control design](identity-and-access.md). Initial defaults
should be conservative:

- pseudo-devices such as `null` and `zero` may be world-readable/writable;
- the active terminal is restricted by session and foreground-group policy;
- input devices are not world-readable;
- raw disks are root-owned and unavailable to regular users;
- admin-group membership may authorize a brokered elevation request but does not
  directly grant raw device capabilities.

## Crash and replacement behavior

Device sessions should reuse the lifecycle rules proven by the filesystem
services:

- requests and descriptors carry provider generation and session identity;
- replacement cancels in-flight old-generation operations with `IO`;
- old descriptors remain stale;
- close records from old generations are never sent to replacements;
- malformed replies fail the affected proxy or provider session closed;
- resource queues and shared buffers remain bounded.

Reconnectable devices such as a system console may add an explicit brokered
reconnection policy later. It must not be the default for raw or stateful
devices.

## Userspace-driver prerequisites

A useful devfs can precede userspace hardware drivers by exposing kernel-backed
adapters. Moving hardware drivers out of the kernel additionally requires:

- checked PCI-function ownership and reset;
- constrained MMIO and I/O-port capabilities;
- IRQ notification capabilities;
- pinned DMA buffers and ownership transitions;
- IOMMU isolation or an explicitly documented trusted-driver phase;
- cache, ordering, and device-coherency rules.

Until those primitives exist, AHCI and other hardware remain kernel-backed while
devfs and applications interact with supervised adapter services.

## Recommended milestones

1. Mount a static userspace devfs and implement `null`, `zero`, and `full`.
2. Add device node kinds and generation-scoped device descriptor backends.
3. Adapt `/dev/console` and `/dev/tty` to the existing terminal subsystem.
4. Add dynamic provider registration, removal, and stable aliases.
5. Publish read-only partition devices under `/dev/disk` with explicit policy.
6. Add readiness waiting and typed device-control protocols.
7. Introduce IRQ, MMIO, DMA, and PCI capabilities for selected userspace drivers.
