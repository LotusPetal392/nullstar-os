# Application portal transport

The application portal transport turns the file-portal admission policy into an owned capability
boundary. It deliberately keeps application requests and trusted compositor gestures on different
kernel endpoint objects. The portal service receives both streams and uses only the kernel-stamped
sender process ID as transport identity.

This milestone supplies the transport and reply lifecycle. It does not yet supply the compositor
renderer or the live portal process integration.

Follow-on implementation now supplies the allocation-free policy beneath those integrations. The
trusted picker accepts only canonical entries returned for its current authenticated provider
directory and navigates by retained entry slot rather than application-provided path. The canonical
checksummed `NSPS` startup record pins distinct application-manager and compositor process IDs before
minting the separated ingress sources. The permission store has a two-checkpoint/two-selector commit
protocol described in [Application permission store](application-permission-store.md). Rendering and
live process launch remain separate integration work.

The filesystem-session adaptation is now implemented. It restores the current stable directory,
reads at most eight entries through a private shared-memory attachment, rejects malformed entries and
symbolic links, reopens every name relative to that directory, verifies the reply node ID, and then
resolves the complete stable resource identity before publishing a picker page. Selection feeds the
existing failure-atomic grant and endpoint preparation directly. The compositor renderer and live
portal process remain future integration work.

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
  before committing the prepared permission mutation; or
- durably complete that selection, retaining the broker until the resulting checkpoint and selector
  publication synchronizes.

Dropping a pending request closes its reply authority. A failed selected send still invokes the
selection transaction's existing rollback behavior, so neither an orphaned resource endpoint nor a
published grant remains. A durable persistence error occurs after the reply transfer, closes the
broker peer, and requires the portal generation to fail-stop and recover rather than retry.

## Coverage

The freestanding application launch probe mints both ingress pairs, verifies every reduced right,
rejects a non-manager binding attempt, installs a desktop binding, sends a gesture and request over
the real channels, and validates a capability-free terminal response. Selection coverage verifies
atomic selected-response transfer, permission rollback, post-transfer persistence failure handling,
and recovery of a durably completed one-shot grant.

## Next steps

1. Implement the compositor-hosted trusted picker and its portal-service request lifecycle.
2. Connect the transport sources to authenticated application-manager and compositor startup.
3. Add a deliberate process-crash gate around durable reply publication and recovery.
