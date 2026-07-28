# Media-graph direction

## Status

A userspace, capability-controlled, clock-aware media graph is **accepted direction**.
The first implementation remains intentionally narrow. Video integration and network
media are **distant goals**.

## Core model

NullStar media streams are ports in a graph of nodes and links. Anything that can
produce a compatible stream may connect to anything that can consume it, subject to
permissions and policy.

Core objects are:

- **node**: an application, device, processor, mixer, converter, or virtual endpoint;
- **port**: a typed input or output advertising supported formats;
- **link**: a negotiated connection between ports;
- **clock domain**: the timing source that drives one portion of the graph.

The graph mechanism and desktop policy must be separate. The graph service manages
nodes, links, transport, timing, and negotiation. A session-policy manager chooses
default devices, restores routes, handles hotplug, and applies user preferences.

## Formats and conversion

Audio ports advertise supported sample formats, sample rates, channel layouts, buffer
quantums, and latency ranges. Link negotiation should prefer a direct common format
and insert explicit processing nodes when conversion is needed.

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

Control operations use normal IPC. Audio samples use mapped shared-memory ring
buffers and bounded events or completion notifications. The kernel does not interpret
audio formats.

The graph should be clock-driven and primarily pull-based. A destination clock requests
the next quantum; upstream applications and processors provide the frames needed for
that deadline. Applications may fill buffers ahead of time, but hardware timing drives
the active processing cycle.

Independent hardware and network clocks drift even when configured for the same
nominal sample rate. Links crossing clock domains require adaptive resampling or
elastic buffering.

## Realtime rules

The realtime engine must not allocate from an unbounded heap, access files, perform
blocking control IPC, restructure the graph under locks, or initialize codecs in the
processing path.

A normal-priority control thread builds a new immutable graph snapshot. The realtime
worker switches snapshots at a safe quantum boundary.

The scheduler grants bounded realtime execution to trusted graph workers. The media
graph may temporarily preempt interactive compositor work to meet an audio deadline,
but it receives a measured CPU budget rather than unlimited priority. Repeated budget
or deadline violations cause warning, bypass, silence substitution, disconnection, or
restart according to policy.

## Latency

Every node and link should report minimum, preferred, and maximum latency. The graph
calculates end-to-end latency and can offer policy classes such as power-saving,
normal desktop, interactive, and professional audio.

Processors that add algorithmic delay must report it. Parallel paths require delay
compensation so dry and processed signals remain synchronized.

## Permissions

Applications receive capabilities for specific operations such as playback,
microphone capture, system-audio capture, virtual-device creation, and control of
foreign streams. Playback permission does not imply capture or graph-inspection
authority.

Sensitive capture requests should pass through a user-facing broker. Applications
receive only the approved source or route. Device providers delegate attenuated ports
rather than exposing raw hardware globally.

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
2. Capture, permission brokerage, multiple devices, hotplug, resampling, format
   conversion, and channel mapping.
3. Arbitrary graph routing, virtual devices, processing nodes, graph inspection, and
   saved routing profiles.
4. Multiple clock domains, adaptive resampling, latency negotiation, MIDI timing, and
   professional low-latency policy.
5. Video ports, cameras, screen capture, hardware codecs, and audio/video
   synchronization.

## Open questions

- Ring-buffer and completion protocol details.
- Realtime thread placement across multiple hardware clock domains.
- The internal format and quantum used by the first implementation.
- Policy for moving active streams between devices with incompatible clocks.
- Whether video ultimately shares the same graph daemon or only the common protocol
  model.
