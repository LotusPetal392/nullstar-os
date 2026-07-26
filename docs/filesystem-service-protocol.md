# Filesystem service protocol

NullStar OS is migrating from filesystem-specific kernel routing to a common
userspace filesystem-service contract. The first version of that contract is
defined in `shared/filesystem_protocol.rs`. It is a migration target: the
existing tmpfs v2 protocol remains active until tmpfs implements sessions,
node handles, and registered bulk buffers.

## Design goals

The protocol separates responsibilities that are currently combined in the
kernel VFS:

- a future VFS service resolves mount points and selects a filesystem service;
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
Clients must not need to know that the returned node belongs to a different
service; the future VFS service owns that routing and returns its own stable
vnode capability or handle.

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
4. Move path routing and open-file descriptions into a VFS service.
5. Put FAT behind the same protocol.
6. Introduce the native metadata-rich filesystem as another service.
7. Remove the kernel-resident FAT and tmpfs data paths after equivalent smoke
   and recovery coverage exists.

The current implementation completes the shared wire contract, typed request
builders, bounded service session and buffer tables, monotonic tmpfs node IDs,
root-relative lookup, node attributes, shared-memory reads and writes, identity
validation, file create/open, stable directory iteration, and unit tests. The
service accepts generic and legacy requests on the same endpoint, and the boot
probe verifies shared data visibility plus generic creation and enumeration.
The userspace `tmpfs::Mount` compatibility API now translates its bounded
write, read, stat, remove, and list calls into generic lookup, create,
attributes, unlink, shared-buffer I/O, and directory-iteration operations. It
also disconnects its persistent session explicitly. The kernel proxy remains
on the legacy protocol; the next migration step is to teach that asynchronous
proxy to maintain a generic service session and shared I/O window behind its
existing public file-descriptor ABI.

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
allows one outstanding request at a time.
