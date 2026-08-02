#![no_std]
#![no_main]

use nswp_logging::{
    EventId, LOGGING_MAX_MESSAGE_BYTES, LOGGING_MAX_SUBSYSTEM_BYTES, LogDelivery, LogDisposition,
    LogRecord, LogSeverity, LoggingProducer, PrivacyClass,
};
use nswp_runtime::{ClientEvent, RuntimeError};
use userspace::{endpoint_transport::EndpointTransport, syscall};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SEND_HANDLE: u64 = 1;
const RECEIVE_HANDLE: u64 = 2;
const PROBE_SUBSYSTEM: &str = "logging-probe/v1";
const PROBE_MESSAGE: &str = concat!(
    "native-nswp-log-",
    "native-nswp-log-",
    "native-nswp-log-",
    "native-nswp-log-",
);
const _: () = assert!(PROBE_SUBSYSTEM.len() == LOGGING_MAX_SUBSYSTEM_BYTES);
const _: () = assert!(PROBE_MESSAGE.len() == LOGGING_MAX_MESSAGE_BYTES);

// Stable event type: 4b9b47d8-309b-48a8-bc41-7e63c0d912c5.
const PROBE_EVENT_TYPE_ID: EventId = match EventId::from_bytes([
    0x4b, 0x9b, 0x47, 0xd8, 0x30, 0x9b, 0x48, 0xa8, 0xbc, 0x41, 0x7e, 0x63, 0xc0, 0xd9, 0x12, 0xc5,
]) {
    Ok(event_id) => event_id,
    Err(_) => panic!("logging probe event type ID must be a canonical UUIDv4"),
};

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    let transport = match EndpointTransport::new(SEND_HANDLE, RECEIVE_HANDLE) {
        Ok(transport) => transport,
        Err(_) => syscall::exit(2),
    };
    let mut producer = LoggingProducer::new(transport);

    loop {
        match producer.try_negotiate() {
            Ok(()) => break,
            Err(RuntimeError::WouldBlock) => yield_or_exit(3),
            Err(_) => syscall::exit(4),
        }
    }
    loop {
        match producer.poll() {
            Ok(Some(ClientEvent::Bound(_))) => break,
            Ok(None) => yield_or_exit(5),
            Ok(Some(_)) | Err(_) => syscall::exit(6),
        }
    }

    let record = LogRecord {
        event_id: PROBE_EVENT_TYPE_ID,
        severity: LogSeverity::Info,
        privacy: PrivacyClass::Public,
        monotonic_time_ns: 0x0102_0304_0506_0708,
        subsystem: PROBE_SUBSYSTEM,
        message: PROBE_MESSAGE,
        wall_time_unix_ns: Some(0x1112_1314_1516_1718),
    };
    loop {
        match producer.try_log(record, LogDelivery::Reliable, [0; 16]) {
            Ok(LogDisposition::Queued) => syscall::exit(0),
            Err(RuntimeError::WouldBlock) => yield_or_exit(7),
            Ok(LogDisposition::Dropped) | Err(_) => syscall::exit(8),
        }
    }
}

fn yield_or_exit(code: u64) {
    if syscall::yield_now().is_err() {
        syscall::exit(code);
    }
}
