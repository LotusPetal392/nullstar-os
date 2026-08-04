# Tmpfs mount routing

Phase 4 adds a restart-aware mount contract above the Phase 3 userspace tmpfs
service. The request endpoint remains stable across supervised service restarts,
but endpoint identity alone is not enough: the replacement service has lost its
volatile filesystem state and old client assumptions must not silently continue.

## Mount handshake

Before readiness, tmpfs accepts one manager-issued nonzero generation from PID 1 through the strict
`NSGN` bootstrap handoff. A client then sends the versioned `MOUNT` operation, and the running service
returns that generation rather than deriving lifecycle identity from its process ID. The client stores
the value in a typed `tmpfs::Mount` and includes it in every subsequent request.

The service compares each request generation with its current generation before
accessing filesystem state. A mismatch returns `STALE_MOUNT` without changing
state. Clients must discard stale mounts and reconnect.

## Security and recovery properties

- A client still receives only `SEND` authority to the service request endpoint.
- Every request still transfers a private send-only reply capability.
- Old clients cannot accidentally operate on a replacement service's new state.
- Protocol records remain bounded below the endpoint message-size limit.
- A service restart invalidates all outstanding mount identities deterministically.
- Remounting is explicit and obtains the replacement service generation.

## Current boundary

The kernel now uses this mount identity for ordinary `/tmp/<name>` file syscalls.
PID 1 verifies that the mount handshake echoes the generation it issued, then registers the service
request endpoint and that same generation. For proxied `open`, `read`, `write`, `stat`, and directory syscalls,
the kernel queues a bounded protocol request to `/tmpfs-service`, transfers a
private send-only reply endpoint, blocks the calling process, and completes the
saved syscall frame from the service reply.

The kernel still owns descriptor tables, standard-stream redirection, offsets,
append mode, close-on-exec state, process scheduling, signal interruption, and
exit cleanup. The kernel tmpfs remains mounted for kernel-internal fixtures and
legacy smoke-test compatibility, but userspace programs that use normal `/tmp`
file APIs are routed through the supervised service.

Remaining follow-up work includes richer directory records, larger file/storage
bounds, recursive paths, and deterministic replay or cancellation semantics for
requests that are in flight while the service is replaced.
