//! Authorized logging history display used by the command and trusted recovery shell.

use nswp_logging::{
    CollectorStats, HistoryRecordView, HistorySource, LOGGING_OBSERVER_ROLE,
    LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY, LOGGING_SERVICE_ID, LogSeverity, LoggingObserver,
    RecordId, decode_collector_stats_client_response, decode_history_read_client_response,
};
use nswp_runtime::{ClientEvent, RuntimeError};
use service_route::{ProviderGeneration, RouteKey};

use crate::{
    abi::INIT_PROCESS_ID,
    ipc::{self, CapabilityHandle},
    logging_session::{ClientBootstrap, ClientTransport},
    service_route::RouteResolution,
    syscall::{self, STDOUT},
};

const MAX_YIELDS: u32 = 65_536;
const NOW_NS: u64 = 0;
const DEADLINE_NS: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowError {
    Connect,
    Query,
    HistoryChanged,
    Output,
}

type Observer = LoggingObserver<'static, ClientTransport>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SnapshotProgress {
    expected: RecordId,
    high_water: RecordId,
    remaining: u64,
}

impl SnapshotProgress {
    fn new(stats: CollectorStats) -> Result<Option<Self>, ShowError> {
        match (stats.oldest_record_id, stats.newest_record_id) {
            (None, None) if stats.retained_records == 0 => Ok(None),
            (Some(expected), Some(high_water)) if stats.retained_records != 0 => Ok(Some(Self {
                expected,
                high_water,
                remaining: stats.retained_records,
            })),
            _ => Err(ShowError::Query),
        }
    }

    fn accept(&mut self, record_id: RecordId) -> Result<bool, ShowError> {
        if record_id != self.expected || record_id > self.high_water || self.remaining == 0 {
            return Err(ShowError::HistoryChanged);
        }
        self.remaining -= 1;
        if record_id == self.high_water {
            return if self.remaining == 0 {
                Ok(true)
            } else {
                Err(ShowError::HistoryChanged)
            };
        }
        if self.remaining == 0 {
            return Err(ShowError::HistoryChanged);
        }
        self.expected = RecordId::new(
            record_id
                .get()
                .checked_add(1)
                .ok_or(ShowError::HistoryChanged)?,
        )
        .map_err(|_| ShowError::HistoryChanged)?;
        Ok(false)
    }
}

/// Resolves and uses the observer route once. The exact-`SEND` route grant remains caller-owned.
pub fn show(observer_route_grant: CapabilityHandle) -> Result<(), ShowError> {
    let (transport, route_generation) = connect_observer(observer_route_grant)?;
    let mut observer = LoggingObserver::new(transport);
    negotiate(&mut observer, route_generation)?;
    let stats = query_stats(&mut observer)?;
    show_history(&mut observer, stats)
}

const fn observer_route_key() -> RouteKey {
    RouteKey::new(LOGGING_SERVICE_ID, LOGGING_OBSERVER_ROLE)
}

fn connect_observer(
    observer_route_grant: CapabilityHandle,
) -> Result<(ClientTransport, ProviderGeneration), ShowError> {
    let mut resolution = RouteResolution::begin(observer_route_grant, observer_route_key())
        .map_err(|_| ShowError::Connect)?;
    let mut remaining = MAX_YIELDS;
    let resolved = loop {
        match resolution.try_complete() {
            Ok(Some(resolved)) => break resolved,
            Ok(None) => yield_bounded(&mut remaining, ShowError::Connect)?,
            Err(_) => return Err(ShowError::Connect),
        }
    };
    if resolved.broker_process_id() != INIT_PROCESS_ID {
        return Err(ShowError::Connect);
    }

    let route_generation = resolved.generation();
    let observer_ingress = resolved.into_handle();
    let mut bootstrap =
        match ClientBootstrap::new_for_generation(observer_ingress, route_generation) {
            Ok(bootstrap) => bootstrap,
            Err(_) => {
                let _ = ipc::close(observer_ingress);
                return Err(ShowError::Connect);
            }
        };
    let mut remaining = MAX_YIELDS;
    loop {
        match bootstrap.try_connect() {
            Ok(Some(transport)) => return Ok((transport, route_generation)),
            Ok(None) => yield_bounded(&mut remaining, ShowError::Connect)?,
            Err(_) => return Err(ShowError::Connect),
        }
    }
}

fn negotiate(
    observer: &mut Observer,
    expected_generation: ProviderGeneration,
) -> Result<(), ShowError> {
    let mut remaining = MAX_YIELDS;
    loop {
        match observer.try_negotiate() {
            Ok(()) => break,
            Err(RuntimeError::WouldBlock) => {
                yield_bounded(&mut remaining, ShowError::Connect)?;
            }
            Err(_) => return Err(ShowError::Connect),
        }
    }

    let mut remaining = MAX_YIELDS;
    loop {
        match observer.poll() {
            Ok(Some(ClientEvent::Bound(bound)))
                if bound.minor() == LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY
                    && bound_generation_matches(
                        expected_generation,
                        bound.service_generation(),
                    ) =>
            {
                return Ok(());
            }
            Ok(None) => yield_bounded(&mut remaining, ShowError::Connect)?,
            Ok(Some(_)) | Err(_) => return Err(ShowError::Connect),
        }
    }
}

const fn bound_generation_matches(expected: ProviderGeneration, actual: u64) -> bool {
    expected.get() == actual
}

fn query_stats(observer: &mut Observer) -> Result<CollectorStats, ShowError> {
    let mut remaining = MAX_YIELDS;
    let transaction = loop {
        match observer.try_get_collector_stats(NOW_NS, DEADLINE_NS, [0; 16]) {
            Ok(transaction) => break transaction,
            Err(RuntimeError::WouldBlock) => {
                yield_bounded(&mut remaining, ShowError::Query)?;
            }
            Err(_) => return Err(ShowError::Query),
        }
    };
    let event = wait_for_response(observer)?;
    let bound = observer
        .client()
        .bound()
        .and_then(|bound| bound.view().ok())
        .ok_or(ShowError::Query)?;
    decode_collector_stats_client_response(&event, transaction, &bound)
        .map_err(|_| ShowError::Query)
}

fn show_history(observer: &mut Observer, stats: CollectorStats) -> Result<(), ShowError> {
    let Some(mut progress) = SnapshotProgress::new(stats)? else {
        return Ok(());
    };
    let mut cursor = None;
    loop {
        let record_id =
            read_and_print_history(observer, cursor, progress.expected, progress.high_water)?;
        cursor = Some(record_id);
        if progress.accept(record_id)? {
            return Ok(());
        }
    }
}

fn read_and_print_history(
    observer: &mut Observer,
    after_record_id: Option<RecordId>,
    expected: RecordId,
    high_water: RecordId,
) -> Result<RecordId, ShowError> {
    let mut remaining = MAX_YIELDS;
    let transaction = loop {
        match observer.try_read_history(after_record_id, NOW_NS, DEADLINE_NS, [0; 16]) {
            Ok(transaction) => break transaction,
            Err(RuntimeError::WouldBlock) => {
                yield_bounded(&mut remaining, ShowError::Query)?;
            }
            Err(_) => return Err(ShowError::Query),
        }
    };
    let event = wait_for_response(observer)?;
    let bound = observer
        .client()
        .bound()
        .and_then(|bound| bound.view().ok())
        .ok_or(ShowError::Query)?;
    let record = decode_history_read_client_response(&event, transaction, &bound)
        .map_err(|_| ShowError::Query)?
        .ok_or(ShowError::HistoryChanged)?;
    if record.record_id != expected || record.record_id > high_water {
        return Err(ShowError::HistoryChanged);
    }
    let record_id = record.record_id;
    print_record(record)?;
    Ok(record_id)
}

fn wait_for_response(observer: &mut Observer) -> Result<ClientEvent, ShowError> {
    let mut remaining = MAX_YIELDS;
    loop {
        match observer.poll() {
            Ok(Some(event @ ClientEvent::Response { .. })) => return Ok(event),
            Ok(None) => yield_bounded(&mut remaining, ShowError::Query)?,
            Ok(Some(_)) | Err(_) => return Err(ShowError::Query),
        }
    }
}

fn print_record(record: HistoryRecordView<'_>) -> Result<(), ShowError> {
    write_decimal(record.record_id.get())?;
    write_literal(b" ")?;
    write_literal(severity_name(record.severity))?;
    write_literal(b" ")?;
    match record.source {
        HistorySource::Process { process_id, .. } => {
            write_literal(b"pid=")?;
            write_decimal(process_id)?;
        }
        HistorySource::Kernel { sequence, .. } => {
            write_literal(b"kernel#")?;
            write_decimal(sequence.get())?;
        }
    }
    write_literal(b" ")?;
    write_literal(record.subsystem.as_bytes())?;
    write_literal(b": ")?;
    write_literal(record.message.as_bytes())?;
    write_literal(b"\n")
}

const fn severity_name(severity: LogSeverity) -> &'static [u8] {
    match severity {
        LogSeverity::Trace => b"trace",
        LogSeverity::Debug => b"debug",
        LogSeverity::Info => b"info",
        LogSeverity::Notice => b"notice",
        LogSeverity::Warning => b"warning",
        LogSeverity::Error => b"error",
        LogSeverity::Critical => b"critical",
        LogSeverity::Alert => b"alert",
        LogSeverity::Emergency => b"emergency",
    }
}

fn write_decimal(mut value: u64) -> Result<(), ShowError> {
    let mut bytes = [0_u8; 20];
    let mut start = bytes.len();
    if value == 0 {
        return write_literal(b"0");
    }
    while value != 0 {
        start -= 1;
        bytes[start] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    write_literal(&bytes[start..])
}

fn write_literal(bytes: &[u8]) -> Result<(), ShowError> {
    syscall::write_all(STDOUT, bytes).map_err(|_| ShowError::Output)
}

fn yield_bounded(remaining: &mut u32, error: ShowError) -> Result<(), ShowError> {
    if *remaining == 0 || syscall::yield_now().is_err() {
        return Err(error);
    }
    *remaining -= 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_route_and_bound_generation_are_pinned() {
        let key = observer_route_key();
        assert_eq!(key.service(), LOGGING_SERVICE_ID);
        assert_eq!(key.role(), LOGGING_OBSERVER_ROLE);

        let generation = ProviderGeneration::new(9).unwrap();
        assert!(bound_generation_matches(generation, 9));
        assert!(!bound_generation_matches(generation, 10));
    }

    fn stats(oldest: u64, newest: u64, retained: u64) -> CollectorStats {
        CollectorStats {
            received_records: retained,
            retained_records: retained,
            capacity_records: 64,
            evicted_records: 0,
            dropped_records: 0,
            redacted_records: 0,
            oldest_record_id: Some(RecordId::new(oldest).unwrap()),
            newest_record_id: Some(RecordId::new(newest).unwrap()),
        }
    }

    #[test]
    fn snapshot_requires_the_captured_oldest_and_every_successor() {
        let mut progress = SnapshotProgress::new(stats(1, 3, 3)).unwrap().unwrap();
        assert_eq!(
            progress.accept(RecordId::new(2).unwrap()),
            Err(ShowError::HistoryChanged)
        );

        let mut progress = SnapshotProgress::new(stats(1, 3, 3)).unwrap().unwrap();
        assert_eq!(progress.accept(RecordId::new(1).unwrap()), Ok(false));
        assert_eq!(progress.accept(RecordId::new(2).unwrap()), Ok(false));
        assert_eq!(progress.accept(RecordId::new(3).unwrap()), Ok(true));
    }

    #[test]
    fn snapshot_rejects_high_water_before_the_captured_count() {
        let mut progress = SnapshotProgress::new(stats(1, 3, 4)).unwrap().unwrap();
        assert_eq!(progress.accept(RecordId::new(1).unwrap()), Ok(false));
        assert_eq!(progress.accept(RecordId::new(2).unwrap()), Ok(false));
        assert_eq!(
            progress.accept(RecordId::new(3).unwrap()),
            Err(ShowError::HistoryChanged)
        );
    }
}
