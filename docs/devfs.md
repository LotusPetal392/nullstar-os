# Device filesystem and device-management design

## Status

This document describes the accepted long-term direction for NullStar OS device
discovery, naming, publication, access, control, and removal.

The current VFS exposes `/dev` as a synthetic namespace; it does not yet mount a
dynamic devfs or create general device-backed open-file descriptions. The design
below is therefore normative planning rather than a claim about current behavior.

Printing and scanning classes are reserved by this design, but native print and
scan services are intentionally deferred until the graphical desktop,
application model, and service IPC architecture are substantially more mature.
CUPS and SANE compatibility are possible later additions and are not foundational
dependencies.

## Goals

The device system should:

- expose discoverable devices through ordinary, human-readable paths under
  `/dev`;
- preserve the normal `open`, descriptor, `read`, `write`, `fstat`, readiness,
  and close application model where those operations are meaningful;
- represent each discovered physical device independently of whether a suitable
  driver is currently available;
- separate immutable device identity from enumeration-dependent path names;
- support one physical device publishing multiple logical interfaces;
- support capability-based discovery without requiring applications to parse
  path names;
- keep device providers and policy outside the kernel where practical;
- ensure that listing or looking up a device does not grant authority to use it;
- support provider restart, hotplug, stable aliases, and bounded resource use;
- use typed, versioned device protocols instead of an unrestricted control ABI;
- permit kernel-backed adapters now and userspace hardware drivers later;
- define safe, transactional removal semantics for removable storage; and
- remain extensible without allowing drivers to create an inconsistent top-level
  namespace.

It is not required to reproduce Linux `devtmpfs`, Linux major/minor allocation,
BSD device names, or traditional `ioctl` encoding exactly. Compatibility layers
may expose those conventions later without making them the native internal
model.

## Architectural components

The long-term device architecture contains three cooperating roles:

1. **Driver manager**: discovers hardware, maintains physical device objects,
   matches drivers, supervises providers, and controls driver binding.
2. **Device providers**: implement one or more typed hardware or virtual-device
   protocols and publish logical interfaces.
3. **`devfs-service`**: owns the `/dev` mount, validates published records,
   evaluates pathname access policy, and brokers sessions to providers.

`devfs-service` is a registry and connection broker, not the implementation of
every device. Device providers publish bounded records through a privileged
registration protocol. PID 1 or the future driver manager grants registration
authority only to supervised providers.

```text
application open("/dev/terminal/console")
    -> VFS route for /dev
    -> devfs lookup and policy check
    -> provider creates an attenuated session
    -> kernel installs a device-backed open-file description
    -> descriptor operations use the typed provider protocol
```

The initial providers may be adapters around kernel facilities such as the
terminal and AHCI block layer. Moving those drivers to userspace later should
not change application paths, device identity, or descriptor semantics.

Higher-level operating-system services should normally consume device-provider
interfaces rather than requiring applications to access raw hardware directly.
For example, a future audio graph service consumes audio hardware endpoints, a
compositor consumes display endpoints, and a future print service consumes local
printer endpoints or network protocols.

## Physical device objects

Every device discovered by a bus enumerator receives an immutable system device
identifier before driver matching occurs. Driver availability affects usability,
not existence.

A physical device record should contain bounded, validated fields such as:

```yaml
object_id: device-184
parent_id: pci-0000:04:00.0
transport: pci
vendor_id: 0x144d
device_id: 0xa80a
class_code: 0x010802
stable_id: nvme-Samsung_SSD_990_PRO_S6Z...
status: active
driver: nvme
provider_generation: 3
```

The physical object may own zero, one, or many logical interfaces. Applications
must not treat a pathname as the canonical identity of the underlying hardware.
The device identifier and stable identity metadata are the canonical references.

## Device lifecycle

A discovered device progresses through an explicit lifecycle:

```text
hardware detected
        |
        v
bus enumeration
        |
        v
physical device object created
        |
        v
class and identity information recorded
        |
        v
driver matching
     /     \
    v       v
matched    unmatched
    |         |
    v         v
bind and    publish discovery record
start       under /dev/unresolved
provider
    |
    v
publish logical interfaces
```

A provider failure, manual unbind, or hot-unplug changes the device state and
publication generation. It does not cause a different physical device silently
to inherit existing sessions.

Recommended device states include:

- `discovered`: enumerated but not yet matched;
- `unbound`: identified but no driver is bound;
- `binding`: a driver is starting;
- `active`: one or more usable interfaces are published;
- `degraded`: some interfaces or capabilities failed;
- `quiescing`: new sessions are blocked while removal or reset proceeds;
- `removed`: the device is no longer present; and
- `failed`: initialization failed and diagnostic metadata is retained.

## Native namespace

NullStar uses broad functional classes and readable instance names rather than
Linux-style `sdX` names or BSD-style driver abbreviations as its native naming
policy.

The top-level namespace should remain deliberately small:

```text
/dev/
├── accelerator/
├── audio/
├── block/
├── bus/
├── camera/
├── crypto/
├── display/
├── input/
├── network/
├── printer/
├── radio/
├── scanner/
├── sensor/
├── serial/
├── terminal/
├── misc/
├── service/
├── by-id/
├── by-label/
├── by-path/
├── by-uuid/
├── unresolved/
├── extension/
├── null
├── zero
├── full
├── random
└── urandom
```

`random` and `urandom` must not appear until their entropy and readiness
semantics are defined. Device entries are generated at boot or hotplug time and
are not persistent filesystem records.

Initial compatibility aliases may preserve paths such as `/dev/console`,
`/dev/tty`, or `/dev/disk/...`, but new native interfaces should use the
canonical class namespace. For example:

```text
/dev/console -> /dev/terminal/console
/dev/disk/by-uuid/... -> /dev/by-uuid/...
```

Stable aliases may initially be registry aliases rather than filesystem symbolic
links.

## Naming rules

A published logical interface consists of:

```text
/dev/<class>/<functional-name><instance>
```

Examples:

```text
/dev/block/nvme0
/dev/block/nvme0p1
/dev/block/sata0
/dev/block/usb-storage0
/dev/block/virtio-block0
/dev/input/keyboard0
/dev/input/mouse0
/dev/input/touchpad0
/dev/display/gpu0
/dev/audio/card0
/dev/network/ethernet0
/dev/network/wifi0
/dev/radio/sdr0
/dev/sensor/temperature0
/dev/serial/uart0
```

Names describe function rather than incidental protocol history. The device
manager owns instance allocation. Drivers may suggest a validated functional
stem but must not choose arbitrary absolute paths.

Enumeration names such as `nvme0` are convenient aliases and are not guaranteed
to be stable across hardware topology changes. Persistent configuration should
use stable identities or aliases under `/dev/by-*`.

## Stable aliases

A logical interface may have multiple aliases that refer to the same device
object and interface identifier:

```text
/dev/by-id/nvme-Samsung_SSD_990_PRO_S6Z...
/dev/by-path/pci-0000:04:00.0-nvme
/dev/by-uuid/3fb0e0d2-...
/dev/by-label/System
```

The registry must reject duplicate aliases unless the relevant alias type
explicitly permits multiple targets. Stable aliases must not be silently reused
for a different physical device while stale sessions remain.

## Device classes, subclasses, and capabilities

Top-level classes are a controlled system registry. Subclasses and capabilities
provide specificity without requiring a new top-level directory for every niche
device.

Examples include:

```text
radio/software-defined-radio
radio/broadcast-tuner
radio/transceiver
sensor/temperature
sensor/spectrometer
sensor/radiation
accelerator/gpu
accelerator/npu
printer/document
printer/label
scanner/flatbed
scanner/sheet-fed
input/barcode-reader
```

Applications should discover devices primarily through metadata and capability
queries, for example:

```text
class == radio
capability == iq-input
sample-format includes u8-iq
```

or:

```text
class == block
capability == removable
```

The class and pathname support human navigation. The capability set is the
machine-facing contract.

## Driver-requested classes

A provider registration request may include:

```rust
DeviceInterfaceRegistration {
    class: "radio",
    subclass: "software-defined-radio",
    preferred_name: "sdr",
    protocol: RADIO_IQ_V1,
    capabilities: &[IQ_INPUT, FREQUENCY_CONTROL, GAIN_CONTROL],
}
```

The device manager and devfs registry validate the class, subclass, name stem,
protocol, capabilities, provider authority, and parent device.

Drivers may not freely create arbitrary top-level directories. Registration is
handled as follows:

1. A recognized class is published in its canonical namespace.
2. A new subclass under a recognized class may be accepted when its name and
   protocol metadata are valid.
3. A genuinely new, unstandardized class is published beneath
   `/dev/extension/<vendor-or-domain>/<class>/`.
4. Promotion from an extension class to a standard class requires an accepted
   NullStar design change and stable compatibility aliases where appropriate.

Example:

```text
/dev/extension/example.org/quantum/entropy-source0
```

This keeps experimentation possible without allowing competing names such as
`/dev/sdr`, `/dev/rf`, and `/dev/tuner` to fragment the native namespace.

## Multiple interfaces from one device

One physical device may publish multiple logical interfaces, including
interfaces in different classes.

A multifunction printer may publish:

```text
/dev/printer/printer0
/dev/scanner/scanner0
/dev/block/usb-storage0
```

All three records refer to the same parent physical device identifier while
using separate provider protocols and access policy.

An audio interface may publish:

```text
/dev/audio/card0
/dev/audio/card0-playback
/dev/audio/card0-capture
/dev/audio/card0-midi
```

A GPU may publish:

```text
/dev/display/gpu0
/dev/display/gpu0-control
/dev/display/gpu0-render
/dev/display/gpu0-framebuffer
```

Whether related operations appear as separate paths or as typed sessions on one
path is protocol-specific. Separate paths are appropriate when endpoints need
different permissions, ownership, scheduling, or bulk-transfer semantics.

## Software-defined radio example

An RTL-SDR-class USB receiver should appear as a radio device, not an audio
device:

```text
/dev/radio/sdr0
/dev/radio/sdr0-control
/dev/radio/sdr0-stream
```

Example metadata:

```yaml
class: radio
subclass: software-defined-radio
transport: usb
driver: rtl-sdr
sample_formats:
  - u8-iq
capabilities:
  - iq-input
  - frequency-control
  - sample-rate-control
  - gain-control
```

Demodulated PCM is produced by a radio application or service and may then be
published into the future audio graph. The hardware remains classified as radio
because raw I/Q sampling is its native function.

## Printers, scanners, and barcode readers

The following classes are reserved even though their higher-level services are
future work:

```text
/dev/printer/
/dev/scanner/
/dev/input/barcode0
```

Printer and scanner devfs entries represent local hardware/provider endpoints.
They do not themselves define job queues, document conversion, network sharing,
scan workflows, or graphical dialogs. Those responsibilities belong to future
native print and scan services.

A typical USB barcode reader that presents keyboard-like input should publish a
canonical interpreted endpoint:

```text
/dev/input/barcode0
```

It may additionally publish or be represented through a keyboard-compatible
interface when supported:

```text
/dev/input/keyboard2
```

A serial barcode reader may expose both transport and interpreted interfaces:

```text
/dev/serial/tty2
/dev/input/barcode0
```

A future native print service should be designed after the desktop and
application-service foundations exist. Its likely direction is a capability-
oriented job service with IPP interoperability. A complete CUPS port is shelved;
future CUPS client, command, IPP, or legacy-driver compatibility may be added at
service boundaries without making CUPS the native architecture.

## Unknown and unbound devices

Every enumerated device remains inspectable even when no driver is available.
Unbound physical devices are published under a diagnostic namespace rather than
being assigned a misleading functional node:

```text
/dev/unresolved/pci17
/dev/unresolved/usb23
```

Example metadata:

```yaml
object_id: device-410
status: unbound
transport: usb
vendor_id: 0x1234
product_id: 0x5678
reported_class: vendor-specific
driver: none
```

If the bus class is recognizable but no driver exists, metadata may include the
probable functional class, but the device should remain under `/dev/unresolved`
until a provider can expose a usable protocol.

When a driver successfully binds, the unresolved diagnostic entry is removed and
one or more canonical logical interfaces are published. The underlying device
identifier does not change.

Failed initialization remains visible with failure metadata so that device
management tools can explain whether the problem is a missing driver, denied
resource, unsupported protocol version, hardware failure, or provider crash.

## Published interface records

A published logical interface record should contain:

- parent physical device identifier;
- validated path component, standard class, and optional subclass;
- logical interface identifier;
- provider identity and monotonically increasing provider generation;
- stable interface identity where available;
- owner UID, owner GID, permission mode, and class-specific policy identifier;
- supported protocol identifier and version range;
- bounded capability and operation sets the provider is willing to delegate;
- optional label, UUID, serial, manufacturer, model, and description;
- bounded flags describing seekability, readiness, removal, reconnection, and
  hotplug behavior; and
- parent-child relationships, such as partitions belonging to a block device.

The ABI should add explicit character-device, block-device, terminal, input,
display, audio, and other necessary `stat` kinds. Major and minor numbers are
optional compatibility metadata rather than authority-bearing identifiers.

## Open and authority flow

Path visibility is not authority. A successful lookup or directory listing
returns metadata only. Opening a device should perform these steps:

1. Canonicalize and route the path to devfs.
2. Resolve aliases to a device object and logical interface generation.
3. Evaluate identity policy using the caller's immutable process credentials,
   node owner/group, mode bits, and any class-specific policy.
4. Ask the current provider generation to create a new session.
5. Attenuate the session to the operations approved for this caller.
6. Install a generation- and session-scoped device open-file description.
7. Release exactly one provider reference when the final shared description is
   destroyed.

A device path must never expose a globally reusable provider endpoint. Raw block
write authority, MMIO, IRQ, DMA, device reset, driver binding, and administrative
control require explicit capabilities even for UID 0. Root and admin identities
influence policy but do not manufacture missing capabilities.

## Descriptor protocols

The common descriptor layer should support operations only where meaningful:

- character devices: `read`, `write`, readiness, and typed control requests;
- block devices: geometry, aligned reads/writes, flush, discard, and bounds;
- terminals: stream I/O, process-group control, size, and terminal settings;
- input devices: event reads, metadata, and readiness notifications;
- display, camera, radio, or audio devices: control IPC plus shared-memory queues;
- printers and scanners: provider sessions intended primarily for brokered future
  services rather than unrestricted direct application access.

Control operations should use device-class-specific request structures with
version, size, flags, and reserved-field validation. NullStar should not begin
with an untyped variadic `ioctl` that allows providers to interpret arbitrary
application memory.

High-throughput devices should use IPC for session setup and control, bounded
shared-memory rings for bulk transfer, and notification capabilities for
readiness. Every index, length, ownership transition, and generation must be
validated on both sides.

## Device-control interface

A future `devctl` utility should be the canonical administrative interface to the
device manager. Candidate operations include:

```text
devctl list
devctl info <device>
devctl bind <device> <driver>
devctl unbind <device>
devctl reset <device>
devctl rescan <device>
devctl eject <device>
devctl power-off <device>
```

The command is a client of the device-manager IPC protocol. Administrative
utilities and the graphical shell must not duplicate device-management logic.

Human-focused convenience commands may wrap common operations. In particular:

```text
eject <device-or-mount-path>
```

should invoke the same transaction as `devctl eject`.

## Removable storage and eject semantics

`eject` is a transactional safe-removal operation, not merely an alias for
unmounting.

The device manager should distinguish:

- **unmount**: detach one or more filesystems while leaving the device active;
- **eject**: safely release a removable device and perform the strongest supported
  logical or physical removal action;
- **power off**: disable device power where hardware and bus policy permit; and
- **media eject**: physically open or release removable media where supported.

A normal eject transaction should:

1. Resolve a partition, stable alias, or mount path to its parent physical
   removable device.
2. Block new mounts and new non-administrative sessions.
3. Enumerate all child partitions, filesystems, swap-like consumers, and open
   device sessions.
4. Ask cooperative services and applications to release handles when policy
   permits.
5. Refuse the operation with useful busy-owner diagnostics if authority remains
   in use.
6. Flush filesystem state and unmount every mounted child filesystem.
7. Flush the block provider and hardware write cache.
8. Issue any supported media-eject, logical-detach, or power-control operation.
9. Mark the device safe to remove and publish a completion event.

For example, specifying `/dev/block/usb-storage0p1` should resolve to
`/dev/block/usb-storage0` and account for every sibling partition before the
physical device is released.

Device metadata should advertise supported operations:

```yaml
capabilities:
  - removable
  - flush-cache
  - logical-detach
  - power-control
```

An optical drive might instead advertise `removable-media`, `media-eject`, and
`tray-control`. `devctl eject` performs the strongest safe action supported by
the selected device while reporting exactly what occurred.

A forced eject may be added later as a privileged recovery operation. It must be
explicit, must never be the default, and must warn that data loss or filesystem
corruption is possible.

## Publication and hotplug

Provider registration requires a capability granted by PID 1 or the future
driver manager. The registry must reject duplicate canonical paths, invalid
classes, unsupported versions, mismatched parent identities, and records
exceeding namespace bounds.

Provider removal atomically hides its entries from new lookup. Existing
descriptors remain tied to their recorded generation and either finish under an
explicit provider contract or become stale and return `IO`. A replacement may
publish the same stable path with a higher generation, but old descriptors must
not silently rebind unless that device class explicitly defines reconnectable
semantics.

The driver manager should publish bounded device events for discovery, binding,
interface addition, interface removal, state change, safe-removal completion,
and failure. Event visibility and administrative control remain separate
authorities.

## Identity policy

The future identity model is specified in
[Identity and access-control design](identity-and-access.md). Initial defaults
should be conservative:

- pseudo-devices such as `null` and `zero` may be world-readable/writable;
- the active terminal is restricted by session and foreground-group policy;
- input devices are not world-readable;
- raw block devices are root-owned and unavailable to regular users;
- ordinary removable-media users receive brokered mount and eject authority, not
  unrestricted raw block access;
- display, camera, microphone, scanner, and radio capture access is mediated by
  class-specific policy;
- printer access may permit job submission through a future print service without
  exposing raw provider control; and
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
- resource queues and shared buffers remain bounded; and
- an interrupted eject transaction leaves the device in a conservative state
  requiring reconciliation before reuse.

Reconnectable devices such as a system console may add an explicit brokered
reconnection policy later. It must not be the default for raw or stateful
devices.

## Userspace-driver prerequisites

A useful devfs can precede userspace hardware drivers by exposing kernel-backed
adapters. Moving hardware drivers out of the kernel additionally requires:

- checked PCI-function and USB-interface ownership and reset;
- constrained MMIO and I/O-port capabilities;
- IRQ notification capabilities;
- pinned DMA buffers and ownership transitions;
- IOMMU isolation or an explicitly documented trusted-driver phase;
- cache, ordering, and device-coherency rules; and
- driver-manager protocols for matching, binding, failure reporting, and
  supervised restart.

Until those primitives exist, AHCI and other hardware remain kernel-backed while
devfs and applications interact with supervised adapter services.

## Recommended milestones

1. Mount a static userspace devfs and implement `null`, `zero`, and `full`.
2. Add device object and logical-interface identifiers plus generation-scoped
   descriptor backends.
3. Adapt `/dev/terminal/console` and `/dev/terminal/tty` to the existing terminal
   subsystem, retaining compatibility aliases where useful.
4. Add the controlled class registry, dynamic provider registration, removal,
   metadata queries, and stable aliases.
5. Publish discovered block devices and read-only partitions under `/dev/block`
   with explicit parent-child metadata and access policy.
6. Add `/dev/unresolved`, driver-match state, and diagnostic failure reporting.
7. Add readiness waiting, typed device-control protocols, and bounded device
   events.
8. Implement brokered mount, unmount, and transactional eject support for
   removable storage, with `devctl eject` and an `eject` convenience command.
9. Introduce IRQ, MMIO, DMA, PCI, and USB-interface capabilities for selected
   userspace drivers.
10. Add additional standard classes as their subsystems mature, including input,
    display, audio, network, radio, camera, and sensors.
11. Reserve printer, scanner, and barcode-reader publication as described here,
    but defer native print and scan services until after the graphical desktop,
    application model, and service framework are established.
12. Consider IPP, CUPS-client, legacy printer-driver, eSCL, or SANE compatibility
    only as later service-boundary milestones.
