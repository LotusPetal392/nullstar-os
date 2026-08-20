#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate alloc;

pub mod address_space;
pub mod boot_mode;
pub mod capability;
pub mod containment;
pub mod early_log;
pub mod event;
pub mod event_port;
pub mod interrupt_model;
pub mod job;
pub mod nullfs_volume_selection;
pub mod object;
pub mod process_completion;
pub mod process_model;
pub mod scheduling;
pub mod smp;
pub mod timer;
pub mod tmpfs_abi;
pub mod wait_set;

#[cfg(test)]
mod public_api_tests {
    use nswp_logging::{EventId, LogSeverity, PrivacyClass};

    use crate::early_log::{BootIdentity, EarlyLogInput, EarlySource, SynchronizedEarlyLog};

    const EVENT: EventId = match EventId::from_bytes([
        0xb4, 0x54, 0x0a, 0xe2, 0xa0, 0x41, 0x4b, 0x2c, 0x80, 0x5f, 0x72, 0x18, 0xc4, 0x85, 0x96,
        0xbb,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("public API test event ID must be canonical"),
    };

    #[test]
    fn early_log_snapshot_does_not_require_a_fabricated_empty_record() {
        let logger = SynchronizedEarlyLog::<2>::new();
        logger.try_initialize(BootIdentity::Unavailable).unwrap();
        logger
            .try_record(EarlyLogInput {
                event_id: EVENT,
                severity: LogSeverity::Notice,
                privacy: PrivacyClass::Public,
                monotonic_time_ns: 42,
                source: EarlySource::KERNEL,
                subsystem: "kernel.test",
                message: "public snapshot",
            })
            .unwrap();

        let mut records = [None; 2];
        let snapshot = logger.try_snapshot(&mut records).unwrap();

        assert_eq!(snapshot.copied, 1);
        assert_eq!(records[0].as_ref().unwrap().message(), "public snapshot");
        assert_eq!(records[1], None);
    }
}
