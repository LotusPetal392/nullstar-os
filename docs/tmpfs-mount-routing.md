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

This is the restart and identity layer required by a future kernel VFS proxy. It
does not yet redirect ordinary kernel `open`, `read`, `write`, `stat`, or directory
syscalls for `/tmp`. The existing kernel tmpfs remains the compatibility mount.

A safe kernel proxy still needs scheduler-integrated blocking IPC, cancellation
when callers exit or receive signals, request ownership cleanup, descriptor
semantics across service replacement, and rules preventing the kernel from
blocking while holding VFS or process-table locks.
