# Magnetar package and deployment management direction

## Status

**Magnetar** is the accepted name for NullStar's native package and deployment manager,
and `mag` is the accepted command name. The native archive suffix is `.nspkg`.

Magnetar is an **accepted reliable-deployment direction**, not merely a conventional
file-copying package installer. Immutable package objects, complete generations,
transactional activation, dependency solving, verification, rollback retention, and
recovery integration are accepted principles. Exact archive encoding, manifest syntax,
repository-signature implementation, backing-directory names, and solver implementation
remain **tentative design**.

## Purpose

Magnetar manages the complete lifecycle from repository metadata to an active system or
application deployment:

```text
signed repository snapshot or local .nspkg
                    |
                    v
              dependency plan
                    |
                    v
       verified content-addressed objects
                    |
                    v
          complete staged generation
                    |
                    v
       atomic activation and health check
```

The active system must never be left as a partially old and partially new collection of
files. A failed download, verification, extraction, dependency solution, activation, or
health check must leave the previous generation available.

## Core invariants

Magnetar must preserve these invariants:

- resolve the complete dependency graph before changing active state;
- use one immutable repository snapshot for an entire transaction;
- download and verify every required object before activation;
- never silently overwrite a path owned by another package;
- never grant package hooks ambient root, filesystem, device, or network authority;
- construct a complete generation before switching an active binding or boot record;
- retain at least one known-good rollback generation for boot-critical changes;
- keep mutable configuration and state separate from immutable package payloads;
- record enough provenance to explain and reproduce every generation;
- garbage-collect only objects that have no retained generation, process, recovery, or
  explicit pin reference.

## Package and deployment objects

The package system distinguishes:

- **package archive**: one `.nspkg` file plus its manifest, payload, hashes, and
  signatures;
- **package object**: verified immutable content in the local package store;
- **repository snapshot**: a signed immutable catalogue and package-hash set;
- **transaction**: one complete proposed change from a parent generation;
- **generation**: an exact resolved package graph and deployment tree;
- **system deployment**: boot-critical and system-wide packages activated together;
- **application deployment**: one application bundle and its private runtime graph,
  independently selectable where safe;
- **boot generation**: kernel, bootstrap image, manifest, and selection metadata needed
  to enter one system deployment;
- **mutable state**: configuration, databases, logs, caches, and user data outside the
  immutable deployment.

The exact physical package-store and deployment-directory names are not application
interfaces. Applications use canonical `/System` and `/Applications` paths supplied by
VFS namespace bindings.

## `.nspkg` archive

An `.nspkg` archive may contain any package-owned content needed by the system,
including:

- precompiled executables and static or shared libraries;
- application bundles;
- service definitions;
- driver manifests and firmware;
- icons, SVG assets, cursors, fonts, themes, and localization;
- documentation and manual pages;
- default configuration and configuration schemas;
- SDK files, headers, symbols, and development tools;
- recovery utilities and boot-generation inputs.

A possible initial archive layout is:

```text
example-1.4.2-3-x86_64.nspkg
├── manifest.toml
├── payload/
├── metadata/
└── signatures/
```

A deterministic tar-compatible container compressed with Zstandard is a plausible
initial representation, but the archive format must be versioned and canonical enough
for reproducible hashing and signature verification. Extraction paths, links, sizes,
permissions, ownership requests, and special-file declarations are untrusted input and
must be validated before staging.

## Manifest identity

The canonical package ID is separate from the display name:

```toml
format = 1

[package]
id = "org.nullstar.player"
name = "NullStar Player"
version = "1.4.2"
release = 3
epoch = 0
architecture = "x86_64"
kind = "application"
license = "GPL-3.0-or-later"
```

Canonical IDs should be globally unambiguous, case-normalized, path-safe, and stable
across display-name changes. The `org.nullstar.*` namespace is reserved for official
NullStar packages.

Version comparison must use one documented algorithm. `version`, packaging `release`,
and an exceptional `epoch` are distinct fields. A packaging rebuild may increase
`release` without pretending the upstream version changed.

## Architectures

The initial accepted architecture values are:

```text
x86_64   native machine code for the NullStar 64-bit ABI
any      architecture-independent data such as docs, themes, icons, and translations
```

`i686` is reserved for a distant optional compatibility environment. It is not accepted
by the initial native deployment profile. A 64-bit executable dependency must never be
satisfied by a 32-bit library package.

Manifests should also be able to declare platform ABI, loader ABI, required service
protocols, CPU features, and boot or driver compatibility constraints.

## Package kinds and installation domains

Useful package kinds include:

```text
system
application
service
driver
firmware
runtime
development
documentation
localization
debug-symbols
```

The package kind influences activation, required signatures, installation scope,
service restarts, reboot policy, sandboxing, and whether per-user installation is
allowed. Boot-critical packages receive stronger verification and rollback-retention
requirements than documentation or theme packages.

## File classes and ownership

Every payload entry is classified. Initial classes should include:

```text
immutable
config-default
state-template
cache-template
generated
documentation
```

Mutable configuration, state, logs, caches, and runtime data are not ordinary immutable
package files. A package may ship defaults or schemas, while the active machine or user
configuration remains in `/System/config` or the appropriate `Profile` category.

Every immutable installed path has one package owner unless an explicit shared-object
rule permits identical content. Two packages must never silently replace the same path.

Useful queries include:

```text
mag owner /System/bin/example
mag files org.nullstar.example
```

## Dependencies

A dependency record should support at least:

```toml
[[dependencies]]
id = "org.nullstar.ui"
version = ">=1.4,<2.0"
kind = "runtime"
required = true
minimum_abi = 1
```

The solver should understand:

- exact and ranged versions;
- required, optional, recommended, development, and boot dependencies;
- architecture and ABI constraints;
- virtual capabilities supplied through `provides`;
- conflicts, replacements, and preferred providers;
- package pins and holds;
- retained-generation constraints.

The complete solution is presented before installation. If no suitable dependency set
exists, nothing is installed. For a local `.nspkg`, Magnetar may offer to download
matching dependencies from configured trusted repositories; declining cancels the
transaction without changing active state.

## Manual and automatic packages

The package database records why each package is retained:

```text
manual       explicitly requested by a user or administrator
automatic    installed only to satisfy another package or deployment
pinned       retained regardless of reverse dependencies
required     protected by bootstrap, recovery, or system policy
```

Explicitly installing an automatic dependency promotes it to manual.

Removal and pruning should support:

```text
mag remove org.nullstar.player
mag remove org.nullstar.player --prune
mag prune
```

`mag prune` removes only automatic packages with no live reverse dependency, retained
generation, running-process, pin, recovery, or policy reference. The complete prune plan
and reclaimed-space estimate are shown before commit.

## Repository trust

Trust belongs to the repository publisher, not to each content mirror. Mirrors replicate
the same signed repository snapshot and package bytes. A compromised mirror must not be
able to invent a valid package or catalogue.

A repository record should identify:

- canonical repository ID;
- root or trust-anchor key fingerprints;
- delegated online signing keys;
- snapshot sequence and expiration policy;
- mirror URLs;
- channels or tracks;
- package catalogue and object hashes.

OpenPGP signatures and a local trusted-key store are acceptable initial mechanisms.
The design should remain capable of later adding offline root keys, delegated signing,
threshold signatures, expiration, revocation, and rollback/freeze protection.

Adding trust must require explicit fingerprint confirmation or authorization from an
already trusted root. Merely discovering a mirror URL never grants signing authority.

## Mirror ranking

Magnetar should support:

```text
mag mirrors rank --save
```

and may retain the requested convenience alias:

```text
mag -rankmirrors -save
```

Ranking should consider valid snapshot identity, metadata freshness, connection time,
time to first byte, sustained throughput, failure rate, IPv4/IPv6 availability, and
optional region preference. Only mirrors serving the exact signed snapshot qualify.

When `--save` changes the active mirror order, Magnetar writes the new configuration
atomically and retains a bounded timestamped history of the previous list. It should
also provide mirror history and restoration commands rather than accumulating unlimited
backup files.

## Downloads and cache

Parallel downloads are configurable globally and per transaction, with separate total
and per-host limits. The downloader should support:

- resumable partial downloads;
- retry through another valid mirror;
- bandwidth and metered-network policy;
- download-only and offline modes;
- content-addressed cache reuse;
- verified reconstruction from future delta downloads;
- early disk-space and staging-space checks.

No payload is extracted into a deployment until every required package has passed
archive, manifest, hash, signature, architecture, and compatibility validation.

## Command-line interface

Canonical subcommands should include:

```text
mag install <package-or-file>
mag verify <package-file-or-package>
mag list
mag remove <package>
mag prune
mag search <query>
mag info <package>
mag plan <operation>
mag upgrade
mag history
mag generations
mag rollback [generation]
mag pin <generation-or-package>
mag unpin <generation-or-package>
mag mirrors rank --save
mag repo add|remove|trust|list
mag audit
mag repair
mag gc
```

The requested short forms are convenience aliases:

```text
mag -i filename.nspkg
mag -v filename.nspkg
mag -l
mag -r packagename
```

Local installation still performs normal verification, dependency resolution, conflict
checking, and transaction staging. Intentionally unsigned development packages require
an explicit override, are marked visibly in the database, and must not satisfy trusted
boot-critical dependencies by default.

Machine-readable output and stable exit categories should be available for graphical
frontends and automation.

## Transaction lifecycle

A system-changing transaction follows this sequence:

1. load and verify one immutable repository snapshot;
2. resolve the complete dependency and conflict graph;
3. present the plan, download size, installed-size delta, and rollback impact;
4. download every required archive, potentially in parallel;
5. verify signatures, hashes, architecture, manifest schema, and payload structure;
6. import immutable objects into the content-addressed package store;
7. construct a complete staged deployment generation;
8. validate file ownership, executable requirements, service definitions, hooks, and
   available space;
9. record configuration and mutable-state decisions;
10. durably commit generation metadata;
11. atomically select the new application or next-boot system generation;
12. run bounded activation and health confirmation;
13. mark the generation healthy, failed, or pending without deleting its parent.

Only one activation commit may occur at a time, although queries and downloads may be
concurrent.

## Content-addressed store

Package payloads are immutable and addressed by cryptographic hash. Generations refer
to objects rather than copying every unchanged file. This provides:

- deduplication across package versions and generations;
- exact installed-content verification;
- inexpensive rollback;
- safe continued execution of processes using an older generation;
- reliable ownership and provenance;
- generation-aware garbage collection.

The store's exact backing path is an implementation detail visible only to authorized
administrative and recovery tools.

## System and application deployments

A **system generation** covers components whose consistency affects boot or the shared
platform, including the kernel, bootstrap image, PID 1, filesystem and storage services,
core drivers, dynamic loader, core runtimes, and shared system libraries.

An **application generation** may switch one application bundle and its private
libraries independently when that change does not require a system ABI transition.
Application rollback should not force a complete operating-system rollback.

A package transaction may touch both domains only through one explicit plan, with clear
reboot, service-restart, and rollback consequences.

## Generation states and boot fallback

Generations should have a lifecycle such as:

```text
staged
pending
healthy
failed
superseded
pinned
```

Boot-critical activation selects a pending generation while preserving the previous
healthy generation. The bootstrap loader or recovery environment must be able to list
and select retained generations without depending on the active `/System`, package
database service, or dynamic linker.

A configurable failed-boot counter may automatically return to the last healthy
generation. The boot menu should expose healthy, pending, and failed status rather than
presenting generations as indistinguishable entries.

## Transaction record

Every successful or failed transaction retains:

- transaction and parent-generation IDs;
- timestamp and initiating identity/session;
- requested operation and reason;
- before and after package graphs;
- exact versions, architectures, origins, and object hashes;
- manual, automatic, required, and pin changes;
- repository snapshot IDs and signing-key identities;
- configuration merge decisions;
- state-migration declarations and snapshots;
- service activation and reboot plan;
- boot-generation ID and health result;
- rollback compatibility and failure diagnostics.

This record references immutable old objects rather than archiving a separate copy of
every replaced file.

## Configuration and mutable state

Package defaults and administrator or user configuration are separate. An update may
replace an immutable default without overwriting locally owned configuration.

Configuration handling should compare the old default, new default, and active local
value. Possible results include replacing an unchanged default, preserving local edits,
performing a schema-aware merge, or reporting a conflict for explicit resolution.

Removal should distinguish:

```text
mag remove <package>
mag remove <package> --purge-config
mag remove <package> --purge-state
```

Purging state requires stronger confirmation than removing immutable package content.

A service or application state schema must declare forward and rollback compatibility.
If a new package performs a destructive migration that an older generation cannot read,
Magnetar must snapshot the relevant state, postpone migration, warn that rollback is
limited, require explicit approval, or refuse activation. Package-file rollback alone
must never be advertised as full rollback when mutable state is incompatible.

## Services, hooks, and activation

Package manifests may declare service additions, removals, readiness requirements,
restart requests, reboot requirements, and health checks. Magnetar coordinates these
through the service manager; package scripts do not search for PIDs or invoke arbitrary
service commands.

Prefer declarative activation operations for MIME, icons, fonts, users, services,
namespace bindings, and caches. Any unavoidable executable hook runs against the staged
deployment in a capability sandbox with declared writable paths, no network by default,
bounded resources, captured logs, explicit retry behavior, and declared rollback
consequences.

## Verification, audit, and repair

`mag verify file.nspkg` verifies an archive. `mag verify <package>` or
`mag verify --installed` verifies installed immutable content and generation metadata.
A repair operation may restore missing or corrupt immutable objects from a trusted
cache, repository, removable transaction bundle, or retained generation.

Because early NullStar programs will use substantial static linking, package manifests
should record embedded components and versions. `mag audit` can then identify a
vulnerable library even when it is compiled into a Rust executable rather than provided
as a shared package.

## Garbage collection

Generational deployment requires explicit garbage collection:

```text
mag generations
mag generations remove <generation>
mag gc
```

Collection preserves active, pending, healthy-retained, pinned, recovery, running-
process, and policy-required references. A configurable policy may retain the last N
healthy generations, recent generations by age, the last failed generation for
diagnostics, and manually pinned generations.

Every collection plan shows which generations and objects will be removed, why they are
unreferenced, and how much space will be reclaimed.

## Recovery and offline operation

The independent recovery environment should be able to:

- list, verify, pin, and select generations;
- restore a previous boot generation;
- rebuild deployment metadata from signed manifests and object hashes;
- repair immutable objects from cache, removable media, or a trusted repository;
- inspect failed transaction and health logs;
- disable a package or service in a new recovery generation;
- restore repository trust and mirror configuration.

Offline transaction bundles may contain the exact signed repository snapshot, solved
plan, and all required `.nspkg` archives. Applying a bundle must reproduce the same
verified generation without refreshing metadata from the network.

## Initial implementation phases

1. Define deterministic `.nspkg` archives and canonical manifests.
2. Add repository snapshots, trust keys, local verification, and package queries.
3. Add dependency solving, manual/automatic tracking, local installs, and pruning.
4. Add an immutable package-object store and complete generation manifests.
5. Construct staged application deployments and atomically switch one application.
6. Construct staged system deployments and boot generations with previous-generation
   selection.
7. Add health states, failed-boot fallback, generation-aware garbage collection, and
   recovery tooling.
8. Add configuration merges, mutable-state compatibility, audit data, and sandboxed
   declarative activation.
9. Add automatic security policy, offline bundles, richer signing delegation, and
   optional download optimizations.

## Open questions

- The canonical archive container and manifest encoding.
- The initial dependency-solver algorithm and exact version-comparison rules.
- The physical backing layout for immutable objects and deployment generations.
- Whether OpenPGP is the only initial signature format or one supported backend.
- The retained-generation defaults and automatic boot-fallback threshold.
- The first supported user-installation domain.
- How state snapshots integrate with future NullFS snapshots or clones.
