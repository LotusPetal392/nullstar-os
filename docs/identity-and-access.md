# Identity and access-control design

## Status

This document describes a future identity, authentication, and discretionary
access-control model for NullStar OS. The current system does not yet have user
accounts, login sessions, per-process credentials, or UID/GID enforcement.

Identity policy will complement the capability model, not replace it. User and
group membership may authorize a broker or filesystem operation, but cannot
create authority that the deciding service does not possess and cannot forge a
kernel capability.

## Goals

The identity system should:

- support a root account, an administrative group, regular users, and service
  identities;
- attach immutable effective credentials to each process;
- provide bounded supplementary-group membership;
- enforce file ownership and Unix-style permission bits consistently across
  filesystem services;
- support authenticated graphical, terminal, and remote sessions when those
  environments exist;
- use explicit brokers for elevation instead of ambient privilege;
- preserve capability attenuation and service isolation;
- keep account formats versioned, bounded, deterministic, and recoverable;
- make security-relevant decisions auditable without logging secrets.

NullStar does not initially need compatibility with Linux PAM, NSS, `/etc/passwd`,
`sudo`, or set-user-ID binaries. Compatibility adapters may be added later without
making those interfaces the native trust boundary.

## Identity namespace

The initial numeric namespace should reserve:

| ID | Kind | Name | Purpose |
| ---: | --- | --- | --- |
| 0 | UID | `root` | System administrator identity |
| 0 | GID | `root` | Root account's private primary group |
| 1 | GID | `admin` | Accounts eligible to request brokered administration |
| 1–999 | UID/GID | reserved | Core services and future system identities |
| 1000+ | UID/GID | allocated | Regular users and their private primary groups |

UID and GID are separate namespaces, so UID 0 and GID 0 may both be named
`root`. The precise upper bound and invalid/sentinel values must be fixed in the
shared ABI before implementation. Allocation should never silently reuse an ID
while durable files or audit records still depend on its meaning.

A regular account should normally have a private primary group with the same
name and a bounded list of supplementary groups. Group nesting should be omitted
initially: direct membership is simpler to validate, cache, and audit.

## Account roles

### Root

The `root` account is the system's distinguished administrative identity. Policy
may allow UID 0 to bypass ordinary discretionary owner/group/mode checks, change
file ownership, manage accounts, or ask privileged brokers for administrative
operations.

UID 0 is not capability-omnipotent. A root process cannot manufacture an endpoint,
open raw hardware without an authorized broker/provider path, map arbitrary MMIO,
or grant rights absent from its own capability table. A service also cannot
perform an operation for root unless that service itself holds the required
authority.

The root account's home should be `/System/var/root`, with access restricted to
root. It should not be placed in `/Users`, which is intended for regular user
homes.

### Admin group

Membership in GID 1 (`admin`) means that an account is eligible to request
administrative elevation. It does not add rights to every process belonging to
that user and does not imply UID 0.

An authorization broker should verify admin membership, require the configured
authentication or presence check, evaluate the requested operation, and delegate
only the capability or one-shot operation needed. Broad reusable authority
should not be returned when a narrower request can be completed by the broker.

### Regular users

Regular users receive unique UIDs, private primary groups, homes under
`/Users/<name>`, and only the capabilities needed for their login session and
applications. Users may share files through explicit group membership and mode
bits. A future per-user runtime directory should be private, session-managed,
and removed or invalidated when its owning session ends.

## Process credentials

Every process should have kernel-authenticated credentials containing at least:

- real UID and GID, identifying the account that created the process;
- effective UID and GID, used for ordinary discretionary access checks;
- a bounded, duplicate-free sorted set of supplementary GIDs;
- a login-session identifier;
- an authentication/elevation context identifier where applicable.

Credentials must not be writable through ordinary process memory. `fork` should
inherit an exact credential snapshot. `exec` should preserve credentials unless
an authorized process manager explicitly supplies a checked transition. Initial
implementations should omit set-user-ID and set-group-ID executable bits so an
untrusted executable cannot trigger an implicit credential change.

Only PID 1, a future session manager, or a narrowly authorized identity broker
should be able to create a process under another identity. That transition must
also filter inherited descriptors and capabilities: changing UID without
reducing inherited authority is not a security boundary.

A process may query its own credentials and session identity. Inspecting another
process's credentials or changing process ownership should require an explicit
process-management capability and policy approval.

## Authentication and sessions

Authentication proves an account claim; it does not itself grant arbitrary
kernel authority. A future login flow should be:

```text
login client
    -> authentication service verifies account credentials
    -> session manager creates a session identity
    -> policy selects the session's initial capabilities and mounts
    -> process manager launches the user environment with checked credentials
```

Authentication requests should pass through a dedicated service rather than
allowing every application to read password verifiers. Passwords must never be
stored in plaintext. The account record should identify a versioned password
hash scheme and bounded work parameters so hashes can be upgraded after a
successful login. Secret input and verifier material must not appear in normal
logs or process arguments.

The first implementation can support local password authentication only. Public
keys, recovery credentials, hardware-backed authentication, lockout policy, and
remote login can be added through versioned mechanisms later. Authentication
failures and backoff must be bounded so malformed requests cannot grow state
without limit.

A login session should own its terminal seat, initial process group, user-facing
service endpoints, and any per-session capabilities. Logging out must revoke or
close broker-owned session grants where supported and terminate or reparent
remaining session processes under an explicit policy.

## Account and group database

Packaged defaults and machine-local identity configuration should live beneath
the existing system namespace:

```text
/System/config/identity/users/
/System/config/identity/groups/
/System/config/identity/credentials/
/System/var/lib/identity/
/System/var/log/
```

User and group records should use separate files or records with stable numeric
IDs and canonical names. Credential verifiers should be physically and
logically separated from public account metadata. Mutable allocation state,
transaction generations, and recovery data belong under
`/System/var/lib/identity`; authentication and authorization events belong in a
protected audit log under `/System/var/log`.

The native format should be explicitly versioned and bounded. Parsers must reject
unknown mandatory fields, duplicate names or IDs, duplicate group members,
invalid UTF-8 or path-unsafe names where names become path components, oversized
records, and references outside the configured identity namespace. Updates
should be atomic and preserve either the previous complete database or the new
complete database after interruption.

Public account lookup should expose only the fields needed for display and
ownership resolution. Password hashes, recovery material, and broker policy must
not be returned through ordinary account enumeration.

## Filesystem ownership and permissions

Filesystem metadata should carry an owner UID, owner GID, and mode bits for user,
group, and other read/write/execute permissions. Filesystem services should
evaluate requests using credentials supplied through a kernel-authenticated or
otherwise unforgeable request context, never UID/GID values supplied as ordinary
client payload fields.

For an ordinary access check:

1. Apply any mount-level and immutable/read-only restrictions.
2. Select owner permissions if the effective UID matches the node owner.
3. Otherwise select group permissions if the effective GID or any supplementary
   GID matches the node group.
4. Otherwise select other permissions.
5. Apply narrowly specified UID 0 override policy, if enabled for the operation.
6. Require the filesystem service and VFS path to possess the capabilities needed
   to complete the operation.

Directory execute permission controls traversal; directory read permission
controls enumeration; and directory write plus execute permissions control entry
creation, removal, and renaming. File execute semantics can be added when the
loader begins enforcing them. Symlink checks must be defined at the operation and
parent-directory level rather than treating symlink mode bits as authority.

Creation should assign the caller's effective UID and normally its effective
GID. A set-group-ID directory policy may later inherit the parent GID, but set-ID
execution remains out of scope. `umask` may reduce requested creation bits; it
must never add permissions. Ownership changes, privileged mode changes, and
cross-user operations should go through checked filesystem policy.

Each filesystem adapter must implement the same externally visible policy.
NullFS should persist native UID/GID/mode metadata. Filesystems without equivalent
metadata need an explicit mount policy or synthesized ownership; they must not
silently treat every file as universally writable.

## Capabilities and identity policy

Capabilities remain the authoritative mechanism for access to kernel objects and
privileged service endpoints. Identity answers questions such as “may this
broker delegate this operation to this caller?” It does not answer “what kernel
objects can this process name?”

The governing rules are:

- identity checks may deny use of authority already reachable through a broker;
- a broker may delegate only rights it possesses and only at equal or reduced
  strength;
- UID 0 and admin membership cannot forge, amplify, or bypass capability rights;
- transferring a capability does not change the recipient's UID or group
  membership;
- changing process credentials must not preserve capabilities that the new
  identity is not allowed to inherit;
- path visibility, object names, and numeric IDs are not capabilities.

This separation permits familiar ownership and group policy without turning a
compromised root-identity process into an automatic kernel-object oracle.

## Brokered elevation

NullStar should initially avoid set-user-ID binaries and ambient “become root”
state. Administrative tools should send structured, versioned requests to a
privileged authorization broker. The broker should:

1. authenticate the caller's process and session context;
2. resolve immutable caller credentials itself;
3. verify root identity or admin-group eligibility;
4. apply operation-specific policy and optional reauthentication;
5. perform the operation directly or return a narrowly attenuated capability;
6. record the decision and stable request identity in the audit log.

Requests should name semantic operations rather than arbitrary command strings.
If launching an elevated program becomes necessary, the broker and process
manager must construct a sanitized environment, explicit argument vector,
filtered descriptor table, reduced capability set, and auditable credential
transition.

## Service identities

Long-running services should not all run as root. IDs below the regular-user
range may be assigned to stable core services, while dynamically installed
services can use managed identities under a later allocation policy. A service
unit should declare its intended identity and requested capabilities; PID 1
resolves both against trusted policy.

Service identity isolates filesystem data and supports audit attribution, but a
service's capability grant remains its actual operational authority. Restarting
a service should preserve its configured UID/GID while issuing fresh
generation-scoped endpoints and capabilities. Services must not infer trust from
a peer-provided process ID or textual username when the kernel can provide an
authenticated request context.

## Device-filesystem policy

The future [device filesystem](devfs.md) should expose owner UID, owner GID, and
mode metadata for discoverability and familiar access checks. Opening a node then
asks the current provider to create an attenuated, generation-scoped session.

Typical defaults should make harmless pseudo-devices broadly accessible, bind
terminals to their owning login session and foreground process group, restrict
input devices, and deny regular users direct raw-disk access. Admin membership
may authorize a brokered raw-device operation, but merely listing `/dev` or
passing a mode-bit check cannot produce a device-provider capability.

## Auditing

Security-relevant services should emit bounded structured audit records for:

- successful and failed authentication;
- account, group, and credential changes;
- session creation and termination;
- brokered elevation requests and decisions;
- privileged ownership or permission changes;
- sensitive device opens and provider-policy denials;
- service launches under configured identities.

Records should include monotonic event identity, timestamp when a trusted clock
exists, caller UID and session, target identity or object, operation, decision,
and policy generation. Logs must omit passwords, hashes, raw tokens, capability
contents, and unrelated user data. Audit storage exhaustion needs an explicit
fail-open or fail-closed policy per operation rather than silent record loss.

## Recommended milestones

1. Define bounded UID/GID/process-credential ABI types and expose read-only
   credentials for the current single root session.
2. Add native UID/GID/mode metadata and shared access-check tests to tmpfs and
   NullFS, while preserving current boot behavior.
3. Implement a read-only account/group database and deterministic identity lookup
   service.
4. Add service identities and PID 1 credential/capability filtering at launch.
5. Introduce local authentication, login sessions, private regular-user homes,
   and terminal ownership.
6. Add supplementary groups and filesystem group-sharing workflows.
7. Implement the authorization broker and admin-group elevation with auditing.
8. Integrate identity policy with devfs and other privileged service brokers.
9. Harden database updates, password-hash upgrades, recovery, rate limiting, and
   adversarial tests before treating the system as multiuser-secure.

Each milestone should include denial-path tests. Multiuser mode should not be
described as secure until process credentials, filesystem checks, capability
filtering, terminal/session isolation, account storage, and privileged brokers
have all been validated together.
