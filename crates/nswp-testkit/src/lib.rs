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
    COLLECTOR_MAX_RECORDS, CollectedLogRecord, LOG_RECORD_BODY_DESCRIPTOR, LOGGING_EMIT_ORDINAL,
    LOGGING_MAX_MESSAGE_BYTES, LOGGING_MAX_SUBSYSTEM_BYTES, LOGGING_PROTOCOL_ID,
    LOGGING_PROTOCOL_MAJOR, LOGGING_PROTOCOL_MINOR_BASE, LOGGING_PROTOCOL_MINOR_WALL_TIME,
    LogDelivery, LogDisposition, LogRecord, LogRecordSchema, LogRecordView, LogSeverity,
    LoggingCollector, LoggingProducer, PrivacyClass, ProducerIdentity, SECRET_REDACTION,
    decode_log_record, encode_log_record, logging_protocol, logging_protocol_through,
};
pub use sim::{ManualClock, SimEndpoint, channel_pair};
