# Application selection transactions

## Status

This document describes the implemented failure-atomic boundary for completing one admitted file or
directory portal selection. `PreparedApplicationSelection` owns a preflighted permission mutation,
one fresh broker endpoint pair, and the exact selected response until the response capability is
move-transferred successfully.

Endpoint creation, response binding, and transfer failures leave the permission store unchanged.
Successful transfer commits an already-reserved mutation that cannot fail. This is an in-memory
service transaction; crash-safe permission persistence remains a separate milestone.

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
return broker endpoint        discard prepared mutation
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
The prepared grant is committed immediately after that successful syscall with no remaining
fallible policy step.

## Coverage

Host tests prove that dropping prepared existing and newly issued one-shot grants preserves records
and counters, while commit produces the reserved tombstone revision. The freestanding application
probe sends first to a closed portal peer, confirms complete grant/counter rollback, retries the same
admitted selection successfully, validates the resulting consumed tombstone, and exercises the real
send-only application-to-broker channel.

## Remaining work

1. Add the live forwarding adapter with canonical request validation and a rooted node map.
2. Resolve stored identities back to current live nodes without pathname or inode-reuse confusion.
3. Close active brokers on grant revocation, session expiry, provider replacement, or resource
   removal.
4. Implement crash-safe transactional permission persistence.
5. Implement the portal/compositor transport and trusted picker UI.
