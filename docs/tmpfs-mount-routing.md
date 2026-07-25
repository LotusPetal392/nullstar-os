# Tmpfs mount routing

Phase 4 adds a restart-aware mount contract above the Phase 3 userspace tmpfs
service. The request endpoint remains stable across supervised service restarts,
but endpoint identity alone is not enough: the replacement service has lost its
volatile filesystem state and old client assumptions must not silently continue.

## Mount handshake

A client first sends the versioned `MOUNT` operation. The running service returns
a nonzero generation derived from its process identity. The client stores that
generation in a typed `tmpfs::Mount` value and includes it in every subsequent
request.

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
PID 1 registers the service request endpoint and generation after the mount
handshake. For proxied `open`, `read`, `write`, `stat`, and directory syscalls,
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
