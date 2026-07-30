# Filesystem service protocol

NullStar OS is migrating from filesystem-specific kernel routing to a common
userspace filesystem-service contract defined in
`shared/filesystem_protocol.rs`. Generic sessions, opaque node handles, and
registered bulk buffers are active for tmpfs and the read-only NullFS service;
the public file-descriptor ABI remains unchanged while kernel proxies bridge
the migration.

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
before accessing either object. Replies return the completed byte count.

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
version reserves operations for lookup, attributes, open, bulk read/write,
directory iteration, file and directory creation, unlink, rename, node close,
and request cancellation.

Names are single components. They may not be empty or contain `/` or NUL.
Unicode normalization and case-comparison policy belong to each mounted
filesystem and must eventually be reported as volume capabilities.

`LOOKUP` replies include the current logical size as well as the node ID and
kind. `OPEN` accepts either an existing node ID or a parent-directory node plus
a component name. The named form applies create, truncate, append, read, and
write flags in one service round trip; this lets syscall proxies preserve one
blocked operation without embedding path-resolution continuations in the
kernel.

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
- `/System` belongs to the system volume and contains configuration, persistent
  logs, core programs, service executables, userspace drivers, libraries, and
  system applications;
- `/Users` contains per-user home directories and user-owned data;
- `/Applications` contains applications installed for all users, while
  `/System/Applications` contains applications delivered as part of the system;
- `/Volumes` contains the user-visible mount points for additional local,
  removable, and network filesystems. Each child name is a volume name exposed
  by the VFS, with deterministic disambiguation when multiple mounted volumes
  request the same display name.

Mount traversal must be component-based. Looking up `dev` or `tmp` beneath the
root returns a mount-root node owned by the selected service, while lookup of
`System`, `Users`, or `Applications` continues on the native root filesystem.
Clients do not need to know that the selected backend belongs to a different
service. The current VFS service owns longest-prefix routing and the kernel
chains the operation into the selected generation-scoped proxy; moving stable
vnode handles and all open-file ownership into userspace remains later work.

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
   service. (read-only service and static VFS mount complete; raw writable block
   authority implemented; writable filesystem operations remain next)
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
4 KiB shared-memory window, and registers it with `ATTACH_BUFFER`. Service
replacement releases the previous handshake endpoint, session endpoint, and
bulk-window root before establishing the new generation.

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
`/vfs-service`. Its versioned protocol accepts canonical absolute paths and
returns the longest matching namespace prefix, a stable route ID, and a backend
class. PID 1 starts and monitors the service independently of tmpfs, and
`/vfs-probe` verifies the complete target namespace plus `/tmp` backend
selection during normal boot. PID 1 now registers each VFS service generation
with the kernel, which retains the endpoint and asynchronously validates a
versioned root-route handshake. `stat` is the first syscall to use
per-operation routing: the kernel blocks on a generation-bound route reply,
then completes against the boot filesystem or chains directly into the tmpfs
proxy. Both metadata output and saved-register publication occur while the
caller's address space is temporarily active. The boot probe validates public
`stat`, `read_directory`, `chdir`, `open`, and `unlink` calls across both
backends and every declared namespace directory. `open` resolves ownership
before allocating backend state, and `unlink` resolves ownership before
mutation: boot files retain kernel compatibility descriptors, while `/tmp`
operations chain directly into the generic filesystem service without waking
the caller between stages. Boot-FAT deletion remains unsupported until that
filesystem moves behind the protocol. Exact VFS-owned route prefixes,
including the intermediate `/System/var` node, return synthetic directory
metadata and stable paginated listings and can become a process's working
directory. The root
listing merges boot-filesystem entries with `/dev`, `/tmp`, `/System`, `/Users`,
`/Applications`, and `/Volumes`, suppressing backing-store name collisions.
Unresolved descendants remain absent until a filesystem or service is mounted
there. Route replies are checked against the kernel's expected longest-prefix
result before backend dispatch.

Tmpfs separates namespace linkage from node lifetime: unlink removes the name
immediately while existing open node-ID descriptions remain readable and
writable. `LOOKUP` and `CREATE_FILE` return identity without acquiring an open
reference; each successful `OPEN` records exactly one session-owned reference.
`CLOSE_NODE` releases one such reference, and `DISCONNECT` releases every
reference still owned by that session. An unlinked node is reclaimed only after
its global open count reaches zero, and reclaimed storage slots receive a new
monotonic node ID when reused.

The read-only `nullfs-service` implements the same session and node-reference
contract around shared-core `OpenHandle`s. PID 1 delegates a send-only handle to
the writable raw NullFS block endpoint, and the service requires block metadata
advertising `READ | WRITE | FLUSH`. It nevertheless wraps the userspace adapter
in `ReadOnlyBlockDevice` before mounting the core. Raw block authority therefore
does not make any generic filesystem operation writable.

The service presents generation-tagged opaque node IDs rather than inode numbers,
sizes its identity map from the mounted volume's inode capacity, drains duplicate
opens one reference at a time, and preserves both protocol and core accounting
when a close fails. PID 1 registers it independently of tmpfs as a
generation-scoped kernel filesystem proxy. The VFS has a static longest-prefix
route for `/Volumes/NULLSTAR_DATA`, and its `/Volumes` listing exposes that mount.

The NullFS proxy performs `CONNECT` for each registered service generation and
attaches one kernel-owned 4 KiB shared-memory buffer. It translates ordinary
`stat`, read-only `open`, `read`, `fstat`, `seek`, `read_directory`, and `chdir`
behavior without adding a NullFS-specific application ABI. Canonical path
resolution happens before VFS routing, so after `chdir` enters the mounted
volume, relative directory changes and opens are routed back to NullFS. Open
requests carrying write, create, truncate, or append intent, descriptor writes,
and unlink are rejected with the public read-only error. This milestone does not enable
writable filesystem syscalls; implementing those operations with recovery-safe failure
semantics remains the next NullFS service step.

The kernel maps successful `OPEN` references to open-file descriptions rather
than descriptor numbers. `dup`, `dup2`, fork inheritance, and file-backed
standard streams share the same description, so closing one alias cannot close
the service node early. The description's final destruction—through explicit
close, close-on-exec, or process reap—queues one generation- and session-bound
`CLOSE_NODE`. The serialized request slot remains owned through reply payload
validation and shared-buffer consumption, and multi-stage lookup or directory
operations replace the active request ID atomically before sending their next
stage. Accepted close requests are not blindly replayed because a lost or failed
reply cannot prove whether the service already decremented the reference;
explicit `TRY_AGAIN` is the only retryable close status, while failed or
malformed replies leave the proxy fail-stopped until service replacement rather
than risking a leaked or duplicate close.

When PID 1 supervises a replacement, it registers a higher proxy generation.
The kernel releases the previous endpoint, session reply endpoint, and bulk
buffer, fails old in-flight requests with I/O, and refuses to rebind existing
open-file descriptions; subsequent NullFS I/O through those old descriptors
therefore fails with I/O. Close tickets carry the old proxy generation, session
ID, and session generation and are discarded rather than sent to the
replacement. The direct service probe covers protocol lookup, reads, directory
cookies, mutation denial, and close/disconnect accounting. The normal-boot VFS
probe covers the mounted ordinary syscall path, including cwd-relative routing,
but does not yet deliberately restart `nullfs-service` with live descriptors;
that restart fault-injection coverage remains future work.
