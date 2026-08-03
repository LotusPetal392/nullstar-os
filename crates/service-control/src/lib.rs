#![no_std]

//! Allocation-free semantic types and the exact `NSVC` v1 service-control codec.

mod codec;
mod types;

pub use codec::{
    DecodeError, SERVICE_CONTROL_MAGIC, SERVICE_CONTROL_VERSION, SERVICE_CONTROL_WIRE_BYTES,
};
pub use service_route::{ProviderGeneration, ServiceId, ServiceIdError};
pub use types::{
    CorrelationError, DesiredState, ListOutcome, ListResponse, ListResponseError,
    MutationResponseError, ObservedState, Operation, RequestId, ServiceControlFailure,
    ServiceControlMessage, ServiceControlRequest, ServiceControlResponse, ServiceRecord,
    ServiceRecordError, TargetOutcome, TargetResponse,
};
