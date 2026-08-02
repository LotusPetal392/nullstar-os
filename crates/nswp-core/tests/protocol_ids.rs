use nswp_core::{ProtocolId, ProtocolIdError};

const CANONICAL: &str = "00112233-4455-4677-8899-aabbccddeeff";
const RFC_BYTES: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
];

#[test]
fn canonical_uuid_v4_round_trips_rfc_order() {
    let id = ProtocolId::parse(CANONICAL).unwrap();
    assert_eq!(id.into_bytes(), RFC_BYTES);
    assert_eq!(id.to_string(), CANONICAL);
    assert_eq!(CANONICAL.parse::<ProtocolId>().unwrap(), id);
}

#[test]
fn source_spelling_and_uuid_class_are_strict() {
    assert_eq!(
        ProtocolId::parse("00112233-4455-4677-8899-AABBCCDDEEFF"),
        Err(ProtocolIdError::NonCanonical)
    );
    assert_eq!(
        ProtocolId::parse("00112233445546778899aabbccddeeff"),
        Err(ProtocolIdError::InvalidLength)
    );
    assert_eq!(
        ProtocolId::parse("00112233-4455-1677-8899-aabbccddeeff"),
        Err(ProtocolIdError::InvalidVersion)
    );
    assert_eq!(
        ProtocolId::parse("00112233-4455-7677-8899-aabbccddeeff"),
        Err(ProtocolIdError::InvalidVersion)
    );
    assert_eq!(
        ProtocolId::parse("00112233-4455-4677-4899-aabbccddeeff"),
        Err(ProtocolIdError::InvalidVariant)
    );
    assert_eq!(ProtocolId::from_bytes([0; 16]), Err(ProtocolIdError::Nil));
    assert_eq!(
        ProtocolId::from_bytes([u8::MAX; 16]),
        Err(ProtocolIdError::AllOnes)
    );
}

#[test]
fn windows_guid_permutation_is_not_rfc_identity() {
    let rfc = ProtocolId::parse("00112233-4455-4644-8899-aabbccddeeff").unwrap();
    let windows_mixed_endian = [
        0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x44, 0x46, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let mixed = ProtocolId::from_bytes(windows_mixed_endian).unwrap();
    assert_ne!(mixed, rfc);
}

#[test]
fn ordering_is_bytewise() {
    let first = ProtocolId::parse("00112233-4455-4677-8899-aabbccddeeff").unwrap();
    let second = ProtocolId::parse("10112233-4455-4677-8899-aabbccddeeff").unwrap();
    assert!(first < second);
}
