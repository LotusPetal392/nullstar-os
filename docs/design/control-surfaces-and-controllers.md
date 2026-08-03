# Control surfaces and application-controller direction

## Status

A capability-controlled, bidirectional control-device service is **accepted direction**.
Applications interpreting controller meaning and maintaining device feedback through
private controller components is **accepted direction**.

The exact service names, controller-profile format, standard semantic vocabulary, and
MIDI 2.0 feature set remain **tentative design**. Broad vendor-specific controller
support is a **distant goal**.

## Scope

This design covers devices used to control application state, including:

- MIDI keyboards and pad controllers;
- mixer and digital-audio-workstation control surfaces;
- motorized faders and illuminated transport controls;
- application-specific controllers with displays or vendor protocols;
- MIDI clock, automation, and parameter-control sources;
- future non-MIDI devices that expose comparable bidirectional controls.

Control surfaces are not merely one-way input devices. An application must often send
state back to the device for LEDs, pad colors, motorized faders, encoder rings, text or
graphics displays, mode changes, and device initialization.

## Architectural split

NullStar separates physical transport, application interpretation, and timed graph data.

```text
Physical controller
        |
        v
System control-device service
    discovery, transport, ownership, permissions, timestamps
        |
        v
Application controller component
    profiles, mappings, state binding, feedback, modes
        |
        +----> application commands and UI state
        +----> application-local media graph ports
        <---- application and graph state for device feedback
```

The system owns device discovery, endpoint transport, access policy, and stable device
identity. Applications own the meaning of controls and the state shown by their
controllers.

The media graph carries timestamped MIDI, automation, clock, and control-event streams.
The controller component handles LEDs, displays, motorized controls, banking, mode
switches, protocol initialization, and other control-plane state.

## System control-device service

The working service name is `control-device-service`; public protocol names remain
tentative. Its responsibilities include:

- discovering and removing devices;
- exposing typed input and output endpoints;
- grouping related USB or transport endpoints into one logical device;
- assigning stable device identities where hardware permits;
- timestamping received events against a monotonic clock;
- preserving message ordering within an endpoint;
- granting shared, exclusive, or partitioned sessions;
- enforcing input, output, raw-protocol, and system-routing capabilities;
- transporting MIDI 1.0, MIDI 2.0 UMP, SysEx, and bounded vendor messages;
- reporting disconnect, reconnect, reset, and endpoint-change events;
- accounting for queue usage and misbehaving clients.

The service should avoid interpreting application-specific meaning. It may understand
transport and protocol framing, but a privileged driver should not contain complex DAW,
controller-layout, or vendor-workflow policy.

## Logical devices and endpoints

One physical controller may expose several input and output ports. The device service
should group them into one logical device descriptor.

```text
Controller device
├── performance input
├── control-surface input
├── feedback output
├── display output
└── vendor-protocol endpoint
```

Each endpoint declares:

- direction;
- transport and message format;
- maximum event or packet size;
- timestamp support;
- sharing and ownership policy;
- whether bidirectional pairing is required;
- reset and initialization behavior;
- whether raw vendor access needs an elevated grant.

Applications should select devices by descriptors and persistent grant identities, not
by unstable enumeration order or a transport-specific pathname.

## Bidirectional sessions

A controller session provides independent inbound and outbound paths.

Inbound events may include:

- note and polyphonic-expression data;
- control changes and pitch bend;
- pads, keys, buttons, encoders, faders, and touch strips;
- transport controls and clock events;
- device-specific messages and status reports.

Outbound events may include:

- LED and pad illumination;
- button and transport-state feedback;
- motorized-fader positions;
- encoder-ring and meter values;
- text or graphics for device displays;
- mode, page, bank, and color changes;
- SysEx or vendor-specific configuration.

Control and lifecycle operations use NSIDL. Event batches use bounded queues or mapped
shared-memory rings when message rates justify them. An application must not require one
blocking IPC transaction for every MIDI message.

Conceptually, a session exposes operations equivalent to:

```text
ControlDeviceSession
├── descriptor
├── inbound event stream
├── outbound event sink
├── synchronization and reset operations
├── profile or raw-protocol selection
└── lifecycle notifications
```

## Application controller component

An application may instantiate a private controller component through the component
runtime. This component is analogous to an application-local media graph: it provides a
standard system implementation without requiring every application to embed a complete
controller framework.

The component is responsible for:

- matching devices to profiles;
- interpreting physical controls;
- mapping controls to commands and parameters;
- scaling, smoothing, curves, dead zones, and pickup behavior;
- banking, paging, layers, and modes;
- maintaining authoritative feedback state;
- preventing feedback loops;
- reconnecting and resynchronizing devices;
- translating vendor protocols into semantic controls;
- exporting timed event ports into the application media graph.

Applications use a thin SDK crate and generated NSIDL bindings. The controller component
may run in a shared trusted host, a dedicated sandbox, or the application process when
specialized latency or protocol requirements justify it.

A shared host must not load untrusted vendor protocol code without isolation.

## Three event destinations

Not every controller event belongs in the media graph. The controller component routes
an event according to its meaning:

```text
controller event
├── timed media-graph event
├── application command
└── UI or parameter binding
```

Examples:

- a keyboard note is routed to a synthesizer node;
- MIDI clock is routed to a sequencer or synchronization node;
- a transport button invokes an application command;
- a fader changes a mixer parameter;
- a navigation encoder changes application selection or UI focus.

The global system graph should not be required to understand that one controller button
means “arm track four” in a particular application.

## State binding and feedback

Controller feedback should normally be declarative and state-driven rather than a series
of unrelated raw output writes.

A binding associates:

- one semantic or physical control;
- an input action or parameter target;
- an observable authoritative state;
- an output-feedback representation;
- scaling, quantization, and conflict rules.

When a parameter changes through the controller, mouse, keyboard, automation, another
component, or session restoration, the controller component observes the authoritative
application state and updates the device.

```text
physical fader movement
        -> application parameter
        -> authoritative state
        -> motor position or indicator feedback

mouse click on mute
        -> authoritative state
        -> controller mute LED
```

The component must suppress feedback loops and should coalesce superseded display or LED
updates. Motorized controls require touch awareness, pickup policy, rate limits, and safe
handling of abrupt state changes.

Applications remain able to send reviewed raw output for devices that cannot be modeled
through standard bindings.

## Controller profiles

A controller profile maps transport-level messages and device capabilities to semantic
controls. Profiles may be supplied by:

- NullStar system packages;
- hardware vendors;
- applications;
- users;
- signed community packages.

A profile may describe:

- device matching rules;
- endpoint grouping;
- named controls and control arrays;
- input message encodings;
- output feedback encodings;
- value resolution and normalization;
- displays, colors, meters, and motorized capabilities;
- initialization and shutdown sequences;
- modes, banks, pages, and modifiers;
- required vendor-protocol permissions.

Profiles describe hardware semantics. Applications decide what those semantics control.
A profile parser must be bounded and should not execute arbitrary native code. Complex
vendor protocols belong in sandboxed protocol components.

## Standard semantic controls

NullStar should define a small extensible vocabulary for common controls, for example:

```text
transport.play
transport.stop
transport.record
transport.loop

channel.N.volume
channel.N.pan
channel.N.mute
channel.N.solo
channel.N.arm
channel.N.select

navigation.left
navigation.right
navigation.up
navigation.down

parameter.N.encoder
parameter.N.touch
parameter.N.display
```

A hardware profile maps protocol messages to semantic controls. An application maps
semantic controls to its own commands and parameters.

This enables useful generic mappings without claiming that every advanced controller can
be represented by one universal layout. Application-specific namespaces and raw vendor
endpoints remain available when standard semantics are insufficient.

## Vendor-specific protocols

Some controllers expose custom USB interfaces, framebuffer-like displays, proprietary
color encodings, high-resolution input, or command protocols beyond standard MIDI.

NullStar should expose these through a scoped raw-protocol capability:

```text
hardware driver
    -> bounded raw endpoint
    -> control-device service
    -> sandboxed vendor protocol component
    -> semantic controls and feedback
    -> application
```

The transport driver remains simple. Complex parsers and vendor state machines execute
outside privileged driver code and receive only the device endpoints and resources they
need.

Raw protocol access must not implicitly grant unrelated USB, HID, filesystem, network,
or display authority.

## Ownership and sharing

Controller endpoints declare one of several access models.

### Shared input

Appropriate for ordinary performance input or monitoring:

```text
keyboard input
├── synthesizer
├── recorder
└── MIDI monitor
```

Each consumer receives independently bounded queues. One slow consumer must not block
others.

### Exclusive session

Appropriate for integrated application control surfaces whose outputs, modes, and
displays require one authoritative owner.

Only the owner may write feedback or change device modes. Other applications may be
denied input or receive a separately declared shared endpoint.

### Partitioned ownership

A device may expose independently claimable endpoint groups, such as:

```text
performance keys -> synthesizer
control surface  -> DAW
transport port   -> session controller
```

Output ownership is normally stricter than input sharing because conflicting LED,
display, or motor commands produce undefined and potentially unsafe behavior.

The session policy manager may remember user-approved bindings, but reconnecting an
application does not bypass current capability and session policy.

## Media-graph integration

The application-local media graph treats MIDI and timed control data as first-class
stream types. A controller component may export ports such as:

```text
notes: midi-events
clock: clock-events
automation: control-events
```

These streams carry monotonic or graph-relative timestamps so nodes can align events to
processing quantums or audio frames.

Appropriate graph uses include:

- notes and expression;
- MIDI clock and transport timing;
- automation streams;
- continuous controllers and parameter changes;
- sequencer and synthesizer routing.

The graph is not the primary owner of:

- controller displays;
- LEDs and pad colors;
- motorized-fader synchronization;
- mode switching and banking;
- device initialization;
- profile selection;
- application command mapping.

Those remain control-plane responsibilities of the application controller component.

See [Media graph](media-graph.md).

## Timing and latency

Inbound events should be timestamped as close to receipt as practical. Device, transport,
and scheduling latency should be observable so applications can compensate or diagnose
problems.

Realtime event paths must avoid unbounded allocation, blocking control IPC, profile
parsing, device discovery, and display formatting. Controllers with high-rate or
sample-sensitive data should use preallocated event batches and shared rings.

Ordinary LED and display feedback may be coalesced and delivered on a normal-priority
control path. Time-sensitive feedback, such as sequencer position or tightly coupled
motor control, may request bounded scheduling policy without gaining unlimited realtime
priority.

## Hotplug and restoration

On disconnect:

- graph ports become unavailable;
- the application receives a lifecycle event;
- pending output is discarded;
- application state remains authoritative;
- bindings and persistent grant identities are retained according to policy.

On reconnect:

1. the service identifies the logical device;
2. ownership and capabilities are revalidated;
3. the controller profile and protocol component are restored;
4. the application supplies or confirms a complete state snapshot;
5. LEDs, displays, modes, and motorized controls are synchronized;
6. timed graph ports resume with a new connection generation.

Late events from a previous connection generation must not be delivered to the restored
session. A reconnected controller must not display stale state left by a crashed or
previously owning application.

## Security and privacy

Capabilities should distinguish at least:

- device enumeration;
- input subscription;
- output and feedback control;
- exclusive ownership;
- raw MIDI or SysEx;
- vendor-protocol access;
- system-wide routing or observation.

An application granted note input does not automatically gain device-display output,
raw vendor commands, or visibility into other applications’ controller routes.

Device and profile descriptors may contain stable hardware identifiers. APIs and logs
should expose only the identity detail required for matching, grants, and diagnostics.

Malformed profiles, oversized SysEx, excessive event rates, stalled queues, and abusive
feedback traffic must be bounded and attributable to the responsible component.

## Proposed implementation pieces

Tentative component and crate responsibilities are:

```text
control-device-service
    discovery, transport, endpoint ownership, capabilities, timestamps

controller-profile-service
    bounded profile lookup, validation, and precedence

application-controller-component
    mappings, state synchronization, feedback, banking, modes

media-graph-runtime
    timestamped MIDI, clock, automation, and control-event routing

nullstar-control
    public value types and ergonomic application API

nullstar-control-client
    thin generated bindings and runtime integration
```

Names are provisional. Responsibilities and privilege boundaries are more important than
crate or executable naming.

## Staged implementation

1. Basic MIDI 1.0 device discovery, shared input, exclusive output, timestamped event
   batches, and one thin SDK client.
2. Application controller components, reconnect generations, declarative parameter and
   LED bindings, and simple user mappings.
3. Controller profiles, semantic controls, endpoint grouping, SysEx policy, and
   application-local media-graph ports.
4. MIDI 2.0 UMP, high-resolution controls, clock synchronization, motorized faders,
   display feedback, and partitioned ownership.
5. Sandboxed vendor protocol components, signed profile packages, complex displays, and
   broad application-specific controller support.

## Open questions

- Exact NSIDL protocols and shared-ring layouts.
- Profile schema, signing, precedence, and user override rules.
- Stable identity when devices expose no serial number.
- Interaction between session restoration and exclusive ownership.
- Minimum standard semantic-control vocabulary.
- Whether selected session-wide transport controls should be mediated by a desktop
  session service.
- Scheduling policy for high-rate controls and tightly synchronized feedback.
- MIDI 2.0 property exchange and profile negotiation scope.
- Safe policy for raw SysEx and vendor firmware-update commands.
