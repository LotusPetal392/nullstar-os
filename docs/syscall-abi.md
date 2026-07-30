# NullStar OS userspace ABI

NullStar OS exposes a small Rust-oriented ring-3 ABI through software interrupt
`0x80`. The shared numeric and structure definitions live in
`shared/userspace_abi.rs`; the direct-child capability bootstrap extension lives
in `shared/protection_abi.rs`. Kernel and userspace include these files directly
so they cannot silently disagree about call numbers or layouts.

The ABI is experimental, but callers can query the current version, 1.9, and a
documented capability mask before relying on optional platform services.

## Calling convention

The syscall number is placed in `rax`. Arguments use, in order:

```text
rdi, rsi, rdx, r10, r8, r9, rbx
```

A non-negative `rax` value is success. Negative values are negated `errno`
numbers. Calls that copy a structure to userspace take both an address and a
byte length. Supplying a shorter buffer returns `ERANGE`, allowing structures to
grow in later ABI revisions without writing past an older caller's allocation.

Userspace should call the typed wrappers in `userspace::syscall`,
`userspace::platform`, and `userspace::ipc` rather than issuing raw interrupts.

## Version 1.1 platform calls

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 23 | `system_info` | output address, output bytes | fills `SystemInfo` |
| 24 | `stat` | path address, path bytes, output address, output bytes | fills `file::Stat` |
| 25 | `fstat` | descriptor, output address, output bytes | fills `file::Stat` |
| 26 | `read_directory` | path address, path bytes, starting index, record address, capacity | number of records |
| 27 | `chdir` | path address, path bytes | zero |
| 28 | `getcwd` | buffer address, capacity | path bytes excluding trailing NUL |
| 29 | `dup` | source descriptor | new descriptor |
| 30 | `dup2` | source descriptor, target descriptor | target descriptor |
| 31 | `getppid` | none | parent PID, or zero for PID 1 |
| 32 | `kill` | target PID, signal | zero |
| 33 | `get_process_group` | target PID | process-group ID |
| 34 | `set_process_group` | target PID, process-group ID | resulting process-group ID |

`SystemInfo.capabilities` advertises the calls above. Version 1.1 reports
4 KiB pages, the process descriptor bound, the path bound, the maximum number
of directory records accepted by one call, and process-group control support.

## Version 1.2 protection calls

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 35 | `capability_duplicate` | source handle, reduced rights | new handle |
| 36 | `capability_close` | handle | zero |
| 37 | `capability_info` | handle, output address, output bytes | fills `capability::Info` |
| 38 | `endpoint_create` | none | endpoint handle |
| 39 | `endpoint_send` | endpoint, byte address, byte count, transfer handle, transfer rights | zero |
| 40 | `endpoint_receive` | endpoint, buffer address, capacity, message-info address | received byte count |
| 41 | `notification_create` | none | notification handle |
| 42 | `notification_signal` | handle, nonzero amount | pending count |
| 43 | `notification_try_wait` | handle | pending count after consuming one |
| 44 | `shared_memory_create` | byte length | shared-memory handle |
| 45 | `shared_memory_read` | handle, offset, buffer address, byte count | copied byte count |
| 46 | `shared_memory_write` | handle, offset, byte address, byte count | copied byte count |
| 47 | `endpoint_wait` | endpoint handle | zero once the endpoint is readable |
| 48 | `capability_grant_child` | child PID, source handle, reduced rights, requested child handle | child handle |

`SystemInfo.capabilities` reports `capability::PROTECTION_V1` when the handle
table, endpoints, notifications, and shared-memory objects are available.

Capability handles occupy a namespace separate from file descriptors. Handles
are process local, begin at one, and refer to an object plus an explicit rights
mask. Duplication and delegation require the corresponding authority and accept
only a nonempty subset of the source rights.

Endpoint send and receive are non-blocking data-movement calls. A full send
queue or an empty receive queue returns `EAGAIN`. Receiving into a buffer smaller
than the front message returns `ERANGE` without consuming the message. One
message may carry one rights-reduced capability. The `userspace::ipc::receive`
helper yields and retries on `EAGAIN`. `endpoint_wait` is the scheduler-integrated
readiness wait for code that wants to block until an endpoint receives a message.

Notifications are counted. Signaling checks for overflow; try-wait consumes one
pending event or returns `EAGAIN`. Shared-memory calls currently copy bytes
between userspace and bounded kernel storage rather than creating direct virtual
memory mappings.

`capability_grant_child` is a narrow bootstrap operation. The target must be a
live direct child, the source handle must carry `TRANSFER`, and the granted
rights must be a subset of the source rights. A requested child handle of zero
allocates the lowest free slot; a nonzero value requests that exact slot. This
allows recently forked processes to agree on a bootstrap endpoint without a
global service namespace. Capability tables are not implicitly cloned by
`fork`, but they remain attached to a process across `exec`.

See [Capability and IPC protection model](protection-model.md) for lifetime,
security-boundary, testing, and migration details.

## Version 1.4 tmpfs registration

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 49 | `register_tmpfs_service` | request endpoint handle, generation | zero |

Only PID 1 may register the supervised tmpfs service. The handle must refer to
an endpoint with `SEND` authority, and the generation must be nonzero. After
registration, ordinary `/tmp/<name>` file syscalls are proxied to the registered
service endpoint. The kernel creates a private reply endpoint per syscall,
blocks the caller, and completes the saved syscall frame when the service
replies. `/tmp` itself remains a directory mount point.

The reply endpoint is an untrusted ABI boundary. The kernel rejects replies with
the wrong fixed-record size, protocol version, operation, mount generation, or
data bound; nonzero reserved fields and attached capabilities are also rejected.
These checks prevent stale service instances and malformed replies from
completing a blocked filesystem syscall.

## Paths and working directories

Every process starts in `/` unless its inherited environment contains the
kernel-managed `PWD` entry. `chdir` validates that the target exists and is a
directory, then updates that entry. Fork, spawn, and exec preserve the process
environment, so they also preserve the working directory.

The `stat`, `read_directory`, `chdir`, and legacy `open` calls accept absolute
paths or paths relative to the calling process's working directory. The shared
path resolver canonicalizes `.`, `..`, and repeated separators before VFS
lookup. This also makes shell redirection targets relative to the shell's
current directory.

`spawn_command` and `execve` use two executable-name modes:

- names without a slash, such as `cat`, retain the root command namespace and
  resolve to `/cat`;
- explicit relative paths containing a slash, such as `./cat`, `../cat`, or
  `tools/cat`, resolve against the calling process's working directory and are
  canonicalized before image lookup.

Absolute executable paths retain their existing behavior. This milestone does
not add a configurable `PATH` search list.

`PWD` is reserved. The ordinary environment mutation syscalls reject attempts
to set or unset it, preventing a process from claiming a directory that the VFS
did not validate. Because working-directory state uses the bounded process
environment, `PWD` counts toward the environment entry and byte limits.

## Metadata and directory records

`file::Stat` contains:

```text
kind, size, flags
```

Kinds currently identify regular files, directories, terminals, and pipes.
Flags currently identify read-only, hidden, and system nodes.

`read_directory` is index based and bounded. A caller supplies a starting entry
index and an array of fixed-size `file::DirectoryEntry` records. Each record
contains kind, size, flags, a byte length, and a 256-byte name buffer. At most
32 records may be requested in one call. A return value smaller than the
provided capacity indicates the end of the directory.

Directory contents may change between calls; the index is a bounded pagination
mechanism, not a persistent cursor or snapshot.

## Descriptor duplication

`dup` allocates the lowest available non-standard descriptor. `dup2` installs
the source at an explicit descriptor, closing an existing target first.

Duplicated regular-file descriptors share the underlying open-file state,
including the current offset and append mode. Pipe duplication retains the
appropriate reader or writer endpoint. Duplicating a descriptor onto standard
input, output, or error validates that its access direction is compatible.

The default terminal endpoints can be copied between standard descriptors, but
cannot yet be represented as an ordinary descriptor numbered 3 or higher.
`dup` on an unredirected terminal therefore returns `ENOSYS`.

## Process groups and generic launch

A target PID of zero means the calling process for both process-group calls. A
process-group ID of zero in `set_process_group` means the target process's PID.
A process may inspect or move itself, or a parent may inspect or move one of its
direct children. Other targets return `EPERM`.

A process may create a group whose ID is its own PID. Joining another group is
allowed only when a live process with the same parent already belongs to that
group. A process that owns or has been assigned the terminal cannot be moved to
a different group. Repeating an already-completed group assignment succeeds.

The public `userspace::syscall::spawn_command` facade constructs ordinary
launches from `fork`, descriptor duplication, `set_process_group`, optional
foreground transfer, and `execve`. A child waits until the parent completes its
group assignment before claiming the terminal or replacing its image.
Foreground transfer is idempotent so either side of the parent/child race may
complete it.

Pipelines add a close-on-exec pipe barrier before the first stage is forked.
Each child closes its inherited writer, installs standard-stream descriptors,
and blocks on the barrier reader. After every stage has joined the process
group, the shell establishes foreground ownership when needed, closes its data
pipe endpoints, and closes the barrier writer. The resulting end-of-file event
releases all stages together; `execve` then closes every unrelated inherited
endpoint.

The typed facade rejects unsupported flag, descriptor, and process-group
combinations with `EINVAL`; joined-group launches require the barrier form.
The `/exec` helper now follows the same generic path as other bundled programs.
No bundled userspace code calls syscall 7's legacy atomic spawn operation; the
kernel retains that entry point only for version-1 ABI compatibility.

## Direct signals

`kill` uses the focused NullStar OS signal set. A process may target one of its
direct children. Other targets return `EPERM`; missing or completed targets
return `ESRCH`. Process-group signaling and shell job control continue to use
the existing group-oriented syscall.

## Version 1.6 pathname mutation

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 51 | `unlink` | path address, path bytes | zero |

`capability::PATH_MUTATION` advertises this operation. The initial
service-backed implementation removes flat files beneath `/tmp`; namespace
directory nodes return `EISDIR`, and boot-FAT deletion remains unavailable
until FAT is behind the generic filesystem protocol.

## Version 1.7 read-only block-device endpoints

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 52 | `open_block_device_endpoint` | partition-table index | capability handle |

Version 1.7 introduced PID-1 acquisition of discovered filesystem-candidate
partitions in read-only mode. Each successful call returns an endpoint handle
with `SEND | TRANSFER`; PID 1 may delegate a reduced send-only handle to a
supervised filesystem service. The endpoint implements the versioned protocol
in [`block-device-service-protocol.md`](block-device-service-protocol.md), and
the `READ_ONLY_BLOCK_DEVICE_ENDPOINTS` capability bit advertises availability.
Other callers receive `EPERM`, and invalid or unavailable partitions are
rejected.

## Version 1.9 writable NullFS block-device acquisition

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 54 | `open_writable_block_device_endpoint` | partition-table index | capability handle |

Syscall 52 remains unconditionally read-only so older callers cannot accidentally
acquire more authority through an unspecified register. Only PID 1 may call the
new writable operation, and it currently succeeds only when the disk has no
extended partition and the selected entry is a nonzero-start primary MBR
`PartitionKind::NullFs` partition that does not overlap another discovered partition and contains a valid decoded NullFS superblock. Logical/extended MBR,
GPT, and superfloppy writable grants remain disabled until their reserved
disk-metadata ranges are modeled explicitly.

Read-only and writable access to the same partition are distinct endpoint objects
with distinct generations, although both are returned with ordinary endpoint
`SEND | TRANSFER` rights and are normally delegated with `SEND` only. Endpoint
object identity, rather than path, discovery, UID, or an amplified rights mask,
carries the write authority.

Read-only `INFO` replies advertise `READ` and the `READ_ONLY` device flag.
Writable `INFO` replies advertise `READ | WRITE | FLUSH` without `READ_ONLY`.
Read-only `WRITE` remains `READ_ONLY`, and read-only `FLUSH` remains
`NOT_SUPPORTED`; writable `FLUSH` reaches the AHCI cache-flush operation. The
`WRITABLE_NULLFS_BLOCK_DEVICE_ENDPOINTS` system capability bit advertises this
extension. It grants raw partition authority only and does not enable writable
filesystem-service or VFS syscalls.

## Compatibility rules

- Existing syscall numbers 1 through 34 are unchanged.
- ABI 1.2 adds protection calls 35 through 48 and capability feature bits.
- ABI 1.4 adds PID-1 tmpfs service registration at syscall 49.
- ABI 1.5 adds PID-1 VFS routing service registration at syscall 50. The
  kernel retains the send endpoint and completes a versioned root-route
  handshake asynchronously before treating the service as ready.
- ABI 1.6 adds VFS-routed pathname deletion at syscall 51.
- ABI 1.7 adds PID-1 read-only partition endpoint acquisition at syscall 52.
- ABI 1.9 adds the narrowly scoped writable NullFS partition endpoint at syscall
  54 while preserving syscall 52 as unconditionally read-only.
- New structures use `#[repr(C)]` and fixed-width integer fields.
- Unknown calls return `ENOSYS`.
- Resource bounds remain part of normal failure behavior; protection bounds are
  fixed in the shared ABI definitions.
- ABI changes that alter an existing structure or semantic contract must bump
  the reported version and update this document, the shared definitions, and
  the runtime probe together.
