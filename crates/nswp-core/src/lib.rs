#![no_std]

//! Target-independent, allocation-free NullStar Wire Protocol framing and negotiation.

mod error;
mod header;
mod negotiation;
mod packet;
mod protocol_id;
mod validation;

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
