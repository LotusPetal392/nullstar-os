# Userspace tmpfs service

Phase 3 introduces NullStar OS's first real stateful userspace service. The
service is not yet mounted into the kernel VFS; it is reached through the typed
`userspace::tmpfs` client facade while the existing kernel `/tmp` remains the
compatibility path for ordinary file syscalls.

## Process and capability layout

PID 1 creates two endpoint objects before starting `/tmpfs-service`:

- a readiness endpoint, granted to the service with `SEND` only;
- a request endpoint, granted to the service with `RECEIVE` only.

The service receives those capabilities at deterministic handles 1 and 2. It
validates the object types and exact rights before announcing readiness.

A client receives only `SEND` authority to the request endpoint. For each
request it creates a fresh reply endpoint and transfers a `SEND`-only copy to
the service. The client retains `RECEIVE`, so replies cannot be consumed by the
service or another request sender.

```text
client                     tmpfs service
  |                              |
  | request + reply SEND cap --->| request endpoint
  |                              |
  |<----------- reply -----------| private reply endpoint
```

This avoids a shared response queue and gives every request an explicit bounded
reply channel.

## Protocol

`shared/tmpfs_protocol.rs` defines a versioned fixed-layout protocol. Request
and reply records fit within the Phase 1 endpoint message limit. The service
supports:

- write at an offset;
- bounded reads;
- file-size lookup;
- removal;
- newline-separated root listing.

Names are single components. Directories, links, permissions, timestamps, open
file descriptions, and sparse-file policy are deferred.

## Bounds

The first service implementation intentionally uses fixed storage:

| Resource | Limit |
| --- | ---: |
| Files | 16 |
| Name bytes | 48 |
| Bytes per file | 1024 |
| Payload bytes per request/reply | 128 |

Every length, offset, and addition is checked before storage is modified.
Exhaustion and out-of-range access return protocol errors rather than growing
service or kernel memory.

## Boot verification

PID 1 waits for service readiness, then starts `/tmpfs-probe` as a direct child
and grants it request `SEND` authority. The probe verifies:

1. capability type and rights;
2. write;
3. stat;
4. read and content equality;
5. listing;
6. removal;
7. `NOT_FOUND` after removal.

The shell is launched only after the probe exits successfully. The service then
continues under the Phase 2 restart policy.

## Compatibility boundary

Ordinary `open`, `read`, `write`, `stat`, directory, and descriptor operations
still use the kernel VFS and kernel tmpfs for `/tmp`. Redirecting those calls to
a userspace server safely requires additional kernel machinery:

- a kernel-to-userspace request path that can block without cooperative polling;
- cancellation when callers exit or receive signals;
- restart-aware request and descriptor identities;
- protection against priority inversion and server deadlock;
- deterministic handling of in-flight operations when the service fails;
- a mount/service registration contract.

Until those pieces exist, keeping kernel `/tmp` available avoids weakening the
working shell and smoke suite while the userspace service protocol matures.
