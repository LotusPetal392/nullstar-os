# Logging, journal, and rotation direction

## Status

A centralized structured logging service, a kernel early-boot ring, `logctl`, service
log integration, and centrally managed retention are **accepted direction**. Journal
encoding, segment sizes, compression, remote forwarding, and audit tamper-evidence
remain **tentative design**.

## Goals

The logging system should:

- accept records before and after normal userspace starts;
- attribute records to stable process, application, package, service, user, session,
  and boot identities;
- provide structured fields without discarding readable messages;
- support live observation, indexed queries, crash diagnosis, and service health;
- apply bounded queues, rate limits, retention, compression, and disk-pressure policy;
- preserve privacy and access control;
- provide syslog and text-file compatibility without making them the native model.

## Architecture

The first implementation may combine capture, storage, queries, and rotation in one
supervised service. The conceptual boundaries are:

```text
kernel early log and panic records
applications, services, and captured streams
                 |
                 v
            logging service
          /        |        \
  live readers   journal   crash/audit links
                    |
             retention and rotation
```

Possible later service identities include:

```text
logging
logging.storage
logging.rotate
logging.forwarder
crash.service
audit.service
```

Splitting these components should follow measured isolation and reliability needs, not
precede a stable record and query contract.

## Structured record

A native log record should include:

- record ID;
- monotonic timestamp and optional wall-clock timestamp;
- boot ID;
- severity;
- message;
- process and thread ID plus process generation;
- application and package ID where known;
- service ID and service generation where known;
- user and login-session identity;
- subsystem and event name;
- request, trace, connection, device, or other correlation IDs;
- bounded typed fields;
- privacy classification for sensitive fields.

The message remains useful for humans. Callers should not have to encode basic
identity, severity, or event metadata into prose.

Suggested severities are:

```text
trace
debug
info
notice
warning
error
critical
alert
emergency
```

Compatibility APIs may map traditional syslog priorities without changing the native
record model.

## Native logging API

The Rust runtime should offer structured macros or builders that submit through a
bounded IPC or shared-memory path. Source identity is supplied by the launch and
service environment, not trusted from arbitrary caller strings.

The API should support:

- static event names and field schemas where practical;
- text and typed fields;
- explicit correlation IDs;
- nonblocking use by latency-sensitive code;
- detection and reporting of dropped records;
- span or trace integration later.

Realtime media, interrupt workers, and low-level drivers must never block on journal
I/O. They should write to preallocated bounded queues and tolerate loss according to
severity policy.

### Production NSWP logging contract

The allocation-free protocol, codec, producer, and bounded volatile collector live in the `no_std`
`nswp-logging` crate; `nswp-testkit` retains deterministic transport fixtures. The contract is
exercised over native endpoints but is not a persistent journal format. It uses protocol family ID
`7db79cd9-c685-400f-b9f1-55d89b8e8a8a`, major version 2, and a
client-to-server one-way `Emit` method. Major 2 reflects the fixed-layout change from the original
host pilot.

Each submitted record carries a required UUIDv4 `EventId` in RFC byte order. An `EventId`
identifies an event type: an event definition commits to one stable constant and all occurrences
reuse it. It is not generated per record and is distinct from the NSWP transport `trace_id`.
The collector assigns a monotonic nonzero `RecordId` to each retained occurrence. Record IDs are
never producer-supplied or reused and are scoped by the negotiated collector service generation.

The 192-byte endpoint profile requires compact bodies. `Emit` uses an 80-byte fixed root containing
severity, privacy class, monotonic time, the 16-byte event ID, `string<16>` subsystem,
`string<64>` message, and an extension table. Minor 1 adds an optional wall-clock timestamp.
Maximum records encode to exactly 160 bytes at minor 0 and 192 bytes at minor 1; including the
NSWP header, the latter is exactly one 256-byte endpoint packet.

Minor 2 adds request/reply `GetCollectorStats` and `ReadHistory` methods. Statistics use a fixed
64-byte response containing received, retained, capacity, eviction, collector-drop, and redaction
counts plus oldest and newest record IDs. History reads are live, one-record-at-a-time queries using
a caller-supplied `after RecordId` cursor. A present history response carries the record ID,
kernel-stamped source PID, event and trace IDs, severity, privacy, timestamps, and packed text in at
most 192 bytes. An all-zero optional envelope represents the ordinary end of currently retained
history.

Minor 3 distinguishes process and kernel history without increasing the 192-byte maximum. Process
records preserve the exact minor-2 encoding. Kernel records use source PID zero, carry their nonzero
boot-scoped kernel sequence in the wall-time slot, require the wall-time-present flag to be false,
and use the trace-ID slot for an optional canonical UUIDv4 boot ID. Collector `RecordId`, kernel
sequence, event ID, transport trace ID, and boot ID remain distinct identities. Minor-2 readers skip
retained kernel records and continue to receive compatible process records rather than failing the
connection.

Producer principal, service identity, and producer service generation are not accepted from the
record body. The host collector models the intended production rule: the launch or service
environment binds them to a non-default `PeerContextId`, and the collector rejects unspecified or
mismatched contexts. The native pilot currently relies on delegated endpoint authority and does not
yet populate that peer context. The negotiated `service_generation` remains the logging
collector's generation and must not be mistaken for the producer generation.

The producer API exposes two queue-pressure policies:

- `Reliable` reports backpressure so the caller can retry when the endpoint did not accept the
  packet; it does not imply processing, journal commit, durable storage, or safe replay after an
  uncertain send.
- `BestEffort` drops on queue pressure and increments an observable dropped-record count.

`Emit` remains one-way. If a packet was accepted but provider failure makes processing uncertain,
the producer does not replay it on a replacement generation: replay could duplicate a retained
record, and the current protocol has no acknowledgement that distinguishes that case from loss.

The production crate provides an allocation-free fixed collector. The native service currently
keeps 64 records, overwrites the oldest record when full, and saturates its counters. It redacts
`secret-never-persist` before copying message bytes into retained storage. Producer-side
best-effort drops and collector-side drops are separate counters because the collector never sees
a record rejected at the producer mailbox. Authorization-aware general reads, redaction for other
privacy classes, rate limiting, suppression summaries, persistence, and journal rotation remain
future work.

### Native service routes and endpoint sessions

The logging service has stable service ID `7cbd3f65-50a6-4c30-b195-9fbed633da43`, distinct from its
NSWP protocol family ID. Role `1` is producer authority and role `2` is observer authority. They are
separate stable routes: possession of a resolved producer ingress is `Emit` authority, while a
resolved observer ingress is collector-statistics and history-read authority. The role is not
caller-supplied in NSWP, and the service enforces the method set again after validation. A producer
request for history receives `AccessDenied`; an observer that sends a one-way record loses only its
own session.

PID 1 temporarily brokers both routes using the allocation-free
[service route protocol](../service-route-protocol.md). Each route grant is bound to exactly one
service-and-role key. A client sends one exact 40-byte `NSRT` v1 request with one fresh exact-`SEND`
reply capability; acceptance returns one exact-`SEND` capability for the current role ingress and
the nonzero provider generation. Failure replies carry no capability. The broker authorizes the
kernel-stamped sender PID before looking up availability. It never parses NSWP negotiation or log
packets and is not on the logging data path after resolution.

Each `/logging-service` generation receives fresh producer and observer ingress endpoint objects.
PID 1 retains separate stable publication sources for the two roles, and clients resolve each role
independently. Within one generation, each role ingress remains shared by its resolved clients, so a
noisy producer can still impose queue pressure on other producers. Replacement publishes the fresh
objects rather than rebinding old ingress handles. The current pilot uses the service process PID as
the route generation; this is not a durable service-generation authority and must eventually be
replaced by a service-manager-owned counter.

Fresh ingress objects isolate generations but do not provide global revocation. Future resolutions
select only the replacement, and packets sent through an old ingress cannot reach it. Existing
exact-`SEND` handles to the old object cannot all be invalidated, however, because the current kernel
has no general capability-revocation primitive. Old handles and queued transfers can also retain
endpoint objects, which matters under the current system-wide limit of 32 live endpoint objects.
Clients close stale routes and resolve again after replacement.

After resolution, a fixed 16-byte `NSLS` bootstrap record establishes each direct service connection
before ordinary NSWP negotiation. The client transfers exact `SEND` authority for a fresh private
reply endpoint while retaining exact `RECEIVE`. The service binds the resulting session to the role
ingress, kernel-stamped nonzero sender PID, and current generation. Each session owns independent
NSWP negotiation and transaction state, so transaction identifiers and responses cannot cross
clients. The allocation-free service admits at most four total sessions and at most three of either
role. Explicit disconnect releases a slot; malformed packets or failed private replies remove only
the offending session. Current endpoints lack peer-close notification, so a client that crashes
without sending `NSLS` disconnect can occupy a bounded slot until service replacement.

The normal probe resolves and negotiates separate producer and observer sessions at minor 3. It
verifies four imported kernel records with ordered kernel sequences, proves that a non-PID-1 process
cannot open kernel early-log authority, and confirms that producer authority cannot query collector
history. It then sends a maximum-size process record whose 192-byte body exactly fills one 256-byte
endpoint message, submits a secret record, and verifies collector statistics, process attribution,
and redaction through the observer session. Because the role ingresses are independent connections,
the probe waits for the expected collector high-water count rather than assuming an observer query
is ordered after a preceding one-way producer send.

The NullFS restart diagnostic stops the collector, fills the current eight-message producer ingress,
verifies reliable backpressure and one best-effort producer drop, resumes the service, and submits 65
records. It checks the 64-record ring wrap, oldest/newest IDs, redaction, and counters, then replaces
the service after the clients disconnect. PID 1 delegates the same read-only early-log reader to the
replacement, which reimports the kernel snapshot before readiness, and publishes fresh producer and
observer ingress objects. Freshly resolved sessions verify the same boot-scoped kernel sequences at
collector record IDs 1 through 4 before accepting a new process record. Neither PID 1 nor the route
layer replays an uncertain one-way `Emit` across this boundary.

PID 1 delegates only selected stable role grants to the boot probes. The trusted root recovery shell
receives `SEND | DUPLICATE` observer-route authority and implements `logctl show` as a builtin over a
locally duplicated exact-`SEND` route; it has no `TRANSFER` right, and unrelated shell children
receive no logging capability. The standalone `/logctl` binary is used only when an authorized
launcher such as PID 1 installs observer-route authority before execution. This avoids treating a
mutable pathname as an executable-identity security boundary. The arrangement remains an interim
PID 1 broker rather than the general service manager or a durable principal-identity system. Records
at `trace` and `debug` remain retained but are not mirrored to the console.

## Early boot and panic records

The kernel should maintain a bounded ring containing boot stages, hardware discovery,
exceptions, service-launch failures, warnings, and panic information. Serial output may
mirror these records during early development.

When the userspace logging service starts, it imports the ring with the active boot ID.
A previous-boot crash record should remain available from a small crash-safe store where
hardware and durability support make that reliable.

### Initial kernel ring

The kernel has an allocation-free 64-record structured early-log ring. Records use the same stable
event IDs, severity values, privacy classes, and 16-byte subsystem/64-byte message bounds as the
production NSWP logging contract. Each
retained record receives a nonzero boot-scoped sequence number. The ring overwrites its oldest
record when full, preserves chronological snapshot order, saturates accounting counters, and never
reuses a sequence after exhaustion.

The global write path disables local interrupts around the nonblocking lock attempt and bounded
mutation; contention drops the new record rather than spinning. A separate best-effort contention counter exposes these
drops. Serial output remains independent and is never performed while the ring lock is held. Panic
and allocation-failure paths attempt a fixed structured record before using the existing serial
fallback, but guaranteed panic capture still requires a future lock-independent emergency slot.

`secret-never-persist` messages are replaced before caller message bytes enter fixed storage.
Oversized subsystems and non-secret messages are rejected rather than truncated. CPU, process, and
thread source attribution can be represented when known; current bootstrap records correctly leave
unknown source fields absent. A zero monotonic time explicitly means the timer was not ready, and
sequence numbers remain authoritative for ordering.

PID 1 opens the singleton kernel early-log reader capability and retains its transferable source
handle. Each selected logging-service generation receives only `READ` at bootstrap; it cannot
transfer, duplicate, or manufacture the authority. The dedicated cursor syscall returns one record
and ring statistics in a fixed 256-byte response from one lock acquisition, reports lock contention
as `TRY_AGAIN`, and never exposes the private Rust record layout. The userspace decoder rejects
impossible retained ranges and accounting relationships, including inconsistent submitted,
retained, overwritten, dropped, and rejected counts. The service pins the first response's retained
`[oldest, newest]` sequence range, requires exact continuity, and stops at that boundary. Overwrite
of an unread record is a non-restartable startup failure, so PID 1 cannot mask a detected gap by
starting a replacement that pins a newer range. Once a generation has reached readiness, ordinary
supervised replacement remains enabled and the replacement reimports the kernel history before it
becomes ready. Ordinary producers receive neither the reader nor kernel source-creation authority.

The handoff ABI carries optional CPU, process, and thread source details for future consumers. Minor
3 collector history currently preserves kernel source, sequence, boot ID, event metadata, and text,
but does not expose those detailed source fields.

The ring accepts an immutable UUIDv4 boot ID, but the current bootstrap marks boot identity
unavailable because NullStar does not yet have a reviewed early entropy source. It does not invent a
supposedly unique identifier from addresses or timer ticks. Kernel sequence still permits replay
recognition within the current boot; a trustworthy boot-ID source remains the next identity step.

Useful queries include:

```text
logctl boot
logctl boot --previous
logctl show --subsystem kernel
```

## Service-manager integration

The service manager should automatically give every managed service:

- a structured logging endpoint;
- optional stdout and stderr capture;
- stable service and generation attribution;
- configurable minimum level and rate limits;
- a retention class;
- state-transition, readiness, and failure records.

`sv logs` is a convenient filtered view over the same logging service:

```text
sv logs network.policy
sv logs network.policy --follow
sv logs network.policy --previous
sv logs network.policy --since 10m
```

Services should not manage private log files merely to participate in ordinary system
logging.

## `logctl`

The native query and administration command is `logctl`. The first implemented operation is
`logctl show`; the remaining operations are accepted direction:

```text
logctl show
logctl follow
logctl boot
logctl services
logctl apps
logctl query
logctl inspect <record-id>
logctl storage
logctl vacuum
logctl crashes
```

`logctl show` uses an observer session and captures the current oldest and newest collector
`RecordId` plus retained count. It requires the first record, every exact successor, the final
high-water record, and the number of displayed records to match that captured range. It is a bounded
live view rather than an atomic snapshot: if concurrent eviction makes the range inconsistent, the
command reports that history changed instead of silently presenting a partial snapshot.

Filters should include time range, boot, service and generation, application, user,
severity, subsystem, event, and structured fields. A machine-readable output format
must be versioned and separate from the default human display.

The query service enforces field-level access policy. A caller may be allowed to know
that a security event occurred while sensitive fields remain redacted.

## Journal storage

The canonical interface is the service protocol, not direct parsing of files.
A possible physical layout is:

```text
/System/var/log/
├── journal/
│   ├── current/
│   └── archive/
├── crash/
└── exported/
```

User-scoped storage may use `Profile/state/logs`, subject to per-user policy. Native
applications must not hard-code the backing path or journal encoding.

The preferred storage model is append-only, checksummed segments:

```text
journal/
├── segment-000001
├── segment-000002
└── index
```

The active segment is bounded. Completed segments become immutable before compression,
archival, or deletion. A corrupt segment should be isolated without making unrelated
boots unreadable.

## Rotation and retention

Native journal rotation is segment management, not renaming a text file beneath a
writer. Rotation triggers should include:

- maximum active-segment size;
- elapsed time;
- boot boundary;
- total journal size;
- minimum free-space threshold;
- explicit administrative request.

Retention classes may differ for ordinary system, security, user, debug, and crash
records. Each class should combine maximum age, maximum bytes, minimum free space, and
compression policy.

The logging service should reserve enough space or deletion authority to report a
low-disk emergency. Debug volume must not crowd out critical boot, storage, or security
events.

`logctl vacuum` should be an authorized request to apply retention policy, not an
unbounded delete command over arbitrary paths.

## Legacy text files and syslog

Ported software may expect:

- a syslog API;
- `/dev/log` or a compatible local endpoint;
- RFC-compatible import or forwarding;
- ordinary text files followed by a logrotate-like utility.

NullStar should provide adapters that translate these sources into attributed native
records where possible.

A separate compatibility rotator may manage legacy files under an explicit policy. It
should ask the service manager to request a safe reopen action rather than searching
for a PID and sending an uncoordinated signal.

Native services should prefer the journal and should not require legacy file rotation.

## Backpressure and rate limits

A faulty process must not exhaust IPC queues, memory, or disk with logs. Limits should
apply by process generation, service, application, severity, and event class.

Suggested pressure behavior is:

- drop or sample `trace` and `debug` first;
- buffer `info` and `notice` briefly, then summarize loss;
- reserve bounded capacity for warnings and errors;
- mirror critical failures to an emergency ring where possible;
- never allow even critical logging to allocate or block without limit.

Suppression should produce a summary such as the number of records dropped and the
responsible identity. Summaries themselves need bounded aggregation.

## Privacy and access control

Logs may reveal files, websites, contacts, devices, working hours, and security state.
The logging design should support field classifications such as:

```text
public
user-private
administrator
security-sensitive
secret-never-persist
```

Tokens, passwords, encryption keys, and raw secret values must not be persisted.
Sensitive services should use structured identifiers or redacted values rather than
placing secrets in messages.

Applications may submit and inspect their own records. Reading other applications,
system services, the kernel, audit records, or another user requires explicit rights.
Retention and export settings are also authorized operations.

## Crash and audit integration

A crash service should record fault context, registers, loaded modules, available
backtrace information, application version, and references to preceding log records.
It should not duplicate an unbounded portion of the journal into every report.

Operational logging and security auditing share attribution but may have different
storage and deletion policy. Audit events include capability grants, administrative
service actions, package changes, firewall-policy changes, authentication, and
namespace-binding changes.

Later audit segments may use chained hashes or signed summaries. Deleting ordinary
debug logs must not silently delete records retained by mandatory audit policy.

## Remote forwarding

Remote forwarding is optional and later. A low-privilege forwarder should receive only
a filtered subscription and send over an authenticated transport. Policy explicitly
defines which users, subsystems, and fields may leave the machine.

Supported compatibility targets may include syslog over TLS and structured encodings,
but the native journal format does not need to become a network protocol.

## Recommended implementation stages

1. Add a bounded kernel early-log ring and boot IDs.
2. Add a userspace logging endpoint with structured records and identity attribution.
3. Capture service stdout/stderr and implement `logctl show`, `follow`, and `sv logs`.
4. Persist a size-bounded append-only journal with simple indexes.
5. Add immutable segments, time and size rotation, compression, and low-disk policy.
6. Add per-field access control, application/user views, and crash-report links.
7. Add syslog and legacy text-file compatibility.
8. Separate or harden audit storage and optional remote forwarding.

## Open questions

- The on-disk record and segment encoding.
- Whether indexes are stored per segment or rebuilt after validation.
- Default retention classes and disk budgets.
- Exact field-type, schema, and redaction model.
- Crash-safe persistence available before the main filesystem is mounted.
- Whether user-session records reside in a central system journal or a per-user store.
