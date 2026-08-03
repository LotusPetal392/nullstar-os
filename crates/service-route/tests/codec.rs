use service_route::{
    DecodeError, ProviderGeneration, RoleId, RouteFailure, RouteKey, RouteMessage,
    SERVICE_ROUTE_WIRE_BYTES, ServiceId, ServiceIdError,
};

const UUID: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
];
const REQUEST: [u8; SERVICE_ROUTE_WIRE_BYTES] = [
    0x4e, 0x53, 0x52, 0x54, 0x01, 0x00, 0x01, 0x00, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77,
    0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x04, 0x03, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const ACCEPTED: [u8; SERVICE_ROUTE_WIRE_BYTES] = [
    0x4e, 0x53, 0x52, 0x54, 0x01, 0x00, 0x02, 0x00, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77,
    0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x04, 0x03, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00,
    0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
];
const FAILURE: [u8; SERVICE_ROUTE_WIRE_BYTES] = [
    0x4e, 0x53, 0x52, 0x54, 0x01, 0x00, 0x03, 0x03, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77,
    0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x04, 0x03, 0x02, 0x01, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

fn key() -> RouteKey {
    RouteKey::new(
        ServiceId::from_bytes(UUID).unwrap(),
        RoleId::new(0x0102_0304).unwrap(),
    )
}

#[test]
fn all_message_kinds_match_literal_golden_bytes() {
    let request = RouteMessage::Request { key: key() };
    let accepted = RouteMessage::Accepted {
        key: key(),
        generation: ProviderGeneration::new(0x0102_0304_0506_0708).unwrap(),
    };
    let failure = RouteMessage::Failure {
        key: key(),
        failure: RouteFailure::IssuerCapacity,
    };
    assert_eq!(request.encode(), REQUEST);
    assert_eq!(accepted.encode(), ACCEPTED);
    assert_eq!(failure.encode(), FAILURE);
    assert_eq!(RouteMessage::decode(&REQUEST), Ok(request));
    assert_eq!(RouteMessage::decode(&ACCEPTED), Ok(accepted));
    assert_eq!(RouteMessage::decode(&FAILURE), Ok(failure));
}

#[test]
fn framing_identity_and_reserved_bytes_are_strict() {
    assert_eq!(
        RouteMessage::decode(&REQUEST[..39]),
        Err(DecodeError::InvalidLength)
    );
    let mut extended = REQUEST.to_vec();
    extended.push(0);
    assert_eq!(
        RouteMessage::decode(&extended),
        Err(DecodeError::InvalidLength)
    );

    let mut bytes = REQUEST;
    bytes[0] ^= 1;
    assert_eq!(RouteMessage::decode(&bytes), Err(DecodeError::InvalidMagic));
    let mut bytes = REQUEST;
    bytes[4] = 2;
    assert_eq!(
        RouteMessage::decode(&bytes),
        Err(DecodeError::UnsupportedVersion)
    );
    let mut bytes = REQUEST;
    bytes[5] = 1;
    assert_eq!(
        RouteMessage::decode(&bytes),
        Err(DecodeError::UnsupportedVersion)
    );
    let mut bytes = REQUEST;
    bytes[6] = 0;
    assert_eq!(RouteMessage::decode(&bytes), Err(DecodeError::UnknownKind));
    let mut bytes = REQUEST;
    bytes[7] = 4;
    assert_eq!(
        RouteMessage::decode(&bytes),
        Err(DecodeError::UnknownStatus)
    );

    for index in [28, 29, 30, 31] {
        let mut bytes = REQUEST;
        bytes[index] = 1;
        assert_eq!(
            RouteMessage::decode(&bytes),
            Err(DecodeError::NonzeroReserved)
        );
    }
}

#[test]
fn identity_fields_receive_semantic_validation() {
    let mut bytes = REQUEST;
    bytes[8..24].fill(0);
    assert_eq!(
        RouteMessage::decode(&bytes),
        Err(DecodeError::InvalidServiceId(ServiceIdError::Nil))
    );
    let mut bytes = REQUEST;
    bytes[14] = 0x16;
    assert_eq!(
        RouteMessage::decode(&bytes),
        Err(DecodeError::InvalidServiceId(
            ServiceIdError::InvalidVersion
        ))
    );
    let mut bytes = REQUEST;
    bytes[16] = 0x40;
    assert_eq!(
        RouteMessage::decode(&bytes),
        Err(DecodeError::InvalidServiceId(
            ServiceIdError::InvalidVariant
        ))
    );
    let mut bytes = REQUEST;
    bytes[24..28].fill(0);
    assert_eq!(RouteMessage::decode(&bytes), Err(DecodeError::ZeroRole));
}

#[test]
fn request_accepted_and_failure_relationships_are_canonical() {
    for status in 1..=3 {
        let mut bytes = REQUEST;
        bytes[7] = status;
        assert_eq!(
            RouteMessage::decode(&bytes),
            Err(DecodeError::StatusNotAllowed)
        );
    }
    let mut bytes = REQUEST;
    bytes[32] = 1;
    assert_eq!(
        RouteMessage::decode(&bytes),
        Err(DecodeError::GenerationNotAllowed)
    );

    let mut bytes = ACCEPTED;
    bytes[7] = 1;
    assert_eq!(
        RouteMessage::decode(&bytes),
        Err(DecodeError::StatusNotAllowed)
    );
    let mut bytes = ACCEPTED;
    bytes[32..40].fill(0);
    assert_eq!(
        RouteMessage::decode(&bytes),
        Err(DecodeError::GenerationRequired)
    );

    let mut bytes = FAILURE;
    bytes[7] = 0;
    assert_eq!(
        RouteMessage::decode(&bytes),
        Err(DecodeError::StatusRequired)
    );
    let mut bytes = FAILURE;
    bytes[39] = 1;
    assert_eq!(
        RouteMessage::decode(&bytes),
        Err(DecodeError::GenerationNotAllowed)
    );

    for (status, failure) in [
        (1, RouteFailure::Unauthorized),
        (2, RouteFailure::Unavailable),
        (3, RouteFailure::IssuerCapacity),
    ] {
        let mut bytes = FAILURE;
        bytes[7] = status;
        assert_eq!(
            RouteMessage::decode(&bytes),
            Ok(RouteMessage::Failure {
                key: key(),
                failure
            })
        );
    }
}
