use service_route::{
    ProviderGeneration, ProviderGenerationExhausted, ProviderGenerationSequence,
    SERVICE_GENERATION_MAGIC, SERVICE_GENERATION_VERSION, SERVICE_GENERATION_WIRE_BYTES,
    ServiceGenerationDecodeError, ServiceGenerationHandoff,
};

const GOLDEN: [u8; SERVICE_GENERATION_WIRE_BYTES] = [
    0x4e, 0x53, 0x47, 0x4e, 0x01, 0x00, 0x00, 0x00, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
];

fn generation(value: u64) -> ProviderGeneration {
    ProviderGeneration::new(value).unwrap()
}

#[test]
fn handoff_matches_literal_golden_bytes() {
    assert_eq!(SERVICE_GENERATION_MAGIC, *b"NSGN");
    assert_eq!(SERVICE_GENERATION_VERSION, 1);
    assert_eq!(SERVICE_GENERATION_WIRE_BYTES, 16);

    let handoff = ServiceGenerationHandoff::new(generation(0x0102_0304_0506_0708));
    assert_eq!(handoff.generation(), generation(0x0102_0304_0506_0708));
    assert_eq!(handoff.encode(), GOLDEN);
    assert_eq!(ServiceGenerationHandoff::decode(&GOLDEN), Ok(handoff));
}

#[test]
fn handoff_rejects_invalid_framing_and_values() {
    assert_eq!(
        ServiceGenerationHandoff::decode(&GOLDEN[..15]),
        Err(ServiceGenerationDecodeError::InvalidLength)
    );
    let mut extended = GOLDEN.to_vec();
    extended.push(0);
    assert_eq!(
        ServiceGenerationHandoff::decode(&extended),
        Err(ServiceGenerationDecodeError::InvalidLength)
    );

    let mut bytes = GOLDEN;
    bytes[0] ^= 1;
    assert_eq!(
        ServiceGenerationHandoff::decode(&bytes),
        Err(ServiceGenerationDecodeError::InvalidMagic)
    );

    for (index, value) in [(4, 2), (5, 1)] {
        let mut bytes = GOLDEN;
        bytes[index] = value;
        assert_eq!(
            ServiceGenerationHandoff::decode(&bytes),
            Err(ServiceGenerationDecodeError::UnsupportedVersion)
        );
    }

    for index in 6..8 {
        let mut bytes = GOLDEN;
        bytes[index] = 1;
        assert_eq!(
            ServiceGenerationHandoff::decode(&bytes),
            Err(ServiceGenerationDecodeError::NonzeroReserved)
        );
    }

    let mut bytes = GOLDEN;
    bytes[8..16].fill(0);
    assert_eq!(
        ServiceGenerationHandoff::decode(&bytes),
        Err(ServiceGenerationDecodeError::ZeroGeneration)
    );
}

#[test]
fn sequence_starts_at_one_and_strictly_increments() {
    let mut sequence = ProviderGenerationSequence::new();
    assert_eq!(sequence.last_issued(), None);
    for expected in 1..=4 {
        let issued = sequence.next_generation().unwrap();
        assert_eq!(issued, generation(expected));
        assert_eq!(sequence.last_issued(), Some(issued));
    }

    let mut default_sequence = ProviderGenerationSequence::default();
    assert_eq!(default_sequence.next_generation(), Ok(generation(1)));
}

#[test]
fn sequence_resumes_strictly_after_existing_generation() {
    let handoff = ServiceGenerationHandoff::decode(&GOLDEN).unwrap();
    let mut sequence = ProviderGenerationSequence::after(handoff.generation());
    assert_eq!(sequence.last_issued(), Some(handoff.generation()));
    assert_eq!(
        sequence.next_generation(),
        Ok(generation(0x0102_0304_0506_0709))
    );
}

#[test]
fn sequence_issues_max_once_then_remains_exhausted() {
    let mut sequence = ProviderGenerationSequence::after(generation(u64::MAX - 1));
    assert_eq!(sequence.next_generation(), Ok(generation(u64::MAX)));
    assert_eq!(sequence.next_generation(), Err(ProviderGenerationExhausted));
    assert_eq!(sequence.next_generation(), Err(ProviderGenerationExhausted));
    assert_eq!(sequence.last_issued(), Some(generation(u64::MAX)));

    let mut already_exhausted = ProviderGenerationSequence::after(generation(u64::MAX));
    assert_eq!(
        already_exhausted.next_generation(),
        Err(ProviderGenerationExhausted)
    );
}
