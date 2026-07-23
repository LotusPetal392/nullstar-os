use core::{
    arch::{asm, global_asm},
    ops::{BitOr, BitOrAssign},
};

use crate::abi::{
    child_status, descriptor as abi_descriptor, errno as abi_errno, open as abi_open,
    seek as abi_seek, signal, signal_action as abi_signal_action, signal_mask as abi_signal_mask,
    spawn, syscall,
};

global_asm!(
    r#"
    .section .text
    .p2align 4
    .global galactic_userspace_signal_restorer
    .type galactic_userspace_signal_restorer,@function
galactic_userspace_signal_restorer:
    mov rax, {signal_return}
    int 0x80
    ud2
.size galactic_userspace_signal_restorer, .-galactic_userspace_signal_restorer
"#,
    signal_return = const syscall::SIGNAL_RETURN,
);

unsafe extern "C" {
    fn galactic_userspace_signal_restorer();
}

pub type FileDescriptor = u64;
pub type ProcessId = u64;
pub type ProcessGroupId = u64;
pub type SignalHandler = extern "C" fn(u64, *const SignalFrame);
pub use crate::abi::signal_action::Frame as SignalFrame;

pub const STDIN: FileDescriptor = 0;
pub const STDOUT: FileDescriptor = 1;
pub const STDERR: FileDescriptor = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Errno(i32);

impl Errno {
    pub const NO_ENTRY: Self = Self((-abi_errno::NO_ENTRY) as i32);
    pub const INTERRUPTED: Self = Self((-abi_errno::INTERRUPTED) as i32);
    pub const IO: Self = Self((-abi_errno::IO) as i32);
    pub const BAD_FILE_DESCRIPTOR: Self = Self((-abi_errno::BAD_FILE_DESCRIPTOR) as i32);
    pub const NO_CHILD: Self = Self((-abi_errno::NO_CHILD) as i32);
    pub const TRY_AGAIN: Self = Self((-abi_errno::TRY_AGAIN) as i32);
    pub const INVALID_ARGUMENT: Self = Self((-abi_errno::INVALID_ARGUMENT) as i32);
    pub const ARGUMENT_TOO_LARGE: Self = Self((-abi_errno::ARGUMENT_TOO_LARGE) as i32);
    pub const NO_SPACE: Self = Self((-abi_errno::NO_SPACE) as i32);

    pub const fn code(self) -> i32 {
        self.0
    }
}

pub type Result<T> = core::result::Result<T, Errno>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalMask(u64);

impl SignalMask {
    pub const EMPTY: Self = Self(0);

    pub const fn from_signal(signal_number: u64) -> Self {
        Self(signal::bit(signal_number))
    }

    pub const fn from_bits(bits: u64) -> Option<Self> {
        if bits & !signal::SUPPORTED_MASK == 0 {
            Some(Self(bits & !signal::UNBLOCKABLE_MASK))
        } else {
            None
        }
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn contains(self, signal_number: u64) -> bool {
        self.0 & signal::bit(signal_number) != 0
    }
}

impl BitOr for SignalMask {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for SignalMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalActionFlags(u64);

impl SignalActionFlags {
    pub const EMPTY: Self = Self(0);
    pub const RESET_HANDLER: Self = Self(abi_signal_action::RESET_HANDLER);

    pub const fn bits(self) -> u64 {
        self.0
    }
}

impl BitOr for SignalActionFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for SignalActionFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalAction(abi_signal_action::Action);

impl SignalAction {
    pub const DEFAULT: Self = Self(abi_signal_action::Action::DEFAULT);
    pub const IGNORE: Self = Self(abi_signal_action::Action::IGNORE);

    pub fn handler(handler: SignalHandler, mask: SignalMask, flags: SignalActionFlags) -> Self {
        Self(abi_signal_action::Action {
            handler: handler as usize as u64,
            mask: mask.bits(),
            flags: flags.bits(),
            restorer: galactic_userspace_signal_restorer as *const () as usize as u64,
        })
    }

    pub const fn handler_address(self) -> u64 {
        self.0.handler
    }

    pub const fn mask(self) -> SignalMask {
        SignalMask(self.0.mask)
    }

    pub const fn flags(self) -> SignalActionFlags {
        SignalActionFlags(self.0.flags)
    }

    pub const fn is_default(self) -> bool {
        self.0.handler == abi_signal_action::DEFAULT
    }

    pub const fn is_ignored(self) -> bool {
        self.0.handler == abi_signal_action::IGNORE
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalMaskHow {
    Block,
    Unblock,
    Set,
}

impl SignalMaskHow {
    const fn raw(self) -> u64 {
        match self {
            Self::Block => abi_signal_mask::BLOCK,
            Self::Unblock => abi_signal_mask::UNBLOCK,
            Self::Set => abi_signal_mask::SET,
        }
    }
}

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

pub fn fork() -> Result<ProcessId> {
    let mut result = syscall::FORK;
    unsafe {
        asm!("int 0x80", inlateout("rax") result);
    }
    decode(result)
}

pub fn signal_action(
    signal_number: u64,
    action: Option<&SignalAction>,
    previous: Option<&mut SignalAction>,
) -> Result<()> {
    let mut result = syscall::SIGNAL_ACTION;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") signal_number,
            in("rsi") action.map_or(0, |action| core::ptr::from_ref(action) as u64),
            in("rdx") previous.map_or(0, |action| core::ptr::from_mut(action) as u64),
        );
    }
    decode(result).map(|_| ())
}

pub fn query_signal_action(signal_number: u64) -> Result<SignalAction> {
    let mut action = SignalAction::DEFAULT;
    signal_action(signal_number, None, Some(&mut action))?;
    Ok(action)
}

pub fn signal_mask(how: SignalMaskHow, mask: SignalMask) -> Result<SignalMask> {
    let mut result = syscall::SIGNAL_MASK;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") how.raw(),
            in("rsi") mask.bits(),
        );
    }
    decode(result).map(SignalMask)
}

pub fn current_signal_mask() -> Result<SignalMask> {
    signal_mask(SignalMaskHow::Block, SignalMask::EMPTY)
}

pub fn environment_set(name: &[u8], value: &[u8]) -> Result<()> {
    let mut result = syscall::ENVIRONMENT_SET;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") name.as_ptr() as u64,
            in("rsi") name.len() as u64,
            in("rdx") value.as_ptr() as u64,
            in("r10") value.len() as u64,
        );
    }
    decode(result).map(|_| ())
}

pub fn environment_unset(name: &[u8]) -> Result<()> {
    let mut result = syscall::ENVIRONMENT_UNSET;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") name.as_ptr() as u64,
            in("rsi") name.len() as u64,
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
