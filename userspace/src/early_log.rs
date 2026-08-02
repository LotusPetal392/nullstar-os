//! Capability-authorized access to the fixed kernel early-log snapshot ABI.

use core::{arch::asm, mem::size_of, str};

use nswp_logging::{
    BootId, EventId, KernelLogRecordView, KernelSequence, LogSeverity, PrivacyClass,
};

use crate::{
    abi::{errno as abi_errno, syscall},
    ipc::CapabilityHandle,
};

mod wire {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/early_log_abi.rs"
    ));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Syscall(i32),
    InvalidResponse,
}

impl Error {
    pub const PERMISSION: Self = Self::Syscall((-abi_errno::PERMISSION) as i32);
    pub const TRY_AGAIN: Self = Self::Syscall((-abi_errno::TRY_AGAIN) as i32);

    pub const fn code(self) -> i32 {
        match self {
            Self::Syscall(code) => code,
            Self::InvalidResponse => i32::MIN,
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// A logging service that cannot import its pinned kernel history must not be
/// restarted into a newer snapshot that could hide the missing records.
pub const IMPORT_FAILURE_EXIT_STATUS: u64 = 21;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceMetadata {
    pub cpu_id: Option<u32>,
    pub process_id: Option<u64>,
    pub thread_id: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    pub submitted_records: u64,
    pub retained_records: u64,
    pub capacity_records: u64,
    pub overwritten_records: u64,
    pub dropped_records: u64,
    pub rejected_records: u64,
    pub busy_drops: u64,
    pub oldest_sequence: Option<KernelSequence>,
    pub newest_sequence: Option<KernelSequence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelRecord {
    pub sequence: KernelSequence,
    pub boot_id: Option<BootId>,
    pub event_id: EventId,
    pub severity: LogSeverity,
    pub privacy: PrivacyClass,
    pub monotonic_time_ns: u64,
    subsystem_len: u8,
    message_len: u8,
    subsystem: [u8; wire::SUBSYSTEM_BYTES],
    message: [u8; wire::MESSAGE_BYTES],
}

impl KernelRecord {
    pub fn view(&self) -> KernelLogRecordView<'_> {
        KernelLogRecordView {
            sequence: self.sequence,
            boot_id: self.boot_id,
            event_id: self.event_id,
            severity: self.severity,
            privacy: self.privacy,
            monotonic_time_ns: self.monotonic_time_ns,
            subsystem: self.subsystem(),
            message: self.message(),
        }
    }

    pub fn subsystem(&self) -> &str {
        str::from_utf8(&self.subsystem[..usize::from(self.subsystem_len)])
            .expect("validated kernel early-log subsystem must remain UTF-8")
    }

    pub fn message(&self) -> &str {
        str::from_utf8(&self.message[..usize::from(self.message_len)])
            .expect("validated kernel early-log message must remain UTF-8")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadResult {
    pub boot_id: Option<BootId>,
    pub stats: Stats,
    pub source: SourceMetadata,
    pub record: Option<KernelRecord>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotRange {
    boot_id: Option<BootId>,
    next_sequence: Option<KernelSequence>,
    newest_sequence: KernelSequence,
}

impl SnapshotRange {
    pub fn new(boot_id: Option<BootId>, stats: Stats) -> Result<Self> {
        let oldest_sequence = stats.oldest_sequence.ok_or(Error::InvalidResponse)?;
        let newest_sequence = stats.newest_sequence.ok_or(Error::InvalidResponse)?;
        Ok(Self {
            boot_id,
            next_sequence: Some(oldest_sequence),
            newest_sequence,
        })
    }

    /// Accepts one record from the pinned range and returns whether the range is complete.
    pub fn accept(&mut self, boot_id: Option<BootId>, sequence: KernelSequence) -> Result<bool> {
        let expected = self.next_sequence.ok_or(Error::InvalidResponse)?;
        if boot_id != self.boot_id || sequence != expected || sequence > self.newest_sequence {
            return Err(Error::InvalidResponse);
        }
        if sequence == self.newest_sequence {
            self.next_sequence = None;
            return Ok(true);
        }
        self.next_sequence = sequence
            .get()
            .checked_add(1)
            .and_then(|next| KernelSequence::new(next).ok());
        if self.next_sequence.is_none() {
            return Err(Error::InvalidResponse);
        }
        Ok(false)
    }
}

pub fn open_reader() -> Result<CapabilityHandle> {
    let mut result = syscall::OPEN_KERNEL_EARLY_LOG_READER;
    unsafe {
        asm!("int 0x80", inlateout("rax") result);
    }
    decode_syscall(result)
}

pub fn read_after(
    handle: CapabilityHandle,
    after: Option<KernelSequence>,
    storage: &mut ResponseStorage,
) -> Result<ReadResult> {
    storage.raw = wire::ReadResponse::EMPTY;
    let mut result = syscall::KERNEL_EARLY_LOG_READ;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
            in("rsi") after.map(KernelSequence::get).unwrap_or(0),
            in("rdx") (&mut storage.raw as *mut wire::ReadResponse) as u64,
            in("r10") size_of::<wire::ReadResponse>() as u64,
        );
    }
    decode_syscall(result)?;
    decode_response(&storage.raw)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResponseStorage {
    raw: wire::ReadResponse,
}

impl ResponseStorage {
    pub const fn new() -> Self {
        Self {
            raw: wire::ReadResponse::EMPTY,
        }
    }
}

impl Default for ResponseStorage {
    fn default() -> Self {
        Self::new()
    }
}

fn decode_syscall(result: u64) -> Result<u64> {
    let signed = result as i64;
    if signed < 0 {
        Err(Error::Syscall((-signed) as i32))
    } else {
        Ok(result)
    }
}

fn decode_response(response: &wire::ReadResponse) -> Result<ReadResult> {
    if response.version != wire::READ_RESPONSE_VERSION
        || response.flags & !wire::KNOWN_FLAGS != 0
        || response.reserved0 != 0
        || response.reserved1 != [0; 24]
    {
        return Err(Error::InvalidResponse);
    }

    let oldest_sequence = decode_optional_sequence(response.oldest_sequence)?;
    let newest_sequence = decode_optional_sequence(response.newest_sequence)?;
    validate_stats(response, oldest_sequence, newest_sequence)?;

    let boot_present = response.flags & wire::FLAG_BOOT_ID_PRESENT != 0;
    let boot_id = if boot_present {
        Some(BootId::from_bytes(response.boot_id).map_err(|_| Error::InvalidResponse)?)
    } else {
        if response.boot_id != [0; 16] {
            return Err(Error::InvalidResponse);
        }
        None
    };

    let present = response.flags & wire::FLAG_RECORD_PRESENT != 0;
    if !present {
        let source_flags = wire::FLAG_CPU_ID_PRESENT
            | wire::FLAG_PROCESS_ID_PRESENT
            | wire::FLAG_THREAD_ID_PRESENT;
        if response.flags & source_flags != 0
            || response.severity != 0
            || response.privacy != 0
            || response.subsystem_len != 0
            || response.message_len != 0
            || response.cpu_id != 0
            || response.sequence != 0
            || response.monotonic_time_ns != 0
            || response.process_id != 0
            || response.thread_id != 0
            || response.event_id != [0; 16]
            || response.subsystem != [0; wire::SUBSYSTEM_BYTES]
            || response.message != [0; wire::MESSAGE_BYTES]
        {
            return Err(Error::InvalidResponse);
        }
        return Ok(ReadResult {
            boot_id,
            stats: decode_stats(response, oldest_sequence, newest_sequence),
            source: SourceMetadata {
                cpu_id: None,
                process_id: None,
                thread_id: None,
            },
            record: None,
        });
    }

    let sequence = KernelSequence::new(response.sequence).map_err(|_| Error::InvalidResponse)?;
    if oldest_sequence.is_none_or(|oldest| sequence < oldest)
        || newest_sequence.is_none_or(|newest| sequence > newest)
    {
        return Err(Error::InvalidResponse);
    }
    let event_id = EventId::from_bytes(response.event_id).map_err(|_| Error::InvalidResponse)?;
    let severity =
        LogSeverity::try_from(u64::from(response.severity)).map_err(|_| Error::InvalidResponse)?;
    let privacy =
        PrivacyClass::try_from(u64::from(response.privacy)).map_err(|_| Error::InvalidResponse)?;
    let subsystem_len = usize::from(response.subsystem_len);
    let message_len = usize::from(response.message_len);
    if subsystem_len == 0
        || subsystem_len > wire::SUBSYSTEM_BYTES
        || message_len > wire::MESSAGE_BYTES
        || response.subsystem[subsystem_len..]
            .iter()
            .any(|byte| *byte != 0)
        || response.message[message_len..]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(Error::InvalidResponse);
    }
    str::from_utf8(&response.subsystem[..subsystem_len]).map_err(|_| Error::InvalidResponse)?;
    str::from_utf8(&response.message[..message_len]).map_err(|_| Error::InvalidResponse)?;

    let cpu_id = decode_optional_u32(
        response.flags & wire::FLAG_CPU_ID_PRESENT != 0,
        response.cpu_id,
    )?;
    let process_id = decode_optional_u64(
        response.flags & wire::FLAG_PROCESS_ID_PRESENT != 0,
        response.process_id,
    )?;
    let thread_id = decode_optional_u64(
        response.flags & wire::FLAG_THREAD_ID_PRESENT != 0,
        response.thread_id,
    )?;

    Ok(ReadResult {
        boot_id,
        stats: decode_stats(response, oldest_sequence, newest_sequence),
        source: SourceMetadata {
            cpu_id,
            process_id,
            thread_id,
        },
        record: Some(KernelRecord {
            sequence,
            boot_id,
            event_id,
            severity,
            privacy,
            monotonic_time_ns: response.monotonic_time_ns,
            subsystem_len: response.subsystem_len,
            message_len: response.message_len,
            subsystem: response.subsystem,
            message: response.message,
        }),
    })
}

fn validate_stats(
    response: &wire::ReadResponse,
    oldest_sequence: Option<KernelSequence>,
    newest_sequence: Option<KernelSequence>,
) -> Result<()> {
    if response.retained_records > response.capacity_records
        || response.rejected_records > response.dropped_records
        || (response.overwritten_records != 0
            && response.retained_records != response.capacity_records)
        || (response.retained_records == 0) != oldest_sequence.is_none()
        || (response.retained_records == 0) != newest_sequence.is_none()
    {
        return Err(Error::InvalidResponse);
    }

    let accepted_records = response
        .retained_records
        .saturating_add(response.overwritten_records);
    if response.submitted_records != accepted_records.saturating_add(response.dropped_records) {
        return Err(Error::InvalidResponse);
    }

    if let Some((oldest, newest)) = oldest_sequence.zip(newest_sequence) {
        let retained_span = newest
            .get()
            .checked_sub(oldest.get())
            .and_then(|difference| difference.checked_add(1))
            .ok_or(Error::InvalidResponse)?;
        if retained_span != response.retained_records || newest.get() != accepted_records {
            return Err(Error::InvalidResponse);
        }
    } else if accepted_records != 0 {
        return Err(Error::InvalidResponse);
    }

    Ok(())
}

fn decode_stats(
    response: &wire::ReadResponse,
    oldest_sequence: Option<KernelSequence>,
    newest_sequence: Option<KernelSequence>,
) -> Stats {
    Stats {
        submitted_records: response.submitted_records,
        retained_records: response.retained_records,
        capacity_records: response.capacity_records,
        overwritten_records: response.overwritten_records,
        dropped_records: response.dropped_records,
        rejected_records: response.rejected_records,
        busy_drops: response.busy_drops,
        oldest_sequence,
        newest_sequence,
    }
}

fn decode_optional_sequence(raw: u64) -> Result<Option<KernelSequence>> {
    if raw == 0 {
        Ok(None)
    } else {
        KernelSequence::new(raw)
            .map(Some)
            .map_err(|_| Error::InvalidResponse)
    }
}

fn decode_optional_u32(present: bool, value: u32) -> Result<Option<u32>> {
    if present {
        Ok(Some(value))
    } else if value == 0 {
        Ok(None)
    } else {
        Err(Error::InvalidResponse)
    }
}

fn decode_optional_u64(present: bool, value: u64) -> Result<Option<u64>> {
    if present && value != 0 {
        Ok(Some(value))
    } else if !present && value == 0 {
        Ok(None)
    } else {
        Err(Error::InvalidResponse)
    }
}

#[cfg(test)]
mod tests {
    use core::mem::{align_of, size_of};

    use nswp_logging::KernelSequence;

    use super::{Error, ResponseStorage, SnapshotRange, Stats, decode_response, wire};

    #[test]
    fn fixed_response_has_the_documented_shape() {
        assert_eq!(size_of::<wire::ReadResponse>(), 256);
        assert_eq!(align_of::<wire::ReadResponse>(), 8);
        assert_eq!(size_of::<ResponseStorage>(), 256);
    }

    #[test]
    fn decoder_accepts_canonical_record_and_end_responses() {
        let mut record = wire::ReadResponse::EMPTY;
        record.flags = wire::FLAG_RECORD_PRESENT;
        record.sequence = 1;
        record.event_id = [
            0x31, 0x86, 0x6a, 0xaf, 0x91, 0x5b, 0x42, 0x45, 0x89, 0x76, 0xe1, 0x64, 0x2a, 0x2f,
            0x66, 0x7d,
        ];
        record.severity = 2;
        record.privacy = 0;
        record.submitted_records = 1;
        record.retained_records = 1;
        record.capacity_records = 64;
        record.oldest_sequence = 1;
        record.newest_sequence = 1;
        record.subsystem_len = 6;
        record.subsystem[..6].copy_from_slice(b"kernel");
        record.message_len = 5;
        record.message[..5].copy_from_slice(b"ready");

        let decoded = decode_response(&record).unwrap();
        assert_eq!(decoded.record.unwrap().sequence.get(), 1);

        let mut end = wire::ReadResponse::EMPTY;
        end.submitted_records = 1;
        end.retained_records = 1;
        end.capacity_records = 64;
        end.oldest_sequence = 1;
        end.newest_sequence = 1;
        assert!(decode_response(&end).unwrap().record.is_none());
    }

    #[test]
    fn pinned_snapshot_rejects_an_overwrite_gap() {
        let stats = Stats {
            submitted_records: 4,
            retained_records: 3,
            capacity_records: 64,
            overwritten_records: 1,
            dropped_records: 0,
            rejected_records: 0,
            busy_drops: 0,
            oldest_sequence: Some(KernelSequence::new(2).unwrap()),
            newest_sequence: Some(KernelSequence::new(4).unwrap()),
        };
        let mut range = SnapshotRange::new(None, stats).unwrap();
        assert!(!range.accept(None, KernelSequence::new(2).unwrap()).unwrap());
        assert_eq!(
            range.accept(None, KernelSequence::new(4).unwrap()),
            Err(Error::InvalidResponse)
        );
    }

    #[test]
    fn decoder_rejects_impossible_ring_accounting() {
        let mut response = wire::ReadResponse::EMPTY;
        response.retained_records = 1;
        response.capacity_records = 64;
        response.oldest_sequence = 1;
        response.newest_sequence = 1;
        assert_eq!(decode_response(&response), Err(Error::InvalidResponse));

        response.submitted_records = 2;
        response.dropped_records = 1;
        response.rejected_records = 2;
        assert_eq!(decode_response(&response), Err(Error::InvalidResponse));

        response.rejected_records = 0;
        response.dropped_records = 0;
        response.overwritten_records = 1;
        assert_eq!(decode_response(&response), Err(Error::InvalidResponse));

        response.retained_records = 64;
        response.submitted_records = 65;
        response.oldest_sequence = 2;
        response.newest_sequence = 64;
        assert_eq!(decode_response(&response), Err(Error::InvalidResponse));
    }

    #[test]
    fn decoder_accepts_saturated_accounting() {
        let mut response = wire::ReadResponse::EMPTY;
        response.submitted_records = u64::MAX;
        response.retained_records = 1;
        response.capacity_records = 1;
        response.overwritten_records = u64::MAX - 1;
        response.oldest_sequence = u64::MAX;
        response.newest_sequence = u64::MAX;
        assert!(decode_response(&response).is_ok());
    }

    #[test]
    fn decoder_rejects_unknown_flags_and_noncanonical_absence() {
        let mut response = wire::ReadResponse::EMPTY;
        response.flags = 1 << 15;
        assert_eq!(decode_response(&response), Err(Error::InvalidResponse));

        response = wire::ReadResponse::EMPTY;
        response.sequence = 1;
        assert_eq!(decode_response(&response), Err(Error::InvalidResponse));

        response = wire::ReadResponse::EMPTY;
        response.retained_records = 2;
        response.capacity_records = 64;
        response.oldest_sequence = 1;
        response.newest_sequence = 1;
        assert_eq!(decode_response(&response), Err(Error::InvalidResponse));

        response = wire::ReadResponse::EMPTY;
        response.flags = wire::FLAG_RECORD_PRESENT;
        response.sequence = 1;
        assert_eq!(decode_response(&response), Err(Error::InvalidResponse));
    }
}
