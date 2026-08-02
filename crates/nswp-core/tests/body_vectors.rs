use nswp_core::{BodyDecoder, BodyEncoder, BodyError, BodyLimits, FieldDecoder, SLICE_REF_BYTES};

const ECHO_BODY: [u8; 96] = [
    0x10, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x18, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x28, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x10, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x68, 0x69, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

fn require_ordinal(field: &FieldDecoder<'_>, expected: u32) -> Result<(), BodyError> {
    if field.ordinal() != expected {
        return Err(BodyError::InvalidOrdinal);
    }
    Ok(())
}

fn decode_echo(body: &[u8]) -> Result<(&str, u64), BodyError> {
    let mut decoder = BodyDecoder::new(body, 16, BodyLimits::ENDPOINT_PROTOTYPE)?;
    let decoded = decoder.root().table(0, 2, |table| {
        let message_field = table.next_field()?.ok_or(BodyError::IncompleteTable)?;
        require_ordinal(&message_field, 1)?;
        let message = message_field.decode(SLICE_REF_BYTES, |value| value.string(0, 32))?;

        let sequence_field = table.next_field()?.ok_or(BodyError::IncompleteTable)?;
        require_ordinal(&sequence_field, 2)?;
        let sequence = sequence_field.decode(8, |value| value.read_u64(0))?;
        if table.next_field()?.is_some() {
            return Err(BodyError::IncompleteTable);
        }
        Ok((message, sequence))
    })?;
    decoder.finish()?;
    Ok(decoded)
}

#[test]
fn echo_request_matches_literal_96_byte_body() {
    let mut encoded = [0xa5; 96];
    let mut encoder = BodyEncoder::new(
        &mut encoded,
        ECHO_BODY.len(),
        16,
        BodyLimits::ENDPOINT_PROTOTYPE,
    )
    .unwrap();
    encoder
        .root()
        .table(0, 2, 2, |table| {
            table.field(1, SLICE_REF_BYTES, |value| value.string(0, 32, "hi"))?;
            table.field(2, 8, |value| value.write_u64(0, 7))?;
            Ok(())
        })
        .unwrap();
    encoder.finish().unwrap();

    assert_eq!(encoded, ECHO_BODY);
    assert_eq!(decode_echo(&encoded), Ok(("hi", 7)));
}

#[test]
fn primitives_and_fixed_structure_padding_are_canonical() {
    let mut encoded = [0xa5; 48];
    let mut encoder =
        BodyEncoder::new(&mut encoded, 48, 48, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    let root = encoder.root();
    root.write_bool(0, true).unwrap();
    root.write_i16(2, -2).unwrap();
    root.write_u32(4, 0x1122_3344).unwrap();
    root.write_u64(8, 0x0102_0304_0506_0708).unwrap();
    root.write_f32(16, f32::NAN).unwrap();
    root.write_f64(24, f64::INFINITY).unwrap();
    root.write_id128(32, [0x5a; 16]).unwrap();
    encoder.finish().unwrap();

    assert_eq!(&encoded[16..20], &0x7fc0_0000_u32.to_le_bytes());
    let mut decoder = BodyDecoder::new(&encoded, 48, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    let root = decoder.root();
    assert_eq!(root.read_bool(0), Ok(true));
    assert_eq!(root.read_i16(2), Ok(-2));
    assert_eq!(root.read_u32(4), Ok(0x1122_3344));
    assert_eq!(root.read_u64(8), Ok(0x0102_0304_0506_0708));
    assert!(root.read_f32(16).unwrap().is_nan());
    assert_eq!(root.read_f64(24), Ok(f64::INFINITY));
    assert_eq!(root.read_id128(32), Ok([0x5a; 16]));
    decoder.finish().unwrap();

    let mut padded = [0xa5; 24];
    let mut encoder =
        BodyEncoder::new(&mut padded, 24, 18, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    encoder.root().write_u8(0, 1).unwrap();
    encoder.root().write_u64(8, 2).unwrap();
    encoder.root().write_u16(16, 3).unwrap();
    encoder.finish().unwrap();
    assert!(padded[1..8].iter().all(|byte| *byte == 0));
    assert!(padded[18..24].iter().all(|byte| *byte == 0));
}

#[test]
fn unknown_table_fields_are_opaque_but_fully_accounted() {
    let mut encoded = [0; 128];
    let mut encoder =
        BodyEncoder::new(&mut encoded, 128, 16, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    encoder
        .root()
        .table(0, 3, 3, |table| {
            table.field(1, 16, |value| value.string(0, 32, "hi"))?;
            table.field(2, 8, |value| value.write_u64(0, 7))?;
            table.field(9, 4, |value| value.write_u32(0, 0xdead_beef))?;
            Ok(())
        })
        .unwrap();
    encoder.finish().unwrap();

    let mut decoder = BodyDecoder::new(&encoded, 16, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    let (message, sequence) = decoder
        .root()
        .table(0, 3, |table| {
            let first = table.next_field()?.unwrap();
            let message = first.decode(16, |value| value.string(0, 32))?;
            let second = table.next_field()?.unwrap();
            let sequence = second.decode(8, |value| value.read_u64(0))?;
            let unknown = table.next_field()?.unwrap();
            assert_eq!(unknown.ordinal(), 9);
            assert!(!unknown.is_unit());
            assert!(table.next_field()?.is_none());
            Ok((message, sequence))
        })
        .unwrap();
    decoder.finish().unwrap();
    assert_eq!((message, sequence), ("hi", 7));
}

#[test]
fn closed_results_support_payload_and_unit_branches() {
    let mut success = [0; 48];
    let mut encoder =
        BodyEncoder::new(&mut success, 48, 24, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    encoder
        .root()
        .closed_result(0, 1, 16, |value| value.string(0, 8, "ok"))
        .unwrap();
    encoder.finish().unwrap();

    let mut decoder = BodyDecoder::new(&success, 24, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    let value = decoder
        .root()
        .closed_result(0, |result| {
            assert!(result.is_success());
            result.decode(16, |value| value.string(0, 8))
        })
        .unwrap();
    decoder.finish().unwrap();
    assert_eq!(value, "ok");

    let mut error = [0; 24];
    let mut encoder = BodyEncoder::new(&mut error, 24, 24, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    encoder.root().closed_result_unit(0, 2).unwrap();
    encoder.finish().unwrap();

    let mut decoder = BodyDecoder::new(&error, 24, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    decoder
        .root()
        .closed_result(0, |result| {
            assert!(result.is_error());
            result.require_unit()
        })
        .unwrap();
    decoder.finish().unwrap();
}
