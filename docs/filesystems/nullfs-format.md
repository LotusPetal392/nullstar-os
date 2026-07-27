# NullFS on-disk format

This document defines the first on-disk format decisions for NullFS, NullStar's
native persistent filesystem. It is a format contract, not an implementation
status document. Fields and structures not assigned here remain unspecified and
must not be inferred from Rust data layouts.

NullFS is intended to run as a userspace filesystem service. The disk format is
private to that service and its host-side tools; clients access files through
the common contract in [`../filesystem-service-protocol.md`](../filesystem-service-protocol.md).
Disk block numbers, allocation groups, and physical extents must not leak into
VFS node IDs or protocol messages.

## Version 1, Phase 1 scope

Phase 1 establishes enough format to create, identify, inspect, and safely
reject unsupported NullFS volumes. It does not yet standardize inode, directory,
extent, free-space, journal, or snapshot records.

The fixed decisions are:

- all integers are unsigned and little-endian;
- the logical filesystem block size is exactly 4096 bytes;
- bytes `0..65536` are a fixed 64 KiB reserved boot area;
- the primary superblock occupies block 16, at byte offset 65536;
- allocation-group descriptor reservation begins at block 17;
- all records use explicit byte-level encode/decode routines; code must never
  serialize or parse a Rust struct with `memcpy`, pointer casts, `transmute`, or
  an equivalent representation-dependent operation;
- the primary superblock is protected by CRC32C (Castagnoli);
- unknown incompatible features cause the volume to be rejected;
- unknown read-only-compatible features permit mounting only read-only;
- a volume records an explicit clean or dirty state.

A v1 reader must reject a volume whose declared block size is not 4096. A later
format version may add other block sizes, but it must not reinterpret v1.

## Device layout

All offsets are from the start of the filesystem's block device or image.

| Byte range | Blocks | Purpose |
| --- | ---: | --- |
| `0..65536` | `0..15` | Reserved boot area |
| `65536..69632` | `16` | Primary superblock |
| starting at `69632` | starting at `17` | Allocation-group descriptor reservation |
| after the descriptor reservation | format-defined later | Allocation groups and filesystem data |

The boot area is always reserved, even on a non-bootable volume. Formatters must
zero it unless explicitly given boot payload data. Filesystem metadata must not
point into it.

The superblock declares the number of blocks reserved for allocation-group
descriptors. This makes the first allocatable block calculable without assuming
how descriptors will eventually be encoded. Phase 1 tools reserve and zero that
range but do not emit descriptor records.

All arithmetic involving offsets, block counts, capacities, or group geometry
must be checked for overflow and checked against the actual device length before
I/O. Trailing device bytes that do not form a complete 4096-byte block are not
part of the filesystem capacity.

## Primary superblock

The primary superblock is one complete 4096-byte block. The following byte
layout is frozen for format version 1. Reserved bytes must be written as zero
and ignored after validation by v1 readers; they are not an extension mechanism.

| Offset | Size | Field | v1 meaning |
| ---: | ---: | --- | --- |
| `0` | 8 | magic | ASCII `NULLFS\0\0` |
| `8` | 2 | version major | `1` |
| `10` | 2 | version minor | `0` for Phase 1 |
| `12` | 4 | header bytes | `256` |
| `16` | 4 | block size | `4096` |
| `20` | 4 | volume state | `0` clean, `1` dirty |
| `24` | 8 | compatible features | Feature bitmap |
| `32` | 8 | read-only-compatible features | Feature bitmap |
| `40` | 8 | incompatible features | Feature bitmap |
| `48` | 16 | volume UUID | Raw UUID bytes; all-zero is invalid |
| `64` | 64 | volume label | UTF-8 bytes followed by zero padding |
| `128` | 8 | capacity blocks | Filesystem capacity in 4096-byte blocks |
| `136` | 8 | allocation-group size blocks | Nominal blocks per group |
| `144` | 4 | allocation-group count | Number of groups covering capacity |
| `148` | 4 | descriptor reservation blocks | Blocks reserved beginning at block 17 |
| `152` | 8 | first descriptor block | Must be `17` in v1 |
| `160` | 8 | first allocatable block | `17 + descriptor reservation blocks` |
| `168` | 88 | reserved header bytes | Must be zero when written |
| `256` | 3836 | reserved superblock bytes | Must be zero when written |
| `4092` | 4 | CRC32C | Checksum of the complete superblock |

The label's first zero byte terminates the label. Bytes after the terminator
must be zero. A full 64-byte label has no terminator. Empty labels are allowed.
Readers must validate the used bytes as UTF-8; normalization and
case-comparison policy are not defined by Phase 1.

`capacity_blocks` must be at least large enough to contain block 16 and the
entire descriptor reservation, and must not exceed the complete blocks present
on the device. The group size must be nonzero. The group count must equal
`ceil(capacity_blocks / allocation_group_size_blocks)` using checked arithmetic;
the last group may be shorter. Phase 1 does not require allocation groups to be
power-of-two sized.

The descriptor reservation must be nonzero and large enough for the declared
group count according to the descriptor encoding introduced by a later phase.
Until that encoding is frozen, Phase 1 formatters use an implementation-defined,
documented reservation policy and inspectors treat the reserved blocks as
opaque zero-filled space. Changing that policy does not change the location of
its first block.

### Checksum

The checksum algorithm is CRC32C using the Castagnoli polynomial. To calculate
or verify it:

1. copy or stream exactly the 4096 bytes of block 16;
2. treat bytes `4092..4096` as four zero bytes;
3. calculate CRC32C over all 4096 bytes in increasing byte-offset order;
4. encode the result as little-endian `u32` in the checksum field.

A checksum mismatch is corruption and must reject the superblock. CRC32C detects
accidental corruption; it is not an authenticity or tamper-resistance mechanism.
Backup superblocks and recovery selection are intentionally deferred.

## Version and feature handling

Major versions describe incompatible format families. A reader supporting major
version 1 must reject every other major version. The minor version may introduce
changes governed by feature bits; a reader must not accept a higher minor version
unless all required behavior is understood through the following feature rules.

- **Compatible:** an unknown bit may be ignored for both reading and writing.
- **Read-only-compatible:** an unknown bit permits inspection or a read-only
  mount, but any request for a writable mount must fail.
- **Incompatible:** an unknown bit rejects the volume for both read-only and
  writable mounts.

No feature bits are assigned in Phase 1, so writers emit zero for all three
bitmaps. Bits acquire meaning only through a format-document update and tests;
they must not be assigned ad hoc by an implementation.

## Clean and dirty state

Only volume-state values `0` and `1` are valid in v1:

- **clean (`0`)**: all committed metadata is consistent according to the format;
- **dirty (`1`)**: a writable mount was active or an update may not have reached
  a recoverable commit point.

Before allowing filesystem mutation, a service must durably write a superblock
with dirty state and a valid updated checksum. It may write clean only after all
required filesystem data and metadata have been durably committed and the
volume is being cleanly unmounted or explicitly synchronized under a format
rule that permits it.

Phase 1 has no journal or repair procedure. Consequently, a dirty volume may be
inspected but must not be mounted writable. Later recovery work must define when
and how dirty state can be cleared; merely rewriting the state is not recovery.
Unknown state values are corruption and reject the volume.

## Encoding and validation requirements

The canonical representation is the byte sequence above, not an in-memory type.
Implementations should expose semantic values such as `Superblock` internally,
but `encode_superblock` and `decode_superblock` (names illustrative) must read
and write each field explicitly with fixed offsets and little-endian conversion.
This avoids padding, alignment, enum representation, host-endian, and compiler
layout dependencies.

A decoder validates before exposing a mountable volume, in this order where
practical:

1. obtain exactly block 16 without reading beyond the device;
2. validate magic and CRC32C;
3. validate major/minor version and header size;
4. validate block size, reserved values, UUID, label, and volume state;
5. apply feature compatibility rules;
6. validate capacity and allocation-group geometry with checked arithmetic;
7. choose read-only or writable eligibility from features and clean state.

Decoders must return structured errors suitable for host tools and for mapping
to filesystem-service failures. They must not panic on arbitrary input. Encoders
must produce deterministic bytes: identical semantic inputs produce identical
4096-byte superblocks, including zeroed reserved ranges.

## Version 1.1, Phase 2 read-only records

Phase 2 images use minor version `1` and set incompatible feature bit
`INCOMPAT_PHASE2_CORE` (`1 << 0`). A Phase 2 mount requires both markers; Phase
1 images remain inspectable but do not contain mountable inode metadata.

### Allocation-group descriptor tables

Descriptor-table blocks begin at block 17. Each is 4096 bytes, has a 64-byte
header, and holds at most 42 descriptors of 96 bytes each. The header is:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 8 | magic `NFSAGDT\0` |
| 8 | 2 | record version, `1` |
| 10 | 2 | header bytes, `64` |
| 12 | 2 | descriptor bytes, `96` |
| 14 | 2 | descriptors used in this block |
| 16 | 4 | first global descriptor index |
| 20 | 4 | total descriptor count |
| 24 | 4 | table-block index |
| 28 | 4 | table-block count |
| 32 | 8 | physical block number of this table block |
| 40 | 20 | reserved, zero |
| 60 | 4 | CRC32C of the complete block with this field zero |

Each descriptor uses this layout:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 4 | group index |
| 4 | 4 | flags, zero in Phase 2 |
| 8 | 8 | group start block |
| 16 | 8 | group block count |
| 24 | 8 | reserved block-bitmap block |
| 32 | 8 | reserved inode-bitmap block |
| 40 | 8 | first inode-table block |
| 48 | 4 | inode-table block count, `16` |
| 52 | 4 | inodes in group, `256` |
| 56 | 8 | first data block |
| 64 | 8 | exclusive data-end block |
| 72 | 4 | root inode index, or `u32::MAX` |
| 76 | 20 | reserved, zero |

The nominal allocation-group size is 8192 blocks. Group metadata consists of a
reserved block bitmap, reserved inode bitmap, and 16-block inode table, followed
by data blocks. Both bitmap blocks must contain only zero bytes in Phase 2; they
are reservations, not allocation authorities. Descriptor capacity limits the
one-block default reservation to 42 groups, so formatters must reject larger
volumes unless a larger reservation is explicitly supported.

### Inodes and inline extents

Every allocation group has 256 fixed 256-byte inode slots. Inode number zero is
invalid; inode 1 is the root. An all-zero slot is free. Allocated slots use:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 2 | record version, `1` |
| 2 | 2 | record bytes, `256` |
| 4 | 2 | kind: `1` regular, `2` directory, `3` symlink |
| 6 | 2 | Unix permission mode, low 12 bits only |
| 8 | 4 | uid |
| 12 | 4 | gid |
| 16 | 4 | link count |
| 20 | 4 | flags, zero in Phase 2 |
| 24 | 8 | generation |
| 32 | 8 | logical size in bytes |
| 40 | 8 | allocated block count |
| 48 | 8 | parent inode (required for directories) |
| 56 | 16 | access timestamp |
| 72 | 16 | modification timestamp |
| 88 | 16 | change timestamp |
| 104 | 12 | creation timestamp |
| 116 | 2 | inline extent count, at most 4 |
| 118 | 2 | reserved, zero |
| 120 | 8 | directory entry count; zero for non-directories |
| 128 | 96 | four 24-byte inline extents |
| 224 | 28 | reserved, zero |
| 252 | 4 | CRC32C of the complete inode with this field zero |

A timestamp stores `u64` seconds followed by `u32` nanoseconds; its remaining
four bytes are zero. Each extent stores `u64 logical_first_block`, `u64
physical_first_block`, `u32 length_blocks`, and zero `u32 flags`. Extents are
ordered and non-overlapping. Logical gaps in regular files read as zero. Directory
extents must cover every logical directory block without gaps. Phase 2 supports
at most four inline extents and defines the symlink kind but no symlink payload,
so non-empty symlinks are rejected.

### Linear directory blocks

A directory is a sequence of checksummed 4096-byte blocks. Each block has a
128-byte header followed by 31 fixed 128-byte entries:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 8 | magic `NFSDIR\0\0` |
| 8 | 2 | record version, `1` |
| 10 | 2 | header bytes, `128` |
| 12 | 2 | entry bytes, `128` |
| 14 | 2 | slots per block, `31` |
| 16 | 8 | owning inode |
| 24 | 8 | logical block index |
| 32 | 4 | occupied entry count |
| 36 | 88 | reserved, zero |
| 124 | 4 | CRC32C of the complete block with this field zero |

An all-zero entry is unused. An occupied entry stores its target inode at offset
0, generation at 8, one-byte node kind at 16, one-byte name length at 17,
reserved zero bytes at 18..24, UTF-8 name bytes plus zero padding at 24..120,
and reserved zero bytes at 120..128. Names are 1 through 96 bytes and may not
contain NUL or `/`; `.` and `..` are synthesized rather than stored. Duplicate
names are invalid.

Directory cookies are deterministic: cookies 1 and 2 follow synthesized `.` and
`..`; stored slot `(logical_block, slot)` has base cookie
`logical_block * 31 + slot + 1`, offset by 2 at the semantic core boundary.

### Phase 2 consistency rules

Read-only mounts validate all metadata before exposing the volume: checksums and
reserved bytes, descriptor geometry, zero reservation bitmaps, inode and extent
ranges, duplicate physical-block ownership, root identity, directory target kind
and generation, duplicate names, parent cycles, entry counts, and reachability
of every allocated inode. Any violation rejects the mount. Phase 2 defines no
allocation-map authority, mutation, repair, or crash-recovery semantics.

## Version 1.2, Phase 3 writable redo format

Phase 3 images use minor version `2` and set incompatible feature bit
`INCOMPAT_PHASE3_WRITABLE_REDO` (`1 << 1`) in addition to the Phase 2 bit. The
Phase 2 inode, extent, directory, and allocation-group descriptor encodings are
unchanged. Phase 3 makes allocation bitmaps authoritative and adds redundant
superblocks, persistent allocator state, and a fixed single-transaction redo
journal.

### Fixed bootstrap geometry

Phase 3 uses the following canonical geometry. Other values are invalid in 1.2:

| Blocks | Purpose |
| ---: | --- |
| 16 | Primary superblock |
| 17 | Allocation-group descriptor table |
| 18 | Backup superblock |
| 19 | Filesystem-state block |
| 20..21 | Two redundant journal control blocks |
| 22..85 | 64 journal tag blocks |
| 86..149 | 64 full-block journal after-images |
| 150 onward | Allocation-group metadata and data |

The superblock fields at offsets 168, 176, and 184 are respectively the `u64`
backup-superblock, filesystem-state, and journal-first block numbers. Offsets
192 and 196 contain the `u32` journal block count (`130`) and maximum update
count (`64`). Bytes 200 through 4091 remain zero. Phase 1 and Phase 2 require the
entire old reserved range beginning at 168 to remain zero.

Both superblock copies encode the same identity and geometry. A writable mount
must publish and flush the dirty state before mutation. `sync` checkpoints the
journal but leaves a mounted volume dirty. Clean state is published only by a
clean unmount. If two checksum-valid copies differ only in volume state, readers
conservatively select dirty; any other disagreement is corruption. One invalid
or stale copy may be repaired from the selected valid copy during writable
mount.

### Filesystem-state block

Block 19 is checksummed over all 4096 bytes with bytes 60..64 zero during CRC32C
calculation:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 8 | magic `NFSSTAT\0` |
| 8 | 2 | version, `1` |
| 10 | 2 | header bytes, `64` |
| 12 | 4 | reserved, zero |
| 16 | 8 | persistent generation allocator |
| 24 | 8 | next transaction ID |
| 32 | 8 | orphan-list head inode; zero means empty |
| 40 | 8 | free data-block count |
| 48 | 8 | free inode count |
| 56 | 4 | reserved, zero |
| 60 | 4 | CRC32C |
| 64 | 4032 | reserved, zero |

Both identifiers are nonzero and monotonically increase. Exhaustion is an error;
identifiers never wrap to zero. Free counters are transactionally maintained
caches and must equal counts derived from the authoritative bitmaps.

### Authoritative allocation bitmaps

A group block-bitmap bit indexes `physical_block - group_start_block`. Every
reserved/bootstrap or allocation-group metadata block in the group and every
extent-owned data block has its bit set. A clear data bit is free. An inode
bitmap bit is set exactly when its corresponding inode slot is allocated. Bits
outside the declared group block or inode count and all remaining bitmap bytes
are zero. Writable mounts reject bitmap/inode disagreement, unowned allocated
data, an extent whose bit is clear, duplicate block ownership, or a clear
metadata bit.

### Persistent orphan list

Unlinking an open regular file removes its directory entry but retains the inode
and data until the final open handle closes. Such an inode has link count zero;
its `parent_inode` field is reinterpreted as the next orphan inode number, with
zero terminating the list. `orphan_head` points to the first orphan. This mode is
valid only for regular allocated inodes explicitly reached through the orphan
list; ordinary inode decoding remains strict about nonzero link counts.

Writable mount validates that the list is acyclic and duplicate-free and that
every member is a zero-link regular inode, then reclaims all members because no
process handles survive a restart. Read-only mount rejects a persistent orphan
list with recovery-required rather than exposing unreachable allocated data.

### Journal records

A transaction contains between one and 64 unique complete home-block
after-images. One update is normally consumed by the filesystem-state block, so
ordinary core mutation staging is limited to 63 additional home blocks.

A tag block has magic `NFSJTAG\0`, version and 64-byte header fields at offsets
8 and 10, transaction ID at 16, target home block at 24, image CRC32C at 32,
zero-based update index at 36, and record CRC32C at 60. All other bytes are zero.
Targets must be unique, in update-index order, inside the filesystem, and outside
the protected bootstrap/journal region except for the filesystem-state block.

A control block has magic `NFSJCTL\0`, version/header fields at 8 and 10,
monotonic control generation at 16, transaction ID at 24, state at 32 (`0` empty,
`1` committed), update count at 36, ordered-tag digest at 40, and record CRC32C
at 60. Empty controls have zero transaction ID, count, and digest. Committed
controls have a nonzero transaction ID and 1 through 64 updates.

### Commit and recovery ordering

A transaction commits in this order:

1. write its ordered tag blocks and full-block after-images;
2. flush;
3. publish a checksum-valid committed control in the older control slot;
4. flush—this is the transaction commit point;
5. copy all after-images to their home blocks;
6. flush;
7. publish a newer checksum-valid empty control;
8. flush.

Before the commit point recovery exposes the old state. At or after the commit
point recovery validates every tag, digest, target, and image checksum, replays
all after-images, flushes them, and publishes a newer empty control. Replay is
idempotent. A selected committed control with missing, mixed, duplicate,
out-of-range, or corrupt records rejects the mount rather than guessing. Any I/O
failure after commit uncertainty poisons the running core; continued mutation
requires remount and recovery.

Large regular-file writes may be split into independently committed bounded
transactions and therefore may expose a committed prefix after failure.
Namespace operations such as create, unlink, and rename must fit in one
transaction and are old-or-new after recovery.

## Service architecture boundary

NullFS does not add a filesystem-specific syscall or kernel data path. A NullFS
service connects to the existing VFS routing architecture as a backend
filesystem service and implements the common session, node, metadata,
registered-buffer I/O, and directory-iteration protocol described in
[`../filesystem-service-protocol.md`](../filesystem-service-protocol.md).

The VFS owns mount-point selection and component-by-component traversal across
mounts. NullFS owns its volume, directory lookup within that volume, stable node
identity, metadata, allocation, and persistence. Service node IDs are opaque,
generation-bound identifiers; they are not inode numbers or disk addresses.
Host tools and a FUSE adapter may call shared NullFS core logic, but neither
changes the NullStar VFS wire contract.

## Checking and repair policy

`fsck-nullfs` check mode mounts through the shared read-only core and therefore
uses the same superblock selection, committed-journal overlay, checksum, bitmap,
extent, orphan, reachability, and namespace validation. Checking never modifies
the image. A persistent orphan list reports recovery-required because reclaiming
it is a writable recovery operation.

Repair remains intentionally unavailable until every repair has a deterministic,
documented policy and preserves the original image or produces an auditable
change log. A checksum failure is not sufficient evidence for guessing intended
metadata.

## Deferred format work

The following remain deferred beyond the bounded Phase 3 writable format and
require separate, versioned specifications:

- backup allocation-group descriptors and additional superblock copies;
- external extent trees, symlink payloads, and scalable directories;
- circular or dynamically relocated journals and concurrent transactions;
- hard-link creation and multi-link lifecycle metadata;
- bitmap mirrors or per-bitmap checksums;
- extended attributes, named forks, clones, snapshots, and quotas;
- Unicode normalization and case-comparison policy;
- online format upgrades and feature-bit assignments.
