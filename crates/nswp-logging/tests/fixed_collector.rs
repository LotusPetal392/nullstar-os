use nswp_logging::{
    CollectDisposition, CollectorError, EventId, FixedLoggingCollector, LogRecordView, LogSeverity,
    PrivacyClass, RecordId,
};

const EVENT_ID: EventId = match EventId::from_bytes([
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
]) {
    Ok(id) => id,
    Err(_) => panic!("test event ID must be valid"),
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
    assert_eq!(third.source_process_id, 77);
    assert_eq!(third.message, "three");
    assert_eq!(third.trace_id, [2; 16]);
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
