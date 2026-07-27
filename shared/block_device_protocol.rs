// Shared protocol for capability-based partition block-device services.
//
// Endpoint messages are fixed-size and bounded. CONNECT transfers a persistent
// send-only reply endpoint; later ATTACH_BUFFER transfers the session's one
// shared-memory capability. All subsequent replies are multiplexed by request_id
// on that persistent endpoint. Block offsets are relative to the exposed
// partition, never to the containing disk.

pub const VERSION: u16 = 1;
pub const MAX_MESSAGE_BYTES: usize = 256;
pub const MAX_TRANSFER_BYTES: usize = 4096;
pub const INITIAL_LOGICAL_BLOCK_SIZE: u32 = 512;
pub const INVALID_ID: u64 = 0;

pub mod operation {
    pub const CONNECT: u16 = 1;
    pub const ATTACH_BUFFER: u16 = 2;
    pub const INFO: u16 = 3;
    pub const READ: u16 = 4;
    pub const WRITE: u16 = 5;
    pub const FLUSH: u16 = 6;
    pub const DISCONNECT: u16 = 7;

    pub const fn is_defined(operation: u16) -> bool {
        operation >= CONNECT && operation <= DISCONNECT
    }
}

pub mod status {
    pub const OK: i32 = 0;
    pub const INVALID: i32 = 1;
    pub const PERMISSION: i32 = 2;
    pub const RANGE: i32 = 3;
    pub const STALE_SESSION: i32 = 4;
    pub const STALE_BUFFER: i32 = 5;
    pub const READ_ONLY: i32 = 6;
    pub const TRY_AGAIN: i32 = 7;
    pub const IO: i32 = 8;
    pub const NOT_SUPPORTED: i32 = 9;
}

pub mod request_flags {
    // No request flags are defined in version 1. Receivers must reject unknown
    // bits so future meanings cannot be interpreted accidentally.
    pub const ALL: u32 = 0;
}

pub mod features {
    pub const READ: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;
    pub const FLUSH: u64 = 1 << 2;

    pub const ALL: u64 = READ | WRITE | FLUSH;
}

pub mod device_flags {
    pub const READ_ONLY: u32 = 1 << 0;

    pub const ALL: u32 = READ_ONLY;
}

/// Canonical request record for every protocol operation.
///
/// For READ and WRITE, `block_offset` and `block_count` identify partition-
/// relative logical blocks. `buffer_offset` and `buffer_length` identify the
/// exact byte range in the registered buffer. ATTACH_BUFFER uses `buffer_id`
/// and `buffer_length`, with `buffer_offset` zero.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub version: u16,
    pub operation: u16,
    pub flags: u32,
    pub request_id: u64,
    pub session_id: u64,
    pub generation: u64,
    pub buffer_id: u64,
    pub buffer_offset: u64,
    pub buffer_length: u64,
    pub block_offset: u64,
    pub block_count: u32,
    pub reserved: [u32; 3],
}

impl Request {
    pub const EMPTY: Self = Self {
        version: VERSION,
        operation: 0,
        flags: 0,
        request_id: INVALID_ID,
        session_id: INVALID_ID,
        generation: 0,
        buffer_id: INVALID_ID,
        buffer_offset: 0,
        buffer_length: 0,
        block_offset: 0,
        block_count: 0,
        reserved: [0; 3],
    };

    pub const fn buffer_end(self) -> Option<u64> {
        self.buffer_offset.checked_add(self.buffer_length)
    }
}

/// Canonical reply record for every protocol operation.
///
/// INFO returns `features`, `block_count`, `logical_block_size`, and
/// `device_flags`. Successful READ and WRITE replies identify `buffer_id` and
/// report `transferred_blocks`; clients may require full completion.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reply {
    pub version: u16,
    pub operation: u16,
    pub status: i32,
    pub request_id: u64,
    pub session_id: u64,
    pub generation: u64,
    pub features: u64,
    pub block_count: u64,
    pub buffer_id: u64,
    pub logical_block_size: u32,
    pub transferred_blocks: u32,
    pub device_flags: u32,
    pub reserved: [u32; 3],
}

impl Reply {
    pub const EMPTY: Self = Self {
        version: VERSION,
        operation: 0,
        status: status::OK,
        request_id: INVALID_ID,
        session_id: INVALID_ID,
        generation: 0,
        features: 0,
        block_count: 0,
        buffer_id: INVALID_ID,
        logical_block_size: 0,
        transferred_blocks: 0,
        device_flags: 0,
        reserved: [0; 3],
    };
}
