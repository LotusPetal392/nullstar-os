// Fixed kernel early-log reader ABI shared by the kernel and userspace runtime.
// This is an explicit wire-like layout; it must not expose Rust enum, Option, or
// private kernel-record representation details.

pub const READ_RESPONSE_VERSION: u16 = 1;
pub const READ_RESPONSE_BYTES: usize = 256;

pub const FLAG_RECORD_PRESENT: u16 = 1 << 0;
pub const FLAG_BOOT_ID_PRESENT: u16 = 1 << 1;
pub const FLAG_CPU_ID_PRESENT: u16 = 1 << 2;
pub const FLAG_PROCESS_ID_PRESENT: u16 = 1 << 3;
pub const FLAG_THREAD_ID_PRESENT: u16 = 1 << 4;
pub const KNOWN_FLAGS: u16 = FLAG_RECORD_PRESENT
    | FLAG_BOOT_ID_PRESENT
    | FLAG_CPU_ID_PRESENT
    | FLAG_PROCESS_ID_PRESENT
    | FLAG_THREAD_ID_PRESENT;

pub const SUBSYSTEM_BYTES: usize = 16;
pub const MESSAGE_BYTES: usize = 64;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadResponse {
    pub version: u16,
    pub flags: u16,
    pub severity: u8,
    pub privacy: u8,
    pub subsystem_len: u8,
    pub message_len: u8,
    pub cpu_id: u32,
    pub reserved0: u32,
    pub sequence: u64,
    pub monotonic_time_ns: u64,
    pub process_id: u64,
    pub thread_id: u64,
    pub event_id: [u8; 16],
    pub boot_id: [u8; 16],
    pub submitted_records: u64,
    pub retained_records: u64,
    pub capacity_records: u64,
    pub overwritten_records: u64,
    pub dropped_records: u64,
    pub rejected_records: u64,
    pub busy_drops: u64,
    pub oldest_sequence: u64,
    pub newest_sequence: u64,
    pub subsystem: [u8; SUBSYSTEM_BYTES],
    pub message: [u8; MESSAGE_BYTES],
    pub reserved1: [u8; 24],
}

impl ReadResponse {
    pub const EMPTY: Self = Self {
        version: READ_RESPONSE_VERSION,
        flags: 0,
        severity: 0,
        privacy: 0,
        subsystem_len: 0,
        message_len: 0,
        cpu_id: 0,
        reserved0: 0,
        sequence: 0,
        monotonic_time_ns: 0,
        process_id: 0,
        thread_id: 0,
        event_id: [0; 16],
        boot_id: [0; 16],
        submitted_records: 0,
        retained_records: 0,
        capacity_records: 0,
        overwritten_records: 0,
        dropped_records: 0,
        rejected_records: 0,
        busy_drops: 0,
        oldest_sequence: 0,
        newest_sequence: 0,
        subsystem: [0; SUBSYSTEM_BYTES],
        message: [0; MESSAGE_BYTES],
        reserved1: [0; 24],
    };
}

const _: () = assert!(core::mem::size_of::<ReadResponse>() == READ_RESPONSE_BYTES);
const _: () = assert!(core::mem::align_of::<ReadResponse>() == 8);
const _: () = assert!(core::mem::offset_of!(ReadResponse, sequence) == 16);
const _: () = assert!(core::mem::offset_of!(ReadResponse, event_id) == 48);
const _: () = assert!(core::mem::offset_of!(ReadResponse, submitted_records) == 80);
const _: () = assert!(core::mem::offset_of!(ReadResponse, subsystem) == 152);
const _: () = assert!(core::mem::offset_of!(ReadResponse, message) == 168);
