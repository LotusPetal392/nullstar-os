from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    target = Path(path)
    source = target.read_text()
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}: {old[:100]!r}")
    target.write_text(source.replace(old, new, 1))


def append_once(path: str, marker: str, text: str) -> None:
    target = Path(path)
    source = target.read_text()
    if marker in source:
        return
    target.write_text(source.rstrip() + "\n\n" + text.rstrip() + "\n")


append_once(
    "userspace/Cargo.toml",
    'name = "runtime_probe"',
    '''[[bin]]
name = "runtime_probe"
path = "src/bin/runtime_probe.rs"
test = false
bench = false''',
)

replace_once(
    "build.rs",
    '''    let userspace_signal_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_signal_probe")
            .expect("userspace signal-probe artifact path was not set"),
    );
    let userspace_shell = PathBuf::from(''',
    '''    let userspace_signal_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_signal_probe")
            .expect("userspace signal-probe artifact path was not set"),
    );
    let userspace_runtime_probe = PathBuf::from(
        env::var_os("CARGO_BIN_FILE_USERSPACE_runtime_probe")
            .expect("userspace runtime-probe artifact path was not set"),
    );
    let userspace_shell = PathBuf::from(''',
)
replace_once(
    "build.rs",
    '''    image.set_file(String::from("signal-probe"), userspace_signal_probe);
    image.set_file(String::from("ush"), userspace_shell);''',
    '''    image.set_file(String::from("signal-probe"), userspace_signal_probe);
    image.set_file(String::from("runtime-probe"), userspace_runtime_probe);
    image.set_file(String::from("ush"), userspace_shell);''',
)

replace_once(
    "kernel/src/process/userspace.rs",
    '''pub use super::{pipe::Snapshot as PipeSnapshot, terminal::Snapshot as TerminalSnapshot};

pub const SYSCALL_VECTOR: u8 = 0x80;

const PAGE_FAULT_VECTOR: u64 = 14;
const GENERAL_PROTECTION_VECTOR: u64 = 13;
const SYSCALL_WRITE: u64 = 1;
const SYSCALL_YIELD: u64 = 2;
const SYSCALL_EXIT: u64 = 3;
const SYSCALL_OPEN: u64 = 4;
const SYSCALL_READ: u64 = 5;
const SYSCALL_CLOSE: u64 = 6;
const SYSCALL_SPAWN_COMMAND: u64 = 7;
const SYSCALL_WAIT_CHILD: u64 = 8;
const SYSCALL_GETPID: u64 = 9;
const SYSCALL_PIPE_PAIR: u64 = 10;
const SYSCALL_TRY_WAIT_CHILD: u64 = 11;
const SYSCALL_SIGNAL_PROCESS_GROUP: u64 = 12;

pub const SIGNAL_INTERRUPT: u64 = 2;
pub const SIGNAL_TERMINATE: u64 = 15;

const USER_MIN_ADDRESS: u64 = 0x0001_0000;
const USER_PML4_SLOT_END: u64 = 0x0000_0080_0000_0000;
const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;
const USER_STACK_SIZE: usize = 64 * 1024;
const USER_STACK_GUARD_SIZE: usize = Size4KiB::SIZE as usize;
const KERNEL_TRANSITION_STACK_SIZE: usize = 64 * 1024;
const KERNEL_TRANSITION_STACK_WORDS: usize = KERNEL_TRANSITION_STACK_SIZE / size_of::<u128>();
const MAX_SYSCALL_WRITE_BYTES: usize = 4096;
const MAX_SYSCALL_READ_BYTES: usize = 4096;
const MAX_OPEN_FILES: usize = 16;
const MAX_ARGUMENTS: usize = 16;
const MAX_ARGUMENT_BYTES: usize = 4096;
const MAX_COMMAND_BYTES: usize = 512;
const SPAWN_FOREGROUND: u64 = 1;
const SPAWN_USE_DESCRIPTORS: u64 = 1 << 1;
const SPAWN_NEW_PROCESS_GROUP: u64 = 1 << 2;
const SPAWN_JOIN_PROCESS_GROUP: u64 = 1 << 3;
const DEFAULT_DESCRIPTOR: u64 = u64::MAX;
const DEFAULT_PROCESS_GROUP: u64 = u64::MAX;
const SHELL_PROCESS_TASK_NAME: &str = "user-shell-process";
const USER_RFLAGS: u64 = 0x202;
const PAGE_BYTES: u64 = Size4KiB::SIZE;

const ERR_NO_ENTRY: i64 = -2;
const ERR_NO_PROCESS: i64 = -3;
const ERR_IO: i64 = -5;
const ERR_ARGUMENT_TOO_LARGE: i64 = -7;
const ERR_BAD_FILE_DESCRIPTOR: i64 = -9;
const ERR_NO_CHILD: i64 = -10;
const ERR_TRY_AGAIN: i64 = -11;
const ERR_BAD_ADDRESS: i64 = -14;
const ERR_IS_DIRECTORY: i64 = -21;
const ERR_INVALID_ARGUMENT: i64 = -22;
const ERR_TOO_MANY_OPEN_FILES: i64 = -24;
const ERR_BROKEN_PIPE: i64 = -32;
const ERR_NOT_IMPLEMENTED: i64 = -38;''',
    '''pub use super::{pipe::Snapshot as PipeSnapshot, terminal::Snapshot as TerminalSnapshot};

mod abi {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/userspace_abi.rs"
    ));
}

pub const SYSCALL_VECTOR: u8 = abi::SYSCALL_VECTOR;

const PAGE_FAULT_VECTOR: u64 = 14;
const GENERAL_PROTECTION_VECTOR: u64 = 13;
const SYSCALL_WRITE: u64 = abi::syscall::WRITE;
const SYSCALL_YIELD: u64 = abi::syscall::YIELD;
const SYSCALL_EXIT: u64 = abi::syscall::EXIT;
const SYSCALL_OPEN: u64 = abi::syscall::OPEN;
const SYSCALL_READ: u64 = abi::syscall::READ;
const SYSCALL_CLOSE: u64 = abi::syscall::CLOSE;
const SYSCALL_SPAWN_COMMAND: u64 = abi::syscall::SPAWN_COMMAND;
const SYSCALL_WAIT_CHILD: u64 = abi::syscall::WAIT_CHILD;
const SYSCALL_GETPID: u64 = abi::syscall::GETPID;
const SYSCALL_PIPE_PAIR: u64 = abi::syscall::PIPE_PAIR;
const SYSCALL_TRY_WAIT_CHILD: u64 = abi::syscall::TRY_WAIT_CHILD;
const SYSCALL_SIGNAL_PROCESS_GROUP: u64 = abi::syscall::SIGNAL_PROCESS_GROUP;

pub const SIGNAL_INTERRUPT: u64 = abi::signal::INTERRUPT;
pub const SIGNAL_TERMINATE: u64 = abi::signal::TERMINATE;

const USER_MIN_ADDRESS: u64 = 0x0001_0000;
const USER_PML4_SLOT_END: u64 = 0x0000_0080_0000_0000;
const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;
const USER_STACK_SIZE: usize = 64 * 1024;
const USER_STACK_GUARD_SIZE: usize = Size4KiB::SIZE as usize;
const KERNEL_TRANSITION_STACK_SIZE: usize = 64 * 1024;
const KERNEL_TRANSITION_STACK_WORDS: usize = KERNEL_TRANSITION_STACK_SIZE / size_of::<u128>();
const MAX_SYSCALL_WRITE_BYTES: usize = abi::limits::MAX_SYSCALL_WRITE_BYTES;
const MAX_SYSCALL_READ_BYTES: usize = abi::limits::MAX_SYSCALL_READ_BYTES;
const MAX_OPEN_FILES: usize = abi::limits::MAX_OPEN_FILES;
const MAX_ARGUMENTS: usize = abi::limits::MAX_ARGUMENTS;
const MAX_ARGUMENT_BYTES: usize = abi::limits::MAX_ARGUMENT_BYTES;
const MAX_COMMAND_BYTES: usize = abi::limits::MAX_COMMAND_BYTES;
const SPAWN_FOREGROUND: u64 = abi::spawn::FOREGROUND;
const SPAWN_USE_DESCRIPTORS: u64 = abi::spawn::USE_DESCRIPTORS;
const SPAWN_NEW_PROCESS_GROUP: u64 = abi::spawn::NEW_PROCESS_GROUP;
const SPAWN_JOIN_PROCESS_GROUP: u64 = abi::spawn::JOIN_PROCESS_GROUP;
const DEFAULT_DESCRIPTOR: u64 = abi::spawn::DEFAULT_DESCRIPTOR;
const DEFAULT_PROCESS_GROUP: u64 = abi::spawn::DEFAULT_PROCESS_GROUP;
const SHELL_PROCESS_TASK_NAME: &str = "user-shell-process";
const USER_RFLAGS: u64 = 0x202;
const PAGE_BYTES: u64 = Size4KiB::SIZE;

const ERR_NO_ENTRY: i64 = abi::errno::NO_ENTRY;
const ERR_NO_PROCESS: i64 = abi::errno::NO_PROCESS;
const ERR_IO: i64 = abi::errno::IO;
const ERR_ARGUMENT_TOO_LARGE: i64 = abi::errno::ARGUMENT_TOO_LARGE;
const ERR_BAD_FILE_DESCRIPTOR: i64 = abi::errno::BAD_FILE_DESCRIPTOR;
const ERR_NO_CHILD: i64 = abi::errno::NO_CHILD;
const ERR_TRY_AGAIN: i64 = abi::errno::TRY_AGAIN;
const ERR_BAD_ADDRESS: i64 = abi::errno::BAD_ADDRESS;
const ERR_IS_DIRECTORY: i64 = abi::errno::IS_DIRECTORY;
const ERR_INVALID_ARGUMENT: i64 = abi::errno::INVALID_ARGUMENT;
const ERR_TOO_MANY_OPEN_FILES: i64 = abi::errno::TOO_MANY_OPEN_FILES;
const ERR_BROKEN_PIPE: i64 = abi::errno::BROKEN_PIPE;
const ERR_NOT_IMPLEMENTED: i64 = abi::errno::NOT_IMPLEMENTED;''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''    let allowed_flags = SPAWN_FOREGROUND
        | SPAWN_USE_DESCRIPTORS
        | SPAWN_NEW_PROCESS_GROUP
        | SPAWN_JOIN_PROCESS_GROUP;''',
    '''    let allowed_flags = abi::spawn::ALLOWED_FLAGS;''',
)

replace_once(
    "kernel/src/main.rs",
    '''    serial_println!(
        "userspace file I/O verified: pid={}, path={}, opens={}, reads={}, closes={}, bytes_read={}, exit_code=0",
        cat_result.process_id,
        cat_result.path,
        cat_result.open_count,
        cat_result.read_count,
        cat_result.close_count,
        cat_result.bytes_read
    );

    const TERMINAL_TEST_LINE: &str = "hello from canonical stdin";''',
    '''    serial_println!(
        "userspace file I/O verified: pid={}, path={}, opens={}, reads={}, closes={}, bytes_read={}, exit_code=0",
        cat_result.process_id,
        cat_result.path,
        cat_result.open_count,
        cat_result.read_count,
        cat_result.close_count,
        cat_result.bytes_read
    );

    const USERSPACE_RUNTIME_PROBE_BYTES: u64 =
        b"userspace Rust runtime probe passed\\n".len() as u64;
    let runtime_probe_result = match userspace_runtime.run("/runtime-probe", &["runtime-smoke"]) {
        Ok(result) => result,
        Err(error) => {
            serial_println!("userspace Rust runtime validation failed to launch: {error}");
            hlt_loop();
        }
    };
    let userspace_rust_runtime_verified = runtime_probe_result.exit_code() == Some(0)
        && runtime_probe_result.path == "/runtime-probe"
        && runtime_probe_result.syscall_count == 3
        && runtime_probe_result.write_count == 1
        && runtime_probe_result.bytes_written == USERSPACE_RUNTIME_PROBE_BYTES;
    if !userspace_rust_runtime_verified {
        serial_println!(
            "userspace Rust runtime verification failed: exit={:?}, path={}, syscalls={}, writes={}, bytes={}",
            runtime_probe_result.exit_code(),
            runtime_probe_result.path,
            runtime_probe_result.syscall_count,
            runtime_probe_result.write_count,
            runtime_probe_result.bytes_written
        );
        hlt_loop();
    }
    serial_println!(
        "userspace Rust runtime verified: pid={}, syscalls={}, writes={}, bytes={}, heap=4096, argv=2",
        runtime_probe_result.process_id,
        runtime_probe_result.syscall_count,
        runtime_probe_result.write_count,
        runtime_probe_result.bytes_written
    );

    const TERMINAL_TEST_LINE: &str = "hello from canonical stdin";''',
)
replace_once(
    "kernel/src/main.rs",
    '''    if file_io_verified {
        println!("Userspace file descriptors verified");
    } else {
        println!("Userspace file descriptors unavailable");
    }
    if terminal_verified {''',
    '''    if file_io_verified {
        println!("Userspace file descriptors verified");
    } else {
        println!("Userspace file descriptors unavailable");
    }
    if userspace_rust_runtime_verified {
        println!("Shared Rust userspace runtime verified");
    } else {
        println!("Rust userspace runtime unavailable");
    }
    if terminal_verified {''',
)

replace_once(
    "kernel/src/shell.rs",
    '    shell_println!("  ush              launch the userspace shell with `|` pipelines");',
    '    shell_println!("  ush              launch the Rust userspace shell and job controller");',
)

replace_once(
    "src/main.rs",
    '''const USERSPACE_TEST_MARKER: &str = "process isolation verified:";
const USER_FILE_IO_TEST_MARKER: &str = "userspace file I/O verified:";
const USER_TERMINAL_TEST_MARKER: &str = "userspace terminal verified:";''',
    '''const USERSPACE_TEST_MARKER: &str = "process isolation verified:";
const USER_FILE_IO_TEST_MARKER: &str = "userspace file I/O verified:";
const USER_RUST_RUNTIME_TEST_MARKER: &str = "userspace Rust runtime verified:";
const USER_TERMINAL_TEST_MARKER: &str = "userspace terminal verified:";''',
)
replace_once(
    "src/main.rs",
    '''        "  --test      Verify hardware, storage, VFS, process control, pipelines, background jobs, and signals"''',
    '''        "  --test      Verify hardware, storage, VFS, the Rust userspace runtime, process control, pipelines, jobs, and signals"''',
)
replace_once(
    "src/main.rs",
    '''        let mut userspace_ready = false;
        let mut user_file_io_ready = false;
        let mut user_terminal_ready = false;''',
    '''        let mut userspace_ready = false;
        let mut user_file_io_ready = false;
        let mut user_rust_runtime_ready = false;
        let mut user_terminal_ready = false;''',
)
replace_once(
    "src/main.rs",
    '''            userspace_ready |= line.contains(USERSPACE_TEST_MARKER);
            user_file_io_ready |= line.contains(USER_FILE_IO_TEST_MARKER);
            user_terminal_ready |= line.contains(USER_TERMINAL_TEST_MARKER);''',
    '''            userspace_ready |= line.contains(USERSPACE_TEST_MARKER);
            user_file_io_ready |= line.contains(USER_FILE_IO_TEST_MARKER);
            user_rust_runtime_ready |= line.contains(USER_RUST_RUNTIME_TEST_MARKER);
            user_terminal_ready |= line.contains(USER_TERMINAL_TEST_MARKER);''',
)
replace_once(
    "src/main.rs",
    '''                && userspace_ready
                && user_file_io_ready
                && user_terminal_ready''',
    '''                && userspace_ready
                && user_file_io_ready
                && user_rust_runtime_ready
                && user_terminal_ready''',
)
