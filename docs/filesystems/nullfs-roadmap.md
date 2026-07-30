# NullFS implementation roadmap

NullFS is NullStar's native persistent-filesystem format and the planned primary
persistent storage backend for the operating system. Development keeps the on-disk
format and filesystem engine host-testable while preserving the userspace filesystem
service architecture. The format baseline is defined in
[`nullfs-format.md`](nullfs-format.md), the service boundary is defined in
[`../filesystem-service-protocol.md`](../filesystem-service-protocol.md), and the
accepted logical namespace and boot direction is defined in
[`../design/filesystem-namespace.md`](../design/filesystem-namespace.md).

The shared format and core implementation now support writable version 1.2 images,
recovery, checking, deterministic image creation, and an explicitly enabled writable
FUSE adapter. NullStar implements narrowly scoped raw writable block-device authority and
a separately supervised service that mounts `nullfs-core` read-write and offers explicitly
negotiated writable protocol sessions. The kernel proxy still uses a read-only session, so
the public `/Volumes/NULLSTAR_DATA` VFS mount remains read-only. Namespace bindings,
ordinary VFS mutation, offline repair policy, and adoption as the primary backing volume
remain future work.

## Status summary

| Phase | Scope | Status |
| --- | --- | --- |
| 1 | Format foundation | Implemented |
| 2 | Read-only core and host tooling | Implemented |
| 3 | Writable core and recovery | Implemented; hardening continues |
| 4 | Read-only NullStar filesystem service | Implemented |
| 5 | Writable service and namespace adoption | In progress; raw authority and writable service operation implemented, namespace adoption next |
| 6 | Hardening and native-volume features | Planned |

## Architectural position

NullFS is a backend selected by the VFS and accessed through the common filesystem
service protocol. It does not add NullFS-specific application syscalls or move
persistent filesystem policy into the kernel.

```text
NullStar applications
        |
        v
synthetic VFS namespace and routing
        |
        v
common filesystem-service protocol
        |
        v
NullFS service -> shared NullFS core -> block-device adapter
```

The current generated image contains a deterministic NullFS partition labelled
`NULLSTAR_DATA`. Its service mounts the core read-write, but the kernel proxy negotiates a
read-only session and exposes a read-only VFS mount at `/Volumes/NULLSTAR_DATA`. That path
describes implemented behavior, not the final human-facing layout.

The accepted long-term direction is:

```text
/Volumes/NullStar/
├── System/
├── Applications/
└── Users/

/System         => namespace binding to the NullStar volume's System node
/Applications   => namespace binding to the NullStar volume's Applications node
/Users          => namespace binding to the NullStar volume's Users node
```

The VFS owns the synthetic root and canonical logical paths. Namespace bindings are
routing records, not symbolic links. Volume selection uses a stable UUID or equivalent
identity; `NullStar` is a display name. NullFS is therefore planned as the primary
**backing volume**, not as the literal owner of `/`.

The canonical source of future boot generations should be `/System/boot`. Initially,
the selected generation is mirrored to a firmware-readable bootstrap partition. Direct
bootloader traversal of NullFS remains a distant option after the format and recovery
contracts are stable.

## Component boundaries

Dependencies should continue to point inward:

- **format crate**: constants, semantic on-disk types, explicit encoders and decoders,
  CRC32C, feature negotiation, and validation; no ambient I/O;
- **core crate**: allocation, namespace, metadata, file I/O, transactions, recovery,
  and checking over narrow storage and clock traits; independent of FUSE and NullStar
  IPC;
- **storage adapters**: checked access to files, memory images, and the capability-based
  NullStar block-device endpoint described in
  [`../block-device-service-protocol.md`](../block-device-service-protocol.md);
- **service adapter**: maps the common filesystem-service protocol to core operations
  and maintains sessions, generation-bound node handles, registered buffers, and error
  translation;
- **FUSE adapter**: maps host FUSE requests to the same core implementation;
- **tooling**: formatter, image builder, inspector, checker, future repair modes, and
  corruption-test helpers using the shared format and core crates rather than duplicate
  parsers.

The format and core crates remain host-testable Rust libraries. Platform adapters own
IPC, capabilities, asynchronous waiting, and OS-specific lifetimes. Inspection,
checking, and repair are distinct modes; tools must not silently repair malformed
input.

## Phase 1: format foundation — implemented

Phase 1 established the frozen superblock and initial image geometry documented in
[`nullfs-format.md`](nullfs-format.md).

Implemented deliverables include:

1. explicit little-endian encoding and decoding without copying Rust structs directly
   to or from disk;
2. CRC32C generation and verification over the complete 4096-byte superblock;
3. validation of magic, version, feature categories, UUID, UTF-8 label, capacity,
   allocation-group geometry, descriptor reservation, and clean or dirty state;
4. a deterministic formatter with a reserved 64 KiB boot area, primary superblock at
   block 16, and allocation-group descriptor reservation beginning at block 17;
5. a read-only semantic inspector with precise validation errors;
6. golden byte vectors, malformed-input cases, and checked-arithmetic coverage.

Representative validation commands are:

```sh
cargo +nightly-2026-02-01 fmt --all -- --check
cargo +nightly-2026-02-01 test -p nullfs-format -p nullfs-blockdev -p mkfs-nullfs -p nullfs-info --locked
cargo +nightly-2026-02-01 clippy -p nullfs-format -p nullfs-blockdev -p mkfs-nullfs -p nullfs-info --all-targets --locked -- -D warnings
cargo +nightly-2026-02-01 run -p mkfs-nullfs --locked -- --size 64MiB --label Phase1 /tmp/nullfs-phase1.img
cargo +nightly-2026-02-01 run -p nullfs-info --locked -- /tmp/nullfs-phase1.img
```

The phase acceptance coverage verifies deterministic layout, field offsets, reserved
bytes, CRC behavior, feature negotiation, malformed geometry, truncation, dirty-state
handling, arbitrary 4096-byte inputs, and arithmetic near integer limits.

## Phase 2: read-only core and host tooling — implemented

Phase 2 added allocation-group descriptors, inode records, extents, directories,
metadata checksums, and a read-only core over an abstract block source.

Implemented behavior includes:

- root and component lookup;
- attributes and stable generation-aware node identity;
- sparse bounded reads;
- deterministic directory cookies and paginated iteration;
- whole-volume consistency validation;
- deterministic source-tree population through `nullfs-image`;
- corruption tests for records, pointers, lengths, and arbitrary input;
- an optional Linux FUSE adapter using the same format and core implementation.

FUSE was the first realistic host integration target because it enabled ordinary host
workloads and faster debugging. It remains an adapter rather than the NullStar
production interface.

Representative validation commands are:

```sh
cargo test -p nullfs-format -p nullfs-blockdev -p nullfs-testkit -p nullfs-core -p mkfs-nullfs -p nullfs-info -p nullfs-image --locked
cargo check -p nullfs-core --no-default-features --locked
cargo build -p nullfs-fuse --features fuse --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
./scripts/check-local.sh --quick
```

Malformed images must fail without panic or access outside their declared capacity.

## Phase 3: writable core and recovery — implemented, hardening continues

Format version 1.2 implements authoritative allocation bitmaps, redundant superblocks,
persistent generation and transaction state, persistent orphan recovery, and a bounded
data-journaling redo transaction containing at most 64 home-block updates.

The shared core supports:

- writable mount and recovery;
- create and `mkdir`;
- sparse, partial, append, and multi-block writes;
- truncate;
- unlink and POSIX unlink-while-open behavior;
- `rmdir`;
- rename, replacement, and directory-cycle rejection;
- durable `sync` and retryable clean unmount;
- generation-bound opaque open handles;
- transactional free-space accounting;
- read-only committed-journal overlays;
- interrupted-superblock-state reconciliation.

The FUSE adapter exposes writable operation only when explicitly launched with
`--read-write`.

Current automated hardening covers every persistence boundary for representative
create, write, rename, mount, and unmount operations. It includes:

- old-or-fully-committed-new semantic outcomes after injected failures;
- sparse writes and tail clearing after truncate;
- directory growth and hole reuse;
- inode-generation changes on reuse;
- persistent orphan recovery;
- 660 deterministic randomized model operations across three seeds with periodic
  remount and full-tree comparison;
- 51 writable mount and unmount transition boundaries;
- a 126-case non-atomic persistence matrix covering partial dirty-block persistence,
  explicit block sets, reversed write-record persistence, and torn critical-record
  prefixes;
- a live writable FUSE create, write, append, rename, read, and unlink workload.

Remaining Phase 3 hardening includes broader long-running randomized seeds, additional
real-device flush and partial-completion policies, offline repair policy, and eventual
authenticated integrity beyond accidental-corruption detection.

Representative validation commands are:

```sh
cargo test -p nullfs-format -p nullfs-blockdev -p nullfs-testkit -p nullfs-core -p mkfs-nullfs -p fsck-nullfs -p nullfs-info -p nullfs-image --locked
cargo check -p nullfs-core --no-default-features --locked
cargo build -p nullfs-fuse --features fuse --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
./scripts/check-local.sh --quick
```

## Phase 4: read-only NullStar filesystem service — implemented

Phase 4 exposed NullFS through a separately supervised userspace backend behind the
common filesystem protocol. Clients were not routed to a NullFS-specific API. The
read-only service details in this section are historical and are explicitly superseded by
the implemented Phase 5 service-operation submilestone below.

The current build appends a deterministic 1 MiB, 256-block `NULLSTAR_DATA` volume as
MBR partition 3. The kernel identifies the partition as NullFS, and an init-delegated
raw probe validates its superblock through the read-only block endpoint. The typed
session client and storage adapter translate the endpoint's 512-byte device geometry
into the 4096-byte blocks required by the shared core.

The Phase 4 service implemented:

- `CONNECT` and `DISCONNECT`;
- registered shared-memory buffers;
- lookup and attributes;
- read-only open and file reads;
- paginated directory iteration;
- `CLOSE_NODE` accounting;
- session, request, provider-generation, and stale-handle validation.

At the Phase 4 milestone, PID 1 delegated a writable raw NullFS endpoint to the service,
which required `READ | WRITE | FLUSH` metadata and deliberately wrapped the adapter in
`ReadOnlyBlockDevice`. Canonical filesystem mutations therefore returned `PERMISSION`;
that historical behavior demonstrated that raw block authority alone did not enable
writable filesystem operations. Phase 5 has superseded the adapter and service-operation
parts of this description: the service now mounts read-write and supports explicitly
writable sessions, while the public VFS client remains read-only.

PID 1 registers `nullfs-service` independently of tmpfs as a generation-scoped kernel
filesystem proxy. The VFS currently mounts it at `/Volumes/NULLSTAR_DATA`. The proxy
creates its own protocol session and registers one kernel-owned 4 KiB shared-memory
buffer for file and directory transfers. Through the public descriptor and filesystem
ABI, the mount supports ordinary `stat`, read-only `open`, `read`, `fstat`, `seek`,
`read_directory`, and `chdir` operations.

Successful opens retain opaque, generation- and session-scoped service nodes in kernel
open-file descriptions. Descriptor duplication and inheritance share the description,
and only final destruction queues `CLOSE_NODE`. When supervision registers a
replacement generation:

- old in-flight requests fail with `IO`;
- old descriptors remain stale instead of silently rebinding;
- later stale I/O continues to fail with `IO`;
- close tickets from the old generation are discarded rather than sent to the
  replacement.

The service adapter must continue to validate buffer bounds, file offsets, capacities,
operation flags, reply byte counts, session ownership, and provider generation. It must
also preserve clean shutdown, dirty startup, replacement, and block-device-loss
semantics without weakening the on-disk rules.

Current normal-boot coverage includes direct protocol probes and mounted VFS probes
through public syscalls. The dedicated `nullfs-restart-test` image stops a service with
a live descriptor and queued read, registers a replacement on a fresh endpoint, and
verifies deterministic cancellation, stale-handle behavior, safe close handling, and
successful access through the replacement generation.

```sh
cargo run --locked --quiet -- --nullfs-restart-check
./scripts/check-local.sh
```

The current FAT bootstrap path and read-only public `/Volumes/NULLSTAR_DATA` mount remain
until Phase 5 adds namespace-binding support, ordinary VFS mutation and persistence
coverage, and an independent recovery path.

## Phase 5: writable service and namespace adoption — in progress

Phase 5 moves from a read-only public test mount to the accepted persistent-volume and
synthetic-namespace architecture. Its raw block-authority and writable
filesystem-service-operation submilestones are implemented. Namespace adoption and
ordinary VFS mutation remain next (PR C).

### Raw writable block authority — implemented

Syscall 52 retains its original read-only partition-endpoint contract. A separate ABI 1.9
syscall acquires writable authority, and only PID 1 may call either operation. Writable
acquisition currently succeeds only on a disk without an extended partition, for a
nonzero-start primary MBR `PartitionKind::NullFs` partition that does not overlap another
discovered partition and
contains a valid decoded NullFS superblock. Logical/extended MBR, GPT, and superfloppy
writable grants remain disabled until their reserved disk-metadata ranges are modeled
explicitly.
Read-only and writable access are distinct endpoint objects and generations, both
delegated through ordinary endpoint `SEND` rights. Paths, discovery, provider
registration, and UID cannot manufacture write authority.

Read-only `INFO` advertises `READ` plus `READ_ONLY`. Writable `INFO` advertises
`READ | WRITE | FLUSH` without `READ_ONLY`, and the userspace NullFS block-device adapter
rejects writable metadata unless both `WRITE` and `FLUSH` are present. Writes are bounded
to complete registered-buffer blocks, at most 4096 bytes, within both partition-relative
and current disk bounds. The kernel copies the complete source to scratch before issuing
AHCI writes and reports `transferred_blocks` only when every block succeeds. A failed
multi-block write may nevertheless have changed earlier physical blocks, so filesystem
recovery—not blind partial-write retry—is required. Writable `FLUSH` maps to the AHCI
cache flush; read-only `WRITE` and `FLUSH` retain `READ_ONLY` and `NOT_SUPPORTED`.

Normal boot performs a reversible write/flush/readback/restore probe on a known free
sector in the deterministic NullFS fixture. An exact marker left by interruption is
restored and verified on the next boot before testing resumes. All previous read-only and
mutation-denial probes remain active. PID 1 then delegates the writable raw endpoint to
`nullfs-service`; Phase 5's service-operation submilestone consumes that authority as
described below.

### Writable filesystem-service operations — implemented

PID 1 launches `/nullfs-service --writable`. The service accepts only a partition-scoped
raw endpoint advertising `READ | WRITE | FLUSH`, mounts `nullfs-core` read-write, and
announces readiness after journal recovery, orphan reclamation, whole-volume validation,
and dirty-state publication.

Generic `CONNECT` negotiates exact session authority:

- flags `0` return feature bits `0` and create a read-only session;
- exactly `WRITE` returns the `WRITE` feature and creates a writable session;
- unsupported flags are rejected rather than silently downgraded.

Explicit direct writable clients can use `CREATE_FILE`, `CREATE_DIRECTORY`, `WRITE`,
append, `TRUNCATE`, `UNLINK`, `RMDIR`, `RENAME`, and `SYNC`. New files and directories use
modes `0644` and `0755`. Writes are bounded to 4 KiB and copied completely from the
registered window into private service memory before mutation. Rename carries its
destination component through a checked registered bulk-buffer range. Every mutation
requires a session that negotiated `WRITE`; raw authority alone remains insufficient.

Open-unlinked access requires the actual matching open handle. Unlink returns `TRY_AGAIN`
when a read-only-owned matching open would make its later close reclaim storage. Removal
of open directories and unsafe replacement of open rename destinations remain restricted,
and the core continues to reject directory cycles.

A poisoned core or any mutation failure with an uncertain durable result replies
`OUTCOME_UNKNOWN`, then the service fail-stops. Supervision restarts it and the next mount
runs recovery before readiness. Clients must not automatically retry an uncertain or
lost operation; retry is allowed only when an explicit status proves it safe.

The direct normal-boot probe retains read-only-session denial, opens an explicit writable
session, exercises the mutation surface, and cleans its namespace. After interruption it
recognizes and safely removes only the exact reserved artifact forms that the probe itself
can leave behind.

The kernel NullFS proxy deliberately still connects with flags `0`. Kernel mutation
guards and the public `/Volumes/NULLSTAR_DATA` VFS mount therefore remain read-only.
Exposing ordinary VFS mutation is part of namespace adoption/PR C, not this implemented
service-operation submilestone.

### Primary volume identity and layout

The generated primary volume should transition from the development label
`NULLSTAR_DATA` to a stable UUID-backed volume with the human-facing display name
`NullStar`. Generated images should contain:

```text
System/
Applications/
Users/
```

The display name is not a boot key. Namespace and boot policy select the volume by UUID
or another stable identifier.

### Namespace bindings

The VFS must support namespace bindings that map a canonical logical path to a volume
and backing node without exposing symbolic-link aliases. Required bindings are:

```text
/System
/Applications
/Users
```

The VFS must preserve canonical logical paths, stable volume-and-node identity, provider
generation, mount policy, and authorization across each binding. A raw administrative
view below `/Volumes/NullStar` may remain available according to policy, but ordinary
applications use only canonical paths.

A staged transition should bind one non-bootstrap tree first, then load ordinary
programs and service definitions through `/System`, and finally bind all three trees.
The synthetic root and bootstrap facilities must remain usable when the primary volume
or service is unavailable.

### Boot generations

After the namespace is reliable, `/System/boot` should become the canonical source for
complete versioned boot generations. An updater stages and verifies a generation,
commits it durably, atomically selects it, retains a known-good previous generation,
and mirrors the selected artifacts to the firmware-readable bootstrap partition.
Direct NullFS loading by the bootloader is not required for Phase 5.

### Phase 5 acceptance

Phase 5 is complete only when integrated tests demonstrate:

- writable generic-protocol operations through the service and public VFS ABI;
- crash injection and remount recovery for service-backed mutations;
- deterministic out-of-space and block-device-loss behavior;
- clean shutdown ordering and dirty-start recovery;
- namespace binding, canonical-path, and file-identity behavior;
- provider replacement without silent stale-handle rebinding;
- continued access to the bootstrap and recovery environment when the primary volume
  cannot mount;
- normal boot loading non-bootstrap programs and service definitions through
  `/System`;
- boot-generation synchronization and rollback without corrupting the previously
  selected generation.

## Phase 6: hardening and native-volume features — planned

After the writable service and namespace transition are reliable:

- broaden fuzzing across every decoder, protocol adapter, namespace-binding record,
  and recovery transition and retain minimized failures;
- add online checking and scrubbing plus an explicitly gated offline repair mode;
- benchmark allocation, fragmentation, directory scaling, shared-buffer I/O, service
  transitions, and namespace traversal;
- add authenticated integrity where justified;
- add extended attributes, normalization-aware lookup, clones, snapshots, quotas,
  named forks, or other native-volume facilities only through assigned format features
  and versioned generic operations;
- define format, protocol, package, and boot-generation upgrade and rollback procedures;
- evaluate read-only verified system deployments and separately writable application or
  user volumes without changing canonical paths;
- remove legacy persistent-filesystem and bootstrap assumptions only after equivalent
  functionality and recovery coverage are demonstrated.

## Cross-phase rules

- Update the format specification before merging a change to frozen bytes or assigning
  a feature bit.
- Keep generated fixtures reproducible and small; document generators rather than
  checking in opaque large images.
- Treat every disk image and protocol request as untrusted input. Use checked arithmetic,
  bounded allocation, and structured errors.
- Do not couple core transactions to FUSE request boundaries or NullStar IPC message
  boundaries.
- Keep host tools and all platform adapters on the same format and core implementation
  to prevent semantic drift.
- Preserve VFS ownership of routing, mount traversal, canonical paths, and namespace
  bindings throughout migration.
- Keep the independent bootstrap and recovery path until the persistent volume can be
  validated, recovered, mounted, and replaced without depending on itself.
- Update current architecture documentation when a planned phase becomes implemented;
  design documents must not be used to claim features that do not yet exist.
