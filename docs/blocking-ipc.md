# Blocking endpoint waits

Phase 5 adds a scheduler-integrated readiness wait for capability endpoints.

## Contract

The Phase 5 `ENDPOINT_WAIT` syscall validates that the caller holds `RECEIVE` authority. If the endpoint already contains a message, it returns immediately. Otherwise the kernel registers the process as a waiter and marks its scheduler task blocked.

A successful `ENDPOINT_SEND` queues the bounded message and then wakes one waiter in FIFO order. Userspace retries the existing non-blocking receive syscall after waking.

The wait is readiness-based rather than a combined receive. A wake may be spurious, or another receiver may consume the message first, so callers must always retry receive in a loop. No payload, message information, or transferred capability is committed by the wait syscall.

## Lost-wakeup avoidance

Waiter registration and scheduler blocking occur while the endpoint-wait registry remains locked. The send path queues the message first and only then enters the waiter registry, so it cannot miss the transition from runnable to blocked.

The capability registry is never held while waking a process. This keeps the wake path from reversing capability-registry and scheduler lock ordering.

## Cancellation and cleanup

A process is removed from any previous endpoint wait before registering a new wait. Send skips stale waiter records whose tasks are no longer blocked. Signal-induced or otherwise spurious wakeups are safe because the userspace facade rechecks receive and waits again when the endpoint is still empty.

## ABI discovery

System information reports ABI minor version 30 and advertises the endpoint-wait feature bit. The original endpoint send and receive operations remain bounded and retain their Phase 1 behavior.

## Generic object waiting

ABI 1.23 adds `OBJECT_WAIT_ONE` alongside this compatibility primitive. It blocks on a requested
subset of the level-triggered endpoint, notification, job, timer, or event signals and accepts an absolute
deadline in the nanosecond domain returned by `MONOTONIC_TIME`. Deadline zero polls immediately,
`UINT64_MAX` waits indefinitely, and finite expiry returns `ETIMEDOUT`. The generic wait returns the
requested asserted mask and does not consume object state.

ABI 1.24 adds `OBJECT_WAIT_MANY` over one to 16 `{handle, requested_signals}` entries. The kernel
copies and validates the complete array before inspecting readiness, then returns the lowest array
index whose requested mask intersects current object state. The same absolute-deadline rules apply,
and each process may have only one outstanding generic one- or many-object wait.

ABI 1.25 adds paired endpoints. Generic waits can select `PEER_CLOSED`; `READABLE` may remain
asserted alongside it while already queued messages are drained. A compatibility `ENDPOINT_WAIT`
wakes when the peer closes so the receive loop can observe `EPIPE`. Async send, receive, and move-send
registrations wait on peer closure as well as ordinary readiness.

ABI 1.26 adds multi-handle send and receive to the same wake paths. A successful atomic send of up
to four moved handles wakes endpoint and generic-object waiters only after the complete message is
queued. A successful receive wakes writers only after every destination table slot is reserved and
the message is dequeued; insufficient byte or handle capacity leaves readiness asserted.

ABI 1.27 adds persistent wait sets with up to 64 insertion-ordered registrations. A wait snapshots
the registered targets and returns the first ready caller-defined tag plus its asserted signal bits.
The same immediate, finite absolute-deadline, infinite, lost-wakeup, and one-outstanding-wait-per-
process rules apply. Registration changes affect the next wait rather than mutating an in-flight
snapshot.

ABI 1.28 adds event ports with up to 64 persistent registrations and 64 queued events. Registration
captures the target's current requested state and queues it immediately when already asserted.
Subsequent events contain only newly asserted bits. One pending FIFO entry is retained per key;
additional rising bits coalesce into that entry, and the registration rearms only after the relevant
state deasserts. Waiting atomically removes one queued event but never consumes the target state.
Removal also purges a pending event for that key. The same deadline and lost-wakeup rules apply, and
a port may be waited while empty because a separately held `MANAGE` capability can add registrations.

ABI 1.29 adds one-shot monotonic timer objects. Arming replaces any prior arm, clears the fired
level, and accepts the same absolute nanosecond domain as object-wait deadlines. A deadline at or
before the current time fires immediately; `UINT64_MAX` is rejected because cancellation is
explicit. Expiration disarms the timer and asserts `TIMER_FIRED` until cancellation or rearming.
Timers work with generic waits, persistent wait sets, and event ports, so a delayed event-port wait
uses the same lost-wakeup ordering as other object signals.

ABI 1.30 adds manual-reset event objects. `EVENT_SET` persistently asserts `SIGNALED` until an
explicit `EVENT_RESET`; both operations are idempotent. Set and reset flow through the same
scheduler-integrated wake and event-port refresh path as other signal mutations. Repeated set while
already asserted does not queue another edge, while reset followed by set rearms exactly one new
event-port edge. Generic waits and wait sets remain level-triggered and never clear the event.

## Current boundary

The original endpoint primitive removes cooperative polling from request/reply clients and remains
the blocking foundation for existing proxies. Generic bounded object waiting now supplies timeout
deadlines and readiness selection, and persistent wait sets remove repeated registration copying for
larger stable sets. Queued event ports add bounded edge delivery for endpoint, notification, job,
timer, and manual-reset event signals. The userspace reactor now turns generic object signals, counted
notifications, and hierarchical job exits into typed futures over those bounded waits and event ports.
The current kernel interfaces still do not provide transaction identifiers, kernel-owned reply slots,
cancellation messages, periodic timers, file/network completion tokens, or restart-aware kernel file
descriptors.

Kernel service completions may arrive while a different userspace address
space is active. The scheduler therefore provides a bounded
`with_process_address_space` operation for updating a blocked process's syscall
frame: interrupts remain disabled, the target CR3 is installed temporarily,
and the original address space is restored before the scheduler lock is
released. Callbacks may not block, schedule, or recursively acquire the
scheduler. The tmpfs proxy and generation-bound VFS `stat`/`read_directory`
continuations use this path for backend dispatch, signal interruption, and
saved-register publication.
