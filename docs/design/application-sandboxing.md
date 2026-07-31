# Capability-based application sandboxing

## Status

- **Accepted direction:** every process launched as an application is sandboxed.
- **Accepted direction:** installation location never disables the sandbox.
- **Accepted direction:** capabilities are the enforcement mechanism; manifests,
  entitlements, and portals are policy inputs rather than authority by themselves.
- **Tentative design:** application bundles use a stable signed identity and a declarative
  manifest interpreted by an application manager.
- **Distant goal:** persistent grants, revocation, user-facing permission management, and
  isolated plugin hosts.

This document records the intended sandbox and permission model for graphical and
bundle-based applications. It describes future architecture rather than currently
implemented behavior. The current capability and IPC foundation is documented in
[`../protection-model.md`](../protection-model.md).

## Core rule

NullStar OS should apply one consistent rule to application launches:

> Being installed grants permission to be discovered and launched, not permission to
> access the rest of the system.

Every process launched through the application runtime receives an explicit initial set
of capabilities. The process cannot name or use a resource for which it has not received
an appropriate capability.

Application location determines installation scope, ownership, update policy, and which
classes of entitlements may be considered. It does not determine whether the application
is sandboxed.

## Application locations

| Location | Scope and management | Sandbox policy |
| --- | --- | --- |
| `/System/Applications` | Operating-system supplied and updated by the trusted system updater | Mandatory |
| `/Applications` | Machine-wide applications installed by an administrator or package service | Mandatory |
| `/Users/<user>/Applications` | Applications installed and managed by one user | Mandatory |

A system application may be eligible for narrowly scoped system entitlements, but it
must not receive a general sandbox bypass. A user-owned application must not inherit the
user's full authority merely because its bundle resides inside the user's home tree.

The application manager should resolve bundles through a trusted package or namespace
service rather than treating a pathname as proof of identity or trust.

## Trust must not be inferred from path

A bundle path is mutable metadata. It is not a security principal. Binding authority to a
path would permit replacement, symlink, mount, and identifier-confusion attacks.

Application identity should instead be derived from verified properties such as:

- application identifier;
- publisher or system signing identity;
- package signature or verified content identity;
- installation provenance and application class;
- approved entitlement set;
- current user and system policy.

Persistent grants should be bound to the application identity and user identity, not only
to the bundle pathname. An application whose signing identity changes unexpectedly should
not silently inherit the previous application's grants.

## Application classes

### System application

A system application is supplied by the operating-system image or trusted system updater,
is signed by a NullStar system key, and is immutable to ordinary users.

It remains sandboxed. Approved system-only entitlements may be translated into explicit,
narrow capabilities or service endpoints.

### Machine application

A machine application is installed for all users by an administrator or machine package
service. It receives the ordinary application sandbox and user-specific grants when each
user launches it.

### User application

A user application is installed and managed by one user. It receives the same mandatory
application sandbox as a machine application and is not eligible for privileged system
entitlements merely because the user owns its files.

### Development application

Developer mode may relax signing and installation requirements, issue a temporary local
development identity, and grant explicit debugging facilities. It should not implicitly
disable the sandbox. Broader testing authority must remain a deliberate, visible, and
revocable user action.

## Launch sequence

The desktop shell should not execute an application bundle directly. It should ask the
application manager to construct a sandbox and launch the application:

1. resolve the selected bundle and installation class;
2. verify its manifest, application identity, signature, and package integrity;
3. evaluate its requested profile and entitlements against system and user policy;
4. create a process, job, resource-accounting domain, and process-local capability table;
5. install the minimum baseline capabilities;
6. restore only valid persistent grants for the current user and application identity;
7. launch the executable through the application runtime;
8. supervise lifecycle, revocation, resource limits, and crash reporting.

Direct execution of a bundle executable must not bypass this sequence. The loader or
application manager should distinguish a desktop application launch from a shell or
service-manager process launch.

## Baseline capability set

A normal application should begin with only the capabilities required to exist as an
application, for example:

- execute authority for its selected executable;
- read-only access to its own bundle and resources;
- read-write access to its private persistent data directory;
- read-write access to its private cache and temporary directories;
- a display-session endpoint that can create and manage only its own surfaces;
- delivery of input events addressed to its own surfaces;
- basic clocks, timers, logging, and process-exit facilities;
- a restricted application-broker endpoint for portal requests;
- explicitly restored user grants.

The baseline must not include ambient access to:

- the user's home directory;
- other application bundles or private data;
- arbitrary devices;
- camera, microphone, or screen contents;
- unrestricted network access;
- other processes or windows;
- global clipboard contents;
- privileged system services.

## Bundle access and private storage

An application receives a read-only capability rooted at its own bundle, not a capability
for the containing applications directory. This prevents ordinary enumeration or
modification of other installed applications.

Application installation, replacement, update, and removal are operations of a package
service with separate authority. Running applications should not modify themselves or
other bundles.

Writable storage should be supplied as explicit directory capabilities with stable
logical roles such as application data, cache, and temporary storage. Physical paths may
remain an implementation detail. One application's private directories must not be
reachable by another application without explicit delegation or a portal-mediated user
action.

## Capability properties

Application sandboxes build on the general capability model:

- handles are process-local and unforgeable;
- every handle carries explicit object-specific rights;
- duplicated or transferred authority may only be attenuated;
- authority is transferred explicitly through IPC or child bootstrap;
- resource use is bounded and accounted;
- revocable resources fail cleanly after revocation;
- possession of one capability does not imply access to related global namespaces.

Manifests and entitlements do not themselves grant authority. They declare what an
application may request or what the application manager may consider when constructing
the initial capability table.

## Application manifests

A bundle should contain a declarative manifest describing identity, executable entry
points, sandbox profile, helpers, and requested facilities. A possible form is:

```toml
[application]
id = "org.example.image-editor"
name = "Image Editor"
version = "1.0.0"
executable = "bin/image-editor"

[sandbox]
profile = "document-app"

[requests]
network = "none"
audio_output = true
audio_input = false
camera = false
notifications = true
clipboard = "user-mediated"

[portals]
open_file = true
save_file = true
open_directory = true

[process]
allow_children = true
child_profile = "inherit-reduced"
```

A declaration such as `audio_input = true` means the application may ask the relevant
portal. It does not mean the application begins with microphone authority.

Unknown manifest fields should not silently grant authority. Versioning and strict
validation are required before manifests become a stable package interface.

## Portals and the application broker

Applications should request privileged or user-mediated operations through narrow,
versioned portal protocols. A portal may perform an operation on the application's behalf
or return an attenuated capability.

Expected portals include:

- open-file, save-file, and directory-selection portals;
- clipboard and drag-and-drop portals;
- open-URI and application-launch portals;
- notifications and printing;
- camera, microphone, and screen-capture portals;
- network and local-network policy portals;
- secrets, credentials, and authentication portals;
- device-selection portals.

The application broker must not be a universal privileged endpoint. It should expose
narrow service discovery and request operations, enforce manifest and policy restrictions,
and return only capabilities approved for the requesting application.

## User-selected files and directories

Applications should not need ambient access to user storage. The file portal presents a
system-owned picker, and the user's selection causes the portal to return authority for
only the selected object.

Opening an existing file may return a `READ` or `READ | WRITE` file capability. Saving a
new file may cause the portal to create the destination and return a capability for that
file. Selecting a project directory may return a directory capability rooted at that
subtree.

The application must not be able to escape a selected directory by spelling parent paths.
Lookup is relative to the directory capability and remains constrained to the represented
subtree.

## Persistent grants

The permission service may remember an approved file, directory, device, or service grant
for future launches. The stored record is authorization to recreate a capability, not a
serialized live handle.

A persistent grant should include at least:

- application identity;
- user identity;
- stable resource identity;
- granted rights and scope;
- grant origin and user decision;
- expiry or revocation state where applicable.

Before restoring authority, the application manager must revalidate the application and
resource identities, current policy, and revocation state. Sensitive grants may require
renewed user confirmation after application identity or publisher changes.

## System entitlements

System applications may need facilities unavailable to third-party applications, such as
package management, account administration, accessibility provision, or specific settings
operations.

These must be represented as narrow signed entitlements, for example:

```toml
[entitlements]
system.package-management = true
system.settings.network = true
```

An entitlement is valid only when all of the following hold:

- the bundle has the required application class and signing identity;
- the entitlement appears in an approved allowlist for that identity;
- the relevant policy service accepts it;
- the entitlement is translated into explicit capabilities or narrow service endpoints.

There must be no generic `sandbox = false`, `trusted = true`, or equivalent entitlement.
A network settings application may receive a configuration endpoint with selected rights;
it must not receive unrelated filesystem, process, device, or kernel authority.

## Privileged services remain separate

User-facing system applications should not embed unsandboxed privileged helpers. A
privileged operation should be implemented by a separately managed service with its own
minimal service sandbox.

For example:

```text
/System/Applications/NetworkSettings.app
        |
        | narrow versioned IPC capability
        v
/System/Services/NetworkConfigurationService
        |
        | typed configuration and device capabilities
        v
Kernel and driver services
```

The application remains low privilege even when its associated service performs a
privileged operation. The service validates every request and exposes only the authority
required by its protocol.

## Network authority

Applications should not receive a generic network namespace by default. Network access
may be represented by profiles or by narrower brokered capabilities, including:

- no network;
- outbound internet client;
- selected hosts or services;
- local-network access;
- listening socket authority;
- custom enterprise or user policy.

Listening, local-network discovery, and unrestricted outbound access are distinct
permissions. The network service may enforce address classes, protocols, ports, domain
policy, proxies, and transport requirements without exposing raw device authority.

## Media and device authority

Applications consume media through graph and portal capabilities, not raw global device
access.

An audio player may receive a playback-stream capability. A recording application may
receive a temporary capture-stream capability for a user-selected source. Camera and
screen-capture access follow the same model. Device enumeration and persistent device
access require separate policy decisions.

The media service should expose format negotiation and conversion without requiring the
application to acquire the underlying hardware capability.

## Clipboard and drag-and-drop

Clipboard reads should normally be coupled to a foreground user paste action. The
compositor or clipboard portal may issue a temporary read capability or perform a bounded
transfer, preventing background applications from continuously observing copied data.

Clipboard writes may be separately authorized. Sensitive clipboard entries may carry
short lifetimes or non-persistence policy.

Drag-and-drop is an explicit capability transfer caused by a user gesture. Dropping a file
or directory into an application transfers only the selected object's approved rights; it
does not grant the destination application access to the containing namespace.

## Child processes and helpers

A child process must not receive all parent capabilities through ambient inheritance.
The parent or application runtime should construct a child capability table from an
explicit, rights-reduced subset.

Expected child profiles include:

- `inherit-none`;
- `inherit-reduced`;
- `declared-helper`;
- `isolated-worker`.

This supports isolation of decoders, renderers, archive extractors, scripting engines,
and other components that process untrusted input. Helpers declared in the bundle must
still have stable identities and explicit launch contracts.

## Plugins

Plugins should not automatically inherit the host application's complete sandbox. The
preferred model is an isolated plugin-host process that receives only the capabilities
required for the plugin protocol, such as shared media buffers, control parameters,
timing information, and read-only plugin resources.

For example, a future LV2 host may delegate audio buffers and control ports without
granting the plugin network, user-document, microphone-selection, or host-private-storage
authority.

In-process plugin execution may exist as a compatibility or performance mode, but it must
be presented as a deliberate reduction in isolation rather than the default security
model.

## Shell and service launches

Mandatory application sandboxing applies to processes launched through the application
runtime. It does not imply that all executable files use the same baseline policy.

A shell-launched command receives authority according to explicit shell and process
inheritance rules. A service-manager launch receives a declared service sandbox and
service-specific capabilities. These paths must not allow an application bundle to bypass
the application manager simply by invoking its inner executable directly.

The launch mechanism, verified identity, and policy domain determine authority; the
executable's pathname alone does not.

## Standard profiles

The application manager may provide named profiles to establish safe defaults, such as:

- `desktop-basic`;
- `document-app`;
- `media-player`;
- `communication`;
- `browser`;
- `game`;
- `development`;
- `service-client`.

Profiles are templates, not coarse privilege levels. The resulting authority remains a
set of explicit capabilities. Applications with the same profile may receive different
user grants and resource limits.

## Kernel and userspace responsibilities

The kernel should enforce:

- capability-table isolation and handle validity;
- object-specific rights masks;
- attenuation, delegation, and transfer rules;
- process, job, IPC, and memory protection;
- resource limits and object lifetime;
- revocation primitives when implemented.

Userspace policy services should decide:

- whether a package and manifest are valid;
- whether an entitlement is accepted;
- whether and how the user is prompted;
- which grants persist or expire;
- which network, media, device, and storage scopes are approved;
- which application handles a document type or URI.

Policy decisions must result in concrete kernel-enforced authority. A userspace permission
record that is not reflected in the process's capabilities is not a complete sandbox.

## Recommended invariant

The architecture should preserve this invariant as application support evolves:

> Every process launched as an application is sandboxed. Installation location never
> disables the sandbox. System-supplied applications may receive additional signed,
> narrowly scoped entitlements, but those entitlements are translated into explicit
> capabilities rather than unrestricted authority.
