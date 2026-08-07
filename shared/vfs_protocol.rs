// Userspace VFS namespace routing protocol.
//
// The VFS service owns mount-point selection. A successful resolution returns
// the longest matching namespace prefix and the backend that owns the rest of
// the path. Binding routes also return one canonical, backend-relative prefix;
// the kernel preserves the requested logical path while traversing that target.

pub const VERSION: u16 = 2;
pub const MAX_PATH_BYTES: usize = 192;

pub mod operation {
    pub const RESOLVE: u16 = 1;
}

pub mod status {
    pub const OK: i32 = 0;
    pub const INVALID: i32 = 1;
    pub const NOT_FOUND: i32 = 2;

    pub const fn known(value: i32) -> bool {
        matches!(value, OK | INVALID | NOT_FOUND)
    }
}

pub mod backend {
    pub const NAMESPACE: u16 = 1;
    pub const BOOT_FILESYSTEM: u16 = 2;
    pub const TMPFS: u16 = 3;
    pub const NULLFS: u16 = 4;
}

pub mod reply_flags {
    pub const BINDING: u16 = 1 << 0;
    pub const ALL: u16 = BINDING;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingError {
    Flags,
    Length,
    Path,
    Padding,
}

pub mod route {
    pub const ROOT: u32 = 1;
    pub const DEV: u32 = 2;
    pub const TMP: u32 = 3;
    pub const SYSTEM: u32 = 4;
    pub const SYSTEM_CONFIG: u32 = 5;
    pub const SYSTEM_VAR_LOG: u32 = 6;
    pub const SYSTEM_BIN: u32 = 7;
    pub const SYSTEM_SERVICES: u32 = 8;
    pub const SYSTEM_DRIVERS: u32 = 9;
    pub const SYSTEM_LIB: u32 = 10;
    pub const SYSTEM_APPLICATIONS: u32 = 11;
    pub const USERS: u32 = 12;
    pub const APPLICATIONS: u32 = 13;
    pub const VOLUMES: u32 = 14;
    pub const SYSTEM_VAR: u32 = 15;
    pub const NULLSTAR_VOLUME: u32 = 16;
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Request {
    pub version: u16,
    pub operation: u16,
    pub request_id: u32,
    pub path_length: u16,
    pub reserved: [u8; 6],
    pub path: [u8; MAX_PATH_BYTES],
}

impl Request {
    pub const EMPTY: Self = Self {
        version: VERSION,
        operation: 0,
        request_id: 0,
        path_length: 0,
        reserved: [0; 6],
        path: [0; MAX_PATH_BYTES],
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Reply {
    pub version: u16,
    pub operation: u16,
    pub request_id: u32,
    pub status: i32,
    pub route_id: u32,
    pub backend: u16,
    pub prefix_length: u16,
    pub backing_prefix_length: u16,
    pub flags: u16,
    pub reserved: [u8; 8],
    pub backing_prefix: [u8; MAX_PATH_BYTES],
}

impl Reply {
    pub const EMPTY: Self = Self {
        version: VERSION,
        operation: 0,
        request_id: 0,
        status: status::OK,
        route_id: 0,
        backend: 0,
        prefix_length: 0,
        backing_prefix_length: 0,
        flags: 0,
        reserved: [0; 8],
        backing_prefix: [0; MAX_PATH_BYTES],
    };

    pub fn binding_prefix(&self) -> Result<Option<&str>, BindingError> {
        if self.flags & !reply_flags::ALL != 0 {
            return Err(BindingError::Flags);
        }
        let length = usize::from(self.backing_prefix_length);
        if self.flags == 0 {
            if length != 0 {
                return Err(BindingError::Length);
            }
            return if self.backing_prefix.iter().all(|byte| *byte == 0) {
                Ok(None)
            } else {
                Err(BindingError::Padding)
            };
        }
        if self.flags != reply_flags::BINDING {
            return Err(BindingError::Flags);
        }
        if length == 0 || length > self.backing_prefix.len() {
            return Err(BindingError::Length);
        }
        let prefix = &self.backing_prefix[..length];
        if prefix[0] != b'/'
            || (length > 1 && prefix[length - 1] == b'/')
            || prefix.contains(&0)
        {
            return Err(BindingError::Path);
        }
        if self.backing_prefix[length..].iter().any(|byte| *byte != 0) {
            return Err(BindingError::Padding);
        }
        core::str::from_utf8(prefix)
            .map(Some)
            .map_err(|_| BindingError::Path)
    }
}

pub fn path_has_prefix(path: &str, prefix: &str) -> bool {
    path == prefix || (path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/'))
}
