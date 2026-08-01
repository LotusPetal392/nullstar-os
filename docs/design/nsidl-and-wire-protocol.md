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
- one negotiated protocol major and minor version is carried by one channel connection;
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

The exact grammar, packet layout, body arena, header size, negotiation records, standard
limits, and command names in this document are **tentative design pending
implementation**. They are concrete enough to build and test, but NSIDL and NSWP should
not be declared stable 1.0 until the freeze criteria near the end of this document are
met.

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
- has no terminating NUL;
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

An open union accepts an unknown alternative ordinal as an unknown value.

NSIDL 1 skips the unknown payload and closes all handles belonging to that alternative.
Opaque round-trip preservation of arbitrary unknown nested payloads is deferred because
it would require retaining unvalidated wire regions and ownership state.

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

Each protocol has:

```text
one opaque 128-bit family ID
one major version
one maximum supported minor version
zero or more feature IDs
```

Example:

```text
@id("3c59c73e-852e-4ad8-bb3d-610ca4920727")
@version(1.2)
protocol FilePortal
```

The UUID is encoded as the 16 bytes in conventional UUID order and treated as opaque
bytes. It is not reinterpreted as native-endian integers.

The service name is discovery metadata. The protocol family ID is wire identity. Neither
one grants authority.

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

Every NSWP packet begins with an 80-byte header.

```rust
#[repr(C)]
struct NswHeaderV1 {
    magic: u32,
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

    protocol_id: [u8; 16],
    trace_context: [u8; 16],
}
```

The Rust declaration illustrates layout only. The wire definition is the explicit offset
table below, not compiler output.

## Header offsets

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 4 | `magic` |
| 4 | 2 | `header_bytes` |
| 6 | 1 | `wire_major` |
| 7 | 1 | `wire_minor` |
| 8 | 1 | `kind` |
| 9 | 1 | `flags` |
| 10 | 2 | `reserved0` |
| 12 | 2 | `protocol_major` |
| 14 | 2 | `protocol_minor` |
| 16 | 4 | `ordinal` |
| 20 | 4 | `body_bytes` |
| 24 | 2 | `handle_count` |
| 26 | 2 | `reserved1` |
| 28 | 4 | `transport_status` |
| 32 | 8 | `transaction_id` |
| 40 | 8 | `deadline_ns` |
| 48 | 16 | `protocol_id` |
| 64 | 16 | `trace_context` |

The total is exactly 80 bytes.

## Header magic

The four header bytes are:

```text
4e 53 57 31
 N  S  W  1
```

As a little-endian integer:

```text
0x3157534e
```

## Fixed header values

For NSWP 1.0:

```text
header_bytes = 80
wire_major = 1
wire_minor = 0
flags = 0
reserved fields = 0
```

A receiver rejects unsupported header size, major version, nonzero reserved fields, or
unknown mandatory flags.

## Body length

`body_bytes`:

- excludes the 80-byte header;
- is a multiple of eight;
- exactly equals the remaining bytes in the transport message;
- does not include attached handles.

Trailing bytes are not permitted.

## Attached handles

`handle_count` exactly equals the number of handles attached by the transport. A mismatch
is a protocol error and causes every received attachment to be closed.

## Deadlines

`deadline_ns` is an absolute monotonic timestamp in nanoseconds.

```text
0xffffffffffffffff = no deadline
```

Zero represents an already expired deadline.

The monotonic clock epoch is local to the current boot. Deadline values are never stored
as durable wall-clock timestamps.

## Trace context

`trace_context` is an opaque 128-bit correlation value.

All zero means no trace context. A response echoes the request context. Nested service
calls may derive a new child context using the runtime tracing API.

Trace context grants no authority and must not contain secrets.

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

The client sends:

```rust
#[repr(C)]
struct NswNegotiateRequestV1 {
    min_minor: u16,
    max_minor: u16,
    max_handles: u16,
    max_outstanding: u16,

    max_body_bytes: u32,
    reserved0: u32,

    features: NswSliceRefV1,
}
```

The encoded size is 32 bytes.

The header carries the requested protocol family and major version. The request record
contains the client's acceptable minor range, limits, and requested feature records.

## Negotiation response

The server returns:

```rust
#[repr(C)]
struct NswNegotiateResponseV1 {
    status: u32,

    selected_minor: u16,
    max_handles: u16,

    max_outstanding: u16,
    reserved0: u16,

    max_body_bytes: u32,

    features: NswSliceRefV1,

    service_generation: u64,
    reserved1: u64,
}
```

The encoded size is 48 bytes.

`service_generation` identifies the current disposable provider incarnation for tracing,
failure attribution, and reconnect decisions. It does not grant authority and is not a
process ID.

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

```text
0 = Ok
1 = UnsupportedMajor
2 = NoCommonMinor
3 = RequiredFeatureUnavailable
4 = TransportBoundsTooSmall
5 = PolicyDenied
6 = Busy
7 = Internal
```

On negotiation failure, the server sends the response and closes the connection.

## Version and feature selection

The server selects the highest minor version satisfying:

```text
client minimum <= selected <= client maximum
server minimum <= selected <= server maximum
selected version fits negotiated body and handle bounds
all required features are available and policy-permitted
```

The final connection limits are the minimum of:

```text
client limit
server limit
transport limit
protocol-declared limit
```

Every later packet uses the selected minor version exactly.

## Body arena

NSWP bodies use a relocatable, self-relative, eight-byte-aligned arena.

The root value starts at body offset zero. Out-of-line values use relative references.
No wire value contains a process virtual address.

Every nonempty out-of-line region:

- begins on an eight-byte boundary;
- lies entirely within the current enclosing region;
- is referenced by a positive forward offset;
- has an exact padded length;
- does not overlap another region.

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
- `region_bytes` is a multiple of eight;
- the complete value and all descendants lie inside that region.

For strings and bytes:

```text
region_bytes = align_up(count, 8)
```

For vectors, `region_bytes` includes the inline element array, all out-of-line
descendants of those elements, and trailing alignment padding.

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

The referenced region begins with a sorted array of field envelopes. The remaining
region contains field payloads in ordinal order.

For an empty table, every member is zero.

## Handle reference

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
- `payload_bytes` includes the payload root and all descendants;
- `handle_start` and `handle_count` select a child handle domain;
- all reserved values are zero.

If there is no payload:

```text
payload_relative_offset = 0
payload_bytes = 0
```

If there are no handles:

```text
handle_start = 0
handle_count = 0
```

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

## Handle domains

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

## Canonical handle ordering

Handles are assigned in depth-first wire traversal order.

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

## Two-phase handle adoption

Decoding occurs in two phases:

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

Every body is one tree-shaped region. The root value begins at offset zero. Out-of-line
regions are emitted in deterministic depth-first order.

### Structure children

Children appear in field declaration order.

### Array and vector children

Children appear in element-index order.

### Table children

A table is emitted as:

1. inline table reference;
2. sorted field-envelope array;
3. field payloads in ascending ordinal order.

### Union and optional children

The selected payload follows the inline envelope.

## Canonical body constraints

- every out-of-line target is eight-byte aligned;
- every relative offset points forward;
- regions do not overlap;
- each region length is exact;
- there are no unused body bytes;
- every padding byte is zero;
- table envelopes are sorted;
- attached handles follow the same depth-first ordering;
- every reserved value is zero.

A strict decoder rejects a known value whose representation is valid but noncanonical.

Unknown table-field payloads are skipped as bounded opaque regions. An older decoder does
not recursively canonicalize a future field it does not understand.

## Table validation

A decoder validates a table in this order:

1. validate the table reference and region bounds;
2. validate that the envelope array fits the table region;
3. require strictly increasing envelope ordinals;
4. reject duplicate and reserved ordinals;
5. validate nonoverlapping payload regions;
6. validate nonoverlapping handle domains;
7. decode known fields;
8. skip unknown fields and close their handles;
9. verify required fields for the selected minor and feature set;
10. verify complete byte and handle consumption.

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
- echoes the request trace context;
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

This gives one deterministic 96-byte body representation.

## Handle-transfer example

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
trace context
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
Header bytes:                        80
Maximum body bytes:              65,456
Maximum attached handles:            64
Maximum nesting depth:                32
Maximum table fields:              1,024
Default outstanding calls:           256
Maximum negotiated outstanding:    4,096
```

A protocol may declare lower limits.

Large media, graphics, storage, and network payloads use shared memory.

## Early endpoint profile

The current bounded endpoint implementation can prototype a reduced NSWP profile:

```text
Maximum total packet bytes:        256
Maximum body bytes:                176
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
protocol family and selected version
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
- body arena builder;
- canonical structure layout;
- slice, table, and handle references;
- envelopes and handle domains;
- primitive, string, vector, enum, and union validation;
- negotiation records and state machine;
- request, response, event, and cancellation handling.

Use hand-written Rust type descriptors so the wire decisions can be measured before the
source language and generator are relied upon.

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

- Whether the final fixed header remains 80 bytes after pilot measurements.
- Whether protocol family identifiers remain UUID-form 128-bit values.
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
