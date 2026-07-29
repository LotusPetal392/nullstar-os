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

## Early boot and panic records

The kernel should maintain a bounded ring containing boot stages, hardware discovery,
exceptions, service-launch failures, warnings, and panic information. Serial output may
mirror these records during early development.

When the userspace logging service starts, it imports the ring with the active boot ID.
A previous-boot crash record should remain available from a small crash-safe store where
hardware and durability support make that reliable.

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

The native query and administration command should be `logctl`:

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
