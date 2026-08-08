#![no_std]

//! Bounded semantic types and parser for NullStar service-definition files.

mod parser;
mod types;

pub use parser::{Field, ParseError, ServiceIdTextError, parse};
pub use service_route::ServiceId;
pub use types::{
    MAX_ARGUMENT_BYTES, MAX_ARGUMENTS, MAX_DEFINITION_BYTES, MAX_DESCRIPTION_BYTES,
    MAX_EXECUTABLE_BYTES, MAX_NAME_BYTES, MAX_READY_MESSAGE_BYTES, MAX_RESTART_BACKOFF_YIELDS,
    MAX_RESTART_LIMIT, Readiness, RestartPolicy, SERVICE_DEFINITION_HEADER, ServiceDefinition,
};
