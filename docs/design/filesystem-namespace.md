# Filesystem namespace and boot direction

## Status

A synthetic VFS root, canonical logical paths, and VFS namespace bindings are
**accepted direction**. The first staged non-bootstrap binding is now implemented:
canonical `/Applications` targets the UUID-selected primary NullFS provider's
backend-root `/Applications` node. `/System` and `/Users` remain synthetic and unbound,
so complete namespace adoption is still future work. Magnetar-managed immutable
deployments and previous-generation boot selection are accepted direction. Exact
deployment-store layout, boot-generation encoding, final raw-backing visibility, and
the remainder of the transition from the FAT-rooted bootstrap image remain
**tentative design**.

This document describes both that implemented first step and the future architecture.
The current mounted layout remains specified by
[the implementation architecture](../architecture.md), and NullFS implementation work
remains tracked by the [NullFS roadmap](../filesystems/nullfs-roadmap.md).

## Core principle

NullStar separates the logical operating-system namespace from physical storage.
Applications use stable paths that describe what an object is for; the VFS decides
which volume or service provides it.

The long-term root is assembled by the VFS rather than inherited from one disk
filesystem:

```text
/
├── System
├── Applications
├── Users
├── Volumes
├── dev
└── tmp
```

The primary persistent NullFS volume initially contains:

```text
/Volumes/NullStar/
├── System/
├── Applications/
└── Users/
```

The VFS projects those trees into their canonical locations. The first projection is
implemented; the other two remain planned:

```text
/System         => NullStar volume, System node          (planned; currently synthetic)
/Applications   => NullStar volume, Applications node    (implemented)
/Users          => NullStar volume, Users node            (planned; currently synthetic)
```

`/dev` and `/tmp` remain service-backed mounts. Other local, removable, encrypted,
and network volumes appear below `/Volumes` without changing the logical locations
used by applications.

## Namespace bindings

A namespace binding is a VFS routing record, not a symbolic link. It identifies:

- the canonical logical path;
- the backing volume identity;
- the backing filesystem node identity;
- the provider generation and protocol session;
- mount and access policy;
- whether a raw administrative view is exposed.

Opening `/Applications/example` retains `/Applications/example` as its canonical view
path, and changing into the tree retains a `/Applications/...` working directory. Ordinary
clients do not observe a symlink expansion to
`/Volumes/NullStar/Applications/example`. The same rule will apply to future `/System` and
`/Users` bindings.

Bindings avoid the problems caused by path aliases:

- applications do not persist storage-layout details;
- sandbox rules can target one canonical namespace;
- moving a tree to another volume does not change application-visible paths;
- a changed display label does not break boot or configuration;
- file identity is based on volume and node identity rather than only text paths.

The term **namespace binding** is preferred in native documentation. A POSIX
compatibility layer may expose equivalent behavior as a bind mount where useful.

The implemented routing contract is VFS namespace protocol version 2. Its bounded
224-byte reply preserves the route ID, backend, and matched canonical-prefix length, and
adds a binding flag plus a length-delimited, zero-padded backend-relative backing prefix.
The VFS service owns the `/Applications` binding record. The kernel validates that it is
the exact known NullFS target with backing prefix `/Applications`, appends only the
unmatched canonical suffix, and traverses that backend path internally. This first version
deliberately does not let the service redirect the kernel to arbitrary backing targets.

## Volume identity and naming

The display name `NullStar` is intended for the human-facing mount below `/Volumes`.
Boot selection and namespace policy must use a stable volume UUID or another
non-display identifier.

A volume record should distinguish:

- stable volume identity;
- current provider and generation;
- display name;
- filesystem type and feature set;
- writable, removable, encrypted, and degraded state;
- the stable root node supplied by the filesystem service.

Renaming a volume changes its display path below `/Volumes`; it must not invalidate a
binding selected by UUID.

## Canonical and administrative views

`/System`, `/Applications`, and `/Users` are the canonical paths for normal software.
Today only `/Applications` has persistent backing; `/System` and `/Users` remain synthetic
and unbound. `/Volumes/NullStar/Applications` remains the raw administrative alias for the
same NullFS node reached canonically as `/Applications`. The broader raw backing view below
`/Volumes/NullStar` is intended for recovery and administration.

The final visibility policy is tentative. Acceptable implementations include:

- exposing both views only to authorized administrative tools;
- showing the volume root while hiding nodes already projected elsewhere;
- exposing a read-only raw view;
- retaining both views but marking logical paths as canonical in file identity APIs.

Regardless of presentation, path-based access checks must not accidentally grant more
authority because the same node is reachable through an administrative alias.

## Future volume separation

The logical namespace allows storage policy to evolve without changing applications.
For example:

```text
/System         => verified read-only system volume
/Applications   => application volume
/Users          => writable user-data volume
```

A user home may later be backed by an encrypted per-user volume:

```text
/Users/Natalie  => encrypted volume selected at login
```

Applications continue to use `/Users/Natalie`. They neither select nor need to know
the physical provider.

The accepted per-user managed-data layout remains:

```text
/Users/<name>/Profile/
├── config/
├── cache/
├── state/
├── data/
├── logs/
└── runtime/
```

Its directory contracts are defined in
[the userspace architecture](userspace-architecture.md).

## Boot architecture

The persistent source of truth for boot artifacts should be `/System/boot`:

```text
/System/boot/
├── generations/
│   ├── 41/
│   │   ├── kernel
│   │   ├── bootstrap-image
│   │   ├── manifest
│   │   └── checksums
│   └── 42/
│       ├── kernel
│       ├── bootstrap-image
│       ├── manifest
│       └── checksums
├── selected-generation
└── previous-generation
```

The exact record format and naming are tentative. Magnetar owns construction and
verification of complete deployment and boot generations; the bootstrap loader owns
independent enumeration and selection. The required behavior is:

1. resolve and stage a complete deployment generation;
2. verify every package object, boot artifact, manifest, and compatibility requirement;
3. durably commit the generation without modifying the active deployment;
4. mark it `pending` and atomically select it for the next boot;
5. retain a known-good `healthy` previous generation;
6. let the bootstrap loader enumerate retained healthy, pending, and failed generations;
7. mark the pending generation `healthy` only after PID 1 reports the agreed system
   health milestone through an authenticated channel;
8. mark or count a failed attempt without destroying the parent generation;
9. automatically or manually return to the last healthy generation according to policy.

Initially, a boot synchronization service should copy the selected generation to a
small firmware-readable bootstrap partition. This keeps BIOS and future UEFI loading
independent from the full NullFS format while making `/System/boot` canonical after the
system is running. Boot selection must not depend on the active dynamic linker,
package-management service, or the potentially damaged generation.

Direct bootloader traversal of NullFS is a distant option, not a prerequisite. It
should be considered only after the on-disk format, recovery rules, and loader
compatibility policy are stable.

## Bootstrap and recovery

A userspace filesystem service cannot load itself from a root that is unavailable.
The boot image therefore needs a small independent bootstrap set containing enough to:

- start PID 1 or an early supervisor;
- access the boot storage path;
- start the VFS and NullFS services;
- validate or recover the primary volume;
- install namespace bindings;
- provide a recovery shell and diagnostics when normal activation fails.

If the primary NullFS service cannot mount, the synthetic root and bootstrap facilities
remain available. `/System`, `/Applications`, and `/Users` may be marked unavailable,
while `/Volumes`, essential devices, temporary storage, logs, and recovery tools remain
usable.

A root-backing service restart must preserve the normal provider-generation rules:
old in-flight requests fail deterministically, old handles remain stale, and a
replacement does not silently inherit old sessions. Essential services must reopen
resources through the new generation.

## Transition from the current system

The recommended progression is:

1. Retain the independent FAT bootstrap and recovery environment while strengthening
   protocol and recovery coverage. (active foundation)
2. Give the main NullFS volume the human-facing `/Volumes/NullStar` identity, select it by
   UUID, and populate `System`, `Applications`, and `Users`. (implemented)
3. Add VFS namespace protocol version 2 and bind one non-bootstrap tree first.
   (implemented for `/Applications`)
4. Bind `/System` and load ordinary programs and service definitions through it while
   retaining the existing bootstrap path.
5. Bind `/Users`, complete policy for all three persistent trees, and treat the synthetic
   VFS root as the normal namespace.
6. Complete administrative visibility, authorization, recovery, and namespace-mutation
   policy without weakening the implemented writable-service lifecycle.
7. Add Magnetar-managed immutable system and boot generations, health states,
   previous-generation selection, and synchronization to the firmware-readable bootstrap
   partition.

Current integration coverage exercises canonical `/Applications` mutation, visibility and
identity through both canonical and raw views, stale descriptors across NullFS service
restart, and continued bootstrap availability. Each later stage must retain that independent
recovery path and integrated boot coverage.

## Open questions

- Whether the raw `/Volumes/NullStar/System` view is visible to ordinary sessions.
- The exact stable file-identity representation across bindings and service restart.
- How namespace changes are authorized, audited, and made atomic.
- Whether `/System` and `/Applications` become read-only deployments before or after
  the first writable NullFS-backed user homes.
- The deployment-store backing layout and exact relationship between system,
  application, and boot generation identifiers.
- The boot-generation manifest, signature, rollback, attempt-counter, and
  health-confirmation formats.
- How removable or temporarily unavailable bound volumes appear to applications.
