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

## Current boundary

This primitive removes cooperative polling from request/reply clients and is the blocking foundation for a future kernel-to-userspace VFS proxy. It does not yet provide transaction identifiers, kernel-owned reply slots, cancellation messages, timeout deadlines, or restart-aware kernel file descriptors.
