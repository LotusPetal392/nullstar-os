# NullFS implementation roadmap

NullFS is NullStar's planned native persistent filesystem. Development should
keep the disk-format engine testable on a host while preserving the operating
system's userspace filesystem architecture. The format baseline is defined in
[`nullfs-format.md`](nullfs-format.md); the service boundary is defined in
[`../filesystem-service-protocol.md`](../filesystem-service-protocol.md).

This roadmap describes intended work. Phases 1 and 2 now include the
`nullfs-format`, `nullfs-blockdev`, `nullfs-core`, and `nullfs-testkit` crates;
mountable Phase 2 output from `mkfs-nullfs`; inspection through `nullfs-info`;
deterministic source-tree population through `nullfs-image`; and an optional
Linux read-only `nullfs-fuse` adapter. Writable operation, repair tooling, and
the NullStar filesystem service remain future work.

## Architectural position

NullFS will be a backend filesystem service selected by the existing VFS
routing layer. It must not bypass that layer, add NullFS-specific syscalls, or
move path resolution and persistent filesystem logic into the kernel.

```text
NullStar applications
        |
        v
VFS routing and mount namespace
        |
        v
common filesystem-service protocol
        |
        v
NullFS service -> shared NullFS core -> block-device adapter
```

The VFS resolves the rooted namespace and crosses mount points one component at
a time. The NullFS service implements the common `CONNECT` session model, opaque
node IDs, attributes, lookup/open, registered-buffer reads and writes, stable
directory iteration, mutation operations, cancellation, and generation
handling as those operations become available. On-disk inode or extent numbers
remain an implementation detail.

FUSE is the first integration target because it exercises realistic filesystem
workloads with ordinary host tooling and faster debugging. FUSE is an adapter,
not the NullStar production interface and not a substitute for protocol-level
service tests.

## Proposed component boundaries

Names may be adjusted when crates are added, but dependencies should continue to
point inward:

- **format crate**: constants, semantic on-disk types, explicit encoders and
  decoders, CRC32C, feature negotiation, and validation; no ambient I/O;
- **core crate**: allocation, namespace, metadata, file I/O, transactions, and
  recovery over narrow storage and clock traits; independent of FUSE and
  NullStar IPC;
- **storage adapters**: checked access to files, memory images, and eventually a
  NullStar block-device service or capability;
- **service adapter**: maps the common filesystem-service protocol to core
  operations and maintains sessions, generation-bound node handles, registered
  buffers, and error translation;
- **FUSE adapter**: maps host FUSE requests to the same core operations;
- **tooling**: formatter, inspector, checker/repair tool, image builder, and
  corruption-test helpers using the format/core crates rather than duplicate
  parsers.

The format and core crates should remain host-testable Rust libraries. Platform
adapters own IPC, capabilities, async/event loops, and OS-specific lifetimes.
Tools must not silently repair malformed input; inspection, checking, and repair
are distinct modes.

## Phase 1: format foundation

Freeze and test the superblock and initial image geometry in
[`nullfs-format.md`](nullfs-format.md).

Deliverables:

1. A host-testable format library with explicit little-endian field encoding and
   decoding. No Rust struct is copied directly to or from disk bytes.
2. CRC32C generation and verification over the complete 4096-byte superblock.
3. Validation of magic, version, the three feature categories, UUID, UTF-8 label,
   capacity, group geometry, descriptor reservation, and clean/dirty state.
4. A deterministic formatter that reserves 64 KiB, writes the primary
   superblock at block 16, and reserves allocation-group descriptor space from
   block 17.
5. A read-only inspector that reports semantic fields and precise validation
   errors.
6. Golden images/byte vectors and malformed-input tests. Phase 1 does not mount
   files or claim crash recovery.

### Phase 1 acceptance

The Phase 1 workspace packages are `nullfs-format`, `nullfs-blockdev`,
`mkfs-nullfs`, and `nullfs-info`. The acceptance commands are:

```sh
cargo +nightly-2026-02-01 fmt --all -- --check
cargo +nightly-2026-02-01 test -p nullfs-format -p nullfs-blockdev -p mkfs-nullfs -p nullfs-info --locked
cargo +nightly-2026-02-01 clippy -p nullfs-format -p nullfs-blockdev -p mkfs-nullfs -p nullfs-info --all-targets --locked -- -D warnings
cargo +nightly-2026-02-01 run -p mkfs-nullfs --locked -- --size 64MiB --label Phase1 /tmp/nullfs-phase1.img
cargo +nightly-2026-02-01 run -p nullfs-info --locked -- /tmp/nullfs-phase1.img
cargo +nightly-2026-02-01 test --workspace --locked
./scripts/check-local.sh --quick
```

Formatter tests use temporary paths rather than depending on `/tmp`; `/tmp`
above is only a manual smoke-test example.

Phase 1 is accepted only when automated tests demonstrate all of the following:

- golden encoding places every field at its documented offset, writes only
  little-endian values, zeros all reserved bytes, and is byte-for-byte
  deterministic;
- a formatted image has a zeroed 64 KiB boot area, its superblock starts at byte
  65536, and descriptor reservation starts at block 17;
- format/inspect round trips preserve UUID, label, capacity, group fields,
  feature masks, and state;
- known CRC32C vectors pass, single-byte corruption is detected, and the
  checksum field is treated as zero during calculation;
- truncation and mutations of every validated field return errors without panic
  or out-of-bounds I/O;
- non-4096 block sizes, invalid UUID/UTF-8/state/geometry, unsupported major
  versions, and unknown incompatible bits are rejected;
- an unknown read-only-compatible bit rejects writable use but remains eligible
  for read-only inspection/mount planning;
- unknown compatible bits do not prevent use;
- dirty images cannot be selected for writable use in the absence of recovery;
- property or fuzz-style decode tests cover arbitrary 4096-byte inputs and
  checked arithmetic near integer limits;
- the existing workspace test and quick local check remain green.

No QEMU boot marker is required for the host-only format milestone. Once a
NullStar service is added, the full `./scripts/check-local.sh` path and a boot
smoke test become acceptance requirements.

## Phase 2: read-only core

Specify allocation-group descriptors, inode records, extents, directories, and
metadata checksums before depending on them. Implement a read-only core over an
abstract block source.

Deliverables include root lookup, component lookup, attributes, stable node
identity, bounded reads, and deterministic directory cookies. Add fixtures for
empty, sparse, fragmented, and multi-group images, plus corruption tests for
every pointer and length. The checker and inspector should share decoders with
the core.

Implemented Phase 2 packages provide root/component lookup, attributes, sparse
bounded reads, stable directory cookies, whole-volume consistency validation,
deterministic image population, and an optional read-only FUSE adapter. Unit
coverage currently includes traversal, sparse reads, cookies, record corruption,
and arbitrary-record decoding. The adapter builds on Linux; an automated live
FUSE mount workload remains environment-dependent and should run where `/dev/fuse`
is available.

Phase 2 acceptance commands are:

```sh
cargo test -p nullfs-format -p nullfs-blockdev -p nullfs-testkit -p nullfs-core -p mkfs-nullfs -p nullfs-info -p nullfs-image --locked
cargo check -p nullfs-core --no-default-features --locked
cargo build -p nullfs-fuse --features fuse --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
./scripts/check-local.sh --quick
```

Representative images must remain read-only and malformed images must fail
without panic or access outside declared capacity.

## Phase 3: writable core and recovery

Phase 3 is implemented as format version 1.2 with authoritative allocation
bitmaps, redundant superblocks, persistent generation/transaction state, and a
fixed data-journaling redo transaction containing at most 64 home-block
updates. The shared core now supports writable mount/recovery, create, mkdir,
sparse and partial writes, truncate, unlink, rmdir, rename, sync, and clean
unmount. The FUSE adapter exposes writable operation only with the explicit
`--read-write` option.

Define the free-space maps and crash-consistency model before enabling writes.
A journal, copy-on-write scheme, or another transaction design is acceptable
only with documented ordering and replay rules.

Implement create, unlink, rename, truncate, allocation, sparse writes, durable
sync, and clean unmount. Add deterministic crash injection at each persistence
boundary, remount/recovery tests, out-of-space behavior, and checker validation.
The dirty bit must be set durably before mutation and cleared only after the
specified durable commit conditions hold.

FUSE remains the primary workload adapter in this phase. Current automated
coverage enumerates every write/flush failure boundary for create, a multi-block
single-transaction write, and cross-directory rename, requiring recovery to
produce exactly the old or committed new semantic state. It also covers sparse
writes, truncation tail clearing, directory growth/hole reuse, inode generation
changes on reuse, rename replacement, and cycle rejection.

Phase 3 hardening now includes generation-bound opaque open handles, persistent
orphan-list recovery and POSIX unlink-while-open behavior, transactional and
validated free-space counters, read-only committed-journal overlays,
interrupted-superblock-state reconciliation, retryable clean unmount, and 660
deterministic randomized model operations across three seeds with periodic full
tree comparison after remount. FUSE uses the same generation-bound handle model
and propagates clean-unmount failures.

Further hardening now also preserves device ownership on mount failure and
enumerates 51 writable-mount/unmount transition boundaries. A 126-case
non-atomic persistence matrix covers partial dirty-block persistence, explicit
block sets, reversed write-record persistence, and torn critical-record prefixes;
outcomes are old, fully new, or safe rejection, never mixed semantic state. A
live writable FUSE create/write/append/rename/read/unlink workload also passes.

Remaining hardening includes broader long-running randomized seeds, additional
media policies such as partial completion inside a real device flush, offline
repair policy, and eventually authenticated integrity rather than accidental
corruption detection alone.

Phase 3 acceptance commands are:

```sh
cargo test -p nullfs-format -p nullfs-blockdev -p nullfs-testkit -p nullfs-core -p mkfs-nullfs -p fsck-nullfs -p nullfs-info -p nullfs-image --locked
cargo check -p nullfs-core --no-default-features --locked
cargo build -p nullfs-fuse --features fuse --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
./scripts/check-local.sh --quick
```

## Phase 4: NullStar filesystem service

Add the NullFS userspace service as another backend behind the common protocol;
do not route clients directly to a NullFS-specific endpoint.

The service adapter must:

- use `CONNECT`/`DISCONNECT`, session IDs, generations, request IDs, and stale
  session rejection;
- assign opaque node IDs independent of disk addresses and preserve required
  identity across rename;
- use registered shared-memory buffers for file and directory data;
- validate buffer bounds, file offsets, capacities, operation flags, and reply
  byte counts;
- implement `CLOSE_NODE` lifetime accounting so unlinked-but-open nodes can be
  reclaimed correctly;
- expose volume capabilities through versioned generic operations as the common
  protocol gains them;
- handle clean shutdown, dirty startup, service replacement, and block-device
  loss without weakening format rules.

Integrate the service with PID 1 supervision and VFS backend registration using
the same generation checks as other services. Mounting a NullFS volume under
`/Volumes`, or selecting it as a future native root/system volume, remains a VFS
policy decision. Keep the current FAT path until protocol parity, persistence,
and recovery smoke coverage exist.

Acceptance includes protocol conformance tests shared with tmpfs where
applicable, service restart/stale-handle tests, registered-buffer bounds tests,
VFS longest-prefix and mount-crossing tests, and QEMU smoke tests through public
file-descriptor syscalls. Run the complete repository check:

```sh
./scripts/check-local.sh
```

## Phase 5: hardening and native-volume features

After the basic service is reliable:

- add redundant superblocks and allocation-group metadata with explicit recovery
  selection;
- fuzz every decoder and protocol adapter and retain minimized corruption cases;
- add online checking/scrubbing and an explicitly gated offline repair mode;
- benchmark allocation, fragmentation, directory scaling, and shared-buffer I/O;
- add extended attributes, named forks, normalization-aware lookup, clones,
  snapshots, or quotas only through assigned format features and versioned
  generic service operations;
- define compatible format upgrade and rollback procedures;
- remove legacy kernel-resident persistent filesystem paths only after equivalent
  functionality and recovery coverage are demonstrated.

## Cross-phase rules

- Update the format specification before merging a change to frozen bytes or
  assigning a feature bit.
- Keep generated fixtures reproducible and small; document generators rather
  than checking in opaque large images.
- Treat every disk image and protocol request as untrusted input. Use checked
  arithmetic, bounded allocation, and structured errors.
- Do not couple core transactions to FUSE request boundaries or NullStar IPC
  message boundaries.
- Keep host tools and both adapters on the same format/core implementation to
  prevent semantic drift.
- Preserve the current VFS's ownership of routing and mount traversal throughout
  migration.
