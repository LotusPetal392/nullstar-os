# Application permission-store foundation

## Status

This document describes the implemented allocation-free policy foundation for application file and
directory grants. It does not yet provide a trusted picker, portal request protocol, live
file/directory broker endpoint, or durable permission-store service.

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

NullFS already maintains per-inode generations internally. The public filesystem protocol currently
does not expose that generation in `NodeAttributes`; therefore a future portal broker must add an
authenticated provider resolver before it can turn a stored identity back into live authority. It
must fail closed if the UUID, object ID, object generation, or resource kind differs.

## Rights and scopes

File grants may contain only `READ` and `WRITE`. Directory grants may additionally contain `CREATE`,
`REMOVE`, and `ENUMERATE`. No grant conveys execute authority, ambient path lookup, or access to the
containing directory of a selected file.

The scopes are:

- `Once`: bound to the current session and atomically replaced by a `Consumed` tombstone on the first
  successful authorization;
- `Session`: reusable only by the exact verified subject in the issuing session; and
- `Persistent`: reusable by the exact verified subject in later sessions, subject to current policy
  and resource resolution.

Authorization returns only the requested subset of stored rights. The returned policy object is not
a kernel capability; the future portal/provider adapter must use it to mint a fresh rights-reduced
broker endpoint for the exact resolved resource.

## Revocation and persistence

The store has 64 fixed slots. Grant IDs and revisions are monotonic, nonzero, and never wrap. Every
mutation consumes a new revision. Revocation, one-shot consumption, application reset, and resource
removal retain a tombstone in the original slot. New approval after revocation receives a new grant ID
and does not overwrite the tombstone.

`NSPG` version 1 encodes each current record in exactly 128 bytes with fixed little-endian fields,
zeroed reserved bytes, canonical state/scope relationships, and a CRC32C checksum. Checkpoint restore
requires the next grant-ID and revision counters to be strictly newer than every record, rejects
duplicate IDs or revisions, and permits at most one active record for a subject/resource pair.

The implementation does not yet define the durable checkpoint container, transaction protocol, or
tombstone compaction proof. A production permission-store service must persist records and counters
atomically and compact tombstones only while preserving rollback/replay protection.

## Next steps

1. Define the portal request/reply protocol and trusted window/user-gesture tickets.
2. Add authenticated filesystem resource resolution including UUID and inode generation.
3. Mint rights-reduced file and directory broker endpoints from successful selections or restored
   grants.
4. Persist checkpoint state transactionally and expose permission inspection, revocation, and reset.
5. Extend the same policy foundation to drag-and-drop and share transfers.
