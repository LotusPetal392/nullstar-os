# Filesystem service protocol

NullStar OS is migrating from filesystem-specific kernel routing to a common
userspace filesystem-service contract defined in
`shared/filesystem_protocol.rs`. Generic sessions, opaque node handles, and
registered bulk buffers are active for tmpfs and NullFS. NullFS supports
explicitly negotiated direct writable sessions and a bounded writable public VFS
proxy. The public filesystem protocol remains `VERSION = 1`: its `Request` and
`Reply` layouts and operation set are unchanged. The public file-descriptor ABI
also remains unchanged while kernel proxies bridge the migration.

## Design goals

The protocol separates responsibilities that are currently combined in the
kernel VFS:

- the VFS service resolves mount points and selects a filesystem backend;
- filesystem services own directory lookup, node identity, metadata, and file
  contents;
- applications and the VFS refer to filesystem objects by opaque node IDs
  rather than repeatedly sending absolute paths;
- large data transfers use bounded shared-memory windows rather than inline IPC
  payloads;
- request IDs permit multiple outstanding calls, cancellation, and deterministic
  reply matching;
- session generations reject handles and replies from replaced services.

Node IDs are meaningful only within one session generation. A client must
discard every node and buffer ID after receiving `STALE_SESSION` or observing a
replacement generation.

## VFS namespace-routing protocol

The VFS service uses a separate namespace-routing protocol in
`shared/vfs_protocol.rs`; it is not the public filesystem-service protocol. Namespace
routing is now `VERSION = 2`. A request carries a canonical absolute path, and the service
returns its longest matching route. The version 2 reply remains bounded at exactly 224
bytes:

| Byte range | Size | Field | Constraint |
| --- | ---: | --- | --- |
| `0..2` | 2 | version | little-endian `u16`, exactly `2` |
| `2..4` | 2 | operation | little-endian `u16`, `RESOLVE` |
| `4..8` | 4 | request ID | little-endian `u32`, echoed |
| `8..12` | 4 | status | little-endian `i32` |
| `12..16` | 4 | route ID | preserved stable route identity |
| `16..18` | 2 | backend | preserved backend class |
| `18..20` | 2 | prefix length | bytes matched in the canonical path |
| `20..22` | 2 | backing-prefix length | used bytes in `backing_prefix` |
| `22..24` | 2 | flags | only `BINDING` is currently assigned |
| `24..32` | 8 | reserved | zero |
| `32..224` | 192 | backing prefix | backend-relative path followed by zero padding |

Version 2 therefore preserves the version 1 `route_id`, `backend`, and `prefix_length`
contract while adding explicit binding metadata. An ordinary route has flags and
backing-prefix length zero and an entirely zero backing-prefix array. A binding reply sets
exactly `BINDING`, carries a canonical length-delimited backing prefix, and zero-fills every
unused byte. The unmatched suffix begins at `prefix_length` in the canonical request path
and is appended to the backing prefix for internal backend traversal.

The implemented bindings are canonical `/System`, `/Applications`, and `/Users` to matching
paths relative to the UUID-selected NullFS provider's backend root. The VFS service owns these
binding records; the kernel validates each exact route, NullFS backend, binding flag, and
backing prefix before traversing it internally. This is not a general service-controlled
redirect. Matching paths below `/Volumes/NullStar` remain raw administrative aliases,
while cwd and open-file paths retain canonical names.

This routing change does not alter public filesystem protocol version 1, its `Request`,
`Reply`, or operations, the public file-descriptor ABI, or `NSVC` version 1.

## Session bootstrap

Endpoint messages can transfer only one capability. The legacy tmpfs protocol
uses that transfer for a fresh reply endpoint on every request, leaving no way
to attach a shared-memory object for bulk I/O.

The generic protocol instead starts with `CONNECT`:

```text
client                                  filesystem service
  | CONNECT + reply SEND capability ------------> |
  | <---- CONNECT reply on persistent endpoint    |
  |                                               |
  | ATTACH_BUFFER + shared-memory capability ---> |
  | <---- reply matched by request_id             |
  |                                               |
  | READ/WRITE using buffer_id + range ---------->|
  | <---- byte count matched by request_id        |
```

The service returns a nonzero session ID and generation. All later replies use
the persistent endpoint and must match the request ID, session ID, generation,
operation, protocol version, and reserved-field contract.

`CONNECT` also performs exact feature negotiation. Flags `0` request a read-only
session and return feature bits `0`; exactly `WRITE` requests and returns the
`WRITE` session feature. Unsupported flags or combinations are rejected rather
than downgraded. A writable service process or writable raw device therefore
does not make a client session writable: each mutating request must belong to a
session that negotiated `WRITE`.

Clients end a session with `DISCONNECT`. The service replies first, then closes
its persistent reply-endpoint handle and every shared-memory handle registered
to that session. This makes normal client lifetime bounded. Reaping sessions
whose clients terminate without disconnecting requires endpoint peer-liveness
notification and remains future kernel/IPC work.

`ATTACH_BUFFER` transfers a shared-memory capability and associates it with a
client-selected nonzero buffer ID. Read and write requests identify a checked
`buffer_id`, `offset`, and `length`. The initial implementation still uses the
kernel's bounded shared-memory copy calls; direct page mappings can replace
those copies without changing filesystem operations.

Generic tmpfs reads and writes now use these registered windows. For `READ`, the
service copies file bytes into the selected shared-memory range; for `WRITE`, it
copies bytes from the range into the file. Both operations validate the session,
node ID, buffer ID, buffer bounds, file offset, file capacity, and append flags
before accessing either object.

A successful generic `WRITE` reply returns the completed byte count in `value`
and the exact authoritative resulting file offset as eight little-endian bytes
in the inline payload. The offset is authoritative even for append, where the
service selects the current EOF rather than trusting a client-supplied offset.

Directory iteration uses fixed-size `DirectoryEntry` records in a registered
window. Each record carries a node ID, kind, component name, and continuation
cookie. Cookies are monotonic node IDs rather than tmpfs storage slots, so
deleting an earlier entry does not shift later entries or make a continuing
client skip them. A reply marks `END_OF_DIRECTORY` only when no entry remains
after the final returned cookie.

## Namespace operations

Lookup is directory-relative:

```text
root node --lookup("Users")--> directory node
directory --lookup("natalie")--> directory node
directory --lookup("notes")--> file node
```

This avoids duplicating path parsing in every filesystem and allows a VFS
service to cross mount points one component at a time. The first protocol
version defines operations for lookup, attributes, open, bulk read/write,
directory iteration, file and directory creation, truncate, unlink, `rmdir`,
rename, sync, node close, and request cancellation. Backends expose only the
operations supported by the negotiated session and mounted filesystem.

Names are single components. They may not be empty or contain `/` or NUL.
Unicode normalization and case-comparison policy belong to each mounted
filesystem and must eventually be reported as volume capabilities.

`LOOKUP` replies include the current logical size as well as the node ID and
kind. `OPEN` accepts either an existing node ID or a parent-directory node plus
a component name. The named form applies create, truncate, append, read, and
write flags in one service round trip; this lets syscall proxies preserve one
blocked operation without embedding path-resolution continuations in the
kernel.

### Writable NullFS sessions

An explicitly writable NullFS session implements `CREATE_FILE`,
`CREATE_DIRECTORY`, `WRITE`, append, `TRUNCATE`, `UNLINK`, `RMDIR`, `RENAME`,
and `SYNC`, as well as mutating `OPEN` forms. New files use mode `0644` and new
directories use `0755`. Each write is limited to 4096 bytes. The service first
copies the complete registered-buffer range into private memory, so later
client changes cannot alter bytes during the core mutation. `RENAME` carries
the source name inline and reads the destination component from a checked
registered-buffer range. All names remain single validated components.

Open-unlinked reads, writes, attributes, and truncation use the actual matching
session-owned open handle, not merely an opaque ID that once named the inode.
Unlink is rejected with `TRY_AGAIN` if a read-only session owns a matching open
and its eventual close could reclaim storage. Removing an open directory is
also rejected, and rename retains the core's cycle checks plus restrictions on
unsafe replacement of open destinations.

A mutation can fail after durable state changed or after the in-memory core
became poisoned. When the service cannot prove the result, it replies
`OUTCOME_UNKNOWN`, then fail-stops after sending that reply. Supervision starts
a replacement that remounts and runs normal recovery. `OUTCOME_UNKNOWN`, a
failed service generation, and a lost reply are never permission to retry a
mutation automatically; only an explicitly retryable status may be retried.

## Provider offlining and replacement

ABI 1.13 syscall 58 lets PID 1 offline one exact kernel filesystem-provider
generation. Its provider selectors are tmpfs `1`, NullFS `2`, and VFS `3`; the
expected generation must be a nonzero `u32`. An exact active match atomically
becomes an offline tombstone retaining that generation. Repeating the exact
tombstone is idempotent success. Unknown selectors, invalid generations, and
stale or mismatched generations return `EINVAL`; non-PID-1 callers receive
`EPERM`.

The offline transition fails and wakes exact-generation blocked filesystem work
with `EIO`, rejects stale replies and later stale work, and purges stale queued
`CLOSE_NODE` work. It never replays mutations or rebinds an old open-file
description. A replacement registration must carry a strictly newer generation
and a fresh endpoint object, not another handle to the old object whose queue
may still contain requests.

### Private NullFS lifecycle frame

Controlled NullFS replacement uses a private lifecycle frame; it does not add a
public filesystem operation or change the version 1 `Request` or `Reply`.
Every lifecycle frame is exactly 24 bytes:

| Byte range | Size | Field | Encoding and constraint |
| --- | ---: | --- | --- |
| `0..4` | 4 | magic | ASCII `NFLC` |
| `4..6` | 2 | version | little-endian `u16`, exactly `1` |
| `6..8` | 2 | kind | little-endian `u16`, one value below |
| `8..16` | 8 | service generation | little-endian nonzero `u64` |
| `16..24` | 8 | transition ID | little-endian nonzero `u64` |

Kind values are `1` `QUIESCE`, `2` `QUIESCED`, `3` `UNMOUNT`, `4`
`CLEAN_UNMOUNTED`, and `5` `FAILED`. Frames are canonical only at the exact
length and carry no capability. PID 1 queues requests on the existing FIFO
filesystem request endpoint; the service returns events on its private
supervisor endpoint. Generation and transition ID bind every event to one exact
restart attempt.

Controlled NullFS restart is asynchronous and ordered:

1. PID 1 commits restart intent without charging failure backoff or budget, then
   queues `QUIESCE` behind work already on the old generation's request endpoint.
2. The service completes those earlier requests, consumes `QUIESCE`, enters the
   quiesced state, and emits exact `QUIESCED`. It processes no later public
   filesystem operations while quiesced.
3. After validating the sender, kind, generation, transition ID, exact frame, and
   lack of an attached capability, PID 1 offlines that exact provider generation.
   The kernel wakes and fails tail work with `EIO` and preserves the tombstone.
4. PID 1 queues `UNMOUNT` with the same generation and transition ID. The service
   closes every core open handle, calls `try_unmount` to sync and publish a clean
   superblock, emits exact `CLEAN_UNMOUNTED`, and exits with status `0`.
5. PID 1 accepts the clean path only after observing both that exact event and
   final exit status `0`, in either arrival order. It then closes the old endpoint,
   creates a fresh endpoint object, and starts and registers a strictly newer
   generation before completing the restart fence.

A timeout, malformed or mismatched event, attached event capability, `FAILED`,
early exit, or nonzero exit is not durability proof. PID 1 offlines the exact
generation, forces termination with `KILL` when the child is still live, reaps it,
and replaces it through normal dirty mount recovery. Repeating an already
completed exact-generation offline is harmless. This controlled restart boundary
does not implement live filesystem `Start` or `Stop`.

## Target system namespace

The initial system namespace is:

```text
/
├── dev/
├── tmp/
├── System/
│   ├── config/
│   ├── var/
│   │   └── log/
│   ├── bin/
│   ├── services/
│   ├── drivers/
│   ├── lib/
│   └── Applications/
├── Users/
├── Applications/
└── Volumes/
```

The VFS service should present this as one rooted namespace even when different
services or volumes provide parts of it:

- `/dev` is a device namespace supplied by a device/service broker rather than
  stored as ordinary disk files;
- `/tmp` is the volatile tmpfs mount and loses its contents across service or
  system restart;
- `/System` is bound to the primary NullFS provider's backend-root `/System` node and
  contains configuration, persistent logs, core programs, service definitions, userspace
  drivers, libraries, and system applications; bootstrap services remain independent;
- `/Users` is bound to the primary NullFS provider's backend-root `/Users` node and contains
  per-user home directories and user-owned data; the integration fixture includes the
  accepted `Profile/{config,cache,state,data,logs,runtime}` layout;
- `/Applications` is bound to the primary NullFS provider's backend-root `/Applications`
  node; `/System/Applications` is now provided through the `/System` binding and is
  intended for applications delivered as part of the system;
- `/Volumes` contains the user-visible mount points for additional local,
  removable, and network filesystems. Each child name is a volume name exposed
  by the VFS, with deterministic disambiguation when multiple mounted volumes
  request the same display name.

Mount traversal must be component-based. Looking up `dev` or `tmp` beneath the root returns
a mount-root node owned by the selected service. `/System`, `/Applications`, and `/Users`
select the generation-scoped NullFS proxy and internally begin at matching backend-root
nodes. Clients do not need to know that the selected backend belongs to a different
service, and their cwd and open-file paths remain canonical. The current VFS service owns
longest-prefix routing and binding records; moving stable vnode handles and all open-file
ownership into userspace remains later work.

The root filesystem, `/dev`, and `/tmp` are boot namespace entries and do not
also appear beneath `/Volumes`. Mounting and unmounting a child of `/Volumes`
must be atomic from the perspective of lookup, invalidate stale vnode mappings,
and preserve the mounted filesystem's own stable volume and node identities.

`/System/config` and `/System/var/log` have different mutation policies despite
sharing the system volume. Configuration updates should use atomic replacement,
and log writers should receive narrowly scoped append/create authority rather
than general write access to `/System`.

## Metadata direction

`NodeAttributes` already reserves stable node identity, logical and allocated
sizes, creation/modification/change timestamps, node kind, mode, link count,
and flags. This is sufficient for an initial POSIX-like VFS while leaving room
for a Mac-like filesystem to add:

- extended attributes and Finder metadata;
- named forks;
- case-preserving, normalization-aware lookup;
- stable file IDs across rename;
- volume UUIDs and names;
- clone and snapshot operations.

Those features should be added as explicit versioned operations rather than
overloading generic flags or inline data.

## Migration sequence

1. Implement the session and node table in `/tmpfs-service`. (complete)
2. Add a compatibility adapter so existing userspace tmpfs calls use the
   generic client. (complete)
3. Teach the kernel proxy to speak the generic protocol without changing the
   public file-descriptor ABI. (complete)
4. Move path routing and open-file descriptions into a VFS service. (routing,
   the boot namespace contract, and mounted tmpfs/NullFS dispatch are active;
   broader open-file ownership migration remains)
5. Put FAT behind the same protocol.
6. Introduce NullFS, the native metadata-rich persistent filesystem, as another
   service. (read-write service mount, explicitly writable direct sessions, bounded public
   VFS create/write/truncate/append/unlink, all three primary-volume namespace bindings,
   static `/System/bin` execution, and one policy-pinned definition-backed activation pilot
   complete; general service management and broader acceptance remain)
7. Remove the kernel-resident FAT and tmpfs data paths after equivalent smoke
   and recovery coverage exists.

NullFS host development proceeds independently of service integration. The
workspace contains the shared version 1.2 format and writable core, checked
memory and host-file block devices, deterministic crash/recovery tests, and
formatter, image, inspector, checker, and Linux FUSE tools. The frozen layout
and staged path to a NullStar backend service are in the
[NullFS format](filesystems/nullfs-format.md) and
[NullFS roadmap](filesystems/nullfs-roadmap.md).

The current implementation completes the shared wire contract, typed request
builders, bounded service session, buffer, and open-node reference tables,
monotonic tmpfs node IDs, root-relative lookup, node attributes, shared-memory
reads and writes, identity validation, file create/open/close, stable directory
iteration, and unit tests. The
service accepts generic and legacy requests on the same endpoint, and the boot
probe verifies shared data visibility plus generic creation and enumeration.
The userspace `tmpfs::Mount` compatibility API now translates its bounded
write, read, stat, remove, and list calls into generic lookup, create,
attributes, unlink, shared-buffer I/O, and directory-iteration operations. It
also disconnects its persistent session explicitly.

The kernel proxy registration path now starts that migration: it queues
`CONNECT` without blocking PID 1, validates the reply from the normal kernel
poll loop, retains the persistent session reply endpoint, creates a kernel-owned
4 KiB shared-memory window, and registers it with `ATTACH_BUFFER`. Replacement
no longer relies on registration to tear down the old proxy: PID 1 first
offlines the exact old generation, and registration then requires a strictly
newer generation and a fresh service endpoint object before establishing the
new session.

Kernel `/tmp` open, stat, read, write, and directory iteration now use the
generic protocol. Open
stores the returned node ID in the kernel open-file description; later I/O
addresses that node directly and moves bytes through the registered 4 KiB
window. The proxy preserves append behavior, rejects nodes from replaced
service generations, validates completed byte counts, and translates results
back into the unchanged file-descriptor syscall ABI. Directory iteration
validates fixed records, monotonic cookies, names, and end-of-directory state
before producing the existing syscall records. The obsolete per-request legacy
kernel transport has been removed. The initial kernel generic path deliberately
allows one outstanding request at a time. Final node releases use that same
serialized channel through a bounded kernel cleanup queue; interrupted requests
remain owned until their late reply is drained so they cannot be mistaken for a
later close reply.

The first part of migration step 4 is now present as a separately supervised
`/vfs-service`. Its version 2 protocol accepts canonical absolute paths and returns the
longest matching namespace prefix, a stable route ID, a backend class, and canonical
binding metadata in a bounded 224-byte reply. PID 1 starts and monitors the service
independently of tmpfs, and `/vfs-probe` verifies the declared namespace, binding
canonicality, `/tmp` backend selection, and bootstrap availability during normal boot.
PID 1 now registers each VFS service generation
with the kernel, which retains the endpoint and asynchronously validates a
versioned root-route handshake. `stat` is the first syscall to use
per-operation routing: the kernel blocks on a generation-bound route reply,
then completes against the boot filesystem or chains directly into the tmpfs
proxy. Both metadata output and saved-register publication occur while the
caller's address space is temporarily active. The boot probe validates public
`stat`, `read_directory`, `chdir`, `open`, and `unlink` calls across the routed
backends and every declared namespace directory. `open` resolves ownership
before allocating backend state, and `unlink` resolves ownership before
mutation: boot files retain kernel compatibility descriptors, while `/tmp`
operations chain directly into the generic filesystem service without waking
the caller between stages. Boot-FAT deletion remains unsupported until that
filesystem moves behind the protocol. Remaining exact namespace prefixes return synthetic
directory metadata and stable paginated listings and can become a process's working
directory. The root
listing merges boot-filesystem entries with `/dev`, `/tmp`, `/System`, `/Users`,
`/Applications`, and `/Volumes`, suppressing backing-store name collisions.
Unresolved descendants remain absent until a filesystem or service is mounted there. The
VFS-owned `/System`, `/Applications`, and `/Users` bindings select NullFS and return matching
backend-root paths as zero-padded backing prefixes. The kernel checks those exact known
targets before dispatch and appends canonical suffixes only for internal traversal. Route
replies for every other entry carry no binding metadata and are checked against the
kernel's expected longest-prefix result before backend dispatch.

Tmpfs separates namespace linkage from node lifetime: unlink removes the name
immediately while existing open node-ID descriptions remain readable and
writable. `LOOKUP` and `CREATE_FILE` return identity without acquiring an open
reference; each successful `OPEN` records exactly one session-owned reference.
`CLOSE_NODE` releases one such reference, and `DISCONNECT` releases every
reference still owned by that session. An unlinked node is reclaimed only after
its global open count reaches zero, and reclaimed storage slots receive a new
monotonic node ID when reused.

`nullfs-service` implements the session and node-reference contract around
shared-core `OpenHandle`s. PID 1 launches it explicitly with `--writable` and
delegates a send-only handle to the partition-scoped raw NullFS endpoint. The
service requires block metadata advertising `READ | WRITE | FLUSH`, mounts the
core read-write, and announces readiness only after mount-time journal recovery,
orphan reclamation, volume validation, and dirty publication complete.

The service presents generation-tagged opaque node IDs rather than inode numbers,
sizes its identity map from the mounted volume's inode capacity, drains duplicate
opens one reference at a time, and preserves both protocol and core accounting
when a close fails. PID 1 registers it independently of tmpfs as a
generation-scoped kernel filesystem proxy. The VFS has a static longest-prefix
route for `/Volumes/NullStar`, and its `/Volumes` listing exposes that mount.

The NullFS proxy performs `CONNECT` with exactly `WRITE` for each registered
service generation, requires `session_features::WRITE` in the canonical reply,
and attaches one kernel-owned 4 KiB shared-memory buffer. Direct clients that
connect with flags `0` still receive read-only sessions; raw block authority,
filesystem-session authority, and public VFS policy remain separate.

Without adding a NullFS-specific application ABI, `/Volumes/NullStar` and the bound
`/System`, `/Applications`, and `/Users` views support ordinary public `stat`, read, open, `fstat`,
seek, `read_directory`, and `chdir`. Writable, create, truncate, and append opens,
descriptor `write`, and unlink remain available outside the System backing subtree;
canonical and raw public System paths return `READ_ONLY` for mutation. Canonical path
resolution happens before VFS routing, so cwd-relative operations continue to reach the
selected backend while retaining logical paths. Raw matching paths below
`/Volumes/NullStar` remain administrative views of the same nodes. Public `mkdir`, `rmdir`,
and rename remain future work.

Before copying a public write's source bytes, the proxy reserves its sole
outstanding request; it then stages at most 4 KiB in the registered window. A
successful reply must contain the byte count in `value` and the exact resulting
offset as eight little-endian inline bytes. This lets append use the service's
selected EOF. Malformed replies, `OUTCOME_UNKNOWN`, and any post-send mutation
uncertainty map to `IO`, quarantine the generation, and are never automatically
retried.

The kernel maps successful `OPEN` references to open-file descriptions rather
than descriptor numbers. `dup`, `dup2`, fork inheritance, and file-backed
standard streams share the same generation-, session-, node-, and size-bound
state, so append, truncate, cross-handle `fstat`/`SEEK_END`, and open-unlinked
access remain coherent and one alias cannot close the service node early. Final
destruction queues one generation- and session-bound `CLOSE_NODE`.

When PID 1 offlines the exact old generation, the kernel preserves its
tombstone, releases kernel-owned handshake, session-reply, and bulk-buffer
state, and fails and wakes old in-flight requests with `IO` (`EIO` at the
syscall boundary). It rejects stale replies and work and discards old-generation
close tickets. Replacement registration cannot clear those protections unless
it supplies both a strictly newer generation and a fresh endpoint object; old
descriptors remain stale and mutations are never replayed or rebound.

The direct service probe preserves read-only-session denial and exercises the
service's broader writable protocol surface, including directory creation,
`rmdir`, and rename. Public probes separately cover canonical `/Applications` and `/Users`
mutation, cross-view visibility and identity, canonical cwd behavior, create, write,
independent stale append, cross-handle `fstat` and `SEEK_END`, truncate, duplication,
unlink while open, open-unlinked read/write, cleanup, persistence across service restart,
stale old descriptors, and continued bootstrap availability. A dedicated fully allocated image
also proves exact public-ABI `NO_SPACE` for existing-file growth and new-inode creation, unchanged
existing metadata, continued reads, resource reclamation, and successful mutation afterward.
