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
negotiated writable protocol sessions. PR C added a bounded writable kernel proxy and
public create, write, truncate, append, and unlink. Exact-generation provider offlining
and private quiesce/clean-unmount coordination now give controlled NullFS restart a proven
clean path plus a bounded KILL-and-dirty-recovery fallback. The generated primary volume is
selected by stable UUID, exposed at `/Volumes/NullStar`, and populated with `System/`,
`Applications/`, and `Users/`. All three canonical paths target matching nodes below the
selected provider's backend root, while matching `/Volumes/NullStar` paths remain raw
administrative aliases. A static executable loads through `/System/bin`, and writable user
profile state persists through controlled service replacement without making PID 1 or
recovery depend on NullFS. A fully allocated service image proves deterministic data-block and inode
exhaustion, continued reads, resource reclamation, and subsequent mutation through the public VFS
ABI. A generated no-primary-volume image proves exact UUID lookup failure hands control to the
independently available emergency kernel shell. One policy-pinned PID 1 pilot
loads a definition and executable through
the canonical `/System` binding and verifies generation/readiness restart behavior. Public
`mkdir`/`rmdir`/rename, offline repair policy, and general service-definition management remain
future work.

## Status summary

| Phase | Scope | Status |
| --- | --- | --- |
| 1 | Format foundation | Implemented |
| 2 | Read-only core and host tooling | Implemented |
| 3 | Writable core and recovery | Implemented; hardening continues |
| 4 | Read-only NullStar filesystem service | Implemented |
| 5 | Writable service and namespace adoption | In progress; raw authority, writable service operation, controlled clean restart with dirty fallback, provider offlining, primary volume layout, all three primary-tree bindings, managed user-profile layout, deterministic out-of-space, block-device-loss, and service-crash recovery handling, unavailable-primary recovery proof, static `/System/bin` execution, the bounded service-definition parser, and one policy-pinned PID 1 activation pilot are implemented; boot-generation acceptance and general service management remain incomplete |
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

The current generated image contains a deterministic 4 MiB NullFS primary volume with
stable filesystem UUID and the human-facing display name `NullStar`. PID 1 asks the kernel
for exactly that UUID; zero matches fail with `NO_ENTRY` and multiple eligible matches fail
as ambiguous, without falling back to a partition index or label. The service mounts the
core read-write, and the kernel proxy negotiates exactly `WRITE` and exposes a bounded
writable VFS mount at `/Volumes/NullStar`. The VFS also projects that same provider's
backend-root `/System`, `/Applications`, and `/Users` nodes at their canonical paths.

The accepted long-term direction is:

```text
/Volumes/NullStar/
├── System/
├── Applications/
└── Users/

/System         => namespace binding to the NullStar volume's System node       (implemented)
/Applications   => namespace binding to the NullStar volume's Applications node (implemented)
/Users          => namespace binding to the NullStar volume's Users node         (implemented)
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

The Phase 4 build appended a deterministic 1 MiB, 256-block `NULLSTAR_DATA` volume as
MBR partition 3. At that milestone the kernel identified the partition as NullFS, and an
init-delegated raw probe validated its superblock through the read-only block endpoint. The typed
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
writable filesystem operations. Phase 5 superseded the adapter and service-operation
parts of this description: the service mounted read-write and supported explicitly
writable sessions while the public VFS client remained read-only at that submilestone.
PR C later superseded the public-client restriction without changing the historical
Phase 4 behavior recorded here.

PID 1 registers `nullfs-service` independently of tmpfs as a generation-scoped kernel
filesystem proxy. The VFS mounts it at `/Volumes/NullStar` and binds the provider's
backend-root `System`, `Applications`, and `Users` nodes at their canonical root paths. The
proxy negotiates exactly `WRITE`, requires `session_features::WRITE`, and registers one
kernel-owned 4 KiB shared-memory buffer. Through the public descriptor and filesystem ABI,
the mount supports ordinary stat/read/open plus writable, create, truncate, and append
open, descriptor write, unlink, `fstat`, seek, `read_directory`, and `chdir`. Public
`mkdir`, `rmdir`, and rename remain outside this bounded surface.

Successful opens retain opaque, generation- and session-scoped service nodes in kernel
open-file descriptions. Descriptor duplication and inheritance share the description,
and only final destruction queues `CLOSE_NODE`. During controlled NullFS restart, PID 1
queues a private `QUIESCE` marker behind earlier endpoint work. After exact `QUIESCED`, it
offlines that provider generation before asking the quiesced service to unmount:

- earlier requests complete, while tail work fails and wakes with `IO` (`EIO` at the
  syscall boundary);
- old descriptors remain stale instead of silently rebinding;
- stale replies and later stale I/O continue to fail with `IO`;
- close tickets from the old generation are purged rather than sent to the replacement;
- the preserved tombstone permits registration only with a strictly newer generation and
  a fresh endpoint object.

The service adapter must continue to validate buffer bounds, file offsets, capacities,
operation flags, reply byte counts, session ownership, and provider generation. It must
also preserve clean shutdown, dirty startup, replacement, and block-device-loss
semantics without weakening the on-disk rules.

Current normal-boot coverage includes direct protocol probes and mounted VFS probes
through public syscalls. It exercises mutation through canonical `/Applications` and
`/Users`, cross-view visibility and identity, canonical cwd behavior, and continued
bootstrap availability. The dedicated `nullfs-restart-test` image now validates two controlled
replacements. The first proves exact `QUIESCED`, exact `CLEAN_UNMOUNTED`, final exit `0`,
stale-descriptor `EIO`, persisted cross-view data, and access through a fresh endpoint at a
strictly newer generation. The second stops the service so it cannot
consume `QUIESCE`, then validates timeout, exact-generation offlining, KILL/reap, and a
replacement mount through dirty recovery. Neither controlled restart charges the failure
budget.

```sh
cargo run --locked --quiet -- --nullfs-restart-check
./scripts/check-local.sh
```

The FAT bootstrap path remains independent while the UUID-selected primary volume is
available at `/Volumes/NullStar`. Its bounded public mutation surface, initial backing
layout, all three primary-tree bindings, static `/System/bin` execution, writable profile
state, controlled clean/dirty replacement paths, a bounded service-definition parser, and one
policy-pinned activation path through canonical `/System/services` are implemented. General
definition discovery and enablement, broader namespace mutation, and offline repair policy remain
future work.

## Phase 5: writable service and namespace adoption — in progress

Phase 5 moves from a read-only public test mount to the accepted persistent-volume and
synthetic-namespace architecture. Raw block authority, writable filesystem-service
operations, PR C's bounded public writable proxy, controlled quiesce/clean unmount with
dirty-recovery fallback, stable primary-volume identity and layout, all three primary-tree
namespace bindings, managed user-profile layout, deterministic out-of-space, block-device-loss, and
service-crash recovery handling, unavailable-primary recovery, static `/System/bin` execution, the bounded allocation-free
service-definition parser, and one policy-pinned definition-backed PID 1 activation pilot are
implemented. Phase 5 remains in progress; general service management and the
remaining integrated acceptance work are incomplete.

### Raw writable block authority — implemented

Syscall 52 retains its original read-only partition-index endpoint contract. ABI 1.9
syscall 54 remains available as the legacy PID-1 writable index operation for ABI
compatibility, but primary-volume policy does not use it. The separate ABI 1.10 syscall
55 acquires writable NullFS authority by an exact 16-byte filesystem UUID,
and only PID 1 may call either operation. Writable acquisition requires exactly one match;
zero matches return `NO_ENTRY`, and duplicate eligible UUIDs are rejected as ambiguous.
Candidates must be on a disk without an extended partition, use a nonzero-start primary
MBR `PartitionKind::NullFs` entry that does not overlap another discovered partition, and
contain a valid decoded NullFS superblock. Logical/extended MBR, GPT, and superfloppy
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

Normal boot non-destructively validates the selected writable endpoint's superblock,
out-of-range write rejection, and flush operation. It does not use an allocatable NullFS
data block as raw scratch space; durable mutation coverage runs through filesystem
transactions instead. PID 1 then delegates the writable raw endpoint to `nullfs-service`;
Phase 5's service-operation submilestone consumes that authority as described below.

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

Direct flags-zero sessions remain read-only. PR C changes only the kernel proxy's exact
session request and public VFS policy; it does not merge raw block authority, filesystem
session authority, and public path authority.

### Controlled restart quiesce and clean unmount — implemented

The public filesystem protocol remains version 1 with unchanged `Request`, `Reply`, and
operation definitions. Controlled restart instead uses a private exact 24-byte `NFLC`
version 1 frame carrying a kind plus nonzero service generation and transition ID, all
multibyte fields little-endian. Its kinds are `QUIESCE`, `QUIESCED`, `UNMOUNT`,
`CLEAN_UNMOUNTED`, and `FAILED`; lifecycle events carry no capability.

PID 1 asynchronously queues `QUIESCE` on the existing FIFO request endpoint behind earlier
work. The service completes that work, enters a state that processes no later public
operations, and emits exact `QUIESCED`. PID 1 then offlines the exact generation so tail
work wakes with `EIO`, queues `UNMOUNT`, and waits while the service closes all core open
handles and calls `try_unmount`, including sync and clean-superblock publication. Only the
combination of exact `CLEAN_UNMOUNTED` and final exit `0` proves the clean path; replacement
uses a fresh endpoint and a strictly newer generation.

Any timeout, malformed or wrong event, attached event capability, lifecycle `FAILED`, or
early or nonzero exit takes the non-clean path: exact-generation offline, KILL/reap as
needed, and replacement mount through dirty recovery. Controlled restart does not charge
the failure budget. This is not live filesystem stop/start: filesystem `Start` and `Stop`
remain exactly `Unsupported`, and `NSVC` v1 is unchanged.

### Deterministic block-device loss — implemented

ABI 1.14 lets PID 1 offline the writable NullFS block endpoint selected by the exact
configured filesystem UUID and endpoint-object generation. A wrong, stale, or already-offline
generation cannot affect another incarnation. The endpoint becomes a tombstone rather than
being removed: new acquisition and connection return `IO`, existing sessions remain drainable,
all operations except disconnect return explicit `IO`, and disconnect remains available for
cleanup. This prevents delegated handles from queueing requests forever after provider loss.

The targeted acceptance image prepares a mutation through the public VFS ABI before PID 1
injects loss. The uncertain operation returns `IO`; the proxy does not retry it; the NullFS
service reports `OUTCOME_UNKNOWN`, fail-stops with exit status 35, and PID 1 then offlines the
exact filesystem-provider generation. Old descriptors and path operations return `EIO`, while
an independent FAT/bootstrap read still succeeds. The host runner copies the source image before
each run because the mutation may have reached durable storage before failure became observable.

### Service-backed mutation crash and dirty remount — implemented

The dedicated crash-recovery image starts NullFS with one private receive-only capability that is not
part of public filesystem version 1, `NFLC`, or `NSVC`. PID 1 sends an exact 32-byte `NFCR` version 1
`ARM` frame bound to the current service generation and a nonzero nonce. After the next successful
nonempty `WRITE` has completed its core transaction but before the service queues the filesystem
reply, the service emits an exact `MUTATION_REACHED` event containing the generation, nonce, and
request ID, then exits with status 37.

PID 1 requires that exact event and final status, rejects a wrong-generation offline attempt, and
offlines the exact old filesystem generation. This wakes the one blocked public syscall with `EIO`;
the probe submits it only once and never interprets uncertainty as permission to retry. PID 1 charges
the ordinary failure restart budget, uses a fresh endpoint and strictly newer generation, and exposes
the replacement only after writable mount has completed journal recovery, orphan reclamation,
whole-volume validation, and dirty-state publication.

The replacement probe requires every old descriptor operation to return `EIO`, opens the artifact
through the raw alias, and requires the canonical path to expose exactly the baseline plus one suffix.
It rejects missing, partial, or duplicated content, removes the artifact, and confirms independent FAT
access. The runner always uses a disposable image. Exhaustive power-cut, failed-flush, reordered, and
torn-write boundaries remain covered by the host `CrashBlockDevice` matrices; this native gate
specifically proves the userspace service, kernel proxy, supervision, and remount path.

### Public writable proxy (PR C) — implemented and bounded

For each service generation, the kernel proxy requests exactly `WRITE` and requires the
canonical `CONNECT` reply to include `session_features::WRITE`. The public
`/Volumes/NullStar` mount and bound `/System`, `/Applications`, and `/Users` views support
ordinary stat/read/open, `fstat`, seek, `read_directory`, and `chdir`. Writable, create, truncate,
and append open, descriptor write, and unlink remain available outside the System backing
subtree; canonical and raw public System paths return `READ_ONLY` for mutation. Public
`mkdir`, `rmdir`, and rename remain future.

The proxy reserves its single request before staging at most 4 KiB of write data. On
success, generic `WRITE` keeps the byte count in `value` and returns the exact authoritative
resulting offset as eight little-endian inline bytes, including append's service-selected
EOF. Canonical reply validation treats `OUTCOME_UNKNOWN`, malformed replies, and post-send
mutation uncertainty as `IO`, quarantines the generation, and never automatically retries
an uncertain mutation. These rules rely on, and do not extend, the existing NullFS
transaction and recovery semantics.

Open descriptions for the same generation-, session-, and node-bound file share size
state, preserving append, truncate, cross-handle `fstat`/`SEEK_END`, and open-unlinked
coherence. Exact-generation offlining leaves old descriptors stale and neither replays
mutations nor rebinds descriptions.
Public probes cover create, write, independent stale append, cross-handle `fstat` and
`SEEK_END`, truncate, duplication, unlink while open, open-unlinked read/write, cleanup,
canonical/raw application and user-node identity, persistence across service restart, and
stale old descriptors.

### Primary volume identity and layout — implemented

The generated primary volume is 4 MiB, is selected by its stable filesystem UUID, and has
the human-facing display and mount name `NullStar`. This milestone assigns a new UUID
rather than reusing the earlier `NULLSTAR_DATA` fixture identity; existing development
fixtures must be recreated explicitly instead of being mistaken for the primary layout.
The 4 MiB size is four times the old fixture while keeping exhaustive block-by-block
userspace probes within current QEMU timeouts; larger volumes require allocator and
validation batching. Generated images contain:

```text
System/
Applications/
Users/
```

The display name and on-disk label are not boot keys. PID 1 requests the configured UUID,
and the kernel selects exactly one validated writable NullFS candidate before creating or
returning an endpoint capability. Partition reordering and label changes therefore do not
change selection. Missing, malformed, ineligible, or duplicate configured UUID candidates
cannot cause an arbitrary volume to be mounted.

All three directories remain visible below `/Volumes/NullStar` and are projected at their
canonical root paths. The generated System tree preserves the prior directory shape and
contains a static executable fixture under `bin/`. The generated Users tree contains a
fixture home with `Profile/{config,cache,state,data,logs,runtime}` for integration coverage.

### Namespace bindings — all primary trees implemented

VFS namespace routing protocol version 2 carries binding metadata in a bounded 224-byte
reply. It preserves route ID, backend, and matched canonical-prefix length and adds flags
plus a length-delimited, zero-padded backend-relative backing prefix. The VFS service owns
the binding record; the kernel validates the exact known target and traverses it internally.
The public filesystem protocol remains version 1, and `NSVC` remains version 1.

The implemented bindings are canonical `/System`, `/Applications`, and `/Users` to
matching nodes below the UUID-selected NullFS provider's backend root. Matching paths below
`/Volumes/NullStar` remain raw administrative aliases. Working directories and open-file
paths retain canonical names, while raw and logical views resolve the same underlying
service nodes.

The VFS must preserve canonical logical paths, stable volume-and-node identity, provider
generation, mount policy, and authorization across every binding. The implemented raw
views remain available for administration, but ordinary software uses canonical paths. The
kernel also reapplies the system metadata flag in the canonical `/System` view without
changing raw on-disk metadata, and it exposes both public views of the System subtree as
read-only.

The staged transition has now bound all three primary trees. Statically linked fixtures launch
through `/System/bin`; canonical user-profile state is writable and survives controlled provider
replacement. The version 1 definition format and parser are implemented, and one fixed PID 1
migration pilot loads an exact definition through `/System/services`, applies generation/readiness
lifecycle accounting and bounded `on-failure` restart, and works after controlled provider
replacement. General discovery, enablement, dependencies, and a separate service manager remain
future work. The synthetic root and bootstrap facilities remain usable when the primary volume or
service is unavailable.

### Boot generations

After the namespace is reliable, `/System/boot` should become the canonical source for
complete versioned boot generations. An updater stages and verifies a generation,
commits it durably, atomically selects it, retains a known-good previous generation,
and mirrors the selected artifacts to the firmware-readable bootstrap partition.
Direct NullFS loading by the bootloader is not required for Phase 5.

### Remaining Phase 5 acceptance

PR C, controlled restart, all three primary-tree bindings, static system execution, and the
policy-pinned definition-backed activation pilot, deterministic out-of-space, block-device-loss, and
service-crash recovery gates, and unavailable-primary recovery gate supply bounded writable
public-ABI, clean/dirty replacement, canonical-path, cross-view identity, bootstrap-independence,
normal-boot `/System/services` activation, exact data/inode exhaustion with reclamation, explicit
provider-loss failure without hanging or uncertain retry, a post-commit/pre-reply crash with exact
old-generation `EIO` and single-copy durable remount recovery, and missing-primary recovery coverage.
They do not complete Phase 5. Completion still requires the remaining integrated work to demonstrate:

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
