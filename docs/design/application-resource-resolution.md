# Application filesystem resource resolution

## Status

This document describes the implemented bridge from a live, generation-scoped filesystem node to
the stable identity used by application permission grants. NullFS now exposes its authoritative
volume UUID, inode number, inode generation, and node kind through an optional generic filesystem
operation. The application resolver pins that reply to the expected mounted volume and requested
resource kind.

The [application resource capability adapter](application-resource-capabilities.md) now mints and
rights-reduces a grant-bound broker endpoint from this policy identity. Restoring a stored identity to
a new live node, forwarding broker I/O, implementing the picker UI, and making the permission store
durable remain future work.

## Generic filesystem operation

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

Adding an optional operation preserves the version-1 `Request` and `Reply` layouts. Existing clients
and providers remain wire-compatible and do not infer support from protocol version alone.

## NullFS authority

NullFS resolves the opaque node through its current generation-tagged node map, then revalidates the
underlying allocated inode. Deleted, reclaimed, generation-mismatched, kind-mismatched, unlinked, or
otherwise stale nodes fail with `STALE_NODE`.

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

## Coverage

Host tests validate the exact 40-byte shape, mirrored node and kind fields, zero padding, malformed
identity rejection, volume mismatch, object generation preservation, symbolic-link denial, and kind
substitution. The freestanding NullFS probe resolves deterministic root, directory, and file inodes
through the real service and rejects both a wrong expected kind and wrong expected volume. The tmpfs
probe verifies unsupported providers cannot fabricate an identity.

## Next steps

1. Make selection completion failure-atomic across grant issuance, endpoint creation, and response
   transfer.
2. Implement rooted live-filesystem forwarding and resolve stored identities without accepting
   pathname or inode reuse.
3. Implement the portal service transport and trusted picker around these policy boundaries.
4. Persist grants and revocation state transactionally.
