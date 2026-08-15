# Native application runtime, SDK, and service IDL direction

## Status

The following are **accepted direction**:

- Rust is the first-class language for native applications, services, drivers, and
  system components.
- Stable, versioned, language-neutral service protocols are the primary long-term
  application compatibility boundary.
- Rust's compiler-private ABI is not a stable NullStar platform ABI.
- Native applications normally carry their Rust runtime and private dependencies inside
  an immutable application generation.
- Every managed process begins through one bootstrap channel carrying a versioned
  startup message and explicit handles.
- Kernel objects are exposed to safe code through ownership-aware typed handles.
- The native asynchronous runtime is built over waitable objects and an event port or
  wait set rather than one blocking thread per connection.
- Client bindings, server dispatchers, validation, tracing metadata, documentation,
  mocks, compatibility checks, and fuzzing entry points are generated from one service
  interface definition.
- Bulk data uses shared memory and explicit synchronization rather than oversized RPC
  messages.
- Service names and protocol identifiers support discovery but never grant authority.

The exact crate names, command names, SDK support periods, Rust target details, standard
library schedule, and stable C ABI remain **tentative design**. The concrete provisional
interface language and wire format are defined in
[NSIDL and the NullStar Wire Protocol](nsidl-and-wire-protocol.md).

This document describes future architecture. The current syscall, capability, endpoint,
and startup behavior remains authoritative in
[Userspace ABI](../syscall-abi.md) and
[Capability and IPC protection model](../protection-model.md).

## Goals

The native developer platform should:

- let ordinary applications use stable services without depending on kernel details;
- make ownership, transfer, cancellation, deadlines, and failure explicit;
- support efficient desktop event loops and multi-process applications;
- preserve capability isolation in every generated API;
- remain usable from languages other than Rust;
- make service protocols testable on a development host and under QEMU;
- support protocol evolution without silent ABI drift;
- expose enough tracing and diagnostic information to understand service chains;
- keep compatibility interfaces layered over native contracts rather than defining them.

The central compatibility rule is:

> NullStar stabilizes service protocols, executable and loader rules, manifest formats,
> and selected fixed-width runtime ABIs. It does not stabilize Rust's compiler ABI.

## Platform layers

The native application stack should be divided into explicit layers:

```text
Application and service frameworks
    application lifecycle, portals, settings, UI, media
        |
Generated service bindings
    typed clients, servers, events, validation, versioning
        |
Service and asynchronous runtime
    transactions, dispatch, cancellation, task groups, event ports
        |
Core native runtime
    startup, allocator, TLS, panic handling, logging, process lifecycle
        |
Safe kernel-object API
    handles, channels, jobs, memory, timers, waiting
        |
Raw syscall bindings
    fixed-width structures and architecture-specific entry code
        |
Kernel syscall ABI
```

Applications should normally use the upper layers. Direct syscalls are reserved for the
runtime, low-level libraries, diagnostics, and specialized system components.

## Proposed Rust crate layers

The names are tentative, but the responsibilities should remain separate.

```text
nullstar-sys
nullstar-core
nullstar-runtime
nullstar-ipc
nullstar-async
nullstar-service
nullstar-app
nullstar-portal
nullstar-ui
nullstar-test
```

### `nullstar-sys`

`nullstar-sys` contains unsafe, minimal ABI definitions:

- syscall numbers;
- fixed-width syscall structures;
- raw handle and status representations;
- architecture-specific syscall entry helpers;
- shared constants generated from the authoritative ABI definition.

It should not allocate and should not automatically manage resources. Ordinary
applications should not depend on it directly.

### `nullstar-core`

`nullstar-core` provides foundational, `no_std`-compatible value types:

- `Status` and transport status classes;
- rights and signal masks;
- monotonic deadlines and durations;
- object-type identifiers;
- application, component, service, session, package, and generation identifiers;
- bounded strings and identifiers used by generated protocols;
- common error and result types.

These types have explicit representations where they cross an ABI boundary.

### `nullstar-runtime`

`nullstar-runtime` owns process runtime initialization:

- `_start` and architecture entry code;
- bootstrap-channel validation;
- allocator initialization;
- thread-local storage;
- panic and out-of-memory handling;
- process exit and crash reporting;
- standard-stream construction;
- structured logging initialization;
- optional environment and argument compatibility;
- construction of the role-specific process context.

No application-defined entry point runs until mandatory startup resources have been
validated.

### `nullstar-ipc`

`nullstar-ipc` wraps kernel IPC and object primitives:

- owned and borrowed handles;
- channels and typed protocol endpoints;
- shared-memory objects and mappings;
- events, notifications, and timers;
- event-port or wait-set registration;
- handle transfer and rights attenuation;
- packet framing and protocol negotiation.

### `nullstar-async`

`nullstar-async` supplies userspace asynchronous execution:

- futures executor integration;
- event-port registration and wakeup;
- task groups and structured cancellation;
- timers and deadline propagation;
- bounded blocking pools;
- graceful shutdown and draining;
- instrumentation for queue delay, task runtime, and stalled operations.

### `nullstar-service`

`nullstar-service` supports generated protocols:

- client transaction management;
- server dispatch;
- request validation;
- version and feature negotiation;
- connection context;
- typed service and transport errors;
- event delivery and backpressure;
- late-reply cleanup;
- tracing and privacy metadata.

### `nullstar-app`

`nullstar-app` provides the application lifecycle framework:

- launch and readiness;
- typed activation messages;
- document and URI activation;
- session restoration;
- suspend, resume, memory-pressure, and termination events;
- application storage roles;
- the restricted service namespace;
- permission and grant-change notifications.

Services, drivers, migration tools, extensions, and workers receive different contexts
instead of one all-powerful universal runtime context.

## Compatibility boundaries

NullStar's durable compatibility contracts should be:

```text
fixed-width syscall structures
versioned service wire protocols
application, package, service, and driver manifests
executable and dynamic-loader ABI rules
selected stable C runtime interfaces
```

The Rust SDK provides a source-level API over those contracts. An application may be
rebuilt against a newer SDK while continuing to speak the same service protocol major
versions.

An already installed application may continue to run because:

- its immutable generation retains its runtime and private dependencies;
- the system still supports the protocol versions it negotiated;
- it does not rely on another Rust crate's compiler-private memory layout.

Private Rust libraries may be statically linked or packaged inside one coordinated
application generation. They should not become mutable, global platform libraries.

## Protocol stability classes

Every service protocol should declare one stability class.

| Class | Intended use | Compatibility expectation |
| --- | --- | --- |
| `public` | Third-party applications and durable platform APIs | Published cross-generation version support |
| `system` | Coordinated operating-system components | Updated through compatible changes or one system generation |
| `experimental` | Prototypes and developer-only interfaces | May change incompatibly with explicit opt-in |

A production third-party package should not silently depend on an experimental system
protocol. Package verification should identify and display such dependencies.

## Process startup contract

Every managed native process should start with exactly one bootstrap channel in a known
ABI location. The exact initial handle number or register convention remains tentative;
the semantic contract is accepted.

The first channel message is a versioned startup record containing named or numbered
startup resources.

```text
ProcessStart
├── startup protocol version
├── process and component identity
├── application or service identity
├── package and generation identity
├── user and login-session context
├── argument vector
├── compatibility environment
├── standard streams
├── lifecycle endpoint
├── restricted service namespace
├── structured logging endpoint
├── job and process-self handles
├── private storage capabilities
├── read-only bundle capability
├── activation endpoints
└── optional platform facilities
```

Identity fields describe the process. They do not create authority. Authority comes from
the attached handles and the non-relaxable policy of the containing job.

### Startup validation

The runtime should:

1. validate the startup protocol version and message bounds;
2. reject duplicate mandatory resource roles;
3. validate every handle's object type and minimum and maximum rights;
4. close unknown optional handles that the runtime cannot use;
5. fail before application code runs if a mandatory resource is absent;
6. initialize memory, TLS, logging, panic handling, and async execution;
7. construct the context appropriate to the process role;
8. invoke the developer-defined entry point.

The runtime must not infer trust from executable path, parent process, display name,
working directory, or environment variables.

## Role-specific entry points

The SDK should provide source-level entry helpers such as:

```rust
#[nullstar::main]
async fn main(context: ApplicationContext) -> Result<(), AppError> {
    context.ready().await?;

    while let Some(activation) = context.next_activation().await {
        handle_activation(activation).await?;
    }

    Ok(())
}
```

The macro is an SDK convenience, not a stable binary ABI.

Different declared components receive distinct context types:

```text
ApplicationContext
ServiceContext
DriverContext
WorkerContext
ExtensionContext
MigrationContext
RecoveryContext
```

A migration component should not receive display or ordinary network services unless its
manifest and migration contract explicitly require them. A renderer should not receive
selected-document handles merely because the main application owns them.

## Application lifecycle

The application runtime should expose one consistent lifecycle independent of the GUI
toolkit.

### Readiness

A process starts in:

```text
STARTING
```

It validates configuration, connects to required services, constructs initial state, and
then reports:

```text
READY
```

The application manager should not treat process existence as successful launch.
Readiness carries a bounded deadline and an explicit failure result.

### Activation stream

A running application receives typed activation messages:

```text
Launch
OpenDocuments
OpenUris
CreateDocument
NewWindow
PrintDocuments
RestoreSession
ContinueActivity
BackgroundActivation
```

A document activation contains a scoped file capability, not only a pathname.

```text
OpenDocument
├── file capability
├── display name
├── content type
├── granted rights
├── persistent grant identity
├── originating portal or application
└── trusted user-gesture context
```

### Lifecycle events

The runtime should support:

```text
PrepareToSuspend
Resume
MemoryPressure
ConfigurationChanged
PermissionRevoked
PrepareToTerminate
SessionEnding
```

Each event has explicit deadline, cancellation, and acknowledgment semantics. An
application is not expected to infer shutdown from disappearing windows or arbitrary
signals.

## Safe handle API

The raw syscall ABI represents a handle as an opaque fixed-width integer. Safe Rust code
should use ownership-aware wrappers.

Conceptually:

```rust
Owned<Channel>
Borrowed<'a, Channel>
Owned<MemoryObject>
Owned<EventPort>
ClientEnd<Display>
ServerEnd<Display>
```

### Ownership rules

- `Owned<T>` closes the handle when dropped.
- `Owned<T>` is not implicitly cloneable.
- duplication is explicit and requires the `DUPLICATE` right;
- transfer consumes an owned handle by default;
- borrowed handles cannot be transferred;
- a received handle is checked for object type and required and allowed rights;
- conversion to or from a raw handle is explicitly unsafe or advanced;
- rights may be preserved or reduced, never increased.

Example:

```rust
let read_only = file.duplicate(
    Rights::READ | Rights::WAIT | Rights::TRANSFER,
)?;

channel.send(Request {
    file: read_only.into_transfer(),
}).await?;
```

Object type should be represented in the Rust type system. Rights normally remain a
validated runtime property. Encoding every possible rights mask in generic parameters
would make ordinary APIs and generated bindings unnecessarily complex.

## Asynchronous runtime

NullStar should be asynchronous-first but not async-only.

### Event-port foundation

The executor registers waitable objects with an event port or persistent wait set:

```text
Channels
Timers
Process exits
Service completions
Sockets
Display events
Filesystem completion
Device events
Media notifications
        |
        v
Event port
        |
        v
Userspace executor
        |
        v
Ready tasks
```

The kernel reports object events and completion tokens. It does not understand Rust
futures.

### Registration identity

Every registration should contain:

```text
registration identifier
registration generation
user token
requested signals
```

The generation prevents a late event for a removed registration from waking a newly
reused task slot.

Dropping a waiting future unregisters or cancels the wait. If completion is already in
flight, the runtime drains and discards it safely.

### Structured concurrency

Every task should belong to a task group.

```text
Application root task group
├── UI task group
├── activation task group
├── document task groups
└── background-operation task groups
```

Closing a document cancels its task group. Process shutdown cancels the root group under
a bounded deadline. Detached tasks require an explicit API and remain attributable to a
lifecycle owner.

### Blocking work

A bounded blocking pool may support compatibility code that cannot immediately become
asynchronous. It must have:

- a policy-bounded worker count;
- bounded work queues;
- cancellation and shutdown behavior;
- job resource accounting;
- tracing;
- no realtime authority.

Audio and other deadline-sensitive work should use dedicated budgeted workers rather
than the general async executor.

### Deadline propagation

Runtime deadlines use absolute monotonic time. A nested service call propagates the
earlier of:

- the caller's deadline;
- the server's own operation limit.

```rust
client
    .call_with_deadline(request, deadline)
    .await?;
```

Relative timeouts must not restart at every service boundary.

## Service IDL and generated bindings

The working language name is **NullStar Interface Definition Language**, abbreviated
**NSIDL**. Source files use `.nsidl`; compiled descriptors tentatively use `.nsproto`.

NSIDL describes:

- protocol identity and supported versions;
- request and response methods;
- one-way messages and server events;
- fixed structures and evolvable tables;
- enums, unions, results, and bounded containers;
- handle object types and rights;
- cancellation and deadline requirements;
- retry and idempotency properties;
- privacy and tracing classifications;
- protocol limits and negotiated features.

NSIDL does not decide which application may connect. Capability routing, package policy,
user grants, and service-manager policy remain separate.

The concrete grammar, type system, packet header, negotiation, body encoding, handle
domains, and compatibility rules are specified in
[NSIDL and the NullStar Wire Protocol](nsidl-and-wire-protocol.md).

## Service discovery

Applications receive a restricted service-namespace capability in their startup message.
Generated code may provide an API such as:

```rust
let display = context
    .services()
    .connect::<Display>(VersionRange::compatible_with(1, 3))
    .await?;
```

The namespace broker:

1. identifies the caller through trusted process and route metadata;
2. verifies that the caller is permitted to request the protocol;
3. activates the provider if required;
4. creates a fresh channel pair;
5. passes the server endpoint and trusted connection context to the provider;
6. returns the client endpoint;
7. lets generated bindings negotiate a compatible protocol version and features.

Knowing a service name or protocol identifier never grants connection authority.

## Trusted connection context

A server may receive a policy-approved subset of:

```text
application identity
component identity
service identity
user identity
session identity
package and generation identity
capability-route identity
trace identity
negotiated features
connection quotas
```

The broker or runtime delivers this metadata out of band. A client cannot forge it as an
ordinary request field.

Identity does not replace capabilities. A filesystem protocol should prefer a directory
handle to a claim that the caller may access a global path.

## Generated Rust bindings

The NSIDL compiler should generate:

- owned request and response types;
- optional borrowed views for reviewed high-performance paths;
- typed client proxies;
- server traits and dispatchers;
- typed client and server endpoints;
- validators, encoders, and decoders;
- method and event ordinals;
- version and feature metadata;
- typed domain errors;
- event streams;
- documentation and protocol descriptors;
- test mocks and host transports;
- canonical and malformed conformance vectors;
- fuzzing entry points.

A generated client might appear as:

```rust
let reply = portal
    .open_file(OpenFileRequest {
        parent_window: Some(window.token()),
        accepted_types,
        requested_access: FileAccess::ReadWrite,
        allow_multiple: false,
    })
    .with_deadline(deadline)
    .await?;
```

Generated server code receives only validated requests:

```rust
async fn open_file(
    &self,
    context: RequestContext,
    request: OpenFileRequest,
) -> Result<OpenFileReply, OpenFileError>;
```

The dispatcher owns:

- packet decoding and canonical validation;
- transaction matching;
- version and feature enforcement;
- deadline and cancellation state;
- handle adoption and cleanup;
- response encoding;
- protocol errors;
- tracing and privacy policy.

Service implementations should not manipulate transaction identifiers or raw attached
handles manually.

## Connection concurrency

The server runtime should support:

- concurrent request dispatch by default;
- explicitly serialized protocols or connection state;
- bounded outstanding requests;
- per-method concurrency limits;
- graceful draining during shutdown;
- cancellation tokens for in-progress work;
- request attribution in logs and traces.

The runtime should not require one thread per client connection.

## Protocol compiler architecture

The compiler should use a language-neutral intermediate representation.

```text
NSIDL source
    |
    v
Parser and semantic validation
    |
    v
Canonical protocol IR
    ├── Rust generator
    ├── C generator
    ├── documentation generator
    ├── compatibility checker
    ├── descriptor generator
    ├── test-vector generator
    └── tracing metadata generator
```

The canonical IR should represent all bounds, ordinals, rights, availability, privacy,
and lifecycle semantics rather than preserving language-specific syntax alone.

## SDK contents

A versioned NullStar SDK should include:

```text
compiler target specification
linker and startup objects
raw syscall bindings
safe runtime crates
public service definitions and generated bindings
application and service frameworks
bundle and package manifest schemas
NSIDL compiler and compatibility checker
bundle and content-manifest builder
developer signer and package builder
documentation and examples
QEMU runner and host transport simulator
test libraries, debugger, symbols, and tracing tools
```

The SDK should be distributed as a coherent Magnetar development generation. Mixing
arbitrary pieces from unrelated SDK generations should not be the default workflow.

## Rust target and standard library

The long-term Rust target should be equivalent to:

```text
x86_64-unknown-nullstar
```

Early development can continue using the current freestanding target while runtime and
standard-library support mature.

The initial SDK may remain:

```text
no_std + alloc + NullStar runtime crates
```

A Rust `std` port becomes appropriate after these contracts are sufficiently stable:

- threads and TLS;
- filesystem behavior;
- clocks, timers, and waiting;
- networking;
- process launch;
- synchronization;
- environment compatibility;
- executable and dynamic-loader expectations.

A future `std::fs` sees only the process's sandboxed namespace. It does not imply access
to a global filesystem. Native capability-oriented file APIs remain available.

## Build tooling

A practical Rust frontend is tentatively:

```text
cargo nullstar
```

Possible commands include:

```text
cargo nullstar new
cargo nullstar build
cargo nullstar run
cargo nullstar test
cargo nullstar bundle
cargo nullstar sign
cargo nullstar package
cargo nullstar install
cargo nullstar inspect
```

The command should orchestrate language-neutral tools where practical. The bundle
verifier and signer must not require the application to be written in Rust.

A production build could produce:

```text
target/nullstar/release/
├── Example.app/
├── Example.debug/
├── content.manifest
├── build-report.json
└── Example.nspkg
```

The build report should include:

- SDK and compiler generation;
- target platform ABI;
- protocol dependencies;
- embedded components;
- source revision when supplied;
- reproducibility status;
- signing status;
- requested permissions and entitlements.

## Project templates

Initial SDK templates should cover:

```text
desktop application
command-line application
system service
user service
isolated worker
application extension
state migration component
driver
```

Templates begin with the minimum capability set for their role. They should not request
every common service preemptively.

## Developer deployment

Development builds should use a stable local developer signing identity.

```text
cargo nullstar run
        |
        ├── build bundle
        ├── create content manifest
        ├── sign with local developer identity
        ├── import a developer generation
        ├── register temporary activation
        ├── launch through the application manager
        └── attach logs and debugger
```

The application still runs through its declared sandbox, application job, startup
channel, service namespace, and permission policy.

Developer mode may grant explicit debugger or source-tree capabilities. It must not turn
the application manager into an optional path or allow a development package to claim a
reserved system identity without separate authorization.

## C and additional languages

Rust is first-class, but the protocol architecture remains language-neutral.

A later stable C runtime may expose a library tentatively named:

```text
libnsrt
```

It should support:

- owned handle operations;
- channels and waiting;
- startup processing;
- event ports;
- memory objects;
- generated protocol clients and servers;
- application lifecycle integration.

NSIDL can then generate C headers, client functions, server dispatch tables, ownership
helpers, and cleanup functions. C++, Zig, Swift, and other language bindings can build
on the same fixed-width ABI and protocol descriptors.

No binding may depend on Rust's compiler-private layout.

## Host-side testing

The SDK should make protocol and lifecycle behavior testable without booting the complete
OS for every test.

A host transport simulator should reproduce:

- bounded message queues;
- handle-like ownership and transfer;
- peer closure;
- cancellation and deadlines;
- backpressure;
- late replies;
- service replacement;
- malformed packet injection.

The simulator does not replace kernel tests, but it greatly shortens service-development
feedback cycles.

## Generated conformance tests

For each protocol, generated tests should cover:

- canonical encoding and round trips;
- unknown table fields;
- wrong handle type and rights;
- excess handle rights;
- maximum bounds;
- malformed offsets and lengths;
- duplicate fields and ordinals;
- unknown enum and union values;
- late-reply cleanup;
- cancellation and deadline behavior;
- version and feature negotiation;
- privacy-redaction metadata.

## Fuzzing and fault injection

The compiler should generate fuzz targets for decoders, negotiation, dispatch, table and
union handling, attachment domains, and compatibility conversion.

The host and QEMU test runtimes should inject:

```text
peer closes before reply
peer closes after an operation may have committed
queue becomes full
deadline expires
server generation changes
reply arrives after cancellation
wrong object type is attached
shared-memory mapping is revoked
event-port registration is reused
```

A public protocol should not be considered stable until those outcomes have documented
semantics and tests.

## What NullStar should avoid

### Rust-native serialization as the platform wire format

NullStar should not use raw Rust layouts, compiler-derived enum representations, or a
serializer whose stable contract is merely the current Rust type definition.

Serialization libraries may be used by developer tools or application-private formats,
but public service protocols require an explicitly specified representation.

### JSON for core local IPC

JSON remains useful for diagnostics and external tooling. It is not the native local
service protocol because it has no native handle-transfer model, weak numeric bounds,
more expensive validation, and no canonical ownership semantics.

### D-Bus as the native trust boundary

A D-Bus compatibility service may support ported applications. It should translate to
native NullStar services rather than defining native capability, lifecycle, or
authorization semantics.

### Global service lookup

A process must not connect to any service merely by naming it. Discovery occurs through
a restricted namespace capability.

### Implicit handle inheritance

A child receives an explicit handle allowlist. Native process creation does not copy the
parent's complete authority set.

### Automatic replay after service failure

The runtime must not hide uncertain outcomes behind transparent request replay. Protocols
and application code decide whether an operation is safe to repeat.

## Recommended implementation sequence

### Phase 1: Runtime wrappers over the current ABI

- split raw and safe ABI crates;
- introduce owned wrappers for current capability objects;
- unify status and transport error handling;
- centralize startup, logging, panic, and exit behavior;
- add host tests for ownership and cleanup;
- preserve the current endpoint, notification, and copied shared-memory semantics.

The first in-tree slice is implemented in the userspace library without prematurely splitting
crates: raw calls remain available, while sealed object markers, non-cloneable owned handles,
lifetime-bound borrows, automatic close, explicit duplication and rights replacement, unsafe raw
adoption, type erasure and revalidation, and ownership-safe endpoint receive are available to new
code. Host tests cover close-versus-transfer behavior and the QEMU runtime probe verifies drop,
explicit close, reduced-rights replacement, failed-cast ownership preservation, and received-handle
cleanup against the kernel. Moving all services to the layer, consuming owned handles during move
transfer, and separating final public crates remain Phase 1 work.

### Phase 2: Channel and wait runtime

- add channel pairs and peer-closure signals;
- add atomic move-transfer and multiple attached handles;
- add one- and many-object waiting with absolute deadlines;
- add event ports or persistent wait sets;
- add asynchronous channel send and receive;
- add safe mapped shared memory.

### Phase 3: Hand-written protocol runtime

- implement the provisional NullStar Wire Protocol codec;
- add transaction identifiers and version negotiation;
- separate transport and service errors;
- add cancellation and late-reply cleanup;
- add connection quotas, tracing, and privacy metadata;
- validate complete messages and handles before dispatch.

Use a few hand-written pilot protocols before relying on generated code. Good initial
candidates are:

```text
system.logging
system.configuration
system.service-observer
```

A test-only protocol should exercise handle transfer.

### Phase 4: Asynchronous execution

- implement event-port executor integration;
- add task groups and structured cancellation;
- add timers and deadline propagation;
- add a bounded blocking pool;
- integrate application and service shutdown;
- expose role-specific runtime contexts.

### Phase 5: NSIDL compiler MVP

- implement lexer, parser, and semantic analysis;
- support fixed structures and evolvable tables;
- support bounded strings, bytes, vectors, enums, unions, results, and handles;
- require explicit protocol, method, field, event, and feature ordinals;
- generate Rust clients, servers, validators, and descriptors;
- generate compatibility lock files, docs, conformance vectors, and fuzz targets.

### Phase 6: Application SDK

- add application entry and readiness framework;
- add activation streams and lifecycle events;
- add service namespace client;
- add private storage roles;
- add public portal, settings, and notification bindings;
- add application templates;
- implement `cargo nullstar build`, `run`, and `test`.

### Phase 7: Public platform SDK

- define public protocol support policy;
- publish SDK generation manifests and deprecation reports;
- add protocol-support inspection;
- add C runtime and generated C bindings;
- add symbol, crash, and remote debugging tools;
- add additional language bindings after the C and Rust implementations interoperate.

## Required invariants

> NullStar's durable application compatibility boundary is versioned service protocols,
> fixed-width native ABI structures, executable rules, and manifest formats, not Rust's
> compiler ABI.

> Rust is the first-class SDK language, while public wire formats remain language-neutral
> and pointer-independent.

> Every managed process receives one validated startup message over a bootstrap channel.
> Arguments, names, paths, environment data, and identity metadata do not replace explicit
> capabilities.

> Owned handles are not implicitly copied. Duplication is explicit, transfer consumes
> ownership by default, and generated bindings validate object type and rights.

> The asynchronous runtime is a userspace facility built over waitable kernel objects.
> The kernel does not implement language futures.

> Generated bindings own wire validation, transaction dispatch, version negotiation,
> handle cleanup, cancellation, and deadline enforcement.

> Long-running work uses operation endpoints, and bulk data uses shared memory rather
> than indefinitely blocked RPC calls or oversized messages.

> Service names and protocol identifiers support discovery but grant no authority.
> Connection authority comes from a restricted service-namespace capability.

## Open questions

- Final public crate and command names.
- The exact bootstrap handle or register convention.
- Whether the first executor is single-threaded, multithreaded, or configurable.
- The initial stable C runtime surface.
- When a Rust standard-library port becomes an official target.
- Whether borrowed zero-copy message views are exposed to third-party applications.
- Public protocol support and deprecation periods.
- Whether `cargo nullstar` is the final developer command.
- Which pilot service should first transfer a real restricted capability.
- How SDK generations are selected when building against multiple installed platform
  generations.
