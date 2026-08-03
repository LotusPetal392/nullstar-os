//! Authorized logging history display used by the command and trusted recovery shell.

use nswp_logging::{
    CollectorStats, HistoryRecordView, HistorySource, LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY,
    LogSeverity, LoggingObserver, RecordId, decode_collector_stats_client_response,
    decode_history_read_client_response,
};
use nswp_runtime::{ClientEvent, RuntimeError};

use crate::{
    ipc::{self, CapabilityHandle},
    logging_session::{ClientBootstrap, ClientTransport},
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

pub fn show(observer_ingress: CapabilityHandle) -> Result<(), ShowError> {
    let transport = connect_observer(observer_ingress)?;
    let expected_generation = transport.service_generation();
    let mut observer = LoggingObserver::new(transport);
    negotiate(&mut observer, expected_generation)?;
    let stats = query_stats(&mut observer)?;
    show_history(&mut observer, stats)
}

fn connect_observer(observer_ingress: CapabilityHandle) -> Result<ClientTransport, ShowError> {
    let mut bootstrap = match ClientBootstrap::new(observer_ingress) {
        Ok(bootstrap) => bootstrap,
        Err(_) => {
            let _ = ipc::close(observer_ingress);
            return Err(ShowError::Connect);
        }
    };
    let mut remaining = MAX_YIELDS;
    loop {
        match bootstrap.try_connect() {
            Ok(Some(transport)) => return Ok(transport),
            Ok(None) => yield_bounded(&mut remaining, ShowError::Connect)?,
            Err(_) => return Err(ShowError::Connect),
        }
    }
}

fn negotiate(observer: &mut Observer, expected_generation: u64) -> Result<(), ShowError> {
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
                    && bound.service_generation() == expected_generation =>
            {
                return Ok(());
            }
            Ok(None) => yield_bounded(&mut remaining, ShowError::Connect)?,
            Ok(Some(_)) | Err(_) => return Err(ShowError::Connect),
        }
    }
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
