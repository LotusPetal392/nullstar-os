use nswp_core::{BodyError, BoundProtocol, ConnectionLimits};
use nswp_logging::{
    EventId, EventIdError, LOGGING_MAX_MESSAGE_BYTES, LOGGING_MAX_SUBSYSTEM_BYTES,
    LOGGING_PROTOCOL_ID, LOGGING_PROTOCOL_MAJOR, LOGGING_PROTOCOL_MINOR_BASE,
    LOGGING_PROTOCOL_MINOR_WALL_TIME, LogRecord, LogSeverity, PrivacyClass, decode_log_record,
    encode_log_record,
};

const EVENT_ID_BYTES: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
];
const EVENT_ID: EventId = match EventId::from_bytes(EVENT_ID_BYTES) {
    Ok(id) => id,
    Err(_) => panic!("test event ID must be a valid UUIDv4"),
};

fn bound(minor: u16) -> BoundProtocol<'static> {
    BoundProtocol::new(
        LOGGING_PROTOCOL_ID,
        LOGGING_PROTOCOL_MAJOR,
        minor,
        ConnectionLimits::ENDPOINT_PROTOTYPE,
        1,
        &[],
    )
    .unwrap()
}

fn record<'a>(subsystem: &'a str, message: &'a str) -> LogRecord<'a> {
    LogRecord {
        event_id: EVENT_ID,
        severity: LogSeverity::Info,
        privacy: PrivacyClass::Public,
        monotonic_time_ns: 55,
        subsystem,
        message,
        wall_time_unix_ns: None,
    }
}

#[test]
fn event_ids_require_non_nil_uuid_v4_rfc_bytes() {
    assert_eq!(EventId::from_bytes([0; 16]), Err(EventIdError::Nil));

    let mut wrong_version = EVENT_ID_BYTES;
    wrong_version[6] = 0x16;
    assert_eq!(
        EventId::from_bytes(wrong_version),
        Err(EventIdError::InvalidVersion)
    );

    let mut wrong_variant = EVENT_ID_BYTES;
    wrong_variant[8] = 0x49;
    assert_eq!(
        EventId::from_bytes(wrong_variant),
        Err(EventIdError::InvalidVariant)
    );

    assert_eq!(EVENT_ID.as_bytes(), &EVENT_ID_BYTES);
}

#[test]
fn decoded_event_ids_receive_the_same_semantic_validation() {
    let body =
        encode_log_record(record("core", "event"), LOGGING_PROTOCOL_MINOR_WALL_TIME).unwrap();

    let mut nil = body.as_slice().to_vec();
    nil[16..32].fill(0);
    assert_eq!(
        decode_log_record(&nil, &bound(LOGGING_PROTOCOL_MINOR_WALL_TIME)),
        Err(BodyError::MaterializationMismatch)
    );

    let mut wrong_version = body.as_slice().to_vec();
    wrong_version[22] = 0x16;
    assert_eq!(
        decode_log_record(&wrong_version, &bound(LOGGING_PROTOCOL_MINOR_WALL_TIME)),
        Err(BodyError::MaterializationMismatch)
    );

    let mut wrong_variant = body.as_slice().to_vec();
    wrong_variant[24] = 0x49;
    assert_eq!(
        decode_log_record(&wrong_variant, &bound(LOGGING_PROTOCOL_MINOR_WALL_TIME)),
        Err(BodyError::MaterializationMismatch)
    );
}

#[test]
fn subsystem_and_message_bounds_are_utf8_byte_bounds() {
    let subsystem = "s".repeat(LOGGING_MAX_SUBSYSTEM_BYTES + 1);
    assert_eq!(
        encode_log_record(
            record(&subsystem, "message"),
            LOGGING_PROTOCOL_MINOR_WALL_TIME
        ),
        Err(BodyError::LimitExceeded)
    );

    let message = "m".repeat(LOGGING_MAX_MESSAGE_BYTES + 1);
    assert_eq!(
        encode_log_record(record("core", &message), LOGGING_PROTOCOL_MINOR_WALL_TIME),
        Err(BodyError::LimitExceeded)
    );

    let multibyte_message = "é".repeat(LOGGING_MAX_MESSAGE_BYTES / 2 + 1);
    assert_eq!(multibyte_message.chars().count(), 33);
    assert_eq!(multibyte_message.len(), 66);
    assert_eq!(
        encode_log_record(
            record("core", &multibyte_message),
            LOGGING_PROTOCOL_MINOR_WALL_TIME
        ),
        Err(BodyError::LimitExceeded)
    );
}

#[test]
fn wall_time_is_unavailable_at_minor_zero() {
    let mut value = record("core", "wall time");
    value.wall_time_unix_ns = Some(987_654);
    assert_eq!(
        encode_log_record(value, LOGGING_PROTOCOL_MINOR_BASE),
        Err(BodyError::FieldUnavailable)
    );
}

#[test]
fn closed_enums_and_protocol_major_are_enforced() {
    let body =
        encode_log_record(record("core", "enums"), LOGGING_PROTOCOL_MINOR_WALL_TIME).unwrap();

    let mut bad_severity = body.as_slice().to_vec();
    bad_severity[0] = 9;
    assert_eq!(
        decode_log_record(&bad_severity, &bound(LOGGING_PROTOCOL_MINOR_WALL_TIME)),
        Err(BodyError::UnknownEnumValue)
    );

    let mut bad_privacy = body.as_slice().to_vec();
    bad_privacy[1] = 5;
    assert_eq!(
        decode_log_record(&bad_privacy, &bound(LOGGING_PROTOCOL_MINOR_WALL_TIME)),
        Err(BodyError::UnknownEnumValue)
    );

    let wrong_major = BoundProtocol::new(
        LOGGING_PROTOCOL_ID,
        LOGGING_PROTOCOL_MAJOR - 1,
        LOGGING_PROTOCOL_MINOR_WALL_TIME,
        ConnectionLimits::ENDPOINT_PROTOTYPE,
        1,
        &[],
    )
    .unwrap();
    assert_eq!(
        decode_log_record(body.as_slice(), &wrong_major),
        Err(BodyError::ProtocolMismatch)
    );
}
