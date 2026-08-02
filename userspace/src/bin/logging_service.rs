#![no_std]
#![no_main]

use nswp_logging::{
    CollectDisposition, CollectorRequest, FixedLoggingCollector, LOGGING_EMIT_ORDINAL, LogSeverity,
    RecordId, decode_collector_request, decode_log_record, logging_protocol,
    respond_collector_stats, respond_history,
};
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
const STARTED_MARKER: &[u8] = b"logging-service: fixed collector ready\n";
const RECORD_PREFIX: &[u8] = b"logging-service: retained: ";
const RECORD_SEPARATOR: &[u8] = b": ";
const RECORD_SUFFIX: &[u8] = b"\n";
const COLLECTOR_CAPACITY: usize = 64;

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
    let mut collector = match FixedLoggingCollector::<COLLECTOR_CAPACITY>::new(generation) {
        Ok(collector) => collector,
        Err(_) => syscall::exit(5),
    };
    let mut server = match Server::new(transport, logging_protocol(), generation) {
        Ok(server) => server,
        Err(_) => syscall::exit(6),
    };
    if ipc::send(READY_HANDLE, READY_MESSAGE, None).is_err()
        || syscall::write_all(syscall::STDOUT, STARTED_MARKER).is_err()
    {
        syscall::exit(7);
    }

    loop {
        match server.poll(0) {
            Ok(Some(ServerEvent::Bound(_))) => {}
            Ok(Some(ServerEvent::OneWay {
                ordinal,
                trace_id,
                body,
                ..
            })) => {
                if ordinal != LOGGING_EMIT_ORDINAL {
                    syscall::exit(8);
                }
                let source_process_id = match server.transport().peer_process_id() {
                    Some(process_id) if process_id != 0 => process_id,
                    _ => syscall::exit(9),
                };
                let bound = match server.bound().and_then(|bound| bound.view().ok()) {
                    Some(bound) => bound,
                    None => syscall::exit(10),
                };
                let record = match decode_log_record(body.as_slice(), &bound) {
                    Ok(record) => record,
                    Err(_) => syscall::exit(11),
                };
                let record_id = match collector.collect(source_process_id, record, trace_id) {
                    Ok(CollectDisposition::Accepted { record_id, .. }) => record_id,
                    Ok(CollectDisposition::Dropped) | Err(_) => syscall::exit(12),
                };
                let retained = match retained_record(&collector, record_id) {
                    Some(retained) => retained,
                    None => syscall::exit(13),
                };
                if !matches!(retained.severity, LogSeverity::Trace | LogSeverity::Debug)
                    && (syscall::write_all(syscall::STDOUT, RECORD_PREFIX).is_err()
                        || syscall::write_all(syscall::STDOUT, retained.subsystem.as_bytes())
                            .is_err()
                        || syscall::write_all(syscall::STDOUT, RECORD_SEPARATOR).is_err()
                        || syscall::write_all(syscall::STDOUT, retained.message.as_bytes())
                            .is_err()
                        || syscall::write_all(syscall::STDOUT, RECORD_SUFFIX).is_err())
                {
                    syscall::exit(14);
                }
            }
            Ok(Some(ServerEvent::Request { token, body })) => {
                let bound = match server.bound().and_then(|bound| bound.view().ok()) {
                    Some(bound) => bound,
                    None => syscall::exit(15),
                };
                match decode_collector_request(token, body.as_slice(), &bound) {
                    Ok(CollectorRequest::GetCollectorStats) => {
                        if respond_collector_stats(&mut server, token, collector.stats()).is_err() {
                            syscall::exit(16);
                        }
                    }
                    Ok(CollectorRequest::ReadHistory(request)) => {
                        let record = collector.read_after(request.after_record_id);
                        if respond_history(&mut server, token, record).is_err() {
                            syscall::exit(17);
                        }
                    }
                    Err(_) => syscall::exit(18),
                }
            }
            Ok(Some(ServerEvent::Canceled { .. })) => {}
            Ok(None) => {
                if syscall::yield_now().is_err() {
                    syscall::exit(19);
                }
            }
            Err(_) => syscall::exit(20),
        }
    }
}

fn retained_record(
    collector: &FixedLoggingCollector<COLLECTOR_CAPACITY>,
    record_id: RecordId,
) -> Option<nswp_logging::HistoryRecordView<'_>> {
    let previous = record_id
        .get()
        .checked_sub(1)
        .and_then(|value| RecordId::new(value).ok());
    collector
        .read_after(previous)
        .filter(|record| record.record_id == record_id)
}
