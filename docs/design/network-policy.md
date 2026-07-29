# Network-policy and firewall direction

## Status

An identity-aware network policy service that attributes activity to applications,
packages, services, users, and process generations is **accepted direction**. Optional
malware and tracker-list filtering is accepted direction. Exact rule syntax, default
prompt policy, list providers, and packet-filter implementation remain
**tentative design**.

## Goals

NullStar networking should make these questions answerable without PID-reuse races or
packet-capture guesswork:

- Which application or service requested a connection?
- Which package and executable generation launched it?
- Which user session and sandbox own it?
- Which hostname, address, protocol, port, and interface were involved?
- Was the operation allowed, blocked, redirected, rate-limited, or interrupted?
- Which rule and policy source produced that decision?

Network attribution should be attached when a socket or network endpoint is requested,
not inferred after traffic appears.

## Service architecture

The intended split is:

```text
applications and services
        |
        v
socket and resolver APIs
        |
        v
network stack <-> network policy engine
        |                 |
        |                 +-> rule database and immutable policy snapshots
        |                 +-> connection history
        |                 +-> block-list updater
        v
network-device protocol
        |
        v
NIC driver
```

Possible canonical service identities include:

```text
network.stack
network.policy
network.resolver
network.history
network.blocklists
```

These may begin in fewer processes. The enforcement path must retain the last valid
policy snapshot if management, UI, history, or update components restart.

## Stable connection identity

A connection record should capture immutable launch identity:

- process ID and process generation;
- application and package ID;
- executable identity or verified content hash where available;
- service ID and service generation, when applicable;
- user, login session, sandbox, and job identity;
- the delegated network capability or profile used to authorize the request.

The record also tracks destination and transport metadata, byte counts, timestamps,
state changes, and the rule that made the decision.

Ordinary applications may inspect their own connections. Cross-application history and
system-service details require explicit authority.

## Enforcement points

Policy should be enforced at several complementary boundaries:

1. **Capability and socket creation**: decide whether the caller may request this class
   of network operation.
2. **Name resolution**: apply domain policy before an address is returned.
3. **Connect and listen**: evaluate the concrete remote or local endpoint.
4. **Packet layer**: enforce raw, forwarded, tunnel, VPN, and exceptional traffic that
   does not pass through ordinary socket calls.

Normal applications should not receive raw NIC access. They receive socket or higher
network capabilities from the stack service.

## Network capability classes

The native permission model should distinguish at least:

```text
internet-client
local-network-client
loopback-listener
local-network-listener
public-listener
multicast-discovery
raw-network
vpn-provider
dns-provider
```

Local-network discovery and public listening are materially different from ordinary
outbound HTTPS and should not be hidden inside one broad network permission.

Package manifests may declare intended use, but declarations are requests and
explanations, not unconditional grants. The policy UI should be able to compare
observed destinations with declared purposes.

## Policy rules

Rules may match:

- application, package, service, publisher, user, session, or executable identity;
- hostname or domain suffix;
- IP address or prefix;
- protocol and destination or listening port;
- interface, local network, VPN, or internet scope;
- network profile and time window;
- verified block-list category.

Actions should include:

```text
allow
block
ask
allow-once
allow-until-exit
allow-for-session
redirect
rate-limit
log-only
```

Policy layers should have deterministic precedence and an explanation trace. A likely
order is system safety, administrator policy, user policy, application-specific rules,
and temporary session decisions. Mandatory rules must be distinguishable from defaults
that a user may override.

Interactive prompts should be optional and reserved for meaningful decisions. A
usable default should avoid asking about every ordinary connection while still making
unusual authority such as public listening, raw traffic, or local discovery visible.

## Domain attribution and DNS

The resolver service should preserve caller identity and correlate:

1. the application's hostname request;
2. returned addresses and cache lifetime;
3. later connection attempts;
4. observable TLS or protocol names where policy permits;
5. timing and interface information.

The UI must distinguish a domain inferred from resolver correlation from a destination
proven by another protocol field.

A native resolver can provide caching, encrypted transport, DNSSEC validation, and
per-application policy while retaining attribution. Applications that use an
independent encrypted DNS tunnel may reduce domain visibility and should require an
appropriate capability or be shown as opaque resolver traffic.

## Malware and tracker lists

Third-party lists are optional policy inputs, not trusted kernel code. Each installed
list should record:

- stable source and license information;
- category, version, and update time;
- signature or integrity metadata;
- enabled policy and user exceptions;
- entry count and compiled database generation.

Updates should be downloaded by a low-privilege updater, parsed with bounded resource
use, compiled into a new immutable database, verified, and atomically activated. The
previous valid database remains available for rollback.

A block explanation should identify the exact list and matching entry. Lists must not
be silently merged into an opaque result that prevents false-positive diagnosis.

Domain matching should use compiled exact-match sets and reversed-label tries. Address
ranges should use prefix-aware structures. Connection decisions must not parse large
text lists synchronously.

## User interfaces

A graphical firewall should provide:

- live connections and listeners;
- per-application history and bandwidth;
- requested domains and resolved addresses;
- allowed, blocked, interrupted, and redirected operations;
- rule and block-list explanations;
- application profiles and exceptions;
- private-history and retention controls.

The native CLI should be `netctl`:

```text
netctl status
netctl apps
netctl connections
netctl history
netctl rules
netctl blocklists
netctl explain <connection-id>
netctl trace --app <application-id>
```

`sv` manages the underlying services, while `netctl` manages network policy and views
network activity.

## Inbound and local-network policy

Listening records should show the owner, address, port, interfaces, and exposure class.
A service asking to bind all interfaces should not automatically become internet
reachable if policy grants only loopback or local-network listening.

Multicast, broadcast, printer discovery, casting, and similar local-network behavior
should pass through a distinct permission and policy path.

## VPN and routing awareness

Policy and history should record both the application's logical destination and the
physical tunnel endpoint. Otherwise all activity appears to contact only the VPN
server.

Future profiles may include:

```text
Default
No Internet
Local Network Only
Privacy Enhanced
VPN Required
Proxy Routed
Tor Routed
```

A VPN or proxy provider receives narrowly scoped tunnel and routing capabilities. It
does not gain unrelated process, filesystem, or policy authority.

## Logging and privacy

Connection history is sensitive user data. The design must support:

- per-user history and access control;
- configurable retention or no persistent history;
- private-session exclusions and field redaction;
- separation between operational logs and detailed browsing history;
- encrypted-at-rest storage when the user profile is encrypted;
- audit records for policy changes without logging secret payloads.

Network policy should record metadata needed for decisions and diagnostics, not packet
contents by default.

## Failure policy

The enforcing stack should retain the last validated immutable policy snapshot while
policy-management components restart. New raw-network and public-listener grants
should fail closed when policy is unavailable.

Ordinary outbound behavior may use a configured fail-open or fail-closed profile, but
the choice must be explicit. Existing connections should not be terminated merely
because the history or UI process restarts.

Malformed rules, list databases, and policy replies fail closed for the affected rule
source and produce structured diagnostics. They must not crash the network stack.

## Recommended implementation stages

1. Route socket creation through the network service and attach stable caller identity.
2. Record destination address, port, protocol, state, and byte counts; expose
   `netctl connections`.
3. Add per-application allow and block rules for connect and listen operations.
4. Add the native resolver, hostname correlation, domain rules, and history.
5. Add graphical policy controls, local-network permissions, listener exposure, and
   user-scoped profiles.
6. Add verified malware and tracker lists, exceptions, explanations, and rollback.
7. Add VPN awareness, per-application routing, bandwidth policy, and advanced profiles.
8. Add a carefully bounded packet-layer path for raw, forwarded, and tunnel traffic.

## Open questions

- Default outbound and prompt policy for newly installed desktop applications.
- Exact division between the socket service, network stack, and policy engine.
- Resolver behavior for applications that deliberately provide their own encrypted DNS.
- Connection-history retention defaults and redaction rules.
- Initial list formats, trust policy, and update-signing model.
- The fail-open/fail-closed defaults for ordinary desktop, recovery, and locked-down
  profiles.
