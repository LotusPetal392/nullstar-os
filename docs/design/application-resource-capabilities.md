# Application resource capability adapter

## Status

This document describes the implemented authority boundary between an authorized application grant
and one live file/directory broker endpoint. `ApplicationResourceBroker::mint` creates a fresh kernel
channel pair with distinct endpoint identities for every authorization. The broker keeps only
`RECEIVE | WAIT`; the portal stages one non-duplicable `SEND | TRANSFER` peer and moves it to the
application with exact `SEND`.

The adapter now defines endpoint ownership, portal binding, the generic-filesystem operation ceiling,
live forwarding to a dedicated provider session, restoration of its root from stable identity, and
active teardown from authoritative lifecycle events. It does not yet run a standalone broker
process.

## Capability topology

```text
permission authorization
          |
          v
fresh endpoint pair
  broker end: RECEIVE | WAIT
  staged peer: SEND | TRANSFER
          |
          | portal move-transfer, reduced
          v
application handle: SEND
```

The staged peer has no `DUPLICATE` or `RECEIVE` authority. The application handle has neither
`DUPLICATE`, `TRANSFER`, nor `RECEIVE`. The broker cannot send requests into its own ingress and the
application cannot consume another client's requests or delegate the broker endpoint directly.
Closing the broker end makes subsequent application sends observe peer closure instead of leaving an
unserviced shared mailbox alive.

The client endpoint speaks the existing generic filesystem protocol. `CONNECT` transfers a private
send-only reply endpoint, so an application needs only `SEND` on the broker ingress. Shared-memory
attachments carry their own transfer authority; the ingress itself does not need `TRANSFER`.

The portal's selected-response constructor now accepts the staged endpoint only when its immutable
authority copy matches the exact grant ID, revision, subject, stable resource, rights, and scope used
for the response, including the launch session that receives the live endpoint. Receiver-side
envelope validation additionally requires one kernel `Endpoint` with exact `SEND` rights. A
notification, a broader endpoint, a missing endpoint, or a capability attached to a terminal
response fails validation.

## Filesystem operation ceiling

`ApplicationResourceAuthority` maps generic filesystem operations to grant rights:

| Filesystem operation | Required grant authority |
| --- | --- |
| connect read-only, buffers, close, cancel, disconnect | session control |
| writable connect, sync | at least one mutation right |
| attributes | metadata access inherent in the endpoint |
| lookup | directory root plus `READ` |
| open | exact `READ`, `WRITE`, and/or `CREATE` implied by its flags |
| read | `READ` |
| write, append, truncate | `WRITE` |
| directory iteration | directory root plus `ENUMERATE` |
| create file/directory | directory root plus `CREATE`; truncating an existing file also needs `WRITE` |
| unlink/rmdir | directory root plus `REMOVE` |
| rename | directory root plus `CREATE | REMOVE` |

Unknown operations and `RESOLVE_IDENTITY` are denied. No operation maps to execute authority.
Malformed or contradictory operation flags fail before rights are considered.

The live forwarding layer additionally enforces the generic protocol's canonical wire shape, grant-
scoped session generation, attached-buffer ownership and bounds, and a broker-local opaque node
namespace. For a selected file, that namespace begins with only the selected file. For a selected
directory, every child node must originate from a relative lookup or directory entry beneath the
selected root; caller-supplied provider node IDs, absolute paths, `.` and `..` cannot enter the
mapping. Symbolic-link nodes are rejected at this boundary until a provider-independent rooted-link
policy exists.

## Live forwarding boundary

`ApplicationResourceForwarder` binds one immutable grant, one broker ingress, one exact selected or
restored provider node, and one dedicated provider session. Its restoration constructor reads the
identity from the immutable grant itself, so orchestration cannot substitute another same-kind node.
It validates each complete 184-byte request before policy authorization, translates only node IDs
found in its 64-entry table, preserves canonical provider statuses, and rewrites returned node IDs
and inline attributes into the grant-local namespace.

Application shared memory is not retransferred to the provider. The application transfers exact
`READ | WRITE` authority to the broker, which allocates one same-sized private mirror, attaches that
mirror to the provider session, and copies only the validated request range. Four mirrors of at most
4096 bytes are allowed. Writes and rename names copy inward before dispatch; reads and rewritten
directory entries copy outward only after a canonical successful provider reply. Mutation transport
failure becomes `OUTCOME_UNKNOWN`.

The generic `RESOLVE_IDENTITY` operation remains unavailable through application endpoints. Stable
identity is portal policy metadata, not an application-visible escape from the rooted node map.

## Grant and endpoint lifetime

The endpoint is live authority and the `NSPG` record is policy. A process cannot manufacture an
endpoint by copying grant metadata. `ApplicationResourceForwarderRegistry` owns up to eight active
forwarders and consumes committed grant-revocation, application-session-end, provider-replacement,
and resource-removal events. It matches the immutable grant revision, launch session, stable resource
identity, filesystem UUID, and provider generation as appropriate. Matching entries are removed and
their broker ingress, application reply endpoint, and private buffers are dropped before a
best-effort provider disconnect. The application therefore observes peer closure even when the old
provider is unavailable. Reauthorization creates a fresh endpoint pair.

A live authorization always records the launch session that received it. That runtime field is
separate from persisted grant scope: persistent policy can authorize a later launch, but an endpoint
from an ended launch session is still closed. A one-shot grant's `Consumed` tombstone deliberately
does not invalidate the endpoint whose successful transfer consumed it.

The [application selection transaction](application-selection-transactions.md) now preflights grant
issuance or authorization without changing the store, then owns that deferred mutation with both
endpoint sides and the response. Failed endpoint creation, response binding, or portal transfer
closes staged authority and leaves grant records and counters unchanged. A successful transfer
commits the reserved mutation, including a one-shot `Consumed` tombstone, with no remaining fallible
policy operation.

## Coverage

Host tests exercise file and directory operation matrices, canonical request/name/buffer validation,
writable-session denial, unsupported stable-identity queries, and open-flag escalation. Portal tests
validate missing, unexpected, wrong-kind, and over-righted attachments. The freestanding application probe creates a real endpoint,
checks both local rights sets and object identity, move-transfers the application side with exact
`SEND`, rejects receive authority on the application side and send authority on the broker side, and
delivers a message through the resulting one-way channel. It first forces transfer failure through a
closed portal peer and verifies that grant records and counters remain unchanged before retrying.
The full NullFS probe additionally binds a read grant to `welcome.txt`, connects through the broker,
attaches mirrored shared memory, reads live bytes, verifies broker-root attribute rewriting, commits
a user revocation, and confirms both provider disconnect and peer closure on the old application
endpoint. Host tests cover exact lifecycle matching and malformed lifecycle identifiers.

## Next steps

1. Implement the portal/compositor transport and trusted picker UI.
2. Persist grants and revocation state transactionally.
