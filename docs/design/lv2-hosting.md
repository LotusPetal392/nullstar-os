# LV2 hosting direction

## Status

LV2 support is a **distant compatibility goal**. It must not constrain the native media
graph or delay basic desktop audio.

## Architecture

LV2 plugins should run in a userspace host that exposes each plugin instance as an
ordinary NullStar media-graph node. Plugins are never loaded into the kernel or the
core graph service.

The host translates between native graph concepts and LV2 audio, control, CV, Atom,
MIDI, state, worker, and UI facilities. NullStar's native graph remains authoritative
for routing, clocks, latency, permissions, transport, and failure policy.

## Isolation

The host should support shared-process, per-chain, and per-plugin isolation. Per-chain
is the preferred default compromise. Third-party plugins are untrusted native code and
receive only shared media buffers, graph endpoints, bounded realtime scheduling, a
private state location, and user-approved files.

A plugin crash or deadline miss must not stop the device graph. Policy may bypass,
mute, restart, or quarantine the plugin and restore its last saved state.

## Realtime behavior

DSP execution occurs within a graph quantum using preallocated buffers and event
storage. Scanning, file access, allocation, UI work, and graph restructuring remain on
normal-priority threads. LV2 Worker support is required before broad compatibility so
plugins can move non-realtime tasks out of the DSP callback.

Every chain receives a measured CPU budget. Plugin-reported latency feeds the graph's
end-to-end latency accounting and delay-compensation system.

## Discovery and interfaces

A background scanner should validate `.lv2` bundles and build a database of plugin
URIs, ports, required features, UIs, presets, architecture, and crash history. Porting
Lilv and its dependencies is preferable to inventing a separate LV2 metadata stack.

DSP compatibility comes first. A generated NullStar control interface should be the
first UI target. Foreign embedded UIs are deferred; external UI processes may be more
practical before toolkit compatibility exists.

## State and automation

The host stores LV2 plugin state separately from NullStar graph state. Graph state
covers connections, ordering, bypass, wet/dry balance, node placement, and selected
presets. MIDI and parameter automation should use timestamped events with frame offsets
inside the current quantum.

## Staged implementation

1. Complete native audio, event, parameter, latency, and processing-node semantics.
2. Add dynamic loading and the required C ABI support.
3. Port LV2 metadata dependencies and build a headless scanner.
4. Host basic audio and control ports in a sandboxed process.
5. Add Atom, MIDI, Worker, State, presets, and delay reporting.
6. Add generated NullStar controls and per-chain isolation.
7. Investigate external and compatible native LV2 UIs.
