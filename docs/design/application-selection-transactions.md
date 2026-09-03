# Application selection transactions

## Status

This document describes the implemented failure-atomic boundary for completing one admitted file or
directory portal selection. `PreparedApplicationSelection` owns a preflighted permission mutation,
one fresh broker endpoint pair, and the exact selected response until the response capability is
move-transferred successfully.

Endpoint creation, response binding, and transfer failures leave the permission store unchanged.
Successful transfer commits an already-reserved mutation that cannot fail. Durable completion then
synchronously publishes the complete store snapshot and returns the broker only after selector
publication succeeds.

## Completion sequence

```text
admitted portal request + stable resource
                  |
                  v
preflight grant ID, revision, slot, and one-shot consumption
        (permission store remains unchanged)
                  |
                  v
mint fresh broker/client endpoint pair
                  |
                  v
bind canonical response to exact grant and endpoint
                  |
                  v
kernel atomic move-send of client endpoint
          | success                 | failure
          v                         v
commit reserved grant         close both endpoint sides
          |                   discard prepared mutation
          v
write + sync inactive checkpoint
          |
          v
write + sync inactive selector
          | success                 | failure
          v                         v
return broker endpoint        close broker peer and fail-stop
                              portal generation; recover store
```

The mutable borrow held by `PreparedApplicationGrant` prevents another store operation from using
the reserved slot or counters before completion. All capacity, identity, rights, scope, session, and
counter-overflow checks happen during preparation. Commit therefore performs only fixed indexed
writes and counter updates.

## New and existing grants

`PreparedApplicationSelection::issue` covers a newly approved picker result. The grant ID, issuance
revision, store slot, and following counters are calculated without publishing a record. If delivery
fails, no record appears and neither counter advances.

`PreparedApplicationSelection::authorize` covers an existing active grant. Session and rights checks
run without mutating the record. Reusable session and persistent grants need no commit mutation. A
one-shot grant reserves its `Consumed` tombstone revision but remains active until delivery succeeds.

A newly issued one-shot selection combines both operations. Its response names the reserved
issuance revision, while successful completion stores the immediately following `Consumed`
tombstone revision. Failed completion publishes neither revision. This prevents a closed portal
peer, endpoint allocation failure, or response mismatch from silently spending the only use.

## Transfer ownership

The kernel move-send operation is atomic with respect to capability ownership. On success the source
handle is gone and the application receives exact `SEND`. On failure the source remains owned by the
portal; the typed send wrapper returns it, and selection cleanup closes it together with the broker's
`RECEIVE | WAIT` peer. No failed transaction leaves an orphaned live broker mailbox.

The response payload and capability are sent by one syscall. A selected payload therefore cannot be
enqueued without its endpoint, and the endpoint cannot be transferred without the matching payload.
The prepared grant is committed immediately after that successful syscall. Basic `complete` then
returns the broker for explicitly volatile callers. `complete_durable` instead retains the broker
while it publishes the resulting store through the two-slot persistence protocol.

A storage error after transfer is an outcome-unknown boundary: the application may already hold the
queued endpoint and selector synchronization may have reached durable media even if the call
reported failure. `ApplicationSelectionDurableCompletionError::PersistenceAfterTransfer` therefore
closes the broker peer and reports `requires_fail_stop()`. The current portal generation must stop,
recover the permission store, and must not retry that request. Recovery chooses the valid published
selector; it is the authority on whether the mutation committed durably.

## Coverage

Host tests prove that dropping prepared existing and newly issued one-shot grants preserves records
and counters, while commit produces the reserved tombstone revision. The freestanding application
probe sends first to a closed portal peer and confirms complete grant/counter rollback. It then
injects a checkpoint-write failure after successful endpoint transfer, validates the explicit
fail-stop error and closed broker peer, and proves the in-memory one-shot mutation cannot be reused.
Finally it completes against memory-backed persistence, recovers the exact commit and consumed
tombstone, and exercises the real send-only application-to-broker channel.

## Remaining work

The [application portal transport](application-portal-transport.md) now owns an admitted request's
reply endpoint and can complete this transaction directly on it.

1. Add a deliberate process-crash acceptance gate at each portal reply publication stage.
2. Implement and connect the compositor-hosted trusted picker UI.
