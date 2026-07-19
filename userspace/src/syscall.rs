use core::{
    arch::{asm, global_asm},
    ops::{BitOr, BitOrAssign},
};

use crate::abi::{
    child_status, descriptor as abi_descriptor, errno as abi_errno, open as abi_open,
    seek as abi_seek, signal, spawn, syscall,
};

global_asm!(
    r#"
    .section .text
    .p2align 4
    .global galactic_userspace_spawn_command
    .type galactic_userspace_spawn_command,@function
galactic_userspace_spawn_command:
    push rbx
    mov r10, rcx
    mov rbx, qword ptr [rsp + 16]
    mov rax, {spawn_command}
    int 0x80
    pop rbx
    ret
.size galactic_userspace_spawn_command, .-galactic_userspace_spawn_command
"#,
    spawn_command = const syscall::SPAWN_COMMAND,
);

unsafe extern "C" {
    fn galactic_userspace_spawn_command(
        command_address: u64,
        command_length: u64,
        flags: u64,
        stdin_descriptor: u64,
        stdout_descriptor: u64,
        stderr_descriptor: u64,
        process_group: u64,
    ) -> u64;
}

pub type FileDescriptor = u64;
pub type ProcessId = u64;
pub type ProcessGroupId = u64;

pub const STDIN: FileDescriptor = 0;
pub const STDOUT: FileDescriptor = 1;
pub const STDERR: FileDescriptor = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Errno(i32);

impl Errno {
    pub const NO_ENTRY: Self = Self((-abi_errno::NO_ENTRY) as i32);
    pub const IO: Self = Self((-abi_errno::IO) as i32);
    pub const BAD_FILE_DESCRIPTOR: Self = Self((-abi_errno::BAD_FILE_DESCRIPTOR) as i32);
    pub const NO_CHILD: Self = Self((-abi_errno::NO_CHILD) as i32);
    pub const TRY_AGAIN: Self = Self((-abi_errno::TRY_AGAIN) as i32);
    pub const INVALID_ARGUMENT: Self = Self((-abi_errno::INVALID_ARGUMENT) as i32);

    pub const fn code(self) -> i32 {
        self.0
    }
}

pub type Result<T> = core::result::Result<T, Errno>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnFlags(u64);

impl SpawnFlags {
    pub const EMPTY: Self = Self(0);
    pub const FOREGROUND: Self = Self(spawn::FOREGROUND);
    pub const USE_DESCRIPTORS: Self = Self(spawn::USE_DESCRIPTORS);
    pub const NEW_PROCESS_GROUP: Self = Self(spawn::NEW_PROCESS_GROUP);
    pub const JOIN_PROCESS_GROUP: Self = Self(spawn::JOIN_PROCESS_GROUP);

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for SpawnFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for SpawnFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags(u64);

impl OpenFlags {
    pub const READ: Self = Self(abi_open::READ);
    pub const WRITE: Self = Self(abi_open::WRITE);
    pub const CREATE: Self = Self(abi_open::CREATE);
    pub const TRUNCATE: Self = Self(abi_open::TRUNCATE);
    pub const APPEND: Self = Self(abi_open::APPEND);
    pub const CLOSE_ON_EXEC: Self = Self(abi_open::CLOSE_ON_EXEC);
    pub const fn bits(self) -> u64 {
        self.0
    }
}
impl BitOr for OpenFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}
impl BitOrAssign for OpenFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DescriptorFlags(u64);

impl DescriptorFlags {
    pub const EMPTY: Self = Self(0);
    pub const CLOSE_ON_EXEC: Self = Self(abi_descriptor::CLOSE_ON_EXEC);

    pub const fn bits(self) -> u64 {
        self.0
    }
}

impl BitOr for DescriptorFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for DescriptorFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekFrom {
    Start(u64),
    Current(i64),
    End(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipePair {
    pub reader: FileDescriptor,
    pub writer: FileDescriptor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildStatus(u64);

impl ChildStatus {
    pub const fn raw(self) -> u64 {
        self.0
    }

    pub const fn success(self) -> bool {
        self.0 == 0
    }

    pub const fn signal(self) -> Option<u64> {
        let number = self.0.saturating_sub(child_status::SIGNAL_BASE);
        if self.0 >= child_status::SIGNAL_BASE
            && self.0 < child_status::STOPPED_BASE
            && matches!(
                number,
                signal::INTERRUPT
                    | signal::TERMINATE
                    | signal::CONTINUE
                    | signal::STOP
                    | signal::TERMINAL_STOP
            )
        {
            Some(number)
        } else {
            None
        }
    }

    pub const fn stopped_signal(self) -> Option<u64> {
        let number = self.0.saturating_sub(child_status::STOPPED_BASE);
        if self.0 >= child_status::STOPPED_BASE
            && self.0 < child_status::CONTINUED
            && matches!(number, signal::STOP | signal::TERMINAL_STOP)
        {
            Some(number)
        } else {
            None
        }
    }

    pub const fn continued(self) -> bool {
        self.0 == child_status::CONTINUED
    }

    pub fn interrupted(self) -> bool {
        self.signal() == Some(signal::INTERRUPT)
    }
}

fn decode(raw: u64) -> Result<u64> {
    let signed = raw as i64;
    if signed < 0 {
        Err(Errno((-signed) as i32))
    } else {
        Ok(raw)
    }
}

pub fn write(descriptor: FileDescriptor, bytes: &[u8]) -> Result<usize> {
    let mut result = syscall::WRITE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") descriptor,
            in("rsi") bytes.as_ptr() as u64,
            in("rdx") bytes.len() as u64,
        );
    }
    decode(result).map(|count| count as usize)
}

pub fn write_all(descriptor: FileDescriptor, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let written = write(descriptor, bytes)?;
        if written == 0 {
            return Err(Errno::IO);
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

pub fn read(descriptor: FileDescriptor, buffer: &mut [u8]) -> Result<usize> {
    let mut result = syscall::READ;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") descriptor,
            in("rsi") buffer.as_mut_ptr() as u64,
            in("rdx") buffer.len() as u64,
        );
    }
    decode(result).map(|count| count as usize)
}

pub fn open(path: &[u8], flags: OpenFlags) -> Result<FileDescriptor> {
    let mut result = syscall::OPEN;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") path.as_ptr() as u64,
            in("rsi") path.len() as u64,
            in("rdx") flags.bits(),
        );
    }
    decode(result)
}

pub fn close(descriptor: FileDescriptor) -> Result<()> {
    let mut result = syscall::CLOSE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") descriptor,
        );
    }
    decode(result).map(|_| ())
}

pub fn yield_now() -> Result<()> {
    let mut result = syscall::YIELD;
    unsafe {
        asm!("int 0x80", inlateout("rax") result);
    }
    decode(result).map(|_| ())
}

pub fn getpid() -> Result<ProcessId> {
    let mut result = syscall::GETPID;
    unsafe {
        asm!("int 0x80", inlateout("rax") result);
    }
    decode(result)
}

pub fn spawn_command(
    command: &[u8],
    flags: SpawnFlags,
    stdin_descriptor: Option<FileDescriptor>,
    stdout_descriptor: Option<FileDescriptor>,
    stderr_descriptor: Option<FileDescriptor>,
    process_group: Option<ProcessGroupId>,
) -> Result<ProcessId> {
    let raw = unsafe {
        galactic_userspace_spawn_command(
            command.as_ptr() as u64,
            command.len() as u64,
            flags.bits(),
            stdin_descriptor.unwrap_or(spawn::DEFAULT_DESCRIPTOR),
            stdout_descriptor.unwrap_or(spawn::DEFAULT_DESCRIPTOR),
            stderr_descriptor.unwrap_or(spawn::DEFAULT_DESCRIPTOR),
            process_group.unwrap_or(spawn::DEFAULT_PROCESS_GROUP),
        )
    };
    decode(raw)
}

pub fn wait_child(process_id: ProcessId) -> Result<ChildStatus> {
    let mut result = syscall::WAIT_CHILD;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") process_id,
        );
    }
    decode(result).map(ChildStatus)
}

pub fn try_wait_child(process_id: ProcessId) -> Result<ChildStatus> {
    let mut result = syscall::TRY_WAIT_CHILD;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") process_id,
        );
    }
    decode(result).map(ChildStatus)
}

pub fn pipe_pair() -> Result<PipePair> {
    let mut result = syscall::PIPE_PAIR;
    unsafe {
        asm!("int 0x80", inlateout("rax") result);
    }
    decode(result).map(|packed| PipePair {
        reader: packed & u64::from(u32::MAX),
        writer: packed >> 32,
    })
}

pub fn signal_process_group(process_group: ProcessGroupId, signal: u64) -> Result<usize> {
    let mut result = syscall::SIGNAL_PROCESS_GROUP;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") process_group,
            in("rsi") signal,
        );
    }
    decode(result).map(|count| count as usize)
}

pub fn foreground_process_group(process_group: ProcessGroupId) -> Result<usize> {
    let mut result = syscall::FOREGROUND_PROCESS_GROUP;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") process_group,
        );
    }
    decode(result).map(|count| count as usize)
}

pub fn seek(descriptor: FileDescriptor, offset: SeekFrom) -> Result<u64> {
    let (offset, whence) = match offset {
        SeekFrom::Start(offset) => (offset as i64, abi_seek::SET),
        SeekFrom::Current(offset) => (offset, abi_seek::CURRENT),
        SeekFrom::End(offset) => (offset, abi_seek::END),
    };
    let mut result = syscall::SEEK;
    unsafe {
        asm!("int 0x80", inlateout("rax") result, in("rdi") descriptor, in("rsi") offset as u64, in("rdx") whence);
    }
    decode(result)
}

pub fn execve(command: &[u8]) -> Result<()> {
    let mut result = syscall::EXECVE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") command.as_ptr() as u64,
            in("rsi") command.len() as u64,
        );
    }
    decode(result).map(|_| ())
}

pub fn set_descriptor_flags(descriptor: FileDescriptor, flags: DescriptorFlags) -> Result<()> {
    let mut result = syscall::SET_DESCRIPTOR_FLAGS;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") descriptor,
            in("rsi") flags.bits(),
        );
    }
    decode(result).map(|_| ())
}

pub fn exit(code: u64) -> ! {
    unsafe {
        asm!(
            "int 0x80",
            in("rax") syscall::EXIT,
            in("rdi") code,
            options(noreturn),
        );
    }
}
