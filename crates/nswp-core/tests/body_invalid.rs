use std::cell::Cell;

use nswp_core::{BodyDecoder, BodyEncoder, BodyError, BodyLimits, FieldDecoder, SLICE_REF_BYTES};

fn echo_body() -> [u8; 96] {
    let mut body = [0; 96];
    let mut encoder = BodyEncoder::new(&mut body, 96, 16, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    encoder
        .root()
        .table(0, 2, 2, |table| {
            table.field(1, SLICE_REF_BYTES, |value| value.string(0, 32, "hi"))?;
            table.field(2, 8, |value| value.write_u64(0, 7))?;
            Ok(())
        })
        .unwrap();
    encoder.finish().unwrap();
    body
}

fn require_ordinal(field: &FieldDecoder<'_>, expected: u32) -> Result<(), BodyError> {
    if field.ordinal() != expected {
        return Err(BodyError::InvalidOrdinal);
    }
    Ok(())
}

fn decode_echo(body: &[u8], limits: BodyLimits) -> Result<(), BodyError> {
    let mut decoder = BodyDecoder::new(body, 16, limits)?;
    decoder.root().table(0, 2, |table| {
        let message = table.next_field()?.ok_or(BodyError::IncompleteTable)?;
        require_ordinal(&message, 1)?;
        message.decode(16, |value| value.string(0, 32).map(|_| ()))?;
        let sequence = table.next_field()?.ok_or(BodyError::IncompleteTable)?;
        require_ordinal(&sequence, 2)?;
        sequence.decode(8, |value| value.read_u64(0).map(|_| ()))?;
        if table.next_field()?.is_some() {
            return Err(BodyError::IncompleteTable);
        }
        Ok(())
    })?;
    decoder.finish()
}

#[test]
fn references_require_exact_canonical_targets_and_regions() {
    let mut malformed = echo_body();
    malformed[0..4].copy_from_slice(&24_u32.to_le_bytes());
    assert_eq!(
        decode_echo(&malformed, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::UnexpectedTarget)
    );

    let mut malformed = echo_body();
    malformed[24..28].copy_from_slice(&48_u32.to_le_bytes());
    assert_eq!(
        decode_echo(&malformed, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::UnexpectedTarget)
    );

    let mut malformed = echo_body();
    malformed[64..68].copy_from_slice(&24_u32.to_le_bytes());
    assert_eq!(
        decode_echo(&malformed, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::UnexpectedTarget)
    );

    let mut malformed = echo_body();
    malformed[72..76].copy_from_slice(&16_u32.to_le_bytes());
    assert_eq!(
        decode_echo(&malformed, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::RegionLengthMismatch)
    );

    let mut malformed = echo_body();
    malformed[8..12].copy_from_slice(&72_u32.to_le_bytes());
    assert!(matches!(
        decode_echo(&malformed, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::OutOfBounds | BodyError::RegionLengthMismatch)
    ));
}

#[test]
fn table_envelopes_require_ordinals_order_and_zero_control_fields() {
    let mut malformed = echo_body();
    malformed[16..20].fill(0);
    assert_eq!(
        decode_echo(&malformed, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::InvalidOrdinal)
    );

    let mut malformed = echo_body();
    malformed[40..44].copy_from_slice(&1_u32.to_le_bytes());
    assert_eq!(
        decode_echo(&malformed, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::DuplicateOrdinal)
    );

    let mut malformed = echo_body();
    malformed[16..20].copy_from_slice(&2_u32.to_le_bytes());
    malformed[40..44].copy_from_slice(&1_u32.to_le_bytes());
    assert!(matches!(
        decode_echo(&malformed, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::InvalidOrdinal | BodyError::EnvelopeOrder)
    ));

    for offset in [20, 22, 32, 34, 36] {
        let mut malformed = echo_body();
        malformed[offset] = 1;
        assert_eq!(
            decode_echo(&malformed, BodyLimits::ENDPOINT_PROTOTYPE),
            Err(BodyError::ReservedValueUsed)
        );
    }
}

#[test]
fn strings_enforce_declared_bounds_utf8_and_zero_padding() {
    let mut malformed = echo_body();
    malformed[68..72].copy_from_slice(&33_u32.to_le_bytes());
    assert_eq!(
        decode_echo(&malformed, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::LimitExceeded)
    );

    let mut malformed = echo_body();
    malformed[80] = 0xff;
    assert_eq!(
        decode_echo(&malformed, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::InvalidUtf8)
    );

    let mut malformed = echo_body();
    malformed[82] = 1;
    assert_eq!(
        decode_echo(&malformed, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::NonZeroPadding)
    );
}

#[test]
fn bodies_require_complete_consumption_and_configured_limits() {
    let mut with_trailing = [0; 104];
    with_trailing[..96].copy_from_slice(&echo_body());
    assert_eq!(
        decode_echo(&with_trailing, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::TrailingBytes)
    );

    assert_eq!(
        decode_echo(
            &echo_body(),
            BodyLimits {
                max_depth: 1,
                max_table_fields: 8,
            }
        ),
        Err(BodyError::LimitExceeded)
    );

    assert_eq!(
        decode_echo(
            &echo_body(),
            BodyLimits {
                max_depth: 8,
                max_table_fields: 1,
            }
        ),
        Err(BodyError::LimitExceeded)
    );

    let body = echo_body();
    let mut decoder = BodyDecoder::new(&body, 16, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    assert_eq!(
        decoder.root().table(0, 2, |_table| Ok(())),
        Err(BodyError::IncompleteTable)
    );
}

#[test]
fn primitive_and_result_encodings_are_strict() {
    let mut invalid_bool = [0; 8];
    invalid_bool[0] = 2;
    let mut decoder = BodyDecoder::new(&invalid_bool, 1, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    assert_eq!(decoder.root().read_bool(0), Err(BodyError::InvalidBool));

    let mut invalid_nan = [0; 8];
    invalid_nan[..4].copy_from_slice(&0x7fc0_0001_u32.to_le_bytes());
    let mut decoder = BodyDecoder::new(&invalid_nan, 4, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    assert_eq!(
        decoder.root().read_f32(0),
        Err(BodyError::NonCanonicalFloat)
    );

    let mut result = [0; 24];
    result[..4].copy_from_slice(&3_u32.to_le_bytes());
    let mut decoder = BodyDecoder::new(&result, 24, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    assert_eq!(
        decoder.root().closed_result(0, |_result| Ok(())),
        Err(BodyError::UnknownResultOrdinal)
    );

    let mut output = [0; 95];
    assert!(matches!(
        BodyEncoder::new(&mut output, 96, 16, BodyLimits::ENDPOINT_PROTOTYPE),
        Err(BodyError::OutputTooSmall)
    ));
}

#[test]
fn encoder_rejects_noncanonical_table_shapes() {
    let mut body = [0; 96];
    let mut encoder = BodyEncoder::new(&mut body, 96, 16, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    assert_eq!(
        encoder.root().table(0, 2, 2, |table| {
            table.field(2, 8, |value| value.write_u64(0, 1))?;
            table.field(1, 8, |value| value.write_u64(0, 2))
        }),
        Err(BodyError::EnvelopeOrder)
    );
}

#[test]
fn malformed_later_envelopes_fail_before_table_traversal() {
    let mut malformed = echo_body();
    malformed[44] = 1;
    let called = Cell::new(false);
    let mut decoder = BodyDecoder::new(&malformed, 16, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    assert_eq!(
        decoder.root().table(0, 2, |_table| {
            called.set(true);
            Ok(())
        }),
        Err(BodyError::ReservedValueUsed)
    );
    assert!(!called.get());
}

#[test]
fn every_present_envelope_consumes_semantic_depth() {
    let mut opaque = [0; 48];
    let mut encoder =
        BodyEncoder::new(&mut opaque, 48, 16, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    encoder
        .root()
        .table(0, 1, 1, |table| {
            table.field(9, 8, |value| value.write_u64(0, 1))
        })
        .unwrap();
    encoder.finish().unwrap();

    let shallow = BodyLimits {
        max_depth: 1,
        max_table_fields: 8,
    };
    let called = Cell::new(false);
    let mut decoder = BodyDecoder::new(&opaque, 16, shallow).unwrap();
    assert_eq!(
        decoder.root().table(0, 1, |_table| {
            called.set(true);
            Ok(())
        }),
        Err(BodyError::LimitExceeded)
    );
    assert!(!called.get());

    let mut unit = [0; 24];
    let mut encoder = BodyEncoder::new(&mut unit, 24, 24, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    encoder.root().closed_result_unit(0, 1).unwrap();
    encoder.finish().unwrap();
    let mut decoder = BodyDecoder::new(&unit, 24, shallow).unwrap();
    assert_eq!(
        decoder.root().closed_result(0, |_result| Ok(())),
        Err(BodyError::LimitExceeded)
    );
}

#[test]
fn failed_nested_encoding_poison_and_rollback_are_sticky() {
    let mut body = [0; 48];
    let mut encoder = BodyEncoder::new(&mut body, 48, 16, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    assert_eq!(
        encoder.root().table(0, 1, 1, |table| {
            table.field(1, 8, |value| {
                value.write_u64(0, 0xfeed_face_dead_beef)?;
                Err(BodyError::InvalidOrdinal)
            })
        }),
        Err(BodyError::InvalidOrdinal)
    );
    assert_eq!(encoder.finish(), Err(BodyError::Poisoned));
    assert!(body[16..].iter().all(|byte| *byte == 0));
}

#[test]
fn arbitrary_input_smoke_test_never_panics() {
    for length in 0..=256 {
        let mut bytes = [0_u8; 256];
        for (index, byte) in bytes[..length].iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(37).wrapping_add(length as u8);
        }
        let input = &bytes[..length];
        if let Ok(mut decoder) = BodyDecoder::new(input, 16, BodyLimits::DESKTOP) {
            let _ = decoder.root().table(0, 32, |table| {
                while let Some(_field) = table.next_field()? {}
                Ok(())
            });
        }
    }
}
