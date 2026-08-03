#![no_std]

//! Allocation-free service-route identities, wire messages, publication storage, and brokering.

mod broker;
mod codec;
mod generation;
mod id;
mod table;

pub use broker::{
    Authorizer, ConnectError, ConnectResult, IssueError, IssuedRoute, RouteBroker, RouteIssuer,
};
pub use codec::{
    DecodeError, RouteFailure, RouteMessage, SERVICE_ROUTE_MAGIC, SERVICE_ROUTE_VERSION,
    SERVICE_ROUTE_WIRE_BYTES,
};
pub use generation::{
    ProviderGenerationExhausted, ProviderGenerationSequence, SERVICE_GENERATION_MAGIC,
    SERVICE_GENERATION_VERSION, SERVICE_GENERATION_WIRE_BYTES, ServiceGenerationDecodeError,
    ServiceGenerationHandoff,
};
pub use id::{ProviderGeneration, RoleId, RouteKey, SERVICE_ID_BYTES, ServiceId, ServiceIdError};
pub use table::{PublishError, PublishedRoute, RouteTable, WithdrawError};
