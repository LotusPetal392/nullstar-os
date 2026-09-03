# Application permission-store foundation

## Status

This document describes the implemented allocation-free policy foundation for application file and
directory grants. Canonical portal requests and trusted user-gesture admission are now defined by the
[application portal admission foundation](application-portal-admission.md). A trusted picker, live
broker revocation integration, and durable permission-store service remain outstanding. The
[application filesystem resource resolver](application-resource-resolution.md) now supplies the
stable UUID/object/generation identity required before grant issuance, and the
[application resource capability adapter](application-resource-capabilities.md) mints a fresh
rights-reduced endpoint from each authorization. The
[application selection transaction](application-selection-transactions.md) now couples prepared
grant policy to endpoint delivery without spending authority on failure.

The implementation provides:

- stable application grant subjects derived from verified launch authorization;
- filesystem resource identities that survive provider restart but reject inode reuse;
- kind-specific, non-executable file and directory rights;
- one-shot, session, and persistent grant scopes;
- bounded grant IDs and globally ordered record revisions;
- atomic one-shot consumption, explicit revocation, application reset, and resource removal;
- retained revocation tombstones so stale active records cannot be replayed;
- strict checkpoint restoration with duplicate and counter validation; and
- a canonical checksummed `NSPG` version 1 record format.

The host suite covers codec corruption, rights escalation, session crossing, authenticated updates,
publisher and installation changes, transient identities, revocation/reset, stale checkpoints, and
fixed-capacity exhaustion. The QEMU application probe exercises canonical persistence, exact-rights
authorization, escalation denial, and revocation in the freestanding runtime.

## Grant subject

A grant binds to:

```text
user
application identifier
publisher identity
accepted signing lineage
trust class
installation identity and scope
```

Package generation, component, process, and session are not part of the stable subject. This lets an
authorized update or application relaunch restore eligible grants. The current installation identity
is retained so uninstall/reinstall does not silently recover the previous installation's authority.
A publisher or signing-lineage change is a different subject.

Session identity is stored only for one-shot and session grants. Persistent grants require a
non-transient trust class and installation scope. This prevents ad hoc or transient applications from
silently accumulating durable authority.

## Stable resource identity

A persistent pathname is not a resource identity. `ApplicationResourceIdentity` contains:

```text
filesystem UUID
filesystem object ID
object generation
resource kind: file or directory
```

The filesystem UUID and object ID preserve identity across provider restarts and namespace spelling
changes. The object generation prevents a deleted inode number from retargeting an old grant when the
number is reused. Provider-process generation is deliberately excluded because restarting the same
filesystem service must not invalidate the underlying resource.

NullFS exposes this tuple through optional authenticated `RESOLVE_IDENTITY` and `RESTORE_IDENTITY`
filesystem operations. `ApplicationResourceResolver` pins the provider response to an independently
expected volume UUID, rejects symbolic links, and requires the exact file/directory kind admitted by
the portal. `ApplicationResourceRestorer` pins the selected mount to that same UUID before NullFS
revalidates the stored object ID, generation, kind, and link count and returns a fresh live node.

## Rights and scopes

File grants may contain only `READ` and `WRITE`. Directory grants may additionally contain `CREATE`,
`REMOVE`, and `ENUMERATE`. No grant conveys execute authority, ambient path lookup, or access to the
containing directory of a selected file.

The scopes are:

- `Once`: bound to the current session and atomically replaced by a `Consumed` tombstone when an
  immediate authorization commits or a prepared portal capability delivery succeeds;
- `Session`: reusable only by the exact verified subject in the issuing session; and
- `Persistent`: reusable by the exact verified subject in later sessions, subject to current policy
  and resource resolution.

Authorization returns only the requested subset of stored rights. The returned policy object is not
a kernel capability. `ApplicationResourceBroker::mint` now consumes that object as policy input for
a fresh channel pair: the broker retains `RECEIVE | WAIT`, a transfer-staging peer has
`SEND | TRANSFER`, and the application receives exact `SEND`. The retained authority gates generic
filesystem operations against the authorized file/directory rights.

Portal completion uses `PreparedApplicationGrant` instead of immediately mutating the store. New
grant records and one-shot consumption revisions are fully checked and reserved while the store is
mutably borrowed, but are committed only after the selected endpoint move succeeds. Dropping the
prepared grant leaves records and monotonic counters unchanged.

## Revocation and persistence

The store has 64 fixed slots. Grant IDs and revisions are monotonic, nonzero, and never wrap. Every
mutation consumes a new revision. Revocation, one-shot consumption, application reset, and resource
removal retain a tombstone in the original slot. New approval after revocation receives a new grant ID
and does not overwrite the tombstone.

`NSPG` version 1 encodes each current record in exactly 128 bytes with fixed little-endian fields,
zeroed reserved bytes, canonical state/scope relationships, and a CRC32C checksum. Checkpoint restore
requires the next grant-ID and revision counters to be strictly newer than every record, rejects
duplicate IDs or revisions, and permits at most one active record for a subject/resource pair.

The allocation-free persistence foundation defines a fixed `NSGC` checkpoint containing the complete
record set and monotonic counters. Two checkpoint slots are paired with two checksummed `NSGS`
selectors. A commit writes and synchronizes the inactive checkpoint before publishing and
synchronizing the inactive selector. Recovery considers only selector-referenced checkpoints, chooses
the newest valid selector, and falls back to the preceding committed selector if the latest
checkpoint is corrupt. An unreferenced checkpoint is never a committed store.

A production permission-store service must fail-stop on outcome-unknown synchronization and compact
tombstones only while preserving rollback/replay protection. Durable selection completion now
coordinates reply transfer with this persistence protocol: it commits the prepared mutation after
the atomic endpoint move, retains the broker until selector synchronization succeeds, and closes the
broker plus requires a portal-generation fail-stop if storage fails after transfer. Recovery decides
whether an outcome-unknown selector became durable; the request is never retried.

The live storage adapter now maps the two checkpoint and two selector slots into one exact 16,640-byte
file reached through an already-authorized writable filesystem session. New files are formatted only
from exact size zero; existing files must already have the exact layout size. Checkpoint and selector
I/O uses one private shared-memory attachment and every persistence barrier maps to filesystem
`SYNC`; checkpoint transfers are split into 4,096-byte requests to respect the public NullFS write
limit without weakening the selector commit boundary. Host failure injection covers every
publication stage, and the NullFS QEMU probe exercises format, two commits, latest-selector recovery,
and cleanup against the real service. Durable portal-reply coordination is implemented; a deliberate
post-write process-crash gate remains future work.

## Next steps

1. **Implemented foundation:** close live brokers immediately when grants, sessions, providers, or
   resources become invalid.
2. **Implemented foundation:** capability-separated portal/compositor transport, rooted trusted
   picker policy, and authenticated live-filesystem adapter; a concrete compositor renderer remains.
3. **Implemented foundation:** transactional checkpoint container, recovery protocol, live
   NullFS-file binding, and durable reply coordination; process-crash injection, tombstone
   compaction, and administrative transport remain.
4. Extend the same policy foundation to drag-and-drop and share transfers.
