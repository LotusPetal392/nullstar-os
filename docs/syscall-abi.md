# NullStar OS userspace ABI

NullStar OS exposes a small Rust-oriented ring-3 ABI through software interrupt
`0x80`. The shared numeric and structure definitions live in
`shared/userspace_abi.rs`; the direct-child capability bootstrap extension lives
in `shared/protection_abi.rs`. Kernel and userspace include these files directly
so they cannot silently disagree about call numbers or layouts.

The ABI is experimental, but callers can query the current version, 1.26, and a
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
message may carry one rights-reduced capability through these compatibility calls. The `userspace::ipc::receive`
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

ABI 1.12 defines the complete supported signal set:

| Number | Name | Default action | Catchable | Blockable |
| ---: | --- | --- | --- | --- |
| `2` | `INTERRUPT` | terminate | yes | yes |
| `9` | `KILL` | terminate immediately | no | no |
| `15` | `TERMINATE` | terminate | yes | yes |
| `18` | `CONTINUE` | continue | yes | yes |
| `19` | `STOP` | stop | no | no |
| `20` | `TERMINAL_STOP` | stop | yes | yes |

Signal 9 is the supervisor's bounded forced-termination fallback. Installing a
handler or ignore action for it returns `EINVAL`, and signal masks always clear
its bit. It uses the existing direct-child and process-group signaling syscalls;
ABI 1.12 adds no new syscall number.

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

## Version 1.10 UUID-selected writable NullFS block-device acquisition

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 55 | `open_writable_nullfs_block_device_endpoint` | `rdi`: 16-byte UUID address; `rsi`: exact length | capability handle |

ABI 1.9 syscall 54 retains its PID-1-only writable partition-index contract for
compatibility. ABI 1.10 adds syscall 55 for stable filesystem-UUID selection, and
current primary-volume policy uses only the new operation. Syscall 52 remains
unconditionally read-only and index-based. Only PID 1 may call syscall 55;
permission is checked before its UUID pointer is read. The
UUID must be nonzero and exactly 16 readable bytes. Zero eligible matches return
`ENOENT`, while multiple eligible matches return `EINVAL` rather than selecting
an arbitrary partition.

An eligible match requires a disk with no extended partition and a nonzero-start
primary MBR `PartitionKind::NullFs` entry that does not overlap another discovered
partition and contains a valid decoded NullFS superblock. Selection compares only
the decoded UUID, not partition order or label. Logical/extended MBR, GPT, and
superfloppy writable grants remain disabled until their reserved disk-metadata
ranges are modeled explicitly.

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

## Version 1.13 filesystem-provider offlining

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 58 | `OFFLINE_FILESYSTEM_PROVIDER` | `provider_kind`, `expected_generation` | zero |

This PID-1-only operation removes one exact filesystem-provider incarnation from
kernel routing. Provider kinds are `1` for tmpfs, `2` for NullFS, and `3` for
VFS. The expected generation must be representable as a nonzero `u32`.

An exact active-generation match atomically changes that provider to an offline
tombstone retaining the generation. Repeating the call for that exact tombstone
is an idempotent success. An unknown provider kind, zero or out-of-range
generation, or stale or otherwise mismatched generation returns `EINVAL`.
Callers other than PID 1 receive `EPERM`.

Offlining fails and wakes blocked work belonging to that exact generation with
`EIO`, rejects stale replies and later stale work, and purges queued close work
for the old generation. Registration of a replacement must use a strictly newer
generation and a fresh endpoint object; a new handle to the old endpoint object
is not sufficient. The tombstone remains authoritative until that registration
succeeds.

Supervisor ordering is final child status, exact-generation offlining,
failure/wakeup of old work, closure of PID 1's old endpoint handle, creation of
a fresh endpoint object, startup and registration under the newer generation,
and only then completion of the restart fence. Writable NullFS is not withdrawn
before final child status because a pre-exit quiesce-and-sync protocol remains
future work.

## Version 1.14 writable NullFS block-endpoint offlining

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 59 | `OFFLINE_WRITABLE_NULLFS_BLOCK_DEVICE_ENDPOINT` | `rdi`: 16-byte UUID address; `rsi`: exact length; `rdx`: expected endpoint generation | zero |

This PID-1-only fault-containment operation offlines the exact writable NullFS
block-endpoint object selected by a filesystem UUID and endpoint generation.
Permission is checked before reading user memory. The UUID must be exactly 16
readable, nonzero bytes, and the expected endpoint generation must be nonzero.
No matching UUID returns `ENOENT`; an ambiguous UUID, stale or wrong generation,
missing endpoint object, or already-offline generation returns `EINVAL`.

A successful call changes the endpoint to an offline tombstone. New acquisition
and connection attempts return `EIO`. Existing sessions and the endpoint object
remain drainable so operations admitted after the tombstone complete deterministically
rather than hanging: every operation except `DISCONNECT` returns protocol `IO`, while
`DISCONNECT` remains available for bounded cleanup. A physical operation already admitted
before the fence may complete; offlining is not cancellation of in-progress disk I/O.
Closing PID 1's source handle alone is not revocation because delegated handles keep the
endpoint object alive.

A filesystem mutation may have committed before provider loss is observed. The
filesystem service therefore reports `OUTCOME_UNKNOWN`, fail-stops, and requires
supervision to offline the matching filesystem-provider generation. Neither the
kernel proxy nor callers automatically retry an uncertain mutation.

## Version 1.15 basic job containment

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 60 | `JOB_CREATE` | none | new job handle |
| 61 | `JOB_ASSIGN` | job handle, direct-child PID | assigned PID |
| 62 | `JOB_TRY_WAIT` | job handle, `job::Exit` address, exact byte length | zero |
| 63 | `JOB_TERMINATE` | job handle | number of members signaled |

A job handle is capability kind `5`. Its full rights are `DUPLICATE | TRANSFER |
SIGNAL | WAIT | MANAGE`; rights-reduced observer and cleanup handles are supported.
`MANAGE` authorizes adding a live direct child that does not already belong to a job.
Once assigned, that process cannot be moved to another job, and every later `fork`
descendant inherits the same job before becoming visible to userspace.

`JOB_TRY_WAIT` consumes one fixed 16-byte FIFO exit record containing the member PID and
the same terminal status encoding used by child waiting. It returns `EAGAIN` while members
remain but no record is ready, and `ECHILD` when both the member set and completion queue
are empty. Job observation is independent of parent/child waiting: consuming either view
does not consume the other. Fault and signal exits preserve the existing
`child_status::SIGNAL_BASE + value` representation.

Live members and undrained exit records share the existing 64-process bound. Assignment
or inherited `fork` returns `ENOSPC` rather than accepting a member whose exit could not
be retained. `JOB_TERMINATE` requires `SIGNAL` and delivers uncatchable signal 9 to every
current member, including orphaned descendants. The job object remains reachable while
it contains members even if userspace closes its last handle.

The ABI 1.15 slice is intentionally flat. It does not add kill-on-last-handle-close,
resource budgets, process suspension, or blocking/multi-object wait. A launcher requiring
strict containment must keep a new child behind its existing launch barrier until `JOB_ASSIGN`
succeeds.

## Version 1.16 child-job hierarchy

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 64 | `JOB_CREATE_CHILD` | parent job handle | new child-job handle |

`JOB_CREATE_CHILD` requires `MANAGE` on the parent. A child job is attached exactly once at
creation, cannot be reparented, and retains a reverse parent edge so any live handle or process in
the connected tree preserves the immutable hierarchy. Each job may own at most 32 direct children,
and the existing global 32-job bound still applies.

Process assignment remains leaf-local and non-relaxable: `JOB_ASSIGN` adds a direct child process to
the selected job, and later `fork` descendants inherit that exact job. Capability `Info.size`,
`JOB_TRY_WAIT`, and `JOB_TERMINATE` on a job cover its complete subtree. Waiting uses deterministic
parent-before-child breadth-first traversal, preserves FIFO order within each job, and consumes an
exit exactly once across handles to that job or its ancestors. It returns `EAGAIN` while any subtree
member remains without a completion and `ECHILD` only when the entire subtree has no live members or
pending completions. Termination snapshots and signals all current subtree members.

This slice does not add hierarchy enumeration, reparenting, per-job resource budgets, suspension,
kill-on-close, or blocking/multi-object wait. ABI 1.18 adds the narrowly scoped permanent leaf
retirement operation described below.

## Version 1.17 job process limits

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 65 | `JOB_SET_PROCESS_LIMIT` | job handle, maximum live processes | accepted limit |

`JOB_SET_PROCESS_LIMIT` requires `MANAGE`. The configured limit is in the inclusive range `0..=64`,
may stay unchanged or decrease, and cannot increase: relaxation returns `EPERM`, while a value above
the global process bound returns `ERANGE`. A new child job inherits its parent's configured limit.

Before `JOB_ASSIGN` or inherited child creation admits a process, the kernel checks the target job
and every ancestor. Admission returns `ENOSPC` if any checked job already has at least its configured
number of live processes across its complete subtree. Pending completion records do not count toward
this policy ceiling and retain their existing lossless bounded storage.

Tightening below current usage is valid and does not terminate existing processes. It prevents new
admission until usage falls below every applicable ceiling; setting zero therefore freezes process
creation in the subtree without being a termination operation. ABI 1.19 adds the read-only local
limit query described below. This slice does not add CPU or memory accounting, reservations, or
policy relaxation.

## Version 1.18 child-job retirement

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 66 | `JOB_RETIRE` | child-job handle | zero |

`JOB_RETIRE` requires `MANAGE` on the target job. The target must have a parent, no child jobs, no
live processes, and no pending completion records. A root returns `EINVAL`; a nonempty or nonleaf
job returns `EAGAIN`. On success, the kernel atomically marks the target retired and detaches its
single parent edge.

Retirement is permanent and is not reparenting. A retired handle remains a valid job handle for
inspection, waiting, and closure, but assignment, child creation, process-limit changes, and repeated
retirement return `EPERM`. Inspection reports size zero, waiting reports `ECHILD`, and termination
returns zero. With no parent edge, the retired object becomes collectible after its final handle or
transferred reference closes. This allows a long-lived hierarchy owner to rotate more child
generations than the global simultaneous-job bound while preserving non-relaxable policy.

This slice does not recursively retire a subtree, implicitly consume completion records, or retire a
job on last-handle close. Managers must explicitly drain children before retiring them from leaves
upward.

## Version 1.19 job process-limit inspection

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 67 | `JOB_GET_PROCESS_LIMIT` | job handle | configured local process limit |

`JOB_GET_PROCESS_LIMIT` requires `WAIT`, so the same attenuated authority used to observe subtree
exits can inspect policy without receiving mutation authority. It accepts root, child, and retired
job handles and returns that selected job's configured local ceiling in the inclusive range
`0..=64`. A retired job retains its last configured ceiling for inspection.

The result is not an effective ancestor minimum, current usage, or remaining admission capacity.
Capability `Info.size` continues to report current live membership across the complete selected
subtree. Admission still checks every ancestor and the query cannot relax any ceiling.

## Version 1.20 atomic capability replacement

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 68 | `CAPABILITY_REPLACE` | source handle, reduced rights | replacement handle |

`CAPABILITY_REPLACE` requires `DUPLICATE` on the source and accepts the same nonempty, valid
rights subsets as `CAPABILITY_DUPLICATE`. It atomically consumes the source and installs a
rights-reduced handle to the same object without requiring another free capability-table slot.
The replacement preserves diagnostic object identity and cannot amplify authority.

Validation completes before the source is changed. An invalid handle returns `EBADF`; missing
`DUPLICATE`, an empty mask, unknown rights, or rights outside the source mask return `EPERM`, and
the source remains valid and unchanged. Handle values are opaque: callers must use the returned
replacement even when it has the same numeric value as the source.

## Version 1.21 atomic endpoint move-transfer

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 69 | `ENDPOINT_SEND_MOVE` | endpoint, byte address, byte count, transfer handle, transfer rights | zero |

`ENDPOINT_SEND_MOVE` sends one bounded message carrying exactly one rights-reduced capability. The
endpoint requires `SEND`; the source requires `TRANSFER`; and the requested rights must be a
nonempty valid subset of the source. Unlike ABI 1.2 `ENDPOINT_SEND`, success consumes the source
handle atomically when the message and transferred object reference are committed to the endpoint
queue.

All validation and queue-capacity checks precede source removal. `EBADF`, `EPERM`, `EINVAL`,
`EFAULT`, `ERANGE`, or queue-full `EAGAIN` therefore leaves the source handle, message queue, and
object lifetime unchanged. Once committed, receive follows the existing atomic rules: insufficient
buffer or handle-table capacity does not dequeue the message. This slice moves exactly one handle;
multi-handle transfer and sender-side receiver-capacity reservation remain future work.

## Version 1.22 level-triggered object signal state

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 70 | `CAPABILITY_SIGNAL_STATE` | capability handle | current signal mask |

`CAPABILITY_SIGNAL_STATE` requires `WAIT` on the selected handle and returns an immediate,
level-triggered snapshot. Endpoint handles report `READABLE` while their queue is nonempty and
`WRITABLE` while it has capacity. Notification handles report `SIGNALED` while their pending count
is nonzero. Job handles report `READABLE` while any exit record in the selected subtree is pending
and `TERMINATED` while that subtree contains no active process. `READABLE | TERMINATED` is valid
while a fully exited job still has undrained completion records.

An invalid handle returns `EBADF`; missing `WAIT` returns `EPERM`; and object kinds without defined
waitable signals return `EINVAL`. The query does not consume state, block, accept a deadline, or
promise that a later operation will still observe the same condition. `OBJECT_SIGNAL_STATE` in
`SystemInfo.capabilities` advertises the operation.

## Version 1.23 monotonic clock and single-object waiting

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 71 | `MONOTONIC_TIME` | none | nanoseconds since kernel timer initialization |
| 72 | `OBJECT_WAIT_ONE` | capability handle, requested signal mask, absolute deadline in monotonic nanoseconds | requested asserted signal mask |

`OBJECT_WAIT_ONE` requires `WAIT`, a nonempty known signal mask, and only signals supported by the
selected object. It returns immediately when any requested signal is asserted; the return value is
the intersection of the requested and current masks. Otherwise it registers the process and blocks
its scheduler task until a relevant level-triggered transition or deadline expiry. Waking does not
consume messages, notification counts, or job completion records.

Deadline zero is an immediate poll and `UINT64_MAX` means no deadline. Other values are absolute
timestamps in the same nanosecond domain returned by `MONOTONIC_TIME`; finite deadlines are serviced
at the kernel timer's current 100 Hz resolution. An expired deadline returns `ETIMEDOUT`. Invalid or
empty masks and signals unsupported by the object return `EINVAL`; invalid handles return `EBADF`;
and missing `WAIT` returns `EPERM`. `MONOTONIC_CLOCK` and `OBJECT_WAIT_ONE` in
`SystemInfo.capabilities` advertise the two operations.

## Version 1.24 bounded many-object waiting

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 73 | `OBJECT_WAIT_MANY` | wait-item array address, item count, absolute deadline in monotonic nanoseconds | lowest satisfied array index |

Each wait item is the following exact 16-byte structure:

```rust
#[repr(C)]
pub struct ObjectWaitItem {
    pub handle: u64,
    pub requested_signals: u64,
}
```

The item count must be between one and `MAX_OBJECT_WAIT_ITEMS` (currently 16). The kernel copies and
validates the complete array before inspecting readiness: every handle must exist, carry `WAIT`, and
request a nonempty signal subset supported by its object. An unreadable array returns `EFAULT`, zero
items return `EINVAL`, and an oversized array returns `E2BIG`. A validation error in any item wins
over readiness in an earlier item.

After validation, the syscall returns the lowest array index whose requested mask intersects the
object's current level-triggered signal state. Duplicate handles are valid and retain array-order
priority. If no item is ready, the process blocks under the same immediate, infinite, and finite
absolute-deadline rules as `OBJECT_WAIT_ONE`; expiry returns `ETIMEDOUT`. The operation does not
consume object state, and one process may have only one outstanding generic object wait.
`OBJECT_WAIT_MANY` in `SystemInfo.capabilities` advertises the operation.

## Version 1.25 channel pairs and peer closure

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 74 | `ENDPOINT_CREATE_PAIR` | output address, output bytes | fills `capability::EndpointPair` |

The output is the following exact 16-byte structure:

```rust
#[repr(C)]
pub struct EndpointPair {
    pub first: u64,
    pub second: u64,
}
```

Pair creation atomically reserves two distinct endpoint objects and two handles with full endpoint
rights. Exhausting either bound returns `ENOSPC` without creating a partial pair. A short output
buffer returns `ERANGE`, and an invalid writable range returns `EFAULT`, before any allocation.

Each endpoint owns one bounded incoming queue. Sending through one endpoint enqueues on its peer;
receiving dequeues from the selected endpoint's own queue. `READABLE` is asserted while that queue
is nonempty. `WRITABLE` is asserted while the peer exists and its incoming queue has capacity.
Dropping the final reference to either endpoint permanently asserts `PEER_CLOSED` on the survivor
and clears `WRITABLE`. Unread messages already queued on the surviving endpoint remain readable and
may be drained; afterward receive and all sends return `EPIPE`. Destroying an endpoint discards its
own unread queue and closes capabilities held only by those messages.

Duplication and transfer preserve endpoint lifetime, so peer closure occurs only after the final
handle, queued attachment, and kernel root is gone. Process teardown removes its capability table
and publishes any resulting closure. The older `ENDPOINT_CREATE` call remains a compatible
one-ended loopback mailbox: its sends target its own queue and it never asserts `PEER_CLOSED`.
`CHANNEL_PAIRS` in `SystemInfo.capabilities` advertises the operation.

## Version 1.26 bounded multi-handle messages

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 75 | `ENDPOINT_SEND_MOVE_MANY` | endpoint, byte address, byte count, disposition address, disposition count | zero |
| 76 | `ENDPOINT_RECEIVE_MANY` | endpoint, buffer address, byte capacity, handle-output address, handle capacity, message-info address | received byte count |

The new calls use these fixed-width records:

```rust
#[repr(C)]
pub struct HandleDisposition {
    pub handle: u64,
    pub rights: u64,
}

#[repr(C)]
pub struct ReceivedHandle {
    pub handle: u64,
    pub rights: u64,
}

#[repr(C)]
pub struct MessageInfoMany {
    pub sender_process_id: u64,
    pub byte_count: u64,
    pub handle_count: u64,
    pub reserved: u64,
}
```

`ENDPOINT_SEND_MOVE_MANY` accepts one through four distinct source handles. Every source must carry
`TRANSFER`, and each requested nonempty rights mask must be a subset of both its source rights and
the rights valid for that object kind. The kernel copies and validates the entire disposition array,
resolves the destination, and checks queue capacity before removing any source. Duplicate sources
return `EINVAL`; an empty array returns `EINVAL`; more than four entries returns `E2BIG`. Any failed
validation or full-queue `EAGAIN` leaves all source handles, bytes, and queue state unchanged.

`ENDPOINT_RECEIVE_MANY` accepts capacity for zero through four output handles. It always validates
the output ranges before inspecting the front message. When the byte or handle capacity is too
small, it writes canonical required counts to `MessageInfoMany` and returns `ERANGE` without
dequeueing or installing any handle. On success the kernel reserves all required process-local
handle-table slots before dequeue, then publishes the bytes, ordered `{handle, rights}` records, and
message metadata together. Table exhaustion returns `ENOSPC` with the message still queued.

The ABI 1.2 receive call returns `ERANGE` without consuming a message containing more than one
attachment, allowing old callers to leave it for a multi-handle-aware receiver. Existing zero- and
one-handle messages are accepted by `ENDPOINT_RECEIVE_MANY`. `MULTI_HANDLE_MESSAGES` in
`SystemInfo.capabilities` advertises both calls. Sender-side reservation against the eventual
receiving process remains future work because receive authority may currently be shared.

## Version 1.27 bounded persistent wait sets

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 77 | `WAIT_SET_CREATE` | none | wait-set handle |
| 78 | `WAIT_SET_ADD` | wait set, target, requested signals, key | zero |
| 79 | `WAIT_SET_REMOVE` | wait set, key | zero |
| 80 | `WAIT_SET_WAIT` | wait set, absolute deadline | packed readiness event |

A wait set is a transferable kernel object with `DUPLICATE | TRANSFER | WAIT | MANAGE` rights and
space for at most 64 persistent registrations. `WAIT_SET_ADD` requires `MANAGE` on the set and
`WAIT` on an endpoint, notification, or job target. The signal mask must be nonempty and supported
by the target, the key must be unique within the set, and keys are limited to `2^55 - 1`.
Unsupported masks, duplicate keys, nested wait sets, and out-of-range keys return `EINVAL`; a full
set returns `ENOSPC`. `WAIT_SET_REMOVE` requires `MANAGE` and returns `ENOENT` for an unknown key.

Each registration retains the target object even if its original handle is closed. Consequently a
wait-set handle reduced to `WAIT` intentionally delegates observation of every registered target
without delegating their individual handles. Removing the registration or destroying the final
wait-set reference releases that retained object identity.

`WAIT_SET_WAIT` requires `WAIT` on a nonempty set and uses the same absolute monotonic deadlines as
the object-wait calls: zero polls, `UINT64_MAX` waits indefinitely, and an expired finite deadline
returns `ETIMEDOUT`. The kernel snapshots the current registrations before state inspection and
scheduler blocking, so add/remove operations affect the next wait while target signal transitions
continue to wake the in-flight snapshot. Ready registrations are selected in insertion order.
Readiness is level-triggered and does not consume the underlying object state.

A successful result packs the caller key and asserted signals into one nonnegative `u64`:

```text
event = (key << 8) | asserted_signals
key = event >> 8
asserted_signals = event & 0xff
```

The key bound keeps bit 63 clear, so a successful event cannot overlap the negative errno encoding.
`WAIT_SETS` in `SystemInfo.capabilities` advertises the object and all four calls.

## Compatibility rules

- Existing syscall numbers 1 through 34 are unchanged.
- ABI 1.2 adds protection calls 35 through 48 and capability feature bits.
- ABI 1.4 adds PID-1 tmpfs service registration at syscall 49.
- ABI 1.5 adds PID-1 VFS routing service registration at syscall 50. The
  kernel retains the send endpoint and completes a versioned root-route
  handshake asynchronously before treating the service as ready.
- ABI 1.6 adds VFS-routed pathname deletion at syscall 51.
- ABI 1.7 adds PID-1 read-only partition endpoint acquisition at syscall 52.
- ABI 1.9 added the initial narrowly scoped writable NullFS partition endpoint at
  syscall 54 while preserving syscall 52 as unconditionally read-only.
- ABI 1.10 adds exact, unique filesystem-UUID selection at syscall 55 without
  changing the ABI 1.9 syscall 54 contract.
- ABI 1.12 adds uncatchable, unblockable signal 9 to the existing direct-child
  and process-group signaling contracts.
- ABI 1.13 adds PID-1-only exact-generation filesystem-provider offlining at
  syscall 58 without changing the filesystem or `NSVC` wire protocols.
- ABI 1.14 adds PID-1-only exact-UUID and exact-generation writable NullFS
  block-endpoint offlining at syscall 59 without changing the block-device,
  filesystem, or `NSVC` wire protocols.
- ABI 1.15 adds capability-backed job creation, direct-child assignment, inherited
  descendant containment, independent exit observation, and whole-job termination at
  syscalls 60 through 63.
- ABI 1.16 adds immutable child-job creation and subtree inspection, exit drainage, and
  termination at syscall 64.
- ABI 1.17 adds a tightening-only hierarchy-scoped process ceiling at syscall 65.
- ABI 1.18 adds permanent retirement and reclamation for empty child-job leaves at syscall 66.
- ABI 1.19 adds read-only inspection of a job's configured local process ceiling at syscall 67.
- ABI 1.20 adds atomic rights-reduced capability replacement at syscall 68.
- ABI 1.21 adds atomic one-handle endpoint move-transfer at syscall 69.
- ABI 1.22 adds `WAIT`-authorized, level-triggered object signal snapshots at syscall 70.
- ABI 1.23 adds monotonic clock discovery and absolute-deadline single-object waiting at syscalls
  71 and 72.
- ABI 1.24 adds bounded absolute-deadline many-object waiting at syscall 73.
- ABI 1.25 adds atomic channel-pair creation and level-triggered peer closure at syscall 74.
- ABI 1.26 adds atomic move and receive of up to four message handles at syscalls 75 and 76.
- ABI 1.27 adds bounded persistent tagged wait sets at syscalls 77 through 80.
- New structures use `#[repr(C)]` and fixed-width integer fields.
- Unknown calls return `ENOSYS`.
- Resource bounds remain part of normal failure behavior; protection bounds are
  fixed in the shared ABI definitions.
- ABI changes that alter an existing structure or semantic contract must bump
  the reported version and update this document, the shared definitions, and
  the runtime probe together.
