# Userspace service supervision

NullStar OS Phase 2 turns PID 1 into a general userspace service supervisor while
leaving existing drivers, filesystems, terminals, and other established services
inside the kernel. This recovery layer is required before later phases move
failure-prone subsystems into isolated processes.

## Implemented contract

A supervised service has a static specification containing:

- a diagnostic name;
- an executable command;
- an exact readiness message;
- a deterministic child capability handle;
- a bounded restart limit;
- a cooperative restart-backoff interval.

`ServiceRuntime` records the current process identifier, restart count, and one
of these states:

```text
Stopped -> Starting -> Running
                    \-> Backoff -> Starting
                    \-> Failed
```

Stopped and continued child-status events do not consume the restart budget.
Final exit or signal statuses do. Once the configured budget is exhausted, PID
1 treats the service as failed instead of entering an unbounded restart loop.

## Capability bootstrap and readiness

PID 1 owns a message endpoint. After spawning a service, it grants the child a
send-only capability at the handle specified by the service manifest. The child
cannot receive from the endpoint, duplicate it, or transfer it onward.

The service waits for that handle, validates its object type and rights, and
sends its exact readiness record. PID 1 validates:

- the sender process identifier;
- the complete message bytes;
- the absence of an unexpected transferred capability.

Dependent work does not start until this validation succeeds. The current boot
manifest therefore establishes this ordering:

```text
PID 1
  -> probe service started
  -> send-only readiness endpoint granted
  -> probe service ready
  -> interactive shell started
```

There is no global service namespace yet. Direct-child grants remain the only
bootstrap mechanism, and readiness endpoints are private to PID 1 and its
children.

## Failure injection

The bundled `/service-probe` deliberately exits with status 75 on its first
start after creating `/tmp/service-probe.started`. PID 1 observes the final
status, consumes one restart allowance, performs bounded cooperative backoff,
and starts it again. The second instance reports readiness and remains alive.

This gives every normal and smoke-test boot an end-to-end recovery assertion:

- the child can fail before reporting ready;
- PID 1 does not wait forever on the readiness endpoint;
- the restart budget is updated;
- a replacement child receives a fresh rights-reduced capability;
- shell startup remains dependency ordered.

## Concurrent supervision

After startup, PID 1 polls both the service child and the foreground shell with
nonblocking child waits. It continues to preserve the existing shell behavior:
stopped shells are restored to the foreground, continued events are ignored,
and final shell statuses cause a fresh shell to launch.

A final service status triggers the same bounded restart-and-readiness sequence
used during boot. The shell remains running while the independent service is
recovered.

## Deliberate limitations

Phase 2 does not yet provide:

- a parsed on-disk service manifest;
- multiple service dependency graphs or cycle detection;
- watchdog deadlines or heartbeat timeouts;
- kernel-blocked wait sets;
- exponential or time-based backoff;
- capability revocation after a service is replaced;
- persistent restart accounting;
- a public service-discovery namespace;
- degraded-mode policies for optional services;
- userspace tmpfs, filesystem, driver, networking, display, or audio services.

The current probe is an architectural fixture, not a useful operating-system
service. Its purpose is to prove the lifecycle and recovery contract before the
first kernel subsystem is migrated.

## Next step

The next migration phase should move the bounded `/tmp` implementation into a
userspace tmpfs server. That work should reuse the readiness, restart, and
capability-bootstrap contract established here, while adding request/reply file
operations and explicit behavior for clients connected to a restarting server.
