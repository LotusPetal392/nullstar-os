# Service route protocol

The `service-route` crate defines NullStar's generic, allocation-free userspace service-route
contract. It provides stable service and role identities, an exact wire representation, fixed-capacity
publication storage, authorization ordering, and generation-bound endpoint issuance. The native
adapter in `userspace::service_route` carries that contract over the current endpoint ABI.

`NSRT` is a route-control protocol. It does not carry or interpret the protocol spoken after a route
is accepted. In particular, the logging broker does not parse NSWP packets or log records; it only
validates `NSRT`, applies route policy, and transfers an endpoint capability.

## Identities

A route key is the pair:

```text
(ServiceId, RoleId)
```

- `ServiceId` is a non-nil UUIDv4 in RFC/network byte order. Knowledge of the UUID identifies a
  service but grants no authority.
- `RoleId` is a nonzero `u32`. Roles under one service are independently authorized routes.
- `ProviderGeneration` is a nonzero `u64` identifying one published provider incarnation.

The logging assignments are:

| Item | Value |
| --- | --- |
| Logging service ID | `7cbd3f65-50a6-4c30-b195-9fbed633da43` |
| Producer role | `1` |
| Observer role | `2` |

The service ID is distinct from the logging NSWP protocol family ID
`7db79cd9-c685-400f-b9f1-55d89b8e8a8a`. A service route selects authority and a current provider;
NSWP negotiation selects the application protocol and compatible version on the returned endpoint.

## Exact `NSRT` v1 wire contract

Every request and response is exactly 40 bytes. There is no shorter form, extension trailer, or
Rust-layout encoding.

| Byte range | Size | Field | Encoding and constraint |
| --- | ---: | --- | --- |
| `0..4` | 4 | magic | ASCII `NSRT` (`4e 53 52 54`) |
| `4..6` | 2 | version | little-endian `u16`, exactly `1` |
| `6` | 1 | kind | `1` request, `2` accepted, `3` failure |
| `7` | 1 | status | kind-dependent value below |
| `8..24` | 16 | service ID | UUIDv4 bytes in RFC/network order |
| `24..28` | 4 | role ID | little-endian nonzero `u32` |
| `28..32` | 4 | reserved | all zero |
| `32..40` | 8 | provider generation | little-endian `u64`, kind-dependent value below |

The canonical kind combinations are:

| Kind | `kind` | `status` | `provider_generation` | Capability attachment |
| --- | ---: | ---: | ---: | --- |
| Request | `1` | `0` | `0` | exactly one reply endpoint with exact `SEND` rights |
| Accepted | `2` | `0` | nonzero | exactly one provider ingress endpoint with exact `SEND` rights |
| Failure | `3` | `1`, `2`, or `3` | `0` | none |

Failure status values are:

| Status | Meaning |
| ---: | --- |
| `1` | `Unauthorized` |
| `2` | `Unavailable` |
| `3` | `IssuerCapacity` |

Decoding is strict. It rejects a length other than 40 bytes, incorrect magic or version, unknown kind
or status, nonzero reserved bytes, a noncanonical service UUID, role zero, and every invalid
kind/status/generation combination. Accepted and failure responses echo the complete requested route
key, and the client rejects a mismatch.

All integer fields are little-endian. UUID bytes are not integer-swapped; they retain RFC/network
order exactly as stored by `ServiceId`.

## Capability exchange

A client receives a route grant with exact `SEND` rights. The grant reaches a broker ingress already
bound to one route key; the request cannot use its payload to select a different route.

Resolution proceeds as follows:

```text
client                         userspace broker                 provider
  |                                   |                            |
  | NSRT Request                      |                            |
  | + fresh reply, exact SEND ------->|                            |
  |                                   | authorize and resolve      |
  |<------ NSRT Accepted              |                            |
  |        + ingress, exact SEND      |                            |
  |                                                                |
  |------------- service protocol packets ------------------------>|
```

The client creates a fresh empty reply endpoint object, retains an exact-`RECEIVE` handle, and
transfers an exact-`SEND` handle with the request. The broker sends at most one response and closes
its reply handle after that attempt. The client treats its first received response as terminal and
closes the private receiver.

On acceptance, the broker duplicates the current published provider source with disposable
`SEND | TRANSFER` rights and transfers exact `SEND` authority to the client. The accepted provider
endpoint must be a different object from the private reply endpoint. A failure response carries no
capability. Thus the only capability-bearing request and reply each transfer exactly one capability;
the current endpoint ABI cannot transfer more than one capability in a message.

The kernel stamps a nonzero sender PID on received messages. The broker authorizes that stamped PID,
not a PID claimed in `NSRT`; there is no caller-identity field in the 40-byte record. The current
policy hook receives the PID and route key.

## Authorization and publication ordering

The broker applies operations in this order:

1. decode and validate the request, its granted route key, sender PID, and reply capability;
2. authorize the sender for that route key;
3. only after authorization, consult route availability;
4. issue the current generation's endpoint or return a canonical failure.

Authorization therefore precedes availability disclosure. A denied caller receives `Unauthorized`
whether or not a provider is currently published.

Publication storage is fixed-capacity and allocation-free. Each slot is permanently associated with
its first route key. Publishing an existing key requires a generation strictly greater than the
retained generation. Withdrawal requires the exact active generation and leaves a tombstone, so the
same or an older generation cannot be republished later. Tombstones count against the table's
lifetime distinct-key capacity.

The native table owns a stable provider source with exactly `SEND | DUPLICATE | TRANSFER` rights.
Issuance duplicates that source; it never gives the source itself to a client. Replacing a
publication returns the displaced source to its owner so it can be closed deliberately.

## Stable route authority and provider generations

Stable route authority and provider endpoint authority are separate:

- a stable route grant authorizes requests for one `(ServiceId, RoleId)` and remains meaningful as
  providers are replaced;
- a successful resolution returns an exact-`SEND` handle to the currently published provider
  generation's ingress object;
- each provider generation uses fresh ingress endpoint objects, so an endpoint issued for an old
  generation is not silently rebound to its replacement.

The logging producer and observer are separate stable routes. They have distinct broker ingresses,
policy decisions, publications, and generation-specific provider ingress objects. Within one
provider generation, clients resolving the same role receive handles to that role's shared ingress;
they do not receive a fresh provider endpoint object per resolution.

Fresh objects provide generation isolation, not global revocation. Closing or replacing the
broker's published source prevents future resolutions from selecting the old generation, and a new
provider cannot receive packets sent to the old ingress object. However, NullStar has no primitive
that invalidates every exact-`SEND` handle already delegated to clients. An old object remains alive
while any handle or queued transfer retains it. Callers must compare the accepted generation, bind
subsequent protocol state to it, and resolve again after replacement rather than assuming old
handles were globally revoked.

## Current logging deployment

PID 1 is the temporary broker, publication owner, and generation authority for the logging producer
and observer routes. It owns an allocation-free monotonic provider-generation sequence independent
of process IDs. Every `/logging-service` startup attempt consumes a nonzero generation, including an
attempt that fails before readiness. Every attempt also receives a fresh flat job while its child is
still behind the launch barrier. PID 1 retains exact `SIGNAL | WAIT`, uses the job for forced cleanup,
and drains all exits to `ECHILD` before closing route sources or starting a replacement. The current
contract provides no durable cross-boot persistence.

PID 1 creates one private managed-start channel and grants the logging-service child its exact
receive-only endpoint as bootstrap handle 1, with no other initial capability. It sends a role-tagged
`NSPC` envelope containing the readiness, producer, observer, and moved kernel early-log authorities,
then the required `NSPD` sections and canonical `NSPX` end record. The shared receiver pins the
kernel-stamped sender as PID 1, validates service identity and arguments, adopts authorities by role,
and obtains the nonzero provider generation from authenticated launch data. The accepted generation
identifies the collector, binds `NSLS` sessions and NSWP negotiation, and is the generation PID 1
publishes on both logging routes. PID 1 reacquires the transfer-only early-log reader for every
replacement generation.

This remains an integration step, not the intended permanent service manager, policy engine, or
global service namespace. PID 1 delegates only selected stable role grants. A restartable service
manager must eventually own the sequence and receive its current state across manager replacement.

After route resolution, logging clients perform `NSLS` session bootstrap and NSWP negotiation
directly with `/logging-service`. The broker is not on that data path and never parses, queues, or
replays NSWP negotiation, `Emit`, collector-statistics, or history packets.

## Exhaustion and retry rules

All route resources are bounded:

- route tables have a compile-time slot count, and withdrawn-key tombstones retain slots;
- the kernel currently permits 32 live endpoint objects system-wide and eight queued messages per
  endpoint;
- each in-progress resolution creates a private reply endpoint object;
- each provider generation creates fresh role ingress endpoint objects;
- retained old-generation handles or queued capability transfers can keep old endpoint objects alive.

Endpoint creation or provider duplication can therefore fail under object or handle pressure even
when the route table itself has capacity. `IssuerCapacity` reports failure to issue a provider
capability; it is not evidence that the service is absent. Clients must bound retries and release
private reply and stale provider handles promptly.

Route resolution does not make application operations replay-safe. In particular, logging `Emit` is
a one-way NSWP operation. If the client cannot determine whether a successfully submitted emit was
processed before a provider failure, it must not replay that record automatically on a newly
resolved generation. Replaying could duplicate a retained event, while not replaying may lose one;
the current protocol provides no acknowledgement that resolves that uncertainty. `Reliable` logging
permits retry only when local queue backpressure proves the send was not accepted; it does not imply
processing, journal commit, durable storage, or safe replay after an uncertain send.
