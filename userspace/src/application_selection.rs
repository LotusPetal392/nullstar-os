//! Failure-atomic completion for portal-selected application resources.
//!
//! A selection keeps its permission-store mutation prepared while it creates and binds a fresh
//! broker endpoint. The kernel move-send is the only fallible completion step. Success commits the
//! preflighted grant mutation; failure drops both endpoint sides and leaves the store unchanged.

use crate::{
    application_permission::{
        ApplicationGrantAuthorization, ApplicationGrantAuthorizationError,
        ApplicationPermissionStore, ApplicationPermissionStoreError, ApplicationResourceIdentity,
        PreparedApplicationGrant,
    },
    application_portal::{AdmittedPortalRequest, ApplicationPortalResponse, PortalSelectionError},
    application_resource::{
        APPLICATION_RESOURCE_CLIENT_RIGHTS, ApplicationResourceBroker,
        ApplicationResourceClientEndpoint,
    },
    handle::{BorrowedHandle, Endpoint},
    ipc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationSelectionPrepareError {
    Permission(ApplicationPermissionStoreError),
    Authorization(ApplicationGrantAuthorizationError),
    Endpoint(ipc::Error),
    Response(PortalSelectionError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationSelectionCompletionError {
    Transfer(ipc::Error),
}

/// One response, endpoint pair, and deferred grant mutation owned as a single transaction.
pub struct PreparedApplicationSelection<'a> {
    grant: PreparedApplicationGrant<'a>,
    response: ApplicationPortalResponse,
    broker: ApplicationResourceBroker,
    client: ApplicationResourceClientEndpoint,
}

impl<'a> PreparedApplicationSelection<'a> {
    /// Prepares a newly approved picker selection without making its grant visible yet.
    pub fn issue(
        store: &'a mut ApplicationPermissionStore,
        admission: AdmittedPortalRequest,
        resource: ApplicationResourceIdentity,
    ) -> Result<Self, ApplicationSelectionPrepareError> {
        let request = admission.request();
        let grant = store
            .prepare_issue_authorization(
                admission.authorization(),
                resource,
                request.rights(),
                request.scope(),
            )
            .map_err(ApplicationSelectionPrepareError::Permission)?;
        Self::prepare(admission, grant)
    }

    /// Prepares a selection backed by an existing active grant. A one-shot grant remains active if
    /// endpoint creation, response construction, or response transfer fails.
    pub fn authorize(
        store: &'a mut ApplicationPermissionStore,
        admission: AdmittedPortalRequest,
        resource: ApplicationResourceIdentity,
    ) -> Result<Self, ApplicationSelectionPrepareError> {
        let request = admission.request();
        let grant = store
            .prepare_authorization(admission.authorization(), resource, request.rights())
            .map_err(ApplicationSelectionPrepareError::Authorization)?;
        Self::prepare(admission, grant)
    }

    fn prepare(
        admission: AdmittedPortalRequest,
        grant: PreparedApplicationGrant<'a>,
    ) -> Result<Self, ApplicationSelectionPrepareError> {
        let authorization = grant.authorization();
        let (broker, client) = ApplicationResourceBroker::mint(authorization)
            .map_err(ApplicationSelectionPrepareError::Endpoint)?;
        let response = ApplicationPortalResponse::selected_with_resource_endpoint(
            admission,
            authorization,
            &client,
        )
        .map_err(ApplicationSelectionPrepareError::Response)?;
        Ok(Self {
            grant,
            response,
            broker,
            client,
        })
    }

    pub const fn grant_authorization(&self) -> ApplicationGrantAuthorization {
        self.grant.authorization()
    }

    pub const fn response(&self) -> ApplicationPortalResponse {
        self.response
    }

    pub const fn broker(&self) -> &ApplicationResourceBroker {
        &self.broker
    }

    pub const fn client_endpoint(&self) -> &ApplicationResourceClientEndpoint {
        &self.client
    }

    /// Atomically move-sends the selected endpoint and commits the preflighted policy mutation.
    /// A failed send returns only after the still-owned client endpoint has been closed and the
    /// untouched grant transaction has been discarded.
    pub fn complete(
        self,
        portal_endpoint: BorrowedHandle<'_, Endpoint>,
    ) -> Result<ApplicationResourceBroker, ApplicationSelectionCompletionError> {
        let Self {
            grant,
            response,
            broker,
            client,
        } = self;
        if let Err(error) = portal_endpoint.send_move(
            &response.encode(),
            client.into_endpoint(),
            APPLICATION_RESOURCE_CLIENT_RIGHTS,
        ) {
            return Err(ApplicationSelectionCompletionError::Transfer(error.error()));
        }
        grant.commit();
        Ok(broker)
    }
}
