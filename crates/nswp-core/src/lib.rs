#![no_std]

//! Target-independent, allocation-free NullStar Wire Protocol framing, negotiation, and
//! canonical handle-free body primitives.
//!
//! The body codec currently covers fixed values, strings and byte sequences, tables, and
//! closed results. Its traversal closures are intended for deterministic, side-effect-free
//! schema validation; application code must run only after complete packet validation.

mod body;
mod error;
mod header;
mod negotiation;
mod packet;
mod protocol_id;
mod validation;

pub use body::{
    BodyDecoder, BodyEncoder, BodyError, BodyLimits, ClosedResultDecoder, ENVELOPE_BYTES,
    FieldDecoder, SLICE_REF_BYTES, TABLE_REF_BYTES, TableDecoder, TableEncoder, ValueDecoder,
    ValueEncoder,
};
pub use error::{DecodeError, EncodeError, NegotiationError, ProtocolIdError};
pub use header::{Header, NSWP_HEADER_BYTES, NSWP_MAGIC, NSWP_WIRE_MAJOR, NSWP_WIRE_MINOR, offset};
pub use negotiation::{
    AvailableFeature, DecodedNegotiationRequest, DecodedNegotiationResponse, FEATURE_RECORD_BYTES,
    FeatureFlags, FeatureList, FeatureRecord, FeatureSetValidator, MinorVersionProfile,
    NEGOTIATE_REQUEST_ROOT_BYTES, NEGOTIATE_RESPONSE_ROOT_BYTES, NegotiationOutcome,
    NegotiationRequest, NegotiationResponse, NegotiationStatus, SelectedFeatures, ServerProfile,
    negotiate,
};
pub use packet::{
    HeaderFlags, PROTOCOL_ERROR_BODY_BYTES, PacketKind, ProtocolErrorCode, ProtocolErrorRecord,
    TransportStatus,
};
pub use protocol_id::{PROTOCOL_ID_BYTES, PROTOCOL_ID_TEXT_BYTES, ProtocolId};
pub use validation::{
    BoundProtocol, ConnectionLimits, ConnectionState, Direction, OutstandingTransaction,
    TransactionState, ValidationContext,
};
