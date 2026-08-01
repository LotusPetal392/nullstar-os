# NSWP packet header and protocol-identifier decision

## Status

The following are **accepted direction** for the provisional NullStar Wire Protocol:

- NSWP 1.0 uses an exact 64-byte base packet header;
- all numeric header fields use unsigned little-endian encoding;
- one channel connection carries one negotiated protocol family and version;
- the protocol-family identifier is carried during negotiation and retained as immutable
  connection metadata rather than repeated in every ordinary packet;
- a protocol-family identifier is an explicitly generated and committed RFC 9562 UUIDv4;
- UUID bytes use RFC UUID byte order and are never interpreted as a native-endian integer
  or a Windows mixed-endian GUID;
- a packet trace identifier is a separate opaque 128-bit correlation value and is not a
  UUID;
- the header intentionally excludes caller identity, authorization data, descriptor
  hashes, checksums, process identifiers, user identifiers, and transport-redundant
  sequence information.

Exact packet values and field relationships in this document are concrete enough to
implement. They remain provisional until NSWP satisfies the freeze criteria in
[NSIDL and the NullStar Wire Protocol](nsidl-and-wire-protocol.md).

This document is the authoritative decision for the packet-header and protocol-identifier
questions. Where it differs from the provisional header, negotiation, protocol-identity,
transport-profile, or corresponding open-question sections in
[NSIDL and the NullStar Wire Protocol](nsidl-and-wire-protocol.md), this document takes
precedence until those sections are consolidated.

## Decision summary

```text
Packet header:       fixed 64-byte NSWP 1.0 base header
Protocol-family ID: RFC 9562 UUIDv4
UUID source form:    lowercase canonical hyphenated text
UUID binary form:    16 octets in RFC UUID order
Per-packet UUID:     omitted after negotiation
Trace ID:            separate opaque 16-byte correlation value
```

The complete protocol key is:

```text
ProtocolKey {
    protocol_id: ProtocolId,
    major: u16,
}
```

A bound connection additionally retains:

```text
BoundProtocol {
    protocol_id: ProtocolId,
    major: u16,
    minor: u16,
    features: FeatureSet,
    limits: ConnectionLimits,
    service_generation: u64,
}
```

## Exact NSWP 1.0 packet header

Every NSWP 1.0 packet begins with exactly 64 bytes.

```text
Offset  Size  Field
------  ----  ------------------
0x00       4  magic
0x04       2  header_bytes
0x06       1  wire_major
0x07       1  wire_minor

0x08       1  kind
0x09       1  flags
0x0a       2  reserved0

0x0c       2  protocol_major
0x0e       2  protocol_minor

0x10       4  ordinal
0x14       4  body_bytes

0x18       2  handle_count
0x1a       2  reserved1
0x1c       4  transport_status

0x20       8  transaction_id
0x28       8  deadline_ns

0x30      16  trace_id
-------------------------------
Total      64 bytes
```

The conceptual field declaration is:

```rust
struct NswHeaderV1 {
    magic: [u8; 4],
    header_bytes: u16,
    wire_major: u8,
    wire_minor: u8,

    kind: u8,
    flags: u8,
    reserved0: u16,

    protocol_major: u16,
    protocol_minor: u16,

    ordinal: u32,
    body_bytes: u32,

    handle_count: u16,
    reserved1: u16,
    transport_status: u32,

    transaction_id: u64,
    deadline_ns: u64,

    trace_id: [u8; 16],
}
```

This declaration documents names and offsets. The wire representation is the explicit
byte layout above. Implementations must encode and decode fields explicitly and must not
assume that copying a compiler-native Rust or C structure produces a valid packet.

## Numeric and byte-array encoding

All numeric fields use little-endian encoding:

```text
u16  unsigned little-endian 16-bit integer
u32  unsigned little-endian 32-bit integer
u64  unsigned little-endian 64-bit integer
```

Byte-array fields retain their declared byte order:

```text
magic       four literal octets
trace_id    sixteen opaque octets
protocol_id sixteen RFC-order UUID octets in negotiation records
```

A UUID is not encoded as a `u128`, two `u64` values, or a collection of native integer
fields.

## Header magic

The exact magic bytes are:

```text
4e 53 57 50
 N  S  W  P
```

Normatively:

```rust
magic == *b"NSWP"
```

The magic remains constant across NSWP versions. The `wire_major` and `wire_minor`
fields identify the supported wire format.

## Header size and wire version

For NSWP 1.0:

```text
header_bytes = 64
wire_major = 1
wire_minor = 0
```

A conforming NSWP 1.0 receiver rejects any other `header_bytes` value.

A future supported wire version may add fields after byte 63. Any extended header size
must be a multiple of eight. An implementation must first determine that it supports the
wire version; it must not blindly skip an unfamiliar extended header.

The NSWP wire version is independent of the service-protocol version:

```text
NSWP wire version:       1.0
FilePortal protocol:     1.2
MediaGraph protocol:     3.4
```

## Packet kinds

The exact NSWP 1.0 packet-kind values are:

```text
0x00  Invalid
0x01  NegotiateRequest
0x02  NegotiateResponse
0x03  Request
0x04  Response
0x05  OneWay
0x06  Event
0x07  Cancel
0x08  ProtocolError
0x09..0xff Reserved
```

An undefined value is a protocol error.

## Header flags

NSWP 1.0 defines one flag:

```text
0x01  TRACE_SAMPLED
```

All other bits are reserved and must be zero.

`TRACE_SAMPLED` requests detailed timing retention from tracing infrastructure that
supports it. It:

- grants no authority;
- does not permit payload logging;
- does not override NSIDL privacy classifications;
- is only a diagnostic hint.

When `TRACE_SAMPLED` is set, `trace_id` must not be all zero. A nonzero `trace_id` may be
present without `TRACE_SAMPLED` to permit correlation without requesting full sampling.

## Reserved fields

For NSWP 1.0:

```text
reserved0 = 0
reserved1 = 0
```

A receiver rejects nonzero reserved fields. Senders must not use reserved storage for
private extensions.

## Service-protocol version fields

After successful negotiation, every ordinary packet contains the exact selected service
version:

```text
protocol_major = selected major
protocol_minor = selected minor
```

The receiver requires an exact match with immutable connection state.

For `NegotiateRequest` and `NegotiateResponse`:

```text
protocol_major = 0
protocol_minor = 0
```

The requested protocol family and major version are part of the negotiation body because
the connection has not yet been bound.

A pre-negotiation `ProtocolError` also uses `0.0`. A post-negotiation `ProtocolError`
uses the selected protocol version.

The protocol version remains in every ordinary packet because it costs only four bytes
and supports:

- immediate validation against connection state;
- easier interpretation of isolated packet captures;
- detection of senders that encode fields unavailable in the selected minor version.

## Ordinal

`ordinal` identifies a protocol method or event.

```text
NegotiateRequest   0
NegotiateResponse  0
Request             method ordinal
Response            original method ordinal
OneWay              method ordinal
Event               event ordinal
Cancel              original method ordinal
ProtocolError       related ordinal, or 0 if unavailable
```

Ordinal zero is reserved by NSIDL and cannot identify a method, event, field, feature,
enum member, or union alternative.

## Body length

`body_bytes` is the exact number of bytes following the complete header.

For NSWP 1.0:

```text
total transport bytes = 64 + body_bytes
```

Requirements:

- `body_bytes` is a multiple of eight;
- the body begins at transport-message offset 64;
- no trailing bytes follow the declared body;
- the body uses the canonical NSWP self-relative arena encoding;
- the body does not exceed the negotiated connection limit.

## Attached handles

`handle_count` exactly equals the number of handles attached to the transport message.
It includes every nested handle domain represented in the body.

Packets that cannot carry handles use:

```text
handle_count = 0
```

This applies to:

- negotiation packets;
- cancellation packets;
- protocol-error packets;
- responses with a nonzero transport status.

A mismatch between the header count and transport attachment count is fatal to the
connection. Every received attachment is closed if validation fails.

## Transport status

`transport_status` is meaningful only for `Response`.

```text
0  Ok
1  Canceled
2  TimedOut
3  Overloaded
4  ResourceExhausted
5  Unavailable
6  AccessDenied
7  BadState
8  NotSupported
9  Internal
```

Every other packet kind requires:

```text
transport_status = 0
```

When a response has a nonzero transport status:

```text
body_bytes = 0
handle_count = 0
```

Service-domain errors remain in the declared response body, normally as:

```text
result<Success, ServiceError>
```

For example:

```text
transport_status = Ok
response body    = Error(FileNotFound)
```

means transport and dispatch completed normally and the service returned a domain error.

## Transaction identifiers

`transaction_id` is a connection-local unsigned 64-bit value.

Rules:

- zero is reserved;
- the client assigns identifiers;
- a request uses a nonzero identifier;
- its response and cancellation packet echo that identifier;
- an identifier cannot be reused while outstanding;
- runtimes should allocate monotonically from one;
- wraparound requires closing and reopening the connection.

Security must not depend on transaction identifiers being unpredictable.

Packet rules are:

```text
NegotiateRequest   0
NegotiateResponse  0
Request             nonzero
Response            matching nonzero request identifier
OneWay              0
Event               0
Cancel              matching nonzero request identifier
ProtocolError       related identifier, or 0
```

## Deadlines

`deadline_ns` is an absolute timestamp from the NullStar monotonic clock, measured in
nanoseconds.

```text
0xffffffffffffffff = no deadline
0x0000000000000000 = already expired
```

The value is meaningful only within the current running system's monotonic clock domain.
It is never persisted or interpreted as wall-clock time.

Packet behavior is:

| Kind | Deadline value |
| --- | --- |
| `NegotiateRequest` | No deadline |
| `NegotiateResponse` | No deadline |
| `Request` | Declared deadline or no deadline |
| `Response` | Exact value from request |
| `OneWay` | Declared deadline or no deadline |
| `Event` | No deadline |
| `Cancel` | Exact value from original request |
| `ProtocolError` | Related deadline when known, otherwise no deadline |

Nested calls propagate:

```text
minimum(caller_deadline, local_operation_deadline)
```

## Trace identifier

`trace_id` is an opaque 128-bit correlation identifier.

```text
all zero = no trace correlation
nonzero  = active trace correlation
```

It is not a UUID and has no UUID version or variant bits.

Rules:

- a client creates or inherits the request trace identifier;
- a response echoes the request trace identifier exactly;
- a cancellation packet echoes it;
- a server event may use a server-generated trace identifier;
- nested RPCs normally retain the same trace identifier;
- local span identifiers belong to tracing metadata rather than the packet header.

The trace identifier grants no authority and must not influence authentication,
authorization, routing, or retry behavior.

## Packet-kind relationship matrix

| Kind | Protocol version | Ordinal | Transaction | Status | Deadline | Handles |
| --- | --- | ---: | ---: | ---: | --- | ---: |
| `NegotiateRequest` | `0.0` | 0 | 0 | 0 | Infinite | 0 |
| `NegotiateResponse` | `0.0` | 0 | 0 | 0 | Infinite | 0 |
| `Request` | Selected | Method | Nonzero | 0 | Declared | Allowed |
| Successful `Response` | Selected | Method | Matching | 0 | Echo | Allowed |
| Failed `Response` | Selected | Method | Matching | Nonzero | Echo | 0 |
| `OneWay` | Selected | Method | 0 | 0 | Declared | Allowed |
| `Event` | Selected | Event | 0 | 0 | Infinite | Allowed unless lossy |
| `Cancel` | Selected | Method | Matching | 0 | Echo | 0 |
| `ProtocolError` | Selected or `0.0` | Related or 0 | Related or 0 | 0 | Related or infinite | 0 |

A packet violating one of these relationships is malformed even when each individual
field value is otherwise in range.

## Protocol-family identifier choice

A NullStar protocol-family identifier is an
[RFC 9562](https://www.rfc-editor.org/rfc/rfc9562.html) UUIDv4.

NSIDL source uses the canonical lowercase hyphenated representation:

```text
@id("3c59c73e-852e-4ad8-bb3d-610ca4920727")
```

UUIDv4 supplies a decentralized, standardized 128-bit identifier form with 122 random
bits after the version and variant bits are installed.

The identifier is generated once and committed. It is not recomputed from a protocol
name and is not regenerated during ordinary builds.

## Canonical NSIDL UUID spelling

The accepted source form is exactly:

```text
xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx
```

Where:

```text
x = lowercase hexadecimal digit
y = 8, 9, a, or b
```

Requirements:

- exactly 36 ASCII characters;
- lowercase hexadecimal only;
- hyphens at positions 8, 13, 18, and 23;
- version nibble equal to `4`;
- RFC variant bits equal to binary `10`;
- nil UUID prohibited;
- all-ones UUID prohibited.

The compiler rejects noncanonical spelling rather than silently rewriting source. A
diagnostic may display the canonical replacement.

## Protocol ID binary representation

In negotiation bodies and `.nsproto` descriptors, the identifier is exactly 16 octets in
RFC UUID byte order.

Example:

```text
NSIDL text:
00112233-4455-4677-8899-aabbccddeeff

Binary octets:
00 11 22 33 44 55 46 77 88 99 aa bb cc dd ee ff
```

The Windows COM mixed-endian GUID representation is explicitly prohibited.

Rust bindings should represent the value conceptually as:

```rust
#[repr(transparent)]
pub struct ProtocolId([u8; 16]);
```

C bindings should represent it conceptually as:

```c
typedef struct ns_protocol_id {
    uint8_t bytes[16];
} ns_protocol_id_t;
```

Equality is byte-for-byte equality. The value has no numeric ordering semantics.

## Why UUIDv4

A raw random 128-bit value would retain six additional random bits but would require
NullStar to define its own:

- canonical text form;
- separator and case rules;
- byte-order convention;
- validation and formatting tools;
- interoperability guidance.

UUIDv4 already supplies those conventions. The six-bit difference has no practical
collision consequence for the number of protocol families NullStar can plausibly define.

## Why not UUIDv5

UUIDv5 derives an identifier from a namespace and name. That would make canonical naming
security-significant and would create undesirable behavior:

- renaming a protocol could appear to change its identity;
- two publishers could choose the same presentation name;
- moving a protocol between source libraries could create accidental churn;
- identity would become a recomputed consequence instead of a committed decision.

The NSIDL library name is human-readable metadata. It is not the protocol identity.

## Why not UUIDv7 or UUIDv8

UUIDv7 is useful when creation-time ordering matters. Protocol identity does not require
chronological ordering, and exposing creation time adds no routing or compatibility
benefit.

UUIDv8 is reserved for custom formats. NullStar has no protocol metadata that needs to be
embedded in the identifier, so a custom UUID layout would add specification burden
without improving the design.

## Protocol ID lifecycle

The protocol ID identifies a family, not an individual minor or major version.

These share one protocol-family UUID:

```text
FilePortal 1.0
FilePortal 1.1
FilePortal 1.2
FilePortal 2.0
```

The major version forms part of the complete protocol key and expresses incompatibility.

A new UUID is required only when the interface is a genuinely different protocol family
rather than a new version of the same contract.

Renaming a library or service does not change the UUID when the protocol family remains
the same.

An independently developed incompatible fork must allocate a new UUID. It must not retain
another project's protocol ID while changing the contract outside the original
versioning rules.

## Protocol ID generation

The compiler toolchain should provide:

```text
nsidlc new-id
```

The command:

1. obtains bytes from the host cryptographic random-number source;
2. installs the RFC UUIDv4 version and variant bits;
3. emits lowercase canonical text;
4. optionally inserts the value into a new protocol declaration.

Example:

```text
$ nsidlc new-id
3c59c73e-852e-4ad8-bb3d-610ca4920727
```

The generated value is committed to:

```text
.nsidl source
.nsidl.lock history
.nsproto descriptor
```

Reproducible builds consume the committed value and never generate a replacement.

## Protocol IDs are not authority

Knowing or presenting a protocol UUID grants nothing.

A protocol identifier:

- identifies a wire contract;
- does not identify a trusted publisher;
- does not prove package ownership;
- does not authorize service lookup;
- does not grant a service capability;
- is not secret.

Authority remains:

```text
restricted service namespace
+ broker routing policy
+ verified provider identity
+ an actual channel endpoint
```

A malicious process may place any UUID in a negotiation request. The broker and provider
still decide whether that connection is valid.

## Duplicate protocol ID handling

Within one source graph, two distinct declarations cannot use the same:

```text
ProtocolId + major version
```

unless they resolve to the same canonical declaration.

Across packages, multiple client packages may contain the same public protocol
descriptor. That is normal.

Two providers must not publish incompatible descriptors for the same protocol key. The
package and service registries compare canonical compatibility metadata. If incompatible
definitions claim the same protocol key, installation or provider registration fails
closed.

A package signature identifies who supplied a conflicting definition but does not make
the conflict valid.

## Protocol ID placement

The protocol-family UUID appears in:

- NSIDL source;
- the `.nsidl.lock` compatibility history;
- the compiled `.nsproto` descriptor;
- package protocol metadata;
- service-broker lookup and routing metadata;
- the negotiation request;
- the negotiation response;
- connection tracing and diagnostic records.

It does not appear in every request, response, one-way message, event, cancel packet, or
post-negotiation protocol error.

After negotiation, the connection runtime retains the protocol family, version, feature
set, limits, and provider generation as immutable connection metadata.

## Exact negotiation request

Moving the protocol UUID out of the ordinary packet header requires it in the negotiation
request body.

The fixed request root is 48 bytes:

```text
Offset  Size  Field
------  ----  ------------------
0x00      16  protocol_id
0x10       2  protocol_major
0x12       2  min_minor
0x14       2  max_minor
0x16       2  flags
0x18       4  max_body_bytes
0x1c       2  max_handles
0x1e       2  max_outstanding
0x20      16  features
-------------------------------
Total      48 bytes
```

Conceptually:

```rust
struct NswNegotiateRequestV1 {
    protocol_id: [u8; 16],

    protocol_major: u16,
    min_minor: u16,
    max_minor: u16,
    flags: u16,

    max_body_bytes: u32,
    max_handles: u16,
    max_outstanding: u16,

    features: NswSliceRefV1,
}
```

Requirements:

```text
protocol_major != 0
min_minor <= max_minor
flags = 0
max_body_bytes > 0
max_handles <= transport attachment limit
max_outstanding > 0
```

Feature records follow through the canonical body arena. Negotiation packets never carry
handles.

## Exact negotiation response

The fixed response root is 64 bytes:

```text
Offset  Size  Field
------  ----  ------------------
0x00      16  protocol_id
0x10       4  status
0x14       2  protocol_major
0x16       2  selected_minor
0x18       2  server_min_minor
0x1a       2  server_max_minor
0x1c       4  max_body_bytes
0x20       2  max_handles
0x22       2  max_outstanding
0x24       4  reserved0
0x28      16  features
0x38       8  service_generation
-------------------------------
Total      64 bytes
```

Conceptually:

```rust
struct NswNegotiateResponseV1 {
    protocol_id: [u8; 16],
    status: u32,

    protocol_major: u16,
    selected_minor: u16,
    server_min_minor: u16,
    server_max_minor: u16,

    max_body_bytes: u32,
    max_handles: u16,
    max_outstanding: u16,
    reserved0: u32,

    features: NswSliceRefV1,
    service_generation: u64,
}
```

On success:

- `protocol_id` exactly echoes the request;
- `protocol_major` exactly echoes the request;
- `selected_minor` lies within both supported ranges;
- the returned limits are the negotiated minima;
- returned feature records are sorted and enabled;
- `service_generation` is nonzero.

On failure:

```text
selected_minor = 0
features = empty
service_generation = 0
```

The server minor range may still be returned for diagnostics.

## Negotiation statuses

The exact response statuses are:

```text
0  Ok
1  UnsupportedProtocol
2  UnsupportedMajor
3  NoCommonMinor
4  RequiredFeatureUnavailable
5  TransportBoundsTooSmall
6  PolicyDenied
7  Busy
8  Internal
```

`UnsupportedProtocol` means the provider does not implement the requested UUID.

`UnsupportedMajor` means the provider recognizes the protocol family but not the
requested major version.

On negotiation failure, the server sends the response and closes the channel.

## Version and limit selection

The server selects the highest minor version satisfying:

```text
client_min_minor <= selected_minor <= client_max_minor
server_min_minor <= selected_minor <= server_max_minor
all required features are available and policy-permitted
selected types fit the negotiated body and handle limits
```

The selected limits are the minimum of:

```text
client limit
server limit
transport limit
protocol-declared limit
```

The chosen version, features, and limits are immutable for the life of the connection.
Renegotiation on the same channel is prohibited.

## Fields intentionally omitted from the header

The 64-byte header has no:

```text
protocol UUID
service generation
connection identifier
packet sequence number
checksum
descriptor hash
process ID
user ID
application ID
```

### Protocol UUID

One connection carries one protocol. The UUID is validated during negotiation and stored
in immutable connection state.

### Service generation

The generation is returned during negotiation and stored in connection state. A restarted
service uses a new channel and generation.

### Connection identifier

The channel kernel object already identifies the connection.

### Sequence number

The channel transport is ordered and message-boundary preserving. Requests use
transaction identifiers where correlation is required.

### Checksum

Kernel-local IPC preserves bytes atomically. A checksum would not protect against a
malicious peer, which can recompute it. A network bridge or persistent storage format may
add integrity at its own boundary.

### Descriptor hash

An exact descriptor hash would be too rigid for additive minor-version compatibility.
Protocol ID, selected version, negotiated features, generated validators, and trusted
service routing provide the active contract. Descriptor hashes remain useful in build and
diagnostic metadata.

### Caller identity

Trusted caller metadata is supplied out of band by the service broker and process model.
Caller-controlled packet fields must never determine identity or authorization.

## Revised transport profiles

The 64-byte header changes the body capacity of the standard profiles.

### Desktop profile

```text
Maximum total packet bytes:     65,536
Header bytes:                        64
Maximum body bytes:              65,472
Maximum attached handles:            64
Maximum nesting depth:                32
Maximum table fields:              1,024
Default outstanding calls:           256
Maximum negotiated outstanding:    4,096
```

### Current endpoint prototype profile

```text
Maximum total packet bytes:        256
Header bytes:                       64
Maximum body bytes:                192
Maximum attached handles:            1
Maximum outstanding calls:           8
```

The reduced profile can validate header framing, negotiation, transactions, small tables,
versioning, errors, deadlines, cancellation, and one attached capability.

Full NSWP conformance still depends on planned channel features such as atomic move
transfer, multiple attached handles, peer-closure signaling, and event-port-driven
asynchronous dispatch.

## Decoder validation additions

In addition to the body and handle rules in the main NSWP design, the packet decoder must
validate:

```text
magic equals the four bytes NSWP
header_bytes equals 64
wire version equals a supported value
unknown flag bits are zero
TRACE_SAMPLED implies a nonzero trace ID
reserved header fields are zero
protocol version fields match packet kind and connection state
ordinal matches packet kind and transaction state
body length exactly matches the transport message
handle count exactly matches transport attachments
transport status is legal for the packet kind
transaction identifier is legal for the packet kind
response, cancel, deadline, and trace fields echo the request where required
```

No service implementation receives a packet until these checks and complete body and
handle validation succeed.

## Implementation transition

The first protocol-runtime implementation should make this decision directly rather than
implementing the earlier provisional 80-byte form and migrating immediately afterward.

Required work is:

1. define `NswHeaderV1` as an explicitly encoded 64-byte record;
2. add compile-time constants for every field offset and packet-kind value;
3. generate canonical header test vectors;
4. update negotiation roots to 48-byte request and 64-byte response records;
5. implement UUIDv4 source validation and RFC-order binary conversion;
6. add `nsidlc new-id` using a cryptographic random source;
7. store the bound protocol UUID and service generation in connection state;
8. test rejection of Windows mixed-endian GUID encodings;
9. test every packet-kind field relationship;
10. measure the 64-byte header under the desktop and current endpoint profiles.

There is no on-disk or runtime migration requirement because the earlier 80-byte header
was a provisional design and has not been implemented as a stable protocol.

## Required invariants

> NSWP 1.0 uses an exact 64-byte base header. Numeric fields are little-endian, byte-array
> identities retain their declared byte order, and the body begins at offset 64.

> One channel connection carries one negotiated protocol. The protocol UUID is exchanged
> during negotiation and retained as immutable connection metadata rather than repeated
> in ordinary packets.

> A protocol-family identifier is an explicitly generated and committed RFC 9562 UUIDv4.
> Source uses lowercase canonical UUID text; compiled forms use the corresponding 16 RFC-
> order octets.

> Protocol UUIDs are opaque byte strings. They are never native-endian integers or Windows
> mixed-endian GUIDs.

> The protocol UUID remains stable across minor and major versions of the same family. The
> major version forms part of the complete protocol key.

> Human-readable library, service, and package names may change without changing protocol
> identity. Neither a name nor a UUID grants authority.

> The 128-bit trace identifier is an independent opaque correlation value and is not a
> UUID.

> The packet header excludes identity, authorization, checksums, descriptor hashes, and
> transport-redundant fields that belong to connection or broker state.

## Remaining measurement questions

The architectural choices in this document are settled. Pilot implementation should
still measure:

- whether the trace identifier materially affects hot control-message cache behavior;
- whether `TRACE_SAMPLED` is useful enough to retain before the stable 1.0 freeze;
- whether any pilot protocol needs a body larger than the 64 KiB desktop profile;
- whether negotiation should gain a later fast path for trusted pre-bound internal
  endpoints without changing ordinary packet layout.

Those measurements may motivate a deliberate pre-1.0 revision, but the implementation
should begin with the exact layout and identifier rules defined here.
