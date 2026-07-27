// Shared NullStar OS ring-3 ABI constants. This file is included by both the
// kernel process manager and the userspace runtime so numeric definitions stay
// in one place without introducing a runtime dependency.

/// Software interrupt vector used by the NullStar OS userspace ABI.
pub const SYSCALL_VECTOR: u8 = 0x80;

/// Process identifier reserved for the first userspace process.
pub const INIT_PROCESS_ID: u64 = 1;

/// First documented version of the NullStar OS userspace ABI.
pub const ABI_VERSION_MAJOR: u64 = 1;
pub const ABI_VERSION_MINOR: u64 = 6;

pub mod syscall {
    pub const WRITE: u64 = 1;
    pub const YIELD: u64 = 2;
    pub const EXIT: u64 = 3;
    pub const OPEN: u64 = 4;
    pub const READ: u64 = 5;
    pub const CLOSE: u64 = 6;
    // Retained for version-1 ABI compatibility. Bundled userspace builds
    // launches from fork, descriptor, process-group, and exec primitives.
    pub const SPAWN_COMMAND: u64 = 7;
    pub const WAIT_CHILD: u64 = 8;
    pub const GETPID: u64 = 9;
    pub const PIPE_PAIR: u64 = 10;
    pub const TRY_WAIT_CHILD: u64 = 11;
    pub const SIGNAL_PROCESS_GROUP: u64 = 12;
    pub const FOREGROUND_PROCESS_GROUP: u64 = 13;
    pub const SEEK: u64 = 14;
    pub const EXECVE: u64 = 15;
    pub const SET_DESCRIPTOR_FLAGS: u64 = 16;
    pub const FORK: u64 = 17;
    pub const SIGNAL_ACTION: u64 = 18;
    pub const SIGNAL_MASK: u64 = 19;
    pub const SIGNAL_RETURN: u64 = 20;
    pub const ENVIRONMENT_SET: u64 = 21;
    pub const ENVIRONMENT_UNSET: u64 = 22;

    // Userspace platform ABI v1.
    pub const SYSTEM_INFO: u64 = 23;
    pub const STAT: u64 = 24;
    pub const FSTAT: u64 = 25;
    pub const READ_DIRECTORY: u64 = 26;
    pub const CHDIR: u64 = 27;
    pub const GETCWD: u64 = 28;
    pub const DUP: u64 = 29;
    pub const DUP2: u64 = 30;
    pub const GETPPID: u64 = 31;
    pub const KILL: u64 = 32;
    pub const GET_PROCESS_GROUP: u64 = 33;
    pub const SET_PROCESS_GROUP: u64 = 34;

    // Capability and IPC ABI v1. Kernel operations are deliberately bounded
    // and non-blocking; the userspace facade provides cooperative blocking
    // helpers by yielding on TRY_AGAIN.
    pub const CAPABILITY_DUPLICATE: u64 = 35;
    pub const CAPABILITY_CLOSE: u64 = 36;
    pub const CAPABILITY_INFO: u64 = 37;
    pub const ENDPOINT_CREATE: u64 = 38;
    pub const ENDPOINT_SEND: u64 = 39;
    pub const ENDPOINT_RECEIVE: u64 = 40;
    pub const NOTIFICATION_CREATE: u64 = 41;
    pub const NOTIFICATION_SIGNAL: u64 = 42;
    pub const NOTIFICATION_TRY_WAIT: u64 = 43;
    pub const SHARED_MEMORY_CREATE: u64 = 44;
    pub const SHARED_MEMORY_READ: u64 = 45;
    pub const SHARED_MEMORY_WRITE: u64 = 46;
    pub const ENDPOINT_WAIT: u64 = 47;
    pub const REGISTER_TMPFS_SERVICE: u64 = 49;
    pub const REGISTER_VFS_SERVICE: u64 = 50;
    pub const UNLINK: u64 = 51;
}

pub mod capability {
    pub const FILE_METADATA: u64 = 1 << 0;
    pub const DIRECTORY_READ: u64 = 1 << 1;
    pub const WORKING_DIRECTORY: u64 = 1 << 2;
    pub const DESCRIPTOR_DUPLICATION: u64 = 1 << 3;
    pub const PARENT_PROCESS: u64 = 1 << 4;
    pub const DIRECT_SIGNALS: u64 = 1 << 5;
    pub const PROCESS_GROUP_CONTROL: u64 = 1 << 6;
    pub const HANDLE_TABLE: u64 = 1 << 7;
    pub const IPC_ENDPOINTS: u64 = 1 << 8;
    pub const NOTIFICATIONS: u64 = 1 << 9;
    pub const SHARED_MEMORY_OBJECTS: u64 = 1 << 10;
    pub const BLOCKING_ENDPOINT_WAIT: u64 = 1 << 11;
    pub const PATH_MUTATION: u64 = 1 << 12;

    pub const PLATFORM_V1: u64 = FILE_METADATA
        | DIRECTORY_READ
        | WORKING_DIRECTORY
        | DESCRIPTOR_DUPLICATION
        | PARENT_PROCESS
        | DIRECT_SIGNALS
        | PROCESS_GROUP_CONTROL;

    pub const PROTECTION_V1: u64 = HANDLE_TABLE
        | IPC_ENDPOINTS
        | NOTIFICATIONS
        | SHARED_MEMORY_OBJECTS
        | BLOCKING_ENDPOINT_WAIT
        | PATH_MUTATION;

    pub const INVALID_HANDLE: u64 = 0;

    pub const KIND_ENDPOINT: u64 = 1;
    pub const KIND_NOTIFICATION: u64 = 2;
    pub const KIND_SHARED_MEMORY: u64 = 3;

    pub const RIGHT_DUPLICATE: u64 = 1 << 0;
    pub const RIGHT_TRANSFER: u64 = 1 << 1;
    pub const RIGHT_SEND: u64 = 1 << 2;
    pub const RIGHT_RECEIVE: u64 = 1 << 3;
    pub const RIGHT_SIGNAL: u64 = 1 << 4;
    pub const RIGHT_WAIT: u64 = 1 << 5;
    pub const RIGHT_READ: u64 = 1 << 6;
    pub const RIGHT_WRITE: u64 = 1 << 7;

    pub const ENDPOINT_RIGHTS: u64 = RIGHT_DUPLICATE | RIGHT_TRANSFER | RIGHT_SEND | RIGHT_RECEIVE;
    pub const NOTIFICATION_RIGHTS: u64 =
        RIGHT_DUPLICATE | RIGHT_TRANSFER | RIGHT_SIGNAL | RIGHT_WAIT;
    pub const SHARED_MEMORY_RIGHTS: u64 =
        RIGHT_DUPLICATE | RIGHT_TRANSFER | RIGHT_READ | RIGHT_WRITE;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Info {
        pub object_id: u64,
        pub kind: u64,
        pub rights: u64,
        /// Object-specific bounded state: queued endpoint messages, pending
        /// notification count, or shared-memory byte length.
        pub size: u64,
    }

    impl Info {
        pub const EMPTY: Self = Self {
            object_id: 0,
            kind: 0,
            rights: 0,
            size: 0,
        };
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MessageInfo {
        pub sender_process_id: u64,
        pub byte_count: u64,
        pub transferred_handle: u64,
        pub transferred_rights: u64,
    }

    impl MessageInfo {
        pub const EMPTY: Self = Self {
            sender_process_id: 0,
            byte_count: 0,
            transferred_handle: INVALID_HANDLE,
            transferred_rights: 0,
        };
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemInfo {
    pub abi_major: u64,
    pub abi_minor: u64,
    pub capabilities: u64,
    pub page_size: u64,
    pub maximum_open_files: u64,
    pub maximum_path_bytes: u64,
    pub maximum_directory_entries: u64,
    pub init_process_id: u64,
}

impl SystemInfo {
    pub const EMPTY: Self = Self {
        abi_major: 0,
        abi_minor: 0,
        capabilities: 0,
        page_size: 0,
        maximum_open_files: 0,
        maximum_path_bytes: 0,
        maximum_directory_entries: 0,
        init_process_id: 0,
    };
}

pub mod file {
    pub const KIND_FILE: u64 = 1;
    pub const KIND_DIRECTORY: u64 = 2;
    pub const KIND_TERMINAL: u64 = 3;
    pub const KIND_PIPE: u64 = 4;

    pub const FLAG_READ_ONLY: u64 = 1 << 0;
    pub const FLAG_HIDDEN: u64 = 1 << 1;
    pub const FLAG_SYSTEM: u64 = 1 << 2;

    /// Directory records keep one trailing byte available for a NUL terminator
    /// when callers choose to present the name as a C-compatible string.
    pub const DIRECTORY_ENTRY_NAME_CAPACITY: usize = 256;
    pub const MAX_DIRECTORY_ENTRY_NAME_BYTES: usize = DIRECTORY_ENTRY_NAME_CAPACITY - 1;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Stat {
        pub kind: u64,
        pub size: u64,
        pub flags: u64,
    }

    impl Stat {
        pub const EMPTY: Self = Self {
            kind: 0,
            size: 0,
            flags: 0,
        };

        pub const fn is_file(self) -> bool {
            self.kind == KIND_FILE
        }

        pub const fn is_directory(self) -> bool {
            self.kind == KIND_DIRECTORY
        }
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DirectoryEntry {
        pub kind: u64,
        pub size: u64,
        pub flags: u64,
        pub name_length: u64,
        pub name: [u8; DIRECTORY_ENTRY_NAME_CAPACITY],
    }

    impl DirectoryEntry {
        pub const EMPTY: Self = Self {
            kind: 0,
            size: 0,
            flags: 0,
            name_length: 0,
            name: [0; DIRECTORY_ENTRY_NAME_CAPACITY],
        };

        pub fn name(&self) -> &[u8] {
            let length = usize::try_from(self.name_length)
                .unwrap_or(MAX_DIRECTORY_ENTRY_NAME_BYTES)
                .min(MAX_DIRECTORY_ENTRY_NAME_BYTES);
            &self.name[..length]
        }

        pub const fn is_file(self) -> bool {
            self.kind == KIND_FILE
        }

        pub const fn is_directory(self) -> bool {
            self.kind == KIND_DIRECTORY
        }
    }
}

pub mod signal {
    pub const INTERRUPT: u64 = 2;
    pub const TERMINATE: u64 = 15;
    pub const CONTINUE: u64 = 18;
    pub const STOP: u64 = 19;
    pub const TERMINAL_STOP: u64 = 20;
    pub const MAX: u64 = 63;

    pub const fn bit(signal: u64) -> u64 {
        if signal == 0 || signal > MAX {
            0
        } else {
            1_u64 << (signal - 1)
        }
    }

    pub const SUPPORTED_MASK: u64 =
        bit(INTERRUPT) | bit(TERMINATE) | bit(CONTINUE) | bit(STOP) | bit(TERMINAL_STOP);
    pub const UNBLOCKABLE_MASK: u64 = bit(STOP);
}

pub mod signal_action {
    pub const DEFAULT: u64 = 0;
    pub const IGNORE: u64 = 1;
    pub const RESET_HANDLER: u64 = 1 << 0;
    pub const ALLOWED_FLAGS: u64 = RESET_HANDLER;
    pub const FRAME_MAGIC: u64 = 0x4e55_4c4c_5354_4152;

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Action {
        pub handler: u64,
        pub mask: u64,
        pub flags: u64,
        pub restorer: u64,
    }

    impl Action {
        pub const DEFAULT: Self = Self {
            handler: DEFAULT,
            mask: 0,
            flags: 0,
            restorer: 0,
        };

        pub const IGNORE: Self = Self {
            handler: IGNORE,
            mask: 0,
            flags: 0,
            restorer: 0,
        };
    }

    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Frame {
        pub return_address: u64,
        pub magic: u64,
        pub signal: u64,
        pub previous_mask: u64,
        pub cookie: u64,
    }
}

pub mod signal_mask {
    pub const BLOCK: u64 = 0;
    pub const UNBLOCK: u64 = 1;
    pub const SET: u64 = 2;
}

pub mod child_status {
    pub const SIGNAL_BASE: u64 = 128;
    pub const STOPPED_BASE: u64 = 0x100;
    pub const CONTINUED: u64 = 0x200;
}

pub mod spawn {
    pub const FOREGROUND: u64 = 1 << 0;
    pub const USE_DESCRIPTORS: u64 = 1 << 1;
    pub const NEW_PROCESS_GROUP: u64 = 1 << 2;
    pub const JOIN_PROCESS_GROUP: u64 = 1 << 3;
    pub const ALLOWED_FLAGS: u64 =
        FOREGROUND | USE_DESCRIPTORS | NEW_PROCESS_GROUP | JOIN_PROCESS_GROUP;

    pub const DEFAULT_DESCRIPTOR: u64 = u64::MAX;
    pub const DEFAULT_PROCESS_GROUP: u64 = u64::MAX;
}

pub mod open {
    pub const READ: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;
    pub const CREATE: u64 = 1 << 2;
    pub const TRUNCATE: u64 = 1 << 3;
    pub const APPEND: u64 = 1 << 4;
    pub const CLOSE_ON_EXEC: u64 = 1 << 5;
    pub const ALLOWED_FLAGS: u64 = READ | WRITE | CREATE | TRUNCATE | APPEND | CLOSE_ON_EXEC;
}

pub mod descriptor {
    pub const CLOSE_ON_EXEC: u64 = 1 << 0;
    pub const ALLOWED_FLAGS: u64 = CLOSE_ON_EXEC;
}

pub mod seek {
    pub const SET: u64 = 0;
    pub const CURRENT: u64 = 1;
    pub const END: u64 = 2;
}

pub mod limits {
    pub const MAX_SYSCALL_WRITE_BYTES: usize = 4096;
    pub const MAX_SYSCALL_READ_BYTES: usize = 4096;
    pub const MAX_OPEN_FILES: usize = 16;
    pub const MAX_ARGUMENTS: usize = 16;
    pub const MAX_ARGUMENT_BYTES: usize = 4096;
    pub const MAX_ENVIRONMENT_VARIABLES: usize = 16;
    pub const MAX_ENVIRONMENT_BYTES: usize = 4096;
    pub const MAX_ENVIRONMENT_NAME_BYTES: usize = 64;
    pub const MAX_COMMAND_BYTES: usize = 512;
    pub const MAX_PATH_BYTES: usize = 4096;
    pub const MAX_DIRECTORY_ENTRIES_PER_CALL: usize = 32;

    pub const MAX_CAPABILITIES_PER_PROCESS: usize = 64;
    pub const MAX_ENDPOINT_OBJECTS: usize = 32;
    pub const MAX_NOTIFICATION_OBJECTS: usize = 32;
    pub const MAX_SHARED_MEMORY_OBJECTS: usize = 16;
    pub const MAX_ENDPOINT_MESSAGES: usize = 8;
    pub const MAX_IPC_MESSAGE_BYTES: usize = 256;
    pub const MAX_SHARED_MEMORY_BYTES: usize = 16 * 1024;
    pub const MAX_SHARED_MEMORY_TOTAL_BYTES: usize = 256 * 1024;
}

pub mod errno {
    pub const NO_ENTRY: i64 = -2;
    pub const NO_PROCESS: i64 = -3;
    pub const INTERRUPTED: i64 = -4;
    pub const IO: i64 = -5;
    pub const ARGUMENT_TOO_LARGE: i64 = -7;
    pub const BAD_FILE_DESCRIPTOR: i64 = -9;
    pub const NO_CHILD: i64 = -10;
    pub const TRY_AGAIN: i64 = -11;
    pub const PERMISSION: i64 = -13;
    pub const BAD_ADDRESS: i64 = -14;
    pub const NOT_DIRECTORY: i64 = -20;
    pub const IS_DIRECTORY: i64 = -21;
    pub const INVALID_ARGUMENT: i64 = -22;
    pub const TOO_MANY_OPEN_FILES: i64 = -24;
    pub const NO_SPACE: i64 = -28;
    pub const READ_ONLY: i64 = -30;
    pub const BROKEN_PIPE: i64 = -32;
    pub const RANGE: i64 = -34;
    pub const NAME_TOO_LONG: i64 = -36;
    pub const NOT_IMPLEMENTED: i64 = -38;
}
