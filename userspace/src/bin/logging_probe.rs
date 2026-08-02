#![no_std]
#![no_main]

use nswp_logging::{
    COLLECTOR_SECRET_REDACTION, CollectorStats, EventId, HistoryRecordView,
    LOGGING_MAX_MESSAGE_BYTES, LOGGING_MAX_SUBSYSTEM_BYTES, LOGGING_PROTOCOL_MINOR_COLLECTOR_READS,
    LogDelivery, LogDisposition, LogRecord, LogSeverity, LoggingProducer, PrivacyClass, RecordId,
    decode_collector_stats_client_response, decode_history_read_client_response,
};
use nswp_runtime::{ClientEvent, RuntimeError};
use userspace::{
    abi::INIT_PROCESS_ID,
    args::Args,
    endpoint_transport::EndpointTransport,
    ipc::{self, ObjectKind, Rights},
    syscall,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SEND_HANDLE: u64 = 1;
const RECEIVE_HANDLE: u64 = 2;
const STATUS_HANDLE: u64 = 3;
const CONTROL_HANDLE: u64 = 4;
const MAX_YIELDS: u32 = 65_536;
const QUERY_NOW_NS: u64 = 0;
const QUERY_DEADLINE_NS: u64 = 1_000_000;

const PROBE_SUBSYSTEM: &str = "logging-probe/v1";
const PROBE_MESSAGE: &str = concat!(
    "native-nswp-log-",
    "native-nswp-log-",
    "native-nswp-log-",
    "native-nswp-log-",
);
const SECRET_SUBSYSTEM: &str = "logging-secret";
const SECRET_INPUT: &str = "raw-secret-must-never-enter-history";
const STRESS_SUBSYSTEM: &str = "logging-stress";
const STRESS_MESSAGE: &str = "collector-stress-record";
const STRESS_SECRET_INPUT: &str = "stress-secret-must-never-enter-history";
const RESTART_SUBSYSTEM: &str = "logging-restart";
const RESTART_MESSAGE: &str = "collector-reset-record";

const BOUND_MARKER: &[u8] = b"logging-probe: bound";
const FILL_QUEUE_CONTROL: &[u8] = b"logging-probe: fill queue";
const BACKPRESSURE_MARKER: &[u8] = b"logging-probe: backpressure verified";

const _: () = assert!(PROBE_SUBSYSTEM.len() == LOGGING_MAX_SUBSYSTEM_BYTES);
const _: () = assert!(PROBE_MESSAGE.len() == LOGGING_MAX_MESSAGE_BYTES);

// Stable event type: 4b9b47d8-309b-48a8-bc41-7e63c0d912c5.
const PUBLIC_EVENT_TYPE_ID: EventId = event_id([
    0x4b, 0x9b, 0x47, 0xd8, 0x30, 0x9b, 0x48, 0xa8, 0xbc, 0x41, 0x7e, 0x63, 0xc0, 0xd9, 0x12, 0xc5,
]);
// Stable event type: 8f3a9f8b-39aa-4c52-91b0-4747f78c850d.
const SECRET_EVENT_TYPE_ID: EventId = event_id([
    0x8f, 0x3a, 0x9f, 0x8b, 0x39, 0xaa, 0x4c, 0x52, 0x91, 0xb0, 0x47, 0x47, 0xf7, 0x8c, 0x85, 0x0d,
]);
// Stable event type: d284858f-0430-47ae-a35c-b5356bca8970.
const STRESS_EVENT_TYPE_ID: EventId = event_id([
    0xd2, 0x84, 0x85, 0x8f, 0x04, 0x30, 0x47, 0xae, 0xa3, 0x5c, 0xb5, 0x35, 0x6b, 0xca, 0x89, 0x70,
]);
// Stable event type: 51c1e39a-17d5-4697-9f11-ef45ce95bac4.
const STRESS_SECRET_EVENT_TYPE_ID: EventId = event_id([
    0x51, 0xc1, 0xe3, 0x9a, 0x17, 0xd5, 0x46, 0x97, 0x9f, 0x11, 0xef, 0x45, 0xce, 0x95, 0xba, 0xc4,
]);
// Stable event type: 2e2a5872-af68-4a98-897d-1fc50f0d57c5.
const RESTART_EVENT_TYPE_ID: EventId = event_id([
    0x2e, 0x2a, 0x58, 0x72, 0xaf, 0x68, 0x4a, 0x98, 0x89, 0x7d, 0x1f, 0xc5, 0x0f, 0x0d, 0x57, 0xc5,
]);

const fn event_id(bytes: [u8; 16]) -> EventId {
    match EventId::from_bytes(bytes) {
        Ok(event_id) => event_id,
        Err(_) => panic!("logging probe event type ID must be a canonical UUIDv4"),
    }
}

type Producer = LoggingProducer<'static, EndpointTransport>;

#[derive(Clone, Copy)]
enum ProbeMode {
    Basic,
    CollectorStress,
    AfterRestart,
}

#[derive(Clone, Copy)]
struct ExpectedStats {
    received: u64,
    retained: u64,
    capacity: u64,
    evicted: u64,
    dropped: u64,
    redacted: u64,
    oldest: Option<u64>,
    newest: Option<u64>,
}

#[derive(Clone, Copy)]
struct ExpectedHistory<'a> {
    record_id: u64,
    source_process_id: u64,
    event_id: EventId,
    severity: LogSeverity,
    privacy: PrivacyClass,
    monotonic_time_ns: u64,
    subsystem: &'a str,
    message: &'a str,
    wall_time_unix_ns: Option<u64>,
    trace_id: [u8; 16],
}

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let mode = match (arguments.len(), arguments.get(1)) {
        (1, None) | (2, Some(b"basic")) => ProbeMode::Basic,
        (2, Some(b"collector-stress")) => ProbeMode::CollectorStress,
        (2, Some(b"after-restart")) => ProbeMode::AfterRestart,
        _ => syscall::exit(2),
    };
    let process_id = match syscall::getpid() {
        Ok(process_id) if process_id != 0 => process_id,
        _ => syscall::exit(3),
    };
    if matches!(mode, ProbeMode::CollectorStress) {
        validate_stress_handles();
    }
    let transport = match EndpointTransport::new(SEND_HANDLE, RECEIVE_HANDLE) {
        Ok(transport) => transport,
        Err(_) => syscall::exit(4),
    };
    let mut producer = LoggingProducer::new(transport);
    negotiate(&mut producer);

    match mode {
        ProbeMode::Basic => run_basic(&mut producer, process_id),
        ProbeMode::CollectorStress => run_collector_stress(&mut producer, process_id),
        ProbeMode::AfterRestart => run_after_restart(&mut producer, process_id),
    }
    syscall::exit(0)
}

fn negotiate(producer: &mut Producer) {
    let mut remaining = MAX_YIELDS;
    loop {
        match producer.try_negotiate() {
            Ok(()) => break,
            Err(RuntimeError::WouldBlock) => yield_bounded(&mut remaining, 5),
            Err(_) => syscall::exit(6),
        }
    }
    let mut remaining = MAX_YIELDS;
    loop {
        match producer.poll() {
            Ok(Some(ClientEvent::Bound(bound)))
                if bound.minor() == LOGGING_PROTOCOL_MINOR_COLLECTOR_READS =>
            {
                return;
            }
            Ok(None) => yield_bounded(&mut remaining, 7),
            Ok(Some(_)) | Err(_) => syscall::exit(8),
        }
    }
}

fn run_basic(producer: &mut Producer, process_id: u64) {
    let public_trace = [0x11; 16];
    let secret_trace = [0x22; 16];
    send_reliable(
        producer,
        LogRecord {
            event_id: PUBLIC_EVENT_TYPE_ID,
            severity: LogSeverity::Info,
            privacy: PrivacyClass::Public,
            monotonic_time_ns: 0x0102_0304_0506_0708,
            subsystem: PROBE_SUBSYSTEM,
            message: PROBE_MESSAGE,
            wall_time_unix_ns: Some(0x1112_1314_1516_1718),
        },
        public_trace,
        9,
    );
    send_reliable(
        producer,
        LogRecord {
            event_id: SECRET_EVENT_TYPE_ID,
            severity: LogSeverity::Warning,
            privacy: PrivacyClass::SecretNeverPersist,
            monotonic_time_ns: 2,
            subsystem: SECRET_SUBSYSTEM,
            message: SECRET_INPUT,
            wall_time_unix_ns: None,
        },
        secret_trace,
        10,
    );

    verify_stats(
        query_stats(producer, 11),
        ExpectedStats {
            received: 2,
            retained: 2,
            capacity: 64,
            evicted: 0,
            dropped: 0,
            redacted: 1,
            oldest: Some(1),
            newest: Some(2),
        },
        12,
    );
    let first = expect_history(
        producer,
        None,
        Some(ExpectedHistory {
            record_id: 1,
            source_process_id: process_id,
            event_id: PUBLIC_EVENT_TYPE_ID,
            severity: LogSeverity::Info,
            privacy: PrivacyClass::Public,
            monotonic_time_ns: 0x0102_0304_0506_0708,
            subsystem: PROBE_SUBSYSTEM,
            message: PROBE_MESSAGE,
            wall_time_unix_ns: Some(0x1112_1314_1516_1718),
            trace_id: public_trace,
        }),
        13,
    );
    let second = expect_history(
        producer,
        first,
        Some(ExpectedHistory {
            record_id: 2,
            source_process_id: process_id,
            event_id: SECRET_EVENT_TYPE_ID,
            severity: LogSeverity::Warning,
            privacy: PrivacyClass::SecretNeverPersist,
            monotonic_time_ns: 2,
            subsystem: SECRET_SUBSYSTEM,
            message: COLLECTOR_SECRET_REDACTION,
            wall_time_unix_ns: None,
            trace_id: secret_trace,
        }),
        14,
    );
    if COLLECTOR_SECRET_REDACTION == SECRET_INPUT {
        syscall::exit(15);
    }
    expect_history(producer, second, None, 16);
}

fn run_collector_stress(producer: &mut Producer, process_id: u64) {
    if ipc::send(STATUS_HANDLE, BOUND_MARKER, None).is_err() {
        syscall::exit(20);
    }
    wait_for_fill_control();

    for sequence in 1..=8 {
        match producer.try_log(stress_record(sequence), LogDelivery::Reliable, [0x33; 16]) {
            Ok(LogDisposition::Queued) => {}
            Ok(LogDisposition::Dropped) | Err(_) => syscall::exit(21),
        }
    }
    let ninth = stress_record(9);
    if producer.try_log(ninth, LogDelivery::BestEffort, [0x33; 16]) != Ok(LogDisposition::Dropped)
        || producer.dropped_records() != 1
        || producer.try_log(ninth, LogDelivery::Reliable, [0x33; 16])
            != Err(RuntimeError::WouldBlock)
    {
        syscall::exit(22);
    }
    if ipc::send(STATUS_HANDLE, BACKPRESSURE_MARKER, None).is_err() {
        syscall::exit(23);
    }

    send_reliable(producer, ninth, [0x33; 16], 24);
    for sequence in 10..=65 {
        send_reliable(producer, stress_record(sequence), [0x33; 16], 25);
    }
    if producer.dropped_records() != 1 {
        syscall::exit(26);
    }

    verify_stats(
        query_stats(producer, 27),
        ExpectedStats {
            received: 65,
            retained: 64,
            capacity: 64,
            evicted: 1,
            dropped: 0,
            redacted: 1,
            oldest: Some(2),
            newest: Some(65),
        },
        28,
    );
    expect_history(
        producer,
        None,
        Some(ExpectedHistory {
            record_id: 2,
            source_process_id: process_id,
            event_id: STRESS_EVENT_TYPE_ID,
            severity: LogSeverity::Trace,
            privacy: PrivacyClass::Public,
            monotonic_time_ns: 2,
            subsystem: STRESS_SUBSYSTEM,
            message: STRESS_MESSAGE,
            wall_time_unix_ns: None,
            trace_id: [0x33; 16],
        }),
        29,
    );
    let newest = expect_history(
        producer,
        Some(record_id(64, 30)),
        Some(ExpectedHistory {
            record_id: 65,
            source_process_id: process_id,
            event_id: STRESS_SECRET_EVENT_TYPE_ID,
            severity: LogSeverity::Warning,
            privacy: PrivacyClass::SecretNeverPersist,
            monotonic_time_ns: 65,
            subsystem: STRESS_SUBSYSTEM,
            message: COLLECTOR_SECRET_REDACTION,
            wall_time_unix_ns: None,
            trace_id: [0x33; 16],
        }),
        31,
    );
    if COLLECTOR_SECRET_REDACTION == STRESS_SECRET_INPUT {
        syscall::exit(32);
    }
    expect_history(producer, newest, None, 33);
}

fn run_after_restart(producer: &mut Producer, process_id: u64) {
    send_reliable(
        producer,
        LogRecord {
            event_id: RESTART_EVENT_TYPE_ID,
            severity: LogSeverity::Notice,
            privacy: PrivacyClass::Public,
            monotonic_time_ns: 1,
            subsystem: RESTART_SUBSYSTEM,
            message: RESTART_MESSAGE,
            wall_time_unix_ns: None,
        },
        [0x44; 16],
        40,
    );
    verify_stats(
        query_stats(producer, 41),
        ExpectedStats {
            received: 1,
            retained: 1,
            capacity: 64,
            evicted: 0,
            dropped: 0,
            redacted: 0,
            oldest: Some(1),
            newest: Some(1),
        },
        42,
    );
    let first = expect_history(
        producer,
        None,
        Some(ExpectedHistory {
            record_id: 1,
            source_process_id: process_id,
            event_id: RESTART_EVENT_TYPE_ID,
            severity: LogSeverity::Notice,
            privacy: PrivacyClass::Public,
            monotonic_time_ns: 1,
            subsystem: RESTART_SUBSYSTEM,
            message: RESTART_MESSAGE,
            wall_time_unix_ns: None,
            trace_id: [0x44; 16],
        }),
        43,
    );
    expect_history(producer, first, None, 44);
}

fn stress_record(sequence: u64) -> LogRecord<'static> {
    let secret = sequence == 65;
    LogRecord {
        event_id: if secret {
            STRESS_SECRET_EVENT_TYPE_ID
        } else {
            STRESS_EVENT_TYPE_ID
        },
        severity: if secret {
            LogSeverity::Warning
        } else {
            LogSeverity::Trace
        },
        privacy: if secret {
            PrivacyClass::SecretNeverPersist
        } else {
            PrivacyClass::Public
        },
        monotonic_time_ns: sequence,
        subsystem: STRESS_SUBSYSTEM,
        message: if secret {
            STRESS_SECRET_INPUT
        } else {
            STRESS_MESSAGE
        },
        wall_time_unix_ns: None,
    }
}

fn send_reliable(producer: &mut Producer, record: LogRecord<'_>, trace_id: [u8; 16], code: u64) {
    let mut remaining = MAX_YIELDS;
    loop {
        match producer.try_log(record, LogDelivery::Reliable, trace_id) {
            Ok(LogDisposition::Queued) => return,
            Err(RuntimeError::WouldBlock) => yield_bounded(&mut remaining, code),
            Ok(LogDisposition::Dropped) | Err(_) => syscall::exit(code),
        }
    }
}

fn query_stats(producer: &mut Producer, code: u64) -> CollectorStats {
    let mut remaining = MAX_YIELDS;
    let transaction = loop {
        match producer.try_get_collector_stats(QUERY_NOW_NS, QUERY_DEADLINE_NS, [0x55; 16]) {
            Ok(transaction) => break transaction,
            Err(RuntimeError::WouldBlock) => yield_bounded(&mut remaining, code),
            Err(_) => syscall::exit(code),
        }
    };
    let event = wait_for_response(producer, code);
    let bound = match producer
        .client()
        .bound()
        .and_then(|bound| bound.view().ok())
    {
        Some(bound) => bound,
        None => syscall::exit(code),
    };
    match decode_collector_stats_client_response(&event, transaction, &bound) {
        Ok(stats) => stats,
        Err(_) => syscall::exit(code),
    }
}

fn expect_history(
    producer: &mut Producer,
    after_record_id: Option<RecordId>,
    expected: Option<ExpectedHistory<'_>>,
    code: u64,
) -> Option<RecordId> {
    let mut remaining = MAX_YIELDS;
    let transaction = loop {
        match producer.try_read_history(
            after_record_id,
            QUERY_NOW_NS,
            QUERY_DEADLINE_NS,
            [0x66; 16],
        ) {
            Ok(transaction) => break transaction,
            Err(RuntimeError::WouldBlock) => yield_bounded(&mut remaining, code),
            Err(_) => syscall::exit(code),
        }
    };
    let event = wait_for_response(producer, code);
    let bound = match producer
        .client()
        .bound()
        .and_then(|bound| bound.view().ok())
    {
        Some(bound) => bound,
        None => syscall::exit(code),
    };
    let actual = match decode_history_read_client_response(&event, transaction, &bound) {
        Ok(actual) => actual,
        Err(_) => syscall::exit(code),
    };
    match (actual, expected) {
        (None, None) => None,
        (Some(actual), Some(expected)) if history_matches(actual, expected) => {
            Some(actual.record_id)
        }
        _ => syscall::exit(code),
    }
}

fn wait_for_response(producer: &mut Producer, code: u64) -> ClientEvent {
    let mut remaining = MAX_YIELDS;
    loop {
        match producer.poll() {
            Ok(Some(event @ ClientEvent::Response { .. })) => return event,
            Ok(None) => yield_bounded(&mut remaining, code),
            Ok(Some(_)) | Err(_) => syscall::exit(code),
        }
    }
}

fn verify_stats(actual: CollectorStats, expected: ExpectedStats, code: u64) {
    if actual.received_records != expected.received
        || actual.retained_records != expected.retained
        || actual.capacity_records != expected.capacity
        || actual.evicted_records != expected.evicted
        || actual.dropped_records != expected.dropped
        || actual.redacted_records != expected.redacted
        || actual.oldest_record_id.map(RecordId::get) != expected.oldest
        || actual.newest_record_id.map(RecordId::get) != expected.newest
    {
        syscall::exit(code);
    }
}

fn history_matches(actual: HistoryRecordView<'_>, expected: ExpectedHistory<'_>) -> bool {
    actual.record_id.get() == expected.record_id
        && actual.source_process_id == expected.source_process_id
        && actual.event_id == expected.event_id
        && actual.severity == expected.severity
        && actual.privacy == expected.privacy
        && actual.monotonic_time_ns == expected.monotonic_time_ns
        && actual.subsystem == expected.subsystem
        && actual.message == expected.message
        && actual.wall_time_unix_ns == expected.wall_time_unix_ns
        && actual.trace_id == expected.trace_id
}

fn validate_stress_handles() {
    if !matches!(
        ipc::info(STATUS_HANDLE),
        Ok(info) if info.kind == ObjectKind::Endpoint && info.rights == Rights::SEND
    ) || !matches!(
        ipc::info(CONTROL_HANDLE),
        Ok(info) if info.kind == ObjectKind::Endpoint && info.rights == Rights::RECEIVE
    ) {
        syscall::exit(50);
    }
}

fn wait_for_fill_control() {
    let mut remaining = MAX_YIELDS;
    let mut buffer = [0_u8; 64];
    loop {
        match ipc::try_receive(CONTROL_HANDLE, &mut buffer) {
            Ok(message)
                if message.sender_process_id == INIT_PROCESS_ID
                    && message.capability.is_none()
                    && message.bytes == FILL_QUEUE_CONTROL.len()
                    && &buffer[..message.bytes] == FILL_QUEUE_CONTROL =>
            {
                return;
            }
            Ok(_) | Err(ipc::Error::TRY_AGAIN) => yield_bounded(&mut remaining, 51),
            Err(_) => syscall::exit(51),
        }
    }
}

fn record_id(value: u64, code: u64) -> RecordId {
    match RecordId::new(value) {
        Ok(record_id) => record_id,
        Err(_) => syscall::exit(code),
    }
}

fn yield_bounded(remaining: &mut u32, code: u64) {
    if *remaining == 0 {
        syscall::exit(code);
    }
    *remaining -= 1;
    if syscall::yield_now().is_err() {
        syscall::exit(code);
    }
}
