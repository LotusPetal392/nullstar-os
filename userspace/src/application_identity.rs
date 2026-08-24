//! Stable verified application identity and installed-generation policy.
//!
//! Cryptographic package verification remains the responsibility of the
//! package service. This module consumes that service's bounded verification
//! result and independently matches it against the application registry's
//! installation record before producing an opaque launch authorization.

use crate::managed_startup::numeric_executable_id;

pub const MAX_APPLICATION_COMPONENTS: usize = 8;
pub const APPLICATION_IDENTITY_METADATA_BYTES: usize = 72;

pub const DESKTOP_NAMESPACE_PROFILE_ID: u64 = 2;
pub const DESKTOP_CHILD_NAMESPACE_PROFILE_ID: u64 = 3;
pub const WORKER_NAMESPACE_PROFILE_ID: u64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationProfile {
    Desktop,
    DesktopChild,
    Worker,
}

impl ApplicationProfile {
    pub const fn namespace_profile_id(self) -> u64 {
        match self {
            Self::Desktop => DESKTOP_NAMESPACE_PROFILE_ID,
            Self::DesktopChild => DESKTOP_CHILD_NAMESPACE_PROFILE_ID,
            Self::Worker => WORKER_NAMESPACE_PROFILE_ID,
        }
    }

    pub const fn from_namespace_profile_id(value: u64) -> Option<Self> {
        match value {
            DESKTOP_NAMESPACE_PROFILE_ID => Some(Self::Desktop),
            DESKTOP_CHILD_NAMESPACE_PROFILE_ID => Some(Self::DesktopChild),
            WORKER_NAMESPACE_PROFILE_ID => Some(Self::Worker),
            _ => None,
        }
    }

    pub const fn is_reduced_component(self) -> bool {
        matches!(self, Self::DesktopChild | Self::Worker)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationProfileSet(u8);

impl ApplicationProfileSet {
    pub const DESKTOP: Self = Self(1 << 0);
    pub const DESKTOP_CHILD: Self = Self(1 << 1);
    pub const WORKER: Self = Self(1 << 2);
    pub const ALL_REDUCED: Self = Self(Self::DESKTOP_CHILD.0 | Self::WORKER.0);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, profile: ApplicationProfile) -> bool {
        let required = match profile {
            ApplicationProfile::Desktop => Self::DESKTOP.0,
            ApplicationProfile::DesktopChild => Self::DESKTOP_CHILD.0,
            ApplicationProfile::Worker => Self::WORKER.0,
        };
        self.0 & required == required
    }

    const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Runtime identity shared by a root process and its declared components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationIdentity {
    pub package: u64,
    pub package_generation: u64,
    pub application: u64,
    pub component: u64,
    pub user: u64,
    pub session: u64,
}

impl ApplicationIdentity {
    pub const fn is_valid(self) -> bool {
        self.package != 0
            && self.package_generation != 0
            && self.application != 0
            && self.component != 0
            && self.user != 0
            && self.session != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum ApplicationTrustClass {
    System = 1,
    Repository = 2,
    LocalDeveloper = 3,
    Transient = 4,
}

impl ApplicationTrustClass {
    const fn from_raw(value: u64) -> Option<Self> {
        match value {
            1 => Some(Self::System),
            2 => Some(Self::Repository),
            3 => Some(Self::LocalDeveloper),
            4 => Some(Self::Transient),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum ApplicationInstallScope {
    SystemGeneration = 1,
    Machine = 2,
    User = 3,
    Transient = 4,
}

impl ApplicationInstallScope {
    const fn from_raw(value: u64) -> Option<Self> {
        match value {
            1 => Some(Self::SystemGeneration),
            2 => Some(Self::Machine),
            3 => Some(Self::User),
            4 => Some(Self::Transient),
            _ => None,
        }
    }
}

/// Output of the trusted package verifier for one immutable package generation.
///
/// Constructing this record does not perform cryptography. The application
/// manager must accept it only from its authenticated package-verifier route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageVerification<'a> {
    pub package: u64,
    pub package_generation: u64,
    pub application: u64,
    pub publisher: u64,
    pub signing_lineage: u64,
    pub trust_class: ApplicationTrustClass,
    pub system_application: bool,
    /// Component declarations authenticated as part of the package manifest.
    pub components: &'a [InstalledApplicationComponent],
}

/// One component declaration from the verified installed application manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstalledApplicationComponent {
    pub component: u64,
    pub executable: u64,
    pub profiles: ApplicationProfileSet,
    pub entry: bool,
}

impl InstalledApplicationComponent {
    pub fn new(
        component: u64,
        executable_path: &[u8],
        profiles: ApplicationProfileSet,
        entry: bool,
    ) -> Self {
        Self {
            component,
            executable: numeric_executable_id(executable_path),
            profiles,
            entry,
        }
    }

    const EMPTY: Self = Self {
        component: 0,
        executable: 0,
        profiles: ApplicationProfileSet(0),
        entry: false,
    };
}

/// Registry state selecting one verified immutable generation for launch.
#[derive(Debug, Clone, Copy)]
pub struct ApplicationInstallation {
    pub installation: u64,
    pub package: u64,
    pub package_generation: u64,
    pub application: u64,
    pub publisher: u64,
    pub signing_lineage: u64,
    pub trust_class: ApplicationTrustClass,
    pub scope: ApplicationInstallScope,
    /// Zero for system/machine installations; the owning user otherwise.
    pub owner_user: u64,
    pub system_application: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationLaunchSelection {
    pub component: u64,
    pub user: u64,
    pub session: u64,
    pub profile: ApplicationProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StableApplicationPrincipal {
    pub application: u64,
    pub publisher: u64,
    pub signing_lineage: u64,
    pub trust_class: ApplicationTrustClass,
    pub system_application: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationInstallationProvenance {
    pub installation: u64,
    pub scope: ApplicationInstallScope,
}

/// Opaque proof that package verification, installation provenance, component,
/// executable, user scope, and profile policy agreed for one launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorizedApplication {
    identity: ApplicationIdentity,
    principal: StableApplicationPrincipal,
    provenance: ApplicationInstallationProvenance,
    profile: ApplicationProfile,
    components: [InstalledApplicationComponent; MAX_APPLICATION_COMPONENTS],
    component_count: usize,
}

impl AuthorizedApplication {
    pub const fn identity(self) -> ApplicationIdentity {
        self.identity
    }

    pub const fn principal(self) -> StableApplicationPrincipal {
        self.principal
    }

    pub const fn provenance(self) -> ApplicationInstallationProvenance {
        self.provenance
    }

    pub const fn profile(self) -> ApplicationProfile {
        self.profile
    }

    pub(crate) fn authorizes_executable(self, executable_path: &[u8]) -> bool {
        self.components[..self.component_count]
            .iter()
            .any(|component| {
                component.component == self.identity.component
                    && component.executable == numeric_executable_id(executable_path)
                    && component.profiles.contains(self.profile)
            })
    }

    pub(crate) fn authorize_component(
        self,
        component_id: u64,
        profile: ApplicationProfile,
        executable_path: &[u8],
    ) -> Result<Self, ApplicationIdentityError> {
        if !profile.is_reduced_component() {
            return Err(ApplicationIdentityError::ProfileNotAuthorized);
        }
        let Some(component) = self.components[..self.component_count]
            .iter()
            .find(|component| component.component == component_id)
        else {
            return Err(ApplicationIdentityError::ComponentNotAuthorized);
        };
        if component.entry {
            return Err(ApplicationIdentityError::ComponentNotAuthorized);
        }
        if !component.profiles.contains(profile) {
            return Err(ApplicationIdentityError::ProfileNotAuthorized);
        }
        if component.executable != numeric_executable_id(executable_path) {
            return Err(ApplicationIdentityError::ExecutableNotAuthorized);
        }
        Ok(Self {
            identity: ApplicationIdentity {
                component: component_id,
                ..self.identity
            },
            profile,
            ..self
        })
    }

    pub(crate) fn encode_metadata(self) -> [u8; APPLICATION_IDENTITY_METADATA_BYTES] {
        ApplicationIdentityMetadata {
            package: self.identity.package,
            package_generation: self.identity.package_generation,
            principal: self.principal,
            provenance: self.provenance,
        }
        .encode()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationIdentityError {
    InvalidVerification,
    InvalidInstallation,
    PackageMismatch,
    PackageGenerationMismatch,
    ApplicationMismatch,
    PublisherMismatch,
    SigningLineageMismatch,
    TrustClassMismatch,
    SystemPolicyMismatch,
    UserScopeMismatch,
    ComponentNotAuthorized,
    ProfileNotAuthorized,
    ExecutableNotAuthorized,
}

pub fn authorize_application_launch(
    verification: PackageVerification<'_>,
    installation: ApplicationInstallation,
    selection: ApplicationLaunchSelection,
) -> Result<AuthorizedApplication, ApplicationIdentityError> {
    validate_verification(verification)?;
    validate_installation(installation)?;
    if verification.package != installation.package {
        return Err(ApplicationIdentityError::PackageMismatch);
    }
    if verification.package_generation != installation.package_generation {
        return Err(ApplicationIdentityError::PackageGenerationMismatch);
    }
    if verification.application != installation.application {
        return Err(ApplicationIdentityError::ApplicationMismatch);
    }
    if verification.publisher != installation.publisher {
        return Err(ApplicationIdentityError::PublisherMismatch);
    }
    if verification.signing_lineage != installation.signing_lineage {
        return Err(ApplicationIdentityError::SigningLineageMismatch);
    }
    if verification.trust_class != installation.trust_class {
        return Err(ApplicationIdentityError::TrustClassMismatch);
    }
    if verification.system_application != installation.system_application {
        return Err(ApplicationIdentityError::SystemPolicyMismatch);
    }
    if selection.user == 0 || selection.session == 0 {
        return Err(ApplicationIdentityError::UserScopeMismatch);
    }
    if installation.owner_user != 0 && installation.owner_user != selection.user {
        return Err(ApplicationIdentityError::UserScopeMismatch);
    }
    let Some(entry) = verification
        .components
        .iter()
        .find(|component| component.component == selection.component)
    else {
        return Err(ApplicationIdentityError::ComponentNotAuthorized);
    };
    if !entry.entry {
        return Err(ApplicationIdentityError::ComponentNotAuthorized);
    }
    if selection.profile != ApplicationProfile::Desktop
        || !entry.profiles.contains(selection.profile)
    {
        return Err(ApplicationIdentityError::ProfileNotAuthorized);
    }

    let mut components = [InstalledApplicationComponent::EMPTY; MAX_APPLICATION_COMPONENTS];
    components[..verification.components.len()].copy_from_slice(verification.components);
    Ok(AuthorizedApplication {
        identity: ApplicationIdentity {
            package: installation.package,
            package_generation: installation.package_generation,
            application: installation.application,
            component: selection.component,
            user: selection.user,
            session: selection.session,
        },
        principal: StableApplicationPrincipal {
            application: installation.application,
            publisher: installation.publisher,
            signing_lineage: installation.signing_lineage,
            trust_class: installation.trust_class,
            system_application: installation.system_application,
        },
        provenance: ApplicationInstallationProvenance {
            installation: installation.installation,
            scope: installation.scope,
        },
        profile: selection.profile,
        components,
        component_count: verification.components.len(),
    })
}

fn validate_verification(
    verification: PackageVerification<'_>,
) -> Result<(), ApplicationIdentityError> {
    if verification.package == 0
        || verification.package_generation == 0
        || verification.application == 0
        || verification.publisher == 0
        || verification.signing_lineage == 0
        || verification.components.is_empty()
        || verification.components.len() > MAX_APPLICATION_COMPONENTS
    {
        return Err(ApplicationIdentityError::InvalidVerification);
    }
    if verification.system_application
        != (verification.trust_class == ApplicationTrustClass::System)
    {
        return Err(ApplicationIdentityError::SystemPolicyMismatch);
    }
    validate_components(verification.components)?;
    Ok(())
}

fn validate_installation(
    installation: ApplicationInstallation,
) -> Result<(), ApplicationIdentityError> {
    if installation.installation == 0
        || installation.package == 0
        || installation.package_generation == 0
        || installation.application == 0
        || installation.publisher == 0
        || installation.signing_lineage == 0
    {
        return Err(ApplicationIdentityError::InvalidInstallation);
    }
    let system_scope = installation.scope == ApplicationInstallScope::SystemGeneration;
    if installation.system_application != system_scope
        || installation.system_application
            != (installation.trust_class == ApplicationTrustClass::System)
    {
        return Err(ApplicationIdentityError::SystemPolicyMismatch);
    }
    let user_owned = matches!(
        installation.scope,
        ApplicationInstallScope::User | ApplicationInstallScope::Transient
    );
    if user_owned != (installation.owner_user != 0) {
        return Err(ApplicationIdentityError::UserScopeMismatch);
    }
    Ok(())
}

fn validate_components(
    components: &[InstalledApplicationComponent],
) -> Result<(), ApplicationIdentityError> {
    let mut entries = 0;
    for (index, component) in components.iter().enumerate() {
        if component.component == 0
            || component.executable == 0
            || component.profiles.is_empty()
            || components[..index]
                .iter()
                .any(|other| other.component == component.component)
        {
            return Err(ApplicationIdentityError::InvalidInstallation);
        }
        if component.entry {
            entries += 1;
            if component.profiles != ApplicationProfileSet::DESKTOP {
                return Err(ApplicationIdentityError::InvalidInstallation);
            }
        } else if component.profiles.contains(ApplicationProfile::Desktop) {
            return Err(ApplicationIdentityError::InvalidInstallation);
        }
    }
    if entries != 1 {
        return Err(ApplicationIdentityError::InvalidInstallation);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApplicationIdentityMetadata {
    pub package: u64,
    pub package_generation: u64,
    pub principal: StableApplicationPrincipal,
    pub provenance: ApplicationInstallationProvenance,
}

impl ApplicationIdentityMetadata {
    pub fn encode(self) -> [u8; APPLICATION_IDENTITY_METADATA_BYTES] {
        let mut bytes = [0; APPLICATION_IDENTITY_METADATA_BYTES];
        for (index, value) in [
            self.package,
            self.package_generation,
            self.principal.application,
            self.principal.publisher,
            self.principal.signing_lineage,
            self.provenance.installation,
            self.principal.trust_class as u64,
            self.provenance.scope as u64,
            u64::from(self.principal.system_application),
        ]
        .into_iter()
        .enumerate()
        {
            bytes[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != APPLICATION_IDENTITY_METADATA_BYTES {
            return None;
        }
        let value = |index: usize| -> Option<u64> {
            Some(u64::from_le_bytes(
                bytes[index * 8..index * 8 + 8].try_into().ok()?,
            ))
        };
        let system_application = match value(8)? {
            0 => false,
            1 => true,
            _ => return None,
        };
        let metadata = Self {
            package: value(0)?,
            package_generation: value(1)?,
            principal: StableApplicationPrincipal {
                application: value(2)?,
                publisher: value(3)?,
                signing_lineage: value(4)?,
                trust_class: ApplicationTrustClass::from_raw(value(6)?)?,
                system_application,
            },
            provenance: ApplicationInstallationProvenance {
                installation: value(5)?,
                scope: ApplicationInstallScope::from_raw(value(7)?)?,
            },
        };
        (metadata.package != 0
            && metadata.package_generation != 0
            && metadata.principal.application != 0
            && metadata.principal.publisher != 0
            && metadata.principal.signing_lineage != 0
            && metadata.provenance.installation != 0
            && metadata.principal.system_application
                == (metadata.principal.trust_class == ApplicationTrustClass::System)
            && metadata.principal.system_application
                == (metadata.provenance.scope == ApplicationInstallScope::SystemGeneration))
            .then_some(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPONENTS: [InstalledApplicationComponent; 2] = [
        InstalledApplicationComponent {
            component: 21,
            executable: numeric_executable_id(b"/app"),
            profiles: ApplicationProfileSet::DESKTOP,
            entry: true,
        },
        InstalledApplicationComponent {
            component: 22,
            executable: numeric_executable_id(b"/worker"),
            profiles: ApplicationProfileSet::WORKER,
            entry: false,
        },
    ];
    const PACKAGE: PackageVerification<'static> = PackageVerification {
        package: 11,
        package_generation: 12,
        application: 13,
        publisher: 14,
        signing_lineage: 15,
        trust_class: ApplicationTrustClass::Repository,
        system_application: false,
        components: &COMPONENTS,
    };

    fn installation() -> ApplicationInstallation {
        ApplicationInstallation {
            installation: 16,
            package: 11,
            package_generation: 12,
            application: 13,
            publisher: 14,
            signing_lineage: 15,
            trust_class: ApplicationTrustClass::Repository,
            scope: ApplicationInstallScope::User,
            owner_user: 17,
            system_application: false,
        }
    }

    fn selection() -> ApplicationLaunchSelection {
        ApplicationLaunchSelection {
            component: 21,
            user: 17,
            session: 18,
            profile: ApplicationProfile::Desktop,
        }
    }

    #[test]
    fn verified_package_and_installation_produce_stable_principal() {
        let authorized =
            authorize_application_launch(PACKAGE, installation(), selection()).unwrap();
        assert_eq!(authorized.identity().application, 13);
        assert_eq!(authorized.principal().publisher, 14);
        assert_eq!(authorized.principal().signing_lineage, 15);
        assert_eq!(authorized.provenance().installation, 16);
        assert!(authorized.authorizes_executable(b"/app"));
    }

    #[test]
    fn publisher_lineage_generation_and_user_scope_fail_closed() {
        let mut package = PACKAGE;
        package.publisher = 99;
        assert_eq!(
            authorize_application_launch(package, installation(), selection()),
            Err(ApplicationIdentityError::PublisherMismatch)
        );
        package = PACKAGE;
        package.signing_lineage = 99;
        assert_eq!(
            authorize_application_launch(package, installation(), selection()),
            Err(ApplicationIdentityError::SigningLineageMismatch)
        );
        package = PACKAGE;
        package.package_generation = 99;
        assert_eq!(
            authorize_application_launch(package, installation(), selection()),
            Err(ApplicationIdentityError::PackageGenerationMismatch)
        );
        let mut other_user = selection();
        other_user.user = 99;
        assert_eq!(
            authorize_application_launch(PACKAGE, installation(), other_user),
            Err(ApplicationIdentityError::UserScopeMismatch)
        );
    }

    #[test]
    fn only_declared_entry_and_reduced_components_are_authorized() {
        let mut non_entry = selection();
        non_entry.component = 22;
        non_entry.profile = ApplicationProfile::Worker;
        assert_eq!(
            authorize_application_launch(PACKAGE, installation(), non_entry),
            Err(ApplicationIdentityError::ComponentNotAuthorized)
        );
        let authorized =
            authorize_application_launch(PACKAGE, installation(), selection()).unwrap();
        assert!(
            authorized
                .authorize_component(22, ApplicationProfile::Worker, b"/worker")
                .is_ok()
        );
        assert_eq!(
            authorized.authorize_component(22, ApplicationProfile::DesktopChild, b"/worker"),
            Err(ApplicationIdentityError::ProfileNotAuthorized)
        );
        assert_eq!(
            authorized.authorize_component(22, ApplicationProfile::Worker, b"/other"),
            Err(ApplicationIdentityError::ExecutableNotAuthorized)
        );
    }

    #[test]
    fn stable_identity_metadata_round_trips_and_rejects_invalid_flags() {
        let authorized =
            authorize_application_launch(PACKAGE, installation(), selection()).unwrap();
        let encoded = authorized.encode_metadata();
        let decoded = ApplicationIdentityMetadata::decode(&encoded).unwrap();
        assert_eq!(decoded.principal, authorized.principal());
        assert_eq!(decoded.provenance, authorized.provenance());
        let mut malformed = encoded;
        malformed[64..72].copy_from_slice(&2_u64.to_le_bytes());
        assert_eq!(ApplicationIdentityMetadata::decode(&malformed), None);
    }
}
