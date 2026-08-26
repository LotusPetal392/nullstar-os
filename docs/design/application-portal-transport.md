# Application portal transport

The application portal transport turns the file-portal admission policy into an owned capability
boundary. It deliberately keeps application requests and trusted compositor gestures on different
kernel endpoint objects. The portal service receives both streams and uses only the kernel-stamped
sender process ID as transport identity.

This milestone supplies the transport and reply lifecycle. It does not supply the picker UI,
filesystem browsing policy, or crash-safe permission persistence.

## Startup authority

Authenticated startup configuration names two nonzero processes:

- the application manager, which may bind and unbind launched desktop process IDs to opaque
  `AuthorizedApplication` proofs; and
- the trusted gesture issuer, normally the compositor, whose process ID is fixed in
  `ApplicationPortalAdmission`.

The manager retains a `SEND | DUPLICATE | TRANSFER` application source and issues each application
an exact `SEND` endpoint. The compositor receives a separate `SEND` endpoint through a
`SEND | TRANSFER` startup source. The portal retains only `RECEIVE | WAIT` on both ingress objects.
Possession of the wrong source cannot cross the identity check: requests still require a live
manager binding for the kernel sender, and tickets still require the configured compositor sender.

## Request and gesture envelopes

The gesture ingress accepts exactly one canonical 96-byte `NSGT` v1 ticket and no capability. The
request ingress accepts exactly one canonical 64-byte `NSPR` v1 request plus one endpoint carrying
exact `SEND`. The application-side source retains `SEND | TRANSFER` only until the move-send, while
the application keeps the paired `RECEIVE | WAIT` endpoint. The reply endpoint received by the
portal must be empty and must not alias the portal request ingress.

Both receivers use a maximum-size IPC buffer before applying the exact protocol decoder. An
oversized but kernel-valid message is therefore consumed and rejected instead of remaining at the
head of the queue.

The request's ticket and identity fields are correlation data, not authority. Admission uses the
request message's kernel sender to select the manager-installed authorization and to consume the
matching compositor ticket.

## Reply ownership

After admission, `PendingApplicationPortalRequest` owns both the opaque admission proof and the
validated send-only reply endpoint. It can finish in exactly one of two ways:

- send a capability-free `Cancelled`, `Denied`, `InvalidRequest`, or `Unavailable` terminal
  response; or
- complete a `PreparedApplicationSelection`, atomically move-sending the selected resource endpoint
  before committing the prepared permission mutation.

Dropping a pending request closes its reply authority. A failed selected send still invokes the
selection transaction's existing rollback behavior, so neither an orphaned resource endpoint nor a
published grant remains.

## Coverage

The freestanding application launch probe mints both ingress pairs, verifies every reduced right,
rejects a non-manager binding attempt, installs a desktop binding, sends a gesture and request over
the real channels, and validates a capability-free terminal response. Existing selection coverage
continues to verify atomic selected-response transfer and permission rollback.

## Next steps

1. Implement the compositor-hosted trusted picker and its portal-service request lifecycle.
2. Persist permission mutations transactionally before treating persistent grants as durable.
3. Connect the transport sources to authenticated application-manager and compositor startup.
