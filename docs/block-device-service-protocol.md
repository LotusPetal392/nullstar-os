# Block-device service protocol

NullStar exposes storage partitions to trusted userspace services through a
versioned capability-based protocol defined in `shared/block_device_protocol.rs`.
The boundary is intended for filesystem services such as NullFS; ordinary
applications continue to use file descriptors and the VFS rather than raw block
access.

## Authorization and capability model

Only PID 1 may acquire a partition endpoint. `OPEN_BLOCK_DEVICE_ENDPOINT`
retains its original read-only contract. The separate
`OPEN_WRITABLE_BLOCK_DEVICE_ENDPOINT` syscall requests writable access and
succeeds only when the disk has no extended partition and the selected entry is
a nonzero-start primary MBR partition classified as `PartitionKind::NullFs` that
does not overlap any other discovered partition and
contains a valid decoded NullFS superblock. Logical/extended MBR, GPT, and
superfloppy writable grants remain disabled until their reserved disk-metadata
ranges are modeled explicitly. Each successful call returns an endpoint capability
with `SEND | TRANSFER`; PID 1 can delegate a reduced send-only capability to a
supervised filesystem service. The service cannot receive another client's
requests or access the containing disk outside the selected partition.

Read-only and writable access to the same partition are separate kernel-rooted
endpoint objects with separate nonzero generations. Repeated acquisition of the
same partition and access mode returns an independent handle to that mode's
object, so closing one handle does not invalidate another caller's handle. Both
modes use ordinary endpoint rights, including delegated `SEND`; the endpoint
object's access mode is the write authority. Discovering a path or partition,
registering a provider, or possessing a UID—including a future UID 0—cannot
manufacture that authority.

## Session bootstrap

The protocol follows the persistent-reply and registered-buffer pattern used by
filesystem services:

```text
client                                  kernel partition service
  | CONNECT + reply SEND capability ------------> |
  | <---- CONNECT reply on persistent endpoint    |
  |                                               |
  | ATTACH_BUFFER + shared-memory capability ---> |
  | <---- attachment reply                        |
  |                                               |
  | INFO / READ / WRITE / FLUSH ----------------> |
  | <---- reply matched by request_id             |
  |                                               |
  | DISCONNECT ---------------------------------> |
  | <---- final reply                              |
```

Sessions are bound to the sender PID, endpoint generation, and a monotonic
session ID. A process cannot use another process's session identity. Each
session owns one persistent reply endpoint and at most one registered shared
memory buffer. Kernel roots keep those objects alive until `DISCONNECT`, reply
delivery failure, or process death.

The kernel processes at most one block-device request per runtime poll. Clients
cooperatively yield and retry when the bounded request queue reports
`TRY_AGAIN`.

## Operations

- `CONNECT` establishes a generation-bound session and transfers a send-only
  reply endpoint.
- `ATTACH_BUFFER` registers one shared-memory object for block transfers.
- `INFO` returns the logical block size, partition-relative block count,
  supported feature bits, and read-only state. Read-only endpoints advertise
  `READ` plus `READ_ONLY`; writable endpoints advertise `READ | WRITE | FLUSH`
  without `READ_ONLY`.
- `READ` copies one or more complete logical blocks into the registered buffer.
- `WRITE` writes one or more complete logical blocks from the registered buffer
  on a writable endpoint. A read-only endpoint returns `READ_ONLY` without
  reading the buffer or issuing disk I/O.
- `FLUSH` on a writable endpoint maps to an AHCI cache flush. A read-only
  endpoint returns `NOT_SUPPORTED`.
- `DISCONNECT` releases the session and all kernel roots owned by it.

Unknown operations, flags, reserved fields, transferred capabilities, and
noncanonical operation-specific fields are rejected. Request and reply records
are fixed-size, `repr(C)`, and bounded below the 256-byte endpoint message limit.

## Transfer bounds and completion

All block offsets are relative to the selected partition, never the physical
disk. For every read and write the kernel validates:

1. a nonzero whole-block count and exact complete-block buffer length;
2. checked `block_offset + block_count` within the partition;
3. a checked transfer byte length no greater than `MAX_TRANSFER_BYTES` (4096
   bytes);
4. the registered buffer identity and range against both its declared and actual
   size;
5. checked partition translation to an absolute LBA;
6. current AHCI logical block size and disk capacity against the configured
   partition snapshot.

The kernel reads into a scratch buffer without holding process, capability, or
session locks. Only after every AHCI block succeeds does it copy the complete
result into shared memory. A failed read therefore cannot expose a partially
updated transfer window.

For writes, the kernel first copies the complete source range from registered
shared memory into a scratch buffer, then issues the AHCI block writes. It sets
`transferred_blocks` only after every requested block succeeds. That reply rule
does **not** make a multi-block write atomic: if an AHCI write fails, earlier
physical blocks may already have changed even though the reply reports no
transferred blocks. Callers must use filesystem durability and recovery rules
and must never treat such a failure as a safely retryable partial write.

Raw reads, writes, and flushes are serialized with FAT I/O by the AHCI device
lock, but they are not a multi-operation filesystem transaction or snapshot.
Filesystem services must rely on their own on-disk consistency and recovery
rules.

## NullFS adapter

`userspace::block_device` provides the typed no-`std` session client.
`nullfs-userspace-blockdev` adapts that client to
`nullfs_blockdev::BlockDevice`, owning monotonic request IDs and chunking larger
core transfers through the registered window. It exposes 4096-byte NullFS blocks
and translates each one into a checked run of protocol logical blocks; the
current 512-byte logical-block device therefore uses eight protocol blocks per
core block. The adapter rejects metadata that marks a device writable unless
both `WRITE` and `FLUSH` are advertised, preventing a writable core from running
without the durability primitive it requires. Keeping the adapter in a separate
crate prevents host `std` features from leaking into allocator-free userspace
binaries and keeps `nullfs-service` a distinct package.

The current normal boot verifies init-only endpoint acquisition, delegated
send-only authority, read-only and writable metadata, a partition-relative FAT
boot-block read, and the checksummed superblock of the dedicated `NULLSTAR_DATA`
partition. It also performs a reversible probe on a known free sector in the
deterministic NullFS fixture: read the original sector, write a distinct marker,
flush, read it back, restore the original sector, flush again, and verify the
restoration. If a previous boot stopped after making the marker durable, the
next probe recognizes that exact marker and restores it before repeating the
test. The previous read-only buffer-transfer, range-rejection, write-denial,
unsupported-flush, mutation-denial, and disconnect-cleanup probes remain active.

PID 1 explicitly launches `/nullfs-service --writable` and gives it a send-only
handle to the writable raw NullFS endpoint. The service requires
`READ | WRITE | FLUSH` metadata, constructs the `nullfs-userspace-blockdev`
adapter, and mounts the shared NullFS core read-write. Journal recovery, orphan
reclamation, whole-volume validation, and dirty-state publication complete
before it announces readiness.

The block endpoint authorizes only partition-scoped raw operations. The service
separately requires exact generic-filesystem `CONNECT` negotiation: flags `0`
return a read-only session, while exactly `WRITE` returns the `WRITE` feature;
there is no silent downgrade. Its direct protocol probe preserves read-only
denial, then uses an explicit writable session to test create, directory
creation, write, append, truncate, rename, unlink, `rmdir`, and sync. It cleans
the namespace and, after interruption, safely removes only exact reserved
artifact forms left by the probe.

PID 1 also registers each `nullfs-service` process as an independent,
generation-scoped kernel filesystem proxy, while the VFS statically mounts the
backend at `/Volumes/NULLSTAR_DATA`. The filesystem proxy's own kernel-registered
4 KiB buffer is distinct from the service's block-device session window. The
proxy connects with exactly `WRITE`, requires `session_features::WRITE`, and
permits ordinary stat/read/open plus writable, create, truncate, and append open,
descriptor write, unlink, `fstat`, seek, `read_directory`, and `chdir`. Public
`mkdir`, `rmdir`, rename, and broader namespace adoption remain future.

## Writable authority

Partition-scoped raw writable authority is implemented only for discovered
NullFS partitions and can enter userspace only through PID 1 acquisition and
capability delegation. `nullfs-service` now consumes that endpoint through a
read-write core mount, but raw authority remains distinct from both writable
filesystem-session authority and public VFS authority.

Explicit direct filesystem clients can negotiate the `WRITE` session feature
and use the service's bounded mutation operations; flags-zero direct sessions
remain read-only. The kernel NullFS proxy separately negotiates exactly `WRITE`
and applies bounded public VFS policy at `/Volumes/NULLSTAR_DATA`. Its write path
reserves the single proxy request before staging at most 4 KiB and validates the
generic reply's byte count plus exact eight-byte little-endian resulting offset.
That offset records append's service-selected EOF.

Malformed replies, `OUTCOME_UNKNOWN`, and post-send mutation uncertainty map to
`IO`, quarantine the filesystem generation, and are never automatically retried.
Descriptors share generation/session/node size state, while replacement leaves
old descriptors stale without replay or rebinding. Public `mkdir`, `rmdir`,
rename, and namespace identity/bindings remain future; none are implied by raw
block access.

A failed raw write can have modified storage despite reporting no completed
blocks. At the filesystem layer, poisoned or otherwise uncertain mutation
failures therefore return `OUTCOME_UNKNOWN`; the service sends the reply and
fail-stops so supervision remounts through journal/orphan recovery. Callers must
not automatically retry such operations.
