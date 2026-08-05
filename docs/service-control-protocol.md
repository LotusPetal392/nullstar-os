# Service control protocol

`NSVC` v1 is the allocation-free service-control contract and native endpoint transport. It defines canonical 64-byte request and response records for listing services, inspecting state, and requesting start, stop, or restart transitions.

PID 1 temporarily owns separate stable observation and mutation endpoints for its four directly supervised services. The observation endpoint serves `sv list` and `sv status SERVICE` and returns `AccessDenied` for mutations. The mutation endpoint implements generic `sv restart SERVICE` plus live `sv start logging` and `sv stop logging`; filesystem `Start` and `Stop` return `Unsupported`. A separately restartable service manager, cross-reboot desired state, activation, and declarative definitions remain future work.

## Scope

Version 1 covers five operations:

| Value | Operation | Request meaning |
| ---: | --- | --- |
| `1` | `List` | Return at most one service record beginning at `cursor` |
| `2` | `Status` | Return the current record for one service ID |
| `3` | `Start` | Request desired state `Running` for one service ID |
| `4` | `Stop` | Request desired state `Stopped` for one service ID |
| `5` | `Restart` | Request a managed restart of one service ID |

The codec represents requests and responses only. In particular, a successfully decoded `Start`, `Stop`, or `Restart` request does not mean that a manager accepted or completed the transition.

## Exact `NSVC` v1 wire contract

Every request and response is exactly 64 bytes. There is no shorter form, extension trailer, native Rust-layout encoding, or pointer-sized field.

| Byte range | Size | Field | Encoding and constraint |
| --- | ---: | --- | --- |
| `0..4` | 4 | magic | ASCII `NSVC` (`4e 53 56 43`) |
| `4..6` | 2 | version | little-endian `u16`, exactly `1` |
| `6` | 1 | kind | `1` request, `2` response |
| `7` | 1 | operation | `1` list, `2` status, `3` start, `4` stop, `5` restart |
| `8..16` | 8 | request ID | little-endian nonzero `u64` |
| `16..32` | 16 | service ID | optional UUIDv4 in RFC/network byte order; all zero means absent |
| `32..40` | 8 | provider generation | optional little-endian `u64`; `0` means absent and every present value is nonzero |
| `40..44` | 4 | cursor | little-endian `u32`; list request cursor echoed by its response, otherwise `0` |
| `44..48` | 4 | next cursor | little-endian `u32`; next page in a successful list-record response, otherwise `0` |
| `48` | 1 | observed state | value below; `0` means absent |
| `49` | 1 | desired state | value below; `0` means absent |
| `50` | 1 | status | response status below; requests require `0` |
| `51` | 1 | flags | v1 defines no flags; must be `0` |
| `52..64` | 12 | reserved | all zero |

All multibyte integers are little-endian. UUID bytes are not integer-swapped. A present service ID must be a canonical non-nil UUIDv4; the all-zero representation is reserved exclusively for absence.

Decoding is strict. It rejects a record whose size is not 64 bytes; incorrect magic or version; an unknown kind, operation, state, or status; request ID zero; a malformed present service ID; a noncanonical kind/operation/field combination; nonzero flags; or any nonzero reserved byte.

## Lifecycle and result values

Observed state reports what the manager currently knows:

| Value | State | Meaning |
| ---: | --- | --- |
| `0` | absent | No state is carried in this record |
| `1` | `Defined` | Known to policy but not yet activated |
| `2` | `Activating` | Activation work is in progress before startup |
| `3` | `Starting` | A provider incarnation is starting |
| `4` | `Ready` | Running and its readiness contract is satisfied |
| `5` | `Degraded` | Running but not fully healthy |
| `6` | `Stopping` | An orderly stop is in progress |
| `7` | `Terminating` | Forced termination or final cleanup is in progress |
| `8` | `Stopped` | Not running after an orderly or policy-selected stop |
| `9` | `Failed` | Startup, runtime, or shutdown failed |
| `10` | `Quarantined` | Automatic restart is suppressed by policy |

Desired state is deliberately smaller:

| Value | State | Meaning |
| ---: | --- | --- |
| `0` | absent | No desired state is carried in this record |
| `1` | `Stopped` | Policy wants the service stopped |
| `2` | `Running` | Policy wants the service running and ready |

A restart is an operation, not a persistent desired state. The current supervisor commits restart intent by moving the existing generation to `Terminating`, retains desired state `Running`, and later publishes the replacement as `Starting` and `Ready` under the next generation.

Response status values are:

| Value | Status | Meaning |
| ---: | --- | --- |
| `0` | success | The response carries a successful result |
| `1` | `NotFound` | The requested service or list position does not exist |
| `2` | `AccessDenied` | The control endpoint or caller policy does not authorize the operation |
| `3` | `InvalidState` | The requested transition is invalid from the current lifecycle state |
| `4` | `Busy` | A conflicting transition or bounded operation is already in progress |
| `5` | `Exhausted` | A bounded manager resource needed for the operation is exhausted |
| `6` | `Unsupported` | The receiver does not implement the requested operation |

The codec validates and encodes these statuses. PID 1 uses `NotFound` for unknown services or list cursors, `AccessDenied` when mutation reaches the observation endpoint, `Unsupported` for filesystem `Start` and `Stop`, `Busy` for a restart already pending or a failed signal request, `Exhausted` when a stop transition token cannot advance, and `InvalidState` when a transition is not valid for the target.

## Canonical requests

All requests have kind `Request`, a nonzero request ID, status zero, absent observed and desired state, no provider generation, next cursor zero, and zero reserved bytes.

| Operation | Service ID | Cursor |
| --- | --- | --- |
| `List` | absent | page cursor; `0` starts enumeration |
| `Status` | present | `0` |
| `Start` | present | `0` |
| `Stop` | present | `0` |
| `Restart` | present | `0` |

The `Start`, `Stop`, and `Restart` requests do not carry caller-selected lifecycle states or provider generations. The receiver owns transition policy and generation assignment. Status zero on a mutation response means the receiver committed that operation's desired state: `Start` and `Restart` report desired state `Running`, while `Stop` reports desired state `Stopped`. The observed state may still describe an in-progress transition such as `Activating`, `Starting`, or `Stopping`.

## Canonical responses and correlation

Every response echoes the request's nonzero request ID and operation. A list response also echoes the request cursor exactly. A response to `Status`, `Start`, `Stop`, or `Restart` echoes the requested service ID exactly. A client must reject a response with the wrong kind, request ID, operation, list cursor, or target service ID.

Request IDs are correlation values, not authority. A client must not reuse an ID while that request is outstanding. Each request has at most one terminal response; unknown, duplicate, stale, or otherwise unmatched responses are rejected rather than guessed into another call. The codec validates one response against one request. The current native client permits one outstanding request per `ControlExchange`; higher-level clients impose finite request, page, and yield budgets.

On success, a target-operation response carries the service's observed and desired states and a state-consistent provider generation. Its cursor and next cursor are zero. `Start` and `Restart` success require desired state `Running`; `Stop` success requires desired state `Stopped`; `Status` may report either desired state. A nonzero-status response carries absent states, no provider generation, and zero cursors; it still echoes the target service ID for correlation.

A provider generation identifies one supervised incarnation. It is not a PID and grants no authority. `Defined` forbids a generation. `Activating`, `Starting`, `Ready`, `Degraded`, `Stopping`, `Terminating`, and `Failed` require a nonzero generation. `Stopped` and `Quarantined` permit either an absent or nonzero generation so a manager can retain the most relevant incarnation identity.

## Paginated one-record list

`List` is deliberately bounded to one record per request and one response per page:

1. The client sends `List` with no service ID and a cursor. Cursor `0` starts enumeration.
2. Every list response echoes that cursor, including a failure or terminal empty page.
3. A successful response carrying a service ID contains exactly one service record: service ID, state-consistent provider generation, observed state, desired state, and a next cursor.
4. A nonzero next cursor must be greater than the echoed cursor and can be submitted in a new request with a fresh request ID.
5. Next cursor `0` means that the returned record is the final record.
6. A successful response with no service ID, no generation, absent states, and next cursor `0` is an empty terminal page.
7. A failure response has no service record or next cursor and carries one nonzero status.

Cursors are receiver-issued, monotonically ordered tokens. Clients do not generate or perform arithmetic on them and must not assume that they are indexes, service IDs, generations, or stable snapshots. Strictly advancing nonzero tokens prevent direct pagination cycles, and the native client also imposes page, request, and cooperative-yield budgets. The current four-record PID 1 registry is static, but a future manager must specify snapshot behavior when definitions and lifecycle state can change across pages.

## Native transport and authority

The wire record remains exactly 64 bytes. The native request transport adds one envelope capability: the client creates a fresh private reply endpoint, transfers an empty exact-`SEND` handle with the request, retains exact `RECEIVE`, and closes it on every terminal path. PID 1 owns exact-`RECEIVE` duplicates of the separate stable observation and mutation ingresses. A response carries no capability, is sent exactly once through the private reply endpoint, and must have a nonzero kernel-stamped server PID.

Possession of an exact-`SEND` observation grant is the current source of authority to issue `List` and `Status`. Knowing a service ID, provider generation, cursor, request ID, operation number, executable path, or PID grants nothing. The observation grant remains caller-owned while each exchange owns only its private reply receiver. Malformed packets are consumed without terminating PID 1, and malformed or failed paths close transferred reply handles.

Observation and mutation authority are separate endpoint objects. The observation client refuses to originate `Start`, `Stop`, or `Restart`; if a valid mutation packet reaches the observation ingress through another client, PID 1 replies with `AccessDenied` without consulting service state. A mutation client refuses `List` and `Status`. The mutation endpoint accepts generic `Restart` plus logging `Start` and `Stop`; filesystem `Start` and `Stop` return `Unsupported`.

A successful `Restart` response means PID 1 committed restart intent and accepted responsibility for replacing the service; it does not mean the replacement is ready. The response reports the old generation as `Terminating` with desired state `Running`. Controlled replacement uses zero failure backoff, does not consume the automatic failure-restart budget, and assigns the next manager-owned generation to the replacement. Logging keeps restart intent pending across bounded replacement startup until a later bounded mutation pass observes the ingress empty, so queued duplicates receive `Busy`; the still-synchronous filesystem replacement paths retain their existing bounded-queue drain. PID 1 first requests cooperative logging termination, then escalates after a bounded grace period to uncatchable, unblockable signal 9 so an ignored or masked termination request cannot stall convergence indefinitely.

A successful logging `Stop` response is sent after desired state becomes `Stopped` and process-group termination is accepted, but before final exit. PID 1 withdraws producer and observer routes before servicing later route resolutions. Existing delegated provider handles are not revoked, and the old generation's source endpoints remain owned until final child status; they are then closed and never reused. If cooperative termination does not produce final child status within the bounded grace period, PID 1 sends signal 9 to the supervised direct child through the existing `kill` syscall. A successful logging `Start` response commits desired state `Running` before spawn or readiness. PID 1 launches at most one generation, accepts readiness only for the current starting child, and publishes fresh route objects under the next manager-owned generation.

Once a mutation request has been sent, a missing, malformed, or untrusted response—or a local reply-handle cleanup failure after enqueue—is an outcome-unknown condition. The `sv` client never retries automatically. A confirmed commit followed by console-output failure is reported separately from outcome unknown. The caller may later use the observation endpoint to determine the current state, but observation cannot prove whether every transient effect of an earlier request occurred.

PID 1's temporary registry contains `logging`, `nullfs`, `tmpfs`, and `vfs` in stable list order. Logging desired state can now be changed live and remains in PID 1 memory across controlled exits and replacement; filesystem desired state remains `Running`. Controlled stop is distinct from failure, stop can suppress a pending restart, explicit start can re-arm bounded policy, and failed signal delivery can be rolled back. Every service reports the manager-issued generation assigned to that startup attempt rather than deriving lifecycle identity from its process ID. Stopped, backoff, and quarantined views omit generation where allowed by the wire contract.

The standalone `/sv` binary uses observation authority at handle `1` for `sv list` and `sv status SERVICE`, and mutation authority at handle `2` for `sv start SERVICE`, `sv stop SERVICE`, and `sv restart SERVICE`. The trusted `ush` builtins receive `SEND | DUPLICATE` observation authority at handle `2` and mutation authority at handle `3`, duplicate each down to exact `SEND` for one operation, and never receive `TRANSFER`. Pathname identity is not authorization.

Each starting logging generation has a bounded readiness budget. Expiry forces that child to exit with signal 9; normal final-status handling then charges the bounded restart/backoff policy or moves the service to `Failed`, so a live child that never declares readiness cannot hold the service in `Starting` indefinitely.

## Explicit exclusions

The current integration adds none of the following:

- a separately restartable service-manager process;
- filesystem live `start` or `stop`, cross-reboot desired-state persistence, or persistent administrative enable/disable policy;
- service activation, dependency resolution, or a definition loader;
- service launch, stop, readiness, health, or restart-policy changes beyond PID 1's existing hard-coded supervision;
- channel activation or queued activation semantics;
- policy-populated or partially visible registries;
- NSWP negotiation or a general IDL-generated binding;
- new lifecycle syscalls, endpoint forms, or attachment primitives. The existing signal ABI gains uncatchable, unblockable signal 9 for bounded supervisor escalation.

Those remain later lifecycle and service-management work. The present native deliverable is capability-separated observation and mutation over ordinary endpoints, backed by the allocation-free fixed-record contract and its host-testable canonical encoding, decoding, validation, correlation, and desired-state transition rules. Logging provides the first live nonblocking start/stop convergence pilot. Filesystem services additionally require generation-checked provider offlining, deterministic failure of pending proxy requests, fresh generation endpoints, and an orderly writable-NullFS quiesce/flush boundary so old or uncertain operations are never replayed into a replacement.

`NSVC` v1 is intentionally closed: unknown operations, states, statuses, flags, and nonzero reserved fields are rejected. Adding any of them requires a later protocol version and explicit version negotiation or binding rather than silently changing the meaning of a v1 record.
