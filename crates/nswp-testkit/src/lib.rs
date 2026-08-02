//! Deterministic host-side NSWP transport simulation and handwritten protocol fixtures.

mod echo;
mod logging;
mod sim;

pub use echo::{
    ECHO_BODY_DESCRIPTOR, ECHO_HI_7_BODY, ECHO_MAX_DEADLINE_NS, ECHO_PING_ORDINAL,
    ECHO_PROTOCOL_ID, ECHO_PROTOCOL_MAJOR, ECHO_PROTOCOL_MINOR, EchoMessageSchema, EchoMessageView,
    EchoService, decode_echo, echo_protocol, encode_echo,
};
pub use logging::{
    COLLECTOR_MAX_RECORDS, CollectedLogRecord, LoggingCollector, ProducerIdentity, SECRET_REDACTION,
};
pub use sim::{ManualClock, SimEndpoint, channel_pair};
