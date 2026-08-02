#![no_std]

//! Allocation-free NSWP connection runtime for fixed-size, handle-free endpoints.
//!
//! The runtime is deliberately synchronous and nonblocking. Transports preserve complete message
//! boundaries and provide atomic enqueue semantics; callers drive progress through `poll` and
//! explicit clock values.

mod client;
mod server;
mod types;
mod wire;

pub use client::{CancelDisposition, Client, ClientEvent};
pub use server::{CancellationReason, RequestToken, Server, ServerEvent};
pub use types::{
    BodyBuf, BodyValidator, BoundState, CloseReason, ConnectionPhase, DeadlinePolicy,
    HANDLE_FREE_ENDPOINT_LIMITS, MAX_BODY_BYTES, MAX_NEGOTIATED_FEATURES, MAX_OUTSTANDING,
    MAX_PACKET_BYTES, MAX_RECENTLY_CANCELED, MethodDescriptor, MethodKind, PacketBuf,
    PeerContextId, ProtocolDescriptor, RuntimeError, TryRecvError, TrySendError, TryTransport,
    no_features_fit,
};
