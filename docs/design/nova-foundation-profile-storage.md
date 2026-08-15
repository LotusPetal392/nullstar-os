# Nova Foundation application context and profile storage direction

## Status

The following are **accepted direction**:

- Nova Foundation is the application-facing framework for identity-bound access to
  per-user configuration, persistent state, cache, application data, logs, and runtime
  storage;
- the application runtime supplies a verified application identity and Nova Foundation
  derives the current application context from that identity;
- applications request role-specific storage objects rather than constructing profile
  paths or selecting another application's identifier;
- configuration and state use typed records with compiled defaults, explicit schema
  versions, migration support, and transactional writes;
- defaults are layered below stored user overrides rather than copied wholesale into a
  file at first launch;
- configuration, state, cache, data, logs, and runtime storage remain semantically
  distinct even when they share implementation machinery;
- the mature native API returns scoped directory capabilities or storage objects rather
  than ambient path strings;
- a direct-filesystem backend may bootstrap the API before a profile service exists,
  provided that application code does not depend on that backend;
- the same public abstraction is available to graphical applications, command-line
  applications, and user-session agents rather than being coupled to the Nova widget
  toolkit.

Exact crate and module names, derive-macro syntax, serialization format, profile-service
protocol, watcher API, quota policy, cache-eviction policy, and corruption-recovery UI
remain **tentative design**.

This document refines the application-storage role described in
[Native application runtime, SDK, and service IDL](application-runtime-sdk-and-idl.md),
the accepted `Profile` layout in
[Userspace architecture](userspace-architecture.md), and the identity and authority
rules in [Capability-based application sandboxing](application-sandboxing.md) and
[Application bundles, signing, and deployment](application-bundles-and-deployment.md).
Implemented behavior remains authoritative until these interfaces exist.

## Purpose and scope

Nova Foundation should provide the ordinary application-facing API for facilities that
nearly every native application needs but that do not belong to a graphical widget
library:

- verified application identity and process context;
- typed preferences and persistent state;
- application-scoped profile storage;
- lifecycle integration and change observation;
- common migration and recovery behavior.

The framework should be usable by:

```text
graphical Nova applications
command-line applications
background agents
application helpers and workers
applications using another UI toolkit
```

A process may receive a narrower context than the primary application component. A
renderer, decoder, importer, or plugin host should not automatically receive every
storage object available to the main component merely because it belongs to the same
bundle.

The central rule is:

> Nova Foundation exposes application-profile storage through the verified launch
> context. An application identifier names the storage namespace, but only a runtime-
> issued capability grants access to it.

## Position in the platform

The intended layering is:

```text
application, command-line tool, or user agent
                    |
                    v
             Nova Foundation
       application context and typed stores
                    |
          +---------+----------+
          |                    |
          v                    v
 early local backend    profile-service client
          |                    |
          +---------+----------+
                    |
                    v
       capability-scoped profile storage
```

Nova Foundation belongs to the application-framework layer above the lower-level native
runtime. The tentative `nullstar-app` layer remains responsible for validating startup
resources, lifecycle messages, and the restricted service namespace. Nova Foundation
turns that context into a convenient, typed API.

This separation keeps the public API stable while the backend evolves:

```text
bootstrap implementation:
Nova Foundation -> direct filesystem backend -> Profile directories

mature implementation:
Nova Foundation -> versioned profile service -> scoped directory capabilities
```

Application code should not need to change when the backend moves behind a service.

## Application identity

The public value type should be named `ApplicationId` or an equivalent unambiguous term.
Documentation should call values such as `org.example.Player` **reverse-domain-style
application identifiers**. The term `rDNS` should be avoided because it normally refers
to reverse Domain Name System lookup rather than this naming convention.

Examples are:

```text
org.nullstar.Settings
org.example.ImageEditor
com.galacticpirateradio.Player
```

`ApplicationId` must be a validated type rather than an arbitrary string. Validation
should reject at least:

- empty components;
- path separators;
- `.` or `..` path components;
- control characters;
- ambiguous Unicode normalization;
- reserved system namespaces without matching system authorization;
- identifiers that exceed the platform's published bounds.

The exact character and normalization rules should be shared with bundle verification,
package registration, permission storage, and service policy.

### Runtime-provided identity

The normal API should derive identity from the launch context:

```rust
let app = ApplicationContext::current()?;
```

It should not normally ask an application to select its own identity:

```rust
// Not the normal production API.
let app = ApplicationContext::open("org.nullstar.Settings")?;
```

Otherwise an untrusted application could name another application's profile. An
explicit `for_id` or similar constructor may exist for tests, host development,
migration tools, or unmanaged compatibility processes, but the runtime or backend must
verify that the caller is authorized for the requested identity.

Application security identity also includes publisher lineage and trust information as
described by the bundle and sandboxing design. A matching identifier alone must not let
a differently signed application inherit private storage or grants.

## Profile storage roles

Nova Foundation should expose all accepted `Profile` categories even though the initial
request centers on configuration, state, and cache.

| Role | Meaning | Typical contents | May be removed automatically? |
| --- | --- | --- | --- |
| `config` | User-controlled durable preferences | theme choice, volume, sort order | No |
| `state` | Persistent operational state | window position, tabs, playback | Normally no |
| `cache` | Regenerable acceleration data | thumbnails, decoded artwork, indexes | Yes |
| `data` | Durable application-managed content | local databases, imported libraries | No |
| `logs` | Diagnostic records | structured logs, local crash context | Under retention policy |
| `runtime` | Per-login transient coordination | sockets, locks, temporary sessions | Yes |

These categories may share serialization, filesystem, transaction, and observation
machinery, but they must remain distinct public types and policy domains. Clearing a
cache must never erase state or durable data. Resetting application state must not reset
user preferences.

A possible physical layout remains category-first:

```text
/Users/<user>/Profile/
├── config/org.example.Player/
├── state/org.example.Player/
├── cache/org.example.Player/
├── data/org.example.Player/
├── logs/org.example.Player/
└── runtime/org.example.Player/
```

That layout is not an authorization mechanism. Native applications should use storage
objects or rooted directory capabilities issued for the selected role.

## Application context API

An illustrative API is:

```rust
use nova_foundation::{
    application::ApplicationContext,
    settings::{ConfigRecord, StateRecord},
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[nova::config(version = 2)]
struct Preferences {
    volume: f32,
    resume_playback: bool,
    theme: ThemePreference,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            volume: 0.8,
            resume_playback: true,
            theme: ThemePreference::System,
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[nova::state(version = 1)]
struct SessionState {
    current_track: Option<TrackId>,
    playback_position_ms: u64,
    window: WindowState,
}

fn start() -> nova_foundation::Result<()> {
    let app = ApplicationContext::current()?;

    let preferences = app
        .config::<Preferences>("preferences")?
        .load()?;

    let session = app
        .state::<SessionState>("session")?
        .load()?;

    let artwork_cache = app.cache_directory("artwork")?;

    Ok(())
}
```

The syntax is illustrative. The final API may use generated record definitions or a
NullStar-native encoding rather than `serde`. The important contract is typed values,
versioned schemas, explicit storage roles, and an identity-bound context.

A conceptual context API is:

```rust
impl ApplicationContext {
    pub fn current() -> Result<Self>;
    pub fn id(&self) -> &ApplicationId;

    pub fn config<T>(&self, name: &str) -> Result<ConfigStore<T>>
    where
        T: ConfigRecord;

    pub fn state<T>(&self, name: &str) -> Result<StateStore<T>>
    where
        T: StateRecord;

    pub fn cache_directory(&self, namespace: &str)
        -> Result<DirectoryHandle>;
    pub fn data_directory(&self, namespace: &str)
        -> Result<DirectoryHandle>;
    pub fn log_directory(&self) -> Result<DirectoryHandle>;
    pub fn runtime_directory(&self, namespace: &str)
        -> Result<DirectoryHandle>;
}
```

Names such as `preferences`, `session`, and `artwork` are bounded application-local
names. They are not paths and must not contain separators or traversal components.

## Defaults and stored overrides

Compiled defaults should form the bottom layer of the effective value:

```text
compiled application defaults
            +
stored user overrides
            =
effective configuration
```

Nova Foundation should not eagerly serialize every default merely because an
application launched. Doing so would turn a default into a permanent override and stop
a later application version from changing that default.

For example:

```text
version 1 compiled default: animation_speed = 1.0
stored override:            absent

version 2 compiled default: animation_speed = 0.85
effective value:            0.85
```

When the user explicitly chooses `1.0`, that value becomes an override and remains
stable across later default changes.

The settings API should distinguish among:

```rust
settings.get()?;
settings.set(updated)?;
settings.reset_key("animation_speed")?;
settings.reset_all()?;
settings.is_overridden("animation_speed")?;
```

Resetting removes the stored override and reveals the current compiled default. It does
not copy the default into persistent storage.

Defaults must be deterministic for one application version. Defaults that depend on
session policy, appearance, locale, hardware, or another service should normally be
represented as an explicit `System`, `Automatic`, or similar value rather than
serializing an environment-dependent result as though it were a static default.

## Configuration and state semantics

Configuration and state may use the same storage engine but should have different public
record traits and lifecycle expectations.

### Configuration

Configuration represents durable user intent. It should:

- preserve explicit overrides across ordinary application updates;
- participate in backup by default;
- support per-key reset where the schema permits it;
- avoid high-frequency writes during animation, scrolling, or playback;
- expose enough metadata for trusted settings and reset tools.

### State

State represents persistent operational continuity. It should:

- restore sessions, windows, navigation, histories, and resumable work;
- allow an application or system recovery tool to reset it without removing user
  preferences or durable data;
- tolerate more frequent bounded updates than configuration;
- support generation or checkpoint semantics where partial restoration would be
  unsafe;
- never be used as a substitute for cache when the value is regenerable.

The distinction is semantic rather than merely a directory choice. Nova Foundation
should reinforce it with `ConfigStore<T>` and `StateStore<T>` rather than one generic
untyped key-value store.

## Schema versions and migration

Every typed configuration or state record should declare a schema version independent
of the application package version.

```rust
#[derive(Serialize, Deserialize)]
#[nova::config(version = 3, migrate = migrate_preferences)]
struct Preferences {
    output_device: Option<DeviceId>,
    appearance: AppearancePreferences,
}
```

Migrations should be explicit, bounded, testable, and normally sequential:

```rust
fn migrate_preferences(
    old_version: u32,
    value: NovaValue,
) -> Result<NovaValue> {
    match old_version {
        1 => migrate_v1_to_v2(value),
        2 => migrate_v2_to_v3(value),
        3 => Ok(value),
        _ => Err(MigrationError::UnsupportedVersion),
    }
}
```

The storage layer should:

- preserve or quarantine unreadable input before recovery;
- never silently overwrite data written by a newer unsupported schema;
- report whether a record loaded normally, migrated, recovered, or fell back to
  defaults;
- validate the fully migrated record before replacing the previous version;
- retain enough metadata to diagnose migration failures without logging secrets;
- avoid running arbitrary migration code with more authority than the application
  component requires.

An illustrative load status is:

```rust
match loaded.status() {
    LoadStatus::Current => {}
    LoadStatus::Migrated { from, to } => {}
    LoadStatus::RecoveredFromCorruption { backup } => {}
    LoadStatus::DefaultsOnly => {}
}
```

Application-generation rollback must not blindly downgrade mutable state. A bundle
manifest should eventually declare state compatibility, and migrations that cannot be
reversed may require a snapshot or an explicit rollback barrier.

## Transactional writes

A filesystem backend must not overwrite the only live copy of a record in place. At a
minimum, one-record replacement should:

1. serialize and validate the complete new value;
2. write a temporary object in the same storage domain;
3. flush the object when durability is requested;
4. atomically replace the previous committed object;
5. durably commit directory or transaction metadata as required by the filesystem;
6. preserve a bounded last-known-good or quarantine record when policy calls for it.

The typed API should favor update transactions over an unsafe load-modify-save race:

```rust
config.update(|preferences| {
    preferences.volume = 0.65;
    preferences.resume_playback = false;
})?;
```

A future profile service should serialize competing updates or use explicit generation
checks. Multi-record transactions should be added only with defined atomicity, conflict,
and crash-recovery semantics.

## Change observation

Applications may need to react when another component, a settings application, profile
restore, or administrative policy changes a record. Observation should therefore use
committed generations rather than raw filesystem watching.

A change event should identify:

```text
application-local store name
old committed generation
new committed generation
change kind
whether a reload is required
```

Events should be coalescible and bounded. A watcher notification is not the data itself;
the client reloads the current committed value through its store object. This avoids
making file-renames or backend-specific watch behavior part of the public contract.

## Directory capabilities, not ambient paths

During bootstrap, a host or direct-filesystem backend may expose a `PathBuf` internally.
The permanent native API should return a scoped object:

```rust
let cache = app.cache_directory("album-art")?;

cache.create_file("37a921f.thumbnail")?;
cache.remove_tree("obsolete")?;
```

The handle carries authority only for the selected application and role. It must not
grant access to:

```text
another application's profile
all of Profile/config
all of the user's home directory
raw system storage
```

A compatibility layer may project the capability at a private path for POSIX software,
but the path remains a compatibility view rather than the native authority model.

The backend should issue component-specific rights. A thumbnail worker may receive
read-write cache access without receiving configuration or durable data access. A
migration component may receive one old and one new state object without receiving the
application's documents or unrelated profile roles.

## Profile service responsibilities

The mature backend may be a per-session profile service or a closely related storage
broker. Nova Foundation remains the client API. The service should:

- verify the caller's application, publisher, component, user, and session context;
- create required role directories or storage objects with correct ownership and
  metadata;
- return rights-reduced directory or typed-store capabilities;
- prevent application-identifier spoofing and cross-application traversal;
- coordinate transactional writes, committed generations, and watchers;
- enforce bounded record, file, and namespace sizes;
- apply cache quotas and eviction without treating cache as durable data;
- integrate backup, restore, migration, logging, and corruption recovery policy;
- invalidate runtime storage when the login session ends;
- expose structured diagnostics without leaking private values.

The service must not infer authority from a caller-supplied path. It makes an issuance
decision from verified launch identity and policy, then returns a scoped object.

## Suggested crate organization

An initial single crate is sufficient:

```text
nova-foundation/
└── src/
    ├── application.rs
    ├── application_id.rs
    ├── profile.rs
    ├── config.rs
    ├── state.rs
    ├── storage.rs
    ├── migration.rs
    ├── observation.rs
    └── error.rs
```

Possible public modules are:

```rust
nova_foundation::application
nova_foundation::profile
nova_foundation::settings
nova_foundation::state
```

The implementation may later split into crates such as:

```text
nova-foundation
nova-settings
nova-profile-client
```

`nova-foundation` should continue to re-export the ordinary application API so
application authors do not have to assemble backend pieces themselves. The lower-level
runtime, IPC, and generated service bindings remain separate platform layers.

## Bootstrap implementation path

A practical implementation sequence is:

1. define `ApplicationId`, `ApplicationContext`, storage-role types, and strict local-name
   validation;
2. provide a direct-filesystem backend for hosted development and early NullStar
   applications;
3. add typed configuration and state with default overlays, schema versions, migration,
   and atomic single-record replacement;
4. add committed generations and change observation;
5. pass verified identity and role-specific directory handles through the native launch
   context;
6. move creation, transactions, quotas, eviction, and recovery behind a versioned
   profile service without changing application-facing code.

The bootstrap backend should be treated as an implementation strategy, not as permission
for applications to construct their own profile paths.

## Security and privacy requirements

The API and backend should enforce these invariants:

- one application cannot select or traverse into another application's namespace;
- changing a display name, bundle filename, or installation path does not change
  storage identity;
- an unexpected publisher change does not inherit private profile storage;
- system applications remain scoped rather than receiving the entire user profile;
- helpers receive only the storage roles and rights declared for their component;
- logs and migration diagnostics do not expose secrets or complete private records;
- configuration values do not become authority merely because they contain a path,
  service name, device identifier, or application identifier;
- cache eviction cannot delete configuration, state, data, or user documents;
- runtime storage is session-scoped and cannot be mistaken for durable state.

Applications that need shared mutable storage should use an explicitly declared
application group or a service protocol. Reusing a publisher namespace or common path
prefix must not create implicit sharing.

## Open questions

- The first on-disk or service-side encoding for typed configuration and state.
- Whether unknown fields are preserved automatically across read-modify-write cycles.
- The exact transaction and conflict model for multiple application components.
- How profile backup, restore, and application-generation rollback coordinate.
- Whether durable application data receives storage quotas independently from cache.
- The cache eviction contract, recency metadata, pinning, and user-facing controls.
- The policy for unmanaged command-line programs without a verified bundle identity.
- The design of explicitly declared application-group storage.
- Whether secrets are separate typed references into a secrets service rather than
  values allowed in ordinary configuration records.
- The initial profile-service identity and NSIDL protocol name.
