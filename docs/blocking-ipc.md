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

System information reports ABI minor version 3 and advertises the endpoint-wait feature bit. The original endpoint send and receive operations remain bounded and retain their Phase 1 behavior.

## Generic object waiting

ABI 1.23 adds `OBJECT_WAIT_ONE` alongside this compatibility primitive. It blocks on a requested
subset of the level-triggered endpoint, notification, or job signals and accepts an absolute
deadline in the nanosecond domain returned by `MONOTONIC_TIME`. Deadline zero polls immediately,
`UINT64_MAX` waits indefinitely, and finite expiry returns `ETIMEDOUT`. The generic wait returns the
requested asserted mask and does not consume object state.

ABI 1.24 adds `OBJECT_WAIT_MANY` over one to 16 `{handle, requested_signals}` entries. The kernel
copies and validates the complete array before inspecting readiness, then returns the lowest array
index whose requested mask intersects current object state. The same absolute-deadline rules apply,
and each process may have only one outstanding generic one- or many-object wait.

## Current boundary

The original endpoint primitive removes cooperative polling from request/reply clients and remains
the blocking foundation for existing proxies. Generic bounded object waiting now supplies timeout
deadlines and readiness selection, but neither interface provides transaction identifiers,
kernel-owned reply slots, cancellation messages, persistent wait sets, or restart-aware kernel file
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
