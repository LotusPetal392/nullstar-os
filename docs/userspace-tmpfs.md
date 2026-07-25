# Userspace tmpfs service

Phase 3 introduced NullStar OS's first real stateful userspace service. The
service is now also registered with the kernel by PID 1, so ordinary `/tmp/<name>`
file syscalls are routed through `/tmpfs-service` rather than the kernel-resident
tmpfs implementation.

## Process and capability layout

PID 1 creates two endpoint objects before starting `/tmpfs-service`:

- a readiness endpoint, granted to the service with `SEND` only;
- a request endpoint, granted to the service with `RECEIVE` only.

The service receives those capabilities at deterministic handles 1 and 2. It
validates the object types and exact rights before announcing readiness. After
the readiness handshake, PID 1 performs a mount handshake, learns the service
generation, and registers the request endpoint with the kernel.

A userspace client receives only `SEND` authority to the request endpoint. For
each direct protocol request it creates a fresh reply endpoint and transfers a
`SEND`-only copy to the service. The client retains `RECEIVE`, so replies cannot
be consumed by the service or another request sender. Kernel-proxied ordinary
file syscalls use the same request/reply shape: the kernel creates the private
reply endpoint, transfers a send-only reply capability to the service, blocks the
calling process, and completes the saved syscall frame when the reply arrives.

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
- open/create/truncate;
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

## Syscall routing boundary

After PID 1 registers the service, ordinary `open`, `read`, `write`, `stat`,
`fstat`, `read_directory`, descriptor duplication, redirection, and seek offset
bookkeeping for `/tmp/<name>` operate on proxy-backed file descriptions. The
kernel still owns descriptor tables and scheduling, but file contents and file
sizes come from `/tmpfs-service`.

The current proxy intentionally keeps several boundaries small:

- names are single `/tmp/<name>` components, with no subdirectories;
- payloads are limited to 128 bytes per protocol request/reply, so larger reads
  and writes complete as ordinary short file syscalls;
- directory listings are synthesized from the service's newline-separated file
  list, so entry sizes are reported as zero in directory records;
- service restart generation is validated by the protocol, but in-flight syscall
  replay across a service restart remains future work.

The kernel tmpfs is still mounted for kernel-internal compatibility and smoke
fixtures, but userspace programs using normal file APIs no longer write their
`/tmp` data there.
