# Block-device service protocol

NullStar exposes storage partitions to trusted userspace services through a
versioned capability-based protocol defined in `shared/block_device_protocol.rs`.
The boundary is intended for filesystem services such as NullFS; ordinary
applications continue to use file descriptors and the VFS rather than raw block
access.

## Authorization and capability model

Only PID 1 may call `OPEN_BLOCK_DEVICE_ENDPOINT`. The syscall selects a discovered
filesystem-candidate partition by its partition-table index and returns an
endpoint capability with `SEND | TRANSFER`. PID 1 can delegate a reduced
send-only capability to a supervised filesystem service. The service cannot
receive another client's requests, access the containing disk outside the
selected partition, or manufacture additional authority.

Every endpoint-open call returns an independent handle to the same kernel-rooted
endpoint object. Closing one handle therefore does not invalidate another
caller's handle. Endpoint object identity supplies a nonzero generation used to
scope sessions and reject stale requests.

The initial implementation exports partitions read-only. This deliberately
includes the mounted FAT partition for protocol validation, but no write command
can reach AHCI. Writable access must wait for a dedicated NullFS partition so raw
writes cannot race the mounted boot filesystem.

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
  supported feature bits, and read-only state.
- `READ` copies one or more complete logical blocks into the registered buffer.
- `WRITE` is defined for forward compatibility but currently always returns
  `READ_ONLY` without reading the buffer or issuing disk I/O.
- `FLUSH` is defined for forward compatibility but currently returns
  `NOT_SUPPORTED`.
- `DISCONNECT` releases the session and all kernel roots owned by it.

Unknown operations, flags, reserved fields, transferred capabilities, and
noncanonical operation-specific fields are rejected. Request and reply records
are fixed-size, `repr(C)`, and bounded below the 256-byte endpoint message limit.

## Read bounds and atomicity

All block offsets are relative to the selected partition, never the physical
disk. For every read the kernel validates:

1. nonzero whole-block count;
2. checked `block_offset + block_count` within the partition;
3. checked transfer byte length equal to the requested buffer range;
4. transfer size no greater than `MAX_TRANSFER_BYTES` (4096 bytes);
5. registered-buffer range within both its declared and actual size;
6. checked partition translation to an absolute LBA;
7. current AHCI block size and disk capacity against the configured snapshot.

The kernel reads into a scratch buffer without holding process, capability, or
session locks. Only after every AHCI block succeeds does it copy the complete
result into shared memory. A failed read therefore cannot expose a partially
updated transfer window.

Raw reads are serialized with FAT I/O by the AHCI device lock, but they are not a
multi-operation filesystem snapshot. Filesystem services must rely on their own
on-disk consistency and recovery rules.

## NullFS adapter

`userspace::block_device` provides the typed no-`std` session client.
`nullfs-userspace-blockdev` adapts that client to
`nullfs_blockdev::BlockDevice`, owning monotonic request IDs and chunking larger
core transfers through the registered window. Keeping the adapter in a separate
crate prevents host `std` features from leaking into allocator-free userspace
binaries and lets the future NullFS service remain a distinct package.

The current QEMU boot probe verifies init-only endpoint acquisition, delegated
send-only authority, read-only device metadata, a partition-relative FAT boot
block read, buffer transfer, range rejection, write rejection, unsupported
flush, and disconnect cleanup.

## Next step

Add a dedicated NullFS partition to the image and define the policy for granting
writable authority. Only then should the kernel advertise `WRITE` and `FLUSH`,
and only after crash-ordering tests prove that the userspace adapter preserves
NullFS durability semantics. The first NullFS service integration can meanwhile
use this boundary read-only for mount, lookup, attributes, file reads, directory
iteration, and `CLOSE_NODE` handling.
