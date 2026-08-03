# Capability and IPC protection model

NullStar OS is evolving incrementally toward a capability-oriented, service-based
architecture. This document records the implemented protection primitives and explains
how later filesystem-service work now uses them. It is not a claim that the system is
ready for hostile workloads.

## Current status

The original protection phase introduced bounded process-local capability tables,
rights-reduced duplication and delegation, endpoints, counted notifications, shared
byte-memory objects, scheduler-integrated endpoint waiting, and direct-child bootstrap
grants.

Since that phase, the capability and IPC foundation has been used to move several
filesystem responsibilities across userspace service boundaries:

- the userspace tmpfs service is the active `/tmp` backend;
- a separately supervised VFS service owns the versioned longest-prefix namespace table;
- a separately supervised NullFS service mounts its core read-write and offers explicitly
  negotiated writable sessions; its kernel proxy also negotiates bounded public write
  authority for `/Volumes/NullStar`;
- PID 1 delegates endpoint authority, including a narrowly scoped writable raw NullFS
  block endpoint, to those services;
- an allocation-free userspace service-route layer now separates stable logging producer and
  observer route grants from generation-specific provider ingress authority;
- provider generation, protocol session, request, and stale-handle checks protect
  replacement boundaries.

Other established subsystems—including AHCI, terminals, pipes, much of descriptor
ownership, and several VFS proxy paths—remain kernel-resident. Shared memory still uses
bounded kernel-mediated copies rather than direct page mappings. General MMIO, IRQ, DMA,
revocation, and named service-discovery capabilities are not yet implemented.

## Goals

The implemented model establishes these properties:

1. A process can refer to a protected kernel object only through an unforgeable handle
   in its own capability table.
2. Every handle carries an explicit rights mask that can only be reduced during
   duplication or delegation.
3. Processes exchange bounded messages and restricted capabilities through endpoints.
4. Resource use is bounded so exhaustion returns a defined error.
5. Supervised service replacement does not silently rebind old sessions or handles.

The capability namespace is separate from the file-descriptor namespace. Both use small
integers today, but handles are valid only for capability operations and descriptors are
valid only for descriptor and filesystem I/O.

## Implemented kernel objects

| Object | Purpose | Principal rights |
| --- | --- | --- |
| Endpoint | Bounded FIFO messages and optional capability delegation | `SEND`, `RECEIVE`, `DUPLICATE`, `TRANSFER` |
| Notification | Counted asynchronous event delivery | `SIGNAL`, `WAIT`, `DUPLICATE`, `TRANSFER` |
| Shared memory | Bounded byte storage shared by capability holders | `READ`, `WRITE`, `DUPLICATE`, `TRANSFER` |

`DUPLICATE` creates another handle in the same process. `TRANSFER` permits placing a
rights-reduced copy in an endpoint message or granting it to a live direct child. Neither
operation removes the source handle.

A requested rights mask must be nonempty, valid for the object type, and a subset of the
source rights. Rights can be attenuated but not amplified.

## Endpoint messages and waiting

Endpoint operations remain bounded:

- sending to a full queue returns `TRY_AGAIN`;
- receiving from an empty queue returns `TRY_AGAIN`;
- a too-small receive buffer returns `RANGE` without consuming the message;
- failed capability installation does not consume the message;
- each message contains at most 256 bytes and at most one transferred capability;
- each endpoint queue holds at most eight messages.

The kernel provides an endpoint-readiness wait used by service clients and proxies.
Userspace helpers may also yield and retry nonblocking operations. Protocols remain
responsible for request IDs, deadlines or cancellation where defined, generation checks,
and bounded reply validation.

## Direct-child bootstrap

Endpoint transfer assumes both peers already share an endpoint. The current bootstrap
operation therefore permits a process to copy a capability only into a live direct
child. The source must carry `TRANSFER`, and the child receives only a requested subset
of rights. A deterministic child slot can be requested so parent and child agree on the
initial handle across `fork` and `exec`.

This is intentionally not a general operation for opening another process. It cannot
grant directly to siblings, unrelated processes, or arbitrary process identifiers.

## Userspace service-route use

The [service route protocol](service-route-protocol.md) builds generic discovery and issuance from
ordinary endpoint capabilities without adding service names or application-protocol parsing to the
kernel. Its `no_std` route table and codec are allocation-free. PID 1 currently supplies the broker
policy and publication lifetime for the logging pilot; it is a temporary broker, not the final
service manager.

A stable route grant has exact `SEND` rights and is bound by the broker to one UUIDv4 service ID and
nonzero role. Logging producer role `1` and observer role `2` under service ID
`7cbd3f65-50a6-4c30-b195-9fbed633da43` are separate authorities. Knowing those identifiers grants
nothing.

`NSRT` v1 records are exactly 40 bytes. A request must transfer exactly one fresh empty reply
endpoint with exact `SEND` rights. An accepted reply transfers exactly one current provider ingress
with exact `SEND` rights; a failure transfers no capability. These cardinalities use the implemented
endpoint limit of at most one transferred capability per message. The broker validates the granted
key and kernel-stamped sender PID, authorizes before checking availability, and never parses NSWP or
logging packets.

Provider publication retains a stable `SEND | DUPLICATE | TRANSFER` source from which the broker
issues reduced send-only handles. Each provider generation has fresh ingress endpoint objects. This
prevents an old route from reaching the replacement, but it does not revoke all old handles: the
kernel has no general revocation operation, and an endpoint object remains reachable while a handle
or queued transfer refers to it. The current logging pilot also uses provider PID as generation,
which conflates process and service-generation identity until a service manager owns an independent
counter.

The distinction has a resource cost. The kernel currently permits 32 live endpoint objects
system-wide. Every in-progress route resolution creates a private reply endpoint, every provider
generation creates fresh ingress objects, and retained old handles can delay object collection.
Resolution or publication can therefore fail under endpoint pressure even if a route-table slot is
available. Fixed route tables add a separate bound: withdrawn keys leave generation tombstones that
continue to consume distinct-key capacity.

The route broker never queues or replays application traffic. A one-way logging `Emit` is not
replayed on a replacement when processing by the old provider is uncertain; generation isolation
cannot determine whether that record was retained before failure.

## Filesystem-service use

The common [filesystem service protocol](filesystem-service-protocol.md) builds on these
primitives. A client establishes a session through an endpoint, transfers a persistent
reply endpoint and registered shared-memory capability, then uses bounded request and
reply records carrying session, generation, request, operation, and buffer identities.
For NullFS, `CONNECT` flags `0` negotiate no writable features and exactly `WRITE`
negotiates the `WRITE` session feature; unsupported requests are rejected without
downgrade. Every mutation requires that feature even though the service process itself
holds raw write authority.

The VFS and kernel proxies validate:

- protocol version and fixed record sizes;
- provider and mount generation;
- session and request identity;
- operation and reserved fields;
- transferred capability type and rights;
- buffer identifiers, offsets, lengths, reply byte counts, and authoritative resulting
  write offsets.

Open-file descriptions retain shared generation-, session-, node-, and size-bound state.
This keeps append, truncate, cross-handle `fstat`/`SEEK_END`, and open-unlinked access
coherent across descriptor aliases. When a provider is replaced, old in-flight requests
fail, old descriptors remain stale, and neither mutations, descriptions, nor old close
records are replayed or rebound to the replacement.

For writable NullFS operations, the service copies each complete write of at most 4 KiB
from shared memory into private storage before entering the core. The public proxy first
reserves its single request and only then stages at most 4 KiB. A successful generic
`WRITE` reply retains its byte count in `value` and carries the exact resulting offset as
eight little-endian inline bytes, allowing append to report its service-selected EOF.
Open-unlinked access requires the actual matching session-owned open handle. Unlink is
rejected if a read-only session owns an open whose later close could reclaim storage;
open-directory removal and unsafe rename replacement are also restricted.

If a mutation's durable outcome is uncertain or the core is poisoned, the service sends
`OUTCOME_UNKNOWN` and fail-stops. Its supervisor replaces it, causing a fresh mount and
recovery before readiness. The public proxy also maps `OUTCOME_UNKNOWN`, malformed
replies, and post-send mutation uncertainty to `IO`, quarantines the generation, and never
automatically retries. Neither a lost reply nor service replacement proves that the prior
mutation did not commit; durability remains limited to NullFS's transaction and recovery
semantics.

## Raw block-device authority

Only PID 1 may acquire a discovered partition endpoint. The existing acquisition syscall
remains permanently read-only; a separate syscall requests writable access and succeeds
only on a disk without an extended partition, for a nonzero-start, non-overlapping
primary MBR `PartitionKind::NullFs` partition with a valid decoded NullFS superblock. Logical/extended MBR, GPT, and superfloppy
writable grants remain disabled until their reserved disk-metadata ranges are modeled
explicitly. Read-only and writable access to the same partition are distinct endpoint
objects with distinct generations, but both use ordinary endpoint rights and are
delegated to children with `SEND`. The object's kernel-selected access mode—not a path,
partition discovery, provider registration, UID, or a different endpoint rights mask—is
the raw write authority.

Writable requests remain partition-relative and bounded to complete registered-buffer
blocks of at most 4096 bytes. The kernel stages the complete source before AHCI writes and
reports `transferred_blocks` only after all blocks succeed. A failure can still follow
physical modification of earlier blocks, so possession of the capability does not make
blind retry safe; the filesystem must provide ordering and recovery. Writable flushes
reach the AHCI cache flush.

PID 1 selects exactly one eligible NullFS partition by its configured filesystem UUID,
then starts `/nullfs-service --writable` and gives it this writable raw endpoint. Missing
and duplicate UUID matches fail without falling back to partition index or label. The service requires `READ | WRITE | FLUSH` and mounts `nullfs-core` read-write,
but that still grants no client mutation authority by itself. There are three distinct
layers:

1. the partition-scoped raw endpoint authorizes block `READ`, `WRITE`, and `FLUSH` only
   within that discovered partition;
2. a filesystem session authorizes mutations only if its exact `CONNECT` negotiation
   returned the `WRITE` feature;
3. the public VFS path is writable only if its kernel proxy and mount policy grant that
   authority.

The kernel NullFS proxy connects with exactly `WRITE` and requires the returned
`session_features::WRITE`. Its public policy permits writable/create/truncate/append open,
descriptor write, and unlink at `/Volumes/NullStar`, but not public `mkdir`, `rmdir`,
or rename. Direct flags-zero sessions remain read-only. Neither the service's raw endpoint
nor a different client's writable session manufactures public VFS authority.

## Security boundaries and limitations

Capabilities constrain which objects a process can name and which operations it may
request. They do not by themselves make a service implementation trustworthy. Every
service protocol treats peer messages, shared-memory contents, filesystem images, and
device replies as untrusted input.

Important current limitations include:

- no IOMMU-backed userspace driver isolation;
- no general capability revocation primitive; fresh generation endpoints isolate replacements but
  cannot invalidate every previously delegated old-generation handle;
- no general service-manager-owned named broker beyond the temporary PID 1 logging routes;
- no authenticated multiuser credentials or UID/GID enforcement;
- no direct mapped shared-memory pages;
- no complete sandbox or portal system;
- bounded tables and queues that can still be exhausted within their documented limits.

The current system remains suitable for controlled development workloads, not hostile
multiuser or arbitrary third-party execution.

## Migration direction

Future work should preserve the current rules while adding:

- typed MMIO, IRQ, DMA, and device-ownership capabilities;
- direct shared-memory mappings with explicit cache and protection semantics;
- cancellation, multi-object waiting, and endpoint peer-liveness notification;
- replacement of the temporary PID 1 route broker with a named, policy-backed service-manager
  broker and independent service-generation allocation;
- job-level resource accounting and limits;
- capability-aware identity, sandbox, portal, driver, network, media, and graphics
  services.

Long-term architecture is described in the
[design index](design/README.md), while the current syscall contract remains in
[Userspace ABI](syscall-abi.md).