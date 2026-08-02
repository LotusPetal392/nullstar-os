use nswp_core::{
    Availability, BodyEncoder, BodyError, BodyLimits, BoundProtocol, InlineLayout,
    MinorVersionProfile, PrimitiveKind, ProtocolBodyDescriptor, ProtocolId, TableDescriptor,
    TableFieldDescriptor, TypeDescriptor, TypeKind, ValidatedValue, WireSchema, validate_body,
};
use nswp_runtime::{
    BodyBuf, DeadlinePolicy, HANDLE_FREE_ENDPOINT_LIMITS, MethodDescriptor, MethodKind,
    ProtocolDescriptor,
};

pub const ECHO_PROTOCOL_ID: ProtocolId = match ProtocolId::from_bytes([
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
]) {
    Ok(id) => id,
    Err(_) => panic!("Echo protocol ID must be canonical"),
};
pub const ECHO_PROTOCOL_MAJOR: u16 = 1;
pub const ECHO_PROTOCOL_MINOR: u16 = 0;
pub const ECHO_PING_ORDINAL: u32 = 1;
pub const ECHO_MAX_DEADLINE_NS: u64 = 5_000_000_000;

static STRING_TYPE: TypeDescriptor = TypeDescriptor::string(32);
static U64_TYPE: TypeDescriptor = TypeDescriptor::primitive(PrimitiveKind::U64);
static ECHO_FIELDS: [TableFieldDescriptor; 2] = [
    TableFieldDescriptor {
        ordinal: 1,
        ty: &STRING_TYPE,
        required: true,
        availability: Availability::ALWAYS,
    },
    TableFieldDescriptor {
        ordinal: 2,
        ty: &U64_TYPE,
        required: true,
        availability: Availability::ALWAYS,
    },
];
static ECHO_TABLE: TableDescriptor = TableDescriptor {
    maximum_present_fields: 2,
    fields: &ECHO_FIELDS,
    reserved_ordinals: &[],
};
static ECHO_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout::TABLE,
    kind: TypeKind::Table(&ECHO_TABLE),
};
pub static ECHO_BODY_DESCRIPTOR: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: ECHO_PROTOCOL_ID,
    protocol_major: ECHO_PROTOCOL_MAJOR,
    root: &ECHO_TYPE,
};

pub struct EchoMessageSchema;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoMessageView<'wire> {
    pub message: &'wire str,
    pub sequence: u64,
}

impl WireSchema for EchoMessageSchema {
    type View<'wire> = EchoMessageView<'wire>;

    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &ECHO_BODY_DESCRIPTOR;

    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        let message = value
            .table_field(1)?
            .and_then(|field| field.value())
            .ok_or(BodyError::MaterializationMismatch)?
            .string()?;
        let sequence = value
            .table_field(2)?
            .and_then(|field| field.value())
            .ok_or(BodyError::MaterializationMismatch)?
            .u64()?;
        Ok(EchoMessageView { message, sequence })
    }
}

static ECHO_VERSIONS: [MinorVersionProfile; 1] = [MinorVersionProfile {
    minor: ECHO_PROTOCOL_MINOR,
    minimum_body_bytes: 120,
    minimum_handles: 0,
}];

static ECHO_METHODS: [MethodDescriptor; 1] = [MethodDescriptor {
    ordinal: ECHO_PING_ORDINAL,
    kind: MethodKind::RequestResponse,
    deadline: DeadlinePolicy::Optional {
        max_duration_ns: Some(ECHO_MAX_DEADLINE_NS),
    },
    validate_request: validate_echo,
    validate_response: validate_echo,
}];

pub fn echo_protocol() -> ProtocolDescriptor<'static> {
    ProtocolDescriptor {
        protocol_id: ECHO_PROTOCOL_ID,
        major: ECHO_PROTOCOL_MAJOR,
        min_minor: ECHO_PROTOCOL_MINOR,
        max_minor: ECHO_PROTOCOL_MINOR,
        limits: HANDLE_FREE_ENDPOINT_LIMITS,
        requested_features: &[],
        available_features: &[],
        versions: &ECHO_VERSIONS,
        feature_set_fits: nswp_runtime::no_features_fit,
        methods: &ECHO_METHODS,
    }
}

pub fn encode_echo(message: &str, sequence: u64) -> Result<BodyBuf, BodyError> {
    if message.len() > 32 {
        return Err(BodyError::LimitExceeded);
    }
    let body_bytes = 88 + align_eight(message.len());
    let mut output = [0; nswp_runtime::MAX_BODY_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut output, body_bytes, 16, BodyLimits::ENDPOINT_PROTOTYPE)?;
    encoder.root().table(0, 2, 2, |table| {
        table.field(1, 16, |value| value.string(0, 32, message))?;
        table.field(2, 8, |value| value.write_u64(0, sequence))
    })?;
    encoder.finish()?;
    BodyBuf::from_slice(&output[..body_bytes]).map_err(|_| BodyError::OutputTooSmall)
}

pub fn decode_echo<'wire>(
    body: &'wire [u8],
    bound: &BoundProtocol<'_>,
) -> Result<EchoMessageView<'wire>, BodyError> {
    validate_body::<EchoMessageSchema>(body, bound, BodyLimits::ENDPOINT_PROTOTYPE)?.materialize()
}

fn validate_echo(body: &[u8], bound: &BoundProtocol<'_>) -> Result<(), BodyError> {
    validate_body::<EchoMessageSchema>(body, bound, BodyLimits::ENDPOINT_PROTOTYPE).map(|_| ())
}

const fn align_eight(value: usize) -> usize {
    (value + 7) & !7
}

#[derive(Debug, Default)]
pub struct EchoService {
    dispatch_count: usize,
}

impl EchoService {
    pub const fn new() -> Self {
        Self { dispatch_count: 0 }
    }

    pub const fn dispatch_count(&self) -> usize {
        self.dispatch_count
    }

    pub fn dispatch<'wire>(
        &mut self,
        body: &'wire [u8],
        bound: &BoundProtocol<'_>,
    ) -> Result<EchoMessageView<'wire>, BodyError> {
        let message = decode_echo(body, bound)?;
        self.dispatch_count += 1;
        Ok(message)
    }
}

pub const ECHO_HI_7_BODY: [u8; 96] = [
    0x10, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x18, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x28, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x10, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x68, 0x69, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
