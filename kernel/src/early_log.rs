use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use nswp_logging::{
    COLLECTOR_SECRET_REDACTION, EventId, LOGGING_MAX_MESSAGE_BYTES, LOGGING_MAX_SUBSYSTEM_BYTES,
    LogSeverity, PrivacyClass,
};
use spin::Mutex;

pub const KERNEL_EARLY_LOG_CAPACITY: usize = 64;

const _: () = assert!(LOGGING_MAX_SUBSYSTEM_BYTES <= u8::MAX as usize);
const _: () = assert!(LOGGING_MAX_MESSAGE_BYTES <= u8::MAX as usize);
const _: () = assert!(COLLECTOR_SECRET_REDACTION.len() <= LOGGING_MAX_MESSAGE_BYTES);

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootId([u8; 16]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootIdError {
    Nil,
    InvalidVersion,
    InvalidVariant,
}

impl BootId {
    /// Constructs a UUIDv4-form boot identifier in RFC/network byte order.
    ///
    /// A boot ID is correlation metadata, not authority. The kernel must use
    /// `BootIdentity::Unavailable` until a trustworthy boot entropy source can
    /// supply these bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Result<Self, BootIdError> {
        let mut all_zero = true;
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0 {
                all_zero = false;
            }
            index += 1;
        }
        if all_zero {
            return Err(BootIdError::Nil);
        }
        if bytes[6] >> 4 != 4 {
            return Err(BootIdError::InvalidVersion);
        }
        if bytes[8] & 0xc0 != 0x80 {
            return Err(BootIdError::InvalidVariant);
        }
        Ok(Self(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootIdentity {
    Unavailable,
    Id(BootId),
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EarlySequence(NonZeroU64);

impl EarlySequence {
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EarlySource {
    pub cpu_id: Option<u32>,
    pub process_id: Option<NonZeroU64>,
    pub thread_id: Option<NonZeroU64>,
}

impl EarlySource {
    pub const KERNEL: Self = Self {
        cpu_id: None,
        process_id: None,
        thread_id: None,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EarlyLogInput<'a> {
    pub event_id: EventId,
    pub severity: LogSeverity,
    pub privacy: PrivacyClass,
    /// Nanoseconds from the boot-local monotonic clock, or zero while the
    /// clock is unavailable. Sequence numbers remain the ordering authority.
    pub monotonic_time_ns: u64,
    pub source: EarlySource,
    pub subsystem: &'a str,
    pub message: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EarlyLogRecord {
    sequence: EarlySequence,
    event_id: EventId,
    severity: LogSeverity,
    privacy: PrivacyClass,
    monotonic_time_ns: u64,
    source: EarlySource,
    subsystem_len: u8,
    message_len: u8,
    subsystem: [u8; LOGGING_MAX_SUBSYSTEM_BYTES],
    message: [u8; LOGGING_MAX_MESSAGE_BYTES],
}

impl EarlyLogRecord {
    pub const fn sequence(&self) -> EarlySequence {
        self.sequence
    }

    pub const fn event_id(&self) -> EventId {
        self.event_id
    }

    pub const fn severity(&self) -> LogSeverity {
        self.severity
    }

    pub const fn privacy(&self) -> PrivacyClass {
        self.privacy
    }

    pub const fn monotonic_time_ns(&self) -> u64 {
        self.monotonic_time_ns
    }

    pub const fn source(&self) -> EarlySource {
        self.source
    }

    pub fn subsystem(&self) -> &str {
        let bytes = &self.subsystem[..usize::from(self.subsystem_len)];
        core::str::from_utf8(bytes).expect("early-log subsystem was copied from UTF-8")
    }

    pub fn message(&self) -> &str {
        let bytes = &self.message[..usize::from(self.message_len)];
        core::str::from_utf8(bytes).expect("early-log message was copied from UTF-8")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordError {
    EmptySubsystem,
    SubsystemTooLong,
    MessageTooLong,
    CapacityZero,
    SequenceExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PushOutcome {
    pub sequence: EarlySequence,
    pub evicted: Option<EarlySequence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EarlyLogStats {
    pub boot_identity: BootIdentity,
    pub submitted: u64,
    pub retained: usize,
    pub capacity: usize,
    pub overwritten: u64,
    pub dropped: u64,
    pub rejected: u64,
    pub oldest_sequence: Option<EarlySequence>,
    pub newest_sequence: Option<EarlySequence>,
}

pub struct FixedEarlyLog<const N: usize> {
    boot_identity: BootIdentity,
    entries: [Option<EarlyLogRecord>; N],
    head: usize,
    len: usize,
    next_sequence: Option<NonZeroU64>,
    submitted: u64,
    overwritten: u64,
    dropped: u64,
    rejected: u64,
}

impl<const N: usize> FixedEarlyLog<N> {
    pub const fn new(boot_identity: BootIdentity) -> Self {
        Self {
            boot_identity,
            entries: [None; N],
            head: 0,
            len: 0,
            next_sequence: NonZeroU64::new(1),
            submitted: 0,
            overwritten: 0,
            dropped: 0,
            rejected: 0,
        }
    }

    pub const fn boot_identity(&self) -> BootIdentity {
        self.boot_identity
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, input: EarlyLogInput<'_>) -> Result<PushOutcome, RecordError> {
        self.submitted = self.submitted.saturating_add(1);

        let message = if input.privacy == PrivacyClass::SecretNeverPersist {
            COLLECTOR_SECRET_REDACTION
        } else {
            input.message
        };
        if input.subsystem.is_empty() {
            return Err(self.reject(RecordError::EmptySubsystem));
        }
        if input.subsystem.len() > LOGGING_MAX_SUBSYSTEM_BYTES {
            return Err(self.reject(RecordError::SubsystemTooLong));
        }
        if message.len() > LOGGING_MAX_MESSAGE_BYTES {
            return Err(self.reject(RecordError::MessageTooLong));
        }
        if N == 0 {
            self.dropped = self.dropped.saturating_add(1);
            return Err(RecordError::CapacityZero);
        }

        let Some(sequence) = self.next_sequence else {
            self.dropped = self.dropped.saturating_add(1);
            return Err(RecordError::SequenceExhausted);
        };
        let sequence = EarlySequence(sequence);

        let mut subsystem_bytes = [0_u8; LOGGING_MAX_SUBSYSTEM_BYTES];
        subsystem_bytes[..input.subsystem.len()].copy_from_slice(input.subsystem.as_bytes());
        let mut message_bytes = [0_u8; LOGGING_MAX_MESSAGE_BYTES];
        message_bytes[..message.len()].copy_from_slice(message.as_bytes());
        let record = EarlyLogRecord {
            sequence,
            event_id: input.event_id,
            severity: input.severity,
            privacy: input.privacy,
            monotonic_time_ns: input.monotonic_time_ns,
            source: input.source,
            subsystem_len: input.subsystem.len() as u8,
            message_len: message.len() as u8,
            subsystem: subsystem_bytes,
            message: message_bytes,
        };

        let evicted = if self.len == N {
            let evicted = self.entries[self.head].map(|entry| entry.sequence);
            self.entries[self.head] = Some(record);
            self.head = (self.head + 1) % N;
            self.overwritten = self.overwritten.saturating_add(1);
            evicted
        } else {
            let tail = (self.head + self.len) % N;
            self.entries[tail] = Some(record);
            self.len += 1;
            None
        };

        self.next_sequence = sequence.get().checked_add(1).and_then(NonZeroU64::new);
        Ok(PushOutcome { sequence, evicted })
    }

    pub fn get(&self, chronological_index: usize) -> Option<&EarlyLogRecord> {
        if chronological_index >= self.len || N == 0 {
            return None;
        }
        let index = (self.head + chronological_index) % N;
        self.entries[index].as_ref()
    }

    pub fn read_after(&self, after: Option<EarlySequence>) -> Option<&EarlyLogRecord> {
        (0..self.len)
            .filter_map(|index| self.get(index))
            .find(|record| after.is_none_or(|cursor| record.sequence > cursor))
    }

    pub fn copy_chronological(&self, output: &mut [Option<EarlyLogRecord>]) -> usize {
        let count = self.len.min(output.len());
        for (index, slot) in output.iter_mut().take(count).enumerate() {
            *slot = self.get(index).copied();
        }
        count
    }

    pub fn stats(&self) -> EarlyLogStats {
        EarlyLogStats {
            boot_identity: self.boot_identity,
            submitted: self.submitted,
            retained: self.len,
            capacity: N,
            overwritten: self.overwritten,
            dropped: self.dropped,
            rejected: self.rejected,
            oldest_sequence: self.get(0).map(EarlyLogRecord::sequence),
            newest_sequence: self
                .len
                .checked_sub(1)
                .and_then(|index| self.get(index))
                .map(EarlyLogRecord::sequence),
        }
    }

    fn reject(&mut self, error: RecordError) -> RecordError {
        self.dropped = self.dropped.saturating_add(1);
        self.rejected = self.rejected.saturating_add(1);
        error
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitializeError {
    Busy,
    AlreadyInitialized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TryRecordError {
    Busy,
    Uninitialized,
    Record(RecordError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    Busy,
    Uninitialized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub stats: EarlyLogStats,
    pub busy_drops: u64,
    pub copied: usize,
}

pub struct SynchronizedEarlyLog<const N: usize> {
    inner: Mutex<Option<FixedEarlyLog<N>>>,
    busy_drops: AtomicU64,
}

impl<const N: usize> SynchronizedEarlyLog<N> {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            busy_drops: AtomicU64::new(0),
        }
    }

    pub fn try_initialize(&self, boot_identity: BootIdentity) -> Result<(), InitializeError> {
        let Some(mut slot) = self.inner.try_lock() else {
            return Err(InitializeError::Busy);
        };
        if slot.is_some() {
            return Err(InitializeError::AlreadyInitialized);
        }
        *slot = Some(FixedEarlyLog::new(boot_identity));
        Ok(())
    }

    pub fn try_record(&self, input: EarlyLogInput<'_>) -> Result<PushOutcome, TryRecordError> {
        let Some(mut slot) = self.inner.try_lock() else {
            self.note_busy_drop();
            return Err(TryRecordError::Busy);
        };
        let ring = slot.as_mut().ok_or(TryRecordError::Uninitialized)?;
        ring.push(input).map_err(TryRecordError::Record)
    }

    pub fn try_snapshot(
        &self,
        output: &mut [Option<EarlyLogRecord>],
    ) -> Result<SnapshotInfo, SnapshotError> {
        let Some(slot) = self.inner.try_lock() else {
            return Err(SnapshotError::Busy);
        };
        let ring = slot.as_ref().ok_or(SnapshotError::Uninitialized)?;
        let copied = ring.copy_chronological(output);
        Ok(SnapshotInfo {
            stats: ring.stats(),
            busy_drops: self.busy_drops.load(Ordering::Relaxed),
            copied,
        })
    }

    pub fn try_stats(&self) -> Result<SnapshotInfo, SnapshotError> {
        self.try_snapshot(&mut [])
    }

    fn note_busy_drop(&self) {
        let current = self.busy_drops.load(Ordering::Relaxed);
        if current != u64::MAX {
            let _ = self.busy_drops.compare_exchange(
                current,
                current + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }
}

impl<const N: usize> Default for SynchronizedEarlyLog<N> {
    fn default() -> Self {
        Self::new()
    }
}

static KERNEL_EARLY_LOG: SynchronizedEarlyLog<KERNEL_EARLY_LOG_CAPACITY> =
    SynchronizedEarlyLog::new();

pub fn initialize_kernel_early_log(boot_identity: BootIdentity) -> Result<(), InitializeError> {
    without_local_interrupts(|| KERNEL_EARLY_LOG.try_initialize(boot_identity))
}

pub fn try_record_kernel_early_log(
    input: EarlyLogInput<'_>,
) -> Result<PushOutcome, TryRecordError> {
    without_local_interrupts(|| KERNEL_EARLY_LOG.try_record(input))
}

pub fn try_snapshot_kernel_early_log(
    output: &mut [Option<EarlyLogRecord>],
) -> Result<SnapshotInfo, SnapshotError> {
    without_local_interrupts(|| KERNEL_EARLY_LOG.try_snapshot(output))
}

pub fn try_kernel_early_log_stats() -> Result<SnapshotInfo, SnapshotError> {
    without_local_interrupts(|| KERNEL_EARLY_LOG.try_stats())
}

#[cfg(target_os = "none")]
fn without_local_interrupts<R>(operation: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(operation)
}

#[cfg(not(target_os = "none"))]
fn without_local_interrupts<R>(operation: impl FnOnce() -> R) -> R {
    operation()
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;

    use nswp_logging::{EventId, LogSeverity, PrivacyClass};

    use super::{
        BootId, BootIdError, BootIdentity, EarlyLogInput, EarlyLogRecord, EarlySequence,
        EarlySource, FixedEarlyLog, InitializeError, RecordError, SynchronizedEarlyLog,
        TryRecordError,
    };

    const EVENT: EventId = match EventId::from_bytes([
        0x31, 0x86, 0x6a, 0xaf, 0x91, 0x5b, 0x42, 0x45, 0x89, 0x76, 0xe1, 0x64, 0x2a, 0x2f, 0x66,
        0x7d,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("test event ID must be canonical"),
    };
    const BOOT_ID: BootId = match BootId::from_bytes([
        0x46, 0xf4, 0xc1, 0xb0, 0x70, 0x38, 0x48, 0x86, 0xb2, 0x28, 0x75, 0x4f, 0x15, 0xd4, 0x62,
        0x98,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("test boot ID must be canonical"),
    };

    fn input<'a>(message: &'a str) -> EarlyLogInput<'a> {
        EarlyLogInput {
            event_id: EVENT,
            severity: LogSeverity::Info,
            privacy: PrivacyClass::Public,
            monotonic_time_ns: 10,
            source: EarlySource::KERNEL,
            subsystem: "kernel.test",
            message,
        }
    }

    #[test]
    fn boot_ids_are_uuidv4_values_distinct_from_event_ids() {
        assert_eq!(BootId::from_bytes([0; 16]), Err(BootIdError::Nil));

        let mut version_one = *BOOT_ID.as_bytes();
        version_one[6] = 0x10;
        assert_eq!(
            BootId::from_bytes(version_one),
            Err(BootIdError::InvalidVersion)
        );

        let mut wrong_variant = *BOOT_ID.as_bytes();
        wrong_variant[8] = 0x40;
        assert_eq!(
            BootId::from_bytes(wrong_variant),
            Err(BootIdError::InvalidVariant)
        );
    }

    #[test]
    fn ring_fills_and_wraps_in_chronological_order() {
        let mut ring = FixedEarlyLog::<3>::new(BootIdentity::Id(BOOT_ID));
        for message in ["one", "two", "three"] {
            let outcome = ring.push(input(message)).unwrap();
            assert_eq!(outcome.evicted, None);
        }
        let fourth = ring.push(input("four")).unwrap();
        assert_eq!(fourth.sequence.get(), 4);
        assert_eq!(fourth.evicted.map(EarlySequence::get), Some(1));

        let messages: [&str; 3] = core::array::from_fn(|index| ring.get(index).unwrap().message());
        assert_eq!(messages, ["two", "three", "four"]);
        let stats = ring.stats();
        assert_eq!(stats.boot_identity, BootIdentity::Id(BOOT_ID));
        assert_eq!(stats.submitted, 4);
        assert_eq!(stats.retained, 3);
        assert_eq!(stats.overwritten, 1);
        assert_eq!(stats.dropped, 0);
        assert_eq!(stats.oldest_sequence.map(EarlySequence::get), Some(2));
        assert_eq!(stats.newest_sequence.map(EarlySequence::get), Some(4));
    }

    #[test]
    fn multiple_wraparounds_preserve_order_and_copy_only_retained_records() {
        let mut ring = FixedEarlyLog::<2>::new(BootIdentity::Unavailable);
        for message in ["one", "two", "three", "four", "five"] {
            ring.push(input(message)).unwrap();
        }

        let mut output = [None; 4];
        let copied = ring.copy_chronological(&mut output);
        assert_eq!(copied, 2);
        assert_eq!(output[0].as_ref().unwrap().message(), "four");
        assert_eq!(output[1].as_ref().unwrap().message(), "five");
        assert_eq!(output[2], None);
        assert_eq!(ring.read_after(None).unwrap().sequence().get(), 4);
        assert_eq!(
            ring.read_after(NonZeroU64::new(4).map(EarlySequence).as_ref().copied())
                .unwrap()
                .sequence()
                .get(),
            5
        );
    }

    #[test]
    fn invalid_fields_and_zero_capacity_are_counted_without_consuming_sequences() {
        let mut ring = FixedEarlyLog::<1>::new(BootIdentity::Unavailable);
        let mut empty_subsystem = input("message");
        empty_subsystem.subsystem = "";
        assert_eq!(ring.push(empty_subsystem), Err(RecordError::EmptySubsystem));

        let mut long_subsystem = input("message");
        long_subsystem.subsystem = "kernel-subsystem-too-long";
        assert_eq!(
            ring.push(long_subsystem),
            Err(RecordError::SubsystemTooLong)
        );

        assert_eq!(
            ring.push(input("this message is intentionally longer than the fixed sixty-four-byte kernel log message field")),
            Err(RecordError::MessageTooLong)
        );
        let stored = ring.push(input("valid")).unwrap();
        assert_eq!(stored.sequence.get(), 1);
        assert_eq!(ring.stats().rejected, 3);
        assert_eq!(ring.stats().dropped, 3);

        let mut empty = FixedEarlyLog::<0>::new(BootIdentity::Unavailable);
        assert_eq!(empty.push(input("valid")), Err(RecordError::CapacityZero));
        assert_eq!(empty.stats().submitted, 1);
        assert_eq!(empty.stats().dropped, 1);
    }

    #[test]
    fn field_limits_are_exact_utf8_byte_limits() {
        const MAX_TEXT: &str = concat!(
            "0123456789abcdef",
            "0123456789abcdef",
            "0123456789abcdef",
            "0123456789abcdef"
        );
        let mut ring = FixedEarlyLog::<2>::new(BootIdentity::Unavailable);
        let mut maximum = input(MAX_TEXT);
        maximum.subsystem = "0123456789abcdef";
        ring.push(maximum).unwrap();
        assert_eq!(ring.get(0).unwrap().subsystem().len(), 16);
        assert_eq!(ring.get(0).unwrap().message().len(), 64);

        let mut multibyte = input("valid");
        multibyte.subsystem = "ééééééééé";
        assert_eq!(ring.push(multibyte), Err(RecordError::SubsystemTooLong));
    }

    #[test]
    fn maximum_sequence_is_assigned_once_and_never_wraps() {
        let mut ring = FixedEarlyLog::<1>::new(BootIdentity::Unavailable);
        ring.next_sequence = NonZeroU64::new(u64::MAX);

        assert_eq!(ring.push(input("last")).unwrap().sequence.get(), u64::MAX);
        assert_eq!(
            ring.push(input("never")),
            Err(RecordError::SequenceExhausted)
        );
        assert_eq!(ring.get(0).unwrap().message(), "last");
    }

    #[test]
    fn counters_saturate() {
        let mut ring = FixedEarlyLog::<1>::new(BootIdentity::Unavailable);
        ring.submitted = u64::MAX;
        ring.overwritten = u64::MAX;
        ring.dropped = u64::MAX;
        ring.rejected = u64::MAX;

        ring.push(input("one")).unwrap();
        ring.push(input("two")).unwrap();
        let mut invalid = input("bad");
        invalid.subsystem = "";
        assert_eq!(ring.push(invalid), Err(RecordError::EmptySubsystem));

        let stats = ring.stats();
        assert_eq!(stats.submitted, u64::MAX);
        assert_eq!(stats.overwritten, u64::MAX);
        assert_eq!(stats.dropped, u64::MAX);
        assert_eq!(stats.rejected, u64::MAX);
    }

    #[test]
    fn secrets_are_redacted_before_bytes_enter_storage() {
        const SECRET: &str = "raw credential material";
        let mut ring = FixedEarlyLog::<1>::new(BootIdentity::Unavailable);
        let mut secret = input(SECRET);
        secret.privacy = PrivacyClass::SecretNeverPersist;
        ring.push(secret).unwrap();

        let record = ring.get(0).unwrap();
        assert_eq!(record.message(), "[redacted: secret-never-persist]");
        assert!(
            !record
                .message
                .windows(SECRET.len())
                .any(|bytes| bytes == SECRET.as_bytes())
        );
    }

    #[test]
    fn record_preserves_structured_metadata_and_zeroes_padding() {
        let mut ring = FixedEarlyLog::<1>::new(BootIdentity::Unavailable);
        let mut record_input = input("ready");
        record_input.severity = LogSeverity::Notice;
        record_input.monotonic_time_ns = 123;
        record_input.source = EarlySource {
            cpu_id: Some(7),
            process_id: NonZeroU64::new(11),
            thread_id: NonZeroU64::new(13),
        };
        ring.push(record_input).unwrap();

        let record = ring.get(0).unwrap();
        assert_eq!(record.event_id(), EVENT);
        assert_eq!(record.severity(), LogSeverity::Notice);
        assert_eq!(record.monotonic_time_ns(), 123);
        assert_eq!(record.source().cpu_id, Some(7));
        assert_eq!(record.source().process_id.map(NonZeroU64::get), Some(11));
        assert!(
            record.subsystem[usize::from(record.subsystem_len)..]
                .iter()
                .all(|byte| *byte == 0)
        );
        assert!(
            record.message[usize::from(record.message_len)..]
                .iter()
                .all(|byte| *byte == 0)
        );
    }

    #[test]
    fn synchronized_writer_is_nonblocking_and_initializes_once() {
        let logger = SynchronizedEarlyLog::<2>::new();
        assert_eq!(
            logger.try_record(input("before")),
            Err(TryRecordError::Uninitialized)
        );
        logger.try_initialize(BootIdentity::Id(BOOT_ID)).unwrap();
        assert_eq!(
            logger.try_initialize(BootIdentity::Unavailable),
            Err(InitializeError::AlreadyInitialized)
        );

        let guard = logger.inner.lock();
        assert_eq!(logger.try_record(input("busy")), Err(TryRecordError::Busy));
        drop(guard);

        logger.try_record(input("stored")).unwrap();
        let snapshot = logger.try_stats().unwrap();
        assert_eq!(snapshot.stats.boot_identity, BootIdentity::Id(BOOT_ID));
        assert_eq!(snapshot.stats.retained, 1);
        assert_eq!(snapshot.busy_drops, 1);
    }

    #[test]
    fn snapshot_never_exposes_unused_slots() {
        let logger = SynchronizedEarlyLog::<3>::new();
        logger.try_initialize(BootIdentity::Unavailable).unwrap();
        logger.try_record(input("one")).unwrap();
        logger.try_record(input("two")).unwrap();

        let mut output: [Option<EarlyLogRecord>; 3] = [None; 3];
        let snapshot = logger.try_snapshot(&mut output).unwrap();
        assert_eq!(snapshot.copied, 2);
        assert_eq!(output[0].as_ref().unwrap().message(), "one");
        assert_eq!(output[1].as_ref().unwrap().message(), "two");
        assert_eq!(output[2], None);
    }
}
