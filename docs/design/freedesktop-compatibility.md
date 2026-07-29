# Freedesktop compatibility direction

## Status

Freedesktop formats and protocols are an **accepted compatibility target** for the
NullStar desktop. They are not the native kernel, IPC, package, authorization, or
desktop-service architecture. Exact supported versions and the implementation order
remain **tentative design**.

## Compatibility principle

NullStar should adopt freedesktop data formats and user-facing conventions when they
make application porting and toolkit integration easier. Interfaces that expose
privileged operations, global desktop state, devices, or another application's data
must translate through native capabilities, services, and trusted portals.

Support is divided into three classes:

1. **Native-compatible data**: formats and names that can be consumed directly without
   weakening the system model.
2. **Protocol compatibility**: an external protocol implemented faithfully over native
   service objects.
3. **Brokered translation**: an external request is mapped to a native portal or policy
   decision and may expose less authority than it would on another system.

The compatibility layer must publish version and feature information explicitly.
Applications should not infer support merely from an environment variable.

## XDG base directories and `Profile`

The accepted `Profile` layout maps cleanly to XDG categories:

```text
XDG_CONFIG_HOME  => /Users/<name>/Profile/config
XDG_CACHE_HOME   => /Users/<name>/Profile/cache
XDG_STATE_HOME   => /Users/<name>/Profile/state
XDG_DATA_HOME    => /Users/<name>/Profile/data
XDG_RUNTIME_DIR  => session-scoped binding below Profile/runtime
```

The runtime directory must retain session semantics: it is private to the user,
belongs to one active login environment, supports sockets and other IPC objects, and
is cleared when the session ends. `Profile/runtime` is a category root, not a promise
that stale runtime state survives reboot.

Native applications should ask the runtime for application-scoped directory
capabilities. Ported applications receive compatibility paths and `XDG_*` variables.

System-level compatibility data can be projected from `/System` and application
packages into XDG search paths without making `/usr`, `/etc`, or other Unix paths the
native layout.

## Desktop entries and application registry

NullStar should import validated desktop-entry files for application name, icon,
launch actions, categories, localization, content types, and compatibility visibility.
The package manager or application registry associates each imported entry with a
verified package and application identity.

The shell must not blindly execute arbitrary `Exec` text from an untrusted file.
Launching resolves the desktop entry through the application registry, validates
field codes and resources, then asks the native launcher to create an application job
with its declared policy.

Native package manifests remain authoritative for capabilities, sandboxing, services,
and resource policy. Desktop entries are a compatibility description, not a privilege
manifest.

## MIME and content types

The shared MIME-info vocabulary is the preferred initial compatibility vocabulary for
content types. A native content service should support:

- extension and content-signature detection;
- inheritance between content types;
- localized descriptions and icons;
- default and alternate applications;
- safe preview and thumbnail handlers;
- user, administrator, and packaged association layers.

`mimeapps.list` behavior may be exposed for ported applications, while native settings
are stored through the application/content registry. Conflicting or malformed records
must not launch an arbitrary executable outside package policy.

## Icon themes and names

The desktop should support freedesktop icon-theme lookup and common icon names so
ported GTK, Qt, COSMIC, and other applications can find familiar assets.

NullStar's default themes may use SVG-first assets and additional semantic roles.
Compatibility lookup should support inherited themes, scalable directories, symbolic
icons, MIME icons, and raster fallbacks.

Native symbolic SVGs may expose semantic paint roles such as foreground, secondary,
accent, warning, and destructive. External themes that do not provide those roles
remain usable through ordinary fixed-color or symbolic compatibility behavior.

## Desktop portals

XDG Desktop Portal aligns closely with NullStar's sandbox goals. A compatibility
frontend should expose supported portal interfaces and translate them into native
portal requests.

High-priority portal areas include:

- file chooser and selected-file access;
- open URI and application selection;
- notifications;
- screenshots and screen casting;
- camera and microphone access;
- clipboard and file transfer;
- printing;
- settings and appearance;
- inhibit and background execution;
- secrets;
- global shortcuts and remote desktop.

The native portal presents trusted UI and returns a narrow capability or stream. A
ported application does not gain broad `/Users`, microphone, camera, input, or desktop
access merely because it speaks a portal interface.

A D-Bus portal frontend may be required for application compatibility. The actual
portal backend should remain a native service client so authorization and application
identity do not depend solely on D-Bus names or Linux cgroup conventions.

## D-Bus compatibility

NullStar does not need D-Bus as its native IPC. A supervised compatibility service may
provide a session bus and selected system-facing interfaces:

```text
ported application
        |
        v
D-Bus compatibility service
        |
        v
native NullStar service client and authorization
```

The bridge must:

- preserve package, application, process-generation, user, and sandbox identity;
- prevent clients from impersonating trusted service names;
- expose only deliberately supported interfaces;
- limit subscriptions, queued messages, object counts, and activation;
- translate unsupported Unix-specific operations into explicit errors;
- never export all native service endpoints automatically.

System-bus compatibility should be narrower and later than the session bus.

## Notifications

A freedesktop notification frontend should translate requests into the native
notification service. The native service adds application identity, permission policy,
rate limits, grouping, action authorization, quiet modes, sensitive-content handling,
and history.

Notification actions are delivered only to the originating live application or an
explicitly registered activation target.

## Secret Service

A Secret Service compatibility frontend is valuable for browsers, development tools,
mail, chat, and other ported applications. It should map to the native secrets service,
which owns encrypted storage, user-session unlocking, application authorization,
access prompts, hardware-backed keys where available, and audit records.

Compatibility never means returning another application's secrets because a collection
name is known.

## Status notifier and tray compatibility

NullStar's native panel model uses isolated applets. Legacy tray and StatusNotifier
items should be hosted by a dedicated compatibility applet:

```text
legacy application
        |
        v
status-item compatibility broker
        |
        v
sanitized icon, tooltip, menu, and activation model
        |
        v
panel applet
```

A legacy item does not become a panel compositor client and receives no authority over
other applets or desktop surfaces.

## Autostart

Desktop autostart entries may be imported as user-service definitions. The service
manager then provides supervision, logs, dependencies, limits, enablement, and failure
handling.

An imported autostart entry is not launched as an unmanaged shell command. Native
session services remain defined through the service-manager format.

## Trash, thumbnails, and recent documents

These standards are useful compatibility targets but should be mediated by native
services:

- a trash service handles per-volume moves, original locations, retention, undo, and
  access control;
- a thumbnail service runs format handlers in sandboxes and stores compatibility cache
  entries under the user's cache category;
- a recent-document service applies per-user and per-application visibility, private
  session exclusions, and retention rather than exposing one globally readable file.

Ported applications may observe compatible files where necessary, but native clients
should use service APIs and stable file identities.

## Settings and appearance

A settings compatibility service should expose commonly expected desktop values:

- light or dark preference;
- cursor and icon theme;
- font names and text scale;
- interface scale and fractional output scale;
- reduced motion and high contrast;
- locale and selected accessibility preferences.

Values originate from the native configuration and accessibility services. A ported
application may adapt its own UI but cannot override global settings or trusted system
surfaces.

## Wayland relationship

Wayland protocol support belongs to the compositor compatibility frontend described in
[the graphics-stack design](graphics-stack.md). Core surface isolation and `xdg-shell`
are high-priority targets. Global-state and privileged extensions are advertised only
after their native authorization mapping is defined.

Wayland support and freedesktop desktop-service support are related but independently
versioned. A process may use native display objects with freedesktop file formats, or a
Wayland connection with native portals.

## Installation and package integration

The package manager should generate or register compatibility data transactionally:

- desktop entries;
- MIME declarations and associations;
- icons and themes;
- portal backend descriptions where trusted;
- D-Bus activation records for compatible services;
- translated autostart entries.

Uninstalling or rolling back a package removes its generated registrations atomically.
User overrides remain separate from package-owned data.

## Initial implementation priority

1. Map XDG base-directory variables to `Profile` and provide application-scoped native
   directory APIs.
2. Import desktop entries into the application registry.
3. Add MIME detection, associations, and icon-theme lookup.
4. Add core Wayland plus `xdg-shell` compatibility after the native surface model works.
5. Implement notifications and the most important file/open-URI portals.
6. Add a constrained D-Bus session compatibility service.
7. Add Secret Service, settings, screen-cast, file-transfer, and global-shortcut
   portals.
8. Add tray compatibility, autostart import, thumbnail, trash, and recent-document
   services.

## Open questions

- Which exact specification versions define the first supported compatibility profile.
- Whether compatibility files are materialized under projected paths or served from a
  registry-backed virtual view.
- The minimum D-Bus surface needed by targeted GTK, Qt, and COSMIC applications.
- How host application identity is established for software not launched from a native
  package.
- Which portal interfaces can be implemented before a complete desktop shell exists.
- How to expose compatibility runtime paths without weakening native capability-based
  file access.

## External specifications

Implementations should consult the current official specifications rather than treating
this design document as a substitute for them:

- [Freedesktop.org specifications](https://specifications.freedesktop.org/)
- [XDG Desktop Portal documentation](https://flatpak.github.io/xdg-desktop-portal/docs/)
- [Wayland documentation](https://wayland.freedesktop.org/docs/html/)
