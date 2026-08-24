# Capability-based application sandboxing

## Status

The following are **accepted direction**:

- every process launched as an application is sandboxed;
- installation location never disables the sandbox;
- a manifest may declare or request authority but never grants it;
- runtime authority exists through explicit capabilities and restricted service
  endpoints;
- user-selected resources are issued through portals as scoped capabilities;
- system applications remain sandboxed and receive only signed, narrow entitlements;
- sensitive live resources use provider-controlled sessions that can expire or be
  revoked;
- each application instance and component role is contained by jobs and explicit handle
  routing.

Stable signed application identity, permission records, leases, user-facing privacy
controls, application groups, background leases, and operation-specific administrative
authorization are also accepted architectural direction. Exact manifest encoding,
permission vocabulary, prompt wording, grant-database format, lease protocol, and
package-signing implementation remain **tentative design**.

This document describes future architecture rather than currently implemented behavior.
The present capability foundation and limitations are documented in
[Capability and IPC protection model](../protection-model.md). Process construction,
jobs, sessions, and supervision are specified in
[Service, session, and application lifecycle](service-and-session-lifecycle.md).

## Core rule

NullStar should apply one consistent rule to application launches:

> Being installed grants permission to be discovered and launched, not permission to
> access the rest of the system.

Every process launched through the application runtime begins with an explicitly
constructed capability table and restricted service namespace. A process cannot name or
use a resource merely because it knows a pathname, service name, process ID, device
name, user identity, or package location.

The implemented baseline desktop namespace is one exact-`SEND` NSRT endpoint backed by an
immutable route allowlist rather than a global registry handle. It admits display,
application-lifecycle, settings, logging-producer, audio-playback, and portal client routes. A key
outside that set is denied before provider lookup, while an allowed key may independently be
unavailable or resolve to the current generation's exact send-only provider endpoint.

## Terminology

These terms have distinct meanings:

| Term | Meaning |
| --- | --- |
| **Capability** | A live process-local handle or restricted service endpoint carrying authority |
| **Permission declaration** | A manifest statement that an application may request a class of authority |
| **Grant** | A policy record authorizing a capability to be issued under specified conditions |
| **Entitlement** | A privilege authorized by signing, package, or installation policy |
| **Portal** | A trusted broker that obtains user intent and performs an operation or returns a scoped capability |
| **Sandbox profile** | The maximum process and job policy allowed for a technical process class |
| **Lease** | A revocable or expiring provider relationship governing a live capability |
| **Application identity** | A stable principal derived from application ID, publisher identity, and signing lineage |

A permission name such as `audio.capture` is not itself authority. Authority is the
capture-stream endpoint eventually issued by the audio service.

```text
manifest declaration
        |
        v
policy and user decision
        |
        v
restricted provider endpoint
        |
        v
actual capability
```

## Mandatory sandboxing independent of location

Every graphical or bundle-based application is sandboxed regardless of whether it is
stored in:

```text
/System/Applications
/Applications
/Users/<user>/Applications
external volumes
download or development directories
```

Location determines installation scope, ownership, discovery, update policy, and which
trust policy is evaluated. It never provides a sandbox bypass.

| Location | Scope and management | Sandbox policy |
| --- | --- | --- |
| `/System/Applications` | Supplied by a verified system generation | Mandatory |
| `/Applications` | Installed machine-wide through authorized package policy | Mandatory |
| `/Users/<user>/Applications` | Installed for one user | Mandatory |

Copying an ordinary application into `/System/Applications` must not turn it into a
system application. Likewise, a user-owned bundle does not inherit the user's complete
home-directory authority merely because it resides inside the home tree.

The application manager resolves a verified application identity through package and
namespace services. A pathname is mutable metadata, not a security principal.

## Application identity

Permission and update policy should bind to a stable verified identity conceptually
containing:

```text
application identifier
publisher signing identity
accepted signing-key lineage
package trust class
installation provenance
system or third-party designation
```

The reverse-domain-style application identifier remains stable across display-name and
bundle-path changes. The display name and icon are user interface data; security
records use the verified identity.

### Updates and signing lineage

An update may inherit eligible grants only when:

- the application identifier is unchanged;
- the package is signed by the accepted publisher lineage;
- the installation record authorizes the update;
- the new version does not select a more privileged sandbox profile;
- current user and administrator policy still permit the grant.

Newly declared permissions are not automatically granted. A signing-key rotation must
use an authenticated lineage transition. An unexpected publisher change creates a
different security principal and must not silently inherit grants.

### Development applications

Developer mode may accept local signing, assign a stable local developer identity, and
issue explicit debugging capabilities. It should not implicitly disable the sandbox.
Unsigned or ad hoc builds may receive temporary identities and should not silently
inherit persistent grants from a release build or a previous unrelated build.

Trusted permission UI must identify developer builds clearly.

## Policy layering

The maximum authority an application may receive is the intersection of independent
policy layers:

```text
platform maximum
    intersect sandbox-profile maximum
    intersect signed entitlements
    intersect manifest declarations
    intersect administrator policy
    = eligible authority

eligible authority + approved contextual or user grants
    = effective runtime authority
```

No individual layer overrides the others:

- a user cannot grant an ordinary application raw physical-memory access;
- a manifest cannot grant itself a driver entitlement;
- a system signature does not automatically grant every system service;
- an administrator may reduce an application's access;
- a prompt cannot issue authority outside the platform and profile maximum;
- a service may delegate only capabilities it already possesses.

Policy should be enforced primarily when authority is acquired. After an application
receives a read-only file capability, ordinary reads are authorized by that endpoint and
its rights rather than by a global permission-database lookup on every operation.
Revocable live resources may additionally check a provider-controlled lease.

## Application classes

### System application

A system application is part of a verified system generation and signed by an accepted
NullStar system key. It remains sandboxed. A system-only entitlement becomes a narrow
capability or service endpoint only after package and policy validation.

### Machine application

A machine application is installed for all users through authorized machine package
policy. Each user launches it in the ordinary application sandbox with that user's
separate grants.

### User application

A user application is installed for one user. It receives the same mandatory sandbox as
a machine application and is not eligible for system privilege merely because the user
owns its files.

### Development or compatibility application

Development and compatibility applications use explicit, visible profiles. Any broader
filesystem, debugger, toolchain, or legacy authority must be declared and user approved.
It does not imply kernel, device, authentication, or trusted-desktop authority.

## Sandbox profiles

Profiles are technical process classes and non-relaxable job-policy templates. They are
not broad categories such as “game” or “document editor” and are not a replacement for
explicit capabilities.

| Profile | Intended use | Who may select it |
| --- | --- | --- |
| `desktop` | Ordinary graphical application component | Any valid application bundle |
| `desktop-child` | Reduced renderer, decoder, or helper process | Declared application component |
| `worker` | Isolated non-UI application worker | Declared application component |
| `background-agent` | User-visible sync or persistent agent | Declared and user-approved bundle |
| `system-application` | Settings, file manager, and trusted system utilities | System-authorized signature |
| `system-ui` | Compositor shell, login, lock, and trusted UI | System-authorized signature |
| `system-service` | Machine-wide supervised service | Service-manager-authorized definition |
| `driver-host` | Userspace driver process | Verified driver package and device policy |
| `recovery` | Independent recovery environment | Verified system generation |
| `compatibility` | Legacy or development software | Explicit user or developer action |

Editing a manifest must not allow an application to select a privileged profile. Package,
signing, installation, and service policy authorize the profile independently.

## Permission classes

NullStar should divide requested access into five classes.

### 1. Baseline capabilities

These are required for an ordinary desktop application to function and normally do not
produce prompts:

```text
ui.display
ui.theme
ui.fonts
application.lifecycle
settings.private
storage.private
storage.cache
storage.temporary
portal.desktop
logging.limited
audio.playback
```

Each name resolves to a restricted endpoint or directory capability. Baseline access is
not ambient authority to the global display, settings database, filesystem, or media
devices.

### 2. Declared static capabilities

These must appear in the manifest but normally should not interrupt the user every time:

```text
network.client
notifications.publish
process.spawn.same-application
background.task
media.remote-controls
```

They should be visible in application information and system settings. Administrator or
user policy may disable them. Static declaration is appropriate where repeated prompts
would make the desktop unpleasant but undeclared access should still be prevented.

### 3. User-action portals

These are available to sandboxed applications when the user initiates a trusted action:

```text
open file
save file
select directory
print
open URI
share content
drag and drop
choose application
create document
```

The portal performs the operation or returns authority for the exact selected resource.
It does not return a boolean that unlocks an entire global namespace.

### 4. Sensitive runtime permissions

These require a manifest declaration and a contextual runtime decision:

```text
audio.capture
camera.capture
screen.capture
location
network.local-discovery
device.usb
clipboard.background-read
input.global-observe
```

The decision should occur when the feature is used, not through a wall of installation
or first-launch prompts.

### 5. Restricted entitlements

These require system signing, administrative package policy, developer mode, or another
trusted installation authority. An ordinary application cannot obtain them through a
prompt:

```text
driver.device
filesystem.system
filesystem.all-user-data
network.raw
network.packet-capture
vpn.provider
runtime.jit
process.debug-other
accessibility.control
input.global-inject
authentication.provider
package.install
system.configuration.write
ui.trusted-surface
background.unrestricted
```

There is no generic `sandbox = false`, `trusted = true`, or unrestricted administrator
entitlement.

## Recommended capability behavior

| Resource | Acquisition | Typical persistence | Transfer policy |
| --- | --- | --- | --- |
| Display client | Launch baseline | Process lifetime | Same application only |
| Audio playback | Launch baseline | Stream lifetime | Normally nontransferable |
| Outbound internet | Manifest declaration | Until revoked | Nontransferable factory |
| Local-network discovery | Contextual first use | Session or persistent | Nontransferable |
| Microphone | Runtime prompt | Once, while in use, or persistent | Nontransferable |
| Camera | Runtime prompt | Once, while in use, or persistent | Nontransferable |
| Screen capture | Trusted source-selection UI | Capture session | Nontransferable |
| Selected file | File portal | Ephemeral or persistent | Through transfer portal |
| Selected directory | Directory portal | Session or persistent | Through transfer portal |
| Clipboard read | Paste action or user gesture | One operation | Nontransferable |
| USB device | Device-specific prompt | Connection, session, or persistent | Nontransferable |
| Background execution | Manifest plus user setting | Until revoked | Not applicable |
| JIT executable memory | Restricted entitlement | Process lifetime | Not applicable |

The exact defaults may evolve, but distinct resources must not be collapsed into one
coarse “trusted application” decision.

## Application manifest

A bundle manifest declares identity, entry points, component roles, requested services,
permission descriptions, and restricted entitlements. It is policy input, not authority.
A possible conceptual form is:

```toml
[application]
id = "org.example.Editor"
name = "Example Editor"
version = "1.4.0"
profile = "desktop"
entrypoint = "Contents/Executables/editor"

[uses]
required = [
    "ui.display",
    "portal.desktop",
    "audio.playback",
]

declared = [
    "network.client",
    "notifications.publish",
]

[[permission]]
name = "audio.capture"
usage_description = "permission.microphone.voice-notes"
allowed_scopes = ["once", "while-in-use"]

[[permission]]
name = "network.local-discovery"
usage_description = "permission.network.find-devices"
allowed_scopes = ["session", "persistent"]

[[component]]
name = "renderer"
entrypoint = "Contents/Executables/renderer"
profile = "desktop-child"
uses = ["ui.render-surface"]

[[component]]
name = "thumbnail-worker"
entrypoint = "Contents/Executables/thumbnail-worker"
profile = "worker"
uses = ["application.worker-control"]
```

The package verifier should reject:

- unknown required capabilities;
- unauthorized profiles or restricted entitlements;
- invalid or privilege-amplifying combinations;
- missing localized usage descriptions for sensitive requests;
- duplicated application identities with incompatible publishers;
- helper components requesting more authority than policy allows;
- unknown mandatory fields or unsupported manifest major versions.

A feature may be marked required for basic operation or optional. Denial of an optional
permission should disable only that feature. An application must not trap the user in an
endless permission-prompt loop.

## Launch sequence

The desktop shell does not execute a bundle's inner executable directly. It asks the
application manager to construct the sandbox:

1. resolve the bundle and installation record;
2. verify package integrity, signature, application identity, and signing lineage;
3. validate the manifest, profile, component role, and entitlements;
4. load administrator and current-user policy;
5. create an application job with non-relaxable resource and security policy;
6. create the initial process in a suspended state;
7. map the verified executable, runtime, and read-only bundle resources;
8. create private data, cache, temporary, and runtime directory capabilities;
9. route baseline and approved service capabilities;
10. restore only still-valid persistent grants;
11. install one bootstrap channel and send the startup message;
12. start the initial thread and supervise readiness and lifecycle.

Direct execution of a bundle executable must not bypass application mediation. A shell
launch of a graphical bundle should cross back through the application manager.

The native lifecycle layer now supervises the resulting job with root-pinned readiness, bounded
startup and relaunch budgets, whole-job termination, and completion drainage before replacement.
User termination and session teardown are terminal stop reasons and cannot accidentally reactivate an
application from backoff. Persistent restoration and permission grants remain manager policy above
this mechanism.

## Baseline authority

A normal desktop application may initially receive:

- read-only authority rooted at its own bundle;
- read-write private data, cache, and temporary directories;
- a display endpoint limited to its own surfaces;
- input addressed only to those surfaces;
- font, locale, theme, clock, timer, and limited logging services;
- private application settings;
- audio playback;
- an application lifecycle endpoint;
- a restricted portal and service namespace;
- a reduced process-self handle;
- valid restored grants selected for this launch.

It must not initially receive:

- the user's home directory or global filesystem root;
- other applications' bundles or private data;
- arbitrary device handles;
- microphone, camera, or screen streams;
- raw input or global input observation;
- global clipboard history;
- unrestricted service discovery;
- other-process inspection or debugging;
- physical memory, I/O ports, interrupts, or DMA resources;
- authentication secrets or system configuration authority.

## Private storage and filesystem authority

An application receives directory capabilities for logical roles rather than ambient
access to physical paths. It should ask the runtime for its bundle, data, cache,
temporary, and runtime roots using the verified application identity.

User documents remain outside private storage. File operations should be relative to a
file or directory capability:

```text
selected directory capability + relative path -> constrained lookup
```

The application cannot escape the represented subtree with `..`, aliases, mount
bindings, or path spelling. The filesystem or VFS service validates every relative
lookup against the capability root.

Running applications should not modify their own installed bundles. Installation,
replacement, update, and removal belong to Magnetar and package services with separate
authority.

## File and directory portals

The file portal owns the trusted picker. The user's selection returns exactly the
requested authority:

- opening an existing file may return `READ` or `READ | WRITE`;
- saving may create a destination and return a file capability;
- selecting a project directory returns a capability rooted at that subtree;
- importing a copy may return a private application-owned object instead of continued
  external access.

A pathname may be shown for user understanding or compatibility, but it is not the
security token. Persistent permission records store stable resource identity and policy
needed to recreate authority, not a serialized live handle.

### Drag and drop and sharing

Drag and drop is a user-mediated capability transfer. A file-transfer or share portal
receives the source object, target application identity, requested rights, and trusted
user-gesture context, then issues a fresh capability to the destination.

The destination does not gain the containing directory. Cross-application transfer may
be ephemeral unless the user explicitly retains access.

## Network authority

Applications should not receive raw access to network devices.

### Outbound internet

`network.client` is a manifest-declared static capability. The network service returns a
restricted socket factory attributed to the verified application and session identity.
It should be visible and revocable in Settings but should not prompt for every ordinary
connection.

### Local network

Local-network discovery reveals devices and services in the user's environment and is a
separate permission:

```text
network.local-discovery
```

It may cover multicast discovery, service browsing, broad local-address scanning, and
similar enumeration. A user-entered connection to a specific local address may be
handled differently from passive discovery or scanning.

### Listening and privileged networking

`network.listen` returns a listener constrained by protocol, interface class, address,
port or assigned port, and lifetime. The network service owns firewall integration.

Raw packets, packet capture, route management, firewall management, VPN providers, and
custom DNS-provider roles are restricted entitlements. Revocation should close or
disable the application's brokered network sessions.

## Microphone, camera, and screen capture

### Microphone

Audio playback is baseline; capture is runtime-authorized. A capture endpoint is limited
by application identity, selected device or device class, format policy, session
lifetime, lease, and background-capture policy.

The trusted desktop displays a microphone-use indicator while capture is active. The
indicator identifies the application and provides an immediate stop action.

### Camera

A camera grant may constrain device, front/back or integrated/external class,
resolution, still-image versus continuous capture, and background behavior.
Applications should receive camera-service streams rather than raw USB authority where
practical.

### Screen capture

A trusted portal lets the user choose a window, application, monitor, or rectangular
region. The returned capture endpoint is limited to that source. Requesting screen
capture must not default to every display.

A visible trusted indicator remains present while capture is active. Protected or
sensitive surfaces may be excluded by compositor policy.

## Clipboard and input

Writing ordinary clipboard data may be baseline subject to size and format limits.
Reading should normally require a paste action, trusted user-gesture token, drop
operation, or another contextual action.

A clipboard-history manager requires a separate persistent permission. Sensitive entries
may be short-lived or non-persistent.

Applications receive keyboard and pointer events only for their own compositor surfaces.
These remain separate restricted capabilities:

```text
input.global-observe
input.global-hotkey
input.global-inject
accessibility.observe
accessibility.control
```

Global observation and injection can expose credentials or control trusted UI and must
not be normal desktop permissions.

## USB and device access

Ordinary applications should use device-class services:

```text
audio
camera
printing
storage portal
MIDI
game controllers
radio
Bluetooth
```

They should not normally receive a raw `/dev`-style object.

An application that legitimately needs direct USB protocol access uses a USB portal. The
trusted decision identifies the manufacturer, product, serial number where available,
interface or function, requested operations, and duration. Device enumeration is
filtered so the application does not learn about devices outside its allowed scope.

A device capability is bound to provider generation and becomes stale after disconnect,
reset, driver replacement, or explicit revocation.

## Grants and permission records

A per-user permission service should maintain records conceptually containing:

```text
grant identity
application identity
user and session identity
resource kind and stable resource identity
rights and scope
creation and expiration
source of the decision
state and policy generation
```

Useful scopes are:

```text
ONCE
PROCESS
WHILE_IN_USE
APPLICATION_SESSION
LOGIN_SESSION
PERSISTENT
UNTIL_DATE
ADMINISTRATOR_MANAGED
```

Decision sources include user selection, runtime prompt, administrator policy, system
entitlement, application group, and migrated grant.

The store should support atomic updates, denial records, prompt rate limiting, grant
expiration, signing-lineage transitions, uninstall cleanup, reset, and policy migration
across system upgrades.

A stored record is authorization to recreate a capability. It must never let a caller
supply another application's identifier and retrieve that application's authority.

## Revocation and leases

Kernel handle rights remain immutable and monotonic. NullStar should not begin with a
generic kernel operation that searches every process and destroys all derived copies of
a handle.

Sensitive live resources instead use provider-controlled leases:

```text
application endpoint
        |
        v
provider session bound to lease
        |
        v
underlying resource
```

A lease records owner, resource scope, rights, lifetime, parent lease where delegated,
and state such as:

```text
ACTIVE
SUSPENDED
REVOKED
EXPIRED
```

Revocation should:

- prevent new operations;
- cancel pending work where safe;
- close or disable the provider endpoint;
- wake waiters with a distinct revoked or peer-closed result;
- revoke child leases;
- prevent recreation from the associated grant.

Revocation cannot erase data already copied into application memory. Highly revocable
streams should use replaceable shared buffers and provider indirection rather than
permanent unrestricted mappings.

### Delegated leases

A child lease may have no greater rights, broader resource scope, later expiration, or
more permissive transfer policy than its parent. Revoking a parent revokes all
descendants.

This allows an application to give a declared worker a 30-second reduced capture stream
without transferring its entire persistent microphone grant.

## Transfer and delegation policy

A handle's `TRANSFER` right controls technical transferability. Providers decide whether
to include it.

Sensitive grants should be nontransferable by default:

```text
microphone stream                 no transfer
camera stream                     no transfer
screen-capture session            no transfer
authorization ticket              no transfer
selected document                 transfer through document portal
private application pipe          transfer within application policy
worker shared-memory queue        transfer to declared component
```

Same-application helpers do not automatically receive sensitive handles. The main
component may ask the provider to issue a child endpoint, use an application-private
proxy, or create a reduced child lease when policy permits.

Cross-application delegation uses a share, drag-and-drop, document, app-service, or
user-selected-target portal. This prevents one application from laundering authority to
an unrelated process.

## Multi-process application roles

Modern applications contain components with different trust requirements:

```text
main process
renderer
GPU process
media decoder
archive worker
plugin host
background agent
```

The bundle manifest declares each role, executable, profile, and requested capabilities.
The application manager constructs each child through an explicit handle allowlist.
Native process spawning must not clone every parent capability.

A compromised renderer should not inherit selected-document, network, password-manager,
or microphone authority merely because it belongs to the same application job.

A POSIX `fork` compatibility layer may reproduce inherited descriptors within the same
application sandbox where required, but native components should use explicit spawn.

## Application-provided services

An application may export a versioned protocol, for example a password-manager provider,
image-filter service, document converter, media-control endpoint, URL handler, or
extension.

Possible visibility classes are:

```text
private
same-application
same-app-group
same-publisher
user-selected
explicit-allowlist
public
system-only
```

“Public” means discoverable through the scoped user service broker, not a globally
writable kernel namespace. Each connection receives a fresh channel and authenticated
caller metadata where the protocol requires it.

Identity may inform display, auditing, quotas, and policy, but it is not authority. A
service should ask the caller to supply the relevant resource capability instead of
opening an arbitrary global pathname based only on caller identity.

## Application groups

Applications from the same publisher do not automatically share storage, permissions,
or service access.

A shared application group requires:

- explicit group declaration by each package;
- compatible publisher signing lineage;
- installation-policy approval;
- a stable group identity;
- explicit shared resources and protocols.

A group may receive a shared private directory, database service, group-only namespace,
credential domain, or background agent. An ordinary application grant does not propagate
to the group unless the grant explicitly names the group as its subject.

## Background execution

NullStar should behave like a desktop OS rather than suspending every application as soon
as its windows disappear. Persistent background work must nevertheless be visible,
scoped, and controllable.

Recognized background reasons may include:

```text
active audio playback or recording
file transfer
download or upload
user-visible synchronization
status item
device session
scheduled maintenance
approved background service
```

A background lease records reason, expected lifetime, user-visible description,
resource limits, and cancellation behavior. Start-at-login agents appear in a Background
Applications settings panel. There should be no hidden autostart path that grants
unlimited permanent execution merely by placing a file in a directory.

## Administrative operations

A sandboxed Settings application, installer, or network tool must not be relaunched as an
unrestricted privileged process.

Instead:

```text
application requests one semantic operation
                |
                v
authorization service verifies user and policy
                |
                v
single-use operation ticket
                |
                v
privileged service performs exactly that operation
```

A ticket should be bound to the caller application identity, target service, operation,
normalized parameters or digest, user and session identity, expiration, and nonce.

For example, package installation authority may cover one verified package hash and
installation domain. It must not become a reusable generic administrator capability.
A `sudo`-like compatibility interface may exist later, but it is not the native desktop
privilege model.

## Permission and privacy settings

System Settings should provide both application-centric and resource-centric views.

An application view may show:

```text
Example Editor
├── Files and folders
├── Microphone: While using
├── Camera: Never
├── Local network: Allowed
├── Internet access: Allowed
├── Notifications: Allowed
├── Background activity: Allowed
└── Recent sensitive-permission activity
```

A resource view may show every application with microphone, screen-capture, local-
network, accessibility, or background authority.

Users should be able to:

- revoke a grant;
- change persistent access to ask-each-time;
- reset all permissions for an application;
- see active sensitive sessions;
- stop active capture immediately;
- disable background execution;
- inspect why a restricted entitlement exists.

Permission-use auditing records application, category, decision, and time, not captured
content, passwords, document contents, clipboard contents, or raw capability data.

## Uninstall and retained data

Uninstall should revoke or remove:

- persistent document and directory grants;
- sensor and device permissions;
- network exceptions;
- background registrations and scheduled tasks;
- exported service registrations;
- application-group membership;
- active and restorable leases.

The user may separately choose to remove the application, private data, and permission
history. Retained data may be restored to a later installation with the same verified
identity, but permission grants should return only under explicit policy.

## Shell, service, and compatibility launches

Mandatory application sandboxing applies to application-runtime launches. It does not
mean every executable in the system receives the same baseline.

A native shell command receives authority according to explicit shell and child-process
rules. A managed service receives a service definition and service-specific capability
routes. A driver receives a driver-host job and device capabilities. None of these paths
may let an application bundle bypass the application manager by invoking its internal
executable directly.

Ported software that expects ambient paths and subprocess inheritance should use an
explicit compatibility profile. Useful compatibility tiers may include:

```text
compatible sandbox       synthetic filesystem and brokered network
expanded workspace       selected broad directory roots
developer workspace      source trees, toolchain, and debugger
legacy desktop           broad user filesystem, visibly disclosed
```

Even the broadest ordinary compatibility profile does not imply raw hardware,
authentication secrets, trusted UI, other login sessions, or unrestricted system-
service administration.

## Kernel responsibilities

The kernel enforces:

- process-local handle tables and unforgeable object references;
- immutable rights and rights-reduced duplication or transfer;
- object types, signals, and lifecycle;
- process, address-space, memory, IPC, and job isolation;
- resource limits and non-relaxable child-job policy;
- immutable process and job security labels set by trusted launchers;
- denial of unauthorized kernel, physical-memory, interrupt, and device resources.

The kernel does not interpret strings such as `camera`, `Downloads folder`, `allow while
using`, or `trusted editor`. Those are userspace policy concepts.

## Userspace responsibilities

Trusted userspace components decide:

- whether a package, signature, manifest, profile, and entitlement are valid;
- which service and resource capabilities are routed at launch;
- whether and how the user is prompted;
- which grants persist, expire, or are administrator managed;
- which file, network, media, input, device, and background scopes are approved;
- how provider leases are created and revoked;
- which application handles a document type, URI, or exported protocol.

Every policy decision must result in concrete restricted authority. A permission record
that does not control capability issuance is not a complete sandbox.

## Service capability routing

System services use the same capability philosophy without ordinary application prompts.
A service definition declares capabilities consumed, provided, and offered to children.
The service manager constructs and validates the route graph before launch.

```text
device manager
    -> device endpoint
       -> audio driver
          -> audio-device protocol
             -> media service
                -> restricted playback endpoint
                   -> application
```

The application need not know a driver process ID, PCI address, physical device path, or
global service name.

Administrative tools should inspect the resulting graph through commands such as:

```text
capability-list <application>
capability-graph <service>
permission-show <application>
lease-list
active-capture-list
```

A build-time policy checker should compare installed manifests and intended routes before
a system generation is activated.

## Implementation sequence

The native implementation now provides the launch-boundary portions of Milestone 1: clean
descriptor/capability tables, bounded application jobs, typed root startup,
package/installation/publisher-lineage matching, installed executable and profile authorization,
explicit rights-monotonic `desktop-child`/`worker` allowlists, an identity-bound private-storage
broker and restricted service-namespace capability set, canonical relative-root request policy, and
an inherited one-way kernel seal against ambient global paths. Concrete provider-backed directory
provisioning, service-route population, cryptographic package-verifier and registry services, and
general application-manager supervision remain outstanding.

### Milestone 1: mandatory application isolation

- stable application identity;
- application jobs and component roles;
- ordinary `desktop`, `desktop-child`, and `worker` profiles;
- private data, cache, temporary, and runtime capabilities;
- restricted service namespaces and explicit startup handles;
- all graphical bundles launched through the application manager;
- no privilege based on installation path.

### Milestone 2: file portal and permission store

- trusted window and user-gesture tokens;
- open, save, directory-selection, drag-and-drop, and share portals;
- file and directory capabilities;
- persistent resource identities and grant records;
- permission inspection, revocation, and reset;
- synthetic filesystem projection for compatibility applications.

### Milestone 3: privacy-sensitive resources

- microphone, camera, and screen-capture portals;
- contextual clipboard reads;
- once, while-in-use, session, and persistent scopes;
- trusted active-use indicators and immediate stop actions;
- provider-level leases and revocation.

### Milestone 4: network and devices

- declared outbound-network factories;
- local-network and listener policy;
- USB portal with filtered enumeration;
- Bluetooth, MIDI, radio, camera, and other class services;
- per-application network and device controls.

### Milestone 5: delegation and multi-process applications

- explicit child handle allowlists;
- reduced child leases;
- application groups;
- application-exported services;
- cross-application document transfer;
- isolated plugin and decoder hosts.

### Milestone 6: administration and compatibility

- single-use administrative authorization tickets;
- background application management;
- restricted entitlements and developer identities;
- compatibility profiles;
- package-policy linting and capability-graph inspection.

## Required invariants

> Every process launched as an application is sandboxed. Installation location never
> disables the sandbox.

> A manifest, entitlement, path, process ID, user identity, or stored permission record
> is policy input, not authority. Runtime authority exists through explicit
> capabilities.

> User-mediated actions return scoped resource capabilities rather than a boolean that
> unlocks an ambient namespace.

> Sensitive live capabilities are nontransferable by default and use provider-controlled
> leases where revocation or expiration is required.

> Kernel handle rights remain immutable and monotonic. High-level revocation is
> implemented through provider sessions and indirection rather than a global kernel
> permission database.

> System applications, services, and drivers remain sandboxed. Additional authority
> comes from verified identity, restricted profiles, signed entitlements, and explicit
> capability routes.

> Native administrative operations use narrow, single-use authorization rather than
> launching an entire graphical application with unrestricted privilege.
