# Application bundles, signing, and deployment direction

## Status

The following are **accepted direction**:

- a native graphical application is an immutable application bundle selected through an
  application generation rather than a writable directory executed in place;
- `.app` is the canonical native bundle suffix, although graphical tools may hide it;
- application identity combines a stable application identifier with authenticated
  publisher signing lineage;
- package identity, application identity, component identity, generation identity, and
  process identity remain distinct;
- Magnetar imports verified content, constructs complete application generations, and
  atomically selects the generation used for new launches;
- running processes retain the immutable generation from which they were launched;
- manifests request capabilities and describe components, handlers, and compatibility,
  but do not grant authority;
- mutable configuration, state, cache, runtime files, and user documents remain outside
  the immutable bundle;
- application updates cannot silently inherit a different publisher identity, stronger
  sandbox profile, restricted entitlements, or newly requested permissions;
- document and URI activation use typed lifecycle messages and scoped capabilities rather
  than ambient path access;
- helpers, migration tools, extensions, plugin hosts, and background agents are declared
  components with explicit launch contracts;
- direct execution from writable downloads, removable media, network shares, or
  development trees is replaced by verified import into an immutable installed or
  transient generation.

The exact internal directory capitalization, manifest encoding, canonical serialization,
signature algorithms, catalogue representation, registry database format, state-snapshot
mechanism, and user-interface wording remain **tentative design** until implementation
and interoperability testing establish durable contracts.

This document refines application-specific parts of
[Magnetar package and deployment management](package-management.md),
[Capability-based application sandboxing](application-sandboxing.md), and
[Service, session, and application lifecycle](service-and-session-lifecycle.md).
Executable mapping and loader policy remain defined by
[Executable loading and linking](executable-loading.md). Current code does not yet
implement native application bundles, signing, application generations, or the
application registry described here.

## Design goals

The application deployment model should:

- make an application appear as one coherent installable desktop object;
- preserve stable identity across display-name, path, icon-theme, and version changes;
- prevent partial installation and in-place self-modification;
- preserve sandboxing regardless of installation location;
- support per-user and machine-wide installation without conflating installation scope
  with runtime privilege;
- make updates and rollback atomic at the application-generation boundary;
- keep old immutable content available while an old process still executes it;
- expose document types, URI schemes, exported services, extensions, and background roles
  through one verified registration transaction;
- make permission expansion, signing-key changes, and state migration visible and
  enforceable;
- support local development without weakening production trust rules;
- remain compatible with future freedesktop metadata and package import without making
  mutable desktop-entry files the native source of application identity.

The central rule is:

> A NullStar application is identified and launched from a verified immutable generation.
> Its filename and installation path are presentation and deployment metadata, not its
> identity or authority.

## Distinct objects

NullStar should keep the following concepts separate.

| Object | Meaning |
| --- | --- |
| **Application bundle** | Immutable runtime content and semantic manifest |
| **`.nspkg` archive** | Transport and installation container consumed by Magnetar |
| **Package object** | Verified immutable content stored by hash in the package store |
| **Application generation** | Exact bundle and private runtime selected for launch |
| **Application registration** | Mutable record of generation, handlers, policy, and health |
| **Application data** | Mutable configuration, state, cache, runtime files, and documents |
| **Application instance** | One supervised running application job created from one generation |
| **Component instance** | One process role inside an application instance |

The normal flow is:

```text
publisher builds Example.app
            |
            v
Magnetar packages or receives Example.nspkg
            |
            v
archive, signatures, dependencies, and policy are verified
            |
            v
immutable package objects are imported
            |
            v
application generation is constructed and registered
            |
            v
application manager launches the selected generation in a sandbox
```

An update constructs another generation. It does not overwrite the active bundle one
file at a time.

## Identity layers

The deployment and runtime system needs several noninterchangeable identities.

### Package identity

A package identifier names a unit resolved, installed, retained, and audited by Magnetar.
A package may contain one application bundle plus closely related private runtime,
localization, documentation, symbols, or declared extensions.

Examples include:

```text
org.example.Editor
org.example.Editor.locales.fr
org.example.Editor.debug-symbols
org.example.Editor.extension.git
```

### Application identity

Application security policy binds to a stable identity conceptually containing:

```text
application identifier
publisher lineage identifier
trust class
system or third-party designation
```

For ordinary application packages, the package and application identifiers may match,
but they are not the same type or authority.

### Component identity

A component identity names a declared role within a particular application identity,
such as:

```text
org.example.Editor/main
org.example.Editor/renderer
org.example.Editor/thumbnail-worker
```

It constrains which executable, sandbox profile, startup handles, and resources that role
may receive.

### Generation identity

A generation identity names one exact verified deployment graph. It should include or
reference:

- the application identity;
- semantic version and package release;
- package object hashes;
- private runtime graph;
- manifest and registration digest;
- signing lineage and repository snapshot;
- state-compatibility declaration;
- parent generation and transaction identity.

### Process and instance identity

A process ID and application-instance ID identify disposable runtime incarnations. They
never replace stable application or component identity in grants, handlers, update
policy, or stored user choices.

## Application identifier

Application identifiers should be reverse-domain-style, case-normalized, path-safe, and
stable across display-name or bundle-filename changes:

```text
org.nullstar.Settings
org.example.ImageEditor
com.galacticpirateradio.Player
```

The `org.nullstar.*` application namespace is reserved for components authorized by the
NullStar system release process. An administrator installing an ordinary third-party
package cannot authorize it to claim that namespace.

The identifier must not be derived from:

- the display name;
- bundle filename;
- installation directory;
- `.nspkg` filename;
- inner executable path;
- process ID;
- user-visible localized text.

## Publisher signing lineage

A publisher should be able to rotate signing keys without changing application identity,
but the transition must be authenticated.

```text
publisher key A
      | authorizes
      v
publisher key B
      | authorizes
      v
publisher key C
```

A lineage transition should include:

- old and new key identifiers;
- application, package, or publisher scope;
- monotonically increasing transition sequence;
- activation and optional expiration policy;
- authorization by the previous accepted key;
- proof of possession or acceptance by the new key;
- repository or system-policy acceptance where required.

An unexplained key change creates a different application principal. It must not silently
inherit:

- private application storage;
- persistent permission grants;
- document or URI defaults;
- application-group membership;
- background registrations;
- exported-service registrations;
- update authority;
- retained health status.

Emergency key compromise and revocation need explicit recovery policy. A repository or
system trust authority may revoke a compromised key, but replacement identity or data
inheritance must still be auditable rather than inferred from a matching display name.

## Bundle suffix and presentation

`.app` should be the canonical suffix for native application bundles:

```text
Example Editor.app
Ethereal Waves.app
System Settings.app
```

The file manager and application launcher may hide the suffix in normal presentation.
The suffix helps storage tools, package inspection, and compatibility code identify the
bundle type, but it does not prove validity, trust, identity, or executability.

A directory renamed to end in `.app` is not an installed application until its manifest,
content inventory, signatures, package record, and registration have been validated.

## Tentative bundle layout

A possible initial structure is:

```text
Example Editor.app/
├── manifest.toml
├── content.manifest
├── signatures/
│   ├── publisher.signature
│   └── lineage.record
└── Contents/
    ├── Executables/
    │   ├── editor
    │   ├── renderer
    │   └── thumbnail-worker
    ├── Libraries/
    ├── Resources/
    ├── Locales/
    ├── Icons/
    ├── Components/
    └── Metadata/
```

The exact names and capitalization remain tentative. The contract must preserve these
logical divisions even if the physical representation later becomes an indexed archive
or content-addressed projection.

### Semantic manifest

`manifest.toml` is the developer-authored semantic description. It declares:

- application identifier and display metadata keys;
- version, build, architecture, and platform requirements;
- publisher lineage and update identity;
- entry component and other process roles;
- sandbox profiles and requested services;
- permissions and usage-description keys;
- document types and URI schemes;
- exported services and extension points;
- application groups;
- background roles;
- private runtime and protocol dependencies;
- state-schema and rollback compatibility;
- install-scope restrictions.

The exact source syntax may remain TOML while the installed representation is compiled
into a canonical bounded record.

### Content manifest

`content.manifest` is a generated canonical inventory of every bundle entry. Each record
should identify at least:

```text
normalized relative path
entry type
size
content digest
executable status
architecture or data role
allowed metadata
```

The content manifest is not trusted merely because it is inside the bundle. Verification
recomputes the inventory and root digest before import or launch.

### Signatures

The signature directory contains cryptographic records needed to authenticate the
publisher and lineage. It must not contain mutable user grants, administrator policy, or
installation state.

### Contents

`Contents` contains immutable runtime material. No running application receives write
access to its own generation.

## Bundle path and link rules

All manifest and content paths must be normalized, relative, bounded, and confined to the
bundle root. Verification must reject:

- absolute paths;
- `..` traversal;
- repeated or ambiguous separators where canonicalization differs;
- case-colliding names on a case-insensitive backing store;
- device nodes, sockets, FIFOs, or other undeclared special files;
- unsupported ownership or mode metadata;
- entries whose extracted size exceeds declared or policy bounds;
- duplicate normalized paths;
- executable files not declared by a component or approved runtime role.

I recommend prohibiting symbolic and hard links in the first native bundle format. They
complicate canonical signing, extraction, ownership, and namespace confinement. A later
format may permit strictly internal symbolic links after defining canonical targets and
cycle handling.

## Bundle immutability

A selected application generation is immutable.

The application may receive:

- execute authority for its verified component executable;
- read-only access to its bundle resources and private libraries;
- no write authority to the bundle or containing application directory;
- separate capability-scoped configuration, data, state, cache, temporary, and runtime
  storage.

This prevents:

- partial self-updates;
- writable-library injection;
- helper replacement of the main executable;
- one user changing a machine application for another user;
- time-of-check/time-of-use replacement between verification and mapping;
- package-file rollback being confused with mutable data rollback.

The visible path such as `/Applications/Example Editor.app` should project the selected
immutable generation through the namespace or deployment service. It should not be a
conventionally writable directory copied into place.

The backing content-addressed store and generation paths are implementation details, not
application interfaces.

## Mutable application storage

Mutable content remains outside the bundle. A per-user application may receive logical
roles backed by the accepted profile categories:

```text
/Users/<user>/Profile/config/org.example.Editor/
/Users/<user>/Profile/data/org.example.Editor/
/Users/<user>/Profile/state/org.example.Editor/
/Users/<user>/Profile/cache/org.example.Editor/
/Users/<user>/Profile/runtime/<session>/org.example.Editor/
```

Applications should obtain directory capabilities from the runtime rather than
constructing these paths as authority. The physical location may change without altering
the application contract.

The categories mean:

- `config`: user preferences and durable configuration;
- `data`: durable application-owned content not presented as ordinary user documents;
- `state`: sessions, histories, databases, and operational state;
- `cache`: regenerable content;
- `runtime`: per-login sockets, locks, leases, and ephemeral files;
- temporary storage: separately bounded and cleared according to sandbox policy.

User-created documents selected through portals remain user documents, not private
application data, even when one application is their default editor.

## Package identity versus application identity

One ordinary application package should normally provide one primary application
identity. A suite of unrelated applications should use separate packages connected by an
explicit metapackage or application group.

Keeping them separate simplifies:

- permissions;
- uninstall choices;
- update rollback;
- private data ownership;
- default handlers;
- background policy;
- crash containment;
- signing and transfer of ownership.

Related localization, symbols, private runtime, or extensions may use separate package
identities while referencing the primary application identity through verified metadata.

## Conceptual manifest

A conceptual source manifest could look like this:

```toml
format = 1

[application]
id = "org.example.Editor"
name = "Example Editor"
version = "2.4.1"
build = 184
kind = "desktop"
entry_component = "main"
minimum_platform_abi = 3

[publisher]
lineage = "sha256:..."
update_identity = "org.example.Editor"

[sandbox]
profile = "desktop"

[uses]
required = [
    "ui.display",
    "portal.desktop",
    "storage.private",
    "settings.private",
]

declared = [
    "network.client",
    "notifications.publish",
]

[[permission]]
name = "audio.capture"
usage_description = "permission.voice-note-recording"
allowed_scopes = ["once", "while-in-use"]

[[component]]
id = "main"
executable = "Contents/Executables/editor"
role = "application-main"
profile = "desktop"
instance_policy = "per-user"

[[component]]
id = "renderer"
executable = "Contents/Executables/renderer"
role = "isolated-renderer"
profile = "desktop-child"
parent = "main"
uses = [
    "ui.render-surface",
    "application.component-ipc",
]

[[component]]
id = "thumbnail-worker"
executable = "Contents/Executables/thumbnail-worker"
role = "isolated-worker"
profile = "worker"
parent = "main"
uses = ["application.component-ipc"]

[[document_type]]
id = "org.example.Editor.document"
mime = "application/x-example-document"
extensions = ["example"]
role = "editor"

[[uri_handler]]
scheme = "example"
role = "viewer"

[state]
schema = 4
reads_from = [3, 4]
writes = 4
rollback_compatible_to = 3
```

This is illustrative rather than a stable schema. The installed parser must be
versioned, bounded, deterministic, and independent of Rust layout.

The verifier should reject:

- unknown required capabilities or service protocols;
- unauthorized sandbox profiles or restricted entitlements;
- missing or duplicate component identifiers;
- entry points outside the signed bundle;
- components whose requested authority exceeds package or profile policy;
- conflicting document or URI declarations;
- invalid publisher or update identity;
- state declarations that cannot express the required activation plan;
- unknown mandatory fields or unsupported major versions.

## Component roles

A modern application may contain multiple process roles:

```text
Application job
├── main process
├── renderer processes
├── media decoder
├── plugin host
├── GPU helper
├── background agent
└── crash reporter
```

Each role has a separate maximum profile and startup contract.

| Role | Typical authority |
| --- | --- |
| Main application | Lifecycle, windows, portals, approved documents, user-facing services |
| Renderer | Render surface and application-private IPC |
| Decoder | Input and output buffers plus bounded control IPC |
| Plugin host | Plugin resources, media buffers, and plugin protocol |
| Background agent | Declared background endpoint and narrow service access |
| Migration tool | Old-state input, staged-state output, logging, and deadline |
| Crash reporter | Crash record and approved reporting endpoint, not general process inspection |

A helper does not inherit the main process's complete handle table merely because both
executables are signed in one bundle.

Native spawn should name a declared component and provide an explicit startup-handle
allowlist. The application manager verifies that the parent is authorized to launch that
role and that transferred rights do not exceed the component contract.

## Signature layers

NullStar should distinguish three related but separate trust layers.

### Publisher signature

The publisher signs the application content and semantic manifest. This proves content
origin and continuity within the accepted publisher lineage.

It does not by itself grant:

- repository inclusion;
- system application status;
- a restricted entitlement;
- machine-wide installation;
- user permission approval.

### Repository signature

A Magnetar repository signs an immutable catalogue snapshot containing or referencing:

- package identifier and version;
- package and content hashes;
- publisher identity and accepted lineage;
- dependency and compatibility metadata;
- package kind and installation policy;
- entitlement approvals where repository policy participates;
- revocation, replacement, and channel information.

Mirrors reproduce the signed snapshot and package bytes. A mirror does not become a trust
anchor merely because it serves content quickly.

### System-release signature

Official system applications and services require authorization by the NullStar system
release process. This may authorize:

- reserved `org.nullstar.*` identities;
- `system-application`, `system-ui`, service, driver, recovery, or bootstrap profiles;
- specific restricted entitlements;
- inclusion in a system generation;
- independent recovery availability.

An ordinary publisher signature cannot claim these roles.

## Signature coverage

The publisher's canonical signed root should cover:

- semantic manifest;
- content manifest;
- every executable and private library;
- interpreted scripts or bytecode that can affect behavior;
- resources used to construct executable behavior;
- component and extension declarations;
- localization keys and permission-use descriptions;
- icons, names, and identity presentation metadata;
- file type, size, executable status, architecture, and relevant metadata;
- any permitted link target if links are introduced later.

Signing display metadata is important because changing the icon or name without changing
code can still spoof application identity in permission, launch, or update interfaces.

The format should carry algorithm identifiers and permit controlled algorithm migration.
Exact initial cryptographic algorithms belong to the package trust specification rather
than being assumed by directory layout.

## Installation domains

The accepted application locations retain distinct deployment scopes:

```text
/System/Applications
/Applications
/Users/<user>/Applications
```

### System applications

`/System/Applications` contains application generations selected as part of a verified
system generation. They are:

- immutable to ordinary users and administrators;
- authorized through system-release policy;
- rolled back with the relevant system generation when coupled to platform components;
- still launched through the application manager;
- still sandboxed.

### Machine applications

`/Applications` contains machine-wide application generations installed through
Magnetar with administrative authorization. The executable generation is shared, while
private data and permissions remain per-user unless an explicit machine service is
declared.

### User applications

`/Users/<user>/Applications` contains applications installed for one user. Per-user
installation should normally be the default for third-party desktop software. These
applications receive the ordinary application sandbox and cannot request system roles
merely because the user owns the files.

Location affects deployment ownership, visibility, update source, and removal policy. It
never grants runtime authority.

## Import before execution

A bundle found in a writable download directory, external volume, network share, or
source tree should not be mapped directly as a desktop application.

The application manager should offer two high-level operations:

```text
Install
Run Once
```

Both import and verify the content into an immutable generation before execution.

### Install

Installation creates a retained user, machine, or system application generation and an
application registration.

### Run Once

Run Once creates a transient immutable generation with normal sandboxing. It may be
removed after the last process exits unless retained by crash diagnostics, explicit pin,
or policy.

This closes the verification-to-execution race in which the original writable source
changes after inspection but before or during page loading.

The original path remains provenance metadata, not execution authority.

## Application registry

The desktop should not depend on repeatedly scanning application directories every time
it opens a launcher or resolves a document handler.

A trusted application registry should maintain an atomic index of:

- application and publisher identity;
- active, staged, previous healthy, and retained generations;
- display metadata and icon references;
- install scope and owning user where applicable;
- trust class, provenance, and update source;
- document types and URI schemes;
- exported services and extension points;
- background roles and application groups;
- sandbox profile and restricted entitlement set;
- state schema and migration status;
- readiness, crash-loop, quarantine, and rollback state.

Magnetar updates this registry as part of generation activation. Registration and active
generation selection must commit atomically so the launcher never sees a half-updated set
of handlers and content.

The registry supports:

```text
application launcher
search
Open With
file and URI dispatch
default-application settings
permission and privacy settings
background-application settings
notification attribution
service activation
uninstallation
crash recovery
```

The presence of a directory does not itself register an application.

## Typed application activation

Applications should receive typed lifecycle messages instead of unstructured path
arguments as the native desktop contract.

Initial activation operations should include:

```text
Activate
OpenDocuments
OpenUris
NewWindow
RestoreSession
ContinueActivity
PrintDocuments
Quit
```

An `OpenDocuments` request may conceptually include:

```text
document capability
stable grant or transfer identity when applicable
display name
content type
approved rights
originating user gesture or portal request
activation identifier
```

The receiving application does not need ambient access to the containing directory. A
compatibility runtime may synthesize paths inside the application's private projected
namespace when required by a ported program.

Activation messages are sent through the application lifecycle endpoint owned by the
application manager or session manager. They are versioned, bounded, cancellable where
appropriate, and associated with the selected application generation.

## Instance policy

A component manifest should declare an instance policy such as:

```text
multiple
per-user
per-session
per-document
single-system
```

Ordinary graphical applications will usually use `per-user` or `multiple`.

When policy reuses an existing instance, the application manager sends a new activation
message rather than executing an arbitrary second process. The target instance must be
ready, belong to the correct user and session, and run a generation compatible with the
activation contract.

An old generation should not automatically receive an activation intended only for a
new incompatible generation. Update policy may:

- continue routing to the old instance until it exits;
- request an orderly restart;
- start a parallel new-generation instance when state rules permit;
- defer activation of the new generation.

## Document-type registration

An application may declare its roles for a document type:

```text
viewer
editor
creator
importer
exporter
printer
```

A declaration makes the application eligible to appear in Open With or related UI. It
does not make the application the default.

The registry should preserve separate concepts for:

- user-selected default;
- administrator-managed default;
- system recommendation;
- application capability;
- last-used choice;
- one-time user selection.

A new installation or update must not silently take over common types, HTTP links, email
links, archive formats, scripts, package files, or other sensitive handlers.

File extensions are compatibility and presentation hints. Native dispatch should prefer a
verified content-type decision and explicit user action where ambiguity matters.

## URI-scheme registration

URI declarations should identify:

- normalized scheme;
- role and supported operations;
- whether the application accepts external untrusted input;
- activation component;
- expected protocol version;
- whether confirmation is required for sensitive schemes.

Reserved or security-sensitive schemes require system policy. A package cannot claim
system configuration, package installation, authentication, or trusted desktop schemes
through an ordinary manifest field.

## Dependency model

Applications should depend primarily on:

1. stable NullStar platform-library ABIs;
2. versioned service protocols;
3. private runtime packages resolved into the application generation.

They should not depend on arbitrary mutable global library filenames.

```text
Example application generation
├── Example Editor.app
├── private runtime generation A
├── private library generation B
└── required platform and service protocol versions
```

Identical immutable runtime objects may be deduplicated in the package store without
creating mutable global library state.

### Service protocol requirements

A manifest should be able to declare requirements such as:

```toml
[[requires_protocol]]
name = "system.display"
major = 1
minimum_minor = 3
features = ["fractional-scale"]

[[requires_protocol]]
name = "system.media"
major = 2
minimum_minor = 0
```

Magnetar checks whether the target deployment can satisfy the declared requirements.
Runtime feature negotiation confirms the actual endpoint version. A presentation-level
minimum OS version may still exist, but it should not be the only compatibility check.

### Private runtimes

An application may depend on private runtimes selected with its generation. Private
runtime content is immutable, verified, architecture-specific where needed, and subject
to normal provenance and vulnerability auditing.

A private runtime does not bypass the application's sandbox or gain a separate ambient
service namespace.

## Application update transaction

An update should follow this sequence:

1. resolve one immutable repository snapshot or verified local package set;
2. compute and present the complete dependency and installation plan;
3. download every required package object;
4. verify package, publisher, repository, and lineage signatures;
5. verify canonical content and manifest compatibility;
6. construct a complete new application generation;
7. compare identities, profiles, entitlements, permissions, handlers, background roles,
   and exported services with the active generation;
8. evaluate state-schema and migration requirements;
9. register the generation as staged;
10. atomically select it for eligible new launches;
11. observe readiness and bounded early health;
12. mark the generation healthy, failed, quarantined, or pending migration;
13. retain the previous healthy generation according to rollback policy.

No step modifies the active bundle in place.

## Running processes during update

A running process continues to use the immutable generation from which it was launched.
The content-addressed store and generation reference remain retained until every running
process, debugger, crash record, rollback pin, and recovery reference releases them.

New launches normally use the newly selected generation.

The application manager may request an orderly restart when:

- a security update requires process replacement;
- a background component must switch generation;
- state migration forbids concurrent old and new access;
- a service or exported protocol changes incompatibly;
- the user selects Restart to Update.

NullStar should not inject replacement executable code into a live process.

## Permission and entitlement changes

An update never gains authority merely because it has the same application identifier.
The application manager compares old and new policy inputs before activation.

### New optional portal

Adding support for Open, Save, Print, Share, or another user-action portal does not create
persistent authority. The application becomes eligible to invoke the portal when the
user requests the feature.

### New sensitive runtime permission

A new microphone, camera, screen-capture, USB, local-network, or similar declaration may
become requestable, but remains ungranted until current prompt and policy requirements are
satisfied.

### New declared static capability

A newly added static request such as outbound network access should initially be withheld
unless user or administrator policy explicitly applies. The application may launch with
the dependent feature disabled rather than forcing installation-time approval.

### Restricted entitlement change

Adding or broadening a restricted entitlement blocks activation until the appropriate
trusted package, system, or administrator policy authorizes it. A publisher cannot gain
`runtime.jit`, `network.raw`, `accessibility.control`, `package.install`, or system
configuration authority by editing its manifest.

### Sandbox profile change

Moving from an ordinary desktop profile to a system application, service, driver,
recovery, trusted UI, or broader compatibility profile is a trust-significant transition.
It is not an ordinary update and must satisfy the destination profile's signing and
installation policy.

### Handler and background expansion

New default-handler eligibility, exported services, login-start roles, or persistent
background behavior should be surfaced separately from ordinary code changes. Existing
user defaults and background choices remain intact unless policy explicitly changes
them.

## State schema declarations

An application that writes durable state should declare:

- current schema version;
- oldest readable schema;
- schema written by the new version;
- rollback-compatible versions;
- whether migration is reversible;
- whether old and new generations may access the state concurrently;
- which categories or databases require migration;
- required free space and snapshot policy where known.

Example:

```text
Generation 41:
    reads schemas 3 through 4
    writes schema 4

Generation 42:
    reads schemas 4 through 5
    writes schema 5
    rollback to generation 41 requires restoring a schema-4 snapshot
```

Magnetar and the application manager must not advertise full rollback when only the
immutable executable generation can be restored.

## Sandboxed state migration

A migration is a declared component, not an unrestricted package-install script.

It should run with:

- read-only capability to the old state or selected migration inputs;
- write capability to a new staged state area;
- no network by default;
- no unrelated filesystem or service authority;
- bounded memory, CPU, process count, and time;
- structured logs and progress reporting;
- an explicit cancellation and uncertain-outcome contract;
- validation before commit.

A preferred migration pattern is:

```text
existing state
      | read-only
      v
sandboxed migration component
      |
      v
new staged state
      | validate
      v
atomic state-generation or database switch
```

Not every preference-file edit requires a full snapshot system. The migration contract
may select an appropriate mechanism per state category, but destructive migrations must
make rollback consequences explicit.

## Application rollback

Magnetar should retain at least one previous healthy application generation unless
storage policy, explicit removal, or pin rules say otherwise.

Code rollback restores:

- application bundle generation;
- private runtime graph;
- manifest and registration inputs;
- handler and exported-service declarations;
- entitlement version;
- associated immutable resources.

Mutable state is restored only when a compatible state version or snapshot exists.
Management UI must distinguish:

```text
code rollback available
full code-and-state rollback available
rollback requires state conversion
rollback unsafe because state was irreversibly migrated
```

Rollback must not silently re-enable a revoked permission, background role, or restricted
entitlement merely because the older manifest once requested it.

## Crash-loop detection and health

The application manager should observe whether a new generation reaches readiness and
remains healthy for a bounded initial period.

```text
launch new generation
       |
       v
crash before ready
       |
       v
bounded retry or diagnostic launch
       |
       v
repeated early failure
       |
       v
quarantine generation and offer previous healthy version
```

For ordinary applications, recovery choices may include:

- launch the previous healthy generation;
- retry the current generation;
- start without third-party extensions;
- reset only temporary or cache state;
- inspect crash information;
- repair or reinstall immutable content.

Essential system applications may use stricter automatic fallback or a recovery
implementation, but application failure should not require rolling back unrelated system
packages when an independent application generation is sufficient.

## Extensions and plugins

An extension should normally be a separately signed and registered component with its own
stable identity. Its manifest should identify:

```text
extension identity and publisher
host application or protocol
extension point and version
component role
requested capabilities
enablement and update policy
```

The preferred model is:

```text
host application
      | narrow typed IPC
      v
extension-host job
      |
      v
extension component
```

The extension does not inherit host document handles, network access, private storage,
microphone, clipboard, or account authority unless the host or a portal deliberately
delegates a reduced capability.

In-process native-code extensions may exist as a compatibility or performance mode, but
the system should present them as running with the host's complete process authority.
They should not be the default for untrusted third-party extensions.

## Application groups

Applications from the same publisher do not automatically share data or permissions.
A shared application group requires:

- explicit stable group identifier;
- compatible publisher lineage;
- declaration by every member;
- package-policy approval;
- defined shared resources and protocols;
- user or administrator approval where appropriate.

A group may provide:

```text
shared private directory
shared database service
group-only IPC namespace
shared credential domain
shared background agent
```

Each application remains separately identifiable for permissions, network policy,
handlers, notifications, crash reports, background behavior, updates, and uninstall.
The group does not become one broad sandbox.

## Background components

A bundle may declare user-visible background roles such as:

```text
sync
media playback
download or upload
device session
status item
scheduled maintenance
document indexing
```

Declaration permits registration; it does not permanently enable the role.

Each background component should define:

- component identity and executable;
- user-visible reason;
- activation conditions;
- expected duration or persistence;
- resource limits and scheduling class;
- required services and capabilities;
- stop, cancellation, and restart behavior;
- relationship to the foreground application;
- whether it starts at login.

System Settings should expose enabled background applications. There should be no native
mechanism equivalent to dropping an arbitrary executable into an autostart directory and
receiving the user's complete session authority.

## Application-provided services

A bundle may export a versioned service through the application broker. The declaration
should identify:

- protocol name and version;
- serving component;
- activation and instance policy;
- visibility class;
- accepted caller classes or user selection;
- resource and background policy;
- whether the service survives loss of foreground windows.

Exporting a service does not give the provider more authority. Callers pass scoped
resource capabilities where needed, and the broker supplies authenticated caller context
without making identity equivalent to authority.

## Developer mode

Developer mode should make application development practical without collapsing the
production trust model.

Recommended behavior includes:

- create or select a stable local development signing key;
- assign a local developer lineage;
- permit installation of development packages and rapid generation replacement;
- mark development applications visibly in launch, permission, and crash UI;
- preserve the ordinary sandbox by default;
- grant debugger, source-tree, toolchain, or JIT authority only through explicit
  development policy;
- permit permission reset and transient test grants;
- prohibit claiming reserved system identities;
- prohibit system-only entitlements without separate trusted authorization.

A development build does not silently inherit a production application's private data or
persistent grants unless an explicit authenticated identity transition allows it.

Unsigned bare executables may run in a developer or compatibility environment, but an
inner executable from an application bundle must not bypass the application manager when
launched as a desktop application.

## Installation experience

Opening an `.nspkg` should invoke a trusted installation interface that obtains package
information from Magnetar rather than parsing and executing the package in the UI process.
It should present at least:

```text
application name and icon
verified publisher and signing state
version and update source
installation scope
download and installed size
background components
network declaration
sensitive permissions the app may request
restricted entitlements
dependencies
provenance and repository status
rollback or replacement impact
```

Per-user installation should normally be the default. Machine-wide installation requires
operation-specific administrative authorization. System-generation changes belong in
the system update interface rather than masquerading as ordinary application installs.

The installer remains sandboxed. It submits a narrow transaction request to Magnetar and
does not receive general write authority to `/Applications`, `/System`, or the package
store.

## Provenance

Application registration should retain installation provenance such as:

```text
trusted Magnetar repository
verified local package
browser download
removable volume
enterprise source
developer build
system generation
```

Provenance assists first-launch presentation, update-source restrictions, auditing,
incident response, and repository replacement detection. It does not itself grant
runtime capability authority.

A valid publisher-signed package installed outside a configured repository may be
allowed by policy and clearly labeled as locally installed. NullStar need not require one
centralized notarization service for all third-party applications; publisher signatures,
repository trust, local policy, immutable generations, and mandatory sandboxing form the
core model.

## Update source and ownership

An installed application should remember which authority may provide updates:

- the current trusted repository channel;
- an authenticated publisher lineage;
- a system generation;
- an enterprise deployment authority;
- a local developer identity;
- explicit user-selected replacement.

A different repository may offer an application with the same identifier only when
publisher lineage and replacement policy permit it. Repository priority alone must not
authorize application takeover.

Transferring an application to a new publisher is a security-significant ownership
transition. It requires authenticated lineage or an explicit replacement flow that
clearly separates the new principal from the old application's grants and private data.

## Removal and data retention

Removing an application should atomically deactivate its registration and revoke or
remove:

- background registrations;
- exported services;
- file and URI handler eligibility;
- dynamic device and capture grants;
- application-specific network exceptions;
- application-group membership;
- scheduled tasks and pending activations;
- update-channel registration.

The user should receive separate choices such as:

```text
remove application
remove application and cache
remove application and all private data
remove application, private data, and permission history
```

User-created documents remain untouched unless they are explicitly stored inside private
application data and the user selects its removal.

Immutable generations and package objects are physically collected only when no active
or retained generation, running process, rollback pin, debugger, crash record, recovery
reference, or policy reference still requires them.

## Management interfaces

Magnetar remains responsible for package objects and deployment generations:

```text
mag install
mag upgrade
mag remove
mag generations
mag rollback
mag verify
mag audit
mag repair
mag gc
```

A native application-management client may expose registration and runtime state:

```text
appctl list
appctl info <application>
appctl launch <application>
appctl instances <application>
appctl handlers
appctl defaults
appctl permissions <application>
appctl background <application>
appctl generations <application>
appctl reset <application>
```

The `appctl` name is tentative. These commands are service clients. They do not register
applications by editing directories, replace bundle files, or manage instances by
searching for process IDs.

## Failure and uncertain outcomes

Installation, migration, activation, and removal protocols must define uncertain
outcomes explicitly.

Examples include:

- a generation record durably committed but the activation reply was lost;
- migration output committed but health confirmation did not complete;
- the old process exited while restart activation was canceled;
- removal deactivated registration but garbage collection did not finish.

Clients must query transaction or generation identity after uncertainty rather than
blindly replaying a non-idempotent operation. Magnetar, the registry, and application
manager should expose stable transaction records sufficient to determine the selected
generation and remaining cleanup work.

## Observability and audit

Application lifecycle records should include:

```text
transaction identity
application, package, publisher, and generation identity
install scope and initiating user or service
source repository or local provenance
signature and lineage result
old and new permission declarations
entitlement and profile changes
state migration plan and outcome
activation, readiness, crash, quarantine, and rollback decisions
removal and data-retention choices
```

Logs and audit records must not contain document contents, private keys, permission
tokens, raw capabilities, or unrelated application data.

The desktop should be able to explain:

- which generation is active and why;
- which publisher and repository supplied it;
- whether a previous healthy generation is retained;
- why an update is blocked;
- whether rollback includes mutable state;
- which new permissions or background roles were introduced;
- why an application was quarantined.

## Relationship to compatibility metadata

Freedesktop desktop entries, MIME declarations, AppStream-style metadata, D-Bus service
files, and autostart records may be imported or projected for compatibility. They must not
become the native authority for application identity, sandbox policy, installation,
service activation, or background execution.

Compatibility import should translate validated metadata into the application registry
under the compatibility application's verified identity. Conflicting or privilege-bearing
fields are ignored, constrained, or routed through native user and policy decisions.

## Recommended implementation stages

### Stage 1: Bundle and identity foundation

- define the `.app` bundle type and tentative internal layout;
- define canonical application, component, and generation identifiers;
- define the semantic and content manifests;
- reject unsafe paths, special files, and unsupported links;
- provide stable local developer identity and strict bundle verification;
- launch from read-only bundle-root capabilities;
- keep mutable storage outside bundles.

### Stage 2: Application registry and activation

- store active generation, install scope, identity, display metadata, and provenance;
- add atomic registration transactions;
- add typed `Activate`, `OpenDocuments`, and `OpenUris` messages;
- add instance policies and lifecycle readiness;
- add document-type and URI eligibility without automatic default takeover;
- add launcher and Open With integration.

### Stage 3: Magnetar application generations

- import immutable bundle and private runtime objects;
- construct complete application generations;
- atomically switch the generation selected for new launches;
- retain the previous healthy generation;
- retain old content while old processes execute;
- add generation-aware application garbage collection.

### Stage 4: Signing and trust

- verify publisher signatures and canonical content roots;
- verify repository snapshots and update-source ownership;
- reserve NullStar system identities;
- implement signing-key lineage and revocation policy;
- distinguish publisher, repository, and system-release authorization;
- record provenance and trust transitions.

### Stage 5: Update and state safety

- compare permission, entitlement, handler, service, and background declarations;
- withhold newly requested authority;
- add state-schema declarations and rollback reporting;
- run bounded migration components against staged state;
- add readiness health checks, crash-loop quarantine, and previous-version launch;
- represent uncertain activation and migration outcomes through transaction identities.

### Stage 6: Extensions and background roles

- add declared extension points and isolated extension hosts;
- add plugin and helper component contracts;
- add explicit application groups;
- add application-provided services;
- add user-controlled background component registration;
- add safe mode without third-party extensions;
- add application update channels and pins.

## Required invariants

> Application identity is the combination of a stable application identifier and
> authenticated publisher lineage. A path, filename, display name, process ID, or package
> archive name is never an application identity.

> Installed application content is selected through immutable generations. Applications
> never update themselves by modifying their active bundle.

> Magnetar verifies complete package inputs and constructs a complete generation before
> activation. No update modifies an application incrementally in place.

> A publisher signature proves content origin and continuity. A repository signature
> authorizes distribution. A system-release signature authorizes reserved identities,
> system roles, and restricted entitlements.

> Application updates may expand declarations but cannot silently create new runtime
> authority, stronger profiles, restricted entitlements, background behavior, or default
> handler selection.

> Running processes continue using the immutable generation from which they were
> launched. New generation selection affects new launches rather than rewriting live
> process code.

> Application configuration, state, cache, runtime content, and user documents remain
> separate from the immutable bundle and have explicit migration and rollback contracts.

> Helpers, extensions, plugins, background agents, crash tools, and migration tools are
> declared components with explicit sandbox profiles and startup handles.

> Writable source locations are never desktop execution roots. Install and Run Once both
> import content into verified immutable generations before mapping executable pages.

## Open questions

- The exact bundle directory capitalization and whether installed bundles remain ordinary
  projected directories or use a dedicated indexed representation.
- The canonical semantic-manifest source format and compiled installed representation.
- The canonical content-tree hashing and signature formats.
- Initial publisher-key algorithms, repository trust roots, and revocation distribution.
- Whether user-installed application generations use the same physical store as
  machine-wide objects with ownership-separated metadata.
- The exact application-registry service boundary and durable database representation.
- The first state-snapshot mechanism and how fine-grained application data generations
  should be.
- Whether `appctl` is the final native application-management command name.
- How long previous healthy application generations should be retained by default.
- Which compatibility metadata can be imported automatically without presenting an
  application-installation confirmation.
