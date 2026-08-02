use std::cell::Cell;

use nswp_core::{BodyDecoder, BodyEncoder, BodyError, BodyLimits, ENVELOPE_BYTES, SLICE_REF_BYTES};

fn limits(max_depth: u8) -> BodyLimits {
    BodyLimits {
        max_depth,
        max_table_fields: 8,
    }
}

fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[test]
fn vector_u64_matches_literal_encoding() {
    const EXPECTED: [u8; 40] = [
        0x10, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33,
        0x22, 0x11, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];

    let mut body = [0xa5; 40];
    let mut encoder = BodyEncoder::new(&mut body, 40, SLICE_REF_BYTES, limits(4)).unwrap();
    encoder
        .root()
        .vector(0, 3, 3, 8, 8, |vector| {
            vector.element(|element| element.write_u64(0, 1))?;
            vector.element(|element| element.write_u64(0, 0x1122_3344_5566_7788))?;
            vector.element(|element| element.write_u64(0, u64::MAX))
        })
        .unwrap();
    encoder.finish().unwrap();

    assert_eq!(body, EXPECTED);
}

#[test]
fn vector_string_array_precedes_all_descendants() {
    const EXPECTED: [u8; 64] = [
        0x10, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x20, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x18, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, b'a', 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, b'b', b'c', 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];

    let mut body = [0xa5; 64];
    let mut encoder = BodyEncoder::new(&mut body, 64, SLICE_REF_BYTES, limits(4)).unwrap();
    encoder
        .root()
        .vector(0, 2, 2, SLICE_REF_BYTES, 4, |vector| {
            vector.element(|element| element.string(0, 8, "a"))?;
            vector.element(|element| element.string(0, 8, "bc"))
        })
        .unwrap();
    encoder.finish().unwrap();

    assert_eq!(body, EXPECTED);
}

#[test]
fn nested_vectors_advance_one_dynamic_cursor_in_index_order() {
    let mut body = [0xa5; 72];
    let mut encoder = BodyEncoder::new(&mut body, 72, SLICE_REF_BYTES, limits(4)).unwrap();
    encoder
        .root()
        .vector(0, 2, 2, SLICE_REF_BYTES, 4, |outer| {
            assert_eq!(outer.element_count(), 2);
            assert_eq!(outer.next_index(), 0);
            outer.element(|element| {
                element.vector(0, 2, 2, 8, 8, |inner| {
                    inner.element(|value| value.write_u64(0, 10))?;
                    inner.element(|value| value.write_u64(0, 11))
                })
            })?;
            assert_eq!(outer.next_index(), 1);
            outer.element(|element| {
                element.vector(0, 1, 1, 8, 8, |inner| {
                    inner.element(|value| value.write_u64(0, 20))
                })
            })
        })
        .unwrap();
    encoder.finish().unwrap();

    assert_eq!(
        (get_u32(&body, 0), get_u32(&body, 4), get_u32(&body, 8)),
        (16, 2, 56)
    );
    assert_eq!(
        (get_u32(&body, 16), get_u32(&body, 20), get_u32(&body, 24)),
        (32, 2, 16)
    );
    assert_eq!(
        (get_u32(&body, 32), get_u32(&body, 36), get_u32(&body, 40)),
        (32, 1, 8)
    );
    assert_eq!(u64::from_le_bytes(body[48..56].try_into().unwrap()), 10);
    assert_eq!(u64::from_le_bytes(body[56..64].try_into().unwrap()), 11);
    assert_eq!(u64::from_le_bytes(body[64..72].try_into().unwrap()), 20);
}

#[test]
fn empty_vector_is_an_all_zero_reference() {
    let mut body = [0xa5; SLICE_REF_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut body, SLICE_REF_BYTES, SLICE_REF_BYTES, limits(1)).unwrap();
    encoder
        .root()
        .vector(0, 0, 0, 8, 8, |vector| {
            assert_eq!(vector.element_count(), 0);
            assert_eq!(vector.next_index(), 0);
            Ok(())
        })
        .unwrap();
    encoder.finish().unwrap();

    assert_eq!(body, [0; SLICE_REF_BYTES]);
}

fn rejected_layout(inline_bytes: usize, alignment: usize) -> BodyError {
    let mut body = [0; SLICE_REF_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut body, SLICE_REF_BYTES, SLICE_REF_BYTES, limits(4)).unwrap();
    encoder
        .root()
        .vector(0, 0, 0, inline_bytes, alignment, |_| Ok(()))
        .unwrap_err()
}

#[test]
fn vector_rejects_max_count_zero_size_bad_alignment_and_overflow() {
    let called = Cell::new(false);
    let mut body = [0; SLICE_REF_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut body, SLICE_REF_BYTES, SLICE_REF_BYTES, limits(4)).unwrap();
    assert_eq!(
        encoder.root().vector(0, 2, 1, 8, 8, |_| {
            called.set(true);
            Ok(())
        }),
        Err(BodyError::LimitExceeded)
    );
    assert!(!called.get());
    encoder.finish().unwrap();

    assert_eq!(rejected_layout(0, 1), BodyError::InvalidElementLayout);
    assert_eq!(rejected_layout(8, 0), BodyError::InvalidElementLayout);
    assert_eq!(rejected_layout(8, 3), BodyError::InvalidElementLayout);
    assert_eq!(rejected_layout(8, 16), BodyError::InvalidElementLayout);
    assert_eq!(
        rejected_layout(usize::MAX, 8),
        BodyError::ArithmeticOverflow
    );
}

#[test]
fn vector_requires_exactly_the_declared_element_count() {
    let mut missing = [0xa5; 32];
    let mut encoder = BodyEncoder::new(&mut missing, 32, SLICE_REF_BYTES, limits(4)).unwrap();
    assert_eq!(
        encoder.root().vector(0, 2, 2, 8, 8, |vector| {
            vector.element(|element| element.write_u64(0, 1))
        }),
        Err(BodyError::IncompleteVector)
    );
    assert_eq!(encoder.finish(), Err(BodyError::Poisoned));
    assert!(missing[SLICE_REF_BYTES..].iter().all(|byte| *byte == 0));

    let mut extra = [0xa5; 24];
    let mut encoder = BodyEncoder::new(&mut extra, 24, SLICE_REF_BYTES, limits(4)).unwrap();
    assert_eq!(
        encoder.root().vector(0, 1, 1, 8, 8, |vector| {
            vector.element(|element| element.write_u64(0, 1))?;
            vector.element(|element| element.write_u64(0, 2))
        }),
        Err(BodyError::IncompleteVector)
    );
    assert_eq!(encoder.finish(), Err(BodyError::Poisoned));
    assert!(extra[SLICE_REF_BYTES..].iter().all(|byte| *byte == 0));
}

#[test]
fn optional_none_some_unit_and_some_string_are_canonical() {
    let mut none = [0xa5; ENVELOPE_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut none, ENVELOPE_BYTES, ENVELOPE_BYTES, limits(2)).unwrap();
    encoder.root().optional_none(0).unwrap();
    encoder.finish().unwrap();
    assert_eq!(none, [0; ENVELOPE_BYTES]);

    let mut unit = [0xa5; ENVELOPE_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut unit, ENVELOPE_BYTES, ENVELOPE_BYTES, limits(2)).unwrap();
    encoder.root().optional_some_unit(0).unwrap();
    encoder.finish().unwrap();
    assert_eq!(get_u32(&unit, 0), 1);
    assert!(unit[4..].iter().all(|byte| *byte == 0));

    let mut string = [0xa5; 48];
    let mut encoder = BodyEncoder::new(&mut string, 48, ENVELOPE_BYTES, limits(3)).unwrap();
    encoder
        .root()
        .optional_some(0, SLICE_REF_BYTES, |payload| payload.string(0, 8, "ok"))
        .unwrap();
    encoder.finish().unwrap();
    assert_eq!(
        (
            get_u32(&string, 0),
            get_u32(&string, 8),
            get_u32(&string, 12)
        ),
        (1, 16, 24)
    );
    assert_eq!(
        (
            get_u32(&string, 24),
            get_u32(&string, 28),
            get_u32(&string, 32)
        ),
        (16, 2, 8)
    );
    assert_eq!(&string[40..42], b"ok");

    let mut decoder = BodyDecoder::new(&string, ENVELOPE_BYTES, limits(3)).unwrap();
    let decoded = decoder
        .root()
        .closed_result(0, |result| {
            result.decode(SLICE_REF_BYTES, |payload| payload.string(0, 8))
        })
        .unwrap();
    decoder.finish().unwrap();
    assert_eq!(decoded, "ok");
}

#[test]
fn general_closed_union_accepts_any_nonzero_unit_or_payload_ordinal() {
    let mut unit = [0xa5; ENVELOPE_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut unit, ENVELOPE_BYTES, ENVELOPE_BYTES, limits(2)).unwrap();
    encoder.root().closed_union_unit(0, 7).unwrap();
    encoder.finish().unwrap();
    assert_eq!(get_u32(&unit, 0), 7);
    assert!(unit[4..].iter().all(|byte| *byte == 0));

    let mut payload = [0xa5; 32];
    let mut encoder = BodyEncoder::new(&mut payload, 32, ENVELOPE_BYTES, limits(2)).unwrap();
    encoder
        .root()
        .closed_union(0, 9, 8, |value| value.write_u64(0, 0x1122_3344_5566_7788))
        .unwrap();
    encoder.finish().unwrap();
    assert_eq!(
        (
            get_u32(&payload, 0),
            get_u32(&payload, 8),
            get_u32(&payload, 12)
        ),
        (9, 16, 8)
    );
    assert_eq!(
        u64::from_le_bytes(payload[24..32].try_into().unwrap()),
        0x1122_3344_5566_7788
    );

    let mut invalid = [0; ENVELOPE_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut invalid, ENVELOPE_BYTES, ENVELOPE_BYTES, limits(2)).unwrap();
    assert_eq!(
        encoder.root().closed_union_unit(0, 0),
        Err(BodyError::InvalidOrdinal)
    );
    assert_eq!(
        encoder.root().closed_result_unit(0, 3),
        Err(BodyError::UnknownResultOrdinal)
    );
    encoder.finish().unwrap();
}

#[test]
fn vector_optional_and_union_enforce_semantic_depth() {
    let mut vector_body = [0; 24];
    let mut encoder = BodyEncoder::new(&mut vector_body, 24, SLICE_REF_BYTES, limits(1)).unwrap();
    assert_eq!(
        encoder.root().vector(0, 1, 1, 8, 8, |vector| {
            vector.element(|value| value.write_u64(0, 1))
        }),
        Err(BodyError::LimitExceeded)
    );
    assert_eq!(encoder.finish(), Err(BodyError::Poisoned));
    assert!(vector_body[SLICE_REF_BYTES..].iter().all(|byte| *byte == 0));

    let called = Cell::new(false);
    let mut nested_body = [0; 40];
    let mut encoder = BodyEncoder::new(&mut nested_body, 40, SLICE_REF_BYTES, limits(2)).unwrap();
    assert_eq!(
        encoder.root().vector(0, 1, 1, SLICE_REF_BYTES, 4, |outer| {
            outer.element(|element| {
                element.vector(0, 1, 1, 8, 8, |inner| {
                    inner.element(|value| {
                        called.set(true);
                        value.write_u64(0, 1)
                    })
                })
            })
        }),
        Err(BodyError::LimitExceeded)
    );
    assert!(!called.get());
    assert_eq!(encoder.finish(), Err(BodyError::Poisoned));

    let mut optional = [0; ENVELOPE_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut optional, ENVELOPE_BYTES, ENVELOPE_BYTES, limits(1)).unwrap();
    assert_eq!(
        encoder.root().optional_some_unit(0),
        Err(BodyError::LimitExceeded)
    );
    encoder.finish().unwrap();
    assert_eq!(optional, [0; ENVELOPE_BYTES]);

    let called = Cell::new(false);
    let mut union = [0; ENVELOPE_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut union, ENVELOPE_BYTES, ENVELOPE_BYTES, limits(1)).unwrap();
    assert_eq!(
        encoder.root().closed_union(0, 3, 8, |value| {
            called.set(true);
            value.write_u64(0, 1)
        }),
        Err(BodyError::LimitExceeded)
    );
    assert!(!called.get());
    encoder.finish().unwrap();
    assert_eq!(union, [0; ENVELOPE_BYTES]);
}

#[test]
fn recoverable_preflight_errors_preserve_existing_values() {
    let mut vector_body = [0; 24];
    let mut encoder = BodyEncoder::new(&mut vector_body, 24, SLICE_REF_BYTES, limits(2)).unwrap();
    encoder.root().bytes(0, 8, b"x").unwrap();
    assert_eq!(
        encoder.root().vector(0, 2, 1, 8, 8, |_| Ok(())),
        Err(BodyError::LimitExceeded)
    );
    encoder.finish().unwrap();
    assert_eq!(
        (
            get_u32(&vector_body, 0),
            get_u32(&vector_body, 4),
            get_u32(&vector_body, 8)
        ),
        (16, 1, 8)
    );
    assert_eq!(vector_body[16], b'x');

    let mut union_body = [0; ENVELOPE_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut union_body, ENVELOPE_BYTES, ENVELOPE_BYTES, limits(1)).unwrap();
    encoder.root().closed_union_unit(0, 7).unwrap_err();
    encoder.finish().unwrap();
    assert_eq!(union_body, [0; ENVELOPE_BYTES]);

    let mut existing = [0; ENVELOPE_BYTES];
    let mut encoder =
        BodyEncoder::new(&mut existing, ENVELOPE_BYTES, ENVELOPE_BYTES, limits(2)).unwrap();
    encoder.root().closed_union_unit(0, 7).unwrap();
    assert_eq!(
        encoder.root().closed_union(0, 2, 64, |_| Ok(())),
        Err(BodyError::OutputTooSmall)
    );
    encoder.finish().unwrap();
    assert_eq!(u32::from_le_bytes(existing[..4].try_into().unwrap()), 7);
    assert!(existing[4..].iter().all(|byte| *byte == 0));
}

#[test]
fn failed_vector_and_element_callbacks_poison_and_rollback() {
    let mut callback_failure = [0xa5; 32];
    let mut encoder =
        BodyEncoder::new(&mut callback_failure, 32, SLICE_REF_BYTES, limits(3)).unwrap();
    assert_eq!(
        encoder.root().vector(0, 2, 2, 8, 8, |vector| {
            vector.element(|value| value.write_u64(0, 0xfeed_face_dead_beef))?;
            Err(BodyError::InvalidOrdinal)
        }),
        Err(BodyError::InvalidOrdinal)
    );
    assert_eq!(encoder.finish(), Err(BodyError::Poisoned));
    assert_eq!(callback_failure, [0; 32]);

    let mut element_failure = [0xa5; 24];
    let mut encoder =
        BodyEncoder::new(&mut element_failure, 24, SLICE_REF_BYTES, limits(3)).unwrap();
    assert_eq!(
        encoder.root().vector(0, 1, 1, 8, 8, |vector| {
            assert_eq!(
                vector.element(|value| {
                    value.write_u64(0, 0xfeed_face_dead_beef)?;
                    Err(BodyError::InvalidOrdinal)
                }),
                Err(BodyError::InvalidOrdinal)
            );
            Ok(())
        }),
        Err(BodyError::Poisoned)
    );
    assert_eq!(encoder.finish(), Err(BodyError::Poisoned));
    assert_eq!(element_failure, [0; 24]);
}

#[test]
fn failed_optional_or_union_payload_callback_poison_and_rollback() {
    let mut body = [0xa5; 32];
    let mut encoder = BodyEncoder::new(&mut body, 32, ENVELOPE_BYTES, limits(2)).unwrap();
    assert_eq!(
        encoder.root().optional_some(0, 8, |value| {
            value.write_u64(0, 0xfeed_face_dead_beef)?;
            Err(BodyError::InvalidOrdinal)
        }),
        Err(BodyError::InvalidOrdinal)
    );
    assert_eq!(encoder.finish(), Err(BodyError::Poisoned));
    assert_eq!(body, [0; 32]);
}
