# IPC, kernel object, and handle model

## Status

The process-local handle model, rights attenuation, bidirectional channels,
shared-memory data paths, level-triggered object signals, and asynchronous-first
service protocols are **accepted direction**.

The exact numeric ABI, handle bit layout, queue limits, message-size limits, rights-bit
assignments, event-port representation, and kernel-assisted call optimization remain
**tentative design** until implementation and measurement establish durable contracts.

This document expands the IPC overview in
[Kernel, IPC, and scheduling](kernel-ipc-scheduling.md). It describes the intended
native model rather than the currently implemented endpoint ABI. Current behavior and
bounds remain authoritative in
[Capability and IPC protection model](../protection-model.md) and
[Userspace ABI](../syscall-abi.md).

## Design goals

The native IPC architecture should:

- make authority explicit and transferable without ambient global namespaces;
- support restartable services, userspace drivers, and sandboxed applications;
- provide efficient control messages and shared-memory bulk transport;
- integrate cancellation, deadlines, backpressure, waiting, and scheduling;
- keep application protocol policy outside the kernel;
- remain pointer-size and language independent at stable ABI boundaries;
- expose enough lifecycle and tracing information to diagnose service failures;
- preserve a practical POSIX compatibility path without using pipes or sockets as the
  only native mechanism.

The central rule is:

> A process may operate on a kernel-managed resource only through a process-local
> handle carrying the required rights.

An object identifier, process identifier, pathname, service name, or user identity may
help locate or describe a resource, but none of them manufactures authority.

## Kernel objects and process-local handles

A kernel object has an internal type, identity, lifetime, waitable signal state, and
type-specific state. Userspace never receives a kernel pointer. Each process instead
owns a private handle table:

```text
Process A handle table                    Kernel objects
----------------------                    --------------
0x104 -> READ | WAIT --------------------> Channel endpoint
0x211 -> MAP | READ | WRITE -------------> Shared memory
0x308 -> WAIT | INSPECT -----------------> Process
0x442 -> WAIT ---------------------------> Timer
```

A table entry conceptually contains:

```text
object reference + immutable rights + handle flags
```

Handle values are opaque and meaningful only in the owning process. A generation plus
slot index is the preferred implementation so a stale value cannot accidentally refer
to a newly allocated object after table-slot reuse. The exact width and bit layout are
not a public promise until the ABI is specified.

### Core object family

The initial native object family should remain compact:

- process;
- thread;
- job;
- channel endpoint;
- event;
- timer;
- shared memory;
- event port or wait set.

Later kernel objects may include interrupt, DMA buffer, resource, debug-session, and
address-space handles. Files, directories, sockets, windows, audio streams, and similar
resources may be represented by typed userspace protocol endpoints rather than distinct
kernel object types. Safe platform libraries can still present them as first-class typed
objects.

### Object identity

Every object should have a non-reused or sufficiently large diagnostic identity. The
identity supports tracing, crash reports, and determining whether two handles refer to
the same object. It never grants authority, and the kernel must not provide a general
`open_object_by_id` operation.

## Rights model

Rights are fixed-width masks interpreted according to object type. A practical generic
set includes:

| Right | General meaning |
| --- | --- |
| `DUPLICATE` | Create another handle to the same object |
| `TRANSFER` | Attach or move the handle through authorized IPC |
| `READ` | Receive data or query ordinary object state |
| `WRITE` | Send data or modify ordinary object state |
| `WAIT` | Wait for object signals |
| `SIGNAL` | Set or clear permitted user-controlled signals |
| `INSPECT` | Query diagnostic or security-sensitive metadata |
| `MANAGE` | Perform privileged lifecycle or policy operations |
| `MAP` | Map a memory object into an address space |
| `EXECUTE` | Create or use executable mappings where policy permits |
| `RESIZE` | Change an object's size |
| `ENUMERATE` | List children or contained objects where meaningful |

Object-specific definitions refine these meanings. For example, channel `READ` permits
message receive, channel `WRITE` permits send, and shared-memory `MAP | READ` permits a
readable mapping.

### Monotonic authority

A duplicated or transferred handle may preserve or reduce rights, never increase them:

```text
requested rights subset-of source rights
```

This rule applies uniformly. There should be no object-specific escape hatch that
silently amplifies a handle. Acquiring new authority requires an independently held
capability, a broker decision, or a provider operation that already possesses the
resource.

Rights on an installed handle are immutable. A process that wishes to surrender rights
may replace a handle atomically with a reduced-rights version.

### Foundational handle operations

The current ABI now provides operations equivalent to:

```text
handle_close
handle_duplicate
handle_replace
handle_get_info
```

ABI 1.20 `handle_replace` requires `DUPLICATE`, accepts only a nonempty rights subset, and leaves
the original valid if replacement fails. It does not require a second free handle-table slot and
preserves object identity. Handle values remain opaque, so callers must use the returned replacement
value even when the current implementation reuses the same numeric slot. Future inspection should
be scoped by `INSPECT` and reveal only information authorized for the caller.

Closing a handle invalidates that table entry immediately. The object remains alive
while another handle, queued message, mapping, waiter, or legitimate kernel reference
retains it.

## Object signals and waiting

Waitable objects expose a level-triggered signal mask. Generic conditions include:

```text
READABLE
WRITABLE
PEER_CLOSED
SIGNALED
TERMINATED
SUSPENDED
TIMER_FIRED
ERROR
```

An object may expose only the signals meaningful for its type. User-controlled signal
bits must be separate from kernel-maintained conditions so a process cannot forge
`PEER_CLOSED`, `TERMINATED`, or similar state.

Level-triggered behavior is the native baseline:

> A signal remains asserted while its underlying condition remains true.

This avoids lost-wakeup designs and lets a client recheck an object after any wake. Safe
libraries may build edge-triggered event loops on top when useful.

### Waiting interfaces

The first general waiting interface should support one or several objects with an
absolute monotonic deadline:

```text
object_wait_one(handle, requested_signals, deadline)
object_wait_many(items, deadline)
```

Absolute deadlines compose across nested service calls without resetting the timeout at
each layer. Special values may represent an immediate poll and an infinite deadline.

A later event port or persistent wait set should aggregate large numbers of registered
objects and deliver tagged readiness records. It should cover channels, timers, process
exit, sockets, file completion, display events, device events, and media events rather
than forcing every subsystem to invent a separate polling API.

### Events and timers

An event is a basic waitable signal object. Manual-reset behavior should be the initial
primitive; auto-reset behavior may be added only with precisely documented wake and
fairness semantics.

Timers use monotonic deadlines. Periodic timers must define overrun and coalescing
behavior rather than silently enqueueing an unbounded number of expirations.

## Channels

The primary structured IPC primitive is a pair of connected, bidirectional channel
endpoints:

```text
Process A endpoint <------------------------> Process B endpoint
       outgoing messages become the peer's incoming messages
```

The kernel transports bounded byte payloads plus transferred handles. It does not parse
filesystem, display, audio, package, or other application-level protocol fields.

Channels should provide:

- asynchronous send and receive;
- message boundaries;
- atomic rights-reduced handle transfer;
- bounded queues and deterministic backpressure;
- readable, writable, and peer-closed signals;
- optional deadlines and cancellation at the syscall or runtime layer;
- a later optimized call/reply path for small bounded requests.

### Message size and bulk data

Control messages should remain small enough to copy and validate cheaply. Exact limits
are implementation policy, not an early stable ABI promise. Large files, audio, video,
network packets, rendering buffers, and other continuous data use shared memory or
specialized buffer objects.

The standard pattern is:

```text
control plane       channel messages
bulk data plane     shared memory
synchronization     signals, queue indices, and event ports
```

### Handle transfer

A successful channel send may atomically move one or more handles into the peer's
process. Each transferred handle is checked for:

- valid ownership by the sender;
- `TRANSFER` authority;
- requested rights no greater than the source;
- allowed object type and transfer policy;
- receiver handle-table capacity;
- receiver job and resource-policy constraints.

ABI 1.21 adds **move semantics** for one rights-reduced handle on the existing endpoint queue. The
sender loses the handle atomically when the complete message and transferred handle are committed;
a failed send consumes nothing. A retained copy can be created explicitly with `handle_duplicate`
before sending. The original ABI 1.2 copy-transfer call remains available for compatibility.

Future channel pairs must extend the same all-or-nothing rule to multiple handles and account for
receiver capacity at send time rather than deferring handle installation until receive.

A later duplicate-transfer disposition is optional and would require both `TRANSFER`
and `DUPLICATE`.

### Atomic receive

If the receiver's byte or handle buffers are too small, receive should report the
required capacities without consuming the message. Handle-table slots must be reserved
before dequeue. Once receive succeeds, all bytes and handles become visible together.

Destroying the final reference to one endpoint permanently asserts `PEER_CLOSED` on the
other. Unread messages queued for the surviving endpoint remain readable. Unread
messages owned only by the destroyed endpoint are discarded, and handles contained in
them are closed.

### Queue bounds and backpressure

Every endpoint queue is bounded by message count, byte count, and attached-handle count.
A full queue must produce a defined nonblocking result or permit a deadline-bound wait;
it must never grow through unaccounted kernel memory.

Queued memory should be charged to the sending process or job resource domain. A service
protocol carrying a high-volume stream must also define application-level flow control
rather than treating channel writability as unlimited capacity.

`WRITABLE` means a minimal message may currently be accepted. A larger write can still
fail with a retryable result after revalidation.

## Synchronous calls

NullStar should be asynchronous-first, not asynchronous-only. Small, bounded
request/reply operations benefit from a synchronous call abstraction:

```text
send request -> block caller -> receive matching reply
```

Long-running work, open-ended interactions, and continuous streams remain asynchronous.
Protocols should avoid deep synchronous dependency chains and must not hold unrelated
locks across a blocking call.

A later kernel-assisted call path may assign trustworthy transaction identities, route
replies directly to waiting callers, and integrate priority donation. Until then, the
userspace IPC runtime may implement call/reply matching over ordinary channel messages.

### Deadlines and cancellation

Calls use absolute monotonic deadlines. Cancellation means that the requester no longer
needs the result; it does not promise rollback of work already performed.

When a call times out, is canceled, or its thread exits:

- the caller resumes with a distinct transport status;
- a late reply is discarded safely;
- handles attached to a discarded reply are closed;
- the server may receive a cancellation event or protocol message where supported;
- transactional rollback remains an explicit service responsibility.

Transport failure and service-level failure must remain distinct. `PEER_CLOSED`,
`TIMED_OUT`, or malformed IPC is not the same as a filesystem `NOT_FOUND` or package
validation error.

### Scheduling integration

Bounded synchronous IPC should support limited priority inheritance or donation so a
high-priority compositor, input, storage, or media client is not blocked behind a
lower-priority server indefinitely.

Donation must be:

- tied to a specific call dependency;
- bounded by scheduling policy and resource budgets;
- propagated through only a limited dependency chain;
- removed when the reply, timeout, cancellation, or failure completes the dependency;
- observable through scheduler and IPC tracing.

No client may manufacture unbudgeted realtime execution by repeatedly calling a
service.

## Shared memory

Shared-memory objects provide the data plane for large or continuous transfers. They
should support:

- explicit size and accounting;
- independent readable, writable, and executable mapping rights;
- per-mapping protection;
- sealing against resize or write where appropriate;
- mapping into selected address spaces through explicit authority;
- provider-controlled buffer rotation for revocable streams;
- integration with event or queue-index synchronization.

Writable and executable mappings must obey the system W^X policy. A generic memory
object must not automatically become DMA-capable. Device-visible memory belongs to a
separate DMA-buffer object with pinning, cache-coherency, device-domain, and IOMMU
policy.

Revocation cannot make a process forget bytes it already copied. Sensitive live streams
should therefore use provider-managed sessions and replaceable buffers rather than
permanent unrestricted mappings.

## Process bootstrap

Native process creation should not clone an ambient capability table. The launcher
constructs the child process while it is not yet runnable, installs one bootstrap
channel in a well-known initial slot, and sends a versioned startup message through that
channel.

The startup message may contain:

- arguments and environment;
- standard stream handles;
- working-directory or rooted-directory authority;
- process-self and job handles with reduced rights;
- executable, package, application, service, component, user, and session identity;
- a restricted service namespace;
- logging and lifecycle endpoints;
- launch-specific resource capabilities.

Every startup handle is explicitly selected and may carry reduced rights. Environment
variables and pathnames remain convenience data, not the security boundary.

The currently implemented direct-child grant and deterministic child slot are
transitional bootstrap mechanisms. They should evolve toward the single-bootstrap-
channel contract without making arbitrary process-ID-based handle injection part of the
native model.

## Service discovery and protocol bindings

Applications and services connect through restricted namespace or broker capabilities.
A client does not enumerate a global process table and send to a process ID. A broker
resolves a stable service identity, checks the caller's route and policy, activates the
provider where necessary, and returns a fresh channel endpoint.

The current [service route protocol](../service-route-protocol.md) is a smaller implemented step over
one-ended endpoints. Its generic `no_std` core and native adapter are allocation-free: a stable
UUIDv4 service ID and nonzero role select one independently authorized route, while a nonzero
provider generation identifies the current publication. `NSRT` v1 is exactly 40 bytes. Under the
current endpoint ABI, a request transfers exactly one fresh send-only reply capability and an
accepted reply transfers exactly one send-only provider capability; a failure transfers none.
Authorization occurs before availability lookup.

The broker handles only route control. It does not parse the NSWP or application packets sent after
acceptance. Provider generations use fresh ingress endpoint objects, so old handles do not become
handles to the replacement. This is generation isolation rather than global revocation: already
delegated handles remain valid for their old objects until references disappear.

The [service control protocol](../service-control-protocol.md) is another bounded stepping stone:
`NSVC` v1 provides an allocation-free, host-testable codec and native endpoint adapter for exact
64-byte list, status, start, stop, and restart records. Each native request attaches one fresh empty
exact-`SEND` private reply endpoint while the client retains exact `RECEIVE`; responses attach no
capability. PID 1 temporarily owns separate stable observation and mutation ingresses and serves its
current four-service registry to `/sv` and trusted `ush` builtins.

Possession of exact-`SEND` observation authority permits `List` and `Status`; request IDs, service IDs,
provider generations, cursors, PIDs, and executable paths are data and grant no authority. A distinct
exact-`SEND` mutation endpoint permits `Restart` for managed services and `Start`/`Stop` for logging;
filesystem `Start`/`Stop` return `Unsupported`, while mutation on the observation endpoint returns
`AccessDenied`. The trusted shell receives separate
`SEND | DUPLICATE` grants but not `TRANSFER`, and ordinary shell children receive neither.

A successful restart response commits intent rather than waiting for replacement readiness. The old
generation becomes `Terminating`, controlled exit does not charge failure policy, and the replacement
uses the next manager-owned generation. Intent remains pending through replacement startup, causing
queued duplicates to receive `Busy`. Once sent, an unconfirmed mutation is outcome unknown and is not
replayed. No separate manager, activation, persistent stopped state, or kernel change is part of
the current integration.

PID 1 is the temporary generation authority and owns separate allocation-free monotonic sequences for
logging, NullFS, tmpfs, and VFS. Every startup attempt consumes a generation independent of process
IDs. PID 1 sends it once in an exact 16-byte `NSGN` v1 record with no capability over a private
endpoint granted to the service with exact `RECEIVE` rights. The service validates those rights,
kernel-stamped sender PID 1, and canonical encoding, closes the bootstrap handle, and accepts the
generation before readiness. NullFS and tmpfs bind filesystem sessions to it; PID 1 registers the
matching generation with kernel proxies; logging also binds its collector, `NSLS`, NSWP, and routes.
A restartable service manager must eventually own the sequences and receive their state across
replacement. The current contract provides no durable cross-boot persistence.

Stable protocols should define:

- canonical protocol identity;
- major and minor versions;
- required and optional feature negotiation;
- bounded field and collection sizes;
- request, reply, event, and cancellation behavior;
- retry safety and idempotence;
- handle types and required rights;
- lifecycle and peer-restart semantics.

The wire format must not depend on Rust memory layout, native pointers, `usize`, compiler
padding, or compiler-private enum representation. Unknown optional fields should be
ignored only where the protocol explicitly makes that safe; incompatible major versions
must fail cleanly.

A future interface-definition language should generate Rust clients and servers,
validation, protocol identifiers, tracing metadata, test mocks, and documentation. The
IDL is a userspace tool, not a reason for the kernel to parse service protocols.

## Security validation

Every IPC operation must validate before committing state:

- user pointers and integer arithmetic;
- payload and handle counts;
- source handle ownership and rights;
- object type and transfer policy;
- queue and receiver-table capacity;
- job and resource-domain limits;
- mapping size, alignment, and protection;
- peer lifecycle changes during the operation.

Operations transferring multiple handles must be atomic from userspace's perspective.
All parser inputs, shared-memory contents, and peer replies remain untrusted even when a
channel connection was authorized.

## Observability

IPC tracing should be designed in rather than added after service decomposition. With
appropriate inspection authority, diagnostic tools should be able to observe:

- sender and receiver service or application identity;
- object and channel identities;
- protocol, method, and transaction identifiers;
- payload and handle counts;
- queue delay, service time, and deadline misses;
- cancellation, peer closure, malformed replies, and restart boundaries;
- priority donation and wait-chain relationships.

Payload contents are sensitive and must not be logged by default. A future `ipc-trace`,
`handle-list`, service inspector, and graphical wait-chain view should operate on
metadata and explicit debug authority.

## Compatibility streams

Pipes and byte streams remain necessary for shell pipelines, standard input and output,
POSIX compatibility, log streams, and some subprocess protocols. They should coexist
with channels rather than replace them.

Structured native service APIs should use channels because pipes alone do not provide
typed message boundaries, handle transfer, cancellation, peer lifecycle, or explicit
authority delegation.

## Implementation sequence

The current system already provides process-local capability tables, rights-reduced
copying, bounded endpoints, counted notifications, copied shared-memory objects,
endpoint waiting, and direct-child bootstrap grants. The intended evolution is:

1. formalize a common kernel-object and handle-table implementation with object type,
   lifetime, rights, signals, close, duplicate, replace, and inspection semantics;
2. replace one-ended endpoint assumptions with channel pairs, peer closure, bounded
   queues, and atomic move-transfer of multiple handles;
3. add general object waiting with monotonic deadlines, followed by persistent wait sets
   or event ports;
4. add mapped shared-memory objects, per-mapping protection, sealing, accounting, and
   explicit W^X integration;
5. add cancellation and an optimized synchronous call/reply path with bounded priority
   donation;
6. introduce the one-bootstrap-channel process-startup contract and remove ambient
   native handle inheritance;
7. build the userspace IPC runtime, typed service bindings, tracing, and protocol
   conformance tests;
8. add an IDL compiler only after the wire and lifecycle rules have survived real
   service implementations;
9. keep pipes, descriptors, and POSIX interfaces as translated compatibility surfaces.

## Required invariants

Future implementation should preserve these invariants:

> Handles are process-local, opaque, typed by their target object, and carry immutable
> rights that may only be preserved or reduced.

> Channels carry bounded control messages and explicitly transferred capabilities;
> shared memory carries bulk data.

> Application-level protocols remain in userspace, while the kernel validates only
> object, rights, memory, queue, scheduling, and lifecycle mechanics.

> Native process startup receives explicit handles through one bootstrap channel rather
> than ambient inheritance or arbitrary process-ID-based injection.

> Resource exhaustion, peer failure, cancellation, timeout, and malformed messages are
> normal, defined outcomes rather than undefined behavior.
