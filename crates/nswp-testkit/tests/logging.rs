use std::sync::atomic::{AtomicUsize, Ordering};

use nswp_core::{
    BodyEncoder, BodyError, BodyLimits, BoundProtocol, ConnectionLimits, Header,
    MinorVersionProfile, NSWP_HEADER_BYTES, PacketKind, offset,
};
use nswp_runtime::{
    Client, ClientEvent, CloseReason, ConnectionPhase, DeadlinePolicy, MethodDescriptor,
    MethodKind, PeerContextId, ProtocolDescriptor, RuntimeError, Server, ServerEvent, TryTransport,
};
use nswp_testkit::{
    ECHO_PROTOCOL_ID, LOGGING_EMIT_ORDINAL, LOGGING_MAX_MESSAGE_BYTES, LOGGING_MAX_SUBSYSTEM_BYTES,
    LOGGING_PROTOCOL_ID, LOGGING_PROTOCOL_MINOR_BASE, LOGGING_PROTOCOL_MINOR_WALL_TIME,
    LogDelivery, LogDisposition, LogRecord, LogSeverity, LoggingCollector, LoggingProducer,
    PrivacyClass, ProducerIdentity, SECRET_REDACTION, SimEndpoint, channel_pair, decode_log_record,
    encode_log_record, logging_protocol, logging_protocol_through,
};

type Endpoint<const QUEUE: usize> = SimEndpoint<QUEUE>;
type Producer<const QUEUE: usize> = LoggingProducer<'static, Endpoint<QUEUE>>;
type CollectorServer<const QUEUE: usize> = Server<'static, Endpoint<QUEUE>>;

static DEADLINE_VALIDATIONS: AtomicUsize = AtomicUsize::new(0);
static DEADLINE_VERSIONS: [MinorVersionProfile; 1] = [MinorVersionProfile {
    minor: 0,
    minimum_body_bytes: 8,
    minimum_handles: 0,
}];
static DEADLINE_METHODS: [MethodDescriptor; 2] = [
    MethodDescriptor {
        ordinal: 1,
        kind: MethodKind::OneWay,
        deadline: DeadlinePolicy::Required {
            max_duration_ns: Some(100),
        },
        validate_request: validate_empty_body,
        validate_response: validate_empty_body,
    },
    MethodDescriptor {
        ordinal: 2,
        kind: MethodKind::OneWay,
        deadline: DeadlinePolicy::Optional {
            max_duration_ns: Some(100),
        },
        validate_request: validate_empty_body,
        validate_response: validate_empty_body,
    },
];

fn validate_empty_body(body: &[u8], _bound: &BoundProtocol<'_>) -> Result<(), BodyError> {
    DEADLINE_VALIDATIONS.fetch_add(1, Ordering::SeqCst);
    if body.is_empty() {
        Ok(())
    } else {
        Err(BodyError::TrailingBytes)
    }
}

fn deadline_protocol() -> ProtocolDescriptor<'static> {
    ProtocolDescriptor {
        protocol_id: LOGGING_PROTOCOL_ID,
        major: 1,
        min_minor: 0,
        max_minor: 0,
        limits: ConnectionLimits {
            max_body_bytes: 192,
            max_handles: 0,
            max_outstanding: 8,
        },
        requested_features: &[],
        available_features: &[],
        versions: &DEADLINE_VERSIONS,
        feature_set_fits: nswp_runtime::no_features_fit,
        methods: &DEADLINE_METHODS,
    }
}

fn connected<const QUEUE: usize>(
    max_minor: u16,
    collector_generation: u64,
) -> (
    Producer<QUEUE>,
    CollectorServer<QUEUE>,
    Endpoint<QUEUE>,
    Endpoint<QUEUE>,
) {
    let (producer_endpoint, collector_endpoint) = channel_pair::<QUEUE>();
    let producer_control = producer_endpoint.clone();
    let collector_control = collector_endpoint.clone();
    let mut producer = LoggingProducer::through_minor(producer_endpoint, max_minor);
    let mut server = Server::new_with_peer_context(
        collector_endpoint,
        logging_protocol(),
        collector_generation,
        peer_context(),
    )
    .unwrap();
    producer.try_negotiate().unwrap();
    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::Bound(bound)) if bound.service_generation() == collector_generation
    ));
    assert!(matches!(
        producer.poll().unwrap(),
        Some(ClientEvent::Bound(_))
    ));
    (producer, server, producer_control, collector_control)
}

fn record<'a>(subsystem: &'a str, message: &'a str) -> LogRecord<'a> {
    LogRecord {
        severity: LogSeverity::Warning,
        privacy: PrivacyClass::SecuritySensitive,
        monotonic_time_ns: 123_456,
        subsystem,
        message,
        wall_time_unix_ns: Some(987_654),
    }
}

fn peer_context() -> PeerContextId {
    PeerContextId::new(0xfeed)
}

fn producer_identity() -> ProducerIdentity {
    ProducerIdentity {
        peer_context: peer_context(),
        principal_id: 42,
        service_id: ECHO_PROTOCOL_ID,
        service_generation: 77,
    }
}

#[test]
fn generic_one_way_deadlines_are_enforced_before_dispatch() {
    DEADLINE_VALIDATIONS.store(0, Ordering::SeqCst);
    let (client_endpoint, server_endpoint) = channel_pair::<4>();
    let mut client = Client::new(client_endpoint, deadline_protocol());
    let mut server = Server::new(server_endpoint, deadline_protocol(), 1).unwrap();
    client.try_negotiate().unwrap();
    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::Bound(_))
    ));
    assert!(matches!(
        client.poll().unwrap(),
        Some(ClientEvent::Bound(_))
    ));

    assert_eq!(
        client.try_send_one_way(1, &[], 0, u64::MAX, [0; 16]),
        Err(RuntimeError::InvalidDeadline)
    );
    assert_eq!(
        client.try_send_one_way(1, &[], 0, 101, [0; 16]),
        Err(RuntimeError::InvalidDeadline)
    );
    client.try_send_one_way(1, &[], 0, 50, [0; 16]).unwrap();
    let validations_before_receive = DEADLINE_VALIDATIONS.load(Ordering::SeqCst);
    assert_eq!(server.poll(50).unwrap(), None);
    assert_eq!(
        DEADLINE_VALIDATIONS.load(Ordering::SeqCst),
        validations_before_receive + 1
    );

    client
        .try_send_one_way(2, &[], 50, u64::MAX, [0; 16])
        .unwrap();
    assert!(matches!(
        server.poll(50).unwrap(),
        Some(ServerEvent::OneWay { ordinal: 2, .. })
    ));
}

#[test]
fn one_way_record_is_attributed_from_connection_context() {
    let (mut producer, mut server, _, _) = connected::<4>(LOGGING_PROTOCOL_MINOR_WALL_TIME, 12);
    let trace_id = [0x5a; 16];
    assert_eq!(
        producer
            .try_log(
                record("storage", "flush completed"),
                LogDelivery::Reliable,
                trace_id,
            )
            .unwrap(),
        LogDisposition::Queued
    );
    assert_eq!(producer.client().outstanding_count(), 0);

    let (received_peer, body, received_trace) = match server.poll(0).unwrap().unwrap() {
        ServerEvent::OneWay {
            peer_context,
            ordinal,
            trace_id,
            body,
        } => {
            assert_eq!(ordinal, LOGGING_EMIT_ORDINAL);
            (peer_context, body, trace_id)
        }
        event => panic!("unexpected event: {event:?}"),
    };
    assert_eq!(server.executing_count(), 0);

    let mut collector = LoggingCollector::new(producer_identity());
    collector
        .dispatch(
            received_peer,
            body.as_slice(),
            &server.bound().unwrap().view().unwrap(),
            received_trace,
        )
        .unwrap();
    let stored = &collector.records()[0];
    assert_eq!(stored.producer, producer_identity());
    assert_eq!(stored.producer.service_generation, 77);
    assert_eq!(stored.collector_generation, 12);
    assert_ne!(
        stored.producer.service_generation,
        stored.collector_generation
    );
    assert_eq!(stored.severity, LogSeverity::Warning);
    assert_eq!(stored.privacy, PrivacyClass::SecuritySensitive);
    assert_eq!(stored.subsystem, "storage");
    assert_eq!(stored.message, "flush completed");
    assert_eq!(stored.trace_id, trace_id);
}

#[test]
fn peer_context_mismatch_is_rejected_and_secrets_are_not_retained() {
    let (mut producer, mut server, _, _) = connected::<4>(LOGGING_PROTOCOL_MINOR_WALL_TIME, 13);
    let mut secret = record("security", "raw credential material");
    secret.privacy = PrivacyClass::SecretNeverPersist;
    producer
        .try_log(secret, LogDelivery::Reliable, [0; 16])
        .unwrap();
    let (received_peer, body) = match server.poll(0).unwrap().unwrap() {
        ServerEvent::OneWay {
            peer_context, body, ..
        } => (peer_context, body),
        event => panic!("unexpected event: {event:?}"),
    };

    let mut wrong_identity = producer_identity();
    wrong_identity.peer_context = PeerContextId::new(999);
    let mut wrong_collector = LoggingCollector::new(wrong_identity);
    assert_eq!(
        wrong_collector.dispatch(
            received_peer,
            body.as_slice(),
            &server.bound().unwrap().view().unwrap(),
            [0; 16],
        ),
        Err(BodyError::MaterializationMismatch)
    );
    assert!(wrong_collector.records().is_empty());

    let mut unspecified_identity = producer_identity();
    unspecified_identity.peer_context = PeerContextId::UNSPECIFIED;
    let mut unspecified_collector = LoggingCollector::new(unspecified_identity);
    assert_eq!(
        unspecified_collector.dispatch(
            PeerContextId::UNSPECIFIED,
            body.as_slice(),
            &server.bound().unwrap().view().unwrap(),
            [0; 16],
        ),
        Err(BodyError::MaterializationMismatch)
    );

    let mut collector = LoggingCollector::with_capacity(producer_identity(), 1);
    collector
        .dispatch(
            received_peer,
            body.as_slice(),
            &server.bound().unwrap().view().unwrap(),
            [0; 16],
        )
        .unwrap();
    assert_eq!(collector.records()[0].message, SECRET_REDACTION);
    assert!(!collector.records()[0].message.contains("credential"));
    assert_eq!(
        collector.dispatch(
            received_peer,
            body.as_slice(),
            &server.bound().unwrap().view().unwrap(),
            [0; 16],
        ),
        Err(BodyError::LimitExceeded)
    );
    assert_eq!(collector.records().len(), 1);
}

#[test]
fn reliable_backpressure_is_retryable_and_best_effort_counts_drops() {
    let (mut producer, mut server, _, _) = connected::<1>(LOGGING_PROTOCOL_MINOR_WALL_TIME, 2);
    let first = record("kernel", "first");
    let second = record("kernel", "second");
    assert_eq!(
        producer
            .try_log(first, LogDelivery::BestEffort, [0; 16])
            .unwrap(),
        LogDisposition::Queued
    );
    assert_eq!(
        producer.try_log(second, LogDelivery::Reliable, [0; 16]),
        Err(RuntimeError::WouldBlock)
    );
    assert_eq!(producer.dropped_records(), 0);
    assert_eq!(
        producer
            .try_log(second, LogDelivery::BestEffort, [0; 16])
            .unwrap(),
        LogDisposition::Dropped
    );
    assert_eq!(producer.dropped_records(), 1);

    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::OneWay { .. })
    ));
    assert_eq!(
        producer
            .try_log(second, LogDelivery::Reliable, [0; 16])
            .unwrap(),
        LogDisposition::Queued
    );
}

#[test]
fn maximum_minor_one_record_exactly_fills_the_endpoint_profile() {
    let subsystem = "s".repeat(LOGGING_MAX_SUBSYSTEM_BYTES);
    let message = "m".repeat(LOGGING_MAX_MESSAGE_BYTES);
    let body = encode_log_record(
        record(&subsystem, &message),
        LOGGING_PROTOCOL_MINOR_WALL_TIME,
    )
    .unwrap();
    assert_eq!(body.len(), 192);
    let bytes = body.as_slice();
    assert_eq!(bytes[0], LogSeverity::Warning as u8);
    assert_eq!(bytes[1], PrivacyClass::SecuritySensitive as u8);
    assert_eq!(&bytes[2..8], &[0; 6]);
    assert_eq!(&bytes[16..20], &48_u32.to_le_bytes());
    assert_eq!(&bytes[20..24], &16_u32.to_le_bytes());
    assert_eq!(&bytes[24..28], &16_u32.to_le_bytes());
    assert_eq!(&bytes[32..36], &48_u32.to_le_bytes());
    assert_eq!(&bytes[36..40], &80_u32.to_le_bytes());
    assert_eq!(&bytes[40..44], &80_u32.to_le_bytes());
    assert_eq!(&bytes[48..52], &112_u32.to_le_bytes());
    assert_eq!(&bytes[52..54], &1_u16.to_le_bytes());
    assert_eq!(&bytes[56..60], &32_u32.to_le_bytes());
    assert_eq!(&bytes[160..164], &1_u32.to_le_bytes());
    assert_eq!(&bytes[168..172], &16_u32.to_le_bytes());
    assert_eq!(&bytes[172..176], &8_u32.to_le_bytes());
    assert_eq!(&bytes[184..192], &987_654_u64.to_le_bytes());

    let mut base_record = record(&subsystem, &message);
    base_record.wall_time_unix_ns = None;
    assert_eq!(
        encode_log_record(base_record, LOGGING_PROTOCOL_MINOR_BASE)
            .unwrap()
            .len(),
        160
    );

    let (mut producer, _server, _, collector_control) =
        connected::<2>(LOGGING_PROTOCOL_MINOR_WALL_TIME, 3);
    producer
        .try_log(record(&subsystem, &message), LogDelivery::Reliable, [0; 16])
        .unwrap();
    let packet = collector_control.incoming_packet(0).unwrap();
    assert_eq!(packet.len(), 256);
    let header = Header::decode_prefix(&packet).unwrap();
    assert_eq!(header.kind, PacketKind::OneWay);
    assert_eq!(header.body_bytes, 192);
    assert_eq!(header.transaction_id, 0);
    assert_eq!(header.deadline_ns, u64::MAX);
}

#[test]
fn minor_version_controls_wall_time_extension() {
    let (mut producer, mut server, _, _) = connected::<2>(LOGGING_PROTOCOL_MINOR_BASE, 4);
    assert_eq!(producer.client().bound().unwrap().minor(), 0);
    assert_eq!(server.bound().unwrap().minor(), 0);
    assert_eq!(
        producer.try_log(
            record("core", "with wall time"),
            LogDelivery::Reliable,
            [0; 16],
        ),
        Err(RuntimeError::Body(BodyError::FieldUnavailable))
    );

    let mut without_wall = record("core", "monotonic only");
    without_wall.wall_time_unix_ns = None;
    assert_eq!(
        producer
            .try_log(without_wall, LogDelivery::Reliable, [0; 16])
            .unwrap(),
        LogDisposition::Queued
    );
    let body = match server.poll(0).unwrap().unwrap() {
        ServerEvent::OneWay { body, .. } => body,
        event => panic!("unexpected event: {event:?}"),
    };
    let decoded =
        decode_log_record(body.as_slice(), &server.bound().unwrap().view().unwrap()).unwrap();
    assert_eq!(decoded.wall_time_unix_ns, None);
}

#[test]
fn server_protocol_through_helper_caps_advertised_versions() {
    let (producer_endpoint, collector_endpoint) = channel_pair::<2>();
    let mut producer = LoggingProducer::new(producer_endpoint);
    let mut server = Server::new_with_peer_context(
        collector_endpoint,
        logging_protocol_through(LOGGING_PROTOCOL_MINOR_BASE),
        41,
        peer_context(),
    )
    .unwrap();
    producer.try_negotiate().unwrap();
    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::Bound(_))
    ));
    assert!(matches!(
        producer.poll().unwrap(),
        Some(ClientEvent::Bound(_))
    ));
    assert_eq!(server.bound().unwrap().minor(), LOGGING_PROTOCOL_MINOR_BASE);
    assert_eq!(
        producer.client().bound().unwrap().minor(),
        LOGGING_PROTOCOL_MINOR_BASE
    );
}

#[test]
fn unknown_extension_fields_are_opaque_after_complete_validation() {
    let (_producer, server, _, _) = connected::<2>(LOGGING_PROTOCOL_MINOR_WALL_TIME, 5);
    let mut bytes = [0; 112];
    let mut encoder =
        BodyEncoder::new(&mut bytes, 112, 64, BodyLimits::ENDPOINT_PROTOTYPE).unwrap();
    let root = encoder.root();
    root.write_u8(0, LogSeverity::Info as u8).unwrap();
    root.write_u8(1, PrivacyClass::Public as u8).unwrap();
    root.write_u64(8, 55).unwrap();
    root.string(16, 16, "core").unwrap();
    root.string(32, 80, "future").unwrap();
    root.table(48, 1, 4, |table| {
        table.field(2, 8, |value| value.write_u64(0, 0xfeed_beef))
    })
    .unwrap();
    encoder.finish().unwrap();

    let decoded = decode_log_record(&bytes, &server.bound().unwrap().view().unwrap()).unwrap();
    assert_eq!(decoded.message, "future");
    assert_eq!(decoded.wall_time_unix_ns, None);
}

#[test]
fn malformed_records_close_before_collector_dispatch() {
    let (mut producer, mut server, _, collector_control) =
        connected::<2>(LOGGING_PROTOCOL_MINOR_WALL_TIME, 6);
    let collector = LoggingCollector::new(producer_identity());
    collector_control.set_hold_incoming(true);
    producer
        .try_log(
            record("core", "bad severity"),
            LogDelivery::Reliable,
            [0; 16],
        )
        .unwrap();
    assert!(collector_control.corrupt_held(0, NSWP_HEADER_BYTES, 9));
    collector_control.set_hold_incoming(false);
    assert!(collector_control.release_held(0));
    assert_eq!(
        server.poll(0),
        Err(RuntimeError::Closed(CloseReason::ProtocolError))
    );
    assert_eq!(collector.records().len(), 0);
}

#[test]
fn one_way_deadlines_and_method_kinds_are_strict() {
    let (mut producer, mut server, _, collector_control) =
        connected::<2>(LOGGING_PROTOCOL_MINOR_WALL_TIME, 7);
    let body = encode_log_record(
        record("core", "wrong kind"),
        LOGGING_PROTOCOL_MINOR_WALL_TIME,
    )
    .unwrap();
    assert_eq!(
        producer
            .client_mut()
            .try_call(LOGGING_EMIT_ORDINAL, body.as_slice(), 0, u64::MAX, [0; 16],),
        Err(RuntimeError::WrongMethodKind)
    );

    collector_control.set_hold_incoming(true);
    producer
        .try_log(record("core", "deadline"), LogDelivery::Reliable, [0; 16])
        .unwrap();
    assert!(collector_control.corrupt_held(0, offset::DEADLINE_NS, 1));
    collector_control.set_hold_incoming(false);
    assert!(collector_control.release_held(0));
    assert_eq!(
        server.poll(0),
        Err(RuntimeError::Closed(CloseReason::ProtocolError))
    );
}

#[test]
fn severity_privacy_and_string_bounds_are_enforced() {
    let (_producer, server, _, _) = connected::<2>(LOGGING_PROTOCOL_MINOR_WALL_TIME, 8);
    let bound = server.bound().unwrap().view().unwrap();
    for raw in 0..=8 {
        let severity = LogSeverity::try_from(raw).unwrap();
        let mut value = record("core", "value");
        value.severity = severity;
        let body = encode_log_record(value, LOGGING_PROTOCOL_MINOR_WALL_TIME).unwrap();
        assert_eq!(
            decode_log_record(body.as_slice(), &bound).unwrap().severity,
            severity
        );
    }
    for raw in 0..=4 {
        let privacy = PrivacyClass::try_from(raw).unwrap();
        let mut value = record("core", "value");
        value.privacy = privacy;
        let body = encode_log_record(value, LOGGING_PROTOCOL_MINOR_WALL_TIME).unwrap();
        assert_eq!(
            decode_log_record(body.as_slice(), &bound).unwrap().privacy,
            privacy
        );
    }

    let subsystem = "s".repeat(LOGGING_MAX_SUBSYSTEM_BYTES + 1);
    assert_eq!(
        encode_log_record(
            record(&subsystem, "message"),
            LOGGING_PROTOCOL_MINOR_WALL_TIME,
        ),
        Err(BodyError::LimitExceeded)
    );
    let message = "m".repeat(LOGGING_MAX_MESSAGE_BYTES + 1);
    assert_eq!(
        encode_log_record(record("core", &message), LOGGING_PROTOCOL_MINOR_WALL_TIME,),
        Err(BodyError::LimitExceeded)
    );
}

#[test]
fn producer_reports_peer_closure_without_counting_a_drop() {
    let (mut producer, mut server, _, _) = connected::<2>(LOGGING_PROTOCOL_MINOR_WALL_TIME, 9);
    server.transport_mut().close();
    assert_eq!(
        producer.try_log(record("core", "closed"), LogDelivery::BestEffort, [0; 16],),
        Err(RuntimeError::PeerClosed)
    );
    assert_eq!(producer.dropped_records(), 0);
    assert_eq!(producer.client().phase(), ConnectionPhase::Closed);
}
