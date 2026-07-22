//! Safe wrappers for the documented NullStar OS userspace platform ABI.

use core::{arch::asm, mem::size_of};

use crate::abi::{self, syscall};

pub type FileDescriptor = u64;
pub type ProcessId = u64;

pub use crate::abi::{
    SystemInfo,
    file::{DirectoryEntry, Stat},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Errno(i32);

impl Errno {
    pub const NO_ENTRY: Self = Self((-abi::errno::NO_ENTRY) as i32);
    pub const NO_PROCESS: Self = Self((-abi::errno::NO_PROCESS) as i32);
    pub const PERMISSION: Self = Self((-abi::errno::PERMISSION) as i32);
    pub const BAD_FILE_DESCRIPTOR: Self = Self((-abi::errno::BAD_FILE_DESCRIPTOR) as i32);
    pub const BAD_ADDRESS: Self = Self((-abi::errno::BAD_ADDRESS) as i32);
    pub const NOT_DIRECTORY: Self = Self((-abi::errno::NOT_DIRECTORY) as i32);
    pub const INVALID_ARGUMENT: Self = Self((-abi::errno::INVALID_ARGUMENT) as i32);
    pub const TOO_MANY_OPEN_FILES: Self = Self((-abi::errno::TOO_MANY_OPEN_FILES) as i32);
    pub const RANGE: Self = Self((-abi::errno::RANGE) as i32);
    pub const NAME_TOO_LONG: Self = Self((-abi::errno::NAME_TOO_LONG) as i32);
    pub const NOT_IMPLEMENTED: Self = Self((-abi::errno::NOT_IMPLEMENTED) as i32);

    pub const fn code(self) -> i32 {
        self.0
    }
}

pub type Result<T> = core::result::Result<T, Errno>;

fn decode(raw: u64) -> Result<u64> {
    let signed = raw as i64;
    if signed < 0 {
        Err(Errno((-signed) as i32))
    } else {
        Ok(raw)
    }
}

pub fn system_info() -> Result<SystemInfo> {
    let mut info = SystemInfo::EMPTY;
    let mut result = syscall::SYSTEM_INFO;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") core::ptr::from_mut(&mut info) as u64,
            in("rsi") size_of::<SystemInfo>() as u64,
        );
    }
    decode(result).map(|_| info)
}

pub fn stat(path: &[u8]) -> Result<Stat> {
    let mut stat = Stat::EMPTY;
    let mut result = syscall::STAT;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") path.as_ptr() as u64,
            in("rsi") path.len() as u64,
            in("rdx") core::ptr::from_mut(&mut stat) as u64,
            in("r10") size_of::<Stat>() as u64,
        );
    }
    decode(result).map(|_| stat)
}

pub fn fstat(descriptor: FileDescriptor) -> Result<Stat> {
    let mut stat = Stat::EMPTY;
    let mut result = syscall::FSTAT;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") descriptor,
            in("rsi") core::ptr::from_mut(&mut stat) as u64,
            in("rdx") size_of::<Stat>() as u64,
        );
    }
    decode(result).map(|_| stat)
}

pub fn read_directory(
    path: &[u8],
    start_index: usize,
    entries: &mut [DirectoryEntry],
) -> Result<usize> {
    let mut result = syscall::READ_DIRECTORY;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") path.as_ptr() as u64,
            in("rsi") path.len() as u64,
            in("rdx") start_index as u64,
            in("r10") entries.as_mut_ptr() as u64,
            in("r8") entries.len() as u64,
        );
    }
    decode(result).map(|count| count as usize)
}

pub fn chdir(path: &[u8]) -> Result<()> {
    let mut result = syscall::CHDIR;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") path.as_ptr() as u64,
            in("rsi") path.len() as u64,
        );
    }
    decode(result).map(|_| ())
}

pub fn getcwd(buffer: &mut [u8]) -> Result<&[u8]> {
    let mut result = syscall::GETCWD;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") buffer.as_mut_ptr() as u64,
            in("rsi") buffer.len() as u64,
        );
    }
    let length = decode(result)? as usize;
    buffer.get(..length).ok_or(Errno::RANGE)
}

pub fn dup(descriptor: FileDescriptor) -> Result<FileDescriptor> {
    let mut result = syscall::DUP;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") descriptor,
        );
    }
    decode(result)
}

pub fn dup2(
    descriptor: FileDescriptor,
    target_descriptor: FileDescriptor,
) -> Result<FileDescriptor> {
    let mut result = syscall::DUP2;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") descriptor,
            in("rsi") target_descriptor,
        );
    }
    decode(result)
}

pub fn getppid() -> Result<ProcessId> {
    let mut result = syscall::GETPPID;
    unsafe {
        asm!("int 0x80", inlateout("rax") result);
    }
    decode(result)
}

pub fn kill(process_id: ProcessId, signal: u64) -> Result<()> {
    let mut result = syscall::KILL;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") process_id,
            in("rsi") signal,
        );
    }
    decode(result).map(|_| ())
}
