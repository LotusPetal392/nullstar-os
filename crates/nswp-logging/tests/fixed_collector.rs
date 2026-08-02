use nswp_logging::{
    BootId, CollectDisposition, CollectorError, EventId, FixedLoggingCollector, HistorySource,
    KernelLogRecordView, KernelSequence, LOGGING_PROTOCOL_MINOR_COLLECTOR_READS,
    LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY, LogRecordView, LogSeverity, PrivacyClass, RecordId,
};

const EVENT_ID: EventId = match EventId::from_bytes([
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
]) {
    Ok(id) => id,
    Err(_) => panic!("test event ID must be valid"),
};
const BOOT_ID: BootId = match BootId::from_bytes([
    0xf0, 0xe1, 0xd2, 0xc3, 0xb4, 0xa5, 0x46, 0x97, 0x88, 0x79, 0x6a, 0x5b, 0x4c, 0x3d, 0x2e, 0x1f,
]) {
    Ok(id) => id,
    Err(_) => panic!("test boot ID must be valid"),
};

fn record<'a>(message: &'a str) -> LogRecordView<'a> {
    LogRecordView {
        event_id: EVENT_ID,
        severity: LogSeverity::Info,
        privacy: PrivacyClass::Public,
        monotonic_time_ns: 100,
        subsystem: "collector",
        message,
        wall_time_unix_ns: Some(200),
    }
}

fn kernel_record<'a>(
    sequence: KernelSequence,
    message: &'a str,
    privacy: PrivacyClass,
) -> KernelLogRecordView<'a> {
    KernelLogRecordView {
        sequence,
        boot_id: Some(BOOT_ID),
        event_id: EVENT_ID,
        severity: LogSeverity::Critical,
        privacy,
        monotonic_time_ns: 300,
        subsystem: "kernel",
        message,
    }
}

#[test]
fn fixed_ring_overwrites_oldest_and_reads_in_record_id_order() {
    let mut collector = FixedLoggingCollector::<3>::new(41).unwrap();
    assert_eq!(collector.service_generation(), 41);

    for (index, message) in ["one", "two", "three", "four", "five"]
        .into_iter()
        .enumerate()
    {
        let disposition = collector
            .collect(77, record(message), [index as u8; 16])
            .unwrap();
        let CollectDisposition::Accepted {
            record_id,
            evicted_record_id,
        } = disposition
        else {
            panic!("record unexpectedly dropped");
        };
        assert_eq!(record_id.get(), index as u64 + 1);
        let expected_evicted = if index >= 3 {
            Some(index as u64 - 2)
        } else {
            None
        };
        assert_eq!(evicted_record_id.map(RecordId::get), expected_evicted);
    }

    let stats = collector.stats();
    assert_eq!(stats.received_records, 5);
    assert_eq!(stats.retained_records, 3);
    assert_eq!(stats.capacity_records, 3);
    assert_eq!(stats.evicted_records, 2);
    assert_eq!(stats.dropped_records, 0);
    assert_eq!(stats.redacted_records, 0);
    assert_eq!(stats.oldest_record_id.unwrap().get(), 3);
    assert_eq!(stats.newest_record_id.unwrap().get(), 5);

    let third = collector.read_after(None).unwrap();
    assert_eq!(third.record_id.get(), 3);
    assert_eq!(
        third.source,
        HistorySource::Process {
            process_id: 77,
            wall_time_unix_ns: Some(200),
            trace_id: [2; 16],
        }
    );
    assert_eq!(third.message, "three");
    assert_eq!(
        collector
            .read_after(RecordId::new(1).ok())
            .unwrap()
            .record_id
            .get(),
        3
    );
    assert_eq!(
        collector.read_after(RecordId::new(3).ok()).unwrap().message,
        "four"
    );
    assert_eq!(
        collector.read_after(RecordId::new(4).ok()).unwrap().message,
        "five"
    );
    assert!(collector.read_after(RecordId::new(5).ok()).is_none());
}

#[test]
fn kernel_and_process_records_interleave_wrap_filter_and_redact() {
    let sequence = KernelSequence::new(9).unwrap();
    let mut collector = FixedLoggingCollector::<4>::new(42).unwrap();
    collector.collect(7, record("evicted"), [1; 16]).unwrap();
    let first_kernel = collector
        .collect_kernel(kernel_record(
            sequence,
            "raw kernel secret",
            PrivacyClass::SecretNeverPersist,
        ))
        .unwrap();
    collector
        .collect(7, record("process two"), [2; 16])
        .unwrap();
    let duplicate_kernel = collector
        .collect_kernel(kernel_record(
            sequence,
            "same identity",
            PrivacyClass::Public,
        ))
        .unwrap();
    collector
        .collect(7, record("process three"), [3; 16])
        .unwrap();

    assert_eq!(
        first_kernel,
        CollectDisposition::Accepted {
            record_id: RecordId::new(2).unwrap(),
            evicted_record_id: None,
        }
    );
    assert_eq!(
        duplicate_kernel,
        CollectDisposition::Accepted {
            record_id: RecordId::new(4).unwrap(),
            evicted_record_id: None,
        }
    );

    let kernel = collector
        .read_after_for_minor(None, LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY)
        .unwrap();
    assert_eq!(kernel.record_id.get(), 2);
    assert_eq!(kernel.message, nswp_logging::COLLECTOR_SECRET_REDACTION);
    assert_eq!(
        kernel.source,
        HistorySource::Kernel {
            sequence,
            boot_id: Some(BOOT_ID),
        }
    );
    let duplicate = collector
        .read_after_for_minor(RecordId::new(3).ok(), LOGGING_PROTOCOL_MINOR_KERNEL_HISTORY)
        .unwrap();
    assert_eq!(duplicate.record_id.get(), 4);
    assert_eq!(duplicate.source, kernel.source);
    assert_eq!(duplicate.message, "same identity");

    let minor_two_first = collector
        .read_after_for_minor(None, LOGGING_PROTOCOL_MINOR_COLLECTOR_READS)
        .unwrap();
    assert_eq!(minor_two_first.record_id.get(), 3);
    assert_eq!(minor_two_first.message, "process two");
    let minor_two_next = collector
        .read_after_for_minor(
            Some(minor_two_first.record_id),
            LOGGING_PROTOCOL_MINOR_COLLECTOR_READS,
        )
        .unwrap();
    assert_eq!(minor_two_next.record_id.get(), 5);
    assert_eq!(minor_two_next.message, "process three");
    assert!(
        collector
            .read_after_for_minor(
                Some(minor_two_next.record_id),
                LOGGING_PROTOCOL_MINOR_COLLECTOR_READS,
            )
            .is_none()
    );

    let stats = collector.stats();
    assert_eq!(stats.received_records, 5);
    assert_eq!(stats.retained_records, 4);
    assert_eq!(stats.evicted_records, 1);
    assert_eq!(stats.redacted_records, 1);
    assert_eq!(stats.oldest_record_id.unwrap().get(), 2);
    assert_eq!(stats.newest_record_id.unwrap().get(), 5);
}

#[test]
fn capacity_zero_counts_received_records_as_dropped_without_ids() {
    let mut collector = FixedLoggingCollector::<0>::new(9).unwrap();
    assert_eq!(
        collector.collect(1, record("drop"), [0; 16]).unwrap(),
        CollectDisposition::Dropped
    );
    let stats = collector.stats();
    assert_eq!(stats.received_records, 1);
    assert_eq!(stats.retained_records, 0);
    assert_eq!(stats.capacity_records, 0);
    assert_eq!(stats.evicted_records, 0);
    assert_eq!(stats.dropped_records, 1);
    assert_eq!(stats.oldest_record_id, None);
    assert_eq!(stats.newest_record_id, None);
    assert!(collector.read_after(None).is_none());
}

#[test]
fn collector_requires_authoritative_nonzero_generation_and_source_pid() {
    assert!(matches!(
        FixedLoggingCollector::<1>::new(0),
        Err(CollectorError::ZeroServiceGeneration)
    ));
    let mut collector = FixedLoggingCollector::<1>::new(1).unwrap();
    assert_eq!(
        collector.collect(0, record("invalid"), [0; 16]),
        Err(CollectorError::ZeroSourceProcessId)
    );
    assert_eq!(collector.stats().received_records, 0);
}

#[test]
fn counter_relationships_hold_across_accept_evict_and_drop() {
    let mut collector = FixedLoggingCollector::<1>::new(2).unwrap();
    collector.collect(5, record("first"), [0; 16]).unwrap();
    collector.collect(5, record("second"), [0; 16]).unwrap();
    let stats = collector.stats();
    assert_eq!(
        stats.received_records,
        stats.retained_records + stats.evicted_records + stats.dropped_records
    );
}
