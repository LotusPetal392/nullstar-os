# Capability and IPC protection model

NullStar OS is evolving incrementally toward a capability-based microkernel. The
first protection phase adds the primitives needed to move services out of the
kernel without moving any existing service yet. The VFS, FAT implementation,
tmpfs, AHCI driver, terminal, pipes, and other established subsystems continue
to use their current kernel paths.

This document describes the implemented Phase 1 contract. It is not a claim
that the current system is ready for hostile workloads.

## Goals

Phase 1 establishes four properties:

1. A process can refer to a kernel object only through an unforgeable handle in
   its own capability table.
2. Every handle carries an explicit rights mask that can only be reduced when
   it is copied or delegated.
3. Processes can exchange bounded messages and restricted capabilities through
   endpoints.
4. Resource use is bounded so exhaustion returns a defined error instead of
   growing kernel state without limit.

The capability namespace is separate from the file-descriptor namespace. Both
currently use small integers, but a capability handle is valid only for the
capability syscalls and a file descriptor is valid only for descriptor I/O.

## Kernel objects

The Phase 1 registry supports three object types.

| Object | Purpose | Rights |
| --- | --- | --- |
| Endpoint | Bounded FIFO messages and optional capability delegation | `SEND`, `RECEIVE`, `DUPLICATE`, `TRANSFER` |
| Notification | Counted asynchronous event delivery | `SIGNAL`, `WAIT`, `DUPLICATE`, `TRANSFER` |
| Shared memory | Bounded byte storage shared by multiple capability holders | `READ`, `WRITE`, `DUPLICATE`, `TRANSFER` |

`DUPLICATE` authorizes creation of another handle in the same process.
`TRANSFER` authorizes placing a rights-reduced copy in an endpoint message or
copying it into a live direct child's table. Neither operation removes the
source handle.

A requested rights mask must be nonempty, valid for the object type, and a
subset of the source handle's rights. Rights can therefore be attenuated but
not amplified.

## Endpoint messages

Endpoint operations in the kernel are non-blocking:

- sending to a full endpoint returns `TRY_AGAIN`;
- receiving from an empty endpoint returns `TRY_AGAIN`;
- receiving into a buffer smaller than the queued message returns `RANGE`
  without consuming the message;
- a failed capability installation does not consume the message.

The userspace `ipc::receive` helper turns the non-blocking receive operation
into cooperative blocking by yielding and retrying. The kernel queue remains
bounded at every point.

Each message records its sender process identifier, contains at most 256 bytes,
and may carry one rights-reduced capability. Endpoint queues hold at most eight
messages.

## Direct-child bootstrap

Capability transfer through an endpoint assumes that both processes already
hold a handle to that endpoint. Phase 1 resolves this bootstrap problem with a
narrow parent-to-child grant operation.

A process may copy a capability only into a currently live **direct child**. It
must possess `TRANSFER`, and the child receives only the requested subset of
rights. The parent may request a deterministic child handle slot so code on
both sides of a recent `fork` can agree on the bootstrap handle without using a
global namespace.

This is intentionally not a general “open another process” operation. A process
cannot grant directly to siblings, unrelated processes, or arbitrary process
identifiers. Once a bootstrap endpoint is shared, ordinary endpoint transfer is
the expected delegation mechanism.

Capability tables are not implicitly cloned by `fork`. This prevents every
child from automatically receiving all of its parent's authority. The parent
must grant each intended bootstrap capability explicitly. Capability tables are
preserved across `exec` because the process identity remains the same; a future
phase should add close-on-exec or explicit exec filtering policies.

## Notifications

A notification is a checked counter. Signaling adds a nonzero amount with
overflow detection. Waiting consumes one pending event and returns the number
remaining. Waiting on zero pending events returns `TRY_AGAIN`; the userspace
`notification_wait` helper yields and retries.

Notifications are intended to become the basis for interrupt delivery and
lightweight service wakeups. Phase 1 does not connect hardware interrupts to
notification objects.

## Shared memory

Phase 1 shared-memory objects provide common byte storage with independent
`READ` and `WRITE` authority. Access is currently implemented by bounded kernel
copy-in and copy-out operations. The object is shared semantically, but it is
**not yet mapped directly into process address spaces**.

This deliberately postpones several harder requirements:

- page-aligned memory objects;
- per-process mapping addresses and protections;
- revocation of active mappings;
- copy-on-write interactions;
- pinning and DMA ownership;
- cache and device-coherency policy.

Those requirements should be addressed before userspace hardware drivers are
introduced.

## Lifetime and cleanup

The registry records object identity separately from process-local handles.
Objects remain reachable while referenced by a live capability table or by a
capability queued in a reachable endpoint message. Reachability collection
handles endpoint-transfer cycles without relying on recursive reference counts.

Before a capability operation, tables belonging to processes that are no
longer live are removed and unreachable objects are collected. Cleanup is
therefore lazy: a dead process's capability state may remain allocated until the
next capability operation, but all limits remain global and bounded.

## Resource limits

The ABI currently defines these limits:

| Resource | Limit |
| --- | ---: |
| Capability handles per process | 64 |
| Endpoint objects | 32 |
| Notification objects | 32 |
| Shared-memory objects | 16 |
| Messages per endpoint | 8 |
| Bytes per endpoint message | 256 |
| Bytes per shared-memory object | 16 KiB |
| Total shared-memory bytes | 256 KiB |

The constants in `shared/userspace_abi.rs` remain authoritative.

## Verification

The userspace runtime probe verifies:

- capability discovery through `SystemInfo`;
- rights-reduced duplication;
- denial of unauthorized endpoint receive, notification signal, and
  shared-memory write operations;
- shared-memory read/write behavior;
- endpoint payload delivery;
- capability transfer in an endpoint message;
- counted notification delivery;
- explicit handle closure.

The existing fork probe additionally verifies a real cross-process bootstrap:
its parent grants a send-only endpoint to the direct child, the child sends a
message, and then the existing copy-on-write fork and transactional-exec checks
continue. These probes run through the normal QEMU smoke suite.

## Deliberate limitations

Phase 1 does not yet provide:

- userspace drivers or filesystem servers;
- synchronous kernel request/reply IPC;
- kernel-blocked endpoint waits, wait sets, or timeouts;
- priority inheritance or donation across IPC;
- capability revocation trees;
- close-on-exec capability flags;
- memory-mapped shared pages;
- MMIO, I/O-port, IRQ, physical-memory, or DMA capabilities;
- IOMMU isolation;
- a service namespace or discovery protocol;
- a general service supervisor and restart policy.

These omissions mean the current capability layer is a foundation, not yet a
microkernel conversion. The next architectural work should harden this contract
before using it to move `/tmp`, input handling, filesystems, or hardware drivers
out of the kernel.
