use core::str;

pub const SLICE_REF_BYTES: usize = 16;
pub const TABLE_REF_BYTES: usize = 16;
pub const ENVELOPE_BYTES: usize = 24;

const BODY_ALIGNMENT: usize = 8;
const CANONICAL_F32_NAN: u32 = 0x7fc0_0000;
const CANONICAL_F64_NAN: u64 = 0x7ff8_0000_0000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyLimits {
    pub max_depth: u8,
    pub max_table_fields: u16,
}

impl BodyLimits {
    pub const DESKTOP: Self = Self {
        max_depth: 32,
        max_table_fields: 1_024,
    };

    pub const ENDPOINT_PROTOTYPE: Self = Self {
        max_depth: 8,
        max_table_fields: 8,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyError {
    Truncated,
    OutputTooSmall,
    ArithmeticOverflow,
    Misaligned,
    OutOfBounds,
    InvalidReference,
    UnexpectedTarget,
    RegionLengthMismatch,
    TrailingBytes,
    ReservedValueUsed,
    NonZeroPadding,
    InvalidBool,
    NonCanonicalFloat,
    InvalidUtf8,
    LimitExceeded,
    InvalidOrdinal,
    EnvelopeOrder,
    DuplicateOrdinal,
    UnknownResultOrdinal,
    MissingPayload,
    UnexpectedPayload,
    IncompleteTable,
    IncompleteVector,
    InvalidElementLayout,
    InvalidDescriptor,
    ProtocolMismatch,
    UnknownEnumValue,
    UnknownUnionOrdinal,
    InvalidOptionalOrdinal,
    ReservedOrdinal,
    MissingRequiredField,
    FieldUnavailable,
    MaterializationMismatch,
    Poisoned,
}

pub struct BodyDecoder<'a> {
    root: ValueDecoder<'a>,
}

impl<'a> BodyDecoder<'a> {
    pub fn new(
        body: &'a [u8],
        root_inline_bytes: usize,
        limits: BodyLimits,
    ) -> Result<Self, BodyError> {
        if limits.max_depth == 0 {
            return Err(BodyError::LimitExceeded);
        }
        if !body.len().is_multiple_of(BODY_ALIGNMENT) {
            return Err(BodyError::Misaligned);
        }
        Ok(Self {
            root: ValueDecoder::new(body, 0, root_inline_bytes, body.len(), 1, limits)?,
        })
    }

    pub fn root(&mut self) -> &mut ValueDecoder<'a> {
        &mut self.root
    }

    pub fn finish(self) -> Result<(), BodyError> {
        self.root.finish()
    }
}

pub struct ValueDecoder<'a> {
    body: &'a [u8],
    start: usize,
    inline_end: usize,
    next_child: usize,
    end: usize,
    depth: u8,
    limits: BodyLimits,
}

impl<'a> ValueDecoder<'a> {
    fn new(
        body: &'a [u8],
        start: usize,
        inline_bytes: usize,
        end: usize,
        depth: u8,
        limits: BodyLimits,
    ) -> Result<Self, BodyError> {
        if depth == 0 || depth > limits.max_depth {
            return Err(BodyError::LimitExceeded);
        }
        if !start.is_multiple_of(BODY_ALIGNMENT) || !end.is_multiple_of(BODY_ALIGNMENT) {
            return Err(BodyError::Misaligned);
        }
        let inline_end = start
            .checked_add(inline_bytes)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if inline_end > end || end > body.len() {
            return Err(BodyError::Truncated);
        }
        let next_child = align_up(inline_end, BODY_ALIGNMENT)?;
        if next_child > end {
            return Err(BodyError::Truncated);
        }
        require_zero(body, inline_end, next_child)?;
        Ok(Self {
            body,
            start,
            inline_end,
            next_child,
            end,
            depth,
            limits,
        })
    }

    pub const fn inline_bytes(&self) -> usize {
        self.inline_end - self.start
    }

    pub fn read_bool(&self, offset: usize) -> Result<bool, BodyError> {
        match self.read_u8(offset)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(BodyError::InvalidBool),
        }
    }

    pub fn read_u8(&self, offset: usize) -> Result<u8, BodyError> {
        Ok(self.inline(offset, 1, 1)?[0])
    }

    pub fn read_i8(&self, offset: usize) -> Result<i8, BodyError> {
        Ok(self.read_u8(offset)? as i8)
    }

    pub fn read_u16(&self, offset: usize) -> Result<u16, BodyError> {
        Ok(u16::from_le_bytes(copy_array(self.inline(offset, 2, 2)?)))
    }

    pub fn read_i16(&self, offset: usize) -> Result<i16, BodyError> {
        Ok(i16::from_le_bytes(copy_array(self.inline(offset, 2, 2)?)))
    }

    pub fn read_u32(&self, offset: usize) -> Result<u32, BodyError> {
        Ok(u32::from_le_bytes(copy_array(self.inline(offset, 4, 4)?)))
    }

    pub fn read_i32(&self, offset: usize) -> Result<i32, BodyError> {
        Ok(i32::from_le_bytes(copy_array(self.inline(offset, 4, 4)?)))
    }

    pub fn read_u64(&self, offset: usize) -> Result<u64, BodyError> {
        Ok(u64::from_le_bytes(copy_array(self.inline(offset, 8, 8)?)))
    }

    pub fn read_i64(&self, offset: usize) -> Result<i64, BodyError> {
        Ok(i64::from_le_bytes(copy_array(self.inline(offset, 8, 8)?)))
    }

    pub fn read_f32(&self, offset: usize) -> Result<f32, BodyError> {
        let bits = self.read_u32(offset)?;
        let value = f32::from_bits(bits);
        if value.is_nan() && bits != CANONICAL_F32_NAN {
            return Err(BodyError::NonCanonicalFloat);
        }
        Ok(value)
    }

    pub fn read_f64(&self, offset: usize) -> Result<f64, BodyError> {
        let bits = self.read_u64(offset)?;
        let value = f64::from_bits(bits);
        if value.is_nan() && bits != CANONICAL_F64_NAN {
            return Err(BodyError::NonCanonicalFloat);
        }
        Ok(value)
    }

    pub fn read_id128(&self, offset: usize) -> Result<[u8; 16], BodyError> {
        Ok(copy_array(self.inline(offset, 16, 8)?))
    }

    pub fn require_zero(&self, offset: usize, bytes: usize) -> Result<(), BodyError> {
        let range = self.inline(offset, bytes, 1)?;
        if range.iter().any(|byte| *byte != 0) {
            return Err(BodyError::NonZeroPadding);
        }
        Ok(())
    }

    pub fn bytes(&mut self, offset: usize, maximum: u32) -> Result<&'a [u8], BodyError> {
        self.slice(offset, maximum)
    }

    pub fn string(&mut self, offset: usize, maximum: u32) -> Result<&'a str, BodyError> {
        str::from_utf8(self.slice(offset, maximum)?).map_err(|_| BodyError::InvalidUtf8)
    }

    pub fn table<R, F>(
        &mut self,
        offset: usize,
        maximum_fields: u16,
        decode: F,
    ) -> Result<R, BodyError>
    where
        F: FnOnce(&mut TableDecoder<'a>) -> Result<R, BodyError>,
    {
        let reference = self.inline(offset, TABLE_REF_BYTES, 4)?;
        let reference_start = self
            .start
            .checked_add(offset)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let relative_offset = get_u32(reference, 0) as usize;
        let field_count = get_u16(reference, 4);
        let reserved0 = get_u16(reference, 6);
        let region_bytes = get_u32(reference, 8) as usize;
        let reserved1 = get_u32(reference, 12);
        if reserved0 != 0 || reserved1 != 0 {
            return Err(BodyError::ReservedValueUsed);
        }
        if field_count > maximum_fields || field_count > self.limits.max_table_fields {
            return Err(BodyError::LimitExceeded);
        }
        if field_count == 0 {
            if relative_offset != 0 || region_bytes != 0 {
                return Err(BodyError::InvalidReference);
            }
            let mut table = TableDecoder::empty(self.body, self.depth, self.limits);
            let result = decode(&mut table)?;
            table.finish()?;
            return Ok(result);
        }
        if relative_offset == 0 || region_bytes == 0 || !region_bytes.is_multiple_of(BODY_ALIGNMENT)
        {
            return Err(BodyError::InvalidReference);
        }
        let target = reference_start
            .checked_add(relative_offset)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if !target.is_multiple_of(BODY_ALIGNMENT) {
            return Err(BodyError::Misaligned);
        }
        if target != self.next_child {
            return Err(BodyError::UnexpectedTarget);
        }
        let table_end = target
            .checked_add(region_bytes)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if table_end > self.end {
            return Err(BodyError::OutOfBounds);
        }
        let envelope_bytes = usize::from(field_count)
            .checked_mul(ENVELOPE_BYTES)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let payload_start = target
            .checked_add(envelope_bytes)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if payload_start > table_end {
            return Err(BodyError::Truncated);
        }
        validate_table_structure(
            self.body,
            target,
            field_count,
            payload_start,
            table_end,
            self.depth,
            self.limits,
        )?;
        let mut table = TableDecoder {
            body: self.body,
            envelopes_start: target,
            field_count,
            index: 0,
            previous_ordinal: None,
            payload_cursor: payload_start,
            end: table_end,
            depth: self.depth,
            limits: self.limits,
        };
        let result = decode(&mut table)?;
        table.finish()?;
        self.next_child = table_end;
        Ok(result)
    }

    pub fn closed_result<R, F>(&mut self, offset: usize, decode: F) -> Result<R, BodyError>
    where
        F: FnOnce(ClosedResultDecoder<'a>) -> Result<R, BodyError>,
    {
        let envelope_start = self
            .start
            .checked_add(offset)
            .ok_or(BodyError::ArithmeticOverflow)?;
        self.inline(offset, ENVELOPE_BYTES, 4)?;
        let envelope = decode_envelope(self.body, envelope_start, self.next_child, self.end)?;
        if envelope.ordinal != 1 && envelope.ordinal != 2 {
            return Err(BodyError::UnknownResultOrdinal);
        }
        next_depth(self.depth, self.limits)?;
        let result = decode(ClosedResultDecoder {
            field: FieldDecoder {
                body: self.body,
                ordinal: envelope.ordinal,
                payload: envelope.payload,
                parent_depth: self.depth,
                limits: self.limits,
            },
        })?;
        if let Some(payload) = envelope.payload {
            self.next_child = payload.end;
        }
        Ok(result)
    }

    pub fn finish(self) -> Result<(), BodyError> {
        if self.next_child != self.end {
            return Err(BodyError::TrailingBytes);
        }
        Ok(())
    }

    fn inline(&self, offset: usize, bytes: usize, alignment: usize) -> Result<&'a [u8], BodyError> {
        if !offset.is_multiple_of(alignment) {
            return Err(BodyError::Misaligned);
        }
        let start = self
            .start
            .checked_add(offset)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let end = start
            .checked_add(bytes)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if end > self.inline_end {
            return Err(BodyError::Truncated);
        }
        Ok(&self.body[start..end])
    }

    fn slice(&mut self, offset: usize, maximum: u32) -> Result<&'a [u8], BodyError> {
        let reference = self.inline(offset, SLICE_REF_BYTES, 4)?;
        let reference_start = self
            .start
            .checked_add(offset)
            .ok_or(BodyError::ArithmeticOverflow)?;
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
            return Ok(&self.body[0..0]);
        }
        let count = count as usize;
        let expected_region = align_up(count, BODY_ALIGNMENT)?;
        if relative_offset == 0 || region_bytes != expected_region {
            return Err(BodyError::RegionLengthMismatch);
        }
        let target = reference_start
            .checked_add(relative_offset)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if !target.is_multiple_of(BODY_ALIGNMENT) {
            return Err(BodyError::Misaligned);
        }
        if target != self.next_child {
            return Err(BodyError::UnexpectedTarget);
        }
        let data_end = target
            .checked_add(count)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let region_end = target
            .checked_add(region_bytes)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if region_end > self.end {
            return Err(BodyError::OutOfBounds);
        }
        require_zero(self.body, data_end, region_end)?;
        self.next_child = region_end;
        Ok(&self.body[target..data_end])
    }
}

pub struct TableDecoder<'a> {
    body: &'a [u8],
    envelopes_start: usize,
    field_count: u16,
    index: u16,
    previous_ordinal: Option<u32>,
    payload_cursor: usize,
    end: usize,
    depth: u8,
    limits: BodyLimits,
}

impl<'a> TableDecoder<'a> {
    fn empty(body: &'a [u8], depth: u8, limits: BodyLimits) -> Self {
        Self {
            body,
            envelopes_start: 0,
            field_count: 0,
            index: 0,
            previous_ordinal: None,
            payload_cursor: 0,
            end: 0,
            depth,
            limits,
        }
    }

    pub const fn field_count(&self) -> u16 {
        self.field_count
    }

    pub fn next_field(&mut self) -> Result<Option<FieldDecoder<'a>>, BodyError> {
        if self.index == self.field_count {
            return Ok(None);
        }
        let envelope_start = self
            .envelopes_start
            .checked_add(
                usize::from(self.index)
                    .checked_mul(ENVELOPE_BYTES)
                    .ok_or(BodyError::ArithmeticOverflow)?,
            )
            .ok_or(BodyError::ArithmeticOverflow)?;
        let envelope = decode_envelope(self.body, envelope_start, self.payload_cursor, self.end)?;
        if envelope.ordinal == 0 {
            return Err(BodyError::InvalidOrdinal);
        }
        if let Some(previous) = self.previous_ordinal {
            if envelope.ordinal == previous {
                return Err(BodyError::DuplicateOrdinal);
            }
            if envelope.ordinal < previous {
                return Err(BodyError::EnvelopeOrder);
            }
        }
        self.index += 1;
        self.previous_ordinal = Some(envelope.ordinal);
        if let Some(payload) = envelope.payload {
            self.payload_cursor = payload.end;
        }
        next_depth(self.depth, self.limits)?;
        Ok(Some(FieldDecoder {
            body: self.body,
            ordinal: envelope.ordinal,
            payload: envelope.payload,
            parent_depth: self.depth,
            limits: self.limits,
        }))
    }

    fn finish(&self) -> Result<(), BodyError> {
        if self.index != self.field_count {
            return Err(BodyError::IncompleteTable);
        }
        if self.payload_cursor != self.end {
            return Err(BodyError::RegionLengthMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub struct FieldDecoder<'a> {
    body: &'a [u8],
    ordinal: u32,
    payload: Option<PayloadRange>,
    parent_depth: u8,
    limits: BodyLimits,
}

impl<'a> FieldDecoder<'a> {
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    pub const fn is_unit(&self) -> bool {
        self.payload.is_none()
    }

    pub fn require_unit(&self) -> Result<(), BodyError> {
        if self.payload.is_some() {
            return Err(BodyError::UnexpectedPayload);
        }
        Ok(())
    }

    pub fn decode<R, F>(&self, inline_bytes: usize, decode: F) -> Result<R, BodyError>
    where
        F: FnOnce(&mut ValueDecoder<'a>) -> Result<R, BodyError>,
    {
        let payload = self.payload.ok_or(BodyError::MissingPayload)?;
        let mut value = ValueDecoder::new(
            self.body,
            payload.start,
            inline_bytes,
            payload.end,
            next_depth(self.parent_depth, self.limits)?,
            self.limits,
        )?;
        let result = decode(&mut value)?;
        value.finish()?;
        Ok(result)
    }
}

pub struct ClosedResultDecoder<'a> {
    field: FieldDecoder<'a>,
}

impl<'a> ClosedResultDecoder<'a> {
    pub const fn ordinal(&self) -> u32 {
        self.field.ordinal()
    }

    pub const fn is_success(&self) -> bool {
        self.ordinal() == 1
    }

    pub const fn is_error(&self) -> bool {
        self.ordinal() == 2
    }

    pub const fn is_unit(&self) -> bool {
        self.field.is_unit()
    }

    pub fn require_unit(&self) -> Result<(), BodyError> {
        self.field.require_unit()
    }

    pub fn decode<R, F>(&self, inline_bytes: usize, decode: F) -> Result<R, BodyError>
    where
        F: FnOnce(&mut ValueDecoder<'a>) -> Result<R, BodyError>,
    {
        self.field.decode(inline_bytes, decode)
    }
}

#[derive(Clone, Copy)]
struct PayloadRange {
    start: usize,
    end: usize,
}

struct DecodedEnvelope {
    ordinal: u32,
    payload: Option<PayloadRange>,
}

fn validate_table_structure(
    body: &[u8],
    envelopes_start: usize,
    field_count: u16,
    mut payload_cursor: usize,
    table_end: usize,
    depth: u8,
    limits: BodyLimits,
) -> Result<(), BodyError> {
    let mut previous_ordinal = None;
    for index in 0..field_count {
        let envelope_start = envelopes_start
            .checked_add(
                usize::from(index)
                    .checked_mul(ENVELOPE_BYTES)
                    .ok_or(BodyError::ArithmeticOverflow)?,
            )
            .ok_or(BodyError::ArithmeticOverflow)?;
        let envelope = decode_envelope(body, envelope_start, payload_cursor, table_end)?;
        if envelope.ordinal == 0 {
            return Err(BodyError::InvalidOrdinal);
        }
        if let Some(previous) = previous_ordinal {
            if envelope.ordinal == previous {
                return Err(BodyError::DuplicateOrdinal);
            }
            if envelope.ordinal < previous {
                return Err(BodyError::EnvelopeOrder);
            }
        }
        next_depth(depth, limits)?;
        previous_ordinal = Some(envelope.ordinal);
        if let Some(payload) = envelope.payload {
            payload_cursor = payload.end;
        }
    }
    if payload_cursor != table_end {
        return Err(BodyError::RegionLengthMismatch);
    }
    Ok(())
}

fn decode_envelope(
    body: &[u8],
    envelope_start: usize,
    expected_payload: usize,
    enclosing_end: usize,
) -> Result<DecodedEnvelope, BodyError> {
    let envelope_end = envelope_start
        .checked_add(ENVELOPE_BYTES)
        .ok_or(BodyError::ArithmeticOverflow)?;
    let bytes = body
        .get(envelope_start..envelope_end)
        .ok_or(BodyError::Truncated)?;
    let ordinal = get_u32(bytes, 0);
    if get_u16(bytes, 4) != 0
        || get_u16(bytes, 6) != 0
        || get_u16(bytes, 16) != 0
        || get_u16(bytes, 18) != 0
        || get_u32(bytes, 20) != 0
    {
        return Err(BodyError::ReservedValueUsed);
    }
    let relative_offset = get_u32(bytes, 8) as usize;
    let payload_bytes = get_u32(bytes, 12) as usize;
    if relative_offset == 0 && payload_bytes == 0 {
        return Ok(DecodedEnvelope {
            ordinal,
            payload: None,
        });
    }
    if relative_offset == 0 || payload_bytes == 0 || !payload_bytes.is_multiple_of(BODY_ALIGNMENT) {
        return Err(BodyError::InvalidReference);
    }
    let offset_field = envelope_start
        .checked_add(8)
        .ok_or(BodyError::ArithmeticOverflow)?;
    let payload_start = offset_field
        .checked_add(relative_offset)
        .ok_or(BodyError::ArithmeticOverflow)?;
    if !payload_start.is_multiple_of(BODY_ALIGNMENT) {
        return Err(BodyError::Misaligned);
    }
    if payload_start != expected_payload {
        return Err(BodyError::UnexpectedTarget);
    }
    let payload_end = payload_start
        .checked_add(payload_bytes)
        .ok_or(BodyError::ArithmeticOverflow)?;
    if payload_end > enclosing_end {
        return Err(BodyError::OutOfBounds);
    }
    Ok(DecodedEnvelope {
        ordinal,
        payload: Some(PayloadRange {
            start: payload_start,
            end: payload_end,
        }),
    })
}

pub struct BodyEncoder<'a> {
    root: ValueEncoder<'a>,
}

impl<'a> BodyEncoder<'a> {
    pub fn new(
        output: &'a mut [u8],
        body_bytes: usize,
        root_inline_bytes: usize,
        limits: BodyLimits,
    ) -> Result<Self, BodyError> {
        if limits.max_depth == 0 {
            return Err(BodyError::LimitExceeded);
        }
        if !body_bytes.is_multiple_of(BODY_ALIGNMENT) {
            return Err(BodyError::Misaligned);
        }
        let output = output
            .get_mut(..body_bytes)
            .ok_or(BodyError::OutputTooSmall)?;
        let root_inline_end = align_up(root_inline_bytes, BODY_ALIGNMENT)?;
        if root_inline_end > body_bytes {
            return Err(BodyError::OutputTooSmall);
        }
        output.fill(0);
        Ok(Self {
            root: ValueEncoder::new(output, 0, root_inline_bytes, body_bytes, 1, limits)?,
        })
    }

    pub fn root(&mut self) -> &mut ValueEncoder<'a> {
        &mut self.root
    }

    pub fn finish(self) -> Result<(), BodyError> {
        self.root.finish_exact()
    }
}

pub struct ValueEncoder<'a> {
    output: &'a mut [u8],
    start: usize,
    inline_end: usize,
    next_child: usize,
    limit: usize,
    depth: u8,
    limits: BodyLimits,
    poisoned: bool,
}

impl<'a> ValueEncoder<'a> {
    fn new(
        output: &'a mut [u8],
        start: usize,
        inline_bytes: usize,
        limit: usize,
        depth: u8,
        limits: BodyLimits,
    ) -> Result<Self, BodyError> {
        if depth == 0 || depth > limits.max_depth {
            return Err(BodyError::LimitExceeded);
        }
        if !start.is_multiple_of(BODY_ALIGNMENT) || limit > output.len() {
            return Err(BodyError::Misaligned);
        }
        let inline_end = start
            .checked_add(inline_bytes)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let next_child = align_up(inline_end, BODY_ALIGNMENT)?;
        if next_child > limit {
            return Err(BodyError::OutputTooSmall);
        }
        Ok(Self {
            output,
            start,
            inline_end,
            next_child,
            limit,
            depth,
            limits,
            poisoned: false,
        })
    }

    pub fn write_bool(&mut self, offset: usize, value: bool) -> Result<(), BodyError> {
        self.write_u8(offset, u8::from(value))
    }

    pub fn write_u8(&mut self, offset: usize, value: u8) -> Result<(), BodyError> {
        self.inline_mut(offset, 1, 1)?[0] = value;
        Ok(())
    }

    pub fn write_i8(&mut self, offset: usize, value: i8) -> Result<(), BodyError> {
        self.write_u8(offset, value as u8)
    }

    pub fn write_u16(&mut self, offset: usize, value: u16) -> Result<(), BodyError> {
        self.inline_mut(offset, 2, 2)?
            .copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    pub fn write_i16(&mut self, offset: usize, value: i16) -> Result<(), BodyError> {
        self.inline_mut(offset, 2, 2)?
            .copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    pub fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), BodyError> {
        self.inline_mut(offset, 4, 4)?
            .copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    pub fn write_i32(&mut self, offset: usize, value: i32) -> Result<(), BodyError> {
        self.inline_mut(offset, 4, 4)?
            .copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    pub fn write_u64(&mut self, offset: usize, value: u64) -> Result<(), BodyError> {
        self.inline_mut(offset, 8, 8)?
            .copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    pub fn write_i64(&mut self, offset: usize, value: i64) -> Result<(), BodyError> {
        self.inline_mut(offset, 8, 8)?
            .copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    pub fn write_f32(&mut self, offset: usize, value: f32) -> Result<(), BodyError> {
        let bits = if value.is_nan() {
            CANONICAL_F32_NAN
        } else {
            value.to_bits()
        };
        self.write_u32(offset, bits)
    }

    pub fn write_f64(&mut self, offset: usize, value: f64) -> Result<(), BodyError> {
        let bits = if value.is_nan() {
            CANONICAL_F64_NAN
        } else {
            value.to_bits()
        };
        self.write_u64(offset, bits)
    }

    pub fn write_id128(&mut self, offset: usize, value: [u8; 16]) -> Result<(), BodyError> {
        self.inline_mut(offset, 16, 8)?.copy_from_slice(&value);
        Ok(())
    }

    pub fn bytes(&mut self, offset: usize, maximum: u32, value: &[u8]) -> Result<(), BodyError> {
        let count = u32::try_from(value.len()).map_err(|_| BodyError::LimitExceeded)?;
        if count > maximum {
            return Err(BodyError::LimitExceeded);
        }
        self.inline_mut(offset, SLICE_REF_BYTES, 4)?;
        if value.is_empty() {
            return Ok(());
        }
        let reference_start = self
            .start
            .checked_add(offset)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let target = self.next_child;
        let region_bytes = align_up(value.len(), BODY_ALIGNMENT)?;
        let region_end = target
            .checked_add(region_bytes)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if region_end > self.limit {
            return Err(BodyError::OutputTooSmall);
        }
        let relative_offset = relative_u32(reference_start, target)?;
        let region_bytes =
            u32::try_from(region_bytes).map_err(|_| BodyError::ArithmeticOverflow)?;
        self.output[target..target + value.len()].copy_from_slice(value);
        let reference = self.inline_mut(offset, SLICE_REF_BYTES, 4)?;
        put_u32(reference, 0, relative_offset);
        put_u32(reference, 4, count);
        put_u32(reference, 8, region_bytes);
        self.next_child = region_end;
        Ok(())
    }

    pub fn string(&mut self, offset: usize, maximum: u32, value: &str) -> Result<(), BodyError> {
        self.bytes(offset, maximum, value.as_bytes())
    }

    pub fn vector<F>(
        &mut self,
        offset: usize,
        count: u32,
        maximum_elements: u32,
        element_inline_bytes: usize,
        element_alignment: usize,
        encode: F,
    ) -> Result<(), BodyError>
    where
        F: FnOnce(&mut VectorEncoder<'_>) -> Result<(), BodyError>,
    {
        let stride = element_stride(element_inline_bytes, element_alignment)?;
        self.inline_mut(offset, SLICE_REF_BYTES, 4)?;
        if count > maximum_elements {
            return Err(BodyError::LimitExceeded);
        }

        let target = self.next_child;
        let count_usize = usize::try_from(count).map_err(|_| BodyError::ArithmeticOverflow)?;
        let array_bytes = count_usize
            .checked_mul(stride)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let array_end = target
            .checked_add(array_bytes)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let descendants_start = align_up(array_end, BODY_ALIGNMENT)?;
        if descendants_start > self.limit {
            return Err(BodyError::OutputTooSmall);
        }

        self.output[target..descendants_start].fill(0);
        let vector_result = {
            let mut vector = VectorEncoder {
                output: self.output,
                array_start: target,
                element_inline_bytes,
                stride,
                count,
                index: 0,
                descendants_cursor: descendants_start,
                limit: self.limit,
                parent_depth: self.depth,
                limits: self.limits,
                poisoned: false,
            };
            encode(&mut vector)
                .and_then(|_| vector.finish())
                .map(|_| vector.descendants_cursor)
        };
        let vector_end = match vector_result {
            Ok(vector_end) => vector_end,
            Err(error) => {
                self.output[target..self.limit].fill(0);
                self.poisoned = true;
                return Err(error);
            }
        };

        if count == 0 {
            self.inline_mut(offset, SLICE_REF_BYTES, 4)?.fill(0);
            return Ok(());
        }

        let commit = (|| {
            let reference_start = self
                .start
                .checked_add(offset)
                .ok_or(BodyError::ArithmeticOverflow)?;
            let region_bytes = vector_end
                .checked_sub(target)
                .ok_or(BodyError::ArithmeticOverflow)?;
            let relative_offset = relative_u32(reference_start, target)?;
            let region_bytes =
                u32::try_from(region_bytes).map_err(|_| BodyError::ArithmeticOverflow)?;
            let reference = self.inline_mut(offset, SLICE_REF_BYTES, 4)?;
            put_u32(reference, 0, relative_offset);
            put_u32(reference, 4, count);
            put_u32(reference, 8, region_bytes);
            Ok(())
        })();
        if let Err(error) = commit {
            self.output[target..self.limit].fill(0);
            self.poisoned = true;
            return Err(error);
        }
        self.next_child = vector_end;
        Ok(())
    }

    pub fn table<F>(
        &mut self,
        offset: usize,
        field_count: u16,
        maximum_fields: u16,
        encode: F,
    ) -> Result<(), BodyError>
    where
        F: FnOnce(&mut TableEncoder<'_>) -> Result<(), BodyError>,
    {
        self.inline_mut(offset, TABLE_REF_BYTES, 4)?;
        if field_count > maximum_fields || field_count > self.limits.max_table_fields {
            return Err(BodyError::LimitExceeded);
        }
        if field_count == 0 {
            let result = {
                let mut table = TableEncoder::empty(self.output, self.depth, self.limits);
                encode(&mut table).and_then(|_| table.finish())
            };
            if let Err(error) = result {
                self.poisoned = true;
                return Err(error);
            }
            return Ok(());
        }
        let target = self.next_child;
        let envelope_bytes = usize::from(field_count)
            .checked_mul(ENVELOPE_BYTES)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let payload_start = target
            .checked_add(envelope_bytes)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if payload_start > self.limit {
            return Err(BodyError::OutputTooSmall);
        }
        let table_result = {
            let mut table = TableEncoder {
                output: self.output,
                envelopes_start: target,
                field_count,
                index: 0,
                previous_ordinal: None,
                payload_cursor: payload_start,
                limit: self.limit,
                depth: self.depth,
                limits: self.limits,
                poisoned: false,
            };
            encode(&mut table)
                .and_then(|_| table.finish())
                .map(|_| table.payload_cursor)
        };
        let table_end = match table_result {
            Ok(table_end) => table_end,
            Err(error) => {
                self.output[target..self.limit].fill(0);
                self.poisoned = true;
                return Err(error);
            }
        };
        let commit = (|| {
            let reference_start = self
                .start
                .checked_add(offset)
                .ok_or(BodyError::ArithmeticOverflow)?;
            let region_bytes = table_end
                .checked_sub(target)
                .ok_or(BodyError::ArithmeticOverflow)?;
            let relative_offset = relative_u32(reference_start, target)?;
            let region_bytes =
                u32::try_from(region_bytes).map_err(|_| BodyError::ArithmeticOverflow)?;
            let reference = self.inline_mut(offset, TABLE_REF_BYTES, 4)?;
            put_u32(reference, 0, relative_offset);
            put_u16(reference, 4, field_count);
            put_u32(reference, 8, region_bytes);
            Ok(())
        })();
        if let Err(error) = commit {
            self.output[target..self.limit].fill(0);
            self.poisoned = true;
            return Err(error);
        }
        self.next_child = table_end;
        Ok(())
    }

    pub fn optional_none(&mut self, offset: usize) -> Result<(), BodyError> {
        self.inline_mut(offset, ENVELOPE_BYTES, 4)?.fill(0);
        Ok(())
    }

    pub fn optional_some<F>(
        &mut self,
        offset: usize,
        payload_inline_bytes: usize,
        encode: F,
    ) -> Result<(), BodyError>
    where
        F: FnOnce(&mut ValueEncoder<'_>) -> Result<(), BodyError>,
    {
        self.closed_union(offset, 1, payload_inline_bytes, encode)
    }

    pub fn optional_some_unit(&mut self, offset: usize) -> Result<(), BodyError> {
        self.closed_union_unit(offset, 1)
    }

    pub fn closed_union<F>(
        &mut self,
        offset: usize,
        ordinal: u32,
        payload_inline_bytes: usize,
        encode: F,
    ) -> Result<(), BodyError>
    where
        F: FnOnce(&mut ValueEncoder<'_>) -> Result<(), BodyError>,
    {
        if ordinal == 0 {
            return Err(BodyError::InvalidOrdinal);
        }
        self.inline_mut(offset, ENVELOPE_BYTES, 4)?;
        let payload_start = self.next_child;
        let depth = next_depth(self.depth, self.limits)?;
        let payload_result = {
            let mut payload = ValueEncoder::new(
                self.output,
                payload_start,
                payload_inline_bytes,
                self.limit,
                depth,
                self.limits,
            )?;
            encode(&mut payload).and_then(|_| payload.finish_nested())
        };
        let payload_end = match payload_result {
            Ok(payload_end) => payload_end,
            Err(error) => {
                self.output[payload_start..self.limit].fill(0);
                self.poisoned = true;
                return Err(error);
            }
        };
        if payload_end == payload_start {
            self.output[payload_start..self.limit].fill(0);
            self.poisoned = true;
            return Err(BodyError::MissingPayload);
        }
        let envelope_start = self
            .start
            .checked_add(offset)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if let Err(error) = encode_envelope(
            self.output,
            envelope_start,
            ordinal,
            Some(PayloadRange {
                start: payload_start,
                end: payload_end,
            }),
        ) {
            self.output[payload_start..self.limit].fill(0);
            self.poisoned = true;
            return Err(error);
        }
        self.next_child = payload_end;
        Ok(())
    }

    pub fn closed_union_unit(&mut self, offset: usize, ordinal: u32) -> Result<(), BodyError> {
        if ordinal == 0 {
            return Err(BodyError::InvalidOrdinal);
        }
        self.inline_mut(offset, ENVELOPE_BYTES, 4)?;
        next_depth(self.depth, self.limits)?;
        let envelope_start = self
            .start
            .checked_add(offset)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if let Err(error) = encode_envelope(self.output, envelope_start, ordinal, None) {
            self.poisoned = true;
            return Err(error);
        }
        Ok(())
    }

    pub fn closed_result<F>(
        &mut self,
        offset: usize,
        ordinal: u32,
        payload_inline_bytes: usize,
        encode: F,
    ) -> Result<(), BodyError>
    where
        F: FnOnce(&mut ValueEncoder<'_>) -> Result<(), BodyError>,
    {
        if ordinal != 1 && ordinal != 2 {
            return Err(BodyError::UnknownResultOrdinal);
        }
        self.closed_union(offset, ordinal, payload_inline_bytes, encode)
    }

    pub fn closed_result_unit(&mut self, offset: usize, ordinal: u32) -> Result<(), BodyError> {
        if ordinal != 1 && ordinal != 2 {
            return Err(BodyError::UnknownResultOrdinal);
        }
        self.closed_union_unit(offset, ordinal)
    }

    fn inline_mut(
        &mut self,
        offset: usize,
        bytes: usize,
        alignment: usize,
    ) -> Result<&mut [u8], BodyError> {
        if !offset.is_multiple_of(alignment) {
            return Err(BodyError::Misaligned);
        }
        let start = self
            .start
            .checked_add(offset)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let end = start
            .checked_add(bytes)
            .ok_or(BodyError::ArithmeticOverflow)?;
        if end > self.inline_end {
            return Err(BodyError::Truncated);
        }
        Ok(&mut self.output[start..end])
    }

    fn finish_nested(self) -> Result<usize, BodyError> {
        if self.poisoned {
            return Err(BodyError::Poisoned);
        }
        if !self.next_child.is_multiple_of(BODY_ALIGNMENT) {
            return Err(BodyError::Misaligned);
        }
        Ok(self.next_child)
    }

    fn finish_exact(self) -> Result<(), BodyError> {
        if self.poisoned {
            return Err(BodyError::Poisoned);
        }
        if self.next_child != self.limit {
            return Err(BodyError::TrailingBytes);
        }
        Ok(())
    }
}

pub struct VectorEncoder<'a> {
    output: &'a mut [u8],
    array_start: usize,
    element_inline_bytes: usize,
    stride: usize,
    count: u32,
    index: u32,
    descendants_cursor: usize,
    limit: usize,
    parent_depth: u8,
    limits: BodyLimits,
    poisoned: bool,
}

impl<'a> VectorEncoder<'a> {
    pub const fn element_count(&self) -> u32 {
        self.count
    }

    pub const fn next_index(&self) -> u32 {
        self.index
    }

    pub fn element<F>(&mut self, encode: F) -> Result<(), BodyError>
    where
        F: FnOnce(&mut ValueEncoder<'_>) -> Result<(), BodyError>,
    {
        if self.poisoned {
            return Err(BodyError::Poisoned);
        }
        let element_result = (|| {
            if self.index >= self.count {
                return Err(BodyError::IncompleteVector);
            }
            let index = usize::try_from(self.index).map_err(|_| BodyError::ArithmeticOverflow)?;
            let element_offset = index
                .checked_mul(self.stride)
                .ok_or(BodyError::ArithmeticOverflow)?;
            let element_start = self
                .array_start
                .checked_add(element_offset)
                .ok_or(BodyError::ArithmeticOverflow)?;
            let inline_end = element_start
                .checked_add(self.element_inline_bytes)
                .ok_or(BodyError::ArithmeticOverflow)?;
            if inline_end > self.descendants_cursor {
                return Err(BodyError::OutputTooSmall);
            }
            let depth = next_depth(self.parent_depth, self.limits)?;
            let mut element = ValueEncoder {
                output: self.output,
                start: element_start,
                inline_end,
                next_child: self.descendants_cursor,
                limit: self.limit,
                depth,
                limits: self.limits,
                poisoned: false,
            };
            encode(&mut element).and_then(|_| element.finish_nested())
        })();
        match element_result {
            Ok(descendants_cursor) => {
                self.descendants_cursor = descendants_cursor;
                self.index += 1;
                Ok(())
            }
            Err(error) => {
                self.poisoned = true;
                Err(error)
            }
        }
    }

    fn finish(&self) -> Result<(), BodyError> {
        if self.poisoned {
            return Err(BodyError::Poisoned);
        }
        if self.index != self.count {
            return Err(BodyError::IncompleteVector);
        }
        Ok(())
    }
}

pub struct TableEncoder<'a> {
    output: &'a mut [u8],
    envelopes_start: usize,
    field_count: u16,
    index: u16,
    previous_ordinal: Option<u32>,
    payload_cursor: usize,
    limit: usize,
    depth: u8,
    limits: BodyLimits,
    poisoned: bool,
}

impl<'a> TableEncoder<'a> {
    fn empty(output: &'a mut [u8], depth: u8, limits: BodyLimits) -> Self {
        Self {
            output,
            envelopes_start: 0,
            field_count: 0,
            index: 0,
            previous_ordinal: None,
            payload_cursor: 0,
            limit: 0,
            depth,
            limits,
            poisoned: false,
        }
    }

    pub fn field<F>(
        &mut self,
        ordinal: u32,
        payload_inline_bytes: usize,
        encode: F,
    ) -> Result<(), BodyError>
    where
        F: FnOnce(&mut ValueEncoder<'_>) -> Result<(), BodyError>,
    {
        self.prepare_ordinal(ordinal)?;
        let payload_start = self.payload_cursor;
        let depth = next_depth(self.depth, self.limits)?;
        let payload_result = {
            let mut payload = ValueEncoder::new(
                self.output,
                payload_start,
                payload_inline_bytes,
                self.limit,
                depth,
                self.limits,
            )?;
            encode(&mut payload).and_then(|_| payload.finish_nested())
        };
        let payload_end = match payload_result {
            Ok(payload_end) => payload_end,
            Err(error) => {
                self.output[payload_start..self.limit].fill(0);
                self.poisoned = true;
                return Err(error);
            }
        };
        if payload_end == payload_start {
            self.poisoned = true;
            return Err(BodyError::MissingPayload);
        }
        let envelope_start = self.envelope_start()?;
        if let Err(error) = encode_envelope(
            self.output,
            envelope_start,
            ordinal,
            Some(PayloadRange {
                start: payload_start,
                end: payload_end,
            }),
        ) {
            self.output[payload_start..self.limit].fill(0);
            self.poisoned = true;
            return Err(error);
        }
        self.payload_cursor = payload_end;
        self.complete_field(ordinal);
        Ok(())
    }

    pub fn unit_field(&mut self, ordinal: u32) -> Result<(), BodyError> {
        self.prepare_ordinal(ordinal)?;
        next_depth(self.depth, self.limits)?;
        let envelope_start = self.envelope_start()?;
        if let Err(error) = encode_envelope(self.output, envelope_start, ordinal, None) {
            self.poisoned = true;
            return Err(error);
        }
        self.complete_field(ordinal);
        Ok(())
    }

    fn prepare_ordinal(&self, ordinal: u32) -> Result<(), BodyError> {
        if self.poisoned {
            return Err(BodyError::Poisoned);
        }
        if self.index >= self.field_count {
            return Err(BodyError::IncompleteTable);
        }
        if ordinal == 0 {
            return Err(BodyError::InvalidOrdinal);
        }
        if let Some(previous) = self.previous_ordinal {
            if ordinal == previous {
                return Err(BodyError::DuplicateOrdinal);
            }
            if ordinal < previous {
                return Err(BodyError::EnvelopeOrder);
            }
        }
        Ok(())
    }

    fn envelope_start(&self) -> Result<usize, BodyError> {
        self.envelopes_start
            .checked_add(
                usize::from(self.index)
                    .checked_mul(ENVELOPE_BYTES)
                    .ok_or(BodyError::ArithmeticOverflow)?,
            )
            .ok_or(BodyError::ArithmeticOverflow)
    }

    fn complete_field(&mut self, ordinal: u32) {
        self.previous_ordinal = Some(ordinal);
        self.index += 1;
    }

    fn finish(&self) -> Result<(), BodyError> {
        if self.poisoned {
            return Err(BodyError::Poisoned);
        }
        if self.index != self.field_count {
            return Err(BodyError::IncompleteTable);
        }
        Ok(())
    }
}

fn encode_envelope(
    output: &mut [u8],
    envelope_start: usize,
    ordinal: u32,
    payload: Option<PayloadRange>,
) -> Result<(), BodyError> {
    let envelope_end = envelope_start
        .checked_add(ENVELOPE_BYTES)
        .ok_or(BodyError::ArithmeticOverflow)?;
    let encoded_payload = if let Some(payload) = payload {
        let offset_field = envelope_start
            .checked_add(8)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let relative_offset = relative_u32(offset_field, payload.start)?;
        let payload_bytes = payload
            .end
            .checked_sub(payload.start)
            .ok_or(BodyError::ArithmeticOverflow)?;
        let payload_bytes =
            u32::try_from(payload_bytes).map_err(|_| BodyError::ArithmeticOverflow)?;
        Some((relative_offset, payload_bytes))
    } else {
        None
    };
    let envelope = output
        .get_mut(envelope_start..envelope_end)
        .ok_or(BodyError::OutputTooSmall)?;
    envelope.fill(0);
    put_u32(envelope, 0, ordinal);
    if let Some((relative_offset, payload_bytes)) = encoded_payload {
        put_u32(envelope, 8, relative_offset);
        put_u32(envelope, 12, payload_bytes);
    }
    Ok(())
}

fn next_depth(depth: u8, limits: BodyLimits) -> Result<u8, BodyError> {
    let depth = depth.checked_add(1).ok_or(BodyError::LimitExceeded)?;
    if depth > limits.max_depth {
        return Err(BodyError::LimitExceeded);
    }
    Ok(depth)
}

fn element_stride(inline_bytes: usize, alignment: usize) -> Result<usize, BodyError> {
    if inline_bytes == 0
        || alignment == 0
        || alignment > BODY_ALIGNMENT
        || !alignment.is_power_of_two()
    {
        return Err(BodyError::InvalidElementLayout);
    }
    align_up(inline_bytes, alignment)
}

fn align_up(value: usize, alignment: usize) -> Result<usize, BodyError> {
    let mask = alignment - 1;
    value
        .checked_add(mask)
        .map(|value| value & !mask)
        .ok_or(BodyError::ArithmeticOverflow)
}

fn relative_u32(base: usize, target: usize) -> Result<u32, BodyError> {
    let relative = target
        .checked_sub(base)
        .ok_or(BodyError::InvalidReference)?;
    if relative == 0 {
        return Err(BodyError::InvalidReference);
    }
    u32::try_from(relative).map_err(|_| BodyError::ArithmeticOverflow)
}

fn require_zero(body: &[u8], start: usize, end: usize) -> Result<(), BodyError> {
    let bytes = body.get(start..end).ok_or(BodyError::Truncated)?;
    if bytes.iter().any(|byte| *byte != 0) {
        return Err(BodyError::NonZeroPadding);
    }
    Ok(())
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

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
