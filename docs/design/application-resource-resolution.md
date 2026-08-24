# Application filesystem resource resolution

## Status

This document describes the implemented bidirectional bridge between live, generation-scoped
filesystem nodes and the stable identity used by application permission grants. NullFS exposes its
authoritative volume UUID, inode number, inode generation, and node kind and restores that complete
tuple to a fresh opaque node through optional generic filesystem operations. The application policy
layer pins both directions to the expected mounted volume.

The [application resource capability adapter](application-resource-capabilities.md) now mints and
rights-reduces a grant-bound broker endpoint from this policy identity and forwards rooted live I/O
from a restored node. Implementing active revocation, the picker UI, and durable permission storage
remain future work. Endpoint delivery and grant mutation are coordinated by the
[application selection transaction](application-selection-transactions.md).

## Generic filesystem operations

Filesystem protocol version 1 adds optional operation `RESOLVE_IDENTITY` (`20`). Its request has the
same canonical shape as `GET_ATTRIBUTES`: one nonzero node ID scoped to the current session and
provider generation, with all unrelated fields zero.

A successful reply contains one exact 40-byte `StableNodeIdentity`:

```text
filesystem UUID       16 bytes
provider object ID     8 bytes
object generation      8 bytes
node kind               2 bytes
reserved                6 bytes, zero
```

The UUID, object ID, and generation must be nonzero. Kind must be file, directory, or symbolic link.
The reply mirrors the opaque request node ID and kind in the ordinary reply header, carries no flags
or value, and zeroes the unused inline payload. A malformed or partially populated success is a
transport failure. Providers without persistent stable identity return `NOT_SUPPORTED`; tmpfs does
so explicitly through its existing unknown-operation path.

Optional operation `RESTORE_IDENTITY` (`21`) performs the inverse without accepting a pathname. Its
request places one exact canonical `StableNodeIdentity` in the first 40 bytes of the existing inline
request area, sets the inline length to 40, zeroes the remaining bytes, and leaves node IDs, flags,
offset, and bulk fields empty. A successful reply returns a current session-scoped opaque node ID and
kind and echoes the complete identity in its inline payload. Generic reply validation requires that
echo to match the request byte-for-byte before exposing the node.

Adding these optional operations preserves the version-1 `Request` and `Reply` layouts. Existing
clients and providers remain wire-compatible and do not infer support from protocol version alone.

## NullFS authority

NullFS resolves the opaque node through its current generation-tagged node map, then revalidates the
underlying allocated inode. Deleted, reclaimed, generation-mismatched, kind-mismatched, unlinked, or
otherwise stale nodes fail with `STALE_NODE`.

Restoration independently compares the submitted UUID with the selected superblock, reads the inode
named by the object ID, and requires the exact stored inode generation, kind, and a nonzero link
count. Only then does it intern a current opaque node. A wrong volume, freed inode, reused inode,
kind substitution, or unlinked object returns `STALE_NODE`; no directory traversal or remembered
pathname participates.

The returned stable fields come from independent authoritative state:

- filesystem UUID from the selected NullFS superblock;
- object ID from the core inode number, never the service's opaque node-map ID;
- object generation from the inode record; and
- kind from the validated inode.

Provider-process generation is deliberately absent. Restarting the service changes sessions and
opaque node IDs but not the stable identity of the same on-disk object. Reusing a freed inode number
advances its inode generation and therefore cannot retarget an old permission record.

## Authentication and application validation

The resolver is not a pathname query. Its input is a typed node valid only for the live filesystem
session. The portal must obtain that session from its authenticated filesystem/VFS route. Session
bootstrap gives the selected provider the sole transferred send authority for the client's private
reply endpoint; subsequent replies must match request ID, session, provider generation, operation,
protocol version, and canonical payload shape.

`ApplicationResourceResolver` also requires an expected filesystem UUID supplied by trusted mount
selection. It never learns the expected UUID from the reply it is validating. Resolution fails if:

- the provider reports a different volume UUID;
- the stable record is malformed;
- the object is a symbolic link or another unsupported kind; or
- the provider kind differs from the file/directory kind admitted by the portal request.

Only after those checks does it construct `ApplicationResourceIdentity`. The resulting value is
policy identity, not live authority.

`ApplicationResourceRestorer` requires the same independently trusted mounted-volume UUID and rejects
a stored identity for another volume before transport. NullFS then performs the authoritative tuple
validation above. `RESTORE_IDENTITY` is never exposed through the application-facing rooted broker;
the forwarding constructor reads the immutable grant identity itself and uses restoration only to
establish that broker's root.

## Coverage

Host tests validate the exact 40-byte shape, mirrored node and kind fields, zero padding, malformed
identity rejection, volume mismatch, object generation preservation, symbolic-link denial, kind
substitution, and full request/reply echo binding for restoration. The freestanding NullFS probe
resolves and restores deterministic resources through the real service, rejects wrong generations,
kinds, and volumes, and uses the restored file as the root of live grant-backed forwarding. The tmpfs
probe verifies unsupported providers cannot resolve or restore persistent identity.

## Next steps

1. Implement the portal service transport and trusted picker around these policy boundaries.
2. Persist grants and revocation state transactionally.
