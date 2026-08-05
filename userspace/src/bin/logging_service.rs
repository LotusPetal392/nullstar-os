#![no_std]
#![no_main]

use nswp_core::TransportStatus;
use nswp_logging::{
    CollectDisposition, CollectorRequest, FixedLoggingCollector, KernelSequence,
    LOGGING_EMIT_ORDINAL, LogSeverity, RecordId, decode_collector_request, decode_log_record,
    logging_protocol, respond_collector_stats, respond_history,
};
use nswp_runtime::{Server, ServerEvent};
use userspace::{
    abi::{INIT_PROCESS_ID, signal},
    args::Args,
    early_log,
    ipc::{self, ObjectKind, Rights},
    logging_session::{
        AcceptError, InboundPacket, MAX_LOGGING_SESSIONS, PendingConnect, ServerIngress,
        ServerIngressEvent, ServerTransport, SessionRole, admission_rejection,
    },
    service_route::receive_service_generation,
    syscall::{self, SignalAction},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const READY_HANDLE: u64 = 1;
const PRODUCER_INGRESS_HANDLE: u64 = 2;
const OBSERVER_INGRESS_HANDLE: u64 = 3;
const EARLY_LOG_HANDLE: u64 = 4;
const GENERATION_HANDOFF_HANDLE: u64 = 5;
const READY_MESSAGE: &[u8] = b"service-ready: logging";
const STARTED_MARKER: &[u8] = b"logging-service: bounded session collector ready\n";
const KERNEL_IMPORT_MARKER: &[u8] = b"logging-service: kernel early log imported\n";
const RECORD_PREFIX: &[u8] = b"logging-service: retained: ";
const RECORD_SEPARATOR: &[u8] = b": ";
const RECORD_SUFFIX: &[u8] = b"\n";
const COLLECTOR_CAPACITY: usize = 64;

type LoggingServer = Server<'static, ServerTransport>;

struct LoggingSession {
    server: LoggingServer,
}

struct SessionManager {
    entries: [Option<LoggingSession>; MAX_LOGGING_SESSIONS],
    poll_cursor: usize,
}

impl SessionManager {
    const fn new() -> Self {
        Self {
            entries: [const { None }; MAX_LOGGING_SESSIONS],
            poll_cursor: 0,
        }
    }

    fn handle_event(&mut self, event: ServerIngressEvent, generation: u64) {
        match event {
            ServerIngressEvent::Connect(connect) => self.connect(connect, generation),
            ServerIngressEvent::Disconnect(disconnect) => {
                let Some(index) = self.find(disconnect.role(), disconnect.owner_process_id())
                else {
                    return;
                };
                let should_remove = self.entries[index].as_mut().is_some_and(|session| {
                    session
                        .server
                        .transport_mut()
                        .handle_disconnect(disconnect)
                        .is_ok()
                });
                if should_remove {
                    self.entries[index] = None;
                }
            }
            ServerIngressEvent::Packet(packet) => self.queue_packet(&packet),
        }
    }

    fn connect(&mut self, mut connect: PendingConnect, generation: u64) {
        let role = connect.role();
        let owner_process_id = connect.owner_process_id();
        let duplicate = self.find(role, owner_process_id).is_some();
        let role_count = self
            .entries
            .iter()
            .flatten()
            .filter(|session| session.server.transport().role() == role)
            .count();
        let available = self.entries.iter().position(Option::is_none);

        if let Some(status) =
            admission_rejection(self.entries.iter().flatten().count(), role_count, duplicate)
        {
            let _ = connect.try_reject(status);
            return;
        }

        let transport = match connect.try_accept(generation) {
            Ok(transport) => transport,
            Err(AcceptError::WouldBlock | AcceptError::Ipc(_)) => return,
            Err(
                AcceptError::InvalidServiceGeneration
                | AcceptError::InvalidStatus
                | AcceptError::Completed,
            ) => return,
        };
        let server = match Server::new(transport, logging_protocol(), generation) {
            Ok(server) => server,
            Err(_) => return,
        };
        self.entries[available.expect("an available slot was checked above")] =
            Some(LoggingSession { server });
    }

    fn queue_packet(&mut self, packet: &InboundPacket) {
        let Some(index) = self.find(packet.role(), packet.owner_process_id()) else {
            return;
        };
        let queued = self.entries[index].as_mut().is_some_and(|session| {
            session
                .server
                .transport_mut()
                .try_queue_packet(packet)
                .is_ok()
        });
        if !queued {
            self.entries[index] = None;
        }
    }

    fn find(&self, role: SessionRole, owner_process_id: u64) -> Option<usize> {
        self.entries.iter().position(|entry| {
            entry.as_ref().is_some_and(|session| {
                session.server.transport().role() == role
                    && session.server.transport().owner_process_id() == owner_process_id
            })
        })
    }

    fn poll(&mut self, collector: &mut FixedLoggingCollector<COLLECTOR_CAPACITY>) {
        for offset in 0..MAX_LOGGING_SESSIONS {
            let index = (self.poll_cursor + offset) % MAX_LOGGING_SESSIONS;
            let result = match self.entries[index].as_mut() {
                Some(session) => session.server.poll(0),
                None => continue,
            };
            let keep = match result {
                Ok(Some(event)) => self.handle_server_event(index, event, collector),
                Ok(None) => true,
                Err(_) => false,
            };
            if !keep {
                self.entries[index] = None;
            }
        }
        self.poll_cursor = (self.poll_cursor + 1) % MAX_LOGGING_SESSIONS;
    }

    fn handle_server_event(
        &mut self,
        index: usize,
        event: ServerEvent,
        collector: &mut FixedLoggingCollector<COLLECTOR_CAPACITY>,
    ) -> bool {
        let Some(session) = self.entries[index].as_mut() else {
            return false;
        };
        let role = session.server.transport().role();
        match event {
            ServerEvent::Bound(_) | ServerEvent::Canceled { .. } => true,
            ServerEvent::OneWay {
                ordinal,
                trace_id,
                body,
                ..
            } => {
                if role != SessionRole::Producer || ordinal != LOGGING_EMIT_ORDINAL {
                    return false;
                }
                let bound = match session.server.bound().and_then(|bound| bound.view().ok()) {
                    Some(bound) => bound,
                    None => return false,
                };
                let record = match decode_log_record(body.as_slice(), &bound) {
                    Ok(record) => record,
                    Err(_) => return false,
                };
                let source_process_id = session.server.transport().owner_process_id();
                let record_id = match collector.collect(source_process_id, record, trace_id) {
                    Ok(CollectDisposition::Accepted { record_id, .. }) => record_id,
                    Ok(CollectDisposition::Dropped) | Err(_) => return false,
                };
                let Some(retained) = retained_record(collector, record_id) else {
                    return false;
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
                    return false;
                }
                true
            }
            ServerEvent::Request { token, body } => {
                if role == SessionRole::Producer {
                    return session
                        .server
                        .respond(token, TransportStatus::AccessDenied, &[])
                        .is_ok();
                }
                let bound = match session.server.bound().and_then(|bound| bound.view().ok()) {
                    Some(bound) => bound,
                    None => return false,
                };
                match decode_collector_request(token, body.as_slice(), &bound) {
                    Ok(CollectorRequest::GetCollectorStats) => {
                        respond_collector_stats(&mut session.server, token, collector.stats())
                            .is_ok()
                    }
                    Ok(CollectorRequest::ReadHistory(request)) => {
                        let record =
                            collector.read_after_for_minor(request.after_record_id, bound.minor());
                        respond_history(&mut session.server, token, record).is_ok()
                    }
                    Err(_) => false,
                }
            }
        }
    }
}

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let ignore_terminate = arguments.len() == 2 && arguments.get(1) == Some(b"--ignore-terminate");
    let suppress_readiness =
        arguments.len() == 2 && arguments.get(1) == Some(b"--suppress-readiness");
    if !(arguments.len() == 1 || ignore_terminate || suppress_readiness) {
        syscall::exit(1);
    }
    if ignore_terminate
        && syscall::signal_action(signal::TERMINATE, Some(&SignalAction::IGNORE), None).is_err()
    {
        syscall::exit(1);
    }
    if !matches!(
        ipc::info(READY_HANDLE),
        Ok(info) if info.kind == ObjectKind::Endpoint && info.rights == Rights::SEND
    ) {
        syscall::exit(2);
    }
    let mut producer_ingress =
        match ServerIngress::new(SessionRole::Producer, PRODUCER_INGRESS_HANDLE) {
            Ok(ingress) => ingress,
            Err(_) => syscall::exit(3),
        };
    let mut observer_ingress =
        match ServerIngress::new(SessionRole::Observer, OBSERVER_INGRESS_HANDLE) {
            Ok(ingress) => ingress,
            Err(_) => syscall::exit(3),
        };
    let generation = match receive_service_generation(GENERATION_HANDOFF_HANDLE, INIT_PROCESS_ID) {
        Ok(generation) => generation.get(),
        Err(_) => syscall::exit(4),
    };
    let mut collector = match FixedLoggingCollector::<COLLECTOR_CAPACITY>::new(generation) {
        Ok(collector) => collector,
        Err(_) => syscall::exit(5),
    };
    if !matches!(
        ipc::info(EARLY_LOG_HANDLE),
        Ok(info) if info.kind == ObjectKind::KernelEarlyLogReader && info.rights == Rights::READ
    ) || early_log::open_reader() != Err(early_log::Error::PERMISSION)
    {
        syscall::exit(6);
    }
    if import_kernel_history(&mut collector).is_err() {
        syscall::exit(early_log::IMPORT_FAILURE_EXIT_STATUS);
    }
    if syscall::write_all(syscall::STDOUT, KERNEL_IMPORT_MARKER).is_err() {
        syscall::exit(6);
    }
    let mut sessions = SessionManager::new();
    if (!suppress_readiness && ipc::send(READY_HANDLE, READY_MESSAGE, None).is_err())
        || syscall::write_all(syscall::STDOUT, STARTED_MARKER).is_err()
    {
        syscall::exit(8);
    }

    loop {
        poll_ingress(&mut producer_ingress, &mut sessions, generation);
        poll_ingress(&mut observer_ingress, &mut sessions, generation);
        sessions.poll(&mut collector);
        if syscall::yield_now().is_err() {
            syscall::exit(19);
        }
    }
}

fn poll_ingress(ingress: &mut ServerIngress, sessions: &mut SessionManager, generation: u64) {
    match ingress.try_receive() {
        Ok(Some(event)) => sessions.handle_event(event, generation),
        Ok(None) => {}
        Err(_) => {}
    }
}

fn import_kernel_history(
    collector: &mut FixedLoggingCollector<COLLECTOR_CAPACITY>,
) -> Result<(), ()> {
    let mut storage = early_log::ResponseStorage::new();
    let first = read_kernel_after(None, &mut storage)?;
    if first.stats.retained_records == 0 || first.stats.retained_records > COLLECTOR_CAPACITY as u64
    {
        return Err(());
    }
    let mut range = early_log::SnapshotRange::new(first.boot_id, first.stats).map_err(|_| ())?;
    let mut record = first.record.ok_or(())?;
    let mut imported = 0_usize;
    loop {
        let complete = range
            .accept(record.boot_id, record.sequence)
            .map_err(|_| ())?;
        if imported >= COLLECTOR_CAPACITY {
            return Err(());
        }
        match collector.collect_kernel(record.view()).map_err(|_| ())? {
            CollectDisposition::Accepted { .. } => {}
            CollectDisposition::Dropped => return Err(()),
        }
        imported += 1;
        if complete {
            return Ok(());
        }

        let read = read_kernel_after(Some(record.sequence), &mut storage)?;
        record = read.record.ok_or(())?;
    }
}

fn read_kernel_after(
    after: Option<KernelSequence>,
    storage: &mut early_log::ResponseStorage,
) -> Result<early_log::ReadResult, ()> {
    loop {
        match early_log::read_after(EARLY_LOG_HANDLE, after, storage) {
            Ok(read) => return Ok(read),
            Err(error) if error == early_log::Error::TRY_AGAIN => {
                syscall::yield_now().map_err(|_| ())?;
            }
            Err(_) => return Err(()),
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
