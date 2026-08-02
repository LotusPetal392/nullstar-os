# NSIDL and NullStar Wire Protocol direction

## Status

This document defines a concrete, **provisional** first version of:

```text
NSIDL 1.0
NullStar Interface Definition Language

NSWP 1.0
NullStar Wire Protocol
```

The following are **accepted direction**:

- service interfaces are bounded, language-neutral, ordinal-based definitions;
- one negotiated protocol family, major version, minor version, feature set, and limit
  profile is carried by one channel connection;
- NSWP 1.0 uses the exact 64-byte base header specified below;
- protocol-family identifiers are generated and committed RFC 9562 UUIDv4 values carried
  during negotiation rather than repeated in ordinary packets;
- the wire representation is fixed-width, little-endian, pointer-independent, and
  independent of Rust or C compiler layout;
- tables support additive minor-version evolution while structures retain fixed layouts;
- every dynamic value and message has a declared maximum;
- handles are attached out of band, validated for object type and rights, and adopted
  only after complete message validation;
- generated bindings own negotiation, validation, transactions, cancellation, deadline
  propagation, handle cleanup, and protocol-error behavior;
- bulk data remains outside ordinary RPC messages and uses shared memory with explicit
  synchronization.

This document is the normative NSIDL and NSWP specification. The packet header,
protocol-family identifier rules, negotiation records, and transport-profile arithmetic
specified below are accepted provisional NSWP 1.0 decisions. Their design rationale is
retained in
[NSWP packet header and protocol-identifier decision](nswp-header-and-protocol-identifiers.md).
The exact grammar, body arena, remaining limits, and command names remain **tentative
design pending implementation**. NSIDL and NSWP should not be declared stable 1.0 until
the freeze criteria near the end of this document are met.

This specification refines
[Native application runtime, SDK, and service IDL](application-runtime-sdk-and-idl.md)
and
[IPC, kernel object, and handle model](ipc-and-object-model.md). It does not describe the
currently implemented endpoint ABI. Current behavior remains authoritative in
[Userspace ABI](../syscall-abi.md) and
[Capability and IPC protection model](../protection-model.md).

## Names and artifacts

Source definitions use:

```text
*.nsidl
```

Compiled canonical descriptors tentatively use:

```text
*.nsproto
```

The compiler is tentatively named:

```text
nsidlc
```

A protocol package may contain:

```text
protocols/
├── files.nsidl
├── files.nsidl.lock
└── files.nsproto
```

The source is the developer-authored interface. The lock file preserves compatibility
history. The descriptor is canonical immutable package metadata for generated code,
diagnostics, documentation, tracing, and conformance tools.

## Core connection model

One NSWP connection carries one negotiated protocol family, major version, minor version,
and feature set.

```text
Client endpoint                     Server endpoint
      |                                   |
      |---- negotiation request --------->|
      |<--- selected version/features ----|
      |                                   |
      |---- request or one-way ---------->|
      |<--- response or event ------------|
      |                                   |
      |---- cancellation ---------------->|
```

The underlying transport must provide:

- ordered, reliable, message-boundary-preserving delivery;
- atomic enqueueing of bytes and attached handles;
- bounded queues and explicit backpressure;
- peer-closure notification;
- a defined maximum message size;
- a defined maximum attached-handle count.

The intended transport is a NullStar channel endpoint. NSWP does not define the kernel
syscalls that implement that transport.

## Directionality

The underlying channel is bidirectional, but one connection has fixed client and server
roles.

| Direction | Allowed packet kinds |
| --- | --- |
| Client to server | negotiation request, request, one-way, cancel |
| Server to client | negotiation response, response, event |
| Either direction | protocol error followed by closure |

The server does not make reverse RPC calls over the same connection. If reverse calls are
required, a client transfers a separate typed protocol endpoint.

This rule:

- prevents transaction-identifier collisions;
- reduces synchronous call cycles;
- makes generated dispatch simpler;
- keeps peer roles visible in protocol review;
- lets reverse authority be represented by an explicit transferred capability.

## Source encoding and lexical rules

NSIDL source is UTF-8.

Version 1 identifiers are ASCII:

```text
[A-Za-z_][A-Za-z0-9_]*
```

Library segments should be lowercase:

```text
nullstar.portal.files
nullstar.system.configuration
org.example.editor.protocols
```

Comments use:

```text
// line comment

/* block comment */
```

Numeric literals may be decimal or hexadecimal and may contain underscores:

```text
42
0x1000
65_536
```

String literals are UTF-8 and use ordinary escaped characters:

```text
"system.portal.files"
"permission.microphone"
```

The compiler must reject invalid UTF-8, duplicate declarations, ambiguous imports,
identifier normalization tricks, and unsupported source-version directives.

## Complete NSIDL example

```text
library nullstar.portal.files;

use nullstar.storage.File;
use nullstar.ui.WindowToken;

const MAX_ACCEPTED_TYPES: u32 = 32;
const MAX_SELECTED_FILES: u32 = 64;

type GrantId = id128;

open enum FileAccess : u32 {
    @1 Read;
    @2 ReadWrite;
}

open enum OpenFileError : u32 {
    @1 Canceled;
    @2 AccessDenied;
    @3 ResourceUnavailable;
    @4 UnsupportedContentType;
}

table ContentType {
    @1
    @required
    name: string<127>;
}

table OpenFileRequest {
    @1
    parent_window: client_end<WindowToken>;

    @2
    accepted_types: vector<ContentType, MAX_ACCEPTED_TYPES>;

    @3
    @required
    requested_access: FileAccess;

    @4
    @requires_feature(MultiSelect)
    allow_multiple: bool;
}

table SelectedFile {
    @1
    @required
    file: client_end<File>;

    @2
    @required
    display_name: string<255>;

    @3
    @required
    content_type: ContentType;

    @4
    grant: GrantId;
}

table OpenFileReply {
    @1
    @required
    files: vector<SelectedFile, MAX_SELECTED_FILES>;
}

table GrantRevokedEvent {
    @1
    @required
    grant: GrantId;
}

@id("3c59c73e-852e-4ad8-bb3d-610ca4920727")
@version(1.2)
@stability(public)
@service("system.portal.files")
@limits(max_body = 32768, max_handles = 64, max_outstanding = 32)
protocol FilePortal {
    @1
    @since(1.1)
    feature MultiSelect;

    @2
    @since(1.2)
    feature PersistentGrants;

    @10
    @deadline(required, max = 5m)
    @idempotency(non_repeatable)
    @privacy(private)
    rpc OpenFile(OpenFileRequest)
        -> result<OpenFileReply, OpenFileError>;

    @100
    @since(1.1)
    @delivery(reliable)
    @privacy(private)
    event GrantRevoked(GrantRevokedEvent);

    reserve 11..99;
    reserve 101..127;
}
```

The grammar remains provisional, but every semantic property used by the wire protocol
must have a canonical representation in the compiler's intermediate form.

## Declaration kinds

NSIDL 1 supports:

```text
const
type
struct
table
enum
open enum
union
open union
protocol
feature
```

NSIDL 1 intentionally omits:

- recursive value types;
- user-defined generics;
- protocol inheritance;
- method overloading;
- unbounded strings, byte sequences, or vectors;
- native maps;
- implicit ordinals;
- architecture-sized integers;
- language-native object serialization;
- arbitrary user-defined decode hooks.

These omissions keep code generation, validation, bounds, and compatibility review
tractable for the first platform version.

## Provisional structural grammar

The following grammar is normative for the structure of NSIDL 1. Lexical details,
constant-expression precedence, and documentation-comment syntax may be expanded by the
compiler specification.

```text
source
    = library_decl
      { use_decl | declaration } ;

library_decl
    = "library" qualified_name ";" ;

use_decl
    = "use" qualified_name [ "as" identifier ] ";" ;

declaration
    = annotations const_decl
    | annotations alias_decl
    | annotations struct_decl
    | annotations table_decl
    | annotations enum_decl
    | annotations union_decl
    | annotations protocol_decl ;

annotations
    = { annotation } ;

annotation
    = "@" integer
    | "@" identifier
    | "@" identifier "(" [ annotation_arguments ] ")" ;

const_decl
    = "const" identifier ":" integer_type "=" const_expression ";" ;

alias_decl
    = "type" identifier "=" type_ref ";" ;

struct_decl
    = "struct" identifier "{"
          { annotations identifier ":" type_ref ";" }
      "}" ;

table_decl
    = "table" identifier "{"
          { annotations identifier ":" type_ref ";" | reserve_decl }
      "}" ;

enum_decl
    = [ "open" ] "enum" identifier ":" integer_type "{"
          { annotations identifier [ "=" integer ] ";" | reserve_decl }
      "}" ;

union_decl
    = [ "open" ] "union" identifier "{"
          { annotations identifier "(" type_ref ")" ";" | reserve_decl }
      "}" ;

protocol_decl
    = "protocol" identifier "{"
          { protocol_member | reserve_decl }
      "}" ;

protocol_member
    = annotations "feature" identifier ";"
    | annotations "rpc" identifier
        "(" [ type_ref ] ")"
        "->" type_ref ";"
    | annotations "oneway" identifier
        "(" [ type_ref ] ")" ";"
    | annotations "event" identifier
        "(" [ type_ref ] ")" ";" ;

reserve_decl
    = "reserve" ordinal_range
      { "," ordinal_range } ";" ;

ordinal_range
    = integer
    | integer ".." integer ;
```

Every table field, enum member, union alternative, protocol method, event, and feature
has an explicit positive ordinal. Ordinal zero is reserved.

## Primitive types

| Type | Size | Alignment | Encoding |
| --- | ---: | ---: | --- |
| `bool` | 1 | 1 | `0` or `1` only |
| `u8`, `i8` | 1 | 1 | Signed values use two's complement |
| `u16`, `i16` | 2 | 2 | Little-endian |
| `u32`, `i32` | 4 | 4 | Little-endian |
| `u64`, `i64` | 8 | 8 | Little-endian |
| `f32` | 4 | 4 | IEEE 754 |
| `f64` | 8 | 8 | IEEE 754 |
| `id128` | 16 | 8 | Opaque 16-byte identity |
| `unit` or `()` | 0 | 1 | No bytes |

The wire language has no:

```text
usize
isize
native pointer
Rust char
compiler-native enum
compiler-native structure
```

## Floating-point canonicalization

Encoders normalize NaN values to:

```text
f32: 0x7fc00000
f64: 0x7ff8000000000000
```

Strict NSWP decoders reject noncanonical NaN encodings. Positive and negative infinity
remain valid unless the semantic field definition forbids them.

Protocols that need exact bit preservation should use fixed-width integer or byte-array
fields instead of floating-point values.

## Strings

Strings use:

```text
string<MaximumBytes>
```

A string:

- contains valid UTF-8;
- has no implicit or required terminating NUL;
- may contain U+0000, whose UTF-8 byte `00` is included in `count` like any other
  encoded code point;
- has an explicit maximum byte length;
- is not automatically normalized by the wire runtime.

A semantic protocol may separately require NFC or another normalization form.
Filesystem names, foreign encodings, and opaque binary identifiers use byte sequences or
specialized semantic types when arbitrary bytes are required.

## Byte sequences

Arbitrary bytes use:

```text
bytes<MaximumBytes>
```

They have no text interpretation and no implicit zero terminator.

## Fixed arrays

Fixed arrays use:

```text
array<T, Count>
```

`Count` is a compile-time constant. The encoded array contains exactly that many values.

## Vectors

Vectors use:

```text
vector<T, MaximumElements>
```

Every vector is bounded. The compiler computes its maximum inline and recursive encoded
size and rejects a protocol whose maximum exceeds declared transport limits.

## Optional values

An optional uses:

```text
optional<T>
```

It is encoded as a specialized union:

```text
ordinal 0 = None
ordinal 1 = Some(T)
```

Table fields are already optional by presence. `optional<T>` in a table is used only
when a protocol must distinguish:

```text
field absent
field present with None
field present with Some(value)
```

## Results

Typed method outcomes use:

```text
result<Success, Error>
```

A result is a closed union:

```text
ordinal 1 = Success
ordinal 2 = Error
```

Transport failures are not encoded as the error branch. They remain separate packet or
local-runtime outcomes.

## Structures

A `struct` is a closed, fixed-layout value.

```text
struct Point {
    x: f32;
    y: f32;
}
```

Rules:

- every field is present;
- fields appear in declaration order;
- field offsets are deterministic;
- padding bytes are zero;
- adding, removing, reordering, or changing a field requires a protocol-major change.

Structures are appropriate for:

- small fixed records;
- values whose shape is genuinely stable;
- standard wire-control records;
- high-frequency fixed-layout metadata.

They should not be the default for public request and response objects expected to grow.

## Tables

A `table` is an evolvable set of ordinal-addressed fields.

```text
table WindowOptions {
    @1 title: string<255>;
    @2 width: u32;
    @3 height: u32;
}
```

Rules:

- fields are optional on the wire unless marked `@required`;
- envelopes are sorted by ordinal;
- duplicate ordinals are malformed;
- unknown ordinals are skipped safely;
- removed ordinals remain reserved forever;
- new fields use new ordinals;
- fields may be gated by selected minor version or negotiated feature.

A required field introduced in minor 1.2 is required only when the selected connection
version is at least 1.2.

```text
@4
@since(1.2)
@required
new_field: u32;
```

## Closed enums

A closed enum rejects unknown values.

```text
enum ColorSpace : u32 {
    @1 Srgb;
    @2 DisplayP3;
}
```

The encoded numeric value is the member ordinal unless an explicit compatible value is
provided. Version 1 should prefer the ordinal as the value to avoid two independent
numbering systems.

## Open enums

An open enum preserves unknown numeric values in generated bindings.

```text
open enum DeviceState : u32 {
    @1 Online;
    @2 Offline;
}
```

A generated Rust binding may represent a newer value as:

```rust
DeviceState::Unknown(u32)
```

An older receiver must not invent semantics for the unknown value.

## Closed unions

A closed union rejects an unknown alternative ordinal.

It is appropriate where every alternative changes safety-critical interpretation and an
older implementation cannot continue safely without understanding the value.

## Open unions

An open union is intended to accept an unknown alternative ordinal as an unknown value,
but unknown-alternative body and handle behavior remains tentative and is outside the
current handle-free body-codec milestone. A candidate decoder skips the unknown payload
and closes all handles belonging to that alternative. Opaque round-trip preservation of
arbitrary unknown nested payloads is deferred because it would require retaining
unvalidated wire regions and ownership state.

## Type aliases and constants

A type alias gives a semantic name to an existing type:

```text
type GrantId = id128;
```

An alias does not create a distinct wire representation. Generated languages may choose
newtype wrappers where that improves type safety.

Constants may use integer types and bounded constant expressions. They must be fully
resolved by the compiler and included in descriptor and compatibility metadata.

## Kernel-object handles

A kernel-object handle field may declare both minimum required rights and maximum allowed
rights:

```text
handle<
    Memory,
    required = READ | MAP,
    allowed = READ | MAP
>
```

The receiver validates:

```text
actual object type == Memory
required rights is a subset of actual rights
actual rights is a subset of allowed rights
```

If `allowed` is omitted, it equals `required`.

A protocol may deliberately permit limited extra authority:

```text
handle<
    Memory,
    required = READ | MAP,
    allowed = READ | MAP | WAIT
>
```

This design prevents a sender from accidentally delegating write or management rights
that the receiver does not need.

## Typed protocol endpoints

A protocol endpoint uses:

```text
client_end<nullstar.storage.File>
server_end<nullstar.storage.File>
```

A typed endpoint is physically a channel handle. After transfer, the receiver performs
normal negotiation for the named protocol before sending ordinary packets.

The additional setup round trip keeps NSWP 1 explicit and ensures version, feature,
limit, and service-generation state are established for every new protocol connection.

The generic form:

```text
handle<Any, ...>
```

is prohibited in stable public protocols. It may be used only by reviewed experimental
or internal system protocols.

## Core annotations

NSIDL 1 recognizes the following annotation families.

### Identity and stability

```text
@id("UUID")
@version(1.2)
@stability(public)
@stability(system)
@stability(experimental)
@service("system.configuration")
```

### Availability

```text
@since(1.1)
@deprecated(1.4)
@requires_feature(FeatureName)
```

### Field validation

```text
@required
```

### Deadlines

```text
@deadline(required)
@deadline(optional)
@deadline(forbidden)
@deadline(required, max = 5s)
```

### Retry semantics

```text
@idempotency(idempotent)
@idempotency(retry_safe)
@idempotency(non_repeatable)
```

### Event delivery

```text
@delivery(reliable)
@delivery(lossy)
```

Reliable is the default.

A lossy event:

- may be dropped under backpressure;
- may not carry handles;
- must not represent an unrecoverable state transition;
- must permit later resynchronization or remain purely advisory.

### Privacy

```text
@privacy(public)
@privacy(private)
@privacy(sensitive)
@privacy(secret)
@privacy(opaque)
```

These annotations control generated tracing and diagnostics.

`secret` values are never emitted as payload values in logs or traces. `opaque` values
may expose size and timing but not content.

### Protocol limits

```text
@limits(
    max_body = 32768,
    max_handles = 16,
    max_outstanding = 64
)
```

The compiler computes method-specific maxima and rejects definitions that exceed the
protocol or transport profile.

## Protocol identity

Each protocol family has:

```text
one generated and committed RFC 9562 UUIDv4 family ID
one major version
one maximum supported minor version
zero or more feature IDs
```

The complete protocol key is:

```text
ProtocolKey {
    protocol_id: ProtocolId,
    major: u16,
}
```

The UUID identifies the protocol family, not an individual version. Minor and major
versions of the same family retain the same UUID; the major version forms the incompatible
part of the complete protocol key. A genuinely different protocol family or independently
developed incompatible fork allocates a new UUID.

Example:

```text
@id("3c59c73e-852e-4ad8-bb3d-610ca4920727")
@version(1.2)
protocol FilePortal
```

The accepted NSIDL source form is exactly:

```text
xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx
```

where `x` is a lowercase hexadecimal digit and `y` is `8`, `9`, `a`, or `b`. The value
must contain exactly 36 ASCII characters, with hyphens at positions 8, 13, 18, and 23,
the version nibble set to `4`, and the RFC variant bits set to binary `10`. Nil and
all-ones UUIDs are prohibited. The compiler rejects noncanonical spelling rather than
silently rewriting source.

In negotiation bodies, lock files, and `.nsproto` descriptors, `protocol_id` is exactly 16
octets in RFC UUID byte order. For example:

```text
NSIDL text:
00112233-4455-4677-8899-aabbccddeeff

Binary octets:
00 11 22 33 44 55 46 77 88 99 aa bb cc dd ee ff
```

The UUID is treated as opaque bytes with byte-for-byte equality. It is never encoded as a
native-endian integer or Windows mixed-endian GUID. Bindings should represent it
conceptually as:

```rust
#[repr(transparent)]
pub struct ProtocolId([u8; 16]);
```

The toolchain generates an ID once from a cryptographic random source, installs the RFC
UUIDv4 version and variant bits, emits lowercase canonical text, and commits the result to
the `.nsidl` source, `.nsidl.lock` history, and `.nsproto` descriptor. Reproducible builds
consume that committed value and never generate a replacement. The tentative command is:

```text
nsidlc new-id
```

The protocol UUID appears in negotiation requests and responses, package and broker
metadata, descriptors, lock files, and diagnostics. It does not appear in ordinary
requests, responses, one-way messages, events, cancellation packets, or post-negotiation
protocol errors. After successful negotiation, the runtime retains the UUID, selected
version, feature set, limits, and service generation as immutable connection metadata.

The service name is discovery metadata. The protocol-family UUID is wire identity. Neither
a name nor a UUID grants authority, identifies a trusted publisher, or authorizes service
lookup.

## Major versions

A major version changes when compatibility cannot be preserved, including:

- changing an existing field type;
- changing a structure layout;
- changing ownership or handle-transfer semantics;
- removing a method;
- reusing an ordinal;
- changing a method from idempotent to non-repeatable;
- changing an existing union alternative;
- changing the security meaning of an existing field or operation.

Multiple major versions may coexist during migration.

## Minor versions

A minor version may add:

- methods with unused ordinals;
- events with unused ordinals;
- table fields with new ordinals;
- open-enum values;
- open-union alternatives;
- optional features;
- optional diagnostics.

Every addition declares `@since`.

## Ordinal rules

Within one protocol major:

- ordinal zero is reserved;
- ordinals are explicit and never inferred from source order;
- an ordinal is never reused;
- deleted ordinals remain reserved;
- renaming does not change ordinal or semantic identity;
- methods and events share one protocol-member namespace;
- feature ordinals use a separate feature namespace;
- table fields and union alternatives use namespaces local to their type.

## Wire header

Every NSWP 1.0 packet begins with exactly 64 bytes:

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

This declaration documents names and offsets. Implementations encode and decode the
explicit wire layout and must not copy a compiler-native Rust or C structure. All numeric
fields are unsigned little-endian integers. Byte arrays retain their declared byte order:
`magic` is four literal octets and `trace_id` is sixteen opaque octets.

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

The magic remains constant across NSWP versions. `wire_major` and `wire_minor` identify
the supported wire format.

## Header size and wire version

For NSWP 1.0:

```text
header_bytes = 64
wire_major = 1
wire_minor = 0
```

A conforming NSWP 1.0 receiver rejects any other `header_bytes` value. A future supported
wire version may add fields after byte 63, but an extended header size must be a multiple
of eight and may only be skipped after the implementation recognizes the wire version.
The NSWP wire version is independent of the selected service-protocol version.

## Header flags

NSWP 1.0 defines one flag:

```text
0x01  TRACE_SAMPLED
```

All other bits are reserved and must be zero. `TRACE_SAMPLED` is a diagnostic hint that
requests detailed timing retention; it grants no authority, permits no payload logging,
and does not override NSIDL privacy classifications. When set, `trace_id` must not be all
zero. A nonzero `trace_id` may be present without `TRACE_SAMPLED`.

## Reserved fields

For NSWP 1.0, `reserved0` and `reserved1` are zero. A receiver rejects nonzero reserved
fields, and senders must not use them for private extensions.

## Service-protocol version fields

After successful negotiation, every ordinary packet carries the exact selected service
version in `protocol_major` and `protocol_minor`; the receiver requires an exact match with
immutable connection state. `NegotiateRequest` and `NegotiateResponse` use `0.0` because
the requested protocol family and major version are in the negotiation body. A
pre-negotiation `ProtocolError` also uses `0.0`; a post-negotiation `ProtocolError` uses the
selected version.

## Body length

`body_bytes` is the exact number of bytes following the complete header:

```text
total transport bytes = 64 + body_bytes
```

It is a multiple of eight, the body begins at transport-message offset 64, and no trailing
bytes follow the declared body. Attached handles are not included in `body_bytes`. The
body continues to use the canonical NSWP self-relative arena encoding specified below and
must not exceed the negotiated connection limit.

## Attached handles

`handle_count` exactly equals the number of handles attached by the transport, including
every nested handle domain represented in the body. A mismatch is fatal to the connection
and causes every received attachment to be closed. Negotiation, cancellation,
protocol-error, and non-`Ok` response packets carry zero handles.

## Deadlines

`deadline_ns` is an absolute timestamp from the NullStar monotonic clock, measured in
nanoseconds.

```text
0xffffffffffffffff = no deadline
0x0000000000000000 = already expired
```

The value is meaningful only within the current boot's monotonic clock domain. It is never
persisted or interpreted as wall-clock time.

## Trace identifier

`trace_id` is an opaque 128-bit correlation identifier:

```text
all zero = no trace correlation
nonzero  = active trace correlation
```

It is not a UUID and has no UUID version or variant bits. A client creates or inherits a
request trace identifier; the response and cancellation packet echo it exactly. A server
event may use a server-generated trace identifier, and nested RPCs normally retain the
same identifier. Local span identifiers belong to tracing metadata rather than the packet
header.

The trace identifier grants no authority and must not influence authentication,
authorization, routing, or retry behavior.

## Packet kinds

```text
0 = Invalid
1 = NegotiateRequest
2 = NegotiateResponse
3 = Request
4 = Response
5 = OneWay
6 = Event
7 = Cancel
8 = ProtocolError
```

## Packet-kind requirements

| Kind | Transaction ID | Ordinal | Body |
| --- | ---: | ---: | --- |
| `NegotiateRequest` | 0 | 0 | Negotiation request |
| `NegotiateResponse` | 0 | 0 | Negotiation response |
| `Request` | Nonzero | Method ordinal | Request value |
| `Response` | Same as request | Same as request | Response or empty transport error |
| `OneWay` | 0 | Method ordinal | Request value |
| `Event` | 0 | Event ordinal | Event value |
| `Cancel` | Original transaction | Original method ordinal | Empty |
| `ProtocolError` | Related transaction or 0 | Related ordinal or 0 | Fixed error record |

A packet kind used in the wrong channel direction is a protocol error.

## Transport status

`transport_status` is meaningful only in a `Response`.

```text
0 = Ok
1 = Canceled
2 = TimedOut
3 = Overloaded
4 = ResourceExhausted
5 = Unavailable
6 = AccessDenied
7 = BadState
8 = NotSupported
9 = Internal
```

If `transport_status` is not `Ok`:

```text
body_bytes = 0
handle_count = 0
```

Service-domain failures use the declared response type, normally a `result`.

Local errors such as peer closure, malformed reply, encode failure, service-generation
replacement, or failure before enqueueing are generated by the client runtime rather
than received as a transport status.

## Connection state machine

A fresh typed endpoint begins in:

```text
NEW
  |
  v
NEGOTIATING
  |
  v
BOUND
  |
  v
CLOSED
```

No ordinary request, one-way message, response, or event may be sent before negotiation
completes.

Renegotiation on the same connection is prohibited.

## Negotiation request

The fixed request root is exactly 48 bytes:

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

The `protocol_id` uses RFC UUID byte order. Requirements are:

```text
protocol_major != 0
min_minor <= max_minor
flags = 0
max_body_bytes > 0
max_handles <= transport attachment limit
max_outstanding > 0
```

Feature records follow through the unchanged canonical body arena. Negotiation packets
never carry handles.

## Negotiation response

The fixed response root is exactly 64 bytes:

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

`reserved0` is always zero. Every response, including failure, exactly echoes the
request's `protocol_id` and `protocol_major`. On success, `selected_minor` lies within both
supported ranges, returned limits are the negotiated minima, returned feature records are
sorted and enabled, and `service_generation` is nonzero. The generation identifies the current disposable
provider incarnation for tracing, failure attribution, and reconnect decisions; it grants
no authority and is not a process ID.

On failure:

```text
selected_minor = 0
max_body_bytes = 0
max_handles = 0
max_outstanding = 0
features = empty
service_generation = 0
```

The server minor range may be returned for failures concerning a recognized protocol key.
`UnsupportedProtocol` and `UnsupportedMajor` return a zero server-minor range so the
response never associates another protocol key or major version with unrelated minors.

## Feature records

```rust
#[repr(C)]
struct NswFeatureRecordV1 {
    id: u32,
    flags: u32,
}
```

Request flags:

```text
0 = optional
1 = required
```

Response flags are zero.

Feature records are sorted by ascending feature ID and contain no duplicates.

## Negotiation statuses

The exact response statuses are:

```text
0 = Ok
1 = UnsupportedProtocol
2 = UnsupportedMajor
3 = NoCommonMinor
4 = RequiredFeatureUnavailable
5 = TransportBoundsTooSmall
6 = PolicyDenied
7 = Busy
8 = Internal
```

`UnsupportedProtocol` means the provider does not implement the requested UUID.
`UnsupportedMajor` means it recognizes the protocol family but not the requested major
version. On negotiation failure, the server sends the response and closes the connection.

## Version and feature selection

The server advertises a contiguous supported minor-version range and selects the highest
minor version satisfying:

```text
client minimum <= selected <= client maximum
server minimum <= selected <= server maximum
selected version's baseline types fit negotiated body and handle bounds
all required features are available and policy-permitted at the selected minor
all required features' types fit negotiated body and handle bounds
```

Each feature declaration carries its introduction minor. Generated protocol metadata must
also evaluate the complete selected feature set at a candidate minor against the negotiated
body and handle bounds; checking features independently is insufficient because combinations
can enlarge the same message. Optional features are considered in ascending feature-ID order
and admitted only while the complete set remains valid. A required feature that is unavailable
prevents selecting that minor; a required feature set whose types exceed the negotiated bounds
produces `TransportBoundsTooSmall` when no lower compatible minor succeeds. Clients run the
same complete-set validation over returned features before binding the connection.

The final connection limits are the minimum of:

```text
client limit
server limit
transport limit
protocol-declared limit
```

The chosen protocol family, version, features, limits, and service generation are
immutable for the life of the connection. Every later packet uses the selected major and
minor version exactly, and renegotiation on the same channel is prohibited.

## Body arena

The byte-region, canonical-placement, table, and depth rules in this section define the
precise but still tentative target for the current **handle-free** body-codec milestone.
They do not stabilize the body format. The first implementation slice covers primitive
and fixed-structure roots, strings and byte sequences, tables, and closed results. Generic
`vector<T>`, optionals, enums, general closed unions, handle references, handle domains,
canonical handle ordering, and unknown open-union alternatives remain separate tentative
work and are not required by this implementation slice.

NSWP bodies use a relocatable, self-relative, eight-byte-aligned arena. The root semantic
value starts at body offset zero. Out-of-line values use relative references. No wire
value contains a process virtual address.

A region occupies the half-open byte interval `[start, start + bytes)`. Every nonempty
out-of-line region:

- begins on an eight-byte boundary;
- is referenced by a positive forward offset;
- has an exact length that is a multiple of eight;
- lies entirely within the allocation of the semantic value that owns the reference.

Region nesting follows semantic ownership. A descendant region is contained in every
ancestor region that accounts for it, so ancestor containment is legal and expected.
Regions belonging to distinct siblings, or to descendants of distinct siblings, are
disjoint. Partial overlap between regions that have no ancestor relationship, overlap
between siblings, and aliasing one region from multiple references are malformed.

## Slice reference

Strings, bytes, and vectors use:

```rust
#[repr(C)]
struct NswSliceRefV1 {
    relative_offset: u32,
    count: u32,
    region_bytes: u32,
    reserved0: u32,
}
```

The encoded size is 16 bytes.

`relative_offset` is measured from the address of the `relative_offset` field to the
first byte of the target region.

For an empty value:

```text
relative_offset = 0
count = 0
region_bytes = 0
reserved0 = 0
```

For a nonempty value:

- the target is eight-byte aligned;
- the offset is positive;
- `region_bytes` is a nonzero multiple of eight;
- the inline backing data and all of its descendant regions lie inside the target region.

`region_bytes` measures only the target region referenced by this slice. It excludes the
16-byte `NswSliceRefV1` itself. If the slice is an envelope payload, the envelope's
`payload_bytes` separately accounts for the payload root, including that slice reference,
and all descendants.

For strings and bytes:

```text
region_bytes = align_up(count, 8)
```

The first `count` bytes are the value; remaining bytes are zero alignment padding. A
string has no implicit terminator. A counted `00` byte may encode U+0000 and is data,
whereas any bytes after `count` are padding.

For vectors, `region_bytes` includes the inline element array, padding of that inline
array to eight bytes, all out-of-line descendants of those elements, and any required
trailing alignment padding.

## Table reference

```rust
#[repr(C)]
struct NswTableRefV1 {
    relative_offset: u32,
    field_count: u16,
    reserved0: u16,
    region_bytes: u32,
    reserved1: u32,
}
```

The encoded size is 16 bytes.

`field_count` is exactly the number of present field envelopes, not the greatest ordinal
and not the number of fields declared in the schema. Absent fields have no envelope.
Every present envelope has a nonzero ordinal; ordinal zero is invalid even when the field
is unknown to the decoder.

For a nonempty table, `relative_offset` is positive, the target is eight-byte aligned,
and `region_bytes` is a nonzero multiple of eight. The referenced region begins with
exactly `field_count` sorted field envelopes. The remaining region contains their
nonempty payload allocations in ascending ordinal order.

`region_bytes` measures only this referenced target region: the envelope array, its
padding, and all field payload allocations and descendants. It excludes the 16-byte
`NswTableRefV1` itself. If the table is an envelope payload, `payload_bytes` includes both
the table-reference root and the table's target region.

For an empty table, `relative_offset`, `field_count`, both reserved fields, and
`region_bytes` are all zero.

## Handle reference (tentative, outside the milestone)

The following representation is a tentative future extension. The handle-free body-codec
milestone does not emit `NswHandleRefV1`; every milestone packet has
`header.handle_count = 0`.

```rust
#[repr(C)]
struct NswHandleRefV1 {
    index: u16,
    reserved0: u16,
    reserved1: u32,
}
```

The encoded size is eight bytes.

The index is relative to the current handle domain.

## Envelope format

Tables, unions, results, and optionals use a 24-byte envelope.

```rust
#[repr(C)]
struct NswEnvelopeV1 {
    ordinal: u32,
    flags: u16,
    reserved0: u16,

    payload_relative_offset: u32,
    payload_bytes: u32,

    handle_start: u16,
    handle_count: u16,

    reserved1: u32,
}
```

The fields mean:

- `ordinal` identifies the table field or union alternative;
- `flags` is zero in NSWP 1;
- `payload_relative_offset` is measured from its own field address;
- `payload_bytes` is the complete allocation of the semantic payload rooted at the
  target, including its eight-byte-padded inline root and all descendants;
- `handle_start` and `handle_count` are reserved for the tentative child-handle-domain
  design;
- all reserved values are zero.

`payload_bytes` differs from slice and table `region_bytes`: `payload_bytes` begins at the
typed payload root and includes that root plus all descendants, while `region_bytes`
begins at the out-of-line target owned by a slice or table reference and excludes the
reference itself.

The payload fields form an exact pair. If there is no payload:

```text
payload_relative_offset = 0
payload_bytes = 0
```

For a nonempty payload, `payload_relative_offset` is positive, its target is eight-byte
aligned, and `payload_bytes` is a nonzero multiple of eight. A zero offset with nonzero
bytes, or a nonzero offset with zero bytes, is malformed.

For the handle-free body-codec milestone every envelope, including an unknown field's
envelope, must contain:

```text
handle_start = 0
handle_count = 0
```

Any other value is malformed for this milestone.

## Optional encoding

An optional uses one envelope:

```text
all-zero envelope = None
ordinal 1 = Some
```

No other ordinal is valid in NSWP 1.

## Result encoding

A result uses one envelope:

```text
ordinal 1 = Success
ordinal 2 = Error
```

## Root and envelope payload padding

The inline root of a body and the inline root of every non-unit envelope payload are
padded with zero bytes to an eight-byte boundary before any out-of-line child is emitted.
This boundary padding is included in `body_bytes` or `payload_bytes`, respectively. It is
not inserted around an ordinary value embedded inline in a structure, array, or vector;
those values follow their normal inline layout.

A direct `unit` body has `body_bytes = 0`. A selected envelope branch whose declared
payload type is `unit` is represented by its nonzero branch ordinal with:

```text
payload_relative_offset = 0
payload_bytes = 0
```

No synthetic byte or eight-byte allocation is emitted for `unit`. Thus a unit branch is
present because of its envelope ordinal even though it has no payload allocation. An
all-zero optional envelope remains `None`, distinct from `Some(unit)`, whose ordinal is
one and whose other fields are zero.

## Handle domains (tentative, outside the milestone)

The following domain and adoption rules remain a tentative future extension. In the
current handle-free milestone the header handle count and every envelope handle field are
zero, so no handle domain is created.

The packet begins with one root handle domain:

```text
attachments[0 .. header.handle_count]
```

Every envelope creates a child domain:

```text
parent_domain[
    envelope.handle_start
    ..
    envelope.handle_start + envelope.handle_count
]
```

Within the envelope payload, a handle reference is relative to that child domain.

This design ensures that:

1. an unknown table field's handles can be closed without decoding the field;
2. a malformed field cannot reference a sibling field's handles;
3. handle adoption remains tree-shaped and auditable;
4. skipped future fields cannot leak capabilities into old code.

## Canonical handle ordering (tentative, outside the milestone)

Handles are tentatively assigned in depth-first wire traversal order.

Sibling envelope handle ranges:

- follow field-ordinal order;
- never overlap;
- form a contiguous partition of the handles delegated to the sibling set.

Within a known payload:

- every handle is referenced exactly once;
- every reference is in range;
- nested domains do not overlap;
- no attachment remains unused.

If a table field or open-union alternative is unknown, the runtime closes its complete
child handle domain.

## Two-phase handle adoption (tentative, outside the milestone)

The future handle-bearing decoder is intended to operate in two phases:

```text
1. validate the complete packet, value tree, and attachment domains
2. adopt attachments into generated owned objects
```

Application or service code never sees partially validated ownership.

On any validation failure, every attached handle is closed.

## Structure layout

Structure fields appear in declaration order.

Each field begins at:

```text
align_up(current_offset, min(field_alignment, 8))
```

Structure alignment is the greatest field alignment, capped at eight. Final size is
padded to that alignment. Every padding byte is zero.

Example:

```text
struct Example {
    a: u8;
    b: u64;
    c: u16;
}
```

Layout:

```text
offset 0: a
offset 1..7: zero padding
offset 8: b
offset 16: c
offset 18..23: zero padding
size: 24
alignment: 8
```

## Canonical body ordering

Every body is one tree-shaped allocation whose root value begins at offset zero. Placement
is deterministic **inline-first depth-first**. The encoder and strict decoder apply this
cursor algorithm to the root allocation and recursively to every region with children:

1. Emit the owner's complete inline footprint first. For a body root or envelope payload,
   this is the typed inline root padded with zeros to eight bytes. For a slice target it is
   the inline byte or element data padded to eight bytes; for a table target it is the
   complete envelope array, already a multiple of eight.
2. Set `cursor` to the first byte after that padded inline footprint.
3. Visit direct out-of-line children in the canonical order below. An empty child emits no
   region and does not move the cursor. For every nonempty child, require
   `child.start == cursor`, encode and account for that child's complete region and all of
   its descendants before visiting the next sibling, then set `cursor = child.end`.
4. After the last child, require `cursor == owner.end`. The applicable `body_bytes`,
   envelope `payload_bytes`, slice `region_bytes`, or table `region_bytes` must equal
   `owner.end - owner.start` exactly.

The equality checks are normative: there is no alignment gap before the first child, no
gap between children, and no unaccounted tail after the last child. Required zero padding
belongs to the preceding inline footprint and is not a gap.

### Structure children

Out-of-line children referenced by structure fields appear in field declaration order.
A child's complete subtree precedes the next field's child.

### Array and vector children

Out-of-line children appear in element-index order, using field declaration order within
an aggregate element. A child's complete subtree precedes the next child's region. The
inline array for a vector is emitted in full before any element child.

### Table children

A table is emitted as:

1. the inline table reference;
2. its target region beginning with all present field envelopes in ascending ordinal
   order;
3. each nonempty field payload allocation in the same ascending ordinal order.

Each field payload's complete subtree precedes the next field payload.

### Union, result, and optional children

The inline envelope is emitted first. A non-unit selected payload and its complete subtree
follow it; an absent optional or selected unit branch emits no payload region.

## Canonical body constraints

- every nonempty out-of-line target is eight-byte aligned and forward of its reference;
- every declared region length is exact and a multiple of eight;
- ancestor regions contain their descendant regions;
- sibling regions and their descendant subtrees are disjoint;
- every understood padding byte is zero;
- table envelopes are strictly ordered by nonzero ordinal;
- every reserved value is zero;
- the root cursor ends exactly at `body_bytes`.

A strict decoder rejects a known value whose representation is valid but noncanonical.

An unknown table-field payload is opaque but is not exempt from outer structural and
accounting validation. The decoder validates its envelope flags and reserved fields, the
zero handle fields required by this milestone, the offset/length pair, eight-byte target
alignment, containment in the table region, sibling disjointness, canonical cursor
position, and exact consumption of `payload_bytes`. Those bytes advance the parent cursor
as one opaque allocation. The decoder does not interpret the unknown payload's type,
follow references within it, validate its UTF-8 or semantic bounds, or classify and check
padding inside it. Consequently it does not recursively canonicalize bytes whose schema
it does not know.

## Semantic nesting depth

Depth is counted over typed semantic values, not arena-region containment. The operation's
root semantic value has depth 1. Whenever decoding enters a nested typed value or payload,
its depth is its semantic parent's depth plus one. This includes structure fields, array
and vector elements, present table-field payloads, and selected union, result, or optional
payloads. Multiple sibling values have the same depth; their number does not accumulate
as depth. Envelope arrays, references, padding, and the raw backing octets of strings and
bytes are representation details and do not add depth.

Before entering a nested semantic value, the decoder requires the resulting depth not to
exceed the negotiated profile's maximum nesting depth. Unknown table-field payload bytes
remain opaque and therefore consume one field-payload depth level but are not traversed
for additional semantic depth.

## Table validation

A decoder validates a table in this order:

1. validate the table reference, reserved fields, offset/length pair, alignment, and region
   bounds;
2. interpret `field_count` as the exact number of present envelopes and require the
   `field_count * 24` byte envelope array to fit at the start of the table region;
3. require every envelope ordinal to be nonzero and strictly increasing, thereby rejecting
   ordinal zero and duplicates, and reject ordinals reserved by the selected schema;
4. validate every envelope's flags, reserved fields, payload offset/length pair, alignment,
   and containment, and require both handle fields to be zero for this milestone;
5. starting immediately after the envelope array, require each nonempty payload to begin
   at the current canonical cursor in ordinal order, advance by exactly `payload_bytes`,
   and thereby prove sibling disjointness;
6. recursively validate known field payloads and their semantic bounds;
7. treat unknown field payloads as opaque while retaining all outer structural, cursor,
   bounds, and byte-accounting checks;
8. verify required fields for the selected minor and feature set;
9. require the final cursor to equal the exact end declared by table `region_bytes`.

A zero-field table is valid only in the all-zero reference form. For a nonempty table,
`region_bytes` includes both the envelope array and every present nonempty payload, with no
gaps or unaccounted tail.

A sender must not emit a field whose:

- `@since` version exceeds the selected minor;
- required feature was not negotiated;
- ordinal is reserved.

## Request packets

A request has:

```text
kind = Request
transaction_id != 0
ordinal = method ordinal
transport_status = 0
```

Transaction identifiers are selected by the client runtime and remain unique among
currently outstanding calls on that connection.

The server may complete requests out of order.

## Response packets

A response:

- echoes the request transaction identifier;
- echoes the method ordinal;
- uses the selected protocol minor;
- echoes the request trace identifier;
- contains either a successful response body or a nonzero transport status.

A duplicate response is a protocol error.

A response to a recently canceled transaction is drained and discarded safely. A
response to an otherwise unknown transaction is a protocol error.

The client runtime should retain a bounded recently canceled transaction set so ordinary
cancellation races do not close a healthy connection.

## One-way messages

A one-way method has no response.

Successful send means only:

> The message and attached handles were atomically queued to the peer.

It does not mean the receiver processed or durably committed the operation.

One-way methods are suitable for:

- advisory notifications;
- telemetry;
- invalidations followed by later state resynchronization;
- acknowledgments whose loss is explicitly harmless.

They are not suitable for:

- durable storage mutation;
- package installation;
- privilege or identity changes;
- destructive device operations;
- any operation whose completion must be known.

## Events

Events flow from server to client.

Reliable events use normal queue backpressure. A service should not block a
latency-critical worker indefinitely while emitting an event. High-volume event streams
should use shared-memory queues plus notifications.

Lossy events may be dropped when the outgoing queue is full. They cannot carry handles.
The runtime records dropped-event counts in structured diagnostics.

## Cancellation packets

A cancellation packet contains:

```text
kind = Cancel
transaction_id = original request transaction
ordinal = original method ordinal
body_bytes = 0
handle_count = 0
```

Cancellation means:

> The client no longer needs the result.

It does not imply rollback.

### Server cancellation behavior

- a queued request may be removed and completed as canceled;
- an executing request receives a cancellation token;
- a completed or already-replied request ignores the cancellation;
- an unknown old transaction may be ignored to tolerate ordinary races.

### Client cancellation behavior

Dropping a generated call future:

1. removes the local waiter;
2. sends cancellation if the request was already queued;
3. records the transaction in the bounded late-reply set;
4. closes handles contained in a discarded late reply.

## Long-running operations

A long-running operation should return its own endpoint rather than hold an ordinary RPC
open indefinitely.

```text
rpc StartExport(ExportRequest)
    -> result<client_end<ExportOperation>, ExportError>;
```

The operation protocol may provide:

```text
Cancel
Progress event
Completed event
Failed event
```

This makes ownership, lifecycle, cancellation, and service restart explicit.

## Bulk data and streaming

Control-plane streams may use dedicated protocol endpoints.

High-volume streams use:

```text
control channel
+ shared-memory ring or queue
+ event, notification, or completion object
```

NSIDL describes setup, ownership, format, and synchronization. It does not serialize
every audio frame, video frame, network packet, or large storage block as RPC body data.

## Deadline behavior

A method annotated with:

```text
@deadline(required)
```

must carry a finite deadline.

For:

```text
@deadline(forbidden)
```

`deadline_ns` must be infinite.

The server runtime checks the deadline before dispatch. If it has already expired, the
runtime replies with `TimedOut` and no body or handles.

During execution, expiration triggers the request cancellation token.

A nested call uses:

```text
minimum(caller deadline, local operation limit)
```

## Protocol-error record

Protocol errors use:

```rust
#[repr(C)]
struct NswProtocolErrorV1 {
    code: u32,
    detail: u32,
    related_transaction_id: u64,
    related_ordinal: u32,
    reserved0: u32,
}
```

The encoded size is 24 bytes.

## Protocol-error codes

```text
1  InvalidHeader
2  UnsupportedWireVersion
3  WrongProtocol
4  WrongProtocolVersion
5  UnexpectedPacketKind
6  UnknownOrdinal
7  InvalidBody
8  NoncanonicalBody
9  HandleCountMismatch
10 WrongHandleType
11 InsufficientHandleRights
12 ExcessHandleRights
13 DuplicateTransaction
14 InvalidTransaction
15 LimitExceeded
16 ReservedValueUsed
17 FieldUnavailable
18 InternalRuntimeError
```

After sending or receiving `ProtocolError`, the connection closes.

The packet contains no arbitrary diagnostic string. Detailed information belongs in
structured logs under the protocol's privacy classification.

## Generated error model

A generated call conceptually returns:

```rust
pub enum CallError<E> {
    Transport(TransportError),
    Service(E),
}
```

A transport error may include:

```rust
pub enum TransportError {
    NotSent,
    PeerClosed,
    ServiceRestarted {
        old_generation: u64,
        new_generation: Option<u64>,
    },
    Canceled,
    TimedOut,
    Overloaded,
    ResourceExhausted,
    Unavailable,
    AccessDenied,
    BadState,
    NotSupported,
    ProtocolError,
    EncodeError,
    DecodeError,
    Internal,
}
```

The runtime should preserve whether a request was:

```text
not queued
queued but no reply was observed
completed with a reply
```

That distinction matters for non-repeatable operations.

A channel send failure means attached handles were not transferred. Successful enqueue
followed by peer closure means the operation may have been processed.

## Retry annotations

### Idempotent

Repeating the request has the same intended effect.

### Retry-safe

The protocol carries an operation identity or transaction mechanism that lets the server
detect and resolve duplicate execution.

### Non-repeatable

The operation may have committed even if its response was lost.

The NSIDL 1 runtime does not automatically retry any request. The annotations support:

- generated documentation;
- static review and linting;
- future explicit reconnect helpers;
- trace interpretation;
- safe application-level retry decisions.

Automatic service reconnection must not imply automatic request replay.

## Compatible minor-version changes

The following are allowed within one protocol major when correctly annotated:

- add a method with a new ordinal and `@since`;
- add an event with a new ordinal and `@since`;
- add a table field with a new ordinal and `@since`;
- add an open-enum value;
- add an open-union alternative;
- add an optional feature;
- widen a string or vector bound when transport limits still admit it;
- add deprecation and diagnostic metadata.

## Changes requiring a major version

- alter a structure;
- change an existing field type;
- change or reuse an ordinal;
- remove an existing method;
- narrow an existing bound;
- change method direction;
- strengthen required handle rights;
- broaden maximum allowed handle rights;
- change ownership or transfer semantics;
- change retry semantics to a less safe classification;
- reinterpret a lossy event as a required state transition;
- make an incompatible closed-enum or closed-union change;
- change the security meaning of an existing value.

## Version-gated generated APIs

Generated encoders know the selected protocol minor and enabled features.

Attempting to send a field unavailable in the selected version returns a local encoding
error. Attempting to invoke an unavailable method returns `MethodUnavailable` without
sending a packet.

Bindings should make feature-gated operations visible through typed capability objects
where practical rather than requiring repeated manual feature checks.

## Compatibility lock file

Each protocol library should commit a generated lock file:

```text
protocols.nsidl.lock
```

It records:

- protocol family IDs;
- major and minor history;
- method, event, field, union, and feature ordinals;
- reserved ranges;
- type-shape fingerprints;
- maximum encoded sizes;
- handle object and rights requirements;
- availability, deadline, retry, delivery, and privacy annotations.

The compiler command:

```text
nsidlc check \
    --against protocols.nsidl.lock \
    protocols.nsidl
```

rejects incompatible changes.

After an accepted compatible update:

```text
nsidlc lock protocols.nsidl
```

updates the history.

## Compiled descriptor

`nsidlc` emits a canonical descriptor:

```text
protocols.nsproto
```

The descriptor contains:

- protocol identity and versions;
- feature definitions;
- methods and events;
- request and response type graphs;
- field layouts and bounds;
- handle types and rights;
- privacy, deadline, retry, and delivery metadata;
- documentation strings;
- canonical compatibility fingerprints.

The descriptor is immutable package content. A digest of its canonical representation
may be included in build reports and diagnostics. It is not required in every packet.

Tools may use the descriptor for:

```text
IPC tracing
protocol inspection
compatibility reports
documentation
fuzz generation
wire decoding
service diagnostics
```

Payload decoding remains subject to privacy policy and connection authority.

## Worked encoding example

Consider:

```text
library org.example.echo;

table PingRequest {
    @1
    @required
    message: string<32>;

    @2
    @required
    sequence: u64;
}

@id("01234567-89ab-4cde-8123-456789abcdef")
@version(1.0)
protocol Echo {
    @1
    rpc Ping(PingRequest) -> PingRequest;
}
```

The request contains:

```text
message = "hi"
sequence = 7
```

### Header values

```text
kind = Request
protocol_major = 1
protocol_minor = 0
ordinal = 1
body_bytes = 96
handle_count = 0
transport_status = 0
transaction_id = 42
deadline_ns = infinite
```

### Body layout

```text
offset 0   NswTableRefV1
offset 16  envelope for field ordinal 1
offset 40  envelope for field ordinal 2
offset 64  field 1 payload root: NswSliceRefV1
offset 80  UTF-8 bytes "hi" plus six zero padding bytes
offset 88  field 2 payload: u64 value 7
offset 96  end
```

### Table reference

At body offset zero:

```text
relative_offset = 16
field_count = 2
region_bytes = 80
```

### Field 1 envelope

The `payload_relative_offset` member is itself at body offset 24. The payload begins at
offset 64.

```text
ordinal = 1
payload_relative_offset = 64 - 24 = 40
payload_bytes = 24
handle_start = 0
handle_count = 0
```

### Field 2 envelope

The offset member is at body offset 48. The payload begins at offset 88.

```text
ordinal = 2
payload_relative_offset = 88 - 48 = 40
payload_bytes = 8
handle_start = 0
handle_count = 0
```

### String slice

At body offset 64:

```text
relative_offset = 16
count = 2
region_bytes = 8
```

The referenced bytes are:

```text
68 69 00 00 00 00 00 00
 h  i
```

The first two bytes are counted UTF-8 data and the final six are alignment padding, not a
NUL terminator. The exact literal 96-byte body, with offsets at the left, is:

```text
0000: 10 00 00 00 02 00 00 00 50 00 00 00 00 00 00 00
0010: 01 00 00 00 00 00 00 00 28 00 00 00 18 00 00 00
0020: 00 00 00 00 00 00 00 00 02 00 00 00 00 00 00 00
0030: 28 00 00 00 08 00 00 00 00 00 00 00 00 00 00 00
0040: 10 00 00 00 02 00 00 00 08 00 00 00 00 00 00 00
0050: 68 69 00 00 00 00 00 00 07 00 00 00 00 00 00 00
```

This is the single canonical body representation for the worked value under the
milestone rules.

## Handle-transfer example (tentative, outside the milestone)

This example illustrates the tentative future handle design; it is not a body accepted by
the current handle-free codec milestone.

Suppose a field is:

```text
@1
@required
memory: handle<
    Memory,
    required = READ | MAP,
    allowed = READ | MAP
>;
```

Its envelope may contain:

```text
payload_bytes = 8
handle_start = 0
handle_count = 1
```

The payload is:

```text
NswHandleRefV1 {
    index = 0
}
```

The transport carries one attached handle. The runtime validates:

```text
object type == Memory
rights contain READ | MAP
rights contain no rights outside READ | MAP
```

If the attachment also has `WRITE`, the decoder fails with `ExcessHandleRights` and
closes the connection and attachment rather than silently accepting overdelegation.

## Generated Rust client

A generated client may look like:

```rust
let endpoint = context
    .services()
    .connect::<FilePortal>()
    .await?;

let portal = FilePortalClient::negotiate(endpoint)
    .minor_range(0..=2)
    .optional_feature(FilePortalFeature::MultiSelect)
    .await?;

let reply = portal
    .open_file(OpenFileRequest {
        parent_window: Some(window.token()),
        accepted_types,
        requested_access: FileAccess::ReadWrite,
        allow_multiple: Some(false),
    })
    .deadline(deadline)
    .await?;
```

## Generated Rust server

Generated server code may expose:

```rust
#[async_trait]
impl FilePortalServer for PortalService {
    async fn open_file(
        &self,
        context: RequestContext,
        request: OpenFileRequest,
    ) -> Result<OpenFileReply, OpenFileError> {
        // The request and handles are already fully validated.
    }
}
```

`RequestContext` contains:

```text
transaction identity
absolute deadline
cancellation token
trace identifier
selected protocol version
enabled feature set
trusted connection identity
service generation
```

The implementation does not manually decode headers, match transactions, validate
handles, negotiate versions, send protocol errors, or close malformed attachments.

## Compiler architecture

The compiler should produce one language-neutral intermediate representation.

```text
NSIDL source
    |
    v
Lexer, parser, and semantic validation
    |
    v
Canonical protocol IR
    ├── Rust code generator
    ├── C code generator
    ├── documentation generator
    ├── compatibility checker
    ├── descriptor generator
    ├── test-vector generator
    └── tracing descriptor generator
```

The IR must preserve every semantic property that affects compatibility, security,
validation, ownership, or diagnostics.

## Compiler outputs

Given:

```text
files.nsidl
```

`nsidlc` should eventually produce:

```text
generated/
├── rust/
│   ├── types.rs
│   ├── client.rs
│   ├── server.rs
│   └── descriptor.rs
├── c/
│   ├── files.h
│   └── files.c
├── docs/
│   └── files.md
├── tests/
│   ├── canonical_vectors.json
│   └── malformed_vectors.json
├── fuzz/
│   └── decode_files.rs
├── files.nsproto
└── files.nsidl.lock
```

The first implementation needs only Rust generation, but the language-neutral IR should
exist immediately so the wire format is not accidentally designed around Rust.

## Standard desktop transport profile

The normal NSWP 1 desktop profile should support at least:

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

A protocol may declare lower limits.

Large media, graphics, storage, and network payloads use shared memory.

## Current endpoint prototype profile

The current bounded endpoint implementation can prototype a reduced NSWP profile:

```text
Maximum total packet bytes:        256
Header bytes:                       64
Maximum body bytes:                192
Maximum attached handles:            1
Maximum outstanding calls:           8
```

This reduced profile can exercise:

- header framing;
- negotiation;
- transaction matching;
- small tables and values;
- versioning;
- transport and service errors;
- one attached capability.

Full conformance to atomic move-transfer, multiple handles, peer closure, and
event-port-driven asynchronous dispatch depends on the planned channel ABI.

The current capability transfer copies a rights-reduced capability rather than consuming
the source. A prototype must not claim full NSWP move-transfer semantics until the
channel implementation supports them.

## Decoder requirements

A conforming decoder validates:

```text
header magic and size
wire version
reserved fields
packet kind and direction
protocol family in negotiation and selected version against connection state
ordinal availability
body and attachment counts
transaction state
deadline constraints
offsets, lengths, and alignment
canonical ordering and zero padding
nesting depth
table envelope order and reserved ordinals
required fields
version and feature availability
enum and union values
UTF-8 and container bounds
handle-domain boundaries
object type
minimum required rights
maximum allowed rights
complete byte and attachment consumption
```

The decoder uses bounded memory and bounded recursion or an explicit bounded work stack.

It must not invoke application-defined decode callbacks before complete validation.
Low-level body-reader closures are schema traversal and validation code, not application
callbacks: they must be deterministic and side-effect-free. The runtime completes the
entire structural and schema-specific validation pass before constructing application-visible
values or invoking service code. Table envelope structure and outer payload accounting are
validated in a complete pre-pass before even low-level field traversal begins.

## Protocol security invariants

> Every packet is bounded before recursive decoding begins.

> Every dynamic field has a compile-time maximum.

> Every attachment belongs to one explicit handle domain and is either adopted exactly
> once or closed.

> Unknown table fields can be skipped without interpreting their bytes or leaking their
> handles.

> Application and service code receives only fully validated values and capabilities.

> Excess handle rights are rejected rather than silently accepted.

> Wire parsing never depends on Rust, C, pointer size, host padding, or compiler-native
> enum representation.

> Messages contain no pointers, architecture-sized integers, or unbounded recursive
> values.

> Negotiation selects one version, feature set, and set of limits for the entire
> connection.

> Transport failure, service-domain failure, cancellation, timeout, peer closure, and
> uncertain completion remain distinguishable.

## Implementation sequence

### Phase 1: Hand-written wire codec

Implement without an IDL parser:

- header encoder and decoder;
- handle-free body arena builder;
- canonical structure layout;
- slice and table references;
- envelopes with zero handle fields;
- primitive, string, vector, enum, and closed-union validation;
- negotiation records and state machine;
- request, response, event, and cancellation handling.

Handle references and domains, handle adoption, and unknown open-union alternatives follow
as separate tentative work after the handle-free body codec. Use hand-written Rust type
descriptors so the wire decisions can be measured before the source language and
generator are relied upon.

### Phase 2: Host transport simulator

Create a host-side transport supporting:

- bounded queues;
- byte packets;
- synthetic handles;
- peer closure;
- cancellation and deadlines;
- backpressure;
- late replies;
- malformed packet injection;
- service-generation replacement.

Host and target encoders must produce identical canonical bytes.

### Phase 3: Pilot protocols

Use hand-written protocol descriptions for:

```text
system.logging
system.configuration
system.service-observer
```

These exercise:

- one-way records;
- RPC;
- events;
- table evolution;
- cancellation;
- deadlines;
- service generations.

A separate test protocol should exercise object and typed-endpoint transfer.

### Phase 4: Parser and canonical IR

Implement:

- lexer and parser;
- imports and symbol resolution;
- constant evaluation;
- type-shape and maximum-size computation;
- ordinal and reserved-range validation;
- version and feature availability;
- compatibility lock files;
- canonical language-neutral protocol IR.

### Phase 5: Rust generator

Generate:

- value types;
- clients and servers;
- validators, encoders, and decoders;
- negotiation metadata;
- protocol descriptors;
- compatibility tests;
- canonical and malformed vectors;
- fuzz targets.

### Phase 6: Full channel integration

After channel-pair IPC exists, add:

- atomic move-transfer;
- multiple attached handles;
- peer-closure signaling;
- one- and many-object waiting;
- event-port executor integration;
- mapped shared memory;
- bounded priority donation for synchronous calls where appropriate.

### Phase 7: C interoperability prototype

Implement a small independent C decoder and client for one public pilot protocol.
Interoperate with the Rust server under the host simulator and QEMU.

This verifies that the specification is actually language-neutral rather than merely
claiming to be.

## Freeze criteria for NSIDL and NSWP 1.0

Do not declare stable 1.0 until:

- at least three pilot services run under QEMU;
- one pilot transfers handles and typed endpoints;
- one pilot emits events and supports cancellation;
- host and target implementations emit identical canonical vectors;
- minor-version compatibility tests cover unknown fields and new methods;
- decoder fuzzing finds no unbounded path;
- malformed-message tests find no handle leaks;
- cancellation and late-reply races are exercised;
- service replacement and uncertain-outcome behavior are tested;
- an independent C implementation interoperates with Rust;
- packet, body, and negotiation limits have been measured under realistic desktop use;
- every remaining incompatible change can be justified as a deliberate pre-1.0 revision.

## Required formal decision

> NSIDL defines bounded, ordinal-based, language-neutral service interfaces. NSWP carries
> one negotiated protocol major, minor, feature set, and limit profile over each channel
> connection. Messages use a fixed header, canonical self-relative body encoding, and
> out-of-band handle attachments partitioned into nested handle domains. Tables provide
> additive evolution; structures provide fixed layouts. Generated bindings own
> validation, transactions, cancellation, deadlines, handle cleanup, and version
> enforcement.

## Open questions


- Whether the first stable body format keeps 32-bit relative offsets.
- Whether strict canonical decoding should be mandatory for all system protocols or
  selectable for explicitly internal high-performance connections.
- Whether optional values retain specialized envelope encoding or use a dedicated
  smaller inline representation.
- Whether negotiated minor version always requires one round trip for newly transferred
  typed endpoints.
- Exact semantics for preserving unknown open-union payloads in a future wire version.
- Whether a future transport profile permits packets larger than 64 KiB for specialized
  control planes.
- The exact representation and signing policy for `.nsproto` descriptors.
- Whether documentation comments and semantic validation constraints become part of
  compatibility fingerprints.
- How much of the current reduced endpoint transport should be used for prototypes before
  channel-pair IPC is available.
