use core::{marker::PhantomData, str};

use crate::{
    BodyError, BodyLimits, BoundProtocol, ENVELOPE_BYTES, ProtocolId, SLICE_REF_BYTES,
    TABLE_REF_BYTES, ValueEncoder,
};

const BODY_ALIGNMENT: usize = 8;
const CANONICAL_F32_NAN: u32 = 0x7fc0_0000;
const CANONICAL_F64_NAN: u64 = 0x7ff8_0000_0000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InlineLayout {
    pub bytes: u32,
    pub alignment: u8,
}

impl InlineLayout {
    pub const UNIT: Self = Self {
        bytes: 0,
        alignment: 1,
    };

    pub const SLICE: Self = Self {
        bytes: SLICE_REF_BYTES as u32,
        alignment: 4,
    };

    pub const TABLE: Self = Self {
        bytes: TABLE_REF_BYTES as u32,
        alignment: 4,
    };

    pub const ENVELOPE: Self = Self {
        bytes: ENVELOPE_BYTES as u32,
        alignment: 4,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimitiveKind {
    Bool,
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
    Id128,
}

impl PrimitiveKind {
    pub const fn layout(self) -> InlineLayout {
        match self {
            Self::Bool | Self::U8 | Self::I8 => InlineLayout {
                bytes: 1,
                alignment: 1,
            },
            Self::U16 | Self::I16 => InlineLayout {
                bytes: 2,
                alignment: 2,
            },
            Self::U32 | Self::I32 | Self::F32 => InlineLayout {
                bytes: 4,
                alignment: 4,
            },
            Self::U64 | Self::I64 | Self::F64 => InlineLayout {
                bytes: 8,
                alignment: 8,
            },
            Self::Id128 => InlineLayout {
                bytes: 16,
                alignment: 8,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegerRepr {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
}

impl IntegerRepr {
    pub const fn layout(self) -> InlineLayout {
        match self {
            Self::U8 | Self::I8 => InlineLayout {
                bytes: 1,
                alignment: 1,
            },
            Self::U16 | Self::I16 => InlineLayout {
                bytes: 2,
                alignment: 2,
            },
            Self::U32 | Self::I32 => InlineLayout {
                bytes: 4,
                alignment: 4,
            },
            Self::U64 | Self::I64 => InlineLayout {
                bytes: 8,
                alignment: 8,
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StructureFieldDescriptor {
    pub offset: u32,
    pub ty: &'static TypeDescriptor,
}

#[derive(Clone, Copy, Debug)]
pub struct StructureDescriptor {
    pub fields: &'static [StructureFieldDescriptor],
}

#[derive(Clone, Copy, Debug)]
pub struct VectorDescriptor {
    pub maximum_elements: u32,
    pub element: &'static TypeDescriptor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Availability {
    pub since_minor: u16,
    pub required_features: &'static [u32],
}

impl Availability {
    pub const ALWAYS: Self = Self {
        since_minor: 0,
        required_features: &[],
    };

    pub fn is_active(self, bound: &BoundProtocol<'_>) -> bool {
        self.since_minor <= bound.minor()
            && self
                .required_features
                .iter()
                .all(|feature| bound.supports_feature(*feature))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrdinalRange {
    pub first: u32,
    pub last: u32,
}

impl OrdinalRange {
    pub const fn contains(self, ordinal: u32) -> bool {
        ordinal >= self.first && ordinal <= self.last
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TableFieldDescriptor {
    pub ordinal: u32,
    pub ty: &'static TypeDescriptor,
    pub required: bool,
    pub availability: Availability,
}

#[derive(Clone, Copy, Debug)]
pub struct TableDescriptor {
    pub maximum_present_fields: u16,
    pub fields: &'static [TableFieldDescriptor],
    pub reserved_ordinals: &'static [OrdinalRange],
}

#[derive(Clone, Copy, Debug)]
pub struct ClosedEnumDescriptor {
    pub repr: IntegerRepr,
    pub values: &'static [u64],
}

#[derive(Clone, Copy, Debug)]
pub struct UnionAlternativeDescriptor {
    pub ordinal: u32,
    pub payload: Option<&'static TypeDescriptor>,
}

#[derive(Clone, Copy, Debug)]
pub struct ClosedUnionDescriptor {
    pub alternatives: &'static [UnionAlternativeDescriptor],
}

#[derive(Clone, Copy, Debug)]
pub enum TypeKind {
    Unit,
    Primitive(PrimitiveKind),
    Bytes { maximum: u32 },
    String { maximum_bytes: u32 },
    Structure(&'static StructureDescriptor),
    Vector(&'static VectorDescriptor),
    Optional { value: &'static TypeDescriptor },
    Table(&'static TableDescriptor),
    ClosedEnum(&'static ClosedEnumDescriptor),
    ClosedUnion(&'static ClosedUnionDescriptor),
}

#[derive(Clone, Copy, Debug)]
pub struct TypeDescriptor {
    pub layout: InlineLayout,
    pub kind: TypeKind,
}

impl TypeDescriptor {
    pub const UNIT: Self = Self {
        layout: InlineLayout::UNIT,
        kind: TypeKind::Unit,
    };

    pub const fn primitive(kind: PrimitiveKind) -> Self {
        Self {
            layout: kind.layout(),
            kind: TypeKind::Primitive(kind),
        }
    }

    pub const fn bytes(maximum: u32) -> Self {
        Self {
            layout: InlineLayout::SLICE,
            kind: TypeKind::Bytes { maximum },
        }
    }

    pub const fn string(maximum_bytes: u32) -> Self {
        Self {
            layout: InlineLayout::SLICE,
            kind: TypeKind::String { maximum_bytes },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ProtocolBodyDescriptor {
    pub protocol_id: ProtocolId,
    pub protocol_major: u16,
    pub root: &'static TypeDescriptor,
}

pub trait SchemaValueEncoder {
    fn closed_enum(
        &mut self,
        offset: usize,
        descriptor: &'static ClosedEnumDescriptor,
        raw_value: u64,
    ) -> Result<(), BodyError>;
}

impl SchemaValueEncoder for ValueEncoder<'_> {
    fn closed_enum(
        &mut self,
        offset: usize,
        descriptor: &'static ClosedEnumDescriptor,
        raw_value: u64,
    ) -> Result<(), BodyError> {
        validate_enum_descriptor(descriptor)?;
        if raw_value > repr_max(descriptor.repr)
            || descriptor.values.binary_search(&raw_value).is_err()
        {
            return Err(BodyError::UnknownEnumValue);
        }
        match descriptor.repr {
            IntegerRepr::U8 | IntegerRepr::I8 => self.write_u8(offset, raw_value as u8),
            IntegerRepr::U16 | IntegerRepr::I16 => self.write_u16(offset, raw_value as u16),
            IntegerRepr::U32 | IntegerRepr::I32 => self.write_u32(offset, raw_value as u32),
            IntegerRepr::U64 | IntegerRepr::I64 => self.write_u64(offset, raw_value),
        }
    }
}

pub trait WireSchema: Sized {
    type View<'wire>;

    const DESCRIPTOR: &'static ProtocolBodyDescriptor;

    fn materialize<'wire>(value: ValidatedValue<'wire>) -> Result<Self::View<'wire>, BodyError>;
}

pub struct ValidatedBody<'wire, T: WireSchema> {
    body: &'wire [u8],
    _schema: PhantomData<fn() -> T>,
}

impl<'wire, T: WireSchema> ValidatedBody<'wire, T> {
    pub const fn body(&self) -> &'wire [u8] {
        self.body
    }

    pub fn materialize(self) -> Result<T::View<'wire>, BodyError> {
        T::materialize(ValidatedValue {
            body: self.body,
            start: 0,
            descriptor: T::DESCRIPTOR.root,
        })
    }
}

#[derive(Clone, Copy)]
pub struct ValidatedValue<'wire> {
    body: &'wire [u8],
    start: usize,
    descriptor: &'static TypeDescriptor,
}

impl<'wire> ValidatedValue<'wire> {
    pub const fn descriptor(&self) -> &'static TypeDescriptor {
        self.descriptor
    }

    pub const fn body(&self) -> &'wire [u8] {
        self.body
    }

    pub const fn start(&self) -> usize {
        self.start
    }

    pub fn bool(&self) -> Result<bool, BodyError> {
        match self.raw_integer(PrimitiveKind::Bool)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(BodyError::InvalidBool),
        }
    }

    pub fn u32(&self) -> Result<u32, BodyError> {
        u32::try_from(self.raw_integer(PrimitiveKind::U32)?)
            .map_err(|_| BodyError::MaterializationMismatch)
    }

    pub fn u64(&self) -> Result<u64, BodyError> {
        self.raw_integer(PrimitiveKind::U64)
    }

    pub fn id128(&self) -> Result<[u8; 16], BodyError> {
        let TypeKind::Primitive(PrimitiveKind::Id128) = self.descriptor.kind else {
            return Err(BodyError::MaterializationMismatch);
        };
        Ok(copy_array(slice_at(self.body, self.start, 16)?))
    }

    pub fn string(&self) -> Result<&'wire str, BodyError> {
        let TypeKind::String { .. } = self.descriptor.kind else {
            return Err(BodyError::MaterializationMismatch);
        };
        let bytes = materialized_slice(self.body, self.start)?;
        str::from_utf8(bytes).map_err(|_| BodyError::InvalidUtf8)
    }

    pub fn bytes(&self) -> Result<&'wire [u8], BodyError> {
        let TypeKind::Bytes { .. } = self.descriptor.kind else {
            return Err(BodyError::MaterializationMismatch);
        };
        materialized_slice(self.body, self.start)
    }

    pub fn vector(&self) -> Result<ValidatedVector<'wire>, BodyError> {
        let TypeKind::Vector(descriptor) = self.descriptor.kind else {
            return Err(BodyError::MaterializationMismatch);
        };
        let reference = slice_at(self.body, self.start, SLICE_REF_BYTES)?;
        let count = get_u32(reference, 4);
        if count == 0 {
            return Ok(ValidatedVector {
                body: self.body,
                elements_start: 0,
                count: 0,
                stride: 0,
                element: descriptor.element,
            });
        }
        let target = self
            .start
            .checked_add(get_u32(reference, 0) as usize)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let stride = element_stride(descriptor.element)?;
        Ok(ValidatedVector {
            body: self.body,
            elements_start: target,
            count,
            stride,
            element: descriptor.element,
        })
    }

    pub fn structure_field(
        &self,
        index: usize,
    ) -> Result<Option<ValidatedValue<'wire>>, BodyError> {
        let TypeKind::Structure(descriptor) = self.descriptor.kind else {
            return Err(BodyError::MaterializationMismatch);
        };
        let Some(field) = descriptor.fields.get(index) else {
            return Ok(None);
        };
        let start = self
            .start
            .checked_add(field.offset as usize)
            .ok_or(BodyError::ArithmeticOverflow)?;
        Ok(Some(Self {
            body: self.body,
            start,
            descriptor: field.ty,
        }))
    }

    pub fn table_field(&self, ordinal: u32) -> Result<Option<ValidatedField<'wire>>, BodyError> {
        let TypeKind::Table(descriptor) = self.descriptor.kind else {
            return Err(BodyError::MaterializationMismatch);
        };
        let field = descriptor
            .fields
            .binary_search_by_key(&ordinal, |field| field.ordinal)
            .ok()
            .map(|index| &descriptor.fields[index]);
        let Some(field) = field else {
            return Ok(None);
        };
        let reference = slice_at(self.body, self.start, TABLE_REF_BYTES)?;
        let field_count = get_u16(reference, 4);
        if field_count == 0 {
            return Ok(None);
        }
        let table_start = self
            .start
            .checked_add(get_u32(reference, 0) as usize)
            .ok_or(BodyError::ArithmeticOverflow)?;
        for index in 0..field_count {
            let envelope_start = table_start
                .checked_add(usize::from(index) * ENVELOPE_BYTES)
                .ok_or(BodyError::ArithmeticOverflow)?;
            let envelope = parse_envelope(self.body, envelope_start)?;
            if envelope.ordinal == ordinal {
                return Ok(Some(ValidatedField {
                    ordinal,
                    value: envelope.payload.map(|payload| Self {
                        body: self.body,
                        start: payload.start,
                        descriptor: field.ty,
                    }),
                }));
            }
            if envelope.ordinal > ordinal {
                break;
            }
        }
        Ok(None)
    }

    pub fn enum_raw(&self) -> Result<u64, BodyError> {
        let TypeKind::ClosedEnum(descriptor) = self.descriptor.kind else {
            return Err(BodyError::MaterializationMismatch);
        };
        read_repr(self.body, self.start, descriptor.repr)
    }

    pub fn union(&self) -> Result<ValidatedUnion<'wire>, BodyError> {
        let TypeKind::ClosedUnion(descriptor) = self.descriptor.kind else {
            return Err(BodyError::MaterializationMismatch);
        };
        materialized_union(self.body, self.start, descriptor)
    }

    pub fn optional(&self) -> Result<Option<Self>, BodyError> {
        let TypeKind::Optional { value } = self.descriptor.kind else {
            return Err(BodyError::MaterializationMismatch);
        };
        let envelope = parse_envelope(self.body, self.start)?;
        match envelope.ordinal {
            0 => Ok(None),
            1 => Ok(Some(Self {
                body: self.body,
                start: envelope.payload.map_or(0, |payload| payload.start),
                descriptor: value,
            })),
            _ => Err(BodyError::InvalidOptionalOrdinal),
        }
    }

    fn raw_integer(&self, expected: PrimitiveKind) -> Result<u64, BodyError> {
        let TypeKind::Primitive(actual) = self.descriptor.kind else {
            return Err(BodyError::MaterializationMismatch);
        };
        if actual != expected {
            return Err(BodyError::MaterializationMismatch);
        }
        read_primitive_raw(self.body, self.start, actual)
    }
}

pub struct ValidatedField<'wire> {
    ordinal: u32,
    value: Option<ValidatedValue<'wire>>,
}

impl<'wire> ValidatedField<'wire> {
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub const fn is_unit(&self) -> bool {
        self.value.is_none()
    }

    pub const fn value(&self) -> Option<ValidatedValue<'wire>> {
        self.value
    }
}

pub struct ValidatedVector<'wire> {
    body: &'wire [u8],
    elements_start: usize,
    count: u32,
    stride: usize,
    element: &'static TypeDescriptor,
}

impl<'wire> ValidatedVector<'wire> {
    pub const fn len(&self) -> u32 {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn get(&self, index: u32) -> Option<ValidatedValue<'wire>> {
        if index >= self.count {
            return None;
        }
        let offset = usize::try_from(index).ok()?.checked_mul(self.stride)?;
        let start = self.elements_start.checked_add(offset)?;
        Some(ValidatedValue {
            body: self.body,
            start,
            descriptor: self.element,
        })
    }
}

pub struct ValidatedUnion<'wire> {
    ordinal: u32,
    payload: Option<ValidatedValue<'wire>>,
}

impl<'wire> ValidatedUnion<'wire> {
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub const fn is_unit(&self) -> bool {
        self.payload.is_none()
    }

    pub const fn payload(&self) -> Option<ValidatedValue<'wire>> {
        self.payload
    }
}

pub fn validate_body<'wire, T: WireSchema>(
    body: &'wire [u8],
    bound: &BoundProtocol<'_>,
    limits: BodyLimits,
) -> Result<ValidatedBody<'wire, T>, BodyError> {
    let schema = T::DESCRIPTOR;
    if schema.protocol_id != bound.protocol_id() || schema.protocol_major != bound.major() {
        return Err(BodyError::ProtocolMismatch);
    }
    if body.len() > bound.limits().max_body_bytes as usize {
        return Err(BodyError::LimitExceeded);
    }
    if !body.len().is_multiple_of(BODY_ALIGNMENT) {
        return Err(BodyError::Misaligned);
    }
    if limits.max_depth == 0 {
        return Err(BodyError::LimitExceeded);
    }
    validate_descriptor_graph(schema.root)?;
    let context = SchemaContext {
        body,
        bound,
        limits,
    };
    validate_allocation(schema.root, 0, body.len(), 1, &context)?;
    Ok(ValidatedBody {
        body,
        _schema: PhantomData,
    })
}

struct SchemaContext<'wire, 'bound, 'features> {
    body: &'wire [u8],
    bound: &'bound BoundProtocol<'features>,
    limits: BodyLimits,
}

#[derive(Clone, Copy)]
struct Cursor {
    next: usize,
    end: usize,
}

#[derive(Clone, Copy)]
struct PayloadRange {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy)]
struct Envelope {
    ordinal: u32,
    payload: Option<PayloadRange>,
}

fn validate_allocation(
    descriptor: &'static TypeDescriptor,
    start: usize,
    end: usize,
    depth: u8,
    context: &SchemaContext<'_, '_, '_>,
) -> Result<(), BodyError> {
    check_depth(depth, context.limits)?;
    if !start.is_multiple_of(BODY_ALIGNMENT) || !end.is_multiple_of(BODY_ALIGNMENT) {
        return Err(BodyError::Misaligned);
    }
    let inline_bytes = descriptor_inline_bytes(descriptor)?;
    let inline_end = start
        .checked_add(inline_bytes)
        .ok_or(BodyError::ArithmeticOverflow)?;
    if inline_end > end || end > context.body.len() {
        return Err(BodyError::Truncated);
    }
    let children_start = align_up(inline_end, BODY_ALIGNMENT)?;
    if children_start > end {
        return Err(BodyError::Truncated);
    }
    require_zero(context.body, inline_end, children_start)?;
    let mut cursor = Cursor {
        next: children_start,
        end,
    };
    validate_value(descriptor, start, &mut cursor, depth, context)?;
    if cursor.next != end {
        return Err(BodyError::TrailingBytes);
    }
    Ok(())
}

fn validate_value(
    descriptor: &'static TypeDescriptor,
    start: usize,
    cursor: &mut Cursor,
    depth: u8,
    context: &SchemaContext<'_, '_, '_>,
) -> Result<(), BodyError> {
    check_depth(depth, context.limits)?;
    validate_layout(descriptor.layout)?;
    let inline_bytes = descriptor_inline_bytes(descriptor)?;
    let inline_end = start
        .checked_add(inline_bytes)
        .ok_or(BodyError::ArithmeticOverflow)?;
    if inline_end > context.body.len() {
        return Err(BodyError::Truncated);
    }
    match descriptor.kind {
        TypeKind::Unit => require_layout(descriptor.layout, InlineLayout::UNIT),
        TypeKind::Primitive(kind) => {
            require_layout(descriptor.layout, kind.layout())?;
            validate_primitive(context.body, start, kind)
        }
        TypeKind::Bytes { maximum } => {
            require_layout(descriptor.layout, InlineLayout::SLICE)?;
            validate_bytes_like(start, maximum, false, cursor, context)
        }
        TypeKind::String { maximum_bytes } => {
            require_layout(descriptor.layout, InlineLayout::SLICE)?;
            validate_bytes_like(start, maximum_bytes, true, cursor, context)
        }
        TypeKind::Structure(structure) => {
            validate_structure(descriptor, structure, start, cursor, depth, context)
        }
        TypeKind::Vector(vector) => {
            require_layout(descriptor.layout, InlineLayout::SLICE)?;
            validate_vector(start, vector, cursor, depth, context)
        }
        TypeKind::Optional { value } => {
            require_layout(descriptor.layout, InlineLayout::ENVELOPE)?;
            validate_optional(start, value, cursor, depth, context)
        }
        TypeKind::Table(table) => {
            require_layout(descriptor.layout, InlineLayout::TABLE)?;
            validate_table(start, table, cursor, depth, context)
        }
        TypeKind::ClosedEnum(enumeration) => {
            validate_closed_enum(descriptor, enumeration, start, context)
        }
        TypeKind::ClosedUnion(union) => {
            require_layout(descriptor.layout, InlineLayout::ENVELOPE)?;
            validate_closed_union(start, union, cursor, depth, context)
        }
    }
}

fn validate_structure(
    descriptor: &'static TypeDescriptor,
    structure: &'static StructureDescriptor,
    start: usize,
    cursor: &mut Cursor,
    depth: u8,
    context: &SchemaContext<'_, '_, '_>,
) -> Result<(), BodyError> {
    if descriptor.layout.bytes == 0 {
        return Err(BodyError::InvalidDescriptor);
    }
    let structure_bytes = descriptor.layout.bytes as usize;
    let mut previous_end = 0_usize;
    for field in structure.fields {
        let field_offset = field.offset as usize;
        let field_bytes = descriptor_inline_bytes(field.ty)?;
        let field_alignment = usize::from(field.ty.layout.alignment);
        if field_offset < previous_end || !field_offset.is_multiple_of(field_alignment) {
            return Err(BodyError::InvalidDescriptor);
        }
        let field_end = field_offset
            .checked_add(field_bytes)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if field_end > structure_bytes {
            return Err(BodyError::InvalidDescriptor);
        }
        require_zero(
            context.body,
            start
                .checked_add(previous_end)
                .ok_or(BodyError::ArithmeticOverflow)?,
            start
                .checked_add(field_offset)
                .ok_or(BodyError::ArithmeticOverflow)?,
        )?;
        validate_value(
            field.ty,
            start
                .checked_add(field_offset)
                .ok_or(BodyError::ArithmeticOverflow)?,
            cursor,
            next_depth(depth, context.limits)?,
            context,
        )?;
        previous_end = field_end;
    }
    require_zero(
        context.body,
        start
            .checked_add(previous_end)
            .ok_or(BodyError::ArithmeticOverflow)?,
        start
            .checked_add(structure_bytes)
            .ok_or(BodyError::ArithmeticOverflow)?,
    )
}

fn validate_bytes_like(
    reference_start: usize,
    maximum: u32,
    utf8: bool,
    cursor: &mut Cursor,
    context: &SchemaContext<'_, '_, '_>,
) -> Result<(), BodyError> {
    let reference = slice_at(context.body, reference_start, SLICE_REF_BYTES)?;
    let relative_offset = get_u32(reference, 0) as usize;
    let count = get_u32(reference, 4);
    let region_bytes = get_u32(reference, 8) as usize;
    if get_u32(reference, 12) != 0 {
        return Err(BodyError::ReservedValueUsed);
    }
    if count > maximum {
        return Err(BodyError::LimitExceeded);
    }
    if count == 0 {
        if relative_offset != 0 || region_bytes != 0 {
            return Err(BodyError::InvalidReference);
        }
        return Ok(());
    }
    let count = count as usize;
    let expected_region = align_up(count, BODY_ALIGNMENT)?;
    if relative_offset == 0 || region_bytes != expected_region {
        return Err(BodyError::RegionLengthMismatch);
    }
    let target = reference_start
        .checked_add(relative_offset)
        .ok_or(BodyError::ArithmeticOverflow)?;
    require_target(target, cursor.next)?;
    let data_end = target
        .checked_add(count)
        .ok_or(BodyError::ArithmeticOverflow)?;
    let region_end = target
        .checked_add(region_bytes)
        .ok_or(BodyError::ArithmeticOverflow)?;
    if region_end > cursor.end {
        return Err(BodyError::OutOfBounds);
    }
    require_zero(context.body, data_end, region_end)?;
    if utf8 {
        str::from_utf8(slice_at(context.body, target, count)?)
            .map_err(|_| BodyError::InvalidUtf8)?;
    }
    cursor.next = region_end;
    Ok(())
}

fn validate_vector(
    reference_start: usize,
    vector: &'static VectorDescriptor,
    parent: &mut Cursor,
    depth: u8,
    context: &SchemaContext<'_, '_, '_>,
) -> Result<(), BodyError> {
    let reference = slice_at(context.body, reference_start, SLICE_REF_BYTES)?;
    let relative_offset = get_u32(reference, 0) as usize;
    let count = get_u32(reference, 4);
    let region_bytes = get_u32(reference, 8) as usize;
    if get_u32(reference, 12) != 0 {
        return Err(BodyError::ReservedValueUsed);
    }
    if count > vector.maximum_elements {
        return Err(BodyError::LimitExceeded);
    }
    let stride = element_stride(vector.element)?;
    if count == 0 {
        if relative_offset != 0 || region_bytes != 0 {
            return Err(BodyError::InvalidReference);
        }
        return Ok(());
    }
    if relative_offset == 0 || region_bytes == 0 || !region_bytes.is_multiple_of(BODY_ALIGNMENT) {
        return Err(BodyError::InvalidReference);
    }
    let target = reference_start
        .checked_add(relative_offset)
        .ok_or(BodyError::ArithmeticOverflow)?;
    require_target(target, parent.next)?;
    let region_end = target
        .checked_add(region_bytes)
        .ok_or(BodyError::ArithmeticOverflow)?;
    if region_end > parent.end {
        return Err(BodyError::OutOfBounds);
    }
    let count_usize = count as usize;
    let array_bytes = count_usize
        .checked_mul(stride)
        .ok_or(BodyError::ArithmeticOverflow)?;
    let array_end = target
        .checked_add(array_bytes)
        .ok_or(BodyError::ArithmeticOverflow)?;
    let children_start = align_up(array_end, BODY_ALIGNMENT)?;
    if children_start > region_end {
        return Err(BodyError::Truncated);
    }
    let element_bytes = descriptor_inline_bytes(vector.element)?;
    for index in 0..count_usize {
        let element_start = target
            .checked_add(
                index
                    .checked_mul(stride)
                    .ok_or(BodyError::ArithmeticOverflow)?,
            )
            .ok_or(BodyError::ArithmeticOverflow)?;
        require_zero(
            context.body,
            element_start
                .checked_add(element_bytes)
                .ok_or(BodyError::ArithmeticOverflow)?,
            element_start
                .checked_add(stride)
                .ok_or(BodyError::ArithmeticOverflow)?,
        )?;
    }
    require_zero(context.body, array_end, children_start)?;
    let mut vector_cursor = Cursor {
        next: children_start,
        end: region_end,
    };
    for index in 0..count_usize {
        let element_start = target
            .checked_add(
                index
                    .checked_mul(stride)
                    .ok_or(BodyError::ArithmeticOverflow)?,
            )
            .ok_or(BodyError::ArithmeticOverflow)?;
        validate_value(
            vector.element,
            element_start,
            &mut vector_cursor,
            next_depth(depth, context.limits)?,
            context,
        )?;
    }
    if vector_cursor.next != region_end {
        return Err(BodyError::RegionLengthMismatch);
    }
    parent.next = region_end;
    Ok(())
}

fn validate_optional(
    envelope_start: usize,
    value: &'static TypeDescriptor,
    cursor: &mut Cursor,
    depth: u8,
    context: &SchemaContext<'_, '_, '_>,
) -> Result<(), BodyError> {
    let envelope_bytes = slice_at(context.body, envelope_start, ENVELOPE_BYTES)?;
    if envelope_bytes.iter().all(|byte| *byte == 0) {
        return Ok(());
    }
    let envelope = parse_envelope(context.body, envelope_start)?;
    if envelope.ordinal != 1 {
        return Err(BodyError::InvalidOptionalOrdinal);
    }
    validate_selected_payload(value, envelope.payload, cursor, depth, context)
}

fn validate_closed_union(
    envelope_start: usize,
    union: &'static ClosedUnionDescriptor,
    cursor: &mut Cursor,
    depth: u8,
    context: &SchemaContext<'_, '_, '_>,
) -> Result<(), BodyError> {
    let envelope = parse_envelope(context.body, envelope_start)?;
    if envelope.ordinal == 0 {
        return Err(BodyError::UnknownUnionOrdinal);
    }
    let alternative = union
        .alternatives
        .binary_search_by_key(&envelope.ordinal, |alternative| alternative.ordinal)
        .ok()
        .map(|index| &union.alternatives[index])
        .ok_or(BodyError::UnknownUnionOrdinal)?;
    match alternative.payload {
        Some(payload) => {
            validate_selected_payload(payload, envelope.payload, cursor, depth, context)
        }
        None => {
            next_depth(depth, context.limits)?;
            if envelope.payload.is_some() {
                return Err(BodyError::UnexpectedPayload);
            }
            Ok(())
        }
    }
}

fn validate_selected_payload(
    descriptor: &'static TypeDescriptor,
    payload: Option<PayloadRange>,
    cursor: &mut Cursor,
    depth: u8,
    context: &SchemaContext<'_, '_, '_>,
) -> Result<(), BodyError> {
    let child_depth = next_depth(depth, context.limits)?;
    if matches!(descriptor.kind, TypeKind::Unit) {
        require_layout(descriptor.layout, InlineLayout::UNIT)?;
        if payload.is_some() {
            return Err(BodyError::UnexpectedPayload);
        }
        return Ok(());
    }
    let payload = payload.ok_or(BodyError::MissingPayload)?;
    require_target(payload.start, cursor.next)?;
    validate_allocation(descriptor, payload.start, payload.end, child_depth, context)?;
    cursor.next = payload.end;
    Ok(())
}

fn validate_closed_enum(
    descriptor: &'static TypeDescriptor,
    enumeration: &'static ClosedEnumDescriptor,
    start: usize,
    context: &SchemaContext<'_, '_, '_>,
) -> Result<(), BodyError> {
    require_layout(descriptor.layout, enumeration.repr.layout())?;
    validate_enum_descriptor(enumeration)?;
    let value = read_repr(context.body, start, enumeration.repr)?;
    if enumeration.values.binary_search(&value).is_err() {
        return Err(BodyError::UnknownEnumValue);
    }
    Ok(())
}

fn validate_table(
    reference_start: usize,
    table: &'static TableDescriptor,
    parent: &mut Cursor,
    depth: u8,
    context: &SchemaContext<'_, '_, '_>,
) -> Result<(), BodyError> {
    let reference = slice_at(context.body, reference_start, TABLE_REF_BYTES)?;
    let relative_offset = get_u32(reference, 0) as usize;
    let field_count = get_u16(reference, 4);
    let region_bytes = get_u32(reference, 8) as usize;
    if get_u16(reference, 6) != 0 || get_u32(reference, 12) != 0 {
        return Err(BodyError::ReservedValueUsed);
    }
    if field_count > table.maximum_present_fields || field_count > context.limits.max_table_fields {
        return Err(BodyError::LimitExceeded);
    }
    if field_count == 0 {
        if relative_offset != 0 || region_bytes != 0 {
            return Err(BodyError::InvalidReference);
        }
        for field in table.fields {
            require_field_if_active(field, context.bound)?;
        }
        return Ok(());
    }
    if relative_offset == 0 || region_bytes == 0 || !region_bytes.is_multiple_of(BODY_ALIGNMENT) {
        return Err(BodyError::InvalidReference);
    }
    let table_start = reference_start
        .checked_add(relative_offset)
        .ok_or(BodyError::ArithmeticOverflow)?;
    require_target(table_start, parent.next)?;
    let table_end = table_start
        .checked_add(region_bytes)
        .ok_or(BodyError::ArithmeticOverflow)?;
    if table_end > parent.end {
        return Err(BodyError::OutOfBounds);
    }
    let envelope_bytes = usize::from(field_count)
        .checked_mul(ENVELOPE_BYTES)
        .ok_or(BodyError::ArithmeticOverflow)?;
    let payload_start = table_start
        .checked_add(envelope_bytes)
        .ok_or(BodyError::ArithmeticOverflow)?;
    if payload_start > table_end {
        return Err(BodyError::Truncated);
    }
    prevalidate_table_envelopes(
        context.body,
        table,
        TableRegion {
            start: table_start,
            field_count,
            payload_start,
            end: table_end,
            depth,
        },
        context.limits,
    )?;

    let mut payload_cursor = payload_start;
    let mut descriptor_index = 0_usize;
    for wire_index in 0..field_count {
        let envelope_start = table_start
            .checked_add(
                usize::from(wire_index)
                    .checked_mul(ENVELOPE_BYTES)
                    .ok_or(BodyError::ArithmeticOverflow)?,
            )
            .ok_or(BodyError::ArithmeticOverflow)?;
        let envelope = parse_envelope(context.body, envelope_start)?;
        while descriptor_index < table.fields.len()
            && table.fields[descriptor_index].ordinal < envelope.ordinal
        {
            require_field_if_active(&table.fields[descriptor_index], context.bound)?;
            descriptor_index += 1;
        }
        if descriptor_index < table.fields.len()
            && table.fields[descriptor_index].ordinal == envelope.ordinal
        {
            let field = &table.fields[descriptor_index];
            if !field.availability.is_active(context.bound) {
                return Err(BodyError::FieldUnavailable);
            }
            validate_selected_payload(
                field.ty,
                envelope.payload,
                &mut Cursor {
                    next: payload_cursor,
                    end: table_end,
                },
                depth,
                context,
            )?;
            if let Some(payload) = envelope.payload {
                payload_cursor = payload.end;
            }
            descriptor_index += 1;
        } else if let Some(payload) = envelope.payload {
            payload_cursor = payload.end;
        }
    }
    while descriptor_index < table.fields.len() {
        require_field_if_active(&table.fields[descriptor_index], context.bound)?;
        descriptor_index += 1;
    }
    if payload_cursor != table_end {
        return Err(BodyError::RegionLengthMismatch);
    }
    parent.next = table_end;
    Ok(())
}

#[derive(Clone, Copy)]
struct TableRegion {
    start: usize,
    field_count: u16,
    payload_start: usize,
    end: usize,
    depth: u8,
}

fn prevalidate_table_envelopes(
    body: &[u8],
    descriptor: &'static TableDescriptor,
    region: TableRegion,
    limits: BodyLimits,
) -> Result<(), BodyError> {
    let mut payload_cursor = region.payload_start;
    let mut previous = None;
    for index in 0..region.field_count {
        let envelope_start = region
            .start
            .checked_add(
                usize::from(index)
                    .checked_mul(ENVELOPE_BYTES)
                    .ok_or(BodyError::ArithmeticOverflow)?,
            )
            .ok_or(BodyError::ArithmeticOverflow)?;
        let envelope = parse_envelope(body, envelope_start)?;
        if envelope.ordinal == 0 {
            return Err(BodyError::InvalidOrdinal);
        }
        if previous == Some(envelope.ordinal) {
            return Err(BodyError::DuplicateOrdinal);
        }
        if previous.is_some_and(|previous| envelope.ordinal < previous) {
            return Err(BodyError::EnvelopeOrder);
        }
        if reserved_ordinal(descriptor.reserved_ordinals, envelope.ordinal) {
            return Err(BodyError::ReservedOrdinal);
        }
        next_depth(region.depth, limits)?;
        if let Some(payload) = envelope.payload {
            require_target(payload.start, payload_cursor)?;
            if payload.end > region.end {
                return Err(BodyError::OutOfBounds);
            }
            payload_cursor = payload.end;
        }
        previous = Some(envelope.ordinal);
    }
    if payload_cursor != region.end {
        return Err(BodyError::RegionLengthMismatch);
    }
    Ok(())
}

fn validate_table_descriptor(table: &'static TableDescriptor) -> Result<(), BodyError> {
    if table.maximum_present_fields == 0
        || usize::from(table.maximum_present_fields) < table.fields.len()
    {
        return Err(BodyError::InvalidDescriptor);
    }
    let mut previous = None;
    for field in table.fields {
        if field.ordinal == 0 || previous.is_some_and(|previous| field.ordinal <= previous) {
            return Err(BodyError::InvalidDescriptor);
        }
        if reserved_ordinal(table.reserved_ordinals, field.ordinal) {
            return Err(BodyError::InvalidDescriptor);
        }
        validate_feature_ids(field.availability.required_features)?;
        validate_layout(field.ty.layout)?;
        previous = Some(field.ordinal);
    }
    let mut previous_end = None;
    for range in table.reserved_ordinals {
        if range.first == 0
            || range.first > range.last
            || previous_end.is_some_and(|previous| range.first <= previous)
        {
            return Err(BodyError::InvalidDescriptor);
        }
        previous_end = Some(range.last);
    }
    Ok(())
}

fn validate_enum_descriptor(enumeration: &'static ClosedEnumDescriptor) -> Result<(), BodyError> {
    if enumeration.values.is_empty() {
        return Err(BodyError::InvalidDescriptor);
    }
    let maximum = repr_max(enumeration.repr);
    let mut previous = None;
    for value in enumeration.values {
        if *value > maximum || previous.is_some_and(|previous| *value <= previous) {
            return Err(BodyError::InvalidDescriptor);
        }
        previous = Some(*value);
    }
    Ok(())
}

fn validate_union_descriptor(union: &'static ClosedUnionDescriptor) -> Result<(), BodyError> {
    if union.alternatives.is_empty() {
        return Err(BodyError::InvalidDescriptor);
    }
    let mut previous = None;
    for alternative in union.alternatives {
        if alternative.ordinal == 0
            || previous.is_some_and(|previous| alternative.ordinal <= previous)
        {
            return Err(BodyError::InvalidDescriptor);
        }
        if let Some(payload) = alternative.payload {
            if matches!(payload.kind, TypeKind::Unit) || payload.layout.bytes == 0 {
                return Err(BodyError::InvalidDescriptor);
            }
            validate_layout(payload.layout)?;
        }
        previous = Some(alternative.ordinal);
    }
    Ok(())
}

fn validate_feature_ids(features: &[u32]) -> Result<(), BodyError> {
    let mut previous = None;
    for feature in features {
        if *feature == 0 || previous.is_some_and(|previous| *feature <= previous) {
            return Err(BodyError::InvalidDescriptor);
        }
        previous = Some(*feature);
    }
    Ok(())
}

fn require_field_if_active(
    field: &TableFieldDescriptor,
    bound: &BoundProtocol<'_>,
) -> Result<(), BodyError> {
    if field.required && field.availability.is_active(bound) {
        return Err(BodyError::MissingRequiredField);
    }
    Ok(())
}

fn require_layout(actual: InlineLayout, expected: InlineLayout) -> Result<(), BodyError> {
    if actual != expected {
        return Err(BodyError::InvalidDescriptor);
    }
    Ok(())
}

fn validate_layout(layout: InlineLayout) -> Result<(), BodyError> {
    let alignment = usize::from(layout.alignment);
    if !matches!(alignment, 1 | 2 | 4 | 8) || !(layout.bytes as usize).is_multiple_of(alignment) {
        return Err(BodyError::InvalidDescriptor);
    }
    Ok(())
}

fn descriptor_inline_bytes(descriptor: &'static TypeDescriptor) -> Result<usize, BodyError> {
    validate_layout(descriptor.layout)?;
    if descriptor.layout.bytes == 0 && !matches!(descriptor.kind, TypeKind::Unit) {
        return Err(BodyError::InvalidDescriptor);
    }
    Ok(descriptor.layout.bytes as usize)
}

fn element_stride(descriptor: &'static TypeDescriptor) -> Result<usize, BodyError> {
    let bytes = descriptor_inline_bytes(descriptor)?;
    if bytes == 0 {
        return Err(BodyError::InvalidElementLayout);
    }
    align_up(bytes, usize::from(descriptor.layout.alignment))
}

fn validate_primitive(body: &[u8], start: usize, kind: PrimitiveKind) -> Result<(), BodyError> {
    if kind == PrimitiveKind::Id128 {
        slice_at(body, start, 16)?;
        return Ok(());
    }
    let raw = read_primitive_raw(body, start, kind)?;
    match kind {
        PrimitiveKind::Bool if raw > 1 => Err(BodyError::InvalidBool),
        PrimitiveKind::F32
            if f32::from_bits(raw as u32).is_nan() && raw as u32 != CANONICAL_F32_NAN =>
        {
            Err(BodyError::NonCanonicalFloat)
        }
        PrimitiveKind::F64 if f64::from_bits(raw).is_nan() && raw != CANONICAL_F64_NAN => {
            Err(BodyError::NonCanonicalFloat)
        }
        _ => Ok(()),
    }
}

fn read_primitive_raw(body: &[u8], start: usize, kind: PrimitiveKind) -> Result<u64, BodyError> {
    Ok(match kind {
        PrimitiveKind::Bool | PrimitiveKind::U8 | PrimitiveKind::I8 => {
            *body.get(start).ok_or(BodyError::Truncated)? as u64
        }
        PrimitiveKind::U16 | PrimitiveKind::I16 => get_u16(slice_at(body, start, 2)?, 0) as u64,
        PrimitiveKind::U32 | PrimitiveKind::I32 | PrimitiveKind::F32 => {
            get_u32(slice_at(body, start, 4)?, 0) as u64
        }
        PrimitiveKind::U64 | PrimitiveKind::I64 | PrimitiveKind::F64 => {
            get_u64(slice_at(body, start, 8)?, 0)
        }
        PrimitiveKind::Id128 => return Err(BodyError::MaterializationMismatch),
    })
}

fn read_repr(body: &[u8], start: usize, repr: IntegerRepr) -> Result<u64, BodyError> {
    Ok(match repr {
        IntegerRepr::U8 | IntegerRepr::I8 => *body.get(start).ok_or(BodyError::Truncated)? as u64,
        IntegerRepr::U16 | IntegerRepr::I16 => get_u16(slice_at(body, start, 2)?, 0) as u64,
        IntegerRepr::U32 | IntegerRepr::I32 => get_u32(slice_at(body, start, 4)?, 0) as u64,
        IntegerRepr::U64 | IntegerRepr::I64 => get_u64(slice_at(body, start, 8)?, 0),
    })
}

const fn repr_max(repr: IntegerRepr) -> u64 {
    match repr {
        IntegerRepr::U8 | IntegerRepr::I8 => u8::MAX as u64,
        IntegerRepr::U16 | IntegerRepr::I16 => u16::MAX as u64,
        IntegerRepr::U32 | IntegerRepr::I32 => u32::MAX as u64,
        IntegerRepr::U64 | IntegerRepr::I64 => u64::MAX,
    }
}

fn check_depth(depth: u8, limits: BodyLimits) -> Result<(), BodyError> {
    if depth == 0 || depth > limits.max_depth {
        return Err(BodyError::LimitExceeded);
    }
    Ok(())
}

fn next_depth(depth: u8, limits: BodyLimits) -> Result<u8, BodyError> {
    let next = depth.checked_add(1).ok_or(BodyError::LimitExceeded)?;
    check_depth(next, limits)?;
    Ok(next)
}

fn reserved_ordinal(ranges: &[OrdinalRange], ordinal: u32) -> bool {
    ranges
        .binary_search_by(|range| {
            if ordinal < range.first {
                core::cmp::Ordering::Greater
            } else if ordinal > range.last {
                core::cmp::Ordering::Less
            } else {
                core::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

const MAX_DESCRIPTOR_NESTING: usize = 64;
const MAX_DESCRIPTOR_VISITS: u32 = 65_536;

struct DescriptorValidation {
    path: [usize; MAX_DESCRIPTOR_NESTING],
    depth: usize,
    remaining_visits: u32,
}

impl DescriptorValidation {
    fn enter(&mut self, descriptor: &'static TypeDescriptor) -> Result<(), BodyError> {
        if self.depth == MAX_DESCRIPTOR_NESTING || self.remaining_visits == 0 {
            return Err(BodyError::InvalidDescriptor);
        }
        let identity = descriptor as *const TypeDescriptor as usize;
        if self.path[..self.depth].contains(&identity) {
            return Err(BodyError::InvalidDescriptor);
        }
        self.path[self.depth] = identity;
        self.depth += 1;
        self.remaining_visits -= 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
        self.path[self.depth] = 0;
    }
}

fn validate_descriptor_graph(root: &'static TypeDescriptor) -> Result<(), BodyError> {
    let mut state = DescriptorValidation {
        path: [0; MAX_DESCRIPTOR_NESTING],
        depth: 0,
        remaining_visits: MAX_DESCRIPTOR_VISITS,
    };
    validate_type_descriptor(root, &mut state)
}

fn validate_type_descriptor(
    descriptor: &'static TypeDescriptor,
    state: &mut DescriptorValidation,
) -> Result<(), BodyError> {
    state.enter(descriptor)?;
    let result = (|| {
        validate_layout(descriptor.layout)?;
        match descriptor.kind {
            TypeKind::Unit => require_layout(descriptor.layout, InlineLayout::UNIT),
            TypeKind::Primitive(kind) => require_layout(descriptor.layout, kind.layout()),
            TypeKind::Bytes { .. } | TypeKind::String { .. } => {
                require_layout(descriptor.layout, InlineLayout::SLICE)
            }
            TypeKind::Structure(structure) => {
                if descriptor.layout.bytes == 0 || structure.fields.is_empty() {
                    return Err(BodyError::InvalidDescriptor);
                }
                let mut previous_end = 0_usize;
                let mut greatest_alignment = 1_u8;
                for field in structure.fields {
                    greatest_alignment = greatest_alignment.max(field.ty.layout.alignment);
                    let field_offset = field.offset as usize;
                    let field_bytes = descriptor_inline_bytes(field.ty)?;
                    if field_offset < previous_end
                        || !field_offset.is_multiple_of(usize::from(field.ty.layout.alignment))
                    {
                        return Err(BodyError::InvalidDescriptor);
                    }
                    previous_end = field_offset
                        .checked_add(field_bytes)
                        .ok_or(BodyError::ArithmeticOverflow)?;
                    if previous_end > descriptor.layout.bytes as usize {
                        return Err(BodyError::InvalidDescriptor);
                    }
                    validate_type_descriptor(field.ty, state)?;
                }
                if descriptor.layout.alignment != greatest_alignment {
                    return Err(BodyError::InvalidDescriptor);
                }
                Ok(())
            }
            TypeKind::Vector(vector) => {
                require_layout(descriptor.layout, InlineLayout::SLICE)?;
                if vector.maximum_elements == 0 {
                    return Err(BodyError::InvalidDescriptor);
                }
                element_stride(vector.element)?;
                validate_type_descriptor(vector.element, state)
            }
            TypeKind::Optional { value } => {
                require_layout(descriptor.layout, InlineLayout::ENVELOPE)?;
                validate_type_descriptor(value, state)
            }
            TypeKind::Table(table) => {
                require_layout(descriptor.layout, InlineLayout::TABLE)?;
                validate_table_descriptor(table)?;
                for field in table.fields {
                    validate_type_descriptor(field.ty, state)?;
                }
                Ok(())
            }
            TypeKind::ClosedEnum(enumeration) => {
                require_layout(descriptor.layout, enumeration.repr.layout())?;
                validate_enum_descriptor(enumeration)
            }
            TypeKind::ClosedUnion(union) => {
                require_layout(descriptor.layout, InlineLayout::ENVELOPE)?;
                validate_union_descriptor(union)?;
                for alternative in union.alternatives {
                    if let Some(payload) = alternative.payload {
                        validate_type_descriptor(payload, state)?;
                    }
                }
                Ok(())
            }
        }
    })();
    state.leave();
    result
}

fn parse_envelope(body: &[u8], start: usize) -> Result<Envelope, BodyError> {
    let bytes = slice_at(body, start, ENVELOPE_BYTES)?;
    if get_u16(bytes, 4) != 0
        || get_u16(bytes, 6) != 0
        || get_u16(bytes, 16) != 0
        || get_u16(bytes, 18) != 0
        || get_u32(bytes, 20) != 0
    {
        return Err(BodyError::ReservedValueUsed);
    }
    let ordinal = get_u32(bytes, 0);
    let relative_offset = get_u32(bytes, 8) as usize;
    let payload_bytes = get_u32(bytes, 12) as usize;
    if relative_offset == 0 && payload_bytes == 0 {
        return Ok(Envelope {
            ordinal,
            payload: None,
        });
    }
    if relative_offset == 0 || payload_bytes == 0 || !payload_bytes.is_multiple_of(BODY_ALIGNMENT) {
        return Err(BodyError::InvalidReference);
    }
    let offset_field = start.checked_add(8).ok_or(BodyError::ArithmeticOverflow)?;
    let payload_start = offset_field
        .checked_add(relative_offset)
        .ok_or(BodyError::ArithmeticOverflow)?;
    if !payload_start.is_multiple_of(BODY_ALIGNMENT) {
        return Err(BodyError::Misaligned);
    }
    let payload_end = payload_start
        .checked_add(payload_bytes)
        .ok_or(BodyError::ArithmeticOverflow)?;
    if payload_end > body.len() {
        return Err(BodyError::OutOfBounds);
    }
    Ok(Envelope {
        ordinal,
        payload: Some(PayloadRange {
            start: payload_start,
            end: payload_end,
        }),
    })
}

fn materialized_slice(body: &[u8], reference_start: usize) -> Result<&[u8], BodyError> {
    let reference = slice_at(body, reference_start, SLICE_REF_BYTES)?;
    let count = get_u32(reference, 4) as usize;
    if count == 0 {
        return Ok(&body[0..0]);
    }
    let target = reference_start
        .checked_add(get_u32(reference, 0) as usize)
        .ok_or(BodyError::ArithmeticOverflow)?;
    slice_at(body, target, count)
}

fn materialized_union<'wire>(
    body: &'wire [u8],
    start: usize,
    descriptor: &'static ClosedUnionDescriptor,
) -> Result<ValidatedUnion<'wire>, BodyError> {
    let envelope = parse_envelope(body, start)?;
    let alternative = descriptor
        .alternatives
        .binary_search_by_key(&envelope.ordinal, |alternative| alternative.ordinal)
        .ok()
        .map(|index| &descriptor.alternatives[index])
        .ok_or(BodyError::UnknownUnionOrdinal)?;
    let payload = match (alternative.payload, envelope.payload) {
        (Some(descriptor), Some(payload)) => Some(ValidatedValue {
            body,
            start: payload.start,
            descriptor,
        }),
        (None, None) => None,
        _ => return Err(BodyError::MaterializationMismatch),
    };
    Ok(ValidatedUnion {
        ordinal: envelope.ordinal,
        payload,
    })
}

fn require_target(actual: usize, expected: usize) -> Result<(), BodyError> {
    if !actual.is_multiple_of(BODY_ALIGNMENT) {
        return Err(BodyError::Misaligned);
    }
    if actual != expected {
        return Err(BodyError::UnexpectedTarget);
    }
    Ok(())
}

fn require_zero(body: &[u8], start: usize, end: usize) -> Result<(), BodyError> {
    let bytes = body.get(start..end).ok_or(BodyError::Truncated)?;
    if bytes.iter().any(|byte| *byte != 0) {
        return Err(BodyError::NonZeroPadding);
    }
    Ok(())
}

fn align_up(value: usize, alignment: usize) -> Result<usize, BodyError> {
    if !alignment.is_power_of_two() {
        return Err(BodyError::InvalidDescriptor);
    }
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or(BodyError::ArithmeticOverflow)
}

fn slice_at(body: &[u8], start: usize, bytes: usize) -> Result<&[u8], BodyError> {
    let end = start
        .checked_add(bytes)
        .ok_or(BodyError::ArithmeticOverflow)?;
    body.get(start..end).ok_or(BodyError::Truncated)
}

fn copy_array<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut output = [0; N];
    output.copy_from_slice(bytes);
    output
}

fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(copy_array(&bytes[offset..offset + 2]))
}

fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(copy_array(&bytes[offset..offset + 4]))
}

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(copy_array(&bytes[offset..offset + 8]))
}
