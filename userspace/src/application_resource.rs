//! Rights-reduced broker endpoints for application filesystem grants.
//!
//! The endpoint is authority; the grant metadata is not. A broker owns the receive side of one
//! fresh channel pair and transfers only send authority to the selected application. The broker must
//! validate the full generic filesystem request before applying this module's operation gate and
//! translating broker-scoped node IDs to provider-scoped nodes.

use crate::{
    application_permission::{
        ApplicationGrantAuthorization, ApplicationGrantRights, ApplicationResourceKind,
    },
    filesystem::protocol,
    handle::{BorrowedHandle, Endpoint, OwnedHandle},
    ipc::{self, Rights},
};

pub const APPLICATION_RESOURCE_CLIENT_RIGHTS: Rights = Rights::SEND;
pub const APPLICATION_RESOURCE_CLIENT_SOURCE_RIGHTS: Rights = Rights::SEND.union(Rights::TRANSFER);
pub const APPLICATION_RESOURCE_BROKER_RIGHTS: Rights = Rights::RECEIVE.union(Rights::WAIT);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationResourceAccess {
    Session,
    Metadata,
    Open,
    Read,
    Write,
    Enumerate,
    Create,
    Remove,
    Rename,
    Synchronize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationResourceAuthorizationError {
    InvalidFlags,
    ResourceKindDenied,
    RightsDenied,
    UnsupportedOperation,
}

/// Immutable policy binding retained by both sides while a client endpoint is staged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationResourceAuthority {
    grant: ApplicationGrantAuthorization,
}

impl ApplicationResourceAuthority {
    pub const fn new(grant: ApplicationGrantAuthorization) -> Self {
        Self { grant }
    }

    pub const fn grant(self) -> ApplicationGrantAuthorization {
        self.grant
    }

    pub fn matches(self, grant: ApplicationGrantAuthorization) -> bool {
        self.grant.grant_id() == grant.grant_id()
            && self.grant.grant_revision() == grant.grant_revision()
            && self.grant.subject() == grant.subject()
            && self.grant.resource() == grant.resource()
            && self.grant.rights().bits() == grant.rights().bits()
            && self.grant.scope() as u8 == grant.scope() as u8
    }

    /// Applies the grant ceiling to one generic filesystem operation and its flags.
    ///
    /// This is deliberately not a wire-shape validator. The broker must first enforce the
    /// filesystem protocol's canonical request shape, session generation, buffer ownership, and
    /// broker-local node namespace. This gate then prevents policy-right escalation.
    pub fn authorize_operation(
        self,
        operation: u16,
        flags: u32,
    ) -> Result<ApplicationResourceAccess, ApplicationResourceAuthorizationError> {
        use ApplicationResourceAccess as Access;
        use ApplicationResourceAuthorizationError as Error;

        let rights = self.grant.rights();
        match operation {
            protocol::operation::CONNECT => {
                if !matches!(flags, 0 | protocol::connect_flags::WRITE) {
                    return Err(Error::InvalidFlags);
                }
                if flags == protocol::connect_flags::WRITE && !has_mutation_right(rights) {
                    return Err(Error::RightsDenied);
                }
                Ok(Access::Session)
            }
            protocol::operation::ATTACH_BUFFER
            | protocol::operation::DETACH_BUFFER
            | protocol::operation::CLOSE_NODE
            | protocol::operation::CANCEL
            | protocol::operation::DISCONNECT => require_zero_flags(flags, Access::Session),
            protocol::operation::GET_ATTRIBUTES => require_zero_flags(flags, Access::Metadata),
            protocol::operation::OPEN => self.authorize_open(flags),
            protocol::operation::LOOKUP => {
                require_zero_flags(flags, Access::Read)?;
                require_kind(
                    self.grant.resource().kind(),
                    ApplicationResourceKind::Directory,
                )?;
                require_rights(rights, ApplicationGrantRights::READ, Access::Read)
            }
            protocol::operation::READ => {
                require_zero_flags(flags, Access::Read)?;
                require_rights(rights, ApplicationGrantRights::READ, Access::Read)
            }
            protocol::operation::WRITE => {
                if flags & !protocol::request_flags::APPEND != 0 {
                    return Err(Error::InvalidFlags);
                }
                require_rights(rights, ApplicationGrantRights::WRITE, Access::Write)
            }
            protocol::operation::TRUNCATE => {
                require_zero_flags(flags, Access::Write)?;
                require_rights(rights, ApplicationGrantRights::WRITE, Access::Write)
            }
            protocol::operation::READ_DIRECTORY => {
                require_zero_flags(flags, Access::Enumerate)?;
                require_kind(
                    self.grant.resource().kind(),
                    ApplicationResourceKind::Directory,
                )?;
                require_rights(rights, ApplicationGrantRights::ENUMERATE, Access::Enumerate)
            }
            protocol::operation::CREATE_FILE => {
                if flags & !(protocol::request_flags::EXCLUSIVE | protocol::request_flags::TRUNCATE)
                    != 0
                {
                    return Err(Error::InvalidFlags);
                }
                self.authorize_directory_mutation(ApplicationGrantRights::CREATE, Access::Create)?;
                if flags & protocol::request_flags::TRUNCATE != 0 {
                    require_rights(rights, ApplicationGrantRights::WRITE, Access::Create)?;
                }
                Ok(Access::Create)
            }
            protocol::operation::CREATE_DIRECTORY => {
                if flags & !protocol::request_flags::EXCLUSIVE != 0 {
                    return Err(Error::InvalidFlags);
                }
                self.authorize_directory_mutation(ApplicationGrantRights::CREATE, Access::Create)
            }
            protocol::operation::UNLINK | protocol::operation::RMDIR => {
                require_zero_flags(flags, Access::Remove)?;
                self.authorize_directory_mutation(ApplicationGrantRights::REMOVE, Access::Remove)
            }
            protocol::operation::RENAME => {
                require_zero_flags(flags, Access::Rename)?;
                require_kind(
                    self.grant.resource().kind(),
                    ApplicationResourceKind::Directory,
                )?;
                require_rights(
                    rights,
                    ApplicationGrantRights::CREATE.union(ApplicationGrantRights::REMOVE),
                    Access::Rename,
                )
            }
            protocol::operation::SYNC => {
                require_zero_flags(flags, Access::Synchronize)?;
                if has_mutation_right(rights) {
                    Ok(Access::Synchronize)
                } else {
                    Err(Error::RightsDenied)
                }
            }
            protocol::operation::RESOLVE_IDENTITY | protocol::operation::RESTORE_IDENTITY => {
                Err(Error::UnsupportedOperation)
            }
            _ => Err(Error::UnsupportedOperation),
        }
    }

    fn authorize_open(
        self,
        flags: u32,
    ) -> Result<ApplicationResourceAccess, ApplicationResourceAuthorizationError> {
        use ApplicationResourceAuthorizationError as Error;

        if flags & !protocol::request_flags::ALL != 0
            || flags & protocol::request_flags::EXCLUSIVE != 0
                && flags & protocol::request_flags::CREATE == 0
            || flags & (protocol::request_flags::APPEND | protocol::request_flags::TRUNCATE) != 0
                && flags & protocol::request_flags::WRITE == 0
        {
            return Err(Error::InvalidFlags);
        }
        let rights = self.grant.rights();
        if flags & protocol::request_flags::READ != 0
            && !rights.contains(ApplicationGrantRights::READ)
            || flags
                & (protocol::request_flags::WRITE
                    | protocol::request_flags::APPEND
                    | protocol::request_flags::TRUNCATE)
                != 0
                && !rights.contains(ApplicationGrantRights::WRITE)
        {
            return Err(Error::RightsDenied);
        }
        if flags & (protocol::request_flags::CREATE | protocol::request_flags::EXCLUSIVE) != 0 {
            require_kind(
                self.grant.resource().kind(),
                ApplicationResourceKind::Directory,
            )?;
            if !rights.contains(ApplicationGrantRights::CREATE) {
                return Err(Error::RightsDenied);
            }
        }
        Ok(ApplicationResourceAccess::Open)
    }

    fn authorize_directory_mutation(
        self,
        required: ApplicationGrantRights,
        access: ApplicationResourceAccess,
    ) -> Result<ApplicationResourceAccess, ApplicationResourceAuthorizationError> {
        require_kind(
            self.grant.resource().kind(),
            ApplicationResourceKind::Directory,
        )?;
        require_rights(self.grant.rights(), required, access)
    }
}

/// Broker-owned receive authority for one grant-bound filesystem adapter.
#[derive(Debug)]
pub struct ApplicationResourceBroker {
    authority: ApplicationResourceAuthority,
    endpoint: OwnedHandle<Endpoint>,
}

/// Transfer-staging handle for the application side of one resource broker.
#[derive(Debug)]
pub struct ApplicationResourceClientEndpoint {
    authority: ApplicationResourceAuthority,
    endpoint: OwnedHandle<Endpoint>,
}

impl ApplicationResourceBroker {
    /// Creates a fresh, non-aliased channel pair and irreversibly reduces both local handles.
    pub fn mint(
        grant: ApplicationGrantAuthorization,
    ) -> ipc::Result<(Self, ApplicationResourceClientEndpoint)> {
        let authority = ApplicationResourceAuthority::new(grant);
        let (mut endpoint, mut client) = OwnedHandle::<Endpoint>::create_pair()?;
        endpoint.replace_rights(APPLICATION_RESOURCE_BROKER_RIGHTS)?;
        client.replace_rights(APPLICATION_RESOURCE_CLIENT_SOURCE_RIGHTS)?;
        Ok((
            Self {
                authority,
                endpoint,
            },
            ApplicationResourceClientEndpoint {
                authority,
                endpoint: client,
            },
        ))
    }

    pub const fn authority(&self) -> ApplicationResourceAuthority {
        self.authority
    }

    pub fn endpoint(&self) -> BorrowedHandle<'_, Endpoint> {
        self.endpoint.borrow()
    }

    pub(crate) fn try_receive(
        &self,
        output: &mut [u8],
    ) -> ipc::Result<crate::handle::ReceivedMessage> {
        self.endpoint.try_receive(output)
    }

    pub fn authorize_operation(
        &self,
        operation: u16,
        flags: u32,
    ) -> Result<ApplicationResourceAccess, ApplicationResourceAuthorizationError> {
        self.authority.authorize_operation(operation, flags)
    }
}

impl ApplicationResourceClientEndpoint {
    pub const fn authority(&self) -> ApplicationResourceAuthority {
        self.authority
    }

    pub fn matches(&self, grant: ApplicationGrantAuthorization) -> bool {
        self.authority.matches(grant)
    }

    pub fn endpoint(&self) -> BorrowedHandle<'_, Endpoint> {
        self.endpoint.borrow()
    }

    /// Consumes the staging wrapper so the portal can move the handle with
    /// [`APPLICATION_RESOURCE_CLIENT_RIGHTS`].
    pub fn into_endpoint(self) -> OwnedHandle<Endpoint> {
        self.endpoint
    }
}

fn require_zero_flags(
    flags: u32,
    access: ApplicationResourceAccess,
) -> Result<ApplicationResourceAccess, ApplicationResourceAuthorizationError> {
    if flags == 0 {
        Ok(access)
    } else {
        Err(ApplicationResourceAuthorizationError::InvalidFlags)
    }
}

fn require_kind(
    actual: ApplicationResourceKind,
    required: ApplicationResourceKind,
) -> Result<(), ApplicationResourceAuthorizationError> {
    if actual as u8 == required as u8 {
        Ok(())
    } else {
        Err(ApplicationResourceAuthorizationError::ResourceKindDenied)
    }
}

fn require_rights(
    actual: ApplicationGrantRights,
    required: ApplicationGrantRights,
    access: ApplicationResourceAccess,
) -> Result<ApplicationResourceAccess, ApplicationResourceAuthorizationError> {
    if actual.contains(required) {
        Ok(access)
    } else {
        Err(ApplicationResourceAuthorizationError::RightsDenied)
    }
}

fn has_mutation_right(rights: ApplicationGrantRights) -> bool {
    rights.bits()
        & (ApplicationGrantRights::WRITE.bits()
            | ApplicationGrantRights::CREATE.bits()
            | ApplicationGrantRights::REMOVE.bits())
        != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application_identity::{
            ApplicationInstallScope, ApplicationInstallation, ApplicationLaunchSelection,
            ApplicationProfile, ApplicationProfileSet, ApplicationTrustClass,
            InstalledApplicationComponent, PackageVerification, authorize_application_launch,
        },
        application_permission::{
            ApplicationGrantScope, ApplicationPermissionStore, ApplicationResourceIdentity,
        },
    };

    fn authority(
        kind: ApplicationResourceKind,
        rights: ApplicationGrantRights,
    ) -> ApplicationResourceAuthority {
        let components = [InstalledApplicationComponent::new(
            21,
            b"/resource-test",
            ApplicationProfileSet::DESKTOP,
            true,
        )];
        let authorization = authorize_application_launch(
            PackageVerification {
                package: 11,
                package_generation: 12,
                application: 13,
                publisher: 14,
                signing_lineage: 15,
                trust_class: ApplicationTrustClass::Repository,
                system_application: false,
                components: &components,
            },
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
            },
            ApplicationLaunchSelection {
                component: 21,
                user: 17,
                session: 18,
                profile: ApplicationProfile::Desktop,
            },
        )
        .unwrap();
        let resource = ApplicationResourceIdentity::new([0x51; 16], 41, 43, kind).unwrap();
        let mut store = ApplicationPermissionStore::new();
        store
            .issue(
                authorization,
                resource,
                rights,
                ApplicationGrantScope::Session,
            )
            .unwrap();
        ApplicationResourceAuthority::new(store.authorize(authorization, resource, rights).unwrap())
    }

    #[test]
    fn file_authority_is_exact_and_never_executes_or_mutates_without_rights() {
        let read = authority(ApplicationResourceKind::File, ApplicationGrantRights::READ);
        assert_eq!(
            read.authorize_operation(protocol::operation::READ, 0),
            Ok(ApplicationResourceAccess::Read)
        );
        assert_eq!(
            read.authorize_operation(protocol::operation::WRITE, 0),
            Err(ApplicationResourceAuthorizationError::RightsDenied)
        );
        assert_eq!(
            read.authorize_operation(protocol::operation::READ_DIRECTORY, 0),
            Err(ApplicationResourceAuthorizationError::ResourceKindDenied)
        );
        assert_eq!(
            read.authorize_operation(protocol::operation::RESOLVE_IDENTITY, 0),
            Err(ApplicationResourceAuthorizationError::UnsupportedOperation)
        );
        assert_eq!(
            read.authorize_operation(protocol::operation::RESTORE_IDENTITY, 0),
            Err(ApplicationResourceAuthorizationError::UnsupportedOperation)
        );
        assert_eq!(
            read.authorize_operation(protocol::operation::CONNECT, protocol::connect_flags::WRITE),
            Err(ApplicationResourceAuthorizationError::RightsDenied)
        );
    }

    #[test]
    fn directory_authority_requires_each_named_operation_right() {
        let authority = authority(
            ApplicationResourceKind::Directory,
            ApplicationGrantRights::READ
                | ApplicationGrantRights::CREATE
                | ApplicationGrantRights::ENUMERATE,
        );
        assert_eq!(
            authority.authorize_operation(protocol::operation::LOOKUP, 0),
            Ok(ApplicationResourceAccess::Read)
        );
        assert_eq!(
            authority.authorize_operation(protocol::operation::READ_DIRECTORY, 0),
            Ok(ApplicationResourceAccess::Enumerate)
        );
        assert_eq!(
            authority.authorize_operation(protocol::operation::CREATE_FILE, 0),
            Ok(ApplicationResourceAccess::Create)
        );
        assert_eq!(
            authority.authorize_operation(
                protocol::operation::CREATE_FILE,
                protocol::request_flags::TRUNCATE,
            ),
            Err(ApplicationResourceAuthorizationError::RightsDenied)
        );
        assert_eq!(
            authority.authorize_operation(protocol::operation::UNLINK, 0),
            Err(ApplicationResourceAuthorizationError::RightsDenied)
        );
        assert_eq!(
            authority.authorize_operation(protocol::operation::RENAME, 0),
            Err(ApplicationResourceAuthorizationError::RightsDenied)
        );
    }

    #[test]
    fn open_flags_cannot_bypass_the_grant_ceiling() {
        let read = authority(ApplicationResourceKind::File, ApplicationGrantRights::READ);
        assert_eq!(
            read.authorize_operation(protocol::operation::OPEN, protocol::request_flags::READ),
            Ok(ApplicationResourceAccess::Open)
        );
        assert_eq!(
            read.authorize_operation(protocol::operation::OPEN, protocol::request_flags::WRITE),
            Err(ApplicationResourceAuthorizationError::RightsDenied)
        );
        assert_eq!(
            read.authorize_operation(protocol::operation::OPEN, protocol::request_flags::TRUNCATE),
            Err(ApplicationResourceAuthorizationError::InvalidFlags)
        );
        assert_eq!(
            read.authorize_operation(protocol::operation::OPEN, protocol::request_flags::CREATE),
            Err(ApplicationResourceAuthorizationError::ResourceKindDenied)
        );
    }
}
