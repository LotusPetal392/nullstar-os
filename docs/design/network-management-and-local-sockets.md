# Network management, diagnostics, and local sockets

## Status

A unified, scriptable network-management service and command-line interface are
**accepted direction**. Manual route and neighbor-table administration, complete
command-line Wi-Fi management, transactional firewall updates, native IPC channels,
and later Unix-domain-socket compatibility are also accepted direction.

Exact command spelling, shell language extensions, firewall policy syntax, and the
internal sharing of primitives between native IPC and local sockets remain
**tentative design**.

This document complements [Network policy and firewall](network-policy.md), which
covers application-aware authorization, attribution, connection history, and policy
enforcement.

## Goals

NullStar networking should be fully operable from a text console for installation,
recovery, development, remote administration, automated testing, and systems without
a graphical session. The graphical network settings application and command-line
tools must use the same service APIs and policy engine rather than implementing
separate configuration paths.

The design should:

- provide coherent native commands instead of accumulating overlapping historical
  utilities;
- preserve familiar standalone diagnostic tools where they are useful in pipelines;
- support human-readable and stable structured output;
- make configuration operations idempotent and transactional;
- distinguish observation authority from configuration authority;
- keep native service IPC independent from filesystem pathnames;
- provide a practical POSIX compatibility path without making Unix-domain sockets the
  native service architecture.

## Service architecture

The intended management path is:

```text
network settings UI     netctl and diagnostics     installers and scripts
         \                     |                         /
          +--------------------+------------------------+
                               |
                               v
                 network management service
                 |       |       |       |
                 |       |       |       +-- profile and credential services
                 |       |       +---------- resolver and route management
                 |       +------------------ network policy and firewall
                 +-------------------------- network stack and device services
```

Administrative commands request changes from the service. They do not manipulate NIC
registers, routing structures, Wi-Fi firmware, or packet-filter state directly.

## Command-line suite

The primary management command should be `netctl`. Frequently used diagnostics remain
standalone programs so they work naturally in interactive sessions and scripts.

An intended base suite is:

```text
netctl        interfaces, addresses, routes, DNS, Wi-Fi, profiles, and status
netstat       sockets, listeners, interfaces, routes, and protocol statistics
ping          ICMP reachability and latency
trace         route tracing
lookup        DNS and service lookup
netcat        TCP, UDP, and local-socket client and listener
fetch         basic HTTP and HTTPS transfer
packetdump    packet capture and protocol inspection
firewallctl   transactional firewall policy management
ssh           secure remote shell
scp / sftp    secure file transfer
```

Additional capabilities may begin as `netctl` subcommands and become separate tools
only when they have a distinct security boundary or substantial interactive use:

```text
netctl dhcp
netctl vpn
netctl proxy
netctl neighbor
netctl bridge
netctl tunnel
```

## `netctl` command model

Representative commands are:

```text
netctl status
netctl interface list
netctl interface show ethernet0
netctl interface set ethernet0 up

netctl address list
netctl address add ethernet0 192.168.1.20/24
netctl address remove ethernet0 192.168.1.20/24

netctl route list
netctl route add 10.20.0.0/16 via 192.168.1.1
netctl route delete 10.20.0.0/16
netctl route get 10.20.4.12

netctl dns status
netctl dns set ethernet0 1.1.1.1 1.0.0.1
netctl dns search ethernet0 example.internal

netctl wifi scan
netctl wifi connect "Network Name"
netctl wifi disconnect
netctl wifi status

netctl profile list
netctl profile show home
netctl profile activate home
```

Management commands should support these behaviors where applicable:

```text
--json             stable machine-readable output
--quiet            suppress ordinary output
--noninteractive   fail rather than prompt
--dry-run          validate and describe changes without committing them
--format <name>    select a documented output representation
```

Scripts should not scrape decorative tables. Structured output schemas should be
versioned, documented, and additive within a schema version.

Idempotent operations should be available for declarative automation. For example,
`netctl address ensure ...` succeeds whether it creates the address or finds the
requested state already present.

## Manual routing

Routing tables must be manually queryable and manageable. Automatic desktop
networking remains the common default, but manual routing is required for VPNs,
virtual machines, containers, development networks, multihomed hosts, gateways,
recovery, and policy routing.

The route model should support at least:

- IPv4 and IPv6 destinations;
- default routes;
- gateways and directly attached routes;
- interface scope;
- route metrics;
- multiple routing tables;
- source and destination policy rules;
- blackhole, unreachable, and prohibit results;
- explicit ownership and lifetime;
- optional per-route MTU and protocol metadata.

Representative advanced commands are:

```text
netctl route add table vpn 0.0.0.0/0 via 10.8.0.1
netctl rule add from 192.168.50.0/24 lookup vpn
```

Each route should record its owner, such as DHCP, an administrator, a VPN session, a
network profile, or a sandbox controller. One component must not silently destroy or
replace another component's routes.

## Neighbor tables: ARP and IPv6 discovery

ARP and IPv6 Neighbor Discovery should be exposed through one protocol-neutral
neighbor table:

```text
netctl neighbor list
netctl neighbor show 192.168.1.1
netctl neighbor list --interface ethernet0
netctl neighbor list --state stale
netctl neighbor list --json
```

The table should expose, at minimum:

- interface identity;
- network-layer address;
- link-layer address;
- protocol, such as ARP or NDP;
- reachability state;
- dynamic or permanent origin;
- age, expiration, and last-probe information where known.

A simplified public state model should include:

```text
incomplete
reachable
stale
probing
failed
permanent
```

Administrative operations should include:

```text
netctl neighbor probe 192.168.1.1
netctl neighbor delete 192.168.1.15
netctl neighbor flush
netctl neighbor flush --interface ethernet0
netctl neighbor add 192.168.1.50 aa:bb:cc:dd:ee:ff --permanent
netctl neighbor remove 192.168.1.50
```

A live event stream is desirable:

```text
netctl neighbor watch
```

Reading neighbor state requires observation authority. Adding, deleting, or flushing
entries requires stronger network-configuration authority. Sandboxed applications do
not receive either capability merely because they have ordinary outbound networking.

## Wi-Fi management

Wi-Fi must be fully manageable from the command line. Required operations include:

- enumerate radios and interfaces;
- scan and rescan;
- connect, disconnect, and inspect status;
- save, activate, reorder, and forget network profiles;
- display signal, frequency, channel, security mode, and roaming state;
- support hidden networks, WPA2, WPA3, and enterprise authentication;
- configure hotspot mode, regulatory domain, and MAC randomization;
- associate per-network DNS, proxy, VPN, and routing policy.

Secrets should not normally appear in process arguments or shell history. Interactive
password prompts and references to credential-service entries are preferred:

```text
netctl wifi connect "Home Network"
netctl wifi connect "Home Network" --credential network/home
```

The network service owns Wi-Fi policy and coordinates authentication and drivers. The
CLI does not issue hardware commands directly.

## Netcat-style diagnostic tool

NullStar should provide `netcat`, with `nc` as an optional compatibility alias. It
should support:

- TCP connections and listeners;
- UDP send and receive;
- IPv4 and IPv6;
- local stream, datagram, and sequenced-packet sockets when available;
- timeouts and verbose diagnostics;
- standard input and output pipelines;
- bounded port probing;
- optional TLS when certificate and trust behavior can be made explicit.

Native NSIDL services should be diagnosed through a separate protocol-aware tool or
`netcat` extension rather than pretending typed capability IPC is an ordinary byte
stream.

## Firewall scripting

Firewall policy must be manageable by scripts, but updates should be transactional and
atomic. Scripts should normally validate and apply a complete policy snapshot rather
than exposing a partially updated live ruleset.

Representative commands are:

```text
firewallctl status
firewallctl rules list
firewallctl validate workstation.nsfirewall
firewallctl apply workstation.nsfirewall
firewallctl rollback
```

Important properties are:

- atomic policy replacement;
- stable rule names or identifiers rather than mutable ordinal numbers;
- dry-run and validation modes;
- an explanation trace for rejected rules;
- preservation of the previous validated policy for rollback;
- remote-administration protection through timed rollback and explicit confirmation;
- capability-gated administration;
- application, package, service, user, interface, address, port, and profile matches;
- integration with the identity-aware policy model in `network-policy.md`.

A remote administrator should be able to use a guarded change such as:

```text
firewallctl apply server.nsfirewall --rollback-after 60s
firewallctl confirm
```

Ordinary applications cannot rewrite global policy. A sandboxed application may
request a narrowly scoped listener or portal-mediated exception without receiving
firewall administration authority.

## Shell and scripting direction

The existing `ush` shell should evolve into the primary interactive shell and ordinary
system scripting language. It should remain familiar enough for command-oriented use:

```text
command arg
command1 | command2
command > file
VAR=value
if ...
for ...
while ...
```

NullStar should not be bound to every historical POSIX shell ambiguity. Accepted goals
for the native shell include:

- arrays, booleans, integers, records, and explicit null values;
- no implicit word splitting unless explicitly requested;
- reliable quoting and escaping;
- structured error and command-status values;
- strict script mode;
- predictable pipeline failure propagation;
- first-class structured pipelines in addition to byte streams;
- convenient JSON and record transformation.

A future structured pipeline might look like:

```text
netctl interface list --records | where state == "up" | select name, addresses
```

Lua is the preferred initial embedded scripting language for applications,
configuration, and more complex automation because it is compact, mature, easy to
embed, and practical to sandbox. JavaScript or WebAssembly runtimes may be added later
for application compatibility, but they should not delay the basic native shell and
Lua embedding work.

The intended language roles are:

```text
ush     interactive command line and system scripts
Lua     embedded automation and application scripting
Rust    native system services, tools, and applications
```

## Native IPC before Unix-domain sockets

Native IPC channels should be implemented and stabilized before Unix-domain sockets.
They are the foundation for system services and must support semantics that are not
naturally modeled as byte streams:

- bidirectional bounded messages;
- handle and capability transfer;
- request and reply transactions;
- peer identity and credentials;
- blocking, asynchronous, and readiness-based operation;
- backpressure and peer-closure notification;
- service discovery;
- shared-memory transfer for large payloads.

NSIDL-generated bindings should connect through stable service identities such as:

```text
org.nullstar.network
org.nullstar.audio
org.nullstar.desktop.compositor
```

Native service endpoints are memory-resident kernel objects referenced by handles.
They should normally be published through the service manager rather than represented
by filesystem nodes.

## Unix-domain-socket compatibility

Unix-domain sockets should be added later as a compatibility and portability layer.
The intended compatibility surface includes:

```text
AF_LOCAL / AF_UNIX
SOCK_STREAM
SOCK_DGRAM
SOCK_SEQPACKET
socketpair
bind
listen
accept
connect
sendmsg
recvmsg
```

Descriptor or handle passing should be supported through `SCM_RIGHTS` compatibility
when the descriptor model can preserve NullStar rights reduction and policy checks.

Native IPC and Unix-domain sockets may share low-level queue, wait, handle-accounting,
and shared-memory machinery. Their public semantics should remain separate: native
IPC is typed, capability-oriented, and service-aware; Unix-domain sockets provide the
local stream, datagram, and sequenced-packet behavior expected by portable software.

## Naming and filesystem placement

An endpoint object and the name used to discover it are separate concepts. A local
endpoint may be:

- transferred directly as a capability;
- registered under a native service identity;
- published under an abstract local-socket name;
- bound to a filesystem pathname for POSIX compatibility.

The endpoint, queues, and transferred data remain memory resident in every case. A
filesystem socket node is only a rendezvous name and access-control object; it does not
contain the communication data.

General IPC and local sockets must not appear under `/dev`. That namespace is reserved
for devices and device-facing interfaces.

Filesystem-bound local sockets should live in volatile runtime state, tentatively:

```text
/System/Run/Services/database.sock
/System/Run/Users/<uid>/ssh-agent.sock
/System/Run/Users/<uid>/app/com.example.editor/socket
```

`/System/Run` should be memory-backed, recreated during boot, nonpersistent, and scoped
by system service or user session. A future Unix-compatibility environment may expose
`/run` as an alias or compatibility mount.

Abstract local names should also be supported for endpoints that must disappear
automatically when their final owning handle closes.

## Capability and authorization model

Suggested capability distinctions include:

```text
network.observe
network.configure
network.route.manage
network.neighbor.manage
network.wifi.manage
network.firewall.manage
service.publish:<service-id>
service.connect:<service-id>
local-socket.bind:<scope>
local-socket.connect:<scope>
```

The exact capability vocabulary remains tentative. The important requirement is that
reading state, changing global state, publishing a native service, and binding a local
socket are separate authorities.

Filesystem permissions and peer credentials should apply to pathname-bound Unix-domain
sockets for compatibility. Sandbox policy may impose additional restrictions even
when ordinary path permissions would allow a connection.

## Recommended implementation stages

1. Stabilize native channel endpoints, waits, notifications, capability transfer, and
   peer closure.
2. Add service-manager publication and connection by stable service identity.
3. Introduce the network-management service with interface and address inspection.
4. Add route, DNS, DHCP, and neighbor-table inspection and manual administration.
5. Add Wi-Fi scanning, connection profiles, and credential-service integration.
6. Add `ping`, `lookup`, `netstat`, `netcat`, and packet-capture diagnostics.
7. Add transactional firewall validation, apply, rollback, and script-safe output.
8. Extend `ush` with strict scripts and structured records; add embedded Lua.
9. Add abstract local sockets and `socketpair` over shared local primitives.
10. Add pathname-bound `AF_UNIX` compatibility under volatile runtime storage,
    including stream, datagram, sequenced-packet, and rights-preserving handle passing.

## Open questions

- Exact division among the network stack, management service, policy service, resolver,
  and Wi-Fi authentication components.
- Final structured-output schema and version-negotiation convention.
- Whether firewall policy files use a dedicated declarative language or a common
  system configuration format.
- Exact native shell record and pipeline syntax.
- Lua runtime implementation and sandbox boundary.
- Mapping Unix credentials and `SCM_RIGHTS` onto native identities and reduced-rights
  capability handles.
- Whether abstract local names are global, per-user, per-session, per-sandbox, or a
  hierarchy containing all of those scopes.
