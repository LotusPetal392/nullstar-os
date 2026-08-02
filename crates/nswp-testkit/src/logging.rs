use nswp_core::{BodyError, BoundProtocol, ProtocolId};
use nswp_logging::{EventId, LogSeverity, PrivacyClass, decode_log_record};
use nswp_runtime::PeerContextId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProducerIdentity {
    pub peer_context: PeerContextId,
    pub principal_id: u64,
    pub service_id: ProtocolId,
    pub service_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectedLogRecord {
    pub producer: ProducerIdentity,
    pub collector_generation: u64,
    pub event_id: EventId,
    pub severity: LogSeverity,
    pub privacy: PrivacyClass,
    pub monotonic_time_ns: u64,
    pub subsystem: String,
    pub message: String,
    pub wall_time_unix_ns: Option<u64>,
    pub trace_id: [u8; 16],
}

pub const COLLECTOR_MAX_RECORDS: usize = 64;
pub const SECRET_REDACTION: &str = "[redacted: secret-never-persist]";

#[derive(Debug)]
pub struct LoggingCollector {
    producer: ProducerIdentity,
    capacity: usize,
    records: Vec<CollectedLogRecord>,
}

impl LoggingCollector {
    pub fn new(producer: ProducerIdentity) -> Self {
        Self::with_capacity(producer, COLLECTOR_MAX_RECORDS)
    }

    pub fn with_capacity(producer: ProducerIdentity, capacity: usize) -> Self {
        let capacity = capacity.min(COLLECTOR_MAX_RECORDS);
        Self {
            producer,
            capacity,
            records: Vec::with_capacity(capacity),
        }
    }

    pub fn dispatch(
        &mut self,
        peer_context: PeerContextId,
        body: &[u8],
        bound: &BoundProtocol<'_>,
        trace_id: [u8; 16],
    ) -> Result<(), BodyError> {
        if peer_context == PeerContextId::UNSPECIFIED
            || self.producer.peer_context == PeerContextId::UNSPECIFIED
            || peer_context != self.producer.peer_context
        {
            return Err(BodyError::MaterializationMismatch);
        }
        if self.records.len() >= self.capacity {
            return Err(BodyError::LimitExceeded);
        }
        let record = decode_log_record(body, bound)?;
        let message = if record.privacy == PrivacyClass::SecretNeverPersist {
            SECRET_REDACTION.into()
        } else {
            record.message.into()
        };
        self.records.push(CollectedLogRecord {
            producer: self.producer,
            collector_generation: bound.service_generation(),
            event_id: record.event_id,
            severity: record.severity,
            privacy: record.privacy,
            monotonic_time_ns: record.monotonic_time_ns,
            subsystem: record.subsystem.into(),
            message,
            wall_time_unix_ns: record.wall_time_unix_ns,
            trace_id,
        });
        Ok(())
    }

    pub fn records(&self) -> &[CollectedLogRecord] {
        &self.records
    }
}
