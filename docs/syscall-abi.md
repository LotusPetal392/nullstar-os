# NullStar OS userspace ABI

NullStar OS exposes a small Rust-oriented ring-3 ABI through software interrupt
`0x80`. The shared numeric and structure definitions live in
`shared/userspace_abi.rs`; kernel and userspace include that file directly so
they cannot silently disagree about call numbers or layouts.

The ABI is experimental, but callers can now query a documented version and
capability mask before relying on optional platform services.

## Calling convention

The syscall number is placed in `rax`. Arguments use, in order:

```text
rdi, rsi, rdx, r10, r8, r9, rbx
```

A non-negative `rax` value is success. Negative values are negated `errno`
numbers. Calls that copy a structure to userspace take both an address and a
byte length. Supplying a shorter buffer returns `ERANGE`, allowing structures to
grow in later ABI revisions without writing past an older caller's allocation.

Userspace should call the typed wrappers in `userspace::syscall` and
`userspace::platform` rather than issuing raw interrupts.

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

The public `userspace::syscall::spawn_command` facade now constructs simple
new-process-group launches from `fork`, `set_process_group`, optional foreground
transfer, and `execve`. The child waits until the parent completes the group
assignment before claiming the terminal or replacing its image. Foreground
transfer is idempotent so either side of the parent/child race may complete it.

Descriptor-bearing launches and joined pipeline stages still use syscall 7's
legacy atomic spawn operation. The `/exec` compatibility launcher also remains
on that path so its existing transactional-exec accounting is unchanged. The
raw syscall remains available during this migration.

## Direct signals

`kill` uses the focused NullStar OS signal set. A process may target one of its
direct children. Other targets return `EPERM`; missing or completed targets
return `ESRCH`. Process-group signaling and shell job control continue to use
the existing group-oriented syscall.

## Compatibility rules

- Existing syscall numbers 1 through 32 are unchanged.
- ABI 1.1 adds process-group calls 33 and 34 and a capability bit.
- New structures use `#[repr(C)]` and fixed-width integer fields.
- Unknown calls return `ENOSYS`.
- Resource bounds are reported by `system_info` and remain part of normal
  failure behavior.
- ABI changes that alter an existing structure or semantic contract must bump
  the reported version and update this document, the shared definitions, and
  the runtime probe together.
