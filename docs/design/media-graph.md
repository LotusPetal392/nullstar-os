# Media-graph direction

## Status

A userspace, capability-controlled, clock-aware media graph is **accepted direction**.
Applications owning private processing graphs while the system owns routing between
exported endpoints is also **accepted direction**.

A runtime-hosted application graph with an optional in-process execution mode is
**tentative design**. Video integration and network media are **distant goals**.

## Core model

NullStar media streams are ports in graphs of nodes and links. Anything that can
produce a compatible stream may connect to anything that can consume it, subject to
permissions and policy.

Core objects are:

- **node**: an application, device, processor, mixer, converter, or virtual endpoint;
- **port**: a typed input or output advertising supported formats;
- **link**: a negotiated connection between ports;
- **clock domain**: the timing source that drives one portion of a graph;
- **graph context**: one ownership, lifecycle, resource-accounting, and scheduling
  boundary containing nodes, links, and exported ports.

The graph mechanism and desktop policy must be separate. Graph runtimes manage nodes,
links, transport, timing, and negotiation. A session-policy manager chooses default
devices, restores routes, handles hotplug, and applies user preferences.

## Two-level graph architecture

NullStar distinguishes between private application processing and system-wide routing.

```text
Application-local graph
    source -> decoder -> effects -> mixer -> exported port
                                           |
                                           v
System media graph
    exported application port -> policy -> device, recorder, or another application
```

Applications own their internal processing graphs. The system owns routing between
exported application ports, system services, virtual endpoints, and physical devices.

This boundary keeps application implementation details out of the global graph. A music
player may internally contain a decoder, ReplayGain stage, equalizer, crossfade mixer,
and visualization tap while exporting only one `main` output. A professional application
may deliberately export additional monitor, cue, track, side-chain, MIDI, or control
ports.

The system must not require every decoder, effect, or internal mixer to become a global
node. Doing so would expose private state, enlarge the global failure domain, complicate
policy, and create unnecessary IPC and namespace churn.

## Application-local media graphs

An application may create one or more private graph contexts. These contexts may be
used for:

- decoding and encoding;
- application-owned mixing and routing;
- effects and signal processing;
- resampling and format conversion;
- synchronization and buffering;
- visualization and metering taps;
- timed MIDI and control-event processing;
- video processing in later implementations.

Only explicitly exported ports become visible to the system graph. Internal nodes and
links remain private unless the application grants inspection or debugging authority.

Exported ports have stable identities within the application generation and declare:

- stream type;
- direction;
- supported formats;
- latency range;
- clock behavior;
- visibility and routing policy;
- whether the port is persistent, dynamic, or tied to one document or session.

A simple player may export only:

```text
outputs:
    main: audio
```

A digital audio workstation may export:

```text
outputs:
    main: audio
    cue: audio
    track.1: audio
    track.2: audio
    midi.clock: midi-events

inputs:
    microphone: audio
    sidechain: audio
    controller: control-events
```

## Application graph runtime

Applications should use a stable graph API without being required to embed the complete
media engine. The SDK should separate:

```text
media API and value types
thin client bindings
runtime-hosted graph engine
sandboxed codec and plugin hosts
system media service
```

The thin application crate provides graph construction, typed nodes and ports, event
handling, and error types. The heavy graph scheduler, standard processors, format
negotiation, and transport implementation may execute in a runtime-managed component.

The component runtime creates a graph context with explicit capabilities and resource
budgets. The graph host must not automatically inherit all application capabilities.
Codecs, plugins, renderers, and other high-risk processors should receive only the
streams and services required for their declared role.

### Hosted execution

Hosted execution is the default for ordinary applications. A graph executes in a
runtime-managed component host or dedicated graph process.

Benefits include:

- reduced application size;
- one centrally maintained implementation;
- codec and processor isolation;
- independent crash recovery;
- consistent diagnostics and resource accounting;
- system-wide security fixes without rebuilding every application.

The service broker participates in connection setup and capability routing, but it must
not proxy every realtime operation. After authorization, the application and graph host
communicate through direct endpoints and shared-memory transports.

### In-process execution

Latency-sensitive applications may request an in-process graph engine through their
manifest. Expected users include:

- digital audio workstations;
- software synthesizers;
- live effects processors;
- games with specialized audio engines;
- professional recording and monitoring tools;
- applications implementing custom low-latency nodes.

The public graph API and graph description should remain substantially the same in both
modes. Policy may deny in-process execution for untrusted extensions or require that
particular codecs and plugins remain isolated even when the main graph is embedded.

A component is a security and lifecycle boundary; it does not imply that every node is a
separate process.

## Standard and custom nodes

The runtime should provide reviewed standard nodes such as:

- decoders and encoders;
- mixers, splitters, queues, and selectors;
- resamplers and sample-format converters;
- channel mappers and interleaving converters;
- gain, pan, limiting, and delay;
- equalizers and common filters;
- synchronization and clock-domain bridges;
- visualization and metering taps;
- application output and input endpoints.

Standard trusted nodes may share one graph host. Untrusted, failure-prone, or
vendor-supplied nodes should run in isolated component hosts and exchange stream data
through bounded shared memory.

Applications may implement custom nodes in-process when policy allows. A future stable
native plugin ABI and LV2 hosting model should layer over the same graph and component
boundaries rather than placing third-party plugin code inside the privileged system
mixer.

## System media graph

The system graph connects exported ports to:

- physical playback and capture devices;
- other applications;
- virtual devices and loopback endpoints;
- recorders and screen-capture services;
- Bluetooth and USB media devices;
- network endpoints in later implementations;
- session-level processors such as echo cancellation.

It is responsible for routing policy, device selection, per-stream volume, user-visible
inspection, permission enforcement, route restoration, and system-level clock-domain
coordination.

The system graph should see application endpoints rather than every private processor.
Application failure must not stop unrelated routes. A stalled producer yields silence;
a malformed or crashed node is disconnected, bypassed, or restarted while unrelated
device paths continue.

## Formats and conversion

Audio ports advertise supported sample formats, sample rates, channel layouts, buffer
quantums, and latency ranges. Link negotiation should prefer a direct common format and
insert explicit processing nodes when conversion is needed.

Automatic processors may include:

- sample-rate conversion;
- integer and floating-point sample conversion;
- channel mapping and upmix or downmix;
- interleaved and planar layout conversion;
- mixing, volume, limiting, and delay.

For PCM, “bitrate conversion” is normally the consequence of sample-rate, format, and
channel conversion. Compressed codecs should remain outside the core realtime mixer
unless an endpoint explicitly requires encoded media.

The first playback implementation should use one internal format, preferably 48 kHz
32-bit floating-point PCM, to keep the initial engine verifiable. General negotiation
is added after the fixed-format path is reliable.

## Transport and processing

Control operations use normal IPC. Audio samples and other high-rate streams use mapped
shared-memory ring buffers with bounded events or completion notifications. The kernel
does not interpret audio or MIDI formats.

Graph setup operations include creating contexts, adding nodes, connecting ports,
setting properties, committing graph snapshots, and starting or stopping processing.
These operations occur on normal-priority control threads.

The realtime data path must not perform one synchronous IPC transaction per sample,
MIDI event, or processing quantum. Once links are established, graph workers exchange
bounded buffers directly. A hosted graph schedules connected internal nodes without
returning to the application after each block.

The graph should be clock-driven and primarily pull-based. A destination clock requests
the next quantum; upstream applications and processors provide the frames needed for
that deadline. Applications may fill buffers ahead of time, but hardware timing drives
the active processing cycle.

Independent hardware and network clocks drift even when configured for the same
nominal sample rate. Links crossing clock domains require adaptive resampling or
elastic buffering.

## Realtime rules

The realtime engine must not allocate from an unbounded heap, access files, perform
blocking control IPC, restructure the graph under locks, initialize codecs, or load
plugins in the processing path.

A normal-priority control thread builds a new immutable graph snapshot. The realtime
worker switches snapshots at a safe quantum boundary.

The scheduler grants bounded realtime execution to trusted graph workers. The media
graph may temporarily preempt interactive compositor work to meet an audio deadline,
but it receives a measured CPU budget rather than unlimited priority. Repeated budget
or deadline violations cause warning, bypass, silence substitution, disconnection, or
restart according to policy.

Synchronous control calls on latency-sensitive paths should support direct scheduler
handoff and bounded priority propagation where the IPC and scheduler designs permit it.
Possession of a validated endpoint capability should authorize ordinary operations
without repeating full manifest-policy evaluation on every message.

## Latency

Every node and link should report minimum, preferred, and maximum latency. The graph
calculates end-to-end latency and can offer policy classes such as power-saving,
normal desktop, interactive, and professional audio.

Processors that add algorithmic delay must report it. Parallel paths require delay
compensation so dry and processed signals remain synchronized.

Hosted execution adds a process boundary, but its hot path should remain shared-memory
based. In-process execution exists for applications whose latency or custom-node needs
justify reducing isolation. The architecture must be benchmarked before exact latency
budgets become stable contracts.

## MIDI and control-event streams

MIDI and other timestamped control data are first-class graph stream types rather than
being encoded as audio or ordinary un-timed RPC messages.

Initial stream categories should include:

```text
audio
midi-events
control-events
clock-events
```

MIDI notes, clock, automation, and continuous-controller data may be routed to graph
nodes with sample-accurate or quantum-relative timestamps. Device discovery, ownership,
LED feedback, motorized controls, displays, and application command mapping belong to
the control-device architecture rather than the realtime media graph itself.

See [Control surfaces and application controllers](control-surfaces-and-controllers.md).

## Permissions

Applications receive capabilities for specific operations such as playback,
microphone capture, system-audio capture, virtual-device creation, exported-port
publication, and control of foreign streams. Playback permission does not imply capture,
graph-inspection, or global routing authority.

Sensitive capture requests should pass through a user-facing broker. Applications
receive only the approved source or route. Device providers delegate attenuated ports
rather than exposing raw hardware globally.

An application graph host receives the minimum capabilities needed for its graph. A
private decoder does not gain microphone or network authority merely because the
application owns those capabilities elsewhere.

## Virtual routing

Virtual nodes are a normal consequence of the graph model. Planned uses include
loopback, monitor streams, virtual microphones, application recorders, game and voice
mixes, echo cancellation, and remote endpoints.

Application failure must not stop unrelated routes. A stalled producer yields silence;
a malformed or crashed node is disconnected or restarted while the device graph
continues.

## Staged implementation

1. One output device, fixed-format playback, software mixing, per-stream volume, and
   silence on underrun.
2. A thin application media API, private graph contexts, one hosted graph engine,
   standard playback nodes, and one exported application output.
3. Capture, permission brokerage, multiple devices, hotplug, resampling, format
   conversion, and channel mapping.
4. Arbitrary system routing, virtual devices, processing nodes, graph inspection, and
   saved routing profiles.
5. In-process graph execution, isolated custom nodes, timestamped MIDI and control-event
   streams, multiple clock domains, adaptive resampling, latency negotiation, and
   professional low-latency policy.
6. Video ports, cameras, screen capture, hardware codecs, network media, and audio/video
   synchronization.

## Open questions

- Ring-buffer and completion protocol details.
- Realtime thread placement across multiple hardware clock domains.
- The internal format and quantum used by the first implementation.
- Policy for moving active streams between devices with incompatible clocks.
- Exact graph-description format and transaction model.
- Criteria for dedicated graph processes versus shared trusted hosts.
- Stable custom-node ABI and interaction with LV2 hosting.
- Whether video ultimately shares the same graph daemon or only the common protocol
  model.
