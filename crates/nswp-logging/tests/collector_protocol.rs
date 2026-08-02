use nswp_core::{BodyError, BoundProtocol, ConnectionLimits};
use nswp_logging::{
    CollectorStats, EventId, HistoryReadRequest, HistoryRecordView, LOGGING_PROTOCOL_ID,
    LOGGING_PROTOCOL_MAJOR, LOGGING_PROTOCOL_MINOR_COLLECTOR_READS,
    LOGGING_PROTOCOL_MINOR_WALL_TIME, LogSeverity, PrivacyClass, RecordId, RecordIdError,
    decode_collector_stats_request, decode_collector_stats_response, decode_history_read_request,
    decode_history_read_response, encode_collector_stats_request, encode_collector_stats_response,
    encode_history_read_request, encode_history_read_response, logging_protocol_through,
};
use nswp_runtime::MAX_BODY_BYTES;

const EVENT_ID_BYTES: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
];
const EVENT_ID: EventId = match EventId::from_bytes(EVENT_ID_BYTES) {
    Ok(id) => id,
    Err(_) => panic!("test event ID must be valid"),
};

const STATS_VECTOR: [u8; 64] = [
    5, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0,
    1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0,
];

const HISTORY_VECTOR: [u8; 192] = [
    1, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 168, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2,
    1, 24, 23, 22, 21, 20, 19, 18, 17, 56, 55, 54, 53, 52, 51, 50, 49, 40, 39, 38, 37, 36, 35, 34,
    33, 0, 17, 34, 51, 68, 85, 70, 119, 136, 153, 170, 187, 204, 221, 238, 255, 165, 165, 165, 165,
    165, 165, 165, 165, 165, 165, 165, 165, 165, 165, 165, 165, 24, 0, 0, 0, 80, 0, 0, 0, 80, 0, 0,
    0, 0, 0, 0, 0, 4, 3, 1, 16, 64, 0, 0, 0, 115, 115, 115, 115, 115, 115, 115, 115, 115, 115, 115,
    115, 115, 115, 115, 115, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109,
    109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109,
    109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109,
    109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109, 109,
];

fn bound(minor: u16) -> BoundProtocol<'static> {
    BoundProtocol::new(
        LOGGING_PROTOCOL_ID,
        LOGGING_PROTOCOL_MAJOR,
        minor,
        ConnectionLimits::ENDPOINT_PROTOTYPE,
        7,
        &[],
    )
    .unwrap()
}

fn stats() -> CollectorStats {
    CollectorStats {
        received_records: 5,
        retained_records: 2,
        capacity_records: 2,
        evicted_records: 2,
        dropped_records: 1,
        redacted_records: 1,
        oldest_record_id: Some(RecordId::new(3).unwrap()),
        newest_record_id: Some(RecordId::new(4).unwrap()),
    }
}

#[test]
fn record_ids_are_nonzero_and_stats_match_literal_fixed_bodies() {
    assert_eq!(RecordId::new(0), Err(RecordIdError::Zero));
    assert_eq!(RecordId::new(1).unwrap().get(), 1);

    let request = encode_collector_stats_request(LOGGING_PROTOCOL_MINOR_COLLECTOR_READS).unwrap();
    assert!(request.is_empty());
    assert_eq!(
        decode_collector_stats_request(request.as_slice(), &bound(2)),
        Ok(())
    );

    let encoded =
        encode_collector_stats_response(stats(), LOGGING_PROTOCOL_MINOR_COLLECTOR_READS).unwrap();
    assert_eq!(encoded.as_slice(), STATS_VECTOR);
    assert_eq!(encoded.len(), 64);
    assert_eq!(
        decode_collector_stats_response(encoded.as_slice(), &bound(2)).unwrap(),
        stats()
    );
}

#[test]
fn maximum_history_record_matches_a_literal_192_byte_body() {
    let subsystem = "s".repeat(16);
    let message = "m".repeat(64);
    let record = HistoryRecordView {
        record_id: RecordId::new(0x0102_0304_0506_0708).unwrap(),
        source_process_id: 0x2122_2324_2526_2728,
        event_id: EVENT_ID,
        severity: LogSeverity::Warning,
        privacy: PrivacyClass::SecuritySensitive,
        monotonic_time_ns: 0x1112_1314_1516_1718,
        subsystem: &subsystem,
        message: &message,
        wall_time_unix_ns: Some(0x3132_3334_3536_3738),
        trace_id: [0xa5; 16],
    };

    let encoded =
        encode_history_read_response(Some(record), LOGGING_PROTOCOL_MINOR_COLLECTOR_READS).unwrap();
    assert_eq!(encoded.len(), MAX_BODY_BYTES);
    assert_eq!(encoded.as_slice(), HISTORY_VECTOR);

    let decoded = decode_history_read_response(encoded.as_slice(), &bound(2))
        .unwrap()
        .unwrap();
    assert_eq!(decoded, record);
}

#[test]
fn history_none_and_cursor_sentinel_are_canonical() {
    let none = encode_history_read_response(None, LOGGING_PROTOCOL_MINOR_COLLECTOR_READS).unwrap();
    assert_eq!(none.as_slice(), &[0; 24]);
    assert_eq!(
        decode_history_read_response(none.as_slice(), &bound(2)),
        Ok(None)
    );

    let begin = encode_history_read_request(
        HistoryReadRequest {
            after_record_id: None,
        },
        LOGGING_PROTOCOL_MINOR_COLLECTOR_READS,
    )
    .unwrap();
    assert_eq!(begin.as_slice(), &[0; 8]);
    assert_eq!(
        decode_history_read_request(begin.as_slice(), &bound(2)).unwrap(),
        HistoryReadRequest {
            after_record_id: None
        }
    );

    let after = encode_history_read_request(
        HistoryReadRequest {
            after_record_id: Some(RecordId::new(9).unwrap()),
        },
        LOGGING_PROTOCOL_MINOR_COLLECTOR_READS,
    )
    .unwrap();
    assert_eq!(after.as_slice(), &9_u64.to_le_bytes());
}

#[test]
fn malformed_stats_relationships_are_rejected() {
    let mut malformed = STATS_VECTOR;
    malformed[8..16].copy_from_slice(&3_u64.to_le_bytes());
    assert_eq!(
        decode_collector_stats_response(&malformed, &bound(2)),
        Err(BodyError::MaterializationMismatch)
    );

    let mut malformed = STATS_VECTOR;
    malformed[56..64].copy_from_slice(&5_u64.to_le_bytes());
    assert_eq!(
        decode_collector_stats_response(&malformed, &bound(2)),
        Err(BodyError::MaterializationMismatch)
    );

    let mut malformed = STATS_VECTOR;
    malformed[0..8].copy_from_slice(&4_u64.to_le_bytes());
    assert_eq!(
        decode_collector_stats_response(&malformed, &bound(2)),
        Err(BodyError::MaterializationMismatch)
    );
}

#[test]
fn malformed_history_semantics_are_rejected() {
    let mut zero_record_id = HISTORY_VECTOR;
    zero_record_id[24..32].fill(0);
    assert_eq!(
        decode_history_read_response(&zero_record_id, &bound(2)),
        Err(BodyError::MaterializationMismatch)
    );

    let mut zero_source = HISTORY_VECTOR;
    zero_source[48..56].fill(0);
    assert_eq!(
        decode_history_read_response(&zero_source, &bound(2)),
        Err(BodyError::MaterializationMismatch)
    );

    let mut absent_nonzero_wall_time = HISTORY_VECTOR;
    absent_nonzero_wall_time[106] = 0;
    assert_eq!(
        decode_history_read_response(&absent_nonzero_wall_time, &bound(2)),
        Err(BodyError::MaterializationMismatch)
    );

    let mut bad_lengths = HISTORY_VECTOR;
    bad_lengths[107] = 15;
    assert_eq!(
        decode_history_read_response(&bad_lengths, &bound(2)),
        Err(BodyError::MaterializationMismatch)
    );

    let mut oversized_subsystem = HISTORY_VECTOR;
    oversized_subsystem[107] = 17;
    oversized_subsystem[108] = 63;
    assert_eq!(
        decode_history_read_response(&oversized_subsystem, &bound(2)),
        Err(BodyError::MaterializationMismatch)
    );

    let mut bad_utf8 = HISTORY_VECTOR;
    bad_utf8[112] = 0xff;
    assert_eq!(
        decode_history_read_response(&bad_utf8, &bound(2)),
        Err(BodyError::InvalidUtf8)
    );

    let mut nil_event_id = HISTORY_VECTOR;
    nil_event_id[56..72].fill(0);
    assert_eq!(
        decode_history_read_response(&nil_event_id, &bound(2)),
        Err(BodyError::MaterializationMismatch)
    );
}

#[test]
fn collector_reads_are_minor_two_only_and_capped_descriptors_hide_methods() {
    assert_eq!(
        encode_history_read_request(
            HistoryReadRequest {
                after_record_id: None,
            },
            LOGGING_PROTOCOL_MINOR_WALL_TIME,
        ),
        Err(BodyError::FieldUnavailable)
    );
    assert_eq!(
        decode_history_read_response(&[0; 24], &bound(LOGGING_PROTOCOL_MINOR_WALL_TIME)),
        Err(BodyError::FieldUnavailable)
    );
    assert_eq!(logging_protocol_through(0).methods.len(), 1);
    assert_eq!(logging_protocol_through(1).methods.len(), 1);
    assert_eq!(logging_protocol_through(2).methods.len(), 3);
}
