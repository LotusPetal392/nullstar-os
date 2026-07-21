// Shared GalacticOS ring-3 ABI constants. This file is included by both the
// kernel process manager and the userspace runtime so numeric definitions stay
// in one place without introducing a runtime dependency.

/// Software interrupt vector used by the GalacticOS userspace ABI.
pub const SYSCALL_VECTOR: u8 = 0x80;

pub mod syscall {
    pub const WRITE: u64 = 1;
    pub const YIELD: u64 = 2;
    pub const EXIT: u64 = 3;
    pub const OPEN: u64 = 4;
    pub const READ: u64 = 5;
    pub const CLOSE: u64 = 6;
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

    pub const SUPPORTED_MASK: u64 = bit(INTERRUPT)
        | bit(TERMINATE)
        | bit(CONTINUE)
        | bit(STOP)
        | bit(TERMINAL_STOP);
    pub const UNBLOCKABLE_MASK: u64 = bit(STOP);
}

pub mod signal_action {
    pub const DEFAULT: u64 = 0;
    pub const IGNORE: u64 = 1;
    pub const RESET_HANDLER: u64 = 1 << 0;
    pub const ALLOWED_FLAGS: u64 = RESET_HANDLER;
    pub const FRAME_MAGIC: u64 = 0x4741_4c41_4354_5347;

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
    pub const ALLOWED_FLAGS: u64 =
        READ | WRITE | CREATE | TRUNCATE | APPEND | CLOSE_ON_EXEC;
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
}

pub mod errno {
    pub const NO_ENTRY: i64 = -2;
    pub const INTERRUPTED: i64 = -4;
    pub const NO_PROCESS: i64 = -3;
    pub const IO: i64 = -5;
    pub const ARGUMENT_TOO_LARGE: i64 = -7;
    pub const BAD_FILE_DESCRIPTOR: i64 = -9;
    pub const NO_CHILD: i64 = -10;
    pub const TRY_AGAIN: i64 = -11;
    pub const BAD_ADDRESS: i64 = -14;
    pub const IS_DIRECTORY: i64 = -21;
    pub const INVALID_ARGUMENT: i64 = -22;
    pub const TOO_MANY_OPEN_FILES: i64 = -24;
    pub const NO_SPACE: i64 = -28;
    pub const READ_ONLY: i64 = -30;
    pub const BROKEN_PIPE: i64 = -32;
    pub const NOT_IMPLEMENTED: i64 = -38;
}
