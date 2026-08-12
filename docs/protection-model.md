# Capability and IPC protection model

NullStar OS is evolving incrementally toward a capability-oriented, service-based
architecture. This document records the implemented protection primitives and explains
how later filesystem-service work now uses them. It is not a claim that the system is
ready for hostile workloads.

## Current status

The original protection phase introduced bounded process-local capability tables,
rights-reduced duplication and delegation, endpoints, counted notifications, shared
byte-memory objects, scheduler-integrated endpoint waiting, and direct-child bootstrap
grants. ABI 1.15 adds the first capability-backed job object with non-relaxable `fork`
inheritance, independent process-exit records, and bounded whole-job termination. ABI 1.16 adds
immutable child-job creation plus subtree inspection, exit drainage, and termination while reverse
parent reachability prevents hierarchy relaxation through handle closure. ABI 1.17 adds a
tightening-only live-process ceiling inherited by child jobs and enforced against every ancestor's
complete subtree. ABI 1.18 permits only an empty child leaf to retire: retirement permanently makes
the object inert, detaches its parent edge, and allows reclamation after final handle closure. ABI
1.19 lets `WAIT` authority inspect a job's local configured process ceiling without permitting
relaxation.

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
- PID 1 exposes separate stable service-observation and mutation endpoints to authorized `sv`
  clients and trusted shell builtins; logging supports live start/stop while filesystem mutations
  remain restricted;
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
6. A process assigned to a job cannot escape by forking or being moved into a different
   job.

The capability namespace is separate from the file-descriptor namespace. Both use small
integers today, but handles are valid only for capability operations and descriptors are
valid only for descriptor and filesystem I/O.

## Implemented kernel objects

| Object | Purpose | Principal rights |
| --- | --- | --- |
| Endpoint | Bounded FIFO messages and optional capability delegation | `SEND`, `RECEIVE`, `DUPLICATE`, `TRANSFER` |
| Notification | Counted asynchronous event delivery | `SIGNAL`, `WAIT`, `DUPLICATE`, `TRANSFER` |
| Shared memory | Bounded byte storage shared by capability holders | `READ`, `WRITE`, `DUPLICATE`, `TRANSFER` |
| Job | Flat descendant containment and FIFO process-exit observation | `MANAGE`, `WAIT`, `SIGNAL`, `DUPLICATE`, `TRANSFER` |

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

## Basic job containment and exit observation

`JOB_CREATE` returns a new empty job. A handle with `MANAGE` may assign only one live
direct child that has no prior job. Membership is non-relaxable: a later attempt to move
that process is denied, and all of its `fork` descendants inherit the same job. Parent
exit and reparenting to the kernel reaper do not change membership.

Every member contributes one immutable terminal record to the job in addition to the
ordinary parent completion. A `WAIT` handle consumes those records FIFO without racing or
stealing the parent's `wait_child` result. The combined live-member and unconsumed-record
count is bounded at 64, so the kernel rejects new membership rather than dropping exit
information. Capability inspection reports the active member count.

A `SIGNAL` handle may force signal 9 across the current member snapshot. The object is
kept alive by kernel roots while it contains members, even if its controller exits or
closes every userspace handle. This provides explicit bounded cleanup, not implicit
kill-on-close. PID 1 uses fresh jobs for policy-pinned definition-backed service attempts
and every logging, NullFS, tmpfs, and VFS generation, assigns leaders before launch-barrier release,
retains only `SIGNAL | WAIT`, and drains all completion records to `ECHILD` before replacement.
Logging retains cooperative process-group termination but escalates and removes escaped
descendants through the job. The logging-lifecycle QEMU gate also proves that escaped
process-group descendants of tmpfs and VFS generations are terminated, each whole job is
drained, and a replacement generation starts. NullFS restart, crash-recovery, and provider-loss
gates likewise inject escaped descendants and require complete drainage without weakening clean
quiesce and unmount proof. Shared PID 1 cleanup classifies process-group
signaling, direct-leader waiting, job termination and drainage, capability closure, barrier
release, and cooperative yields without allocation. Normal progress remains quiet; unexpected success values, exact errno
codes, and missing handles emit canonical bootstrap diagnostics when observed. Final budget
exhaustion emits at most once per cleanup episode. A representative record is
`init: cleanup service=logging phase=job-drain operation=job-try-wait result=error code=9`.
An `Ok(0)` process-group signal is an invariant violation, while `JOB_TERMINATE` returning
zero is valid and still requires drainage to `ECHILD`. NullFS completes exact quiesce and
clean-unmount durability proof before its clean generation job is terminated and drained;
timeout, invalid lifecycle traffic, crash, and provider-loss paths terminate and drain the
whole job before dirty recovery. Jobs support immutable hierarchy and process-count policy but do
not yet carry CPU or memory limits; current PID 1 service generations remain flat roots.

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
or queued transfer refers to it.

PID 1 owns separate allocation-free monotonic provider-generation sequences for logging, NullFS,
tmpfs, and VFS, and every startup attempt consumes a value independently of process IDs. It sends one
exact 16-byte `NSGN` v1 record with no capability over a private endpoint granted to the service with
exact `RECEIVE` rights. Each service accepts only the canonical record from kernel-stamped sender PID
1 on that exact-rights handle and closes the handle after the one receive attempt. NullFS and tmpfs
bind filesystem sessions to the value, PID 1 registers matching generations with the kernel proxies,
and logging also uses it for its collector, `NSLS`, NSWP, and route publications. The current
contract provides no durable cross-boot sequence persistence.

The distinction has a resource cost. The kernel currently permits 32 live endpoint objects
system-wide. Every in-progress route resolution creates a private reply endpoint, every provider
generation creates fresh ingress objects, and retained old handles can delay object collection.
Resolution or publication can therefore fail under endpoint pressure even if a route-table slot is
available. Fixed route tables add a separate bound: withdrawn keys leave generation tombstones that
continue to consume distinct-key capacity.

The route broker never queues or replays application traffic. A one-way logging `Emit` is not
replayed on a replacement when processing by the old provider is uncertain; generation isolation
cannot determine whether that record was retained before failure.

## Service-control observation and mutation use

The [service control protocol](service-control-protocol.md) uses distinct stable endpoint objects for
observation and mutation. PID 1 retains each source and an exact-`RECEIVE` duplicate. An
authorized client holds exact `SEND`, creates a fresh private reply endpoint per request, transfers
only exact `SEND` for that reply, and retains exact `RECEIVE`. The 64-byte `NSVC` response transfers
no capability and is accepted only when it is canonically correlated and has a nonzero kernel-stamped
server PID; the reusable `sv` client additionally pins that PID to PID 1.

The trusted recovery shell receives separate `SEND | DUPLICATE` observation and mutation authorities
but not `TRANSFER`. It attenuates each local duplicate to exact `SEND`; arbitrary child processes do
not inherit either grant. The standalone `/sv` pathname is not trusted by itself and works only when
an authorized launcher installs authority at its expected handle. Service IDs, list cursors, request
IDs, generations, PIDs, and UID-like identity do not manufacture access.

The observation client refuses to originate mutation packets, and PID 1 returns canonical
`AccessDenied` if a valid `Start`, `Stop`, or `Restart` reaches that ingress. The mutation client
refuses observation operations. Its endpoint implements generic `Restart` plus logging `Start` and
`Stop`; filesystem `Start` and `Stop` return `Unsupported`. Controlled restart and stop do not charge
failure policy. PID 1 first requests cooperative logging termination and, after a bounded grace
period, uses uncatchable and unblockable signal 9 through the existing direct-child `kill` syscall;
this escalation authority is not delegated merely by granting service-control mutation access. An
unconfirmed sent mutation is outcome unknown and never retried automatically. Malformed requests are
consumed without killing PID 1, and every terminal or failed path closes private reply handles. Logging stopped state persists only
in PID 1 memory; there is no cross-reboot policy, separate manager process, partially visible registry
policy, or general revocation primitive yet.

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
- no general service-manager-owned named broker beyond the temporary PID 1 logging routes and
  service-control endpoints;
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
- replacement of the temporary PID 1 route and generation owner with a named, policy-backed,
  restartable service-manager broker that owns the sequence and receives its current state;
- broader job-level resource accounting and limits plus service, session, and application
  hierarchy integration;
- capability-aware identity, sandbox, portal, driver, network, media, and graphics
  services.

Long-term architecture is described in the
[design index](design/README.md), while the current syscall contract remains in
[Userspace ABI](syscall-abi.md).
