use nswp_core::{
    Availability, BodyEncoder, BodyError, BodyLimits, BoundProtocol, ClosedEnumDescriptor,
    InlineLayout, IntegerRepr, MinorVersionProfile, PrimitiveKind, ProtocolBodyDescriptor,
    ProtocolId, StructureDescriptor, StructureFieldDescriptor, TableDescriptor,
    TableFieldDescriptor, TypeDescriptor, TypeKind, ValidatedValue, WireSchema, validate_body,
};
use nswp_runtime::{
    BodyBuf, Client, ClientEvent, DeadlinePolicy, HANDLE_FREE_ENDPOINT_LIMITS, MethodDescriptor,
    MethodKind, ProtocolDescriptor, RuntimeError, TryTransport,
};

pub const LOGGING_PROTOCOL_ID: ProtocolId = match ProtocolId::from_bytes([
    0x7d, 0xb7, 0x9c, 0xd9, 0xc6, 0x85, 0x40, 0x0f, 0xb9, 0xf1, 0x55, 0xd8, 0x9b, 0x8e, 0x8a, 0x8a,
]) {
    Ok(id) => id,
    Err(_) => panic!("logging protocol ID must be canonical"),
};
pub const LOGGING_PROTOCOL_MAJOR: u16 = 1;
pub const LOGGING_PROTOCOL_MINOR_BASE: u16 = 0;
pub const LOGGING_PROTOCOL_MINOR_WALL_TIME: u16 = 1;
pub const LOGGING_EMIT_ORDINAL: u32 = 1;
pub const LOGGING_MAX_SUBSYSTEM_BYTES: usize = 16;
pub const LOGGING_MAX_MESSAGE_BYTES: usize = 80;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogSeverity {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Notice = 3,
    Warning = 4,
    Error = 5,
    Critical = 6,
    Alert = 7,
    Emergency = 8,
}

impl TryFrom<u64> for LogSeverity {
    type Error = BodyError;

    fn try_from(value: u64) -> Result<Self, BodyError> {
        match value {
            0 => Ok(Self::Trace),
            1 => Ok(Self::Debug),
            2 => Ok(Self::Info),
            3 => Ok(Self::Notice),
            4 => Ok(Self::Warning),
            5 => Ok(Self::Error),
            6 => Ok(Self::Critical),
            7 => Ok(Self::Alert),
            8 => Ok(Self::Emergency),
            _ => Err(BodyError::UnknownEnumValue),
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivacyClass {
    Public = 0,
    UserPrivate = 1,
    Administrator = 2,
    SecuritySensitive = 3,
    SecretNeverPersist = 4,
}

impl TryFrom<u64> for PrivacyClass {
    type Error = BodyError;

    fn try_from(value: u64) -> Result<Self, BodyError> {
        match value {
            0 => Ok(Self::Public),
            1 => Ok(Self::UserPrivate),
            2 => Ok(Self::Administrator),
            3 => Ok(Self::SecuritySensitive),
            4 => Ok(Self::SecretNeverPersist),
            _ => Err(BodyError::UnknownEnumValue),
        }
    }
}

static SEVERITY_VALUES: [u64; 9] = [0, 1, 2, 3, 4, 5, 6, 7, 8];
static SEVERITY_DESCRIPTOR: ClosedEnumDescriptor = ClosedEnumDescriptor {
    repr: IntegerRepr::U8,
    values: &SEVERITY_VALUES,
};
static SEVERITY_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout {
        bytes: 1,
        alignment: 1,
    },
    kind: TypeKind::ClosedEnum(&SEVERITY_DESCRIPTOR),
};
static PRIVACY_VALUES: [u64; 5] = [0, 1, 2, 3, 4];
static PRIVACY_DESCRIPTOR: ClosedEnumDescriptor = ClosedEnumDescriptor {
    repr: IntegerRepr::U8,
    values: &PRIVACY_VALUES,
};
static PRIVACY_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout {
        bytes: 1,
        alignment: 1,
    },
    kind: TypeKind::ClosedEnum(&PRIVACY_DESCRIPTOR),
};
static U64_TYPE: TypeDescriptor = TypeDescriptor::primitive(PrimitiveKind::U64);
static SUBSYSTEM_TYPE: TypeDescriptor = TypeDescriptor::string(LOGGING_MAX_SUBSYSTEM_BYTES as u32);
static MESSAGE_TYPE: TypeDescriptor = TypeDescriptor::string(LOGGING_MAX_MESSAGE_BYTES as u32);

static METADATA_FIELDS: [StructureFieldDescriptor; 3] = [
    StructureFieldDescriptor {
        offset: 0,
        ty: &SEVERITY_TYPE,
    },
    StructureFieldDescriptor {
        offset: 1,
        ty: &PRIVACY_TYPE,
    },
    StructureFieldDescriptor {
        offset: 8,
        ty: &U64_TYPE,
    },
];
static METADATA_DESCRIPTOR: StructureDescriptor = StructureDescriptor {
    fields: &METADATA_FIELDS,
};
static METADATA_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout {
        bytes: 16,
        alignment: 8,
    },
    kind: TypeKind::Structure(&METADATA_DESCRIPTOR),
};

static EXTENSION_FIELDS: [TableFieldDescriptor; 1] = [TableFieldDescriptor {
    ordinal: 1,
    ty: &U64_TYPE,
    required: false,
    availability: Availability {
        since_minor: LOGGING_PROTOCOL_MINOR_WALL_TIME,
        required_features: &[],
    },
}];
static EXTENSION_DESCRIPTOR: TableDescriptor = TableDescriptor {
    maximum_present_fields: 4,
    fields: &EXTENSION_FIELDS,
    reserved_ordinals: &[],
};
static EXTENSION_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout::TABLE,
    kind: TypeKind::Table(&EXTENSION_DESCRIPTOR),
};

static LOG_RECORD_FIELDS: [StructureFieldDescriptor; 4] = [
    StructureFieldDescriptor {
        offset: 0,
        ty: &METADATA_TYPE,
    },
    StructureFieldDescriptor {
        offset: 16,
        ty: &SUBSYSTEM_TYPE,
    },
    StructureFieldDescriptor {
        offset: 32,
        ty: &MESSAGE_TYPE,
    },
    StructureFieldDescriptor {
        offset: 48,
        ty: &EXTENSION_TYPE,
    },
];
static LOG_RECORD_DESCRIPTOR: StructureDescriptor = StructureDescriptor {
    fields: &LOG_RECORD_FIELDS,
};
static LOG_RECORD_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout {
        bytes: 64,
        alignment: 8,
    },
    kind: TypeKind::Structure(&LOG_RECORD_DESCRIPTOR),
};
pub static LOG_RECORD_BODY_DESCRIPTOR: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: LOGGING_PROTOCOL_ID,
    protocol_major: LOGGING_PROTOCOL_MAJOR,
    root: &LOG_RECORD_TYPE,
};

pub struct LogRecordSchema;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogRecordView<'wire> {
    pub severity: LogSeverity,
    pub privacy: PrivacyClass,
    pub monotonic_time_ns: u64,
    pub subsystem: &'wire str,
    pub message: &'wire str,
    pub wall_time_unix_ns: Option<u64>,
}

impl WireSchema for LogRecordSchema {
    type View<'wire> = LogRecordView<'wire>;

    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &LOG_RECORD_BODY_DESCRIPTOR;

    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        let metadata = required_structure_field(value, 0)?;
        let severity = required_structure_field(metadata, 0)?
            .enum_raw()?
            .try_into()?;
        let privacy = required_structure_field(metadata, 1)?
            .enum_raw()?
            .try_into()?;
        let monotonic_time_ns = required_structure_field(metadata, 2)?.u64()?;
        let subsystem = required_structure_field(value, 1)?.string()?;
        let message = required_structure_field(value, 2)?.string()?;
        let extensions = required_structure_field(value, 3)?;
        let wall_time_unix_ns = extensions
            .table_field(1)?
            .and_then(|field| field.value())
            .map(|value| value.u64())
            .transpose()?;
        Ok(LogRecordView {
            severity,
            privacy,
            monotonic_time_ns,
            subsystem,
            message,
            wall_time_unix_ns,
        })
    }
}

fn required_structure_field(
    value: ValidatedValue<'_>,
    index: usize,
) -> Result<ValidatedValue<'_>, BodyError> {
    value
        .structure_field(index)?
        .ok_or(BodyError::MaterializationMismatch)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogRecord<'a> {
    pub severity: LogSeverity,
    pub privacy: PrivacyClass,
    pub monotonic_time_ns: u64,
    pub subsystem: &'a str,
    pub message: &'a str,
    pub wall_time_unix_ns: Option<u64>,
}

pub fn encode_log_record(record: LogRecord<'_>, minor: u16) -> Result<BodyBuf, BodyError> {
    if record.subsystem.len() > LOGGING_MAX_SUBSYSTEM_BYTES
        || record.message.len() > LOGGING_MAX_MESSAGE_BYTES
    {
        return Err(BodyError::LimitExceeded);
    }
    if record.wall_time_unix_ns.is_some() && minor < LOGGING_PROTOCOL_MINOR_WALL_TIME {
        return Err(BodyError::FieldUnavailable);
    }
    let body_bytes = 64
        + align_eight(record.subsystem.len())
        + align_eight(record.message.len())
        + if record.wall_time_unix_ns.is_some() {
            32
        } else {
            0
        };
    let mut output = [0; nswp_runtime::MAX_BODY_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut output, body_bytes, 64, BodyLimits::ENDPOINT_PROTOTYPE)?;
    let root = encoder.root();
    root.write_u8(0, record.severity as u8)?;
    root.write_u8(1, record.privacy as u8)?;
    root.write_u64(8, record.monotonic_time_ns)?;
    root.string(16, LOGGING_MAX_SUBSYSTEM_BYTES as u32, record.subsystem)?;
    root.string(32, LOGGING_MAX_MESSAGE_BYTES as u32, record.message)?;
    match record.wall_time_unix_ns {
        Some(wall_time) => root.table(48, 1, 4, |table| {
            table.field(1, 8, |value| value.write_u64(0, wall_time))
        })?,
        None => root.table(48, 0, 4, |_| Ok(()))?,
    }
    encoder.finish()?;
    BodyBuf::from_slice(&output[..body_bytes]).map_err(|_| BodyError::OutputTooSmall)
}

pub fn decode_log_record<'wire>(
    body: &'wire [u8],
    bound: &BoundProtocol<'_>,
) -> Result<LogRecordView<'wire>, BodyError> {
    validate_body::<LogRecordSchema>(body, bound, BodyLimits::ENDPOINT_PROTOTYPE)?.materialize()
}

fn validate_log_record(body: &[u8], bound: &BoundProtocol<'_>) -> Result<(), BodyError> {
    decode_log_record(body, bound).map(|_| ())
}

static LOGGING_VERSIONS: [MinorVersionProfile; 2] = [
    MinorVersionProfile {
        minor: LOGGING_PROTOCOL_MINOR_BASE,
        minimum_body_bytes: 160,
        minimum_handles: 0,
    },
    MinorVersionProfile {
        minor: LOGGING_PROTOCOL_MINOR_WALL_TIME,
        minimum_body_bytes: 192,
        minimum_handles: 0,
    },
];
static LOGGING_METHODS: [MethodDescriptor; 1] = [MethodDescriptor {
    ordinal: LOGGING_EMIT_ORDINAL,
    kind: MethodKind::OneWay,
    deadline: DeadlinePolicy::Forbidden,
    validate_request: validate_log_record,
    validate_response: validate_log_record,
}];

pub fn logging_protocol() -> ProtocolDescriptor<'static> {
    logging_protocol_through(LOGGING_PROTOCOL_MINOR_WALL_TIME)
}

pub fn logging_protocol_through(max_minor: u16) -> ProtocolDescriptor<'static> {
    let max_minor = max_minor.min(LOGGING_PROTOCOL_MINOR_WALL_TIME);
    let versions = if max_minor == LOGGING_PROTOCOL_MINOR_BASE {
        &LOGGING_VERSIONS[..1]
    } else {
        &LOGGING_VERSIONS
    };
    ProtocolDescriptor {
        protocol_id: LOGGING_PROTOCOL_ID,
        major: LOGGING_PROTOCOL_MAJOR,
        min_minor: LOGGING_PROTOCOL_MINOR_BASE,
        max_minor,
        limits: HANDLE_FREE_ENDPOINT_LIMITS,
        requested_features: &[],
        available_features: &[],
        versions,
        feature_set_fits: nswp_runtime::no_features_fit,
        methods: &LOGGING_METHODS,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogDelivery {
    Reliable,
    BestEffort,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogDisposition {
    Queued,
    Dropped,
}

pub struct LoggingProducer<'a, T: TryTransport> {
    client: Client<'a, T>,
    dropped_records: u64,
}

impl<T: TryTransport> LoggingProducer<'static, T> {
    pub fn new(transport: T) -> Self {
        Self::with_protocol(transport, logging_protocol())
    }

    pub fn through_minor(transport: T, max_minor: u16) -> Self {
        Self::with_protocol(transport, logging_protocol_through(max_minor))
    }

    fn with_protocol(transport: T, protocol: ProtocolDescriptor<'static>) -> Self {
        Self {
            client: Client::new(transport, protocol),
            dropped_records: 0,
        }
    }
}

impl<'a, T: TryTransport> LoggingProducer<'a, T> {
    pub fn try_negotiate(&mut self) -> Result<(), RuntimeError> {
        self.client.try_negotiate()
    }

    pub fn poll(&mut self) -> Result<Option<ClientEvent>, RuntimeError> {
        self.client.poll()
    }

    pub fn try_log(
        &mut self,
        record: LogRecord<'_>,
        delivery: LogDelivery,
        trace_id: [u8; 16],
    ) -> Result<LogDisposition, RuntimeError> {
        let minor = self
            .client
            .bound()
            .ok_or(RuntimeError::InvalidState)?
            .minor();
        let body = encode_log_record(record, minor)?;
        match self.client.try_send_one_way(
            LOGGING_EMIT_ORDINAL,
            body.as_slice(),
            0,
            u64::MAX,
            trace_id,
        ) {
            Ok(()) => Ok(LogDisposition::Queued),
            Err(RuntimeError::WouldBlock) if delivery == LogDelivery::BestEffort => {
                self.dropped_records = self.dropped_records.saturating_add(1);
                Ok(LogDisposition::Dropped)
            }
            Err(error) => Err(error),
        }
    }

    pub const fn dropped_records(&self) -> u64 {
        self.dropped_records
    }

    pub const fn client(&self) -> &Client<'a, T> {
        &self.client
    }

    pub fn client_mut(&mut self) -> &mut Client<'a, T> {
        &mut self.client
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProducerIdentity {
    pub peer_context: nswp_runtime::PeerContextId,
    pub principal_id: u64,
    pub service_id: ProtocolId,
    pub service_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectedLogRecord {
    pub producer: ProducerIdentity,
    pub collector_generation: u64,
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
        peer_context: nswp_runtime::PeerContextId,
        body: &[u8],
        bound: &BoundProtocol<'_>,
        trace_id: [u8; 16],
    ) -> Result<(), BodyError> {
        if peer_context == nswp_runtime::PeerContextId::UNSPECIFIED
            || self.producer.peer_context == nswp_runtime::PeerContextId::UNSPECIFIED
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

const fn align_eight(value: usize) -> usize {
    (value + 7) & !7
}
