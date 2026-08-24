// Shared protocol for userspace filesystem services.
//
// The transport permits one capability transfer per endpoint message. CONNECT
// spends that transfer on a persistent send-only reply endpoint. Later
// ATTACH_BUFFER requests can therefore transfer shared-memory capabilities
// while replies are multiplexed by request_id on the session endpoint.

pub const VERSION: u16 = 1;
pub const MAX_NAME_BYTES: usize = 96;
pub const MAX_INLINE_DATA_BYTES: usize = 64;
pub const WRITE_REPLY_OFFSET_BYTES: usize = 8;
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
    pub const TRUNCATE: u16 = 17;
    pub const RMDIR: u16 = 18;
    pub const SYNC: u16 = 19;
    pub const RESOLVE_IDENTITY: u16 = 20;
}

pub mod lifecycle {
    pub const MAGIC: [u8; 4] = *b"NFLC";
    pub const VERSION: u16 = 1;
    pub const MESSAGE_BYTES: usize = 24;

    pub mod kind {
        pub const QUIESCE: u16 = 1;
        pub const QUIESCED: u16 = 2;
        pub const UNMOUNT: u16 = 3;
        pub const CLEAN_UNMOUNTED: u16 = 4;
        pub const FAILED: u16 = 5;

        pub const fn known(value: u16) -> bool {
            matches!(
                value,
                QUIESCE | QUIESCED | UNMOUNT | CLEAN_UNMOUNTED | FAILED
            )
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Message {
        pub kind: u16,
        pub generation: u64,
        pub transition_id: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum DecodeError {
        Length,
        Magic,
        Version,
        Kind,
        Generation,
        Transition,
    }

    impl Message {
        pub const fn new(kind: u16, generation: u64, transition_id: u64) -> Option<Self> {
            if !kind::known(kind) || generation == 0 || transition_id == 0 {
                return None;
            }
            Some(Self {
                kind,
                generation,
                transition_id,
            })
        }

        pub fn encode(self) -> [u8; MESSAGE_BYTES] {
            let mut bytes = [0; MESSAGE_BYTES];
            bytes[..4].copy_from_slice(&MAGIC);
            bytes[4..6].copy_from_slice(&VERSION.to_le_bytes());
            bytes[6..8].copy_from_slice(&self.kind.to_le_bytes());
            bytes[8..16].copy_from_slice(&self.generation.to_le_bytes());
            bytes[16..24].copy_from_slice(&self.transition_id.to_le_bytes());
            bytes
        }

        pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
            if bytes.len() != MESSAGE_BYTES {
                return Err(DecodeError::Length);
            }
            if bytes[..4] != MAGIC {
                return Err(DecodeError::Magic);
            }
            let version = u16::from_le_bytes([bytes[4], bytes[5]]);
            if version != VERSION {
                return Err(DecodeError::Version);
            }
            let kind = u16::from_le_bytes([bytes[6], bytes[7]]);
            if !kind::known(kind) {
                return Err(DecodeError::Kind);
            }
            let generation = u64::from_le_bytes(
                bytes[8..16]
                    .try_into()
                    .expect("validated lifecycle generation width"),
            );
            if generation == 0 {
                return Err(DecodeError::Generation);
            }
            let transition_id = u64::from_le_bytes(
                bytes[16..24]
                    .try_into()
                    .expect("validated lifecycle transition width"),
            );
            if transition_id == 0 {
                return Err(DecodeError::Transition);
            }
            Ok(Self {
                kind,
                generation,
                transition_id,
            })
        }
    }
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
    pub const NOT_EMPTY: i32 = 16;
    pub const WOULD_CYCLE: i32 = 17;
    pub const OUTCOME_UNKNOWN: i32 = 18;
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

pub mod connect_flags {
    pub const WRITE: u32 = super::request_flags::WRITE;

    pub const ALL: u32 = WRITE;
}

pub mod session_features {
    pub const WRITE: u64 = 1;

    pub const ALL: u64 = WRITE;
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

pub fn encode_write_reply_offset(reply: &mut Reply, resulting_offset: u64) {
    reply.data_length = WRITE_REPLY_OFFSET_BYTES as u16;
    reply.data = [0; MAX_INLINE_DATA_BYTES];
    reply.data[..WRITE_REPLY_OFFSET_BYTES].copy_from_slice(&resulting_offset.to_le_bytes());
}

pub fn decode_write_reply_offset(reply: &Reply) -> Option<u64> {
    if usize::from(reply.data_length) != WRITE_REPLY_OFFSET_BYTES
        || reply.data[WRITE_REPLY_OFFSET_BYTES..]
            .iter()
            .any(|byte| *byte != 0)
    {
        return None;
    }

    let mut bytes = [0; WRITE_REPLY_OFFSET_BYTES];
    bytes.copy_from_slice(&reply.data[..WRITE_REPLY_OFFSET_BYTES]);
    Some(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod lifecycle_tests {
    use super::lifecycle::{self, DecodeError, Message, kind};

    #[test]
    fn lifecycle_message_has_a_canonical_golden_encoding() {
        let message = Message::new(
            kind::QUIESCE,
            0x0102_0304_0506_0708,
            0x1112_1314_1516_1718,
        )
        .unwrap();
        assert_eq!(
            message.encode(),
            [
                b'N', b'F', b'L', b'C', 1, 0, 1, 0, 8, 7, 6, 5, 4, 3, 2, 1, 0x18, 0x17,
                0x16, 0x15, 0x14, 0x13, 0x12, 0x11,
            ]
        );
        assert_eq!(Message::decode(&message.encode()), Ok(message));
        assert_eq!(lifecycle::MESSAGE_BYTES, 24);
        assert_eq!(super::VERSION, 1);
    }

    #[test]
    fn lifecycle_message_rejects_noncanonical_fields() {
        assert_eq!(Message::new(0, 1, 1), None);
        assert_eq!(Message::new(kind::QUIESCE, 0, 1), None);
        assert_eq!(Message::new(kind::QUIESCE, 1, 0), None);

        let valid = Message::new(kind::UNMOUNT, 7, 9).unwrap().encode();
        assert_eq!(
            Message::decode(&valid[..valid.len() - 1]),
            Err(DecodeError::Length)
        );
        let mut invalid = valid;
        invalid[0] ^= 1;
        assert_eq!(Message::decode(&invalid), Err(DecodeError::Magic));
        invalid = valid;
        invalid[4] = 2;
        assert_eq!(Message::decode(&invalid), Err(DecodeError::Version));
        invalid = valid;
        invalid[6..8].copy_from_slice(&99_u16.to_le_bytes());
        assert_eq!(Message::decode(&invalid), Err(DecodeError::Kind));
        invalid = valid;
        invalid[8..16].fill(0);
        assert_eq!(Message::decode(&invalid), Err(DecodeError::Generation));
        invalid = valid;
        invalid[16..24].fill(0);
        assert_eq!(Message::decode(&invalid), Err(DecodeError::Transition));
    }
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

/// Stable provider-owned identity for one generation-scoped node.
///
/// `node_id` values in requests and replies are session/provider-generation opaque. This record is
/// the explicit bridge to persistent resource identity and must be returned only by a provider that
/// owns the named filesystem UUID and object namespace.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StableNodeIdentity {
    pub filesystem_uuid: [u8; 16],
    pub object_id: u64,
    pub object_generation: u64,
    pub kind: u16,
    pub reserved: [u16; 3],
}

impl StableNodeIdentity {
    pub const EMPTY: Self = Self {
        filesystem_uuid: [0; 16],
        object_id: INVALID_ID,
        object_generation: 0,
        kind: node_kind::UNKNOWN,
        reserved: [0; 3],
    };

    pub fn new(
        filesystem_uuid: [u8; 16],
        object_id: u64,
        object_generation: u64,
        kind: u16,
    ) -> Option<Self> {
        if filesystem_uuid == [0; 16]
            || object_id == INVALID_ID
            || object_generation == 0
            || !matches!(
                kind,
                node_kind::FILE | node_kind::DIRECTORY | node_kind::SYMBOLIC_LINK
            )
        {
            return None;
        }
        Some(Self {
            filesystem_uuid,
            object_id,
            object_generation,
            kind,
            reserved: [0; 3],
        })
    }

    pub fn canonical(self) -> bool {
        self.reserved == [0; 3]
            && Self::new(
                self.filesystem_uuid,
                self.object_id,
                self.object_generation,
                self.kind,
            )
            .is_some()
    }
}
