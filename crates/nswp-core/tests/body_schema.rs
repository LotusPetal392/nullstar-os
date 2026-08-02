use std::sync::atomic::{AtomicUsize, Ordering};

use nswp_core::{
    Availability, BodyEncoder, BodyError, BodyLimits, BoundProtocol, ClosedEnumDescriptor,
    ClosedUnionDescriptor, ConnectionLimits, InlineLayout, IntegerRepr, OrdinalRange,
    PrimitiveKind, ProtocolBodyDescriptor, ProtocolId, SchemaValueEncoder, StructureDescriptor,
    StructureFieldDescriptor, TableDescriptor, TableFieldDescriptor, TypeDescriptor, TypeKind,
    UnionAlternativeDescriptor, ValidatedValue, VectorDescriptor, WireSchema, validate_body,
};

const PROTOCOL_ID: ProtocolId = match ProtocolId::from_bytes([
    0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0x4c, 0xde, 0x81, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
]) {
    Ok(id) => id,
    Err(_) => panic!("test protocol ID must be canonical"),
};

static U32_TYPE: TypeDescriptor = TypeDescriptor::primitive(PrimitiveKind::U32);
static U64_TYPE: TypeDescriptor = TypeDescriptor::primitive(PrimitiveKind::U64);
static BOOL_TYPE: TypeDescriptor = TypeDescriptor::primitive(PrimitiveKind::Bool);
static STRING_TYPE: TypeDescriptor = TypeDescriptor::string(32);
static VECTOR_STRING_DESCRIPTOR: VectorDescriptor = VectorDescriptor {
    maximum_elements: 4,
    element: &STRING_TYPE,
};
static VECTOR_STRING_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout::SLICE,
    kind: TypeKind::Vector(&VECTOR_STRING_DESCRIPTOR),
};
static OPTIONAL_U64_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout::ENVELOPE,
    kind: TypeKind::Optional { value: &U64_TYPE },
};
static ENUM_VALUES: [u64; 2] = [1, 3];
static ENUM_DESCRIPTOR: ClosedEnumDescriptor = ClosedEnumDescriptor {
    repr: IntegerRepr::U32,
    values: &ENUM_VALUES,
};
static ENUM_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout {
        bytes: 4,
        alignment: 4,
    },
    kind: TypeKind::ClosedEnum(&ENUM_DESCRIPTOR),
};
static UNION_ALTERNATIVES: [UnionAlternativeDescriptor; 2] = [
    UnionAlternativeDescriptor {
        ordinal: 1,
        payload: None,
    },
    UnionAlternativeDescriptor {
        ordinal: 2,
        payload: Some(&STRING_TYPE),
    },
];
static UNION_DESCRIPTOR: ClosedUnionDescriptor = ClosedUnionDescriptor {
    alternatives: &UNION_ALTERNATIVES,
};
static UNION_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout::ENVELOPE,
    kind: TypeKind::ClosedUnion(&UNION_DESCRIPTOR),
};

static FEATURE_7: [u32; 1] = [7];
static REQUEST_FIELDS: [TableFieldDescriptor; 7] = [
    TableFieldDescriptor {
        ordinal: 1,
        ty: &STRING_TYPE,
        required: true,
        availability: Availability::ALWAYS,
    },
    TableFieldDescriptor {
        ordinal: 2,
        ty: &VECTOR_STRING_TYPE,
        required: false,
        availability: Availability::ALWAYS,
    },
    TableFieldDescriptor {
        ordinal: 3,
        ty: &U64_TYPE,
        required: true,
        availability: Availability {
            since_minor: 2,
            required_features: &[],
        },
    },
    TableFieldDescriptor {
        ordinal: 4,
        ty: &BOOL_TYPE,
        required: true,
        availability: Availability {
            since_minor: 0,
            required_features: &FEATURE_7,
        },
    },
    TableFieldDescriptor {
        ordinal: 5,
        ty: &ENUM_TYPE,
        required: false,
        availability: Availability::ALWAYS,
    },
    TableFieldDescriptor {
        ordinal: 6,
        ty: &UNION_TYPE,
        required: false,
        availability: Availability::ALWAYS,
    },
    TableFieldDescriptor {
        ordinal: 7,
        ty: &OPTIONAL_U64_TYPE,
        required: false,
        availability: Availability::ALWAYS,
    },
];
static RESERVED: [OrdinalRange; 1] = [OrdinalRange { first: 9, last: 10 }];
static REQUEST_TABLE_DESCRIPTOR: TableDescriptor = TableDescriptor {
    maximum_present_fields: 10,
    fields: &REQUEST_FIELDS,
    reserved_ordinals: &RESERVED,
};
static REQUEST_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout::TABLE,
    kind: TypeKind::Table(&REQUEST_TABLE_DESCRIPTOR),
};
static REQUEST_PROTOCOL: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: PROTOCOL_ID,
    protocol_major: 1,
    root: &REQUEST_TYPE,
};

static MATERIALIZE_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, PartialEq, Eq)]
struct RequestView<'wire> {
    message: &'wire str,
}

struct RequestSchema;

impl WireSchema for RequestSchema {
    type View<'wire> = RequestView<'wire>;

    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &REQUEST_PROTOCOL;

    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        MATERIALIZE_COUNT.fetch_add(1, Ordering::SeqCst);
        let message = value
            .table_field(1)?
            .and_then(|field| field.value())
            .ok_or(BodyError::MaterializationMismatch)?
            .string()?;
        Ok(RequestView { message })
    }
}

static VECTOR_U64_DESCRIPTOR: VectorDescriptor = VectorDescriptor {
    maximum_elements: 4,
    element: &U64_TYPE,
};
static VECTOR_U64_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout::SLICE,
    kind: TypeKind::Vector(&VECTOR_U64_DESCRIPTOR),
};
static VECTOR_U64_PROTOCOL: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: PROTOCOL_ID,
    protocol_major: 1,
    root: &VECTOR_U64_TYPE,
};

struct VectorU64Schema;
impl WireSchema for VectorU64Schema {
    type View<'wire> = (u64, u64, u64);
    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &VECTOR_U64_PROTOCOL;

    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        let vector = value.vector()?;
        Ok((
            vector.get(0).unwrap().u64()?,
            vector.get(1).unwrap().u64()?,
            vector.get(2).unwrap().u64()?,
        ))
    }
}

static VECTOR_STRING_PROTOCOL: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: PROTOCOL_ID,
    protocol_major: 1,
    root: &VECTOR_STRING_TYPE,
};
struct VectorStringSchema;
impl WireSchema for VectorStringSchema {
    type View<'wire> = (&'wire str, &'wire str);
    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &VECTOR_STRING_PROTOCOL;

    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        let vector = value.vector()?;
        Ok((
            vector.get(0).unwrap().string()?,
            vector.get(1).unwrap().string()?,
        ))
    }
}

fn bound(minor: u16, features: &'static [u32]) -> BoundProtocol<'static> {
    BoundProtocol::new(
        PROTOCOL_ID,
        1,
        minor,
        ConnectionLimits::DESKTOP,
        1,
        features,
    )
    .unwrap()
}

fn encode_table<F>(body_bytes: usize, field_count: u16, encode: F) -> Vec<u8>
where
    F: FnOnce(&mut nswp_core::TableEncoder<'_>) -> Result<(), BodyError>,
{
    let mut body = vec![0; body_bytes];
    let mut encoder = BodyEncoder::new(&mut body, body_bytes, 16, BodyLimits::DESKTOP).unwrap();
    encoder.root().table(0, field_count, 10, encode).unwrap();
    encoder.finish().unwrap();
    body
}

fn baseline_body() -> Vec<u8> {
    encode_table(64, 1, |table| {
        table.field(1, 16, |value| value.string(0, 32, "hello"))
    })
}

#[test]
fn required_fields_follow_minor_and_feature_availability() {
    let baseline = baseline_body();
    validate_body::<RequestSchema>(&baseline, &bound(1, &[]), BodyLimits::DESKTOP).unwrap();
    assert_eq!(
        validate_body::<RequestSchema>(&baseline, &bound(2, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::MissingRequiredField)
    );

    let future = encode_table(96, 2, |table| {
        table.field(1, 16, |value| value.string(0, 32, "hello"))?;
        table.field(3, 8, |value| value.write_u64(0, 7))
    });
    assert_eq!(
        validate_body::<RequestSchema>(&future, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::FieldUnavailable)
    );
    validate_body::<RequestSchema>(&future, &bound(2, &[]), BodyLimits::DESKTOP).unwrap();

    let feature_only = encode_table(96, 2, |table| {
        table.field(1, 16, |value| value.string(0, 32, "hello"))?;
        table.field(4, 1, |value| value.write_bool(0, true))
    });
    assert_eq!(
        validate_body::<RequestSchema>(&feature_only, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::FieldUnavailable)
    );

    static FEATURE: [u32; 1] = [7];
    assert_eq!(
        validate_body::<RequestSchema>(&future, &bound(2, &FEATURE), BodyLimits::DESKTOP).err(),
        Some(BodyError::MissingRequiredField)
    );
    let complete = encode_table(128, 3, |table| {
        table.field(1, 16, |value| value.string(0, 32, "hello"))?;
        table.field(3, 8, |value| value.write_u64(0, 7))?;
        table.field(4, 1, |value| value.write_bool(0, true))
    });
    validate_body::<RequestSchema>(&complete, &bound(2, &FEATURE), BodyLimits::DESKTOP).unwrap();
}

#[test]
fn unknown_fields_are_opaque_but_reserved_ordinals_are_rejected() {
    let unknown = encode_table(96, 2, |table| {
        table.field(1, 16, |value| value.string(0, 32, "hello"))?;
        table.field(8, 8, |value| value.write_u64(0, 0xdead_beef))
    });
    validate_body::<RequestSchema>(&unknown, &bound(1, &[]), BodyLimits::DESKTOP).unwrap();

    let reserved = encode_table(96, 2, |table| {
        table.field(1, 16, |value| value.string(0, 32, "hello"))?;
        table.field(9, 8, |value| value.write_u64(0, 0xdead_beef))
    });
    assert_eq!(
        validate_body::<RequestSchema>(&reserved, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::ReservedOrdinal)
    );
}

#[test]
fn validation_and_materialization_are_strictly_separate() {
    MATERIALIZE_COUNT.store(0, Ordering::SeqCst);
    let body = baseline_body();
    let validated =
        validate_body::<RequestSchema>(&body, &bound(1, &[]), BodyLimits::DESKTOP).unwrap();
    assert_eq!(MATERIALIZE_COUNT.load(Ordering::SeqCst), 0);
    assert_eq!(
        validated.materialize().unwrap(),
        RequestView { message: "hello" }
    );
    assert_eq!(MATERIALIZE_COUNT.load(Ordering::SeqCst), 1);

    let malformed = encode_table(96, 2, |table| {
        table.field(1, 16, |value| value.string(0, 32, "hello"))?;
        table.field(5, 4, |value| value.write_u32(0, 2))
    });
    assert_eq!(
        validate_body::<RequestSchema>(&malformed, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::UnknownEnumValue)
    );
    assert_eq!(MATERIALIZE_COUNT.load(Ordering::SeqCst), 1);
}

#[test]
fn scalar_and_dynamic_vectors_validate_and_materialize() {
    let mut scalar = [0; 40];
    let mut encoder = BodyEncoder::new(&mut scalar, 40, 16, BodyLimits::DESKTOP).unwrap();
    encoder
        .root()
        .vector(0, 3, 4, 8, 8, |vector| {
            vector.element(|value| value.write_u64(0, 1))?;
            vector.element(|value| value.write_u64(0, 2))?;
            vector.element(|value| value.write_u64(0, 3))
        })
        .unwrap();
    encoder.finish().unwrap();
    let validated =
        validate_body::<VectorU64Schema>(&scalar, &bound(1, &[]), BodyLimits::DESKTOP).unwrap();
    assert_eq!(validated.materialize().unwrap(), (1, 2, 3));

    let mut strings = [0; 64];
    let mut encoder = BodyEncoder::new(&mut strings, 64, 16, BodyLimits::DESKTOP).unwrap();
    encoder
        .root()
        .vector(0, 2, 4, 16, 4, |vector| {
            vector.element(|value| value.string(0, 32, "a"))?;
            vector.element(|value| value.string(0, 32, "bc"))
        })
        .unwrap();
    encoder.finish().unwrap();
    let validated =
        validate_body::<VectorStringSchema>(&strings, &bound(1, &[]), BodyLimits::DESKTOP).unwrap();
    assert_eq!(validated.materialize().unwrap(), ("a", "bc"));

    strings[32..36].copy_from_slice(&16_u32.to_le_bytes());
    assert_eq!(
        validate_body::<VectorStringSchema>(&strings, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::UnexpectedTarget)
    );
}

static OPTIONAL_PROTOCOL: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: PROTOCOL_ID,
    protocol_major: 1,
    root: &OPTIONAL_U64_TYPE,
};
struct OptionalSchema;
impl WireSchema for OptionalSchema {
    type View<'wire> = Option<u64>;
    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &OPTIONAL_PROTOCOL;

    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        value.optional()?.map(|value| value.u64()).transpose()
    }
}

static UNION_PROTOCOL: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: PROTOCOL_ID,
    protocol_major: 1,
    root: &UNION_TYPE,
};
struct UnionSchema;
impl WireSchema for UnionSchema {
    type View<'wire> = (u32, Option<&'wire str>);
    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &UNION_PROTOCOL;

    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        let union = value.union()?;
        Ok((
            union.ordinal(),
            union
                .payload()
                .map(|payload| payload.string())
                .transpose()?,
        ))
    }
}

static ENUM_PROTOCOL: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: PROTOCOL_ID,
    protocol_major: 1,
    root: &ENUM_TYPE,
};
struct EnumSchema;
impl WireSchema for EnumSchema {
    type View<'wire> = u64;
    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &ENUM_PROTOCOL;

    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        value.enum_raw()
    }
}

#[test]
fn optional_enum_and_closed_union_descriptors_are_strict() {
    let mut none = [0; 24];
    let mut encoder = BodyEncoder::new(&mut none, 24, 24, BodyLimits::DESKTOP).unwrap();
    encoder.root().optional_none(0).unwrap();
    encoder.finish().unwrap();
    let validated =
        validate_body::<OptionalSchema>(&none, &bound(1, &[]), BodyLimits::DESKTOP).unwrap();
    assert_eq!(validated.materialize().unwrap(), None);
    none[4] = 1;
    assert_eq!(
        validate_body::<OptionalSchema>(&none, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::ReservedValueUsed)
    );

    let mut some = [0; 32];
    let mut encoder = BodyEncoder::new(&mut some, 32, 24, BodyLimits::DESKTOP).unwrap();
    encoder
        .root()
        .optional_some(0, 8, |value| value.write_u64(0, 42))
        .unwrap();
    encoder.finish().unwrap();
    let validated =
        validate_body::<OptionalSchema>(&some, &bound(1, &[]), BodyLimits::DESKTOP).unwrap();
    assert_eq!(validated.materialize().unwrap(), Some(42));
    some[0..4].copy_from_slice(&2_u32.to_le_bytes());
    assert_eq!(
        validate_body::<OptionalSchema>(&some, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::InvalidOptionalOrdinal)
    );
    some[0..4].copy_from_slice(&1_u32.to_le_bytes());
    some[8..16].fill(0);
    assert_eq!(
        validate_body::<OptionalSchema>(&some, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::MissingPayload)
    );

    let mut enumeration = [0; 8];
    let mut encoder = BodyEncoder::new(&mut enumeration, 8, 4, BodyLimits::DESKTOP).unwrap();
    encoder.root().closed_enum(0, &ENUM_DESCRIPTOR, 3).unwrap();
    encoder.finish().unwrap();
    let validated =
        validate_body::<EnumSchema>(&enumeration, &bound(1, &[]), BodyLimits::DESKTOP).unwrap();
    assert_eq!(validated.materialize().unwrap(), 3);
    enumeration[..4].copy_from_slice(&2_u32.to_le_bytes());
    assert_eq!(
        validate_body::<EnumSchema>(&enumeration, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::UnknownEnumValue)
    );

    let mut unit = [0; 24];
    let mut encoder = BodyEncoder::new(&mut unit, 24, 24, BodyLimits::DESKTOP).unwrap();
    encoder.root().closed_union_unit(0, 1).unwrap();
    encoder.finish().unwrap();
    let validated =
        validate_body::<UnionSchema>(&unit, &bound(1, &[]), BodyLimits::DESKTOP).unwrap();
    assert_eq!(validated.materialize().unwrap(), (1, None));
    unit[0..4].copy_from_slice(&2_u32.to_le_bytes());
    assert_eq!(
        validate_body::<UnionSchema>(&unit, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::MissingPayload)
    );

    let mut payload = [0; 48];
    let mut encoder = BodyEncoder::new(&mut payload, 48, 24, BodyLimits::DESKTOP).unwrap();
    encoder
        .root()
        .closed_union(0, 2, 16, |value| value.string(0, 32, "ok"))
        .unwrap();
    encoder.finish().unwrap();
    let validated =
        validate_body::<UnionSchema>(&payload, &bound(1, &[]), BodyLimits::DESKTOP).unwrap();
    assert_eq!(validated.materialize().unwrap(), (2, Some("ok")));
    payload[0..4].copy_from_slice(&3_u32.to_le_bytes());
    assert_eq!(
        validate_body::<UnionSchema>(&payload, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::UnknownUnionOrdinal)
    );
    payload[0..4].copy_from_slice(&1_u32.to_le_bytes());
    assert_eq!(
        validate_body::<UnionSchema>(&payload, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::UnexpectedPayload)
    );
}

static STRUCTURE_FIELDS: [StructureFieldDescriptor; 2] = [
    StructureFieldDescriptor {
        offset: 0,
        ty: &BOOL_TYPE,
    },
    StructureFieldDescriptor {
        offset: 8,
        ty: &U64_TYPE,
    },
];
static STRUCTURE_DESCRIPTOR: StructureDescriptor = StructureDescriptor {
    fields: &STRUCTURE_FIELDS,
};
static STRUCTURE_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout {
        bytes: 16,
        alignment: 8,
    },
    kind: TypeKind::Structure(&STRUCTURE_DESCRIPTOR),
};
static STRUCTURE_PROTOCOL: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: PROTOCOL_ID,
    protocol_major: 1,
    root: &STRUCTURE_TYPE,
};
struct StructureSchema;
impl WireSchema for StructureSchema {
    type View<'wire> = (bool, u64);
    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &STRUCTURE_PROTOCOL;
    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError> {
        let flag = value
            .structure_field(0)?
            .ok_or(BodyError::MaterializationMismatch)?
            .bool()?;
        let number = value
            .structure_field(1)?
            .ok_or(BodyError::MaterializationMismatch)?
            .u64()?;
        if value.structure_field(2)?.is_some() {
            return Err(BodyError::MaterializationMismatch);
        }
        Ok((flag, number))
    }
}

static INVALID_STRUCTURE_FIELDS: [StructureFieldDescriptor; 2] = [
    StructureFieldDescriptor {
        offset: 0,
        ty: &U64_TYPE,
    },
    StructureFieldDescriptor {
        offset: 4,
        ty: &U32_TYPE,
    },
];
static INVALID_STRUCTURE_DESCRIPTOR: StructureDescriptor = StructureDescriptor {
    fields: &INVALID_STRUCTURE_FIELDS,
};
static INVALID_STRUCTURE_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout {
        bytes: 16,
        alignment: 8,
    },
    kind: TypeKind::Structure(&INVALID_STRUCTURE_DESCRIPTOR),
};
static INVALID_STRUCTURE_PROTOCOL: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: PROTOCOL_ID,
    protocol_major: 1,
    root: &INVALID_STRUCTURE_TYPE,
};
struct InvalidStructureSchema;
impl WireSchema for InvalidStructureSchema {
    type View<'wire> = ();
    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &INVALID_STRUCTURE_PROTOCOL;
    fn materialize<'wire>(_: ValidatedValue<'wire>) -> Result<(), BodyError> {
        Ok(())
    }
}

#[test]
fn structures_and_malformed_descriptors_are_checked_without_panics() {
    let mut body = [0; 16];
    body[0] = 1;
    body[8..16].copy_from_slice(&7_u64.to_le_bytes());
    assert_eq!(
        validate_body::<StructureSchema>(&body, &bound(1, &[]), BodyLimits::DESKTOP)
            .unwrap()
            .materialize(),
        Ok((true, 7))
    );
    body[1] = 1;
    assert_eq!(
        validate_body::<StructureSchema>(&body, &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::NonZeroPadding)
    );
    assert_eq!(
        validate_body::<InvalidStructureSchema>(&[0; 16], &bound(1, &[]), BodyLimits::DESKTOP)
            .err(),
        Some(BodyError::InvalidDescriptor)
    );
}

static RECURSIVE_TYPE: TypeDescriptor = TypeDescriptor {
    layout: InlineLayout::ENVELOPE,
    kind: TypeKind::Optional {
        value: &RECURSIVE_TYPE,
    },
};
static RECURSIVE_PROTOCOL: ProtocolBodyDescriptor = ProtocolBodyDescriptor {
    protocol_id: PROTOCOL_ID,
    protocol_major: 1,
    root: &RECURSIVE_TYPE,
};
struct RecursiveSchema;
impl WireSchema for RecursiveSchema {
    type View<'wire> = ();
    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &RECURSIVE_PROTOCOL;
    fn materialize<'wire>(_: ValidatedValue<'wire>) -> Result<(), BodyError> {
        Ok(())
    }
}

#[test]
fn static_descriptor_shape_is_independent_of_runtime_body_limits() {
    let baseline = baseline_body();
    validate_body::<RequestSchema>(
        &baseline,
        &bound(1, &[]),
        BodyLimits {
            max_depth: 2,
            max_table_fields: 1,
        },
    )
    .unwrap();

    let empty_vector = [0; 16];
    validate_body::<VectorU64Schema>(
        &empty_vector,
        &bound(1, &[]),
        BodyLimits {
            max_depth: 1,
            max_table_fields: 1,
        },
    )
    .unwrap();

    let optional_none = [0; 24];
    validate_body::<OptionalSchema>(
        &optional_none,
        &bound(1, &[]),
        BodyLimits {
            max_depth: 1,
            max_table_fields: 1,
        },
    )
    .unwrap();
}

#[test]
fn recursive_and_malformed_enum_descriptors_are_rejected_intrinsically() {
    assert_eq!(
        validate_body::<RecursiveSchema>(&[0; 24], &bound(1, &[]), BodyLimits::DESKTOP).err(),
        Some(BodyError::InvalidDescriptor)
    );

    static DUPLICATE_VALUES: [u64; 2] = [1, 1];
    static DUPLICATE_ENUM: ClosedEnumDescriptor = ClosedEnumDescriptor {
        repr: IntegerRepr::U32,
        values: &DUPLICATE_VALUES,
    };
    let mut body = [0; 8];
    let mut encoder = BodyEncoder::new(&mut body, 8, 4, BodyLimits::DESKTOP).unwrap();
    assert_eq!(
        encoder.root().closed_enum(0, &DUPLICATE_ENUM, 1),
        Err(BodyError::InvalidDescriptor)
    );
    encoder.finish().unwrap();
}

#[test]
fn schema_protocol_identity_and_negotiated_body_limit_are_enforced() {
    const OTHER_ID: ProtocolId = match ProtocolId::from_bytes([
        0x11, 0x23, 0x45, 0x67, 0x89, 0xab, 0x4c, 0xde, 0x81, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd,
        0xef,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("test protocol ID must be canonical"),
    };
    let wrong_protocol =
        BoundProtocol::new(OTHER_ID, 1, 1, ConnectionLimits::DESKTOP, 1, &[]).unwrap();
    let body = baseline_body();
    assert_eq!(
        validate_body::<RequestSchema>(&body, &wrong_protocol, BodyLimits::DESKTOP).err(),
        Some(BodyError::ProtocolMismatch)
    );

    let small = BoundProtocol::new(
        PROTOCOL_ID,
        1,
        1,
        ConnectionLimits {
            max_body_bytes: 32,
            max_handles: 0,
            max_outstanding: 1,
        },
        1,
        &[],
    )
    .unwrap();
    assert_eq!(
        validate_body::<RequestSchema>(&body, &small, BodyLimits::DESKTOP).err(),
        Some(BodyError::LimitExceeded)
    );
}

static SMOKE_MATERIALIZE_COUNT: AtomicUsize = AtomicUsize::new(0);
struct SmokeSchema;
impl WireSchema for SmokeSchema {
    type View<'wire> = ();
    const DESCRIPTOR: &'static ProtocolBodyDescriptor = &REQUEST_PROTOCOL;
    fn materialize<'wire>(_: ValidatedValue<'wire>) -> Result<(), BodyError> {
        SMOKE_MATERIALIZE_COUNT.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn arbitrary_input_schema_validation_never_panics_or_materializes() {
    SMOKE_MATERIALIZE_COUNT.store(0, Ordering::SeqCst);
    let connection = bound(2, &[]);
    for length in 0..=256 {
        let mut bytes = [0_u8; 256];
        for (index, byte) in bytes[..length].iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(53).wrapping_add(length as u8);
        }
        let _ = validate_body::<SmokeSchema>(&bytes[..length], &connection, BodyLimits::DESKTOP);
    }
    assert_eq!(SMOKE_MATERIALIZE_COUNT.load(Ordering::SeqCst), 0);
}
