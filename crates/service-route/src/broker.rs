use crate::{ProviderGeneration, PublishError, RouteKey, RouteTable, WithdrawError};

/// Policy boundary evaluated before a broker reveals route availability.
pub trait Authorizer<Caller> {
    type Error;

    fn authorize(&mut self, caller: &Caller, key: RouteKey) -> Result<(), Self::Error>;
}

/// Endpoint issuance failure. Capacity is distinguished from provider-specific failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueError<E> {
    Capacity,
    Provider(E),
}

/// Creates a fresh client connection from a published provider authority.
pub trait RouteIssuer<A, Caller> {
    type Connection;
    type Error;

    fn issue(
        &mut self,
        authority: &mut A,
        caller: &Caller,
        key: RouteKey,
        generation: ProviderGeneration,
    ) -> Result<Self::Connection, IssueError<Self::Error>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IssuedRoute<C> {
    pub generation: ProviderGeneration,
    pub connection: C,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectError<A, I> {
    Unauthorized(A),
    Unavailable,
    IssuerCapacity,
    Issuer(I),
}

pub type ConnectResult<C, A, I> = Result<IssuedRoute<C>, ConnectError<A, I>>;

/// Authorization and endpoint-issuance facade over a fixed [`RouteTable`].
pub struct RouteBroker<A, const N: usize> {
    routes: RouteTable<A, N>,
}

impl<A, const N: usize> RouteBroker<A, N> {
    pub const fn new() -> Self {
        Self {
            routes: RouteTable::new(),
        }
    }

    pub const fn from_table(routes: RouteTable<A, N>) -> Self {
        Self { routes }
    }

    pub const fn table(&self) -> &RouteTable<A, N> {
        &self.routes
    }

    pub fn table_mut(&mut self) -> &mut RouteTable<A, N> {
        &mut self.routes
    }

    pub fn into_table(self) -> RouteTable<A, N> {
        self.routes
    }

    pub fn publish(
        &mut self,
        key: RouteKey,
        generation: ProviderGeneration,
        authority: A,
    ) -> Result<Option<A>, PublishError<A>> {
        self.routes.publish(key, generation, authority)
    }

    pub fn withdraw(
        &mut self,
        key: RouteKey,
        generation: ProviderGeneration,
    ) -> Result<A, WithdrawError> {
        self.routes.withdraw(key, generation)
    }

    /// Authorizes first, then resolves availability, then asks the provider to issue a connection.
    pub fn connect<Caller, Policy, Issuer>(
        &mut self,
        caller: &Caller,
        key: RouteKey,
        policy: &mut Policy,
        issuer: &mut Issuer,
    ) -> ConnectResult<Issuer::Connection, Policy::Error, Issuer::Error>
    where
        Policy: Authorizer<Caller>,
        Issuer: RouteIssuer<A, Caller>,
    {
        policy
            .authorize(caller, key)
            .map_err(ConnectError::Unauthorized)?;
        let published = self.routes.get_mut(key).ok_or(ConnectError::Unavailable)?;
        let connection = issuer
            .issue(published.authority, caller, key, published.generation)
            .map_err(|error| match error {
                IssueError::Capacity => ConnectError::IssuerCapacity,
                IssueError::Provider(error) => ConnectError::Issuer(error),
            })?;
        Ok(IssuedRoute {
            generation: published.generation,
            connection,
        })
    }
}

impl<A, const N: usize> Default for RouteBroker<A, N> {
    fn default() -> Self {
        Self::new()
    }
}
