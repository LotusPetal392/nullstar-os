# Identity and access-control design

## Status

This document describes a future identity, authentication, and discretionary
access-control model for NullStar OS. The current system does not yet have user accounts,
login sessions, per-process credentials, or UID/GID enforcement.

Identity policy complements the capability model rather than replacing it. User and
group membership may authorize a broker or filesystem operation, but cannot create
authority that the deciding service does not possess and cannot forge a kernel
capability.

The process and session lifecycle used here is specified in
[Service, session, and application lifecycle](design/service-and-session-lifecycle.md).

## Goals

The identity system should:

- support root, an administrative group, regular users, and service identities;
- attach immutable effective credentials to each process;
- provide bounded supplementary groups;
- enforce file ownership and Unix-style mode bits consistently across filesystem
  services;
- support graphical, terminal, and remote login sessions;
- use explicit brokers for elevation instead of ambient privilege;
- preserve capability attenuation and service isolation;
- keep account formats versioned, bounded, deterministic, and recoverable;
- emit structured security events without logging secrets.

NullStar does not initially require PAM, NSS, `/etc/passwd`, `sudo`, or set-user-ID
binary compatibility. Compatibility adapters may be added later without making those
interfaces the native trust boundary.

## Identity namespace

The initial numeric namespace should reserve:

| ID | Kind | Name | Purpose |
| ---: | --- | --- | --- |
| 0 | UID | `root` | System administrator identity |
| 0 | GID | `root` | Root account's private primary group |
| 1 | GID | `admin` | Accounts eligible to request brokered administration |
| 1–999 | UID/GID | reserved | Core services and future system identities |
| 1000+ | UID/GID | allocated | Regular users and private primary groups |

UID and GID are separate namespaces. The precise upper bound and sentinel values must be
fixed in the shared ABI before implementation. IDs must not be silently reused while
files, audit records, grants, or durable policy still depend on their meaning.

A regular account should normally have a private primary group with the same name and a
bounded list of direct supplementary memberships. Nested groups should be omitted
initially.

## Account roles

### Root

UID 0 is the distinguished administrative identity. Policy may allow it to bypass
ordinary discretionary owner/group/mode checks, change ownership, manage accounts, or
request privileged broker operations.

Root is not capability-omnipotent. A root process cannot manufacture endpoints, map
arbitrary MMIO, open raw hardware without an authorized provider or broker, or grant
rights absent from its own capability table. A service also cannot act for root unless
that service holds the necessary authority.

The root home should remain `/System/var/root`, restricted to root rather than placed in
`/Users`.

### Admin group

Membership in GID 1 means an account is eligible to request administrative elevation.
It does not grant every process belonging to that user additional rights and does not
imply UID 0.

An authorization broker should verify membership, perform any configured authentication
or presence check, evaluate the requested semantic operation, and delegate only the
one-shot action, ticket, or narrow capability required.

### Regular users

Regular users receive unique UIDs, private primary groups, homes under `/Users/<name>`,
and only the capabilities required for their session and applications. Sharing uses
explicit group membership and filesystem policy.

The accepted managed-data layout is:

```text
/Users/<name>/Profile/
├── config/
├── cache/
├── state/
├── data/
├── logs/
└── runtime/
```

`Profile/runtime` is the logical location for per-login ephemeral state. Its contents
must be private, created and owned by the session manager, and removed or invalidated
when the session ends. Applications receive runtime directory capabilities or projected
paths from the runtime rather than constructing them blindly. See
[Userspace architecture](design/userspace-architecture.md).

## Process credentials

Every process should eventually have kernel-authenticated credentials containing at
least:

- real and effective UID/GID;
- a bounded, duplicate-free supplementary-group set;
- login-session identity;
- service, application, or component identity where applicable;
- authentication or elevation context where applicable.

Credentials must not be writable through ordinary process memory. `fork` inherits a
snapshot. `exec` preserves credentials unless an authorized process manager supplies a
checked transition. Initial implementations should omit set-user-ID and set-group-ID
execution.

Only the root bootstrap supervisor, system service manager, session or application
manager acting within its delegated subtree, or a narrowly authorized identity broker
should create a process under another identity. Such transitions must also filter
handles, descriptors, namespace routes, and job policy; changing UID without reducing
inherited authority is not a security boundary.

A process may query its own identity. Inspecting or changing another process requires an
explicit process-management capability and policy approval.

## Authentication and sessions

Authentication proves an account claim; it does not grant arbitrary kernel authority.
The accepted login flow is:

```text
trusted login UI
    -> authentication service verifies credentials
    -> system service manager creates a login-session job
    -> policy assigns immutable user, seat, and session identity
    -> session manager receives a bounded session capability set
    -> session manager launches the compositor, shell, services, and applications
```

Authentication must go through a dedicated service rather than exposing password
verifiers to applications or the login UI. Passwords must never be stored in plaintext.
Account records should name a versioned password-hash scheme and bounded work parameters
so hashes can be upgraded after successful login. Passwords, hashes, recovery data, and
tokens must not appear in logs or process arguments.

The first implementation can support local password authentication. Public keys,
hardware-backed authentication, recovery credentials, lockout policy, and remote login
can be added later through versioned mechanisms.

A login session owns its seat or terminal, session job, session-scoped services, runtime
namespace, and delegated capabilities. Logout requests orderly shutdown and then
terminates the complete session-job subtree. Broker-owned session grants and leases are
revoked or invalidated where supported.

The lock screen is a trusted session component backed by compositor policy. Ordinary
applications cannot draw above it, capture it, synthesize unlock input, or inspect its
authentication traffic.

## Account and group storage

Packaged defaults and machine-local identity configuration should live under:

```text
/System/config/identity/users/
/System/config/identity/groups/
/System/config/identity/credentials/
/System/var/lib/identity/
```

Credential verifiers must be physically and logically separated from public account
metadata. Mutable allocation state, transaction generations, and recovery data belong
under `/System/var/lib/identity`.

The native format should be versioned and bounded. Parsers must reject duplicate names
or IDs, duplicate members, invalid or path-unsafe names, oversized records, unknown
mandatory fields, and references outside the configured namespace. Updates should be
atomic and preserve either the old complete database or the new complete database after
interruption.

Public lookup exposes only fields needed for display and ownership resolution. Password
hashes, recovery material, and broker policy are never returned through ordinary
enumeration.

## Filesystem ownership and permissions

Filesystem metadata should carry owner UID, owner GID, and user/group/other mode bits.
Filesystem services evaluate requests using an authenticated request context, never UID
or GID values supplied as ordinary client payload fields.

An ordinary check should:

1. apply mount-level, immutable, read-only, and directory-capability restrictions;
2. select owner, group, or other mode bits;
3. apply a narrowly specified UID 0 override if enabled;
4. require the filesystem service and VFS route to possess the capabilities needed to
   complete the operation.

Directory execute controls traversal; read controls enumeration; write plus execute
controls entry creation, removal, and renaming. Creation normally assigns the caller's
effective UID and GID. `umask` may remove requested bits but never add them.

NullFS should persist native UID/GID/mode metadata. Filesystems lacking equivalent
metadata need explicit synthesized ownership or mount policy rather than silently making
everything universally writable.

Identity and mode checks do not let a sandboxed application escape a directory
capability. Both the capability root and discretionary access check must permit the
operation.

## Capabilities and identity policy

Capabilities remain authoritative for kernel objects and privileged service endpoints.
Identity answers whether a broker may use authority it already possesses for a caller.
It does not determine which kernel objects a process can name.

Rules include:

- identity may deny brokered use of reachable authority;
- a broker delegates only rights it possesses, at equal or reduced strength;
- root and admin status cannot forge or amplify capabilities;
- transferring a capability does not change credentials;
- credential transitions must filter capabilities inappropriate to the new identity;
- paths, names, PIDs, UIDs, GIDs, application IDs, and service names are not
  capabilities.

## Brokered elevation

NullStar should initially avoid set-user-ID binaries and ambient “become root” state.
Administrative tools send structured, versioned requests to an authorization broker.
The broker should:

1. authenticate the caller and session context;
2. resolve immutable credentials and verified application or service identity itself;
3. verify root identity or admin eligibility;
4. apply operation-specific policy and optional reauthentication;
5. perform the operation or return a narrow, expiring, preferably single-use ticket;
6. emit a structured audit decision with a stable request identity.

Requests name semantic operations, not arbitrary shell command strings. A ticket should
be bound to caller, target service, operation, normalized parameters or digest, user,
session, expiration, and nonce.

If an elevated process must be launched, the authorized process manager constructs a
sanitized environment, explicit argument vector, filtered descriptors and handles,
restricted namespace, non-relaxable job policy, and auditable credential transition.

## Service identities

Long-running services should not all run as root. Stable core services may use reserved
IDs, while installed services use managed identities under a later allocation policy.
Service definitions declare intended identity and requested capabilities; the system
service manager resolves both against trusted package, signing, and machine policy.

Restarting a service preserves configured credentials but issues a fresh generation and
fresh endpoints. Services rely on broker- or kernel-authenticated peer context, not
peer-supplied PIDs or usernames.

Session services receive user and login-session identity plus only the capabilities
routed into that session. They cannot control unrelated users merely because the same
executable implements several instances.

## Application identity and permissions

A stable signed application identity is separate from UID. Several applications may run
as one user while receiving different capabilities, private storage, network policy, and
permission grants.

Permission records bind to application identity, user identity, resource identity,
rights, scope, and current policy. They authorize a trusted provider to recreate a
capability; they are not live handles and cannot be claimed by supplying another
application's identifier.

See [Capability-based application sandboxing](design/application-sandboxing.md) for
application signing lineage, profiles, portals, leases, and administrative tickets.

## Device and privileged-service policy

The future [device filesystem](devfs.md) exposes discoverable ownership and mode metadata,
but opening a node asks the provider to create an attenuated generation-scoped session.
Listing a device or passing a mode check does not create raw-device, MMIO, IRQ, or DMA
authority.

The same principle applies to network policy, service control, package installation,
namespace changes, logs, secrets, and other privileged services: identity authorizes a
brokered request; capabilities and provider authority determine what can actually be
done.

## Logging and auditing

Identity, authentication, and authorization components emit structured records to the
accepted logging and audit architecture rather than appending directly to an
application-managed `/System/var/log` file.

Security-relevant events include:

- successful and failed authentication;
- account, group, and credential changes;
- session creation, lock, unlock, and termination;
- elevation requests and decisions;
- privileged ownership or permission changes;
- sensitive device, grant, or service opens;
- service and application launches under configured identities.

Records should include stable event and request IDs, monotonic time and trusted wall time
when available, caller, application or service, session, target, operation, decision,
and policy generation. They must omit passwords, hashes, tokens, document contents, raw
capability contents, and unrelated user data.

The centralized [logging design](design/logging.md) owns retention, access control,
rotation, privacy classification, and storage policy. Audit data may use a stronger
retention or tamper-evidence class, but callers use the service API rather than assuming
a physical journal path. Audit-storage exhaustion requires an explicit operation-
specific fail-open or fail-closed policy.

## Recommended milestones

1. Define bounded UID/GID and credential ABI types and expose read-only credentials for
   the current root session.
2. Add native ownership and mode metadata plus shared access-check tests to tmpfs and
   NullFS.
3. Implement read-only account and group lookup.
4. Add service identities and checked credential, handle, and namespace filtering in the
   system lifecycle manager.
5. Introduce a dedicated authentication service, trusted login UI, login-session jobs,
   private homes, terminal ownership, and session-managed `Profile/runtime`.
6. Add supplementary groups and shared-file workflows.
7. Implement semantic brokered elevation with single-use authorization and structured
   auditing.
8. Integrate identity policy with application permissions, devfs, network, packages, and
   other privileged brokers.
9. Harden updates, hash upgrades, recovery, rate limiting, log access, key rotation, and
   adversarial denial-path tests.

Multiuser mode should not be described as secure until credentials, filesystem checks,
capability filtering, session isolation, account storage, application policy,
authorization brokers, and audit behavior have been validated together.
