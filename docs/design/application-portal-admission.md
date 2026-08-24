# Application portal admission foundation

## Status

This document describes the implemented allocation-free admission and wire-protocol foundation for
file and directory portals. It connects a verified desktop application and a trusted user gesture to
one bounded picker transaction. It does not yet implement a compositor, portal service process,
picker UI, authenticated filesystem resolver, or resource-capability adapter.

The implementation provides:

- canonical fixed-size open-file, save-file, and select-directory requests;
- exact operation-specific rights and one-shot, session, or persistent scope requests;
- short-lived gesture tickets bound to one process, application installation, user session, parent
  surface, seat, and physical event sequence;
- authenticated issuer registration, duplicate event rejection, expiry tombstones, and atomic
  one-shot admission;
- opaque admitted-request proofs carrying the verified launch authorization;
- monotonic nonzero portal transaction IDs; and
- canonical terminal and selected responses whose capability cardinality is unambiguous.

The host suite covers codec corruption, invalid operation/rights combinations, issuer spoofing,
event cloning, process/application/session/surface mismatch, replay, expiry, transient persistence,
fixed-capacity exhaustion, transaction-ID exhaustion, cross-subject grant substitution, and response
capability cardinality. The QEMU application probe exercises the complete in-memory path from a
gesture ticket through grant-backed selected response validation.

## Trust boundary

The ticket number supplied in an application request is not authority. A portal service accepts a
ticket record only when the transport reports the configured trusted gesture issuer as the
kernel-stamped sender. The issuer process identity must itself arrive through authenticated service
startup; choosing an arbitrary process number at runtime would defeat the model.

The portal service must own `ApplicationPortalAdmission` privately and pass kernel-stamped sender
identities into both registration and admission. The module is policy code and cannot make an
untrusted process authoritative merely because that process can construct the same Rust data type.

Each `NSGT` version 1 ticket binds:

```text
ticket ID
target process ID
user and session
application and installation
parent surface
seat and physical event sequence
issue and expiry times
```

Every field except the issue time is nonzero. Expiry must be later than issue and no more than five
seconds later. Registration rejects future or already expired tickets, duplicate ticket IDs, and a
second ticket derived from the same seat/event pair.

## Request admission

An `NSPR` version 1 request is exactly 64 bytes and carries a nonzero request ID, registered ticket
ID, parent surface, operation, exact requested rights, and grant scope. Reserved bytes must be zero.

The operations admit these rights:

- `OpenFile`: `READ`, optionally with `WRITE`;
- `SaveFile`: `WRITE`, optionally with `READ`; and
- `SelectDirectory`: any nonempty subset of directory `READ`, `WRITE`, `CREATE`, `REMOVE`, and
  `ENUMERATE` rights.

No operation can request execute authority. Persistent scope is rejected before gesture consumption
for transient application trust or installation scope.

Admission requires the authenticated client process, verified application and installation, user
session, and parent surface to match the ticket exactly. A successful check assigns a monotonic
transaction ID and replaces the ticket with a consumed tombstone in one mutation. Failed identity
checks leave the ticket available; an expired ticket becomes an expiry tombstone. A consumed or
expired ticket can never be replayed, even if wall-clock input later moves backward.

The registry has 64 slots and intentionally does not compact consumed tickets. This makes replay
behavior explicit for the current bounded foundation. A production service needs a generation
lifecycle and reclamation proof before accepting an unbounded stream of gestures.

## Responses and grant binding

An `NSPS` version 1 response is exactly 64 bytes. Cancellation, denial, invalid-request, and
unavailable responses carry only the request ID and require no transferred capability. A selected
response additionally carries the portal transaction, grant ID and revision, resource kind, exact
rights, and scope, and requires exactly one transferred capability.

The selected-response constructor accepts only a grant authorization with the same stable
application subject, resource kind, rights, and scope as the admitted request. This prevents a broker
implementation from accidentally returning another application's grant, a directory for a file
operation, broadened/reduced rights, or a differently scoped authorization.

Capability cardinality does not yet validate the transferred object's type, rights, provider
generation, or stable resource identity. Those checks belong to the filesystem resource resolver and
endpoint adapter in the next increment.

## Next steps

1. Expose NullFS object generation through an authenticated filesystem resolver.
2. Resolve a picker selection to the exact filesystem UUID, object ID, generation, and kind.
3. Issue the grant and mint one rights-reduced resource endpoint with failure-atomic cleanup.
4. Implement the portal service transport using kernel-stamped sender identities and authenticated
   compositor startup authority.
5. Add a trusted picker UI and transactional permission persistence.
