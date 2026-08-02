#![no_std]
#![no_main]

use nswp_logging::{PrivacyClass, decode_log_record, logging_protocol};
use nswp_runtime::{Server, ServerEvent};
use userspace::{
    endpoint_transport::EndpointTransport,
    ipc::{self, ObjectKind, Rights},
    syscall,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const READY_HANDLE: u64 = 1;
const RECEIVE_HANDLE: u64 = 2;
const SEND_HANDLE: u64 = 3;
const READY_MESSAGE: &[u8] = b"service-ready: logging";
const PROBE_RECORD_DECODED: &[u8] = b"logging-probe: record decoded";
const RECORD_PREFIX: &[u8] = b"logging-service: ";
const RECORD_SEPARATOR: &[u8] = b": ";
const RECORD_SUFFIX: &[u8] = b"\n";
const SECRET_REDACTION: &[u8] = b"[redacted: secret-never-persist]";
const DECODED_MARKER: &[u8] = b"logging-service: decoded native NSWP endpoint record\n";

extern "C" fn rust_main(_initial_stack: *const usize) -> ! {
    if !matches!(
        ipc::info(READY_HANDLE),
        Ok(info) if info.kind == ObjectKind::Endpoint && info.rights == Rights::SEND
    ) {
        syscall::exit(2);
    }
    let transport = match EndpointTransport::new(SEND_HANDLE, RECEIVE_HANDLE) {
        Ok(transport) => transport,
        Err(_) => syscall::exit(3),
    };
    let generation = match syscall::getpid() {
        Ok(generation) if generation != 0 => generation,
        _ => syscall::exit(4),
    };
    let mut server = match Server::new(transport, logging_protocol(), generation) {
        Ok(server) => server,
        Err(_) => syscall::exit(5),
    };
    if ipc::send(READY_HANDLE, READY_MESSAGE, None).is_err() {
        syscall::exit(6);
    }

    loop {
        match server.poll(0) {
            Ok(Some(ServerEvent::Bound(_))) => {}
            Ok(Some(ServerEvent::OneWay { body, .. })) => {
                let bound = match server.bound().and_then(|bound| bound.view().ok()) {
                    Some(bound) => bound,
                    None => syscall::exit(7),
                };
                let record = match decode_log_record(body.as_slice(), &bound) {
                    Ok(record) => record,
                    Err(_) => syscall::exit(8),
                };
                let message = if record.privacy == PrivacyClass::SecretNeverPersist {
                    SECRET_REDACTION
                } else {
                    record.message.as_bytes()
                };
                if syscall::write_all(syscall::STDOUT, RECORD_PREFIX).is_err()
                    || syscall::write_all(syscall::STDOUT, record.subsystem.as_bytes()).is_err()
                    || syscall::write_all(syscall::STDOUT, RECORD_SEPARATOR).is_err()
                    || syscall::write_all(syscall::STDOUT, message).is_err()
                    || syscall::write_all(syscall::STDOUT, RECORD_SUFFIX).is_err()
                    || syscall::write_all(syscall::STDOUT, DECODED_MARKER).is_err()
                    || ipc::send(READY_HANDLE, PROBE_RECORD_DECODED, None).is_err()
                {
                    syscall::exit(9);
                }
            }
            Ok(Some(ServerEvent::Request { .. } | ServerEvent::Canceled { .. })) => {
                syscall::exit(10)
            }
            Ok(None) => {
                if syscall::yield_now().is_err() {
                    syscall::exit(11);
                }
            }
            Err(_) => syscall::exit(12),
        }
    }
}
