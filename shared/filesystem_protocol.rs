// Shared protocol for userspace filesystem services.
//
// The transport permits one capability transfer per endpoint message. CONNECT
// spends that transfer on a persistent send-only reply endpoint. Later
// ATTACH_BUFFER requests can therefore transfer shared-memory capabilities
// while replies are multiplexed by request_id on the session endpoint.

pub const VERSION: u16 = 1;
pub const MAX_NAME_BYTES: usize = 96;
pub const MAX_INLINE_DATA_BYTES: usize = 64;
pub const ROOT_NODE_ID: u64 = 1;
pub const INVALID_ID: u64 = 0;

pub mod operation {
    pub const CONNECT: u16 = 1;
    pub const ATTACH_BUFFER: u16 = 2;
    pub const DETACH_BUFFER: u16 = 3;
    pub const LOOKUP: u16 = 4;
    pub const GET_ATTRIBUTES: u16 = 5;
    pub const OPEN: u16 = 6;
    pub const READ: u16 = 7;
    pub const WRITE: u16 = 8;
    pub const READ_DIRECTORY: u16 = 9;
    pub const CREATE_FILE: u16 = 10;
    pub const CREATE_DIRECTORY: u16 = 11;
    pub const UNLINK: u16 = 12;
    pub const RENAME: u16 = 13;
    pub const CLOSE_NODE: u16 = 14;
    pub const CANCEL: u16 = 15;
    pub const DISCONNECT: u16 = 16;
}

pub mod status {
    pub const OK: i32 = 0;
    pub const INVALID: i32 = 1;
    pub const NOT_FOUND: i32 = 2;
    pub const NOT_DIRECTORY: i32 = 3;
    pub const IS_DIRECTORY: i32 = 4;
    pub const EXISTS: i32 = 5;
    pub const PERMISSION: i32 = 6;
    pub const NO_SPACE: i32 = 7;
    pub const RANGE: i32 = 8;
    pub const STALE_SESSION: i32 = 9;
    pub const STALE_NODE: i32 = 10;
    pub const STALE_BUFFER: i32 = 11;
    pub const TRY_AGAIN: i32 = 12;
    pub const IO: i32 = 13;
    pub const NOT_SUPPORTED: i32 = 14;
    pub const CANCELLED: i32 = 15;
}

pub mod request_flags {
    pub const CREATE: u32 = 1 << 0;
    pub const EXCLUSIVE: u32 = 1 << 1;
    pub const TRUNCATE: u32 = 1 << 2;
    pub const APPEND: u32 = 1 << 3;
    pub const READ: u32 = 1 << 4;
    pub const WRITE: u32 = 1 << 5;

    pub const ALL: u32 = CREATE | EXCLUSIVE | TRUNCATE | APPEND | READ | WRITE;
}

pub mod reply_flags {
    pub const END_OF_DIRECTORY: u32 = 1 << 0;

    pub const ALL: u32 = END_OF_DIRECTORY;
}

pub mod node_kind {
    pub const UNKNOWN: u16 = 0;
    pub const FILE: u16 = 1;
    pub const DIRECTORY: u16 = 2;
    pub const SYMBOLIC_LINK: u16 = 3;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BulkBuffer {
    pub buffer_id: u64,
    pub offset: u64,
    pub length: u64,
}

impl BulkBuffer {
    pub const NONE: Self = Self {
        buffer_id: INVALID_ID,
        offset: 0,
        length: 0,
    };

    pub const fn end(self) -> Option<u64> {
        self.offset.checked_add(self.length)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub node_id: u64,
    pub next_cookie: u64,
    pub kind: u16,
    pub name_length: u16,
    pub reserved: u32,
    pub name: [u8; MAX_NAME_BYTES],
}

impl DirectoryEntry {
    pub const EMPTY: Self = Self {
        node_id: INVALID_ID,
        next_cookie: 0,
        kind: node_kind::UNKNOWN,
        name_length: 0,
        reserved: 0,
        name: [0; MAX_NAME_BYTES],
    };
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub version: u16,
    pub operation: u16,
    pub flags: u32,
    pub request_id: u64,
    pub session_id: u64,
    pub generation: u64,
    pub node_id: u64,
    pub secondary_node_id: u64,
    pub file_offset: u64,
    pub bulk: BulkBuffer,
    pub name_length: u16,
    pub reserved: [u16; 3],
    pub name: [u8; MAX_NAME_BYTES],
}

impl Request {
    pub const EMPTY: Self = Self {
        version: VERSION,
        operation: 0,
        flags: 0,
        request_id: INVALID_ID,
        session_id: INVALID_ID,
        generation: 0,
        node_id: INVALID_ID,
        secondary_node_id: INVALID_ID,
        file_offset: 0,
        bulk: BulkBuffer::NONE,
        name_length: 0,
        reserved: [0; 3],
        name: [0; MAX_NAME_BYTES],
    };
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reply {
    pub version: u16,
    pub operation: u16,
    pub status: i32,
    pub flags: u32,
    pub request_id: u64,
    pub session_id: u64,
    pub generation: u64,
    pub node_id: u64,
    pub value: u64,
    pub data_length: u16,
    pub node_kind: u16,
    pub reserved: [u32; 2],
    pub data: [u8; MAX_INLINE_DATA_BYTES],
}

impl Reply {
    pub const EMPTY: Self = Self {
        version: VERSION,
        operation: 0,
        status: status::OK,
        flags: 0,
        request_id: INVALID_ID,
        session_id: INVALID_ID,
        generation: 0,
        node_id: INVALID_ID,
        value: 0,
        data_length: 0,
        node_kind: node_kind::UNKNOWN,
        reserved: [0; 2],
        data: [0; MAX_INLINE_DATA_BYTES],
    };
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeAttributes {
    pub node_id: u64,
    pub size: u64,
    pub allocated_size: u64,
    pub created_nanoseconds: u64,
    pub modified_nanoseconds: u64,
    pub changed_nanoseconds: u64,
    pub kind: u16,
    pub mode: u16,
    pub link_count: u32,
    pub flags: u64,
}

impl NodeAttributes {
    pub const EMPTY: Self = Self {
        node_id: INVALID_ID,
        size: 0,
        allocated_size: 0,
        created_nanoseconds: 0,
        modified_nanoseconds: 0,
        changed_nanoseconds: 0,
        kind: node_kind::UNKNOWN,
        mode: 0,
        link_count: 0,
        flags: 0,
    };
}
