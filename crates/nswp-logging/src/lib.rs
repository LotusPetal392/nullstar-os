#![no_std]

//! Allocation-free production contract and producer for the NullStar logging protocol.

use core::{fmt, str};

use nswp_core::{
    Availability, BodyEncoder, BodyError, BodyLimits, BoundProtocol, ClosedEnumDescriptor,
    InlineLayout, IntegerRepr, MinorVersionProfile, PrimitiveKind, ProtocolBodyDescriptor,
    ProtocolId, StructureDescriptor, StructureFieldDescriptor, TableDescriptor,
    TableFieldDescriptor, TransportStatus, TypeDescriptor, TypeKind, ValidatedValue, WireSchema,
    validate_body,
};
use nswp_runtime::{
    BodyBuf, Client, ClientEvent, DeadlinePolicy, HANDLE_FREE_ENDPOINT_LIMITS, MethodDescriptor,
    MethodKind, ProtocolDescriptor, RequestToken, RuntimeError, Server, TryTransport,
};

pub const LOGGING_PROTOCOL_ID: ProtocolId = match ProtocolId::from_bytes([
    0x7d, 0xb7, 0x9c, 0xd9, 0xc6, 0x85, 0x40, 0x0f, 0xb9, 0xf1, 0x55, 0xd8, 0x9b, 0x8e, 0x8a, 0x8a,
]) {
    Ok(id) => id,
    Err(_) => panic!("logging protocol ID must be canonical"),
};
pub const LOGGING_PROTOCOL_MAJOR: u16 = 2;
pub const LOGGING_PROTOCOL_MINOR_BASE: u16 = 0;
pub const LOGGING_PROTOCOL_MINOR_WALL_TIME: u16 = 1;
pub const LOGGING_PROTOCOL_MINOR_COLLECTOR_READS: u16 = 2;
pub const LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY: u16 = 3;
pub const LOGGING_EMIT_ORDINAL: u32 = 1;
pub const LOGGING_GET_COLLECTOR_STATS_ORDINAL: u32 = 2;
pub const LOGGING_READ_HISTORY_ORDINAL: u32 = 3;
pub const LOGGING_MAX_SUBSYSTEM_BYTES: usize = 16;
pub const LOGGING_MAX_MESSAGE_BYTES: usize = 64;
pub const LOGGING_MAX_HISTORY_TEXT_BYTES: usize =
    LOGGING_MAX_SUBSYSTEM_BYTES + LOGGING_MAX_MESSAGE_BYTES;
pub const EVENT_ID_BYTES: usize = 16;
pub const COLLECTOR_SECRET_REDACTION: &str = "[redacted: secret-never-persist]";
const _: () = assert!(COLLECTOR_SECRET_REDACTION.len() <= LOGGING_MAX_MESSAGE_BYTES);

/// Stable UUIDv4 identifier for an event type.
///
/// An event definition commits to one `EventId`; producers reuse it for every occurrence of that
/// event. It is distinct from transport trace IDs and future collector-assigned record IDs.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventId([u8; EVENT_ID_BYTES]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventIdError {
    Nil,
    InvalidVersion,
    InvalidVariant,
}

impl EventId {
    /// Constructs an event-type identifier from UUID bytes in RFC/network byte order.
    pub const fn from_bytes(bytes: [u8; EVENT_ID_BYTES]) -> Result<Self, EventIdError> {
        let mut all_zero = true;
        let mut index = 0;
        while index < EVENT_ID_BYTES {
            if bytes[index] != 0 {
                all_zero = false;
            }
            index += 1;
        }
        if all_zero {
            return Err(EventIdError::Nil);
        }
        if bytes[6] >> 4 != 4 {
            return Err(EventIdError::InvalidVersion);
        }
        if bytes[8] & 0xc0 != 0x80 {
            return Err(EventIdError::InvalidVariant);
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; EVENT_ID_BYTES] {
        &self.0
    }

    pub const fn into_bytes(self) -> [u8; EVENT_ID_BYTES] {
        self.0
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().copied().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                formatter.write_str("-")?;
            }
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "EventId({self})")
    }
}

/// UUIDv4 identifier for the kernel boot that produced a retained history record.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BootId([u8; EVENT_ID_BYTES]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootIdError {
    Nil,
    InvalidVersion,
    InvalidVariant,
}

impl BootId {
    /// Constructs a boot identifier from UUID bytes in RFC/network byte order.
    pub const fn from_bytes(bytes: [u8; EVENT_ID_BYTES]) -> Result<Self, BootIdError> {
        let mut all_zero = true;
        let mut index = 0;
        while index < EVENT_ID_BYTES {
            if bytes[index] != 0 {
                all_zero = false;
            }
            index += 1;
        }
        if all_zero {
            return Err(BootIdError::Nil);
        }
        if bytes[6] >> 4 != 4 {
            return Err(BootIdError::InvalidVersion);
        }
        if bytes[8] & 0xc0 != 0x80 {
            return Err(BootIdError::InvalidVariant);
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; EVENT_ID_BYTES] {
        &self.0
    }

    pub const fn into_bytes(self) -> [u8; EVENT_ID_BYTES] {
        self.0
    }
}

impl fmt::Display for BootId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().copied().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                formatter.write_str("-")?;
            }
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for BootId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "BootId({self})")
    }
}

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
static BOOL_TYPE: TypeDescriptor = TypeDescriptor::primitive(PrimitiveKind::Bool);
static U8_TYPE: TypeDescriptor = TypeDescriptor::primitive(PrimitiveKind::U8);
static U64_TYPE: TypeDescriptor = TypeDescriptor::primitive(PrimitiveKind::U64);
static EVENT_ID_TYPE: TypeDescriptor = TypeDescriptor::primitive(PrimitiveKind::Id128);
static SUBSYSTEM_TYPE: TypeDescriptor = TypeDescriptor::string(LOGGING_MAX_SUBSYSTEM_BYTES as u32);
static MESSAGE_TYPE: TypeDescriptor = TypeDescriptor::string(LOGGING_MAX_MESSAGE_BYTES as u32);

static METADATA_FIELDS: [StructureFieldDescriptor; 4] = [
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
    StructureFieldDescriptor {
        offset: 16,
        ty: &EVENT_ID_TYPE,
    },
];
static METADATA_DESCRIPTOR: StructureDescriptor = StructureDescriptor {
    fields: &METADATA_FIELDS,
};
static METADATA_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout {
        bytes: 32,
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
        offset: 32,
        ty: &SUBSYSTEM_TYPE,
    },
    StructureFieldDescriptor {
        offset: 48,
        ty: &MESSAGE_TYPE,
    },
    StructureFieldDescriptor {
        offset: 64,
        ty: &EXTENSION_TYPE,
    },
];
static LOG_RECORD_DESCRIPTOR: StructureDescriptor = StructureDescriptor {
    fields: &LOG_RECORD_FIELDS,
};
static LOG_RECORD_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout {
        bytes: 80,
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
    pub event_id: EventId,
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
        let event_id = EventId::from_bytes(required_structure_field(metadata, 3)?.id128()?)
            .map_err(|_| BodyError::MaterializationMismatch)?;
        let subsystem = required_structure_field(value, 1)?.string()?;
        let message = required_structure_field(value, 2)?.string()?;
        let extensions = required_structure_field(value, 3)?;
        let wall_time_unix_ns = extensions
            .table_field(1)?
            .and_then(|field| field.value())
            .map(|value| value.u64())
            .transpose()?;
        Ok(LogRecordView {
            event_id,
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
    pub event_id: EventId,
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
    let body_bytes = 80
        + align_eight(record.subsystem.len())
        + align_eight(record.message.len())
        + if record.wall_time_unix_ns.is_some() {
            32
        } else {
            0
        };
    let mut output = [0; nswp_runtime::MAX_BODY_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut output, body_bytes, 80, BodyLimits::ENDPOINT_PROTOTYPE)?;
    let root = encoder.root();
    root.write_u8(0, record.severity as u8)?;
    root.write_u8(1, record.privacy as u8)?;
    root.write_u64(8, record.monotonic_time_ns)?;
    root.write_id128(16, record.event_id.into_bytes())?;
    root.string(32, LOGGING_MAX_SUBSYSTEM_BYTES as u32, record.subsystem)?;
    root.string(48, LOGGING_MAX_MESSAGE_BYTES as u32, record.message)?;
    match record.wall_time_unix_ns {
        Some(wall_time) => root.table(64, 1, 4, |table| {
            table.field(1, 8, |value| value.write_u64(0, wall_time))
        })?,
        None => root.table(64, 0, 4, |_| Ok(()))?,
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

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordIdError {
    Zero,
}

impl RecordId {
    pub const fn new(value: u64) -> Result<Self, RecordIdError> {
        if value == 0 {
            Err(RecordIdError::Zero)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Kernel-global sequence number assigned by the kernel logging source.
///
/// This identity is independent of collector-assigned [`RecordId`] values and event-type
/// [`EventId`] values.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KernelSequence(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelSequenceError {
    Zero,
}

impl KernelSequence {
    pub const fn new(value: u64) -> Result<Self, KernelSequenceError> {
        if value == 0 {
            Err(KernelSequenceError::Zero)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySource {
    Process {
        process_id: u64,
        wall_time_unix_ns: Option<u64>,
        trace_id: [u8; 16],
    },
    Kernel {
        sequence: KernelSequence,
        boot_id: Option<BootId>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectorStats {
    pub received_records: u64,
    pub retained_records: u64,
    pub capacity_records: u64,
    pub evicted_records: u64,
    pub dropped_records: u64,
    pub redacted_records: u64,
    pub oldest_record_id: Option<RecordId>,
    pub newest_record_id: Option<RecordId>,
}

static UNIT_TYPE: TypeDescriptor = TypeDescriptor::UNIT;
static COLLECTOR_STATS_FIELDS: [StructureFieldDescriptor; 8] = [
    StructureFieldDescriptor {
        offset: 0,
        ty: &U64_TYPE,
    },
    StructureFieldDescriptor {
        offset: 8,
        ty: &U64_TYPE,
    },
    StructureFieldDescriptor {
        offset: 16,
        ty: &U64_TYPE,
    },
    StructureFieldDescriptor {
        offset: 24,
        ty: &U64_TYPE,
    },
    StructureFieldDescriptor {
        offset: 32,
        ty: &U64_TYPE,
    },
    StructureFieldDescriptor {
        offset: 40,
        ty: &U64_TYPE,
    },
    StructureFieldDescriptor {
        offset: 48,
        ty: &U64_TYPE,
    },
    StructureFieldDescriptor {
        offset: 56,
        ty: &U64_TYPE,
    },
];
static COLLECTOR_STATS_DESCRIPTOR: StructureDescriptor = StructureDescriptor {
    fields: &COLLECTOR_STATS_FIELDS,
};
static COLLECTOR_STATS_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout {
        bytes: 64,
        alignment: 8,
    },
    kind: TypeKind::Structure(&COLLECTOR_STATS_DESCRIPTOR),
};
pub static COLLECTOR_STATS_REQUEST_BODY_DESCRIPTOR: ProtocolBodyDescriptor =
    ProtocolBodyDescriptor {
        protocol_id: LOGGING_PROTOCOL_ID,
        protocol_major: LOGGING_PROTOCOL_MAJOR,
        root: &UNIT_TYPE,
    };
pub static COLLECTOR_STATS_RESPONSE_BODY_DESCRIPTOR: ProtocolBodyDescriptor =
    ProtocolBodyDescriptor {
        protocol_id: LOGGING_PROTOCOL_ID,
        protocol_major: LOGGING_PROTOCOL_MAJOR,
        root: &COLLECTOR_STATS_TYPE,
    };

struct CollectorStatsRequestSchema;
struct CollectorStatsResponseSchema;

impl WireSchema for CollectorStatsRequestSchema {
    type View<'wire> = ();

    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &COLLECTOR_STATS_REQUEST_BODY_DESCRIPTOR;

    fn materialize<'wire>(_value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        Ok(())
    }
}

impl WireSchema for CollectorStatsResponseSchema {
    type View<'wire> = CollectorStats;

    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &COLLECTOR_STATS_RESPONSE_BODY_DESCRIPTOR;

    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        let stats = CollectorStats {
            received_records: required_structure_field(value, 0)?.u64()?,
            retained_records: required_structure_field(value, 1)?.u64()?,
            capacity_records: required_structure_field(value, 2)?.u64()?,
            evicted_records: required_structure_field(value, 3)?.u64()?,
            dropped_records: required_structure_field(value, 4)?.u64()?,
            redacted_records: required_structure_field(value, 5)?.u64()?,
            oldest_record_id: decode_optional_record_id(
                required_structure_field(value, 6)?.u64()?,
            )?,
            newest_record_id: decode_optional_record_id(
                required_structure_field(value, 7)?.u64()?,
            )?,
        };
        validate_collector_stats(stats)?;
        Ok(stats)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryReadRequest {
    pub after_record_id: Option<RecordId>,
}

static HISTORY_READ_REQUEST_FIELDS: [StructureFieldDescriptor; 1] = [StructureFieldDescriptor {
    offset: 0,
    ty: &U64_TYPE,
}];
static HISTORY_READ_REQUEST_DESCRIPTOR: StructureDescriptor = StructureDescriptor {
    fields: &HISTORY_READ_REQUEST_FIELDS,
};
static HISTORY_READ_REQUEST_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout {
        bytes: 8,
        alignment: 8,
    },
    kind: TypeKind::Structure(&HISTORY_READ_REQUEST_DESCRIPTOR),
};
pub static HISTORY_READ_REQUEST_BODY_DESCRIPTOR: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: LOGGING_PROTOCOL_ID,
    protocol_major: LOGGING_PROTOCOL_MAJOR,
    root: &HISTORY_READ_REQUEST_TYPE,
};

static HISTORY_TEXT_TYPE: TypeDescriptor =
    TypeDescriptor::bytes(LOGGING_MAX_HISTORY_TEXT_BYTES as u32);
static HISTORY_RECORD_FIELDS: [StructureFieldDescriptor; 11] = [
    StructureFieldDescriptor {
        offset: 0,
        ty: &U64_TYPE,
    },
    StructureFieldDescriptor {
        offset: 8,
        ty: &U64_TYPE,
    },
    StructureFieldDescriptor {
        offset: 16,
        ty: &U64_TYPE,
    },
    StructureFieldDescriptor {
        offset: 24,
        ty: &U64_TYPE,
    },
    StructureFieldDescriptor {
        offset: 32,
        ty: &EVENT_ID_TYPE,
    },
    StructureFieldDescriptor {
        offset: 48,
        ty: &EVENT_ID_TYPE,
    },
    StructureFieldDescriptor {
        offset: 64,
        ty: &HISTORY_TEXT_TYPE,
    },
    StructureFieldDescriptor {
        offset: 80,
        ty: &SEVERITY_TYPE,
    },
    StructureFieldDescriptor {
        offset: 81,
        ty: &PRIVACY_TYPE,
    },
    StructureFieldDescriptor {
        offset: 82,
        ty: &BOOL_TYPE,
    },
    StructureFieldDescriptor {
        offset: 83,
        ty: &U8_TYPE,
    },
];
static HISTORY_RECORD_MESSAGE_LENGTH_FIELD: StructureFieldDescriptor = StructureFieldDescriptor {
    offset: 84,
    ty: &U8_TYPE,
};
static HISTORY_RECORD_DESCRIPTOR_FIELDS: [StructureFieldDescriptor; 12] = [
    HISTORY_RECORD_FIELDS[0],
    HISTORY_RECORD_FIELDS[1],
    HISTORY_RECORD_FIELDS[2],
    HISTORY_RECORD_FIELDS[3],
    HISTORY_RECORD_FIELDS[4],
    HISTORY_RECORD_FIELDS[5],
    HISTORY_RECORD_FIELDS[6],
    HISTORY_RECORD_FIELDS[7],
    HISTORY_RECORD_FIELDS[8],
    HISTORY_RECORD_FIELDS[9],
    HISTORY_RECORD_FIELDS[10],
    HISTORY_RECORD_MESSAGE_LENGTH_FIELD,
];
static HISTORY_RECORD_DESCRIPTOR: StructureDescriptor = StructureDescriptor {
    fields: &HISTORY_RECORD_DESCRIPTOR_FIELDS,
};
static HISTORY_RECORD_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout {
        bytes: 88,
        alignment: 8,
    },
    kind: TypeKind::Structure(&HISTORY_RECORD_DESCRIPTOR),
};
static HISTORY_RESPONSE_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout::ENVELOPE,
    kind: TypeKind::Optional {
        value: &HISTORY_RECORD_TYPE,
    },
};
pub static HISTORY_READ_RESPONSE_BODY_DESCRIPTOR: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: LOGGING_PROTOCOL_ID,
    protocol_major: LOGGING_PROTOCOL_MAJOR,
    root: &HISTORY_RESPONSE_TYPE,
};

struct HistoryReadRequestSchema;
struct HistoryReadResponseSchema;

impl WireSchema for HistoryReadRequestSchema {
    type View<'wire> = HistoryReadRequest;

    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &HISTORY_READ_REQUEST_BODY_DESCRIPTOR;

    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        Ok(HistoryReadRequest {
            after_record_id: decode_optional_record_id(required_structure_field(value, 0)?.u64()?)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryRecordView<'wire> {
    pub record_id: RecordId,
    pub source: HistorySource,
    pub event_id: EventId,
    pub severity: LogSeverity,
    pub privacy: PrivacyClass,
    pub monotonic_time_ns: u64,
    pub subsystem: &'wire str,
    pub message: &'wire str,
}

impl WireSchema for HistoryReadResponseSchema {
    type View<'wire> = Option<HistoryRecordView<'wire>>;

    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &HISTORY_READ_RESPONSE_BODY_DESCRIPTOR;

    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        let Some(record) = value.optional()? else {
            return Ok(None);
        };
        let record_id = RecordId::new(required_structure_field(record, 0)?.u64()?)
            .map_err(|_| BodyError::MaterializationMismatch)?;
        let monotonic_time_ns = required_structure_field(record, 1)?.u64()?;
        let source_value = required_structure_field(record, 2)?.u64()?;
        let source_process_id = required_structure_field(record, 3)?.u64()?;
        let event_id = EventId::from_bytes(required_structure_field(record, 4)?.id128()?)
            .map_err(|_| BodyError::MaterializationMismatch)?;
        let source_identity = required_structure_field(record, 5)?.id128()?;
        let text = required_structure_field(record, 6)?.bytes()?;
        let severity = required_structure_field(record, 7)?
            .enum_raw()?
            .try_into()?;
        let privacy = required_structure_field(record, 8)?
            .enum_raw()?
            .try_into()?;
        let has_wall_time = required_structure_field(record, 9)?.bool()?;
        let subsystem_bytes = usize::from(primitive_u8(required_structure_field(record, 10)?)?);
        let message_bytes = usize::from(primitive_u8(required_structure_field(record, 11)?)?);
        if subsystem_bytes > LOGGING_MAX_SUBSYSTEM_BYTES
            || message_bytes > LOGGING_MAX_MESSAGE_BYTES
            || subsystem_bytes
                .checked_add(message_bytes)
                .ok_or(BodyError::ArithmeticOverflow)?
                != text.len()
        {
            return Err(BodyError::MaterializationMismatch);
        }
        let source = if source_process_id == 0 {
            if has_wall_time {
                return Err(BodyError::MaterializationMismatch);
            }
            let sequence = KernelSequence::new(source_value)
                .map_err(|_| BodyError::MaterializationMismatch)?;
            let boot_id = if source_identity == [0; EVENT_ID_BYTES] {
                None
            } else {
                Some(
                    BootId::from_bytes(source_identity)
                        .map_err(|_| BodyError::MaterializationMismatch)?,
                )
            };
            HistorySource::Kernel { sequence, boot_id }
        } else {
            if !has_wall_time && source_value != 0 {
                return Err(BodyError::MaterializationMismatch);
            }
            HistorySource::Process {
                process_id: source_process_id,
                wall_time_unix_ns: has_wall_time.then_some(source_value),
                trace_id: source_identity,
            }
        };
        let subsystem =
            str::from_utf8(&text[..subsystem_bytes]).map_err(|_| BodyError::InvalidUtf8)?;
        let message =
            str::from_utf8(&text[subsystem_bytes..]).map_err(|_| BodyError::InvalidUtf8)?;
        Ok(Some(HistoryRecordView {
            record_id,
            source,
            event_id,
            severity,
            privacy,
            monotonic_time_ns,
            subsystem,
            message,
        }))
    }
}

fn primitive_u8(value: ValidatedValue<'_>) -> Result<u8, BodyError> {
    if !matches!(
        value.descriptor().kind,
        TypeKind::Primitive(PrimitiveKind::U8)
    ) {
        return Err(BodyError::MaterializationMismatch);
    }
    value
        .body()
        .get(value.start())
        .copied()
        .ok_or(BodyError::Truncated)
}

fn decode_optional_record_id(raw: u64) -> Result<Option<RecordId>, BodyError> {
    if raw == 0 {
        Ok(None)
    } else {
        RecordId::new(raw)
            .map(Some)
            .map_err(|_| BodyError::MaterializationMismatch)
    }
}

fn encode_optional_record_id(record_id: Option<RecordId>) -> u64 {
    record_id.map_or(0, RecordId::get)
}

fn require_collector_reads_minor(minor: u16) -> Result<(), BodyError> {
    if minor < LOGGING_PROTOCOL_MINOR_COLLECTOR_READS {
        Err(BodyError::FieldUnavailable)
    } else {
        Ok(())
    }
}

fn require_collector_reads(bound: &BoundProtocol<'_>) -> Result<(), BodyError> {
    require_collector_reads_minor(bound.minor())
}

fn validate_collector_stats(stats: CollectorStats) -> Result<(), BodyError> {
    let oldest = encode_optional_record_id(stats.oldest_record_id);
    let newest = encode_optional_record_id(stats.newest_record_id);
    let accounted = stats
        .retained_records
        .saturating_add(stats.evicted_records)
        .saturating_add(stats.dropped_records);
    if stats.retained_records > stats.capacity_records
        || accounted != stats.received_records
        || stats.redacted_records > stats.retained_records.saturating_add(stats.evicted_records)
        || stats.capacity_records == 0
            && (stats.retained_records != 0 || stats.evicted_records != 0)
        || stats.evicted_records != 0 && stats.retained_records != stats.capacity_records
    {
        return Err(BodyError::MaterializationMismatch);
    }
    if stats.retained_records == 0 {
        if oldest != 0 || newest != 0 {
            return Err(BodyError::MaterializationMismatch);
        }
    } else if oldest == 0
        || newest == 0
        || newest < oldest
        || newest
            .checked_sub(oldest)
            .and_then(|span| span.checked_add(1))
            != Some(stats.retained_records)
    {
        return Err(BodyError::MaterializationMismatch);
    }
    Ok(())
}

pub fn encode_collector_stats_request(minor: u16) -> Result<BodyBuf, BodyError> {
    require_collector_reads_minor(minor)?;
    Ok(BodyBuf::new())
}

pub fn decode_collector_stats_request(
    body: &[u8],
    bound: &BoundProtocol<'_>,
) -> Result<(), BodyError> {
    require_collector_reads(bound)?;
    validate_body::<CollectorStatsRequestSchema>(body, bound, BodyLimits::ENDPOINT_PROTOTYPE)?
        .materialize()
}

pub fn encode_collector_stats_response(
    stats: CollectorStats,
    minor: u16,
) -> Result<BodyBuf, BodyError> {
    require_collector_reads_minor(minor)?;
    validate_collector_stats(stats)?;
    let mut output = [0; nswp_runtime::MAX_BODY_BYTES];
    let mut encoder = BodyEncoder::new(&mut output, 64, 64, BodyLimits::ENDPOINT_PROTOTYPE)?;
    let root = encoder.root();
    root.write_u64(0, stats.received_records)?;
    root.write_u64(8, stats.retained_records)?;
    root.write_u64(16, stats.capacity_records)?;
    root.write_u64(24, stats.evicted_records)?;
    root.write_u64(32, stats.dropped_records)?;
    root.write_u64(40, stats.redacted_records)?;
    root.write_u64(48, encode_optional_record_id(stats.oldest_record_id))?;
    root.write_u64(56, encode_optional_record_id(stats.newest_record_id))?;
    encoder.finish()?;
    body_buf(&output[..64])
}

pub fn decode_collector_stats_response(
    body: &[u8],
    bound: &BoundProtocol<'_>,
) -> Result<CollectorStats, BodyError> {
    require_collector_reads(bound)?;
    validate_body::<CollectorStatsResponseSchema>(body, bound, BodyLimits::ENDPOINT_PROTOTYPE)?
        .materialize()
}

pub fn encode_history_read_request(
    request: HistoryReadRequest,
    minor: u16,
) -> Result<BodyBuf, BodyError> {
    require_collector_reads_minor(minor)?;
    let mut output = [0; nswp_runtime::MAX_BODY_BYTES];
    let mut encoder = BodyEncoder::new(&mut output, 8, 8, BodyLimits::ENDPOINT_PROTOTYPE)?;
    encoder
        .root()
        .write_u64(0, encode_optional_record_id(request.after_record_id))?;
    encoder.finish()?;
    body_buf(&output[..8])
}

pub fn decode_history_read_request(
    body: &[u8],
    bound: &BoundProtocol<'_>,
) -> Result<HistoryReadRequest, BodyError> {
    require_collector_reads(bound)?;
    validate_body::<HistoryReadRequestSchema>(body, bound, BodyLimits::ENDPOINT_PROTOTYPE)?
        .materialize()
}

pub fn encode_history_read_response(
    record: Option<HistoryRecordView<'_>>,
    minor: u16,
) -> Result<BodyBuf, BodyError> {
    require_collector_reads_minor(minor)?;
    let mut output = [0; nswp_runtime::MAX_BODY_BYTES];
    let body_bytes = match record {
        None => 24,
        Some(record) => {
            match record.source {
                HistorySource::Process { process_id: 0, .. } => {
                    return Err(BodyError::MaterializationMismatch);
                }
                HistorySource::Kernel { .. } if minor < LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY => {
                    return Err(BodyError::FieldUnavailable);
                }
                _ => {}
            }
            if record.subsystem.len() > LOGGING_MAX_SUBSYSTEM_BYTES
                || record.message.len() > LOGGING_MAX_MESSAGE_BYTES
            {
                return Err(BodyError::LimitExceeded);
            }
            112 + align_eight(record.subsystem.len() + record.message.len())
        }
    };
    let mut encoder =
        BodyEncoder::new(&mut output, body_bytes, 24, BodyLimits::ENDPOINT_PROTOTYPE)?;
    match record {
        None => encoder.root().optional_none(0)?,
        Some(record) => {
            let mut text = [0; LOGGING_MAX_HISTORY_TEXT_BYTES];
            let subsystem_bytes = record.subsystem.len();
            let message_bytes = record.message.len();
            text[..subsystem_bytes].copy_from_slice(record.subsystem.as_bytes());
            text[subsystem_bytes..subsystem_bytes + message_bytes]
                .copy_from_slice(record.message.as_bytes());
            let (source_value, source_process_id, source_identity, has_wall_time) =
                match record.source {
                    HistorySource::Process {
                        process_id,
                        wall_time_unix_ns,
                        trace_id,
                    } => (
                        wall_time_unix_ns.unwrap_or(0),
                        process_id,
                        trace_id,
                        wall_time_unix_ns.is_some(),
                    ),
                    HistorySource::Kernel { sequence, boot_id } => (
                        sequence.get(),
                        0,
                        boot_id.map_or([0; EVENT_ID_BYTES], BootId::into_bytes),
                        false,
                    ),
                };
            encoder.root().optional_some(0, 88, |payload| {
                payload.write_u64(0, record.record_id.get())?;
                payload.write_u64(8, record.monotonic_time_ns)?;
                payload.write_u64(16, source_value)?;
                payload.write_u64(24, source_process_id)?;
                payload.write_id128(32, record.event_id.into_bytes())?;
                payload.write_id128(48, source_identity)?;
                payload.bytes(
                    64,
                    LOGGING_MAX_HISTORY_TEXT_BYTES as u32,
                    &text[..subsystem_bytes + message_bytes],
                )?;
                payload.write_u8(80, record.severity as u8)?;
                payload.write_u8(81, record.privacy as u8)?;
                payload.write_bool(82, has_wall_time)?;
                payload.write_u8(83, subsystem_bytes as u8)?;
                payload.write_u8(84, message_bytes as u8)
            })?;
        }
    }
    encoder.finish()?;
    body_buf(&output[..body_bytes])
}

pub fn decode_history_read_response<'wire>(
    body: &'wire [u8],
    bound: &BoundProtocol<'_>,
) -> Result<Option<HistoryRecordView<'wire>>, BodyError> {
    require_collector_reads(bound)?;
    let record =
        validate_body::<HistoryReadResponseSchema>(body, bound, BodyLimits::ENDPOINT_PROTOTYPE)?
            .materialize()?;
    if matches!(
        record,
        Some(HistoryRecordView {
            source: HistorySource::Kernel { .. },
            ..
        }) if bound.minor() < LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY
    ) {
        return Err(BodyError::FieldUnavailable);
    }
    Ok(record)
}

fn body_buf(body: &[u8]) -> Result<BodyBuf, BodyError> {
    BodyBuf::from_slice(body).map_err(|_| BodyError::OutputTooSmall)
}

fn validate_collector_stats_request(
    body: &[u8],
    bound: &BoundProtocol<'_>,
) -> Result<(), BodyError> {
    decode_collector_stats_request(body, bound)
}

fn validate_collector_stats_response(
    body: &[u8],
    bound: &BoundProtocol<'_>,
) -> Result<(), BodyError> {
    decode_collector_stats_response(body, bound).map(|_| ())
}

fn validate_history_read_request(body: &[u8], bound: &BoundProtocol<'_>) -> Result<(), BodyError> {
    decode_history_read_request(body, bound).map(|_| ())
}

fn validate_history_read_response(body: &[u8], bound: &BoundProtocol<'_>) -> Result<(), BodyError> {
    decode_history_read_response(body, bound).map(|_| ())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectorStatsTransactionId(u64);

impl CollectorStatsTransactionId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryReadTransactionId(u64);

impl HistoryReadTransactionId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoggingResponseError {
    UnexpectedEvent,
    TransactionMismatch,
    Transport(TransportStatus),
    Body(BodyError),
}

impl From<BodyError> for LoggingResponseError {
    fn from(value: BodyError) -> Self {
        Self::Body(value)
    }
}

pub fn decode_collector_stats_client_response(
    event: &ClientEvent,
    transaction: CollectorStatsTransactionId,
    bound: &BoundProtocol<'_>,
) -> Result<CollectorStats, LoggingResponseError> {
    let ClientEvent::Response {
        transaction_id,
        status,
        body,
    } = event
    else {
        return Err(LoggingResponseError::UnexpectedEvent);
    };
    if *transaction_id != transaction.get() {
        return Err(LoggingResponseError::TransactionMismatch);
    }
    if *status != TransportStatus::Ok {
        return Err(LoggingResponseError::Transport(*status));
    }
    decode_collector_stats_response(body.as_slice(), bound).map_err(Into::into)
}

pub fn decode_history_read_client_response<'wire>(
    event: &'wire ClientEvent,
    transaction: HistoryReadTransactionId,
    bound: &BoundProtocol<'_>,
) -> Result<Option<HistoryRecordView<'wire>>, LoggingResponseError> {
    let ClientEvent::Response {
        transaction_id,
        status,
        body,
    } = event
    else {
        return Err(LoggingResponseError::UnexpectedEvent);
    };
    if *transaction_id != transaction.get() {
        return Err(LoggingResponseError::TransactionMismatch);
    }
    if *status != TransportStatus::Ok {
        return Err(LoggingResponseError::Transport(*status));
    }
    decode_history_read_response(body.as_slice(), bound).map_err(Into::into)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectorRequest {
    GetCollectorStats,
    ReadHistory(HistoryReadRequest),
}

pub fn decode_collector_request(
    token: RequestToken,
    body: &[u8],
    bound: &BoundProtocol<'_>,
) -> Result<CollectorRequest, BodyError> {
    match token.ordinal() {
        LOGGING_GET_COLLECTOR_STATS_ORDINAL => {
            decode_collector_stats_request(body, bound)?;
            Ok(CollectorRequest::GetCollectorStats)
        }
        LOGGING_READ_HISTORY_ORDINAL => {
            decode_history_read_request(body, bound).map(CollectorRequest::ReadHistory)
        }
        _ => Err(BodyError::InvalidOrdinal),
    }
}

pub fn respond_collector_stats<T: TryTransport>(
    server: &mut Server<'_, T>,
    token: RequestToken,
    stats: CollectorStats,
) -> Result<(), RuntimeError> {
    if token.ordinal() != LOGGING_GET_COLLECTOR_STATS_ORDINAL {
        return Err(RuntimeError::WrongMethodKind);
    }
    let minor = server.bound().ok_or(RuntimeError::InvalidState)?.minor();
    let body = encode_collector_stats_response(stats, minor)?;
    server.respond(token, TransportStatus::Ok, body.as_slice())
}

pub fn respond_history<T: TryTransport>(
    server: &mut Server<'_, T>,
    token: RequestToken,
    record: Option<HistoryRecordView<'_>>,
) -> Result<(), RuntimeError> {
    if token.ordinal() != LOGGING_READ_HISTORY_ORDINAL {
        return Err(RuntimeError::WrongMethodKind);
    }
    let minor = server.bound().ok_or(RuntimeError::InvalidState)?.minor();
    let body = encode_history_read_response(record, minor)?;
    server.respond(token, TransportStatus::Ok, body.as_slice())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelLogRecordView<'wire> {
    pub sequence: KernelSequence,
    pub boot_id: Option<BootId>,
    pub event_id: EventId,
    pub severity: LogSeverity,
    pub privacy: PrivacyClass,
    pub monotonic_time_ns: u64,
    pub subsystem: &'wire str,
    pub message: &'wire str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectorError {
    ZeroServiceGeneration,
    ZeroSourceProcessId,
    RecordTooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectDisposition {
    Accepted {
        record_id: RecordId,
        evicted_record_id: Option<RecordId>,
    },
    Dropped,
}

const EMPTY_STORED_EVENT_ID: EventId = match EventId::from_bytes([
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
]) {
    Ok(id) => id,
    Err(_) => panic!("empty stored-record event ID must be a valid UUIDv4"),
};

#[derive(Clone, Copy)]
struct StoredRecord {
    record_id: RecordId,
    source: HistorySource,
    event_id: EventId,
    severity: LogSeverity,
    privacy: PrivacyClass,
    monotonic_time_ns: u64,
    subsystem: [u8; LOGGING_MAX_SUBSYSTEM_BYTES],
    subsystem_len: u8,
    message: [u8; LOGGING_MAX_MESSAGE_BYTES],
    message_len: u8,
}

impl StoredRecord {
    const EMPTY: Self = Self {
        record_id: RecordId(1),
        source: HistorySource::Process {
            process_id: 1,
            wall_time_unix_ns: None,
            trace_id: [0; 16],
        },
        event_id: EMPTY_STORED_EVENT_ID,
        severity: LogSeverity::Trace,
        privacy: PrivacyClass::Public,
        monotonic_time_ns: 0,
        subsystem: [0; LOGGING_MAX_SUBSYSTEM_BYTES],
        subsystem_len: 0,
        message: [0; LOGGING_MAX_MESSAGE_BYTES],
        message_len: 0,
    };

    fn view(&self) -> Option<HistoryRecordView<'_>> {
        let subsystem = str::from_utf8(&self.subsystem[..usize::from(self.subsystem_len)]).ok()?;
        let message = str::from_utf8(&self.message[..usize::from(self.message_len)]).ok()?;
        Some(HistoryRecordView {
            record_id: self.record_id,
            source: self.source,
            event_id: self.event_id,
            severity: self.severity,
            privacy: self.privacy,
            monotonic_time_ns: self.monotonic_time_ns,
            subsystem,
            message,
        })
    }
}

pub struct FixedLoggingCollector<const N: usize> {
    service_generation: u64,
    slots: [StoredRecord; N],
    head: usize,
    len: usize,
    next_record_id: Option<RecordId>,
    received_records: u64,
    evicted_records: u64,
    dropped_records: u64,
    redacted_records: u64,
}

impl<const N: usize> FixedLoggingCollector<N> {
    pub fn new(service_generation: u64) -> Result<Self, CollectorError> {
        if service_generation == 0 {
            return Err(CollectorError::ZeroServiceGeneration);
        }
        Ok(Self {
            service_generation,
            slots: [StoredRecord::EMPTY; N],
            head: 0,
            len: 0,
            next_record_id: Some(RecordId(1)),
            received_records: 0,
            evicted_records: 0,
            dropped_records: 0,
            redacted_records: 0,
        })
    }

    pub const fn service_generation(&self) -> u64 {
        self.service_generation
    }

    pub fn collect(
        &mut self,
        source_process_id: u64,
        record: LogRecordView<'_>,
        trace_id: [u8; 16],
    ) -> Result<CollectDisposition, CollectorError> {
        if source_process_id == 0 {
            return Err(CollectorError::ZeroSourceProcessId);
        }
        self.collect_record(
            HistorySource::Process {
                process_id: source_process_id,
                wall_time_unix_ns: record.wall_time_unix_ns,
                trace_id,
            },
            record.event_id,
            record.severity,
            record.privacy,
            record.monotonic_time_ns,
            record.subsystem,
            record.message,
        )
    }

    pub fn collect_kernel(
        &mut self,
        record: KernelLogRecordView<'_>,
    ) -> Result<CollectDisposition, CollectorError> {
        self.collect_record(
            HistorySource::Kernel {
                sequence: record.sequence,
                boot_id: record.boot_id,
            },
            record.event_id,
            record.severity,
            record.privacy,
            record.monotonic_time_ns,
            record.subsystem,
            record.message,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_record(
        &mut self,
        source: HistorySource,
        event_id: EventId,
        severity: LogSeverity,
        privacy: PrivacyClass,
        monotonic_time_ns: u64,
        subsystem: &str,
        message: &str,
    ) -> Result<CollectDisposition, CollectorError> {
        if subsystem.len() > LOGGING_MAX_SUBSYSTEM_BYTES
            || message.len() > LOGGING_MAX_MESSAGE_BYTES
        {
            return Err(CollectorError::RecordTooLarge);
        }
        self.received_records = self.received_records.saturating_add(1);
        if N == 0 {
            self.dropped_records = self.dropped_records.saturating_add(1);
            return Ok(CollectDisposition::Dropped);
        }
        let Some(record_id) = self.next_record_id else {
            self.dropped_records = self.dropped_records.saturating_add(1);
            return Ok(CollectDisposition::Dropped);
        };
        self.next_record_id = record_id.get().checked_add(1).map(RecordId);

        let message = if privacy == PrivacyClass::SecretNeverPersist {
            COLLECTOR_SECRET_REDACTION
        } else {
            message
        };
        let mut stored = StoredRecord {
            record_id,
            source,
            event_id,
            severity,
            privacy,
            monotonic_time_ns,
            subsystem: [0; LOGGING_MAX_SUBSYSTEM_BYTES],
            subsystem_len: subsystem.len() as u8,
            message: [0; LOGGING_MAX_MESSAGE_BYTES],
            message_len: message.len() as u8,
        };
        stored.subsystem[..subsystem.len()].copy_from_slice(subsystem.as_bytes());
        stored.message[..message.len()].copy_from_slice(message.as_bytes());

        let evicted_record_id = if self.len < N {
            let index = (self.head + self.len) % N;
            self.slots[index] = stored;
            self.len += 1;
            None
        } else {
            let evicted = self.slots[self.head].record_id;
            self.slots[self.head] = stored;
            self.head = (self.head + 1) % N;
            self.evicted_records = self.evicted_records.saturating_add(1);
            Some(evicted)
        };
        if privacy == PrivacyClass::SecretNeverPersist {
            self.redacted_records = self.redacted_records.saturating_add(1);
        }
        Ok(CollectDisposition::Accepted {
            record_id,
            evicted_record_id,
        })
    }

    pub fn stats(&self) -> CollectorStats {
        let oldest_record_id = if self.len == 0 {
            None
        } else {
            Some(self.slots[self.head].record_id)
        };
        let newest_record_id = if self.len == 0 {
            None
        } else {
            Some(self.slots[(self.head + self.len - 1) % N].record_id)
        };
        CollectorStats {
            received_records: self.received_records,
            retained_records: self.len as u64,
            capacity_records: N as u64,
            evicted_records: self.evicted_records,
            dropped_records: self.dropped_records,
            redacted_records: self.redacted_records,
            oldest_record_id,
            newest_record_id,
        }
    }

    /// Reads using the current history semantics, including process and kernel records.
    pub fn read_after(&self, after_record_id: Option<RecordId>) -> Option<HistoryRecordView<'_>> {
        self.read_after_for_minor(after_record_id, LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY)
    }

    /// Reads the next record representable by the negotiated logging protocol minor.
    ///
    /// Minor 2 skips retained kernel records while preserving collector record-ID cursor order.
    pub fn read_after_for_minor(
        &self,
        after_record_id: Option<RecordId>,
        minor: u16,
    ) -> Option<HistoryRecordView<'_>> {
        let after = encode_optional_record_id(after_record_id);
        for offset in 0..self.len {
            let record = &self.slots[(self.head + offset) % N];
            let source_is_compatible = minor >= LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY
                || matches!(record.source, HistorySource::Process { .. });
            if record.record_id.get() > after && source_is_compatible {
                return record.view();
            }
        }
        None
    }
}

static LOGGING_VERSIONS: [MinorVersionProfile; 4] = [
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
    MinorVersionProfile {
        minor: LOGGING_PROTOCOL_MINOR_COLLECTOR_READS,
        minimum_body_bytes: 192,
        minimum_handles: 0,
    },
    MinorVersionProfile {
        minor: LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY,
        minimum_body_bytes: 192,
        minimum_handles: 0,
    },
];
static LOGGING_METHODS: [MethodDescriptor; 3] = [
    MethodDescriptor {
        ordinal: LOGGING_EMIT_ORDINAL,
        kind: MethodKind::OneWay,
        deadline: DeadlinePolicy::Forbidden,
        validate_request: validate_log_record,
        validate_response: validate_log_record,
    },
    MethodDescriptor {
        ordinal: LOGGING_GET_COLLECTOR_STATS_ORDINAL,
        kind: MethodKind::RequestResponse,
        deadline: DeadlinePolicy::Required {
            max_duration_ns: None,
        },
        validate_request: validate_collector_stats_request,
        validate_response: validate_collector_stats_response,
    },
    MethodDescriptor {
        ordinal: LOGGING_READ_HISTORY_ORDINAL,
        kind: MethodKind::RequestResponse,
        deadline: DeadlinePolicy::Required {
            max_duration_ns: None,
        },
        validate_request: validate_history_read_request,
        validate_response: validate_history_read_response,
    },
];

pub fn logging_protocol() -> ProtocolDescriptor<'static> {
    logging_protocol_through(LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY)
}

pub fn logging_protocol_through(max_minor: u16) -> ProtocolDescriptor<'static> {
    let max_minor = max_minor.min(LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY);
    let versions = match max_minor {
        LOGGING_PROTOCOL_MINOR_BASE => &LOGGING_VERSIONS[..1],
        LOGGING_PROTOCOL_MINOR_WALL_TIME => &LOGGING_VERSIONS[..2],
        LOGGING_PROTOCOL_MINOR_COLLECTOR_READS => &LOGGING_VERSIONS[..3],
        _ => &LOGGING_VERSIONS,
    };
    let methods = if max_minor < LOGGING_PROTOCOL_MINOR_COLLECTOR_READS {
        &LOGGING_METHODS[..1]
    } else {
        &LOGGING_METHODS
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
        methods,
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

    pub fn try_get_collector_stats(
        &mut self,
        now_ns: u64,
        deadline_ns: u64,
        trace_id: [u8; 16],
    ) -> Result<CollectorStatsTransactionId, RuntimeError> {
        let minor = self
            .client
            .bound()
            .ok_or(RuntimeError::InvalidState)?
            .minor();
        let body = encode_collector_stats_request(minor)?;
        self.client
            .try_call(
                LOGGING_GET_COLLECTOR_STATS_ORDINAL,
                body.as_slice(),
                now_ns,
                deadline_ns,
                trace_id,
            )
            .map(CollectorStatsTransactionId)
    }

    pub fn try_read_history(
        &mut self,
        after_record_id: Option<RecordId>,
        now_ns: u64,
        deadline_ns: u64,
        trace_id: [u8; 16],
    ) -> Result<HistoryReadTransactionId, RuntimeError> {
        let minor = self
            .client
            .bound()
            .ok_or(RuntimeError::InvalidState)?
            .minor();
        let body = encode_history_read_request(HistoryReadRequest { after_record_id }, minor)?;
        self.client
            .try_call(
                LOGGING_READ_HISTORY_ORDINAL,
                body.as_slice(),
                now_ns,
                deadline_ns,
                trace_id,
            )
            .map(HistoryReadTransactionId)
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

const fn align_eight(value: usize) -> usize {
    (value + 7) & !7
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_EVENT_ID: EventId = match EventId::from_bytes([
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("test event ID must be valid"),
    };

    fn record<'a>(message: &'a str, privacy: PrivacyClass) -> LogRecordView<'a> {
        LogRecordView {
            event_id: TEST_EVENT_ID,
            severity: LogSeverity::Info,
            privacy,
            monotonic_time_ns: 1,
            subsystem: "security",
            message,
            wall_time_unix_ns: None,
        }
    }

    #[test]
    fn empty_slot_event_id_preserves_the_uuidv4_invariant() {
        assert_eq!(
            EventId::from_bytes(StoredRecord::EMPTY.event_id.into_bytes()),
            Ok(EMPTY_STORED_EVENT_ID)
        );
    }

    #[test]
    fn secret_bytes_are_never_copied_into_fixed_storage() {
        const SECRET: &str = "raw credential material";
        let mut collector = FixedLoggingCollector::<1>::new(1).unwrap();
        collector
            .collect(9, record(SECRET, PrivacyClass::SecretNeverPersist), [0; 16])
            .unwrap();

        let slot = &collector.slots[0];
        assert_eq!(
            &slot.message[..usize::from(slot.message_len)],
            COLLECTOR_SECRET_REDACTION.as_bytes()
        );
        assert!(
            !slot
                .message
                .windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes())
        );
        assert_eq!(collector.stats().redacted_records, 1);
        assert_eq!(
            collector.read_after(None).unwrap().message,
            COLLECTOR_SECRET_REDACTION
        );
    }

    #[test]
    fn maximum_record_id_is_assigned_once_and_never_wraps() {
        let mut collector = FixedLoggingCollector::<2>::new(2).unwrap();
        collector.next_record_id = Some(RecordId(u64::MAX));
        assert_eq!(
            collector
                .collect(1, record("last", PrivacyClass::Public), [0; 16])
                .unwrap(),
            CollectDisposition::Accepted {
                record_id: RecordId(u64::MAX),
                evicted_record_id: None,
            }
        );
        assert_eq!(collector.next_record_id, None);
        assert_eq!(
            collector
                .collect(1, record("dropped", PrivacyClass::Public), [0; 16])
                .unwrap(),
            CollectDisposition::Dropped
        );
        let stats = collector.stats();
        assert_eq!(stats.received_records, 2);
        assert_eq!(stats.retained_records, 1);
        assert_eq!(stats.dropped_records, 1);
        assert_eq!(stats.oldest_record_id, Some(RecordId(u64::MAX)));
        assert_eq!(stats.newest_record_id, Some(RecordId(u64::MAX)));
    }

    #[test]
    fn collector_counters_saturate() {
        let mut dropping = FixedLoggingCollector::<0>::new(3).unwrap();
        dropping.received_records = u64::MAX;
        dropping.dropped_records = u64::MAX;
        dropping
            .collect(1, record("drop", PrivacyClass::Public), [0; 16])
            .unwrap();
        assert_eq!(dropping.received_records, u64::MAX);
        assert_eq!(dropping.dropped_records, u64::MAX);

        let mut redacting = FixedLoggingCollector::<1>::new(4).unwrap();
        redacting.redacted_records = u64::MAX;
        redacting
            .collect(
                1,
                record("secret", PrivacyClass::SecretNeverPersist),
                [0; 16],
            )
            .unwrap();
        assert_eq!(redacting.redacted_records, u64::MAX);
    }
}
