// Shared userspace tmpfs service protocol.
//
// Requests and replies are fixed-size bounded records that fit within the
// phase-one endpoint message limit. Names are single path components relative
// to the service root; directories are intentionally deferred.

pub const VERSION: u16 = 1;
pub const MAX_NAME_BYTES: usize = 48;
pub const MAX_DATA_BYTES: usize = 128;
pub const MAX_FILES: usize = 16;
pub const MAX_FILE_BYTES: usize = 1024;

pub mod operation {
    pub const WRITE: u16 = 1;
    pub const READ: u16 = 2;
    pub const STAT: u16 = 3;
    pub const REMOVE: u16 = 4;
    pub const LIST: u16 = 5;
}

pub mod status {
    pub const OK: i32 = 0;
    pub const INVALID: i32 = 1;
    pub const NOT_FOUND: i32 = 2;
    pub const NO_SPACE: i32 = 3;
    pub const RANGE: i32 = 4;
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Request {
    pub version: u16,
    pub operation: u16,
    pub name_length: u16,
    pub data_length: u16,
    pub offset: u32,
    pub name: [u8; MAX_NAME_BYTES],
    pub data: [u8; MAX_DATA_BYTES],
}

impl Request {
    pub const EMPTY: Self = Self {
        version: VERSION,
        operation: 0,
        name_length: 0,
        data_length: 0,
        offset: 0,
        name: [0; MAX_NAME_BYTES],
        data: [0; MAX_DATA_BYTES],
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Reply {
    pub version: u16,
    pub operation: u16,
    pub status: i32,
    pub value: u32,
    pub data_length: u16,
    pub reserved: u16,
    pub data: [u8; MAX_DATA_BYTES],
}

impl Reply {
    pub const EMPTY: Self = Self {
        version: VERSION,
        operation: 0,
        status: status::OK,
        value: 0,
        data_length: 0,
        reserved: 0,
        data: [0; MAX_DATA_BYTES],
    };
}
