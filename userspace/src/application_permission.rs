//! Bounded persistent application grants and stable filesystem resource identities.

use core::ops::{BitOr, BitOrAssign};

use nullfs_format::crc32c;

use crate::application_identity::{
    ApplicationInstallScope, ApplicationProfile, ApplicationTrustClass, AuthorizedApplication,
    StableApplicationPrincipal,
};
use crate::filesystem::{self, Node, Session};

pub const MAX_APPLICATION_GRANTS: usize = 64;
pub const APPLICATION_GRANT_RECORD_BYTES: usize = 128;
pub const APPLICATION_GRANT_MAGIC: [u8; 4] = *b"NSPG";
pub const APPLICATION_GRANT_VERSION: u16 = 1;

const CHECKSUM_OFFSET: usize = 124;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ApplicationResourceKind {
    File = 1,
    Directory = 2,
}

impl ApplicationResourceKind {
    const fn from_raw(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::File),
            2 => Some(Self::Directory),
            _ => None,
        }
    }
}

/// Stable identity of one filesystem object. Provider generation is deliberately
/// absent: a service restart must not invalidate a resource, while inode reuse must.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationResourceIdentity {
    filesystem_uuid: [u8; 16],
    object_id: u64,
    object_generation: u64,
    kind: ApplicationResourceKind,
}

impl ApplicationResourceIdentity {
    pub fn new(
        filesystem_uuid: [u8; 16],
        object_id: u64,
        object_generation: u64,
        kind: ApplicationResourceKind,
    ) -> Option<Self> {
        if filesystem_uuid == [0; 16] || object_id == 0 || object_generation == 0 {
            return None;
        }
        Some(Self {
            filesystem_uuid,
            object_id,
            object_generation,
            kind,
        })
    }

    pub const fn filesystem_uuid(self) -> [u8; 16] {
        self.filesystem_uuid
    }

    pub const fn object_id(self) -> u64 {
        self.object_id
    }

    pub const fn object_generation(self) -> u64 {
        self.object_generation
    }

    pub const fn kind(self) -> ApplicationResourceKind {
        self.kind
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationResourceResolveError {
    Filesystem(filesystem::Error),
    InvalidIdentity,
    FilesystemMismatch,
    UnsupportedKind,
    KindMismatch,
}

/// Resolves generation-scoped provider nodes into persistent application resource identities.
///
/// Authority comes from the live filesystem session. The expected UUID must come from trusted mount
/// selection rather than from the provider reply being validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationResourceResolver {
    session: Session,
    expected_filesystem_uuid: [u8; 16],
}

impl ApplicationResourceResolver {
    pub fn new(session: Session, expected_filesystem_uuid: [u8; 16]) -> Option<Self> {
        if expected_filesystem_uuid == [0; 16] {
            return None;
        }
        Some(Self {
            session,
            expected_filesystem_uuid,
        })
    }

    pub const fn expected_filesystem_uuid(self) -> [u8; 16] {
        self.expected_filesystem_uuid
    }

    pub fn resolve(
        self,
        request_id: u64,
        node: Node,
        expected_kind: ApplicationResourceKind,
    ) -> Result<ApplicationResourceIdentity, ApplicationResourceResolveError> {
        let identity = self
            .session
            .stable_identity(request_id, node)
            .map_err(ApplicationResourceResolveError::Filesystem)?;
        validate_resolved_identity(self.expected_filesystem_uuid, identity, expected_kind)
    }
}

fn validate_resolved_identity(
    expected_filesystem_uuid: [u8; 16],
    identity: filesystem::protocol::StableNodeIdentity,
    expected_kind: ApplicationResourceKind,
) -> Result<ApplicationResourceIdentity, ApplicationResourceResolveError> {
    if !identity.canonical() {
        return Err(ApplicationResourceResolveError::InvalidIdentity);
    }
    if identity.filesystem_uuid != expected_filesystem_uuid {
        return Err(ApplicationResourceResolveError::FilesystemMismatch);
    }
    let kind = match identity.kind {
        filesystem::protocol::node_kind::FILE => ApplicationResourceKind::File,
        filesystem::protocol::node_kind::DIRECTORY => ApplicationResourceKind::Directory,
        _ => return Err(ApplicationResourceResolveError::UnsupportedKind),
    };
    if kind != expected_kind {
        return Err(ApplicationResourceResolveError::KindMismatch);
    }
    ApplicationResourceIdentity::new(
        identity.filesystem_uuid,
        identity.object_id,
        identity.object_generation,
        kind,
    )
    .ok_or(ApplicationResourceResolveError::InvalidIdentity)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationGrantRights(u16);

impl ApplicationGrantRights {
    pub const EMPTY: Self = Self(0);
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const CREATE: Self = Self(1 << 2);
    pub const REMOVE: Self = Self(1 << 3);
    pub const ENUMERATE: Self = Self(1 << 4);
    pub const FILE: Self = Self(Self::READ.0 | Self::WRITE.0);
    pub const DIRECTORY: Self =
        Self(Self::READ.0 | Self::WRITE.0 | Self::CREATE.0 | Self::REMOVE.0 | Self::ENUMERATE.0);

    pub const fn from_bits(bits: u16) -> Option<Self> {
        if bits & !Self::DIRECTORY.0 != 0 {
            None
        } else {
            Some(Self(bits))
        }
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn contains(self, requested: Self) -> bool {
        self.0 & requested.0 == requested.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn valid_for(self, kind: ApplicationResourceKind) -> bool {
        if self.0 == 0 {
            return false;
        }
        let maximum = match kind {
            ApplicationResourceKind::File => Self::FILE,
            ApplicationResourceKind::Directory => Self::DIRECTORY,
        };
        maximum.contains(self)
    }
}

impl BitOr for ApplicationGrantRights {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for ApplicationGrantRights {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ApplicationGrantScope {
    Once = 1,
    Session = 2,
    Persistent = 3,
}

impl ApplicationGrantScope {
    const fn from_raw(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Once),
            2 => Some(Self::Session),
            3 => Some(Self::Persistent),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ApplicationGrantRevocation {
    Consumed = 1,
    User = 2,
    SessionEnded = 3,
    ResourceRemoved = 4,
    ApplicationRemoved = 5,
    Policy = 6,
    Reset = 7,
}

impl ApplicationGrantRevocation {
    const fn from_raw(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Consumed),
            2 => Some(Self::User),
            3 => Some(Self::SessionEnded),
            4 => Some(Self::ResourceRemoved),
            5 => Some(Self::ApplicationRemoved),
            6 => Some(Self::Policy),
            7 => Some(Self::Reset),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationGrantState {
    Active,
    Revoked(ApplicationGrantRevocation),
}

/// Stable grant subject. Package generation and process/session identity are
/// intentionally excluded so an authorized update or relaunch can restore a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationGrantSubject {
    user: u64,
    principal: StableApplicationPrincipal,
    installation: u64,
    install_scope: ApplicationInstallScope,
}

impl ApplicationGrantSubject {
    pub fn from_authorization(authorization: AuthorizedApplication) -> Option<Self> {
        if authorization.profile() != ApplicationProfile::Desktop {
            return None;
        }
        let identity = authorization.identity();
        let principal = authorization.principal();
        let provenance = authorization.provenance();
        Self::new(
            identity.user,
            principal,
            provenance.installation,
            provenance.scope,
        )
    }

    const fn new(
        user: u64,
        principal: StableApplicationPrincipal,
        installation: u64,
        install_scope: ApplicationInstallScope,
    ) -> Option<Self> {
        if user == 0
            || principal.application == 0
            || principal.publisher == 0
            || principal.signing_lineage == 0
            || installation == 0
            || principal.system_application
                != matches!(principal.trust_class, ApplicationTrustClass::System)
            || principal.system_application
                != matches!(install_scope, ApplicationInstallScope::SystemGeneration)
        {
            return None;
        }
        Some(Self {
            user,
            principal,
            installation,
            install_scope,
        })
    }

    pub const fn user(self) -> u64 {
        self.user
    }

    pub const fn principal(self) -> StableApplicationPrincipal {
        self.principal
    }

    pub const fn installation(self) -> u64 {
        self.installation
    }

    pub const fn install_scope(self) -> ApplicationInstallScope {
        self.install_scope
    }

    pub const fn is_transient(self) -> bool {
        matches!(self.principal.trust_class, ApplicationTrustClass::Transient)
            || matches!(self.install_scope, ApplicationInstallScope::Transient)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationGrantRecord {
    id: u64,
    revision: u64,
    subject: ApplicationGrantSubject,
    resource: ApplicationResourceIdentity,
    rights: ApplicationGrantRights,
    scope: ApplicationGrantScope,
    session: u64,
    state: ApplicationGrantState,
}

impl ApplicationGrantRecord {
    pub const fn id(self) -> u64 {
        self.id
    }

    pub const fn revision(self) -> u64 {
        self.revision
    }

    pub const fn subject(self) -> ApplicationGrantSubject {
        self.subject
    }

    pub const fn resource(self) -> ApplicationResourceIdentity {
        self.resource
    }

    pub const fn rights(self) -> ApplicationGrantRights {
        self.rights
    }

    pub const fn scope(self) -> ApplicationGrantScope {
        self.scope
    }

    pub const fn session(self) -> Option<u64> {
        if self.session == 0 {
            None
        } else {
            Some(self.session)
        }
    }

    pub const fn state(self) -> ApplicationGrantState {
        self.state
    }

    pub const fn active(self) -> bool {
        matches!(self.state, ApplicationGrantState::Active)
    }

    pub fn encode(self) -> [u8; APPLICATION_GRANT_RECORD_BYTES] {
        assert!(
            self.canonical(),
            "cannot encode a noncanonical application grant"
        );
        let mut bytes = [0_u8; APPLICATION_GRANT_RECORD_BYTES];
        bytes[0..4].copy_from_slice(&APPLICATION_GRANT_MAGIC);
        bytes[4..6].copy_from_slice(&APPLICATION_GRANT_VERSION.to_le_bytes());
        let (state, revocation) = match self.state {
            ApplicationGrantState::Active => (1, 0),
            ApplicationGrantState::Revoked(reason) => (2, reason as u8),
        };
        bytes[6] = state;
        bytes[7] = self.scope as u8;
        write_u64(&mut bytes, 8, self.id);
        write_u64(&mut bytes, 16, self.revision);
        write_u64(&mut bytes, 24, self.subject.user);
        write_u64(&mut bytes, 32, self.subject.principal.application);
        write_u64(&mut bytes, 40, self.subject.principal.publisher);
        write_u64(&mut bytes, 48, self.subject.principal.signing_lineage);
        write_u64(&mut bytes, 56, self.subject.installation);
        bytes[64] = self.subject.principal.trust_class as u8;
        bytes[65] = self.subject.install_scope as u8;
        bytes[66] = u8::from(self.subject.principal.system_application);
        bytes[67] = self.resource.kind as u8;
        bytes[68..70].copy_from_slice(&self.rights.bits().to_le_bytes());
        bytes[70] = revocation;
        bytes[72..88].copy_from_slice(&self.resource.filesystem_uuid);
        write_u64(&mut bytes, 88, self.resource.object_id);
        write_u64(&mut bytes, 96, self.resource.object_generation);
        write_u64(&mut bytes, 104, self.session);
        let remaining_uses =
            u32::from(self.active() && matches!(self.scope, ApplicationGrantScope::Once));
        bytes[120..124].copy_from_slice(&remaining_uses.to_le_bytes());
        let checksum = crc32c(&bytes[..CHECKSUM_OFFSET]);
        bytes[CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ApplicationGrantDecodeError> {
        if bytes.len() != APPLICATION_GRANT_RECORD_BYTES {
            return Err(ApplicationGrantDecodeError::Length);
        }
        if bytes[0..4] != APPLICATION_GRANT_MAGIC {
            return Err(ApplicationGrantDecodeError::Magic);
        }
        if u16::from_le_bytes([bytes[4], bytes[5]]) != APPLICATION_GRANT_VERSION {
            return Err(ApplicationGrantDecodeError::Version);
        }
        let expected = read_u32(bytes, CHECKSUM_OFFSET);
        if crc32c(&bytes[..CHECKSUM_OFFSET]) != expected {
            return Err(ApplicationGrantDecodeError::Checksum);
        }
        if bytes[71] != 0 || bytes[112..120] != [0; 8] {
            return Err(ApplicationGrantDecodeError::Reserved);
        }
        let state = match (bytes[6], bytes[70]) {
            (1, 0) => ApplicationGrantState::Active,
            (2, reason) => ApplicationGrantState::Revoked(
                ApplicationGrantRevocation::from_raw(reason)
                    .ok_or(ApplicationGrantDecodeError::State)?,
            ),
            _ => return Err(ApplicationGrantDecodeError::State),
        };
        let scope =
            ApplicationGrantScope::from_raw(bytes[7]).ok_or(ApplicationGrantDecodeError::Scope)?;
        let trust_class =
            trust_class_from_raw(bytes[64]).ok_or(ApplicationGrantDecodeError::Subject)?;
        let install_scope =
            install_scope_from_raw(bytes[65]).ok_or(ApplicationGrantDecodeError::Subject)?;
        let system_application = match bytes[66] {
            0 => false,
            1 => true,
            _ => return Err(ApplicationGrantDecodeError::Subject),
        };
        let subject = ApplicationGrantSubject::new(
            read_u64(bytes, 24),
            StableApplicationPrincipal {
                application: read_u64(bytes, 32),
                publisher: read_u64(bytes, 40),
                signing_lineage: read_u64(bytes, 48),
                trust_class,
                system_application,
            },
            read_u64(bytes, 56),
            install_scope,
        )
        .ok_or(ApplicationGrantDecodeError::Subject)?;
        let kind = ApplicationResourceKind::from_raw(bytes[67])
            .ok_or(ApplicationGrantDecodeError::Resource)?;
        let mut filesystem_uuid = [0_u8; 16];
        filesystem_uuid.copy_from_slice(&bytes[72..88]);
        let resource = ApplicationResourceIdentity::new(
            filesystem_uuid,
            read_u64(bytes, 88),
            read_u64(bytes, 96),
            kind,
        )
        .ok_or(ApplicationGrantDecodeError::Resource)?;
        let rights = ApplicationGrantRights::from_bits(u16::from_le_bytes([bytes[68], bytes[69]]))
            .filter(|rights| rights.valid_for(kind))
            .ok_or(ApplicationGrantDecodeError::Rights)?;
        let record = Self {
            id: read_u64(bytes, 8),
            revision: read_u64(bytes, 16),
            subject,
            resource,
            rights,
            scope,
            session: read_u64(bytes, 104),
            state,
        };
        let remaining_uses = read_u32(bytes, 120);
        if !record.canonical()
            || remaining_uses
                != u32::from(record.active() && matches!(record.scope, ApplicationGrantScope::Once))
        {
            return Err(ApplicationGrantDecodeError::Canonical);
        }
        Ok(record)
    }

    fn canonical(self) -> bool {
        if self.id == 0
            || self.revision == 0
            || !self.rights.valid_for(self.resource.kind)
            || ApplicationGrantSubject::new(
                self.subject.user,
                self.subject.principal,
                self.subject.installation,
                self.subject.install_scope,
            )
            .is_none()
            || ApplicationResourceIdentity::new(
                self.resource.filesystem_uuid,
                self.resource.object_id,
                self.resource.object_generation,
                self.resource.kind,
            )
            .is_none()
        {
            return false;
        }
        match self.scope {
            ApplicationGrantScope::Once | ApplicationGrantScope::Session => self.session != 0,
            ApplicationGrantScope::Persistent => self.session == 0 && !self.subject.is_transient(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationGrantDecodeError {
    Length,
    Magic,
    Version,
    Checksum,
    Reserved,
    State,
    Scope,
    Subject,
    Resource,
    Rights,
    Canonical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPermissionStoreError {
    InvalidAuthorization,
    InvalidRights,
    TransientPersistence,
    DuplicateGrant,
    Full,
    GrantIdExhausted,
    RevisionExhausted,
    UnknownGrant,
    AlreadyRevoked,
    InvalidRevocation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationGrantAuthorizationError {
    InvalidAuthorization,
    InvalidRights,
    NotGranted,
    RightsDenied,
    ScopeExpired,
    RevisionExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPermissionLoadError {
    Capacity,
    InvalidCounter,
    InvalidRecord,
    DuplicateGrantId,
    DuplicateRevision,
    DuplicateActiveGrant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationGrantAuthorization {
    grant_id: u64,
    grant_revision: u64,
    subject: ApplicationGrantSubject,
    resource: ApplicationResourceIdentity,
    rights: ApplicationGrantRights,
    scope: ApplicationGrantScope,
}

impl ApplicationGrantAuthorization {
    pub const fn grant_id(self) -> u64 {
        self.grant_id
    }

    pub const fn grant_revision(self) -> u64 {
        self.grant_revision
    }

    pub const fn subject(self) -> ApplicationGrantSubject {
        self.subject
    }

    pub const fn resource(self) -> ApplicationResourceIdentity {
        self.resource
    }

    pub const fn rights(self) -> ApplicationGrantRights {
        self.rights
    }

    pub const fn scope(self) -> ApplicationGrantScope {
        self.scope
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreparedGrantCommit {
    Existing {
        index: usize,
        consumed_revision: Option<u64>,
        next_revision: u64,
    },
    Issued {
        slot: usize,
        stored_record: ApplicationGrantRecord,
        next_grant_id: u64,
        next_revision: u64,
    },
}

/// A preflighted grant authorization whose policy mutation is infallible at commit time.
///
/// Dropping this value leaves the permission store unchanged. Portal selection uses that property
/// to mint and transfer a resource endpoint before committing a new grant or consuming a one-shot
/// grant.
pub struct PreparedApplicationGrant<'a> {
    store: &'a mut ApplicationPermissionStore,
    authorization: ApplicationGrantAuthorization,
    commit: PreparedGrantCommit,
}

impl PreparedApplicationGrant<'_> {
    pub const fn authorization(&self) -> ApplicationGrantAuthorization {
        self.authorization
    }

    /// Applies the already-reserved store mutation. All capacity and counter checks happened while
    /// preparing the transaction, so this operation cannot fail.
    pub fn commit(self) -> ApplicationGrantAuthorization {
        let Self {
            store,
            authorization,
            commit,
        } = self;
        match commit {
            PreparedGrantCommit::Existing {
                index,
                consumed_revision,
                next_revision,
            } => {
                if let Some(revision) = consumed_revision {
                    let record = store.grants[index].expect("prepared grant still exists");
                    store.grants[index] = Some(ApplicationGrantRecord {
                        revision,
                        state: ApplicationGrantState::Revoked(ApplicationGrantRevocation::Consumed),
                        ..record
                    });
                    store.next_revision = next_revision;
                }
            }
            PreparedGrantCommit::Issued {
                slot,
                stored_record,
                next_grant_id,
                next_revision,
            } => {
                debug_assert!(store.grants[slot].is_none());
                store.grants[slot] = Some(stored_record);
                store.next_grant_id = next_grant_id;
                store.next_revision = next_revision;
            }
        }
        authorization
    }
}

/// Fixed-capacity policy store. Revoked records remain as tombstones so an old
/// persisted record cannot be replayed after revocation or reset.
pub struct ApplicationPermissionStore {
    grants: [Option<ApplicationGrantRecord>; MAX_APPLICATION_GRANTS],
    next_grant_id: u64,
    next_revision: u64,
}

impl ApplicationPermissionStore {
    pub const fn new() -> Self {
        Self {
            grants: [None; MAX_APPLICATION_GRANTS],
            next_grant_id: 1,
            next_revision: 1,
        }
    }

    pub fn restore_checkpoint(
        records: &[ApplicationGrantRecord],
        next_grant_id: u64,
        next_revision: u64,
    ) -> Result<Self, ApplicationPermissionLoadError> {
        if records.len() > MAX_APPLICATION_GRANTS {
            return Err(ApplicationPermissionLoadError::Capacity);
        }
        if next_grant_id == 0 || next_revision == 0 {
            return Err(ApplicationPermissionLoadError::InvalidCounter);
        }
        let mut store = Self {
            grants: [None; MAX_APPLICATION_GRANTS],
            next_grant_id,
            next_revision,
        };
        for (index, record) in records.iter().copied().enumerate() {
            if !record.canonical() || record.id >= next_grant_id || record.revision >= next_revision
            {
                return Err(ApplicationPermissionLoadError::InvalidRecord);
            }
            if store.grants[..index]
                .iter()
                .flatten()
                .any(|existing| existing.id == record.id)
            {
                return Err(ApplicationPermissionLoadError::DuplicateGrantId);
            }
            if store.grants[..index]
                .iter()
                .flatten()
                .any(|existing| existing.revision == record.revision)
            {
                return Err(ApplicationPermissionLoadError::DuplicateRevision);
            }
            if record.active()
                && store.grants[..index].iter().flatten().any(|existing| {
                    existing.active()
                        && existing.subject == record.subject
                        && existing.resource == record.resource
                })
            {
                return Err(ApplicationPermissionLoadError::DuplicateActiveGrant);
            }
            store.grants[index] = Some(record);
        }
        Ok(store)
    }

    pub fn issue(
        &mut self,
        authorization: AuthorizedApplication,
        resource: ApplicationResourceIdentity,
        rights: ApplicationGrantRights,
        scope: ApplicationGrantScope,
    ) -> Result<ApplicationGrantRecord, ApplicationPermissionStoreError> {
        let subject = ApplicationGrantSubject::from_authorization(authorization)
            .ok_or(ApplicationPermissionStoreError::InvalidAuthorization)?;
        if !rights.valid_for(resource.kind) {
            return Err(ApplicationPermissionStoreError::InvalidRights);
        }
        if scope == ApplicationGrantScope::Persistent && subject.is_transient() {
            return Err(ApplicationPermissionStoreError::TransientPersistence);
        }
        if self.grants.iter().flatten().any(|record| {
            record.active() && record.subject == subject && record.resource == resource
        }) {
            return Err(ApplicationPermissionStoreError::DuplicateGrant);
        }
        let Some(slot) = self.grants.iter().position(Option::is_none) else {
            return Err(ApplicationPermissionStoreError::Full);
        };
        let id = self.next_grant_id;
        let next_grant_id = id
            .checked_add(1)
            .ok_or(ApplicationPermissionStoreError::GrantIdExhausted)?;
        let revision = self.next_revision;
        let next_revision = revision
            .checked_add(1)
            .ok_or(ApplicationPermissionStoreError::RevisionExhausted)?;
        let record = ApplicationGrantRecord {
            id,
            revision,
            subject,
            resource,
            rights,
            scope,
            session: if scope == ApplicationGrantScope::Persistent {
                0
            } else {
                authorization.identity().session
            },
            state: ApplicationGrantState::Active,
        };
        debug_assert!(record.canonical());
        self.grants[slot] = Some(record);
        self.next_grant_id = next_grant_id;
        self.next_revision = next_revision;
        Ok(record)
    }

    pub fn authorize(
        &mut self,
        authorization: AuthorizedApplication,
        resource: ApplicationResourceIdentity,
        requested_rights: ApplicationGrantRights,
    ) -> Result<ApplicationGrantAuthorization, ApplicationGrantAuthorizationError> {
        self.prepare_authorization(authorization, resource, requested_rights)
            .map(PreparedApplicationGrant::commit)
    }

    /// Preflights authorization without changing the store. Dropping the returned transaction
    /// compensates a failed endpoint mint or response transfer by construction.
    pub fn prepare_authorization(
        &mut self,
        authorization: AuthorizedApplication,
        resource: ApplicationResourceIdentity,
        requested_rights: ApplicationGrantRights,
    ) -> Result<PreparedApplicationGrant<'_>, ApplicationGrantAuthorizationError> {
        let subject = ApplicationGrantSubject::from_authorization(authorization)
            .ok_or(ApplicationGrantAuthorizationError::InvalidAuthorization)?;
        if !requested_rights.valid_for(resource.kind) {
            return Err(ApplicationGrantAuthorizationError::InvalidRights);
        }
        let Some(index) = self.grants.iter().position(|record| {
            record.is_some_and(|record| {
                record.active() && record.subject == subject && record.resource == resource
            })
        }) else {
            return Err(ApplicationGrantAuthorizationError::NotGranted);
        };
        let record = self.grants[index].expect("matched grant exists");
        if !record.rights.contains(requested_rights) {
            return Err(ApplicationGrantAuthorizationError::RightsDenied);
        }
        if matches!(
            record.scope,
            ApplicationGrantScope::Once | ApplicationGrantScope::Session
        ) && record.session != authorization.identity().session
        {
            return Err(ApplicationGrantAuthorizationError::ScopeExpired);
        }
        let (consumed_revision, next_revision) = if record.scope == ApplicationGrantScope::Once {
            let revision = self.next_revision;
            let next_revision = revision
                .checked_add(1)
                .ok_or(ApplicationGrantAuthorizationError::RevisionExhausted)?;
            (Some(revision), next_revision)
        } else {
            (None, self.next_revision)
        };
        let authorization = ApplicationGrantAuthorization {
            grant_id: record.id,
            grant_revision: record.revision,
            subject: record.subject,
            resource,
            rights: requested_rights,
            scope: record.scope,
        };
        Ok(PreparedApplicationGrant {
            store: self,
            authorization,
            commit: PreparedGrantCommit::Existing {
                index,
                consumed_revision,
                next_revision,
            },
        })
    }

    /// Preflights issuing and authorizing one exact picker selection. No grant is visible and no
    /// counter advances until the returned transaction commits. A one-shot grant commits directly
    /// as a consumed tombstone after its endpoint has been transferred successfully.
    pub fn prepare_issue_authorization(
        &mut self,
        authorization: AuthorizedApplication,
        resource: ApplicationResourceIdentity,
        rights: ApplicationGrantRights,
        scope: ApplicationGrantScope,
    ) -> Result<PreparedApplicationGrant<'_>, ApplicationPermissionStoreError> {
        let subject = ApplicationGrantSubject::from_authorization(authorization)
            .ok_or(ApplicationPermissionStoreError::InvalidAuthorization)?;
        if !rights.valid_for(resource.kind) {
            return Err(ApplicationPermissionStoreError::InvalidRights);
        }
        if scope == ApplicationGrantScope::Persistent && subject.is_transient() {
            return Err(ApplicationPermissionStoreError::TransientPersistence);
        }
        if self.grants.iter().flatten().any(|record| {
            record.active() && record.subject == subject && record.resource == resource
        }) {
            return Err(ApplicationPermissionStoreError::DuplicateGrant);
        }
        let Some(slot) = self.grants.iter().position(Option::is_none) else {
            return Err(ApplicationPermissionStoreError::Full);
        };
        let id = self.next_grant_id;
        let next_grant_id = id
            .checked_add(1)
            .ok_or(ApplicationPermissionStoreError::GrantIdExhausted)?;
        let revision = self.next_revision;
        let after_issue_revision = revision
            .checked_add(1)
            .ok_or(ApplicationPermissionStoreError::RevisionExhausted)?;
        let record = ApplicationGrantRecord {
            id,
            revision,
            subject,
            resource,
            rights,
            scope,
            session: if scope == ApplicationGrantScope::Persistent {
                0
            } else {
                authorization.identity().session
            },
            state: ApplicationGrantState::Active,
        };
        let (stored_record, next_revision) = if scope == ApplicationGrantScope::Once {
            let next_revision = after_issue_revision
                .checked_add(1)
                .ok_or(ApplicationPermissionStoreError::RevisionExhausted)?;
            (
                ApplicationGrantRecord {
                    revision: after_issue_revision,
                    state: ApplicationGrantState::Revoked(ApplicationGrantRevocation::Consumed),
                    ..record
                },
                next_revision,
            )
        } else {
            (record, after_issue_revision)
        };
        let grant = ApplicationGrantAuthorization {
            grant_id: id,
            grant_revision: revision,
            subject,
            resource,
            rights,
            scope,
        };
        Ok(PreparedApplicationGrant {
            store: self,
            authorization: grant,
            commit: PreparedGrantCommit::Issued {
                slot,
                stored_record,
                next_grant_id,
                next_revision,
            },
        })
    }

    pub fn revoke(
        &mut self,
        grant_id: u64,
        reason: ApplicationGrantRevocation,
    ) -> Result<ApplicationGrantRecord, ApplicationPermissionStoreError> {
        if reason == ApplicationGrantRevocation::Consumed {
            return Err(ApplicationPermissionStoreError::InvalidRevocation);
        }
        let Some(index) = self
            .grants
            .iter()
            .position(|record| record.is_some_and(|record| record.id == grant_id))
        else {
            return Err(ApplicationPermissionStoreError::UnknownGrant);
        };
        let record = self.grants[index].expect("matched grant exists");
        if !record.active() {
            return Err(ApplicationPermissionStoreError::AlreadyRevoked);
        }
        let revision = self.next_revision;
        self.next_revision = revision
            .checked_add(1)
            .ok_or(ApplicationPermissionStoreError::RevisionExhausted)?;
        let revoked = ApplicationGrantRecord {
            revision,
            state: ApplicationGrantState::Revoked(reason),
            ..record
        };
        self.grants[index] = Some(revoked);
        Ok(revoked)
    }

    pub fn reset_application(
        &mut self,
        authorization: AuthorizedApplication,
        reason: ApplicationGrantRevocation,
    ) -> Result<usize, ApplicationPermissionStoreError> {
        let subject = ApplicationGrantSubject::from_authorization(authorization)
            .ok_or(ApplicationPermissionStoreError::InvalidAuthorization)?;
        self.revoke_matching(reason, |record| record.subject == subject)
    }

    pub fn revoke_resource(
        &mut self,
        resource: ApplicationResourceIdentity,
        reason: ApplicationGrantRevocation,
    ) -> Result<usize, ApplicationPermissionStoreError> {
        self.revoke_matching(reason, |record| record.resource == resource)
    }

    pub const fn next_grant_id(&self) -> u64 {
        self.next_grant_id
    }

    pub const fn next_revision(&self) -> u64 {
        self.next_revision
    }

    pub fn records(&self) -> impl Iterator<Item = ApplicationGrantRecord> + '_ {
        self.grants.iter().flatten().copied()
    }

    fn revoke_matching(
        &mut self,
        reason: ApplicationGrantRevocation,
        predicate: impl Fn(ApplicationGrantRecord) -> bool,
    ) -> Result<usize, ApplicationPermissionStoreError> {
        if reason == ApplicationGrantRevocation::Consumed {
            return Err(ApplicationPermissionStoreError::InvalidRevocation);
        }
        let count = self
            .grants
            .iter()
            .flatten()
            .filter(|record| record.active() && predicate(**record))
            .count();
        let next_revision = self
            .next_revision
            .checked_add(count as u64)
            .ok_or(ApplicationPermissionStoreError::RevisionExhausted)?;
        let mut revision = self.next_revision;
        for record in self.grants.iter_mut().flatten() {
            if record.active() && predicate(*record) {
                record.revision = revision;
                record.state = ApplicationGrantState::Revoked(reason);
                revision += 1;
            }
        }
        self.next_revision = next_revision;
        Ok(count)
    }
}

impl Default for ApplicationPermissionStore {
    fn default() -> Self {
        Self::new()
    }
}

fn trust_class_from_raw(value: u8) -> Option<ApplicationTrustClass> {
    match value {
        1 => Some(ApplicationTrustClass::System),
        2 => Some(ApplicationTrustClass::Repository),
        3 => Some(ApplicationTrustClass::LocalDeveloper),
        4 => Some(ApplicationTrustClass::Transient),
        _ => None,
    }
}

fn install_scope_from_raw(value: u8) -> Option<ApplicationInstallScope> {
    match value {
        1 => Some(ApplicationInstallScope::SystemGeneration),
        2 => Some(ApplicationInstallScope::Machine),
        3 => Some(ApplicationInstallScope::User),
        4 => Some(ApplicationInstallScope::Transient),
        _ => None,
    }
}

fn write_u64(bytes: &mut [u8; APPLICATION_GRANT_RECORD_BYTES], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("u64 field"))
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 field"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application_identity::{
        ApplicationInstallation, ApplicationLaunchSelection, ApplicationProfileSet,
        InstalledApplicationComponent, PackageVerification, authorize_application_launch,
    };

    fn authorization(
        package_generation: u64,
        publisher: u64,
        installation_id: u64,
        session: u64,
        trust_class: ApplicationTrustClass,
        install_scope: ApplicationInstallScope,
    ) -> AuthorizedApplication {
        let components = [InstalledApplicationComponent::new(
            21,
            b"/application",
            ApplicationProfileSet::DESKTOP,
            true,
        )];
        let system = trust_class == ApplicationTrustClass::System;
        authorize_application_launch(
            PackageVerification {
                package: 11,
                package_generation,
                application: 13,
                publisher,
                signing_lineage: 15,
                trust_class,
                system_application: system,
                components: &components,
            },
            ApplicationInstallation {
                installation: installation_id,
                package: 11,
                package_generation,
                application: 13,
                publisher,
                signing_lineage: 15,
                trust_class,
                scope: install_scope,
                owner_user: if matches!(
                    install_scope,
                    ApplicationInstallScope::User | ApplicationInstallScope::Transient
                ) {
                    17
                } else {
                    0
                },
                system_application: system,
            },
            ApplicationLaunchSelection {
                component: 21,
                user: 17,
                session,
                profile: ApplicationProfile::Desktop,
            },
        )
        .unwrap()
    }

    fn repository_authorization(session: u64) -> AuthorizedApplication {
        authorization(
            12,
            14,
            16,
            session,
            ApplicationTrustClass::Repository,
            ApplicationInstallScope::User,
        )
    }

    fn resource(object_id: u64, kind: ApplicationResourceKind) -> ApplicationResourceIdentity {
        ApplicationResourceIdentity::new([0x5a; 16], object_id, 91, kind).unwrap()
    }

    #[test]
    fn resource_identity_and_kind_specific_rights_are_strict() {
        assert!(
            ApplicationResourceIdentity::new([0; 16], 1, 1, ApplicationResourceKind::File)
                .is_none()
        );
        assert!(
            ApplicationResourceIdentity::new([1; 16], 0, 1, ApplicationResourceKind::File)
                .is_none()
        );
        assert!(
            ApplicationResourceIdentity::new([1; 16], 1, 0, ApplicationResourceKind::File)
                .is_none()
        );
        assert!(ApplicationGrantRights::READ.valid_for(ApplicationResourceKind::File));
        assert!(
            !(ApplicationGrantRights::READ | ApplicationGrantRights::ENUMERATE)
                .valid_for(ApplicationResourceKind::File)
        );
        assert!(
            (ApplicationGrantRights::READ | ApplicationGrantRights::ENUMERATE)
                .valid_for(ApplicationResourceKind::Directory)
        );
        assert!(!ApplicationGrantRights::EMPTY.valid_for(ApplicationResourceKind::Directory));
    }

    #[test]
    fn resource_resolution_pins_volume_generation_and_kind() {
        let filesystem_uuid = [0x51; 16];
        let stable = filesystem::protocol::StableNodeIdentity::new(
            filesystem_uuid,
            41,
            43,
            filesystem::protocol::node_kind::FILE,
        )
        .unwrap();
        let resolved =
            validate_resolved_identity(filesystem_uuid, stable, ApplicationResourceKind::File)
                .unwrap();
        assert_eq!(resolved.filesystem_uuid(), filesystem_uuid);
        assert_eq!(resolved.object_id(), 41);
        assert_eq!(resolved.object_generation(), 43);
        assert_eq!(resolved.kind(), ApplicationResourceKind::File);

        assert_eq!(
            validate_resolved_identity([0x52; 16], stable, ApplicationResourceKind::File),
            Err(ApplicationResourceResolveError::FilesystemMismatch)
        );
        assert_eq!(
            validate_resolved_identity(filesystem_uuid, stable, ApplicationResourceKind::Directory),
            Err(ApplicationResourceResolveError::KindMismatch)
        );
        let symlink = filesystem::protocol::StableNodeIdentity::new(
            filesystem_uuid,
            41,
            43,
            filesystem::protocol::node_kind::SYMBOLIC_LINK,
        )
        .unwrap();
        assert_eq!(
            validate_resolved_identity(filesystem_uuid, symlink, ApplicationResourceKind::File),
            Err(ApplicationResourceResolveError::UnsupportedKind)
        );
        let mut noncanonical = stable;
        noncanonical.reserved[0] = 1;
        assert_eq!(
            validate_resolved_identity(
                filesystem_uuid,
                noncanonical,
                ApplicationResourceKind::File,
            ),
            Err(ApplicationResourceResolveError::InvalidIdentity)
        );
    }

    #[test]
    fn records_round_trip_and_reject_corruption_and_noncanonical_fields() {
        let mut store = ApplicationPermissionStore::new();
        let record = store
            .issue(
                repository_authorization(18),
                resource(41, ApplicationResourceKind::File),
                ApplicationGrantRights::READ | ApplicationGrantRights::WRITE,
                ApplicationGrantScope::Persistent,
            )
            .unwrap();
        let encoded = record.encode();
        let golden = [
            0x4e, 0x53, 0x50, 0x47, 0x01, 0x00, 0x01, 0x03, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x11, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0e, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0x00, 0x01, 0x03, 0x00,
            0x00, 0x00, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a, 0x5a,
            0x5a, 0x5a, 0x5a, 0x5a, 0x29, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5b, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x76, 0x37,
            0xb7, 0xe6,
        ];
        assert_eq!(encoded.len(), APPLICATION_GRANT_RECORD_BYTES);
        assert_eq!(&encoded[0..4], b"NSPG");
        assert_eq!(encoded, golden);
        assert_eq!(ApplicationGrantRecord::decode(&encoded), Ok(record));

        let mut corrupted = encoded;
        corrupted[90] ^= 1;
        assert_eq!(
            ApplicationGrantRecord::decode(&corrupted),
            Err(ApplicationGrantDecodeError::Checksum)
        );
        let mut reserved = encoded;
        reserved[71] = 1;
        let checksum = crc32c(&reserved[..CHECKSUM_OFFSET]);
        reserved[CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(
            ApplicationGrantRecord::decode(&reserved),
            Err(ApplicationGrantDecodeError::Reserved)
        );
    }

    #[test]
    fn once_grants_are_consumed_atomically_and_cannot_escalate() {
        let authorization = repository_authorization(18);
        let selected = resource(42, ApplicationResourceKind::File);
        let mut store = ApplicationPermissionStore::new();
        let grant = store
            .issue(
                authorization,
                selected,
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Once,
            )
            .unwrap();
        assert_eq!(
            store.authorize(
                authorization,
                selected,
                ApplicationGrantRights::READ | ApplicationGrantRights::WRITE,
            ),
            Err(ApplicationGrantAuthorizationError::RightsDenied)
        );
        let authorized = store
            .authorize(authorization, selected, ApplicationGrantRights::READ)
            .unwrap();
        assert_eq!(authorized.grant_id(), grant.id());
        assert_eq!(authorized.rights(), ApplicationGrantRights::READ);
        assert_eq!(
            store.authorize(authorization, selected, ApplicationGrantRights::READ),
            Err(ApplicationGrantAuthorizationError::NotGranted)
        );
        assert_eq!(
            store.records().next().unwrap().state(),
            ApplicationGrantState::Revoked(ApplicationGrantRevocation::Consumed)
        );
    }

    #[test]
    fn prepared_once_authorization_changes_state_only_on_commit() {
        let authorization = repository_authorization(18);
        let selected = resource(52, ApplicationResourceKind::File);
        let mut store = ApplicationPermissionStore::new();
        let grant = store
            .issue(
                authorization,
                selected,
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Once,
            )
            .unwrap();
        let next_revision = store.next_revision();

        {
            let prepared = store
                .prepare_authorization(authorization, selected, ApplicationGrantRights::READ)
                .unwrap();
            assert_eq!(prepared.authorization().grant_id(), grant.id());
        }
        assert!(store.records().next().unwrap().active());
        assert_eq!(store.next_revision(), next_revision);

        let prepared = store
            .prepare_authorization(authorization, selected, ApplicationGrantRights::READ)
            .unwrap();
        let authorized = prepared.commit();
        assert_eq!(authorized.grant_revision(), grant.revision());
        assert_eq!(store.next_revision(), next_revision + 1);
        assert_eq!(
            store.records().next().unwrap().state(),
            ApplicationGrantState::Revoked(ApplicationGrantRevocation::Consumed)
        );
    }

    #[test]
    fn prepared_issue_rolls_back_all_counters_and_commits_one_shot_tombstone() {
        let authorization = repository_authorization(18);
        let selected = resource(53, ApplicationResourceKind::File);
        let mut store = ApplicationPermissionStore::new();

        {
            let prepared = store
                .prepare_issue_authorization(
                    authorization,
                    selected,
                    ApplicationGrantRights::READ,
                    ApplicationGrantScope::Once,
                )
                .unwrap();
            assert_eq!(prepared.authorization().grant_id(), 1);
            assert_eq!(prepared.authorization().grant_revision(), 1);
        }
        assert_eq!(store.records().count(), 0);
        assert_eq!(store.next_grant_id(), 1);
        assert_eq!(store.next_revision(), 1);

        let prepared = store
            .prepare_issue_authorization(
                authorization,
                selected,
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Once,
            )
            .unwrap();
        let authorized = prepared.commit();
        assert_eq!(authorized.grant_id(), 1);
        assert_eq!(store.next_grant_id(), 2);
        assert_eq!(store.next_revision(), 3);
        let record = store.records().next().unwrap();
        assert_eq!(record.id(), authorized.grant_id());
        assert_eq!(record.revision(), 2);
        assert_eq!(
            record.state(),
            ApplicationGrantState::Revoked(ApplicationGrantRevocation::Consumed)
        );
    }

    #[test]
    fn prepared_reusable_issue_publishes_the_reserved_active_record() {
        let authorization = repository_authorization(18);
        let selected = resource(54, ApplicationResourceKind::Directory);
        let mut store = ApplicationPermissionStore::new();
        let prepared = store
            .prepare_issue_authorization(
                authorization,
                selected,
                ApplicationGrantRights::READ | ApplicationGrantRights::ENUMERATE,
                ApplicationGrantScope::Session,
            )
            .unwrap();
        let authorized = prepared.commit();

        assert_eq!(store.next_grant_id(), 2);
        assert_eq!(store.next_revision(), 2);
        let record = store.records().next().unwrap();
        assert!(record.active());
        assert_eq!(record.id(), authorized.grant_id());
        assert_eq!(record.revision(), authorized.grant_revision());
        assert_eq!(record.rights(), authorized.rights());
        assert_eq!(record.scope(), ApplicationGrantScope::Session);
    }

    #[test]
    fn session_grants_do_not_cross_sessions_but_persistent_grants_survive_updates() {
        let selected = resource(43, ApplicationResourceKind::Directory);
        let mut sessions = ApplicationPermissionStore::new();
        sessions
            .issue(
                repository_authorization(18),
                selected,
                ApplicationGrantRights::READ | ApplicationGrantRights::ENUMERATE,
                ApplicationGrantScope::Session,
            )
            .unwrap();
        assert_eq!(
            sessions.authorize(
                repository_authorization(19),
                selected,
                ApplicationGrantRights::READ,
            ),
            Err(ApplicationGrantAuthorizationError::ScopeExpired)
        );

        let mut persistent = ApplicationPermissionStore::new();
        persistent
            .issue(
                repository_authorization(18),
                selected,
                ApplicationGrantRights::READ | ApplicationGrantRights::ENUMERATE,
                ApplicationGrantScope::Persistent,
            )
            .unwrap();
        let updated = authorization(
            13,
            14,
            16,
            20,
            ApplicationTrustClass::Repository,
            ApplicationInstallScope::User,
        );
        assert!(
            persistent
                .authorize(updated, selected, ApplicationGrantRights::ENUMERATE)
                .is_ok()
        );
        let reused_inode = ApplicationResourceIdentity::new(
            selected.filesystem_uuid(),
            selected.object_id(),
            selected.object_generation() + 1,
            selected.kind(),
        )
        .unwrap();
        assert_eq!(
            persistent.authorize(updated, reused_inode, ApplicationGrantRights::ENUMERATE),
            Err(ApplicationGrantAuthorizationError::NotGranted)
        );
    }

    #[test]
    fn publisher_installation_and_transient_policy_fail_closed() {
        let selected = resource(44, ApplicationResourceKind::File);
        let original = repository_authorization(18);
        let mut store = ApplicationPermissionStore::new();
        store
            .issue(
                original,
                selected,
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Persistent,
            )
            .unwrap();
        let wrong_publisher = authorization(
            12,
            99,
            16,
            18,
            ApplicationTrustClass::Repository,
            ApplicationInstallScope::User,
        );
        let reinstalled = authorization(
            12,
            14,
            99,
            18,
            ApplicationTrustClass::Repository,
            ApplicationInstallScope::User,
        );
        assert_eq!(
            store.authorize(wrong_publisher, selected, ApplicationGrantRights::READ),
            Err(ApplicationGrantAuthorizationError::NotGranted)
        );
        assert_eq!(
            store.authorize(reinstalled, selected, ApplicationGrantRights::READ),
            Err(ApplicationGrantAuthorizationError::NotGranted)
        );

        let transient = authorization(
            12,
            14,
            20,
            18,
            ApplicationTrustClass::Transient,
            ApplicationInstallScope::Transient,
        );
        assert_eq!(
            ApplicationPermissionStore::new().issue(
                transient,
                selected,
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Persistent,
            ),
            Err(ApplicationPermissionStoreError::TransientPersistence)
        );
    }

    #[test]
    fn revocation_reset_and_checkpoint_restore_preserve_tombstones() {
        let authorization = repository_authorization(18);
        let first_resource = resource(45, ApplicationResourceKind::File);
        let second_resource = resource(46, ApplicationResourceKind::Directory);
        let mut store = ApplicationPermissionStore::new();
        let first = store
            .issue(
                authorization,
                first_resource,
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Persistent,
            )
            .unwrap();
        store
            .issue(
                authorization,
                second_resource,
                ApplicationGrantRights::ENUMERATE,
                ApplicationGrantScope::Persistent,
            )
            .unwrap();
        store
            .revoke(first.id(), ApplicationGrantRevocation::Consumed)
            .expect_err("only one-shot authorization may consume a grant");
        store
            .revoke(first.id(), ApplicationGrantRevocation::User)
            .unwrap();
        assert_eq!(
            store.reset_application(authorization, ApplicationGrantRevocation::Reset),
            Ok(1)
        );
        let records = [
            store.records().next().unwrap(),
            store.records().nth(1).unwrap(),
        ];
        let restored = ApplicationPermissionStore::restore_checkpoint(
            &records,
            store.next_grant_id(),
            store.next_revision(),
        )
        .unwrap();
        assert_eq!(restored.records().count(), 2);
        assert!(restored.records().all(|record| !record.active()));
    }

    #[test]
    fn checkpoint_counters_duplicates_and_capacity_are_bounded() {
        let authorization = repository_authorization(18);
        let mut store = ApplicationPermissionStore::new();
        let record = store
            .issue(
                authorization,
                resource(47, ApplicationResourceKind::File),
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Persistent,
            )
            .unwrap();
        assert!(matches!(
            ApplicationPermissionStore::restore_checkpoint(&[record], record.id(), 2),
            Err(ApplicationPermissionLoadError::InvalidRecord)
        ));
        assert!(matches!(
            ApplicationPermissionStore::restore_checkpoint(&[record, record], 2, 2),
            Err(ApplicationPermissionLoadError::DuplicateGrantId)
        ));
        let duplicate_active = ApplicationGrantRecord {
            id: 2,
            revision: 2,
            ..record
        };
        assert!(matches!(
            ApplicationPermissionStore::restore_checkpoint(&[record, duplicate_active], 3, 3),
            Err(ApplicationPermissionLoadError::DuplicateActiveGrant)
        ));

        let mut exhausted_ids =
            ApplicationPermissionStore::restore_checkpoint(&[], u64::MAX, 1).unwrap();
        assert_eq!(
            exhausted_ids.issue(
                authorization,
                resource(48, ApplicationResourceKind::File),
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Persistent,
            ),
            Err(ApplicationPermissionStoreError::GrantIdExhausted)
        );
        let mut exhausted_revisions =
            ApplicationPermissionStore::restore_checkpoint(&[], 1, u64::MAX).unwrap();
        assert_eq!(
            exhausted_revisions.issue(
                authorization,
                resource(49, ApplicationResourceKind::File),
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Persistent,
            ),
            Err(ApplicationPermissionStoreError::RevisionExhausted)
        );

        let mut full = ApplicationPermissionStore::new();
        for object in 1..=MAX_APPLICATION_GRANTS as u64 {
            full.issue(
                authorization,
                resource(1_000 + object, ApplicationResourceKind::File),
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Persistent,
            )
            .unwrap();
        }
        assert_eq!(
            full.issue(
                authorization,
                resource(2_000, ApplicationResourceKind::File),
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Persistent,
            ),
            Err(ApplicationPermissionStoreError::Full)
        );
    }
}
