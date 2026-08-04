# Service control protocol

`NSVC` v1 is the allocation-free service-control contract and native observation transport. It defines canonical 64-byte request and response records for listing services, inspecting state, and requesting start, stop, or restart transitions.

The current native integration is deliberately read-only. PID 1 temporarily owns a stable observation endpoint and exposes its four directly supervised services through `sv list` and `sv status SERVICE`. Valid mutation requests are decoded and receive canonical `AccessDenied` responses; they never change service state. A separately restartable service manager, live mutation authority, activation, and declarative definitions remain future work.

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

A restart is an operation, not a persistent desired state. A future manager may move through `Stopping` and `Starting` while retaining desired state `Running`.

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

The codec validates and encodes these statuses. The current PID 1 observation receiver uses `NotFound` for unknown services or list cursors and `AccessDenied` for every mutation request.

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

The wire record remains exactly 64 bytes. The native request transport adds one envelope capability: the client creates a fresh private reply endpoint, transfers an empty exact-`SEND` handle with the request, retains exact `RECEIVE`, and closes it on every terminal path. PID 1 owns an exact-`RECEIVE` duplicate of the stable observation ingress. A response carries no capability, is sent exactly once through the private reply endpoint, and must have a nonzero kernel-stamped server PID. The `sv` client additionally requires that PID to be PID 1 and validates exact request/response correlation.

Possession of an exact-`SEND` observation grant is the current source of authority to issue `List` and `Status`. Knowing a service ID, provider generation, cursor, request ID, operation number, executable path, or PID grants nothing. The observation grant remains caller-owned while each exchange owns only its private reply receiver. Malformed packets are consumed without terminating PID 1, and malformed or failed paths close transferred reply handles.

Observation and mutation authority are intentionally separate. The current observation client refuses to originate `Start`, `Stop`, or `Restart`; if a valid mutation packet reaches PID 1's observation ingress through another client, PID 1 replies with `AccessDenied` without consulting service state. No mutation endpoint is currently exposed.

PID 1's temporary registry contains `logging`, `nullfs`, `tmpfs`, and `vfs` in stable list order. Desired state is always `Running`. Logging reports its manager-issued generation; the other three services temporarily report their current process PID as a nonzero generation while starting or ready. Stopped, backoff, and quarantined views omit generation where allowed by the wire contract.

The standalone `/sv` binary supports `sv list` and `sv status SERVICE` only when an authorized launcher installs observation authority at handle `1`. The trusted `ush` builtins receive `SEND | DUPLICATE` observation authority at handle `2`, duplicate it down to exact `SEND` for each operation, and never receive `TRANSFER`. Pathname identity is not authorization.

## Explicit exclusions

The current integration adds none of the following:

- a separately restartable service-manager process;
- mutation authority or live `start`, `stop`, and `restart` behavior;
- service activation, dependency resolution, or a definition loader;
- service launch, stop, readiness, health, or restart-policy changes beyond PID 1's existing hard-coded supervision;
- channel activation or queued activation semantics;
- policy-populated or partially visible registries;
- NSWP negotiation or a general IDL-generated binding;
- new kernel, syscall, endpoint, or attachment primitives.

Those remain later lifecycle and service-management work. The present native deliverable is read-only observation over ordinary endpoint capabilities, backed by the allocation-free fixed-record contract and its host-testable canonical encoding, decoding, validation, and correlation rules.

`NSVC` v1 is intentionally closed: unknown operations, states, statuses, flags, and nonzero reserved fields are rejected. Adding any of them requires a later protocol version and explicit version negotiation or binding rather than silently changing the meaning of a v1 record.
