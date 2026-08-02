use nswp_core::{
    AvailableFeature, ConnectionLimits, DecodeError, EncodeError, FeatureRecord,
    MinorVersionProfile, NEGOTIATE_REQUEST_ROOT_BYTES, NEGOTIATE_RESPONSE_ROOT_BYTES,
    NegotiationError, NegotiationRequest, NegotiationResponse, NegotiationStatus, ProtocolId,
    SelectedFeatures, ServerProfile, negotiate,
};

const ID_TEXT: &str = "00112233-4455-4677-8899-aabbccddeeff";
const VERSIONS: &[MinorVersionProfile] = &[
    MinorVersionProfile {
        minor: 2,
        minimum_body_bytes: 64,
        minimum_handles: 0,
    },
    MinorVersionProfile {
        minor: 3,
        minimum_body_bytes: 128,
        minimum_handles: 1,
    },
    MinorVersionProfile {
        minor: 4,
        minimum_body_bytes: 256,
        minimum_handles: 2,
    },
];
const DISJOINT_VERSIONS: &[MinorVersionProfile] = &[MinorVersionProfile {
    minor: 4,
    minimum_body_bytes: 64,
    minimum_handles: 0,
}];
const AVAILABLE_FEATURES: &[AvailableFeature] = &[
    AvailableFeature {
        id: 1,
        since_minor: 2,
    },
    AvailableFeature {
        id: 3,
        since_minor: 3,
    },
];

fn feature_set_fits(minor: u16, features: &dyn SelectedFeatures, limits: ConnectionLimits) -> bool {
    let (mut body_bytes, mut handles) = match minor {
        2 => (64, 0),
        3 => (128, 1),
        4 => (256, 2),
        _ => return false,
    };
    if features.contains(1) && features.contains(3) {
        body_bytes = body_bytes.max(512);
        handles = handles.max(3);
    }
    body_bytes <= limits.max_body_bytes && handles <= limits.max_handles
}

fn protocol_id() -> ProtocolId {
    ProtocolId::parse(ID_TEXT).unwrap()
}

fn request() -> NegotiationRequest {
    NegotiationRequest {
        protocol_id: protocol_id(),
        protocol_major: 2,
        min_minor: 1,
        max_minor: 3,
        max_body_bytes: 0x1000,
        max_handles: 4,
        max_outstanding: 8,
    }
}

fn profile() -> ServerProfile<'static> {
    ServerProfile {
        protocol_id: protocol_id(),
        protocol_major: 2,
        server_limits: ConnectionLimits {
            max_body_bytes: 0x800,
            max_handles: 2,
            max_outstanding: 4,
        },
        transport_limits: ConnectionLimits {
            max_body_bytes: 0x1000,
            max_handles: 4,
            max_outstanding: 8,
        },
        protocol_limits: ConnectionLimits {
            max_body_bytes: 0x2000,
            max_handles: 8,
            max_outstanding: 16,
        },
        service_generation: 0x0102_0304_0506_0708,
        versions: VERSIONS,
        available_features: AVAILABLE_FEATURES,
        feature_set_fits,
    }
}

#[test]
fn negotiation_request_matches_literal_golden_bytes() {
    let expected: [u8; 64] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff, 0x02, 0x00, 0x01, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x04, 0x00,
        0x08, 0x00, 0x10, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00,
        0x01, 0x00, 0x00, 0x00,
    ];
    let features = [FeatureRecord::optional(1), FeatureRecord::required(3)];
    let mut encoded = [0_u8; 64];
    assert_eq!(request().encode(&features, &mut encoded).unwrap(), 64);
    assert_eq!(encoded, expected);
    let decoded = NegotiationRequest::decode(&expected).unwrap();
    assert_eq!(*decoded.root(), request());
    assert_eq!(decoded.features().get(0), Some(features[0]));
    assert_eq!(decoded.features().get(1), Some(features[1]));
    assert_eq!(decoded.features().get(2), None);
}

#[test]
fn negotiation_response_matches_literal_golden_bytes() {
    let response = NegotiationResponse {
        protocol_id: protocol_id(),
        status: NegotiationStatus::Ok,
        protocol_major: 2,
        selected_minor: 3,
        server_min_minor: 2,
        server_max_minor: 4,
        max_body_bytes: 0x800,
        max_handles: 2,
        max_outstanding: 4,
        service_generation: 0x0102_0304_0506_0708,
    };
    let expected: [u8; 72] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x03, 0x00, 0x02, 0x00, 0x04, 0x00, 0x00, 0x08,
        0x00, 0x00, 0x02, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x00, 0x00, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x07, 0x06, 0x05,
        0x04, 0x03, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let features = [FeatureRecord::enabled(1)];
    let mut encoded = [0_u8; 72];
    assert_eq!(response.encode(&features, &mut encoded).unwrap(), 72);
    assert_eq!(encoded, expected);
    let decoded = NegotiationResponse::decode(&expected).unwrap();
    assert_eq!(*decoded.root(), response);
    assert_eq!(decoded.features().get(0), Some(features[0]));
}

#[test]
fn selection_chooses_highest_fitting_minor_and_reduces_limits() {
    let features = [FeatureRecord::optional(1), FeatureRecord::optional(2)];
    let mut encoded = [0; 64];
    let size = request().encode(&features, &mut encoded).unwrap();
    let decoded = NegotiationRequest::decode(&encoded[..size]).unwrap();
    let mut selected = [FeatureRecord::enabled(99); 2];
    let outcome = negotiate(&decoded, &profile(), &mut selected).unwrap();
    assert_eq!(outcome.response.status, NegotiationStatus::Ok);
    assert_eq!(outcome.response.selected_minor, 3);
    assert_eq!(outcome.response.max_body_bytes, 0x800);
    assert_eq!(outcome.response.max_handles, 2);
    assert_eq!(outcome.response.max_outstanding, 4);
    assert_eq!(outcome.feature_count, 1);
    assert_eq!(selected[0], FeatureRecord::enabled(1));
}

#[test]
fn output_capacity_tracks_selected_not_requested_features() {
    let optional = [FeatureRecord::optional(2)];
    let mut encoded = [0; 56];
    let size = request().encode(&optional, &mut encoded).unwrap();
    let decoded = NegotiationRequest::decode(&encoded[..size]).unwrap();

    let outcome = negotiate(&decoded, &profile(), &mut []).unwrap();
    assert_eq!(outcome.response.status, NegotiationStatus::Ok);
    assert_eq!(outcome.feature_count, 0);

    let mut different = profile();
    different.protocol_id = ProtocolId::parse("10112233-4455-4677-8899-aabbccddeeff").unwrap();
    let outcome = negotiate(&decoded, &different, &mut []).unwrap();
    assert_eq!(
        outcome.response.status,
        NegotiationStatus::UnsupportedProtocol
    );

    let supported = [FeatureRecord::optional(1)];
    let size = request().encode(&supported, &mut encoded).unwrap();
    let decoded = NegotiationRequest::decode(&encoded[..size]).unwrap();
    assert_eq!(
        negotiate(&decoded, &profile(), &mut []),
        Err(NegotiationError::OutputTooSmall)
    );
}

#[test]
fn selection_falls_back_when_highest_minor_does_not_fit() {
    let mut constrained = profile();
    constrained.server_limits.max_body_bytes = 64;
    constrained.server_limits.max_handles = 0;
    let mut encoded = [0; NEGOTIATE_REQUEST_ROOT_BYTES];
    let size = request().encode(&[], &mut encoded).unwrap();
    let decoded = NegotiationRequest::decode(&encoded[..size]).unwrap();
    let outcome = negotiate(&decoded, &constrained, &mut []).unwrap();
    assert_eq!(outcome.response.status, NegotiationStatus::Ok);
    assert_eq!(outcome.response.selected_minor, 2);
}

#[test]
fn unavailable_protocol_major_minor_and_features_are_typed_failures() {
    let mut encoded = [0; 64];
    let mut selected = [FeatureRecord::enabled(99); 1];
    let size = request().encode(&[], &mut encoded).unwrap();
    let decoded = NegotiationRequest::decode(&encoded[..size]).unwrap();

    let mut different = profile();
    different.protocol_id = ProtocolId::parse("10112233-4455-4677-8899-aabbccddeeff").unwrap();
    let outcome = negotiate(&decoded, &different, &mut selected).unwrap();
    assert_eq!(
        outcome.response.status,
        NegotiationStatus::UnsupportedProtocol
    );
    assert_eq!(outcome.response.server_min_minor, 0);
    assert_eq!(outcome.response.server_max_minor, 0);

    let mut different = profile();
    different.protocol_major = 3;
    assert_eq!(
        negotiate(&decoded, &different, &mut selected)
            .unwrap()
            .response
            .status,
        NegotiationStatus::UnsupportedMajor
    );

    let mut disjoint = profile();
    disjoint.versions = DISJOINT_VERSIONS;
    assert_eq!(
        negotiate(&decoded, &disjoint, &mut selected)
            .unwrap()
            .response
            .status,
        NegotiationStatus::NoCommonMinor
    );

    let required = [FeatureRecord::required(2)];
    let size = request().encode(&required, &mut encoded).unwrap();
    let decoded = NegotiationRequest::decode(&encoded[..size]).unwrap();
    assert_eq!(
        negotiate(&decoded, &profile(), &mut selected)
            .unwrap()
            .response
            .status,
        NegotiationStatus::RequiredFeatureUnavailable
    );
}

#[test]
fn transport_proposal_overflow_is_malformed_and_small_fit_is_typed_failure() {
    let mut oversized = request();
    oversized.max_handles = 5;
    let mut encoded = [0; NEGOTIATE_REQUEST_ROOT_BYTES];
    let size = oversized.encode(&[], &mut encoded).unwrap();
    let decoded = NegotiationRequest::decode(&encoded[..size]).unwrap();
    assert_eq!(
        negotiate(&decoded, &profile(), &mut []),
        Err(NegotiationError::InvalidLimits)
    );

    let mut reducible = request();
    reducible.max_body_bytes = 0x4000;
    reducible.max_outstanding = 32;
    let size = reducible.encode(&[], &mut encoded).unwrap();
    let decoded = NegotiationRequest::decode(&encoded[..size]).unwrap();
    let outcome = negotiate(&decoded, &profile(), &mut []).unwrap();
    assert_eq!(outcome.response.status, NegotiationStatus::Ok);
    assert_eq!(outcome.response.max_body_bytes, 0x800);
    assert_eq!(outcome.response.max_outstanding, 4);

    let mut too_small = request();
    too_small.max_body_bytes = 32;
    too_small.max_handles = 0;
    let size = too_small.encode(&[], &mut encoded).unwrap();
    let decoded = NegotiationRequest::decode(&encoded[..size]).unwrap();
    assert_eq!(
        negotiate(&decoded, &profile(), &mut [])
            .unwrap()
            .response
            .status,
        NegotiationStatus::TransportBoundsTooSmall
    );
}

#[test]
fn feature_order_flags_and_failure_shapes_are_strict() {
    let mut encoded = [0; 80];
    assert_eq!(
        request().encode(
            &[FeatureRecord::optional(2), FeatureRecord::optional(1)],
            &mut encoded
        ),
        Err(EncodeError::InvalidNegotiation(
            NegotiationError::UnsortedFeatures
        ))
    );
    assert_eq!(
        request().encode(
            &[FeatureRecord::optional(1), FeatureRecord::required(1)],
            &mut encoded
        ),
        Err(EncodeError::InvalidNegotiation(
            NegotiationError::DuplicateFeature
        ))
    );

    let failure = NegotiationResponse {
        protocol_id: protocol_id(),
        status: NegotiationStatus::Busy,
        protocol_major: 2,
        selected_minor: 1,
        server_min_minor: 1,
        server_max_minor: 3,
        max_body_bytes: 0,
        max_handles: 0,
        max_outstanding: 0,
        service_generation: 0,
    };
    assert_eq!(
        failure.encode(&[], &mut encoded),
        Err(EncodeError::InvalidNegotiation(
            NegotiationError::InvalidFailure
        ))
    );

    let success = NegotiationResponse {
        protocol_id: protocol_id(),
        status: NegotiationStatus::Ok,
        protocol_major: 2,
        selected_minor: 2,
        server_min_minor: 1,
        server_max_minor: 3,
        max_body_bytes: 64,
        max_handles: 0,
        max_outstanding: 1,
        service_generation: 0,
    };
    assert_eq!(
        success.encode(&[], &mut encoded),
        Err(EncodeError::InvalidNegotiation(
            NegotiationError::InvalidSuccess
        ))
    );
}

#[test]
fn malformed_negotiation_bytes_and_selected_relationships_are_rejected() {
    let features = [FeatureRecord::optional(1), FeatureRecord::optional(3)];
    let mut request_bytes = [0; 64];
    let request_size = request().encode(&features, &mut request_bytes).unwrap();

    let mut malformed = request_bytes;
    malformed[0x16] = 1;
    assert_eq!(
        NegotiationRequest::decode(&malformed[..request_size]),
        Err(DecodeError::InvalidNegotiation(
            NegotiationError::InvalidFlags
        ))
    );

    let mut malformed = request_bytes;
    malformed[56..60].copy_from_slice(&1_u32.to_le_bytes());
    assert_eq!(
        NegotiationRequest::decode(&malformed[..request_size]),
        Err(DecodeError::InvalidNegotiation(
            NegotiationError::DuplicateFeature
        ))
    );

    let mut malformed = request_bytes;
    malformed[52..56].copy_from_slice(&2_u32.to_le_bytes());
    assert_eq!(
        NegotiationRequest::decode(&malformed[..request_size]),
        Err(DecodeError::InvalidNegotiation(
            NegotiationError::UnknownFeatureFlags
        ))
    );

    let mut with_trailing = [0; 72];
    with_trailing[..request_size].copy_from_slice(&request_bytes[..request_size]);
    assert_eq!(
        NegotiationRequest::decode(&with_trailing),
        Err(DecodeError::InvalidNegotiation(
            NegotiationError::InvalidSlice
        ))
    );

    let mut selected = [FeatureRecord::enabled(1), FeatureRecord::enabled(3)];
    let request = NegotiationRequest::decode(&request_bytes[..request_size]).unwrap();
    let outcome = negotiate(&request, &profile(), &mut selected).unwrap();
    let mut response_bytes = [0; 80];
    let response_size = outcome
        .response
        .encode(&selected[..outcome.feature_count], &mut response_bytes)
        .unwrap();

    let mut malformed = response_bytes;
    malformed[0x24] = 1;
    assert_eq!(
        NegotiationResponse::decode(&malformed[..response_size]),
        Err(DecodeError::InvalidNegotiation(
            NegotiationError::InvalidFlags
        ))
    );

    let mut malformed = response_bytes;
    malformed[0x10..0x14].copy_from_slice(&99_u32.to_le_bytes());
    assert_eq!(
        NegotiationResponse::decode(&malformed[..response_size]),
        Err(DecodeError::InvalidNegotiation(
            NegotiationError::UnknownStatus
        ))
    );

    let mut increased = outcome.response;
    increased.max_body_bytes = request.root().max_body_bytes + 1;
    let increased_size = increased
        .encode(&selected[..outcome.feature_count], &mut response_bytes)
        .unwrap();
    let increased = NegotiationResponse::decode(&response_bytes[..increased_size]).unwrap();
    assert_eq!(
        increased.validate_against(&request, AVAILABLE_FEATURES, feature_set_fits),
        Err(NegotiationError::LimitIncrease)
    );
}

#[test]
fn response_echo_and_required_features_are_packet_bound() {
    let requested = [FeatureRecord::required(1)];
    let mut request_bytes = [0; 64];
    let request_size = request().encode(&requested, &mut request_bytes).unwrap();
    let request = NegotiationRequest::decode(&request_bytes[..request_size]).unwrap();

    let mut selected = [FeatureRecord::enabled(1)];
    let outcome = negotiate(&request, &profile(), &mut selected).unwrap();
    let mut response_bytes = [0; 72];
    let response_size = outcome
        .response
        .encode(&selected[..outcome.feature_count], &mut response_bytes)
        .unwrap();
    let response = NegotiationResponse::decode(&response_bytes[..response_size]).unwrap();
    response
        .validate_against(&request, AVAILABLE_FEATURES, feature_set_fits)
        .unwrap();
    let bound = response
        .bind(&request, AVAILABLE_FEATURES, feature_set_fits, &[1])
        .unwrap();
    assert_eq!(bound.protocol_id(), protocol_id());
    assert_eq!(bound.major(), 2);
    assert_eq!(bound.minor(), 3);
    assert!(bound.supports_feature(1));

    let mut mismatched_root = *response.root();
    mismatched_root.protocol_id =
        ProtocolId::parse("10112233-4455-4677-8899-aabbccddeeff").unwrap();
    let mismatch_size = mismatched_root
        .encode(&selected, &mut response_bytes)
        .unwrap();
    let mismatch = NegotiationResponse::decode(&response_bytes[..mismatch_size]).unwrap();
    assert_eq!(
        mismatch.validate_against(&request, AVAILABLE_FEATURES, feature_set_fits),
        Err(NegotiationError::EchoMismatch)
    );

    let empty = NegotiationResponse::decode(&response_bytes[..NEGOTIATE_RESPONSE_ROOT_BYTES]);
    assert!(matches!(
        empty,
        Err(DecodeError::InvalidNegotiation(
            NegotiationError::InvalidSlice
        ))
    ));
}

#[test]
fn feature_resources_control_selection_and_optional_features() {
    const RESOURCE_FEATURES: &[AvailableFeature] = &[
        AvailableFeature {
            id: 1,
            since_minor: 2,
        },
        AvailableFeature {
            id: 3,
            since_minor: 3,
        },
    ];
    let mut constrained = profile();
    constrained.server_limits.max_body_bytes = 256;
    constrained.available_features = RESOURCE_FEATURES;
    let mut request_bytes = [0; 64];
    let mut selected = [FeatureRecord::enabled(99); 2];

    let optional = [FeatureRecord::optional(1), FeatureRecord::optional(3)];
    let size = request().encode(&optional, &mut request_bytes).unwrap();
    let decoded = NegotiationRequest::decode(&request_bytes[..size]).unwrap();
    let outcome = negotiate(&decoded, &constrained, &mut selected).unwrap();
    assert_eq!(outcome.response.status, NegotiationStatus::Ok);
    assert_eq!(outcome.response.selected_minor, 3);
    assert_eq!(outcome.feature_count, 1);
    assert_eq!(selected[0], FeatureRecord::enabled(1));

    let required = [FeatureRecord::required(1), FeatureRecord::required(3)];
    let size = request().encode(&required, &mut request_bytes).unwrap();
    let decoded = NegotiationRequest::decode(&request_bytes[..size]).unwrap();
    let outcome = negotiate(&decoded, &constrained, &mut selected).unwrap();
    assert_eq!(
        outcome.response.status,
        NegotiationStatus::TransportBoundsTooSmall
    );
}

#[test]
fn client_rejects_features_that_do_not_fit_selected_version_or_limits() {
    let requested = [FeatureRecord::required(3)];
    let mut request_bytes = [0; 64];
    let request_size = request().encode(&requested, &mut request_bytes).unwrap();
    let request = NegotiationRequest::decode(&request_bytes[..request_size]).unwrap();
    let mut response_bytes = [0; 72];
    let selected = [FeatureRecord::enabled(3)];

    let response = NegotiationResponse {
        protocol_id: protocol_id(),
        status: NegotiationStatus::Ok,
        protocol_major: 2,
        selected_minor: 2,
        server_min_minor: 2,
        server_max_minor: 4,
        max_body_bytes: 128,
        max_handles: 1,
        max_outstanding: 4,
        service_generation: 1,
    };
    let size = response.encode(&selected, &mut response_bytes).unwrap();
    let decoded = NegotiationResponse::decode(&response_bytes[..size]).unwrap();
    assert_eq!(
        decoded.validate_against(&request, AVAILABLE_FEATURES, feature_set_fits),
        Err(NegotiationError::FeatureUnavailableAtVersion)
    );

    let mut too_small = response;
    too_small.selected_minor = 3;
    too_small.max_body_bytes = 64;
    let size = too_small.encode(&selected, &mut response_bytes).unwrap();
    let decoded = NegotiationResponse::decode(&response_bytes[..size]).unwrap();
    assert_eq!(
        decoded.validate_against(&request, AVAILABLE_FEATURES, feature_set_fits),
        Err(NegotiationError::FeatureBoundsExceeded)
    );
}

#[test]
fn canonical_failure_ranges_and_contiguous_profiles_are_enforced() {
    let mut encoded = [0; NEGOTIATE_RESPONSE_ROOT_BYTES];
    let failure = NegotiationResponse {
        protocol_id: protocol_id(),
        status: NegotiationStatus::UnsupportedProtocol,
        protocol_major: 2,
        selected_minor: 0,
        server_min_minor: 2,
        server_max_minor: 4,
        max_body_bytes: 0,
        max_handles: 0,
        max_outstanding: 0,
        service_generation: 0,
    };
    assert_eq!(
        failure.encode(&[], &mut encoded),
        Err(EncodeError::InvalidNegotiation(
            NegotiationError::InvalidFailure
        ))
    );

    const GAPPED_VERSIONS: &[MinorVersionProfile] = &[
        MinorVersionProfile {
            minor: 2,
            minimum_body_bytes: 64,
            minimum_handles: 0,
        },
        MinorVersionProfile {
            minor: 4,
            minimum_body_bytes: 64,
            minimum_handles: 0,
        },
    ];
    let mut gapped = profile();
    gapped.versions = GAPPED_VERSIONS;
    let mut request_bytes = [0; NEGOTIATE_REQUEST_ROOT_BYTES];
    let size = request().encode(&[], &mut request_bytes).unwrap();
    let decoded = NegotiationRequest::decode(&request_bytes[..size]).unwrap();
    assert_eq!(
        negotiate(&decoded, &gapped, &mut []),
        Err(NegotiationError::InvalidMinorRange)
    );
}
