# Endpoint IPC formal-model boundary

This note records the implementation correspondence for `formal/EndpointIPC.tla`.
It is intentionally narrower than the complete endpoint ABI.

## Modeled commit rules

The model treats successful operations as indivisible authority transitions:

- **plain send:** append one message with no attached authority;
- **copy send:** append one rights-checked capability transfer while retaining the
  sender's source authority;
- **move send:** validate the complete source set and queue capacity, consume all
  moved sources, and append exactly one message;
- **receive:** verify destination capacity, install every attachment from the FIFO
  head, and dequeue exactly that message.

A failed full-queue send or insufficient-capacity receive is represented as a
stutter step: queue and authority state are unchanged.

## Current implementation correspondence

The live userspace-platform endpoint path already has the required ordering.
Move-send validates its complete source set and observes queue capacity before
`remove_entries` consumes the sources; the message is then appended while the
capability registry remains locked. Receive inspects the front message and output
capacity before `insert_entries` installs the complete transfer set, then removes
that same FIFO head while the registry remains locked.

The runtime probe supplies implementation-level regression coverage for the
security-sensitive failures represented by stuttering in the formal model:

- filling the queue and attempting a multi-capability move-send must fail without
  consuming either source;
- a successful multi-capability move-send makes both source handles invalid;
- duplicate source handles in one move-send are rejected while the source remains
  valid;
- a receive with insufficient attachment capacity reports the required count,
  installs nothing, and leaves the endpoint readable;
- retrying with sufficient capacity receives the complete attachment set.

## Deliberately deferred behavior

This phase does not model:

- paired-endpoint peer closure;
- `PEER_CLOSED`, `READABLE`, or `WRITABLE` signal derivation beyond queue bounds;
- scheduler-integrated blocking and wakeups;
- wait-set/event-port registration;
- deadlines or cancellation;
- byte-buffer copying and address validation;
- generation-checked handle encoding, already covered by `HandleGeneration.tla`.

Keeping those concerns separate makes authority-transfer counterexamples small
and leaves room for a later endpoint lifecycle refinement.
