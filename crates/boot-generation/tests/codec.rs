use boot_generation::{
    DecodeError, GenerationId, Health, RetainedGeneration, SELECTION_CHECKSUM_OFFSET,
    SELECTION_RECORD_BYTES, Selection, SelectionError, SelectionSequence, Slot,
};
use nullfs_format::crc32c;

fn generation(value: u64, slot: Slot, health: Health, artifact_crc32c: u32) -> RetainedGeneration {
    RetainedGeneration::new(
        GenerationId::new(value).unwrap(),
        slot,
        health,
        artifact_crc32c,
    )
}

fn representative_selection() -> Selection {
    Selection::new(
        SelectionSequence::new(0x0102_0304_0506_0708).unwrap(),
        generation(
            0x1112_1314_1516_1718,
            Slot::One,
            Health::Pending,
            0x99aa_bbcc,
        ),
        Some(generation(
            0x2122_2324_2526_2728,
            Slot::Zero,
            Health::Healthy,
            0xddee_ff00,
        )),
    )
    .unwrap()
}

fn reseal(bytes: &mut [u8; SELECTION_RECORD_BYTES]) {
    bytes[SELECTION_CHECKSUM_OFFSET..SELECTION_RECORD_BYTES].fill(0);
    let checksum = crc32c(bytes);
    bytes[SELECTION_CHECKSUM_OFFSET..SELECTION_RECORD_BYTES]
        .copy_from_slice(&checksum.to_le_bytes());
}

#[test]
fn representative_record_matches_literal_golden_bytes() {
    const GOLDEN: [u8; SELECTION_RECORD_BYTES] = [
        0x4e, 0x53, 0x42, 0x53, 0x45, 0x4c, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00,
        0x00, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13,
        0x12, 0x11, 0x28, 0x27, 0x26, 0x25, 0x24, 0x23, 0x22, 0x21, 0xcc, 0xbb, 0xaa, 0x99, 0x00,
        0xff, 0xee, 0xdd, 0x01, 0x00, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0xe0, 0x04, 0xde, 0x4e,
    ];

    assert_eq!(representative_selection().encode(), GOLDEN);
    assert_eq!(Selection::decode(&GOLDEN), Ok(representative_selection()));
}

#[test]
fn initial_selection_has_one_canonical_representation() {
    let selection = Selection::new(
        SelectionSequence::new(1).unwrap(),
        generation(1, Slot::Zero, Health::Healthy, 0),
        None,
    )
    .unwrap();
    let bytes = selection.encode();

    assert_eq!(&bytes[32..40], &[0; 8]);
    assert_eq!(&bytes[44..48], &[0; 4]);
    assert_eq!(bytes[49], 0);
    assert_eq!(bytes[51], 0);
    assert_eq!(Selection::decode(&bytes), Ok(selection));
    assert_eq!(selection.encode(), selection.encode());
}

#[test]
fn framing_checksum_and_reserved_bytes_are_strict() {
    let valid = representative_selection().encode();
    assert_eq!(
        Selection::decode(&valid[..63]),
        Err(DecodeError::InvalidLength)
    );
    let mut extended = valid.to_vec();
    extended.push(0);
    assert_eq!(
        Selection::decode(&extended),
        Err(DecodeError::InvalidLength)
    );

    for (offset, value, error) in [
        (0, 0, DecodeError::InvalidMagic),
        (8, 2, DecodeError::UnsupportedMajorVersion(2)),
        (10, 1, DecodeError::UnsupportedMinorVersion(1)),
        (12, 63, DecodeError::InvalidHeaderSize(63)),
        (14, 1, DecodeError::NonzeroFlags),
    ] {
        let mut bytes = valid;
        bytes[offset] = value;
        reseal(&mut bytes);
        assert_eq!(Selection::decode(&bytes), Err(error));
    }

    let mut corrupted = valid;
    corrupted[24] ^= 0x80;
    assert!(matches!(
        Selection::decode(&corrupted),
        Err(DecodeError::ChecksumMismatch { .. })
    ));

    for offset in 52..60 {
        let mut bytes = valid;
        bytes[offset] = 1;
        reseal(&mut bytes);
        assert_eq!(Selection::decode(&bytes), Err(DecodeError::NonzeroReserved));
    }
}

#[test]
fn identities_slots_health_and_optional_previous_are_strict() {
    let valid = representative_selection().encode();

    let mutations = [
        (16, 0, DecodeError::ZeroSequence),
        (24, 0, DecodeError::ZeroSelectedGeneration),
        (48, 2, DecodeError::UnknownSelectedSlot(2)),
        (50, 0, DecodeError::UnknownSelectedHealth(0)),
        (50, 4, DecodeError::UnknownSelectedHealth(4)),
        (49, 2, DecodeError::UnknownPreviousSlot(2)),
        (51, 4, DecodeError::UnknownPreviousHealth(4)),
    ];
    for (offset, value, error) in mutations {
        let mut bytes = valid;
        if offset == 16 || offset == 24 {
            bytes[offset..offset + 8].fill(0);
        } else {
            bytes[offset] = value;
        }
        reseal(&mut bytes);
        assert_eq!(Selection::decode(&bytes), Err(error));
    }

    let mut missing_previous_health = valid;
    missing_previous_health[51] = 0;
    reseal(&mut missing_previous_health);
    assert_eq!(
        Selection::decode(&missing_previous_health),
        Err(DecodeError::PreviousGenerationRequired)
    );

    for offset in [44, 49, 51] {
        let mut bytes = valid;
        bytes[32..40].fill(0);
        bytes[44..48].fill(0);
        bytes[49] = 0;
        bytes[51] = 0;
        bytes[offset] = 1;
        reseal(&mut bytes);
        assert_eq!(
            Selection::decode(&bytes),
            Err(DecodeError::UnexpectedPreviousGenerationData)
        );
    }
}

#[test]
fn retained_generations_must_be_distinct() {
    let sequence = SelectionSequence::new(1).unwrap();
    let selected = generation(1, Slot::Zero, Health::Healthy, 1);

    assert_eq!(
        Selection::new(
            sequence,
            selected,
            Some(generation(1, Slot::One, Health::Failed, 2))
        ),
        Err(SelectionError::DuplicateGeneration)
    );
    assert_eq!(
        Selection::new(
            sequence,
            selected,
            Some(generation(2, Slot::Zero, Health::Failed, 2))
        ),
        Err(SelectionError::DuplicateSlot)
    );

    let mut duplicate_generation = representative_selection().encode();
    duplicate_generation[32..40].copy_from_slice(&0x1112_1314_1516_1718_u64.to_le_bytes());
    reseal(&mut duplicate_generation);
    assert_eq!(
        Selection::decode(&duplicate_generation),
        Err(DecodeError::InvalidSelection(
            SelectionError::DuplicateGeneration
        ))
    );

    let mut duplicate_slot = representative_selection().encode();
    duplicate_slot[49] = Slot::One as u8;
    reseal(&mut duplicate_slot);
    assert_eq!(
        Selection::decode(&duplicate_slot),
        Err(DecodeError::InvalidSelection(SelectionError::DuplicateSlot))
    );
}

#[test]
fn sequences_never_wrap() {
    let last = SelectionSequence::new(u64::MAX).unwrap();
    assert_eq!(last.checked_next(), None);
    assert_eq!(
        SelectionSequence::new(1)
            .unwrap()
            .checked_next()
            .unwrap()
            .get(),
        2
    );
}
