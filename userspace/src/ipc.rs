//! Capability handles and bounded phase-one IPC primitives.
//!
//! Kernel operations are non-blocking. `receive`, `notification_wait`, and
//! `wait_for_handle` add cooperative blocking facades by yielding whenever the
//! requested state is not ready.

use core::{
    arch::asm,
    mem::size_of,
    ops::{BitOr, BitOrAssign},
};

use crate::abi::{
    capability as abi_capability, errno as abi_errno, object_signal as abi_object_signal, syscall,
};

mod phase1_protection_abi {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/protection_abi.rs"
    ));
}

pub type CapabilityHandle = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(i32);

impl Error {
    pub const NO_ENTRY: Self = Self((-abi_errno::NO_ENTRY) as i32);
    pub const NO_PROCESS: Self = Self((-abi_errno::NO_PROCESS) as i32);
    pub const IO: Self = Self((-abi_errno::IO) as i32);
    pub const BAD_FILE_DESCRIPTOR: Self = Self((-abi_errno::BAD_FILE_DESCRIPTOR) as i32);
    pub const NO_CHILD: Self = Self((-abi_errno::NO_CHILD) as i32);
    pub const TRY_AGAIN: Self = Self((-abi_errno::TRY_AGAIN) as i32);
    pub const PERMISSION: Self = Self((-abi_errno::PERMISSION) as i32);
    pub const BAD_ADDRESS: Self = Self((-abi_errno::BAD_ADDRESS) as i32);
    pub const INVALID_ARGUMENT: Self = Self((-abi_errno::INVALID_ARGUMENT) as i32);
    pub const NO_SPACE: Self = Self((-abi_errno::NO_SPACE) as i32);
    pub const RANGE: Self = Self((-abi_errno::RANGE) as i32);
    pub const NOT_IMPLEMENTED: Self = Self((-abi_errno::NOT_IMPLEMENTED) as i32);
    pub const TIMED_OUT: Self = Self((-abi_errno::TIMED_OUT) as i32);

    pub const fn code(self) -> i32 {
        self.0
    }
}

pub type Result<T> = core::result::Result<T, Error>;

fn decode(raw: u64) -> Result<u64> {
    let signed = raw as i64;
    if signed < 0 {
        Err(Error((-signed) as i32))
    } else {
        Ok(raw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights(u64);

impl Rights {
    pub const EMPTY: Self = Self(0);
    pub const DUPLICATE: Self = Self(abi_capability::RIGHT_DUPLICATE);
    pub const TRANSFER: Self = Self(abi_capability::RIGHT_TRANSFER);
    pub const SEND: Self = Self(abi_capability::RIGHT_SEND);
    pub const RECEIVE: Self = Self(abi_capability::RIGHT_RECEIVE);
    pub const SIGNAL: Self = Self(abi_capability::RIGHT_SIGNAL);
    pub const WAIT: Self = Self(abi_capability::RIGHT_WAIT);
    pub const READ: Self = Self(abi_capability::RIGHT_READ);
    pub const WRITE: Self = Self(abi_capability::RIGHT_WRITE);
    pub const MANAGE: Self = Self(abi_capability::RIGHT_MANAGE);

    pub const ENDPOINT: Self = Self(abi_capability::ENDPOINT_RIGHTS);
    pub const NOTIFICATION: Self = Self(abi_capability::NOTIFICATION_RIGHTS);
    pub const SHARED_MEMORY: Self = Self(abi_capability::SHARED_MEMORY_RIGHTS);
    pub const KERNEL_EARLY_LOG_READER: Self = Self(abi_capability::KERNEL_EARLY_LOG_READER_RIGHTS);
    pub const JOB: Self = Self(abi_capability::JOB_RIGHTS);

    pub const fn from_bits(bits: u64) -> Option<Self> {
        let all = abi_capability::ENDPOINT_RIGHTS
            | abi_capability::NOTIFICATION_RIGHTS
            | abi_capability::SHARED_MEMORY_RIGHTS
            | abi_capability::JOB_RIGHTS;
        if bits & !all == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Rights {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Rights {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signals(u64);

impl Signals {
    pub const EMPTY: Self = Self(0);
    pub const READABLE: Self = Self(abi_object_signal::READABLE);
    pub const WRITABLE: Self = Self(abi_object_signal::WRITABLE);
    pub const PEER_CLOSED: Self = Self(abi_object_signal::PEER_CLOSED);
    pub const SIGNALED: Self = Self(abi_object_signal::SIGNALED);
    pub const TERMINATED: Self = Self(abi_object_signal::TERMINATED);
    pub const TIMER_FIRED: Self = Self(abi_object_signal::TIMER_FIRED);

    pub const fn from_bits(bits: u64) -> Option<Self> {
        if bits & !abi_object_signal::ALL == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Signals {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Deadline(u64);

impl Deadline {
    pub const IMMEDIATE: Self = Self(crate::abi::deadline::IMMEDIATE);
    pub const INFINITE: Self = Self(crate::abi::deadline::INFINITE);

    pub const fn from_monotonic_ns(nanoseconds: u64) -> Self {
        Self(nanoseconds)
    }

    pub const fn as_monotonic_ns(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Endpoint,
    Notification,
    SharedMemory,
    KernelEarlyLogReader,
    Job,
}

impl ObjectKind {
    const fn from_raw(raw: u64) -> Option<Self> {
        match raw {
            abi_capability::KIND_ENDPOINT => Some(Self::Endpoint),
            abi_capability::KIND_NOTIFICATION => Some(Self::Notification),
            abi_capability::KIND_SHARED_MEMORY => Some(Self::SharedMemory),
            abi_capability::KIND_KERNEL_EARLY_LOG_READER => Some(Self::KernelEarlyLogReader),
            abi_capability::KIND_JOB => Some(Self::Job),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityInfo {
    pub object_id: u64,
    pub kind: ObjectKind,
    pub rights: Rights,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transfer {
    pub handle: CapabilityHandle,
    pub rights: Rights,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceivedCapability {
    pub handle: CapabilityHandle,
    pub rights: Rights,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceivedMessage {
    pub sender_process_id: u64,
    pub bytes: usize,
    pub capability: Option<ReceivedCapability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobExit {
    pub process_id: u64,
    pub status: crate::syscall::ChildStatus,
}

pub fn duplicate(handle: CapabilityHandle, rights: Rights) -> Result<CapabilityHandle> {
    let mut result = syscall::CAPABILITY_DUPLICATE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
            in("rsi") rights.bits(),
        );
    }
    decode(result)
}

pub fn replace(handle: CapabilityHandle, rights: Rights) -> Result<CapabilityHandle> {
    let mut result = syscall::CAPABILITY_REPLACE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
            in("rsi") rights.bits(),
        );
    }
    decode(result)
}

pub fn grant_child(
    child_process_id: u64,
    source_handle: CapabilityHandle,
    rights: Rights,
    requested_child_handle: CapabilityHandle,
) -> Result<CapabilityHandle> {
    let mut result = phase1_protection_abi::syscall::GRANT_CHILD;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") child_process_id,
            in("rsi") source_handle,
            in("rdx") rights.bits(),
            in("r10") requested_child_handle,
        );
    }
    decode(result)
}

pub fn close(handle: CapabilityHandle) -> Result<()> {
    let mut result = syscall::CAPABILITY_CLOSE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
        );
    }
    decode(result).map(|_| ())
}

pub fn info(handle: CapabilityHandle) -> Result<CapabilityInfo> {
    let mut raw = abi_capability::Info::EMPTY;
    let mut result = syscall::CAPABILITY_INFO;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
            in("rsi") (&mut raw as *mut abi_capability::Info) as u64,
            in("rdx") size_of::<abi_capability::Info>() as u64,
        );
    }
    decode(result)?;
    let kind = ObjectKind::from_raw(raw.kind).ok_or(Error::IO)?;
    let rights = Rights::from_bits(raw.rights).ok_or(Error::IO)?;
    Ok(CapabilityInfo {
        object_id: raw.object_id,
        kind,
        rights,
        size: raw.size,
    })
}

pub fn signal_state(handle: CapabilityHandle) -> Result<Signals> {
    let mut result = syscall::CAPABILITY_SIGNAL_STATE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
        );
    }
    let signals = decode(result)?;
    Signals::from_bits(signals).ok_or(Error::IO)
}

pub fn wait_one(
    handle: CapabilityHandle,
    requested: Signals,
    deadline: Deadline,
) -> Result<Signals> {
    let mut result = syscall::OBJECT_WAIT_ONE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
            in("rsi") requested.bits(),
            in("rdx") deadline.as_monotonic_ns(),
        );
    }
    let signals = decode(result)?;
    Signals::from_bits(signals).ok_or(Error::IO)
}

pub fn wait_for_handle(handle: CapabilityHandle) -> Result<CapabilityInfo> {
    loop {
        match info(handle) {
            Ok(info) => return Ok(info),
            Err(error) if error == Error::BAD_FILE_DESCRIPTOR || error == Error::TRY_AGAIN => {
                if crate::syscall::yield_now().is_err() {
                    return Err(Error::IO);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn endpoint_create() -> Result<CapabilityHandle> {
    let mut result = syscall::ENDPOINT_CREATE;
    unsafe {
        asm!("int 0x80", inlateout("rax") result);
    }
    decode(result)
}

pub fn send(endpoint: CapabilityHandle, bytes: &[u8], transfer: Option<Transfer>) -> Result<()> {
    let transfer_handle = transfer
        .map(|transfer| transfer.handle)
        .unwrap_or(abi_capability::INVALID_HANDLE);
    let transfer_rights = transfer.map(|transfer| transfer.rights.bits()).unwrap_or(0);
    let mut result = syscall::ENDPOINT_SEND;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") endpoint,
            in("rsi") bytes.as_ptr() as u64,
            in("rdx") bytes.len() as u64,
            in("r10") transfer_handle,
            in("r8") transfer_rights,
        );
    }
    decode(result).map(|_| ())
}

pub fn send_move(endpoint: CapabilityHandle, bytes: &[u8], transfer: Transfer) -> Result<()> {
    let mut result = syscall::ENDPOINT_SEND_MOVE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") endpoint,
            in("rsi") bytes.as_ptr() as u64,
            in("rdx") bytes.len() as u64,
            in("r10") transfer.handle,
            in("r8") transfer.rights.bits(),
        );
    }
    decode(result).map(|_| ())
}

pub fn try_receive(endpoint: CapabilityHandle, buffer: &mut [u8]) -> Result<ReceivedMessage> {
    let mut raw = abi_capability::MessageInfo::EMPTY;
    let mut result = syscall::ENDPOINT_RECEIVE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") endpoint,
            in("rsi") buffer.as_mut_ptr() as u64,
            in("rdx") buffer.len() as u64,
            in("r10") (&mut raw as *mut abi_capability::MessageInfo) as u64,
        );
    }
    let bytes = decode(result)? as usize;
    if bytes != raw.byte_count as usize || bytes > buffer.len() {
        return Err(Error::IO);
    }
    let capability = if raw.transferred_handle == abi_capability::INVALID_HANDLE {
        if raw.transferred_rights != 0 {
            return Err(Error::IO);
        }
        None
    } else {
        Some(ReceivedCapability {
            handle: raw.transferred_handle,
            rights: Rights::from_bits(raw.transferred_rights).ok_or(Error::IO)?,
        })
    };
    Ok(ReceivedMessage {
        sender_process_id: raw.sender_process_id,
        bytes,
        capability,
    })
}

pub fn receive(endpoint: CapabilityHandle, buffer: &mut [u8]) -> Result<ReceivedMessage> {
    loop {
        match try_receive(endpoint, buffer) {
            Ok(message) => return Ok(message),
            Err(error) if error == Error::TRY_AGAIN => {
                if crate::syscall::yield_now().is_err() {
                    return Err(Error::IO);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn notification_create() -> Result<CapabilityHandle> {
    let mut result = syscall::NOTIFICATION_CREATE;
    unsafe {
        asm!("int 0x80", inlateout("rax") result);
    }
    decode(result)
}

pub fn notification_signal(handle: CapabilityHandle, amount: u64) -> Result<u64> {
    let mut result = syscall::NOTIFICATION_SIGNAL;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
            in("rsi") amount,
        );
    }
    decode(result)
}

pub fn notification_try_wait(handle: CapabilityHandle) -> Result<u64> {
    let mut result = syscall::NOTIFICATION_TRY_WAIT;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
        );
    }
    decode(result)
}

pub fn notification_wait(handle: CapabilityHandle) -> Result<u64> {
    loop {
        match notification_try_wait(handle) {
            Ok(remaining) => return Ok(remaining),
            Err(error) if error == Error::TRY_AGAIN => {
                if crate::syscall::yield_now().is_err() {
                    return Err(Error::IO);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn shared_memory_create(length: usize) -> Result<CapabilityHandle> {
    let mut result = syscall::SHARED_MEMORY_CREATE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") length as u64,
        );
    }
    decode(result)
}

pub fn shared_memory_read(
    handle: CapabilityHandle,
    offset: usize,
    buffer: &mut [u8],
) -> Result<usize> {
    let mut result = syscall::SHARED_MEMORY_READ;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
            in("rsi") offset as u64,
            in("rdx") buffer.as_mut_ptr() as u64,
            in("r10") buffer.len() as u64,
        );
    }
    decode(result).map(|count| count as usize)
}

pub fn shared_memory_write(handle: CapabilityHandle, offset: usize, bytes: &[u8]) -> Result<usize> {
    let mut result = syscall::SHARED_MEMORY_WRITE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
            in("rsi") offset as u64,
            in("rdx") bytes.as_ptr() as u64,
            in("r10") bytes.len() as u64,
        );
    }
    decode(result).map(|count| count as usize)
}

pub fn job_create() -> Result<CapabilityHandle> {
    let mut result = syscall::JOB_CREATE;
    unsafe {
        asm!("int 0x80", inlateout("rax") result);
    }
    decode(result)
}

pub fn job_create_child(parent: CapabilityHandle) -> Result<CapabilityHandle> {
    let mut result = syscall::JOB_CREATE_CHILD;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") parent,
        );
    }
    decode(result)
}

pub fn job_set_process_limit(handle: CapabilityHandle, limit: usize) -> Result<usize> {
    let mut result = syscall::JOB_SET_PROCESS_LIMIT;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
            in("rsi") limit as u64,
        );
    }
    decode(result).map(|limit| limit as usize)
}

pub fn job_get_process_limit(handle: CapabilityHandle) -> Result<usize> {
    let mut result = syscall::JOB_GET_PROCESS_LIMIT;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
        );
    }
    decode(result).map(|limit| limit as usize)
}

pub fn job_retire(handle: CapabilityHandle) -> Result<()> {
    let mut result = syscall::JOB_RETIRE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
        );
    }
    decode(result).map(|_| ())
}

pub fn job_assign(handle: CapabilityHandle, child_process_id: u64) -> Result<u64> {
    let mut result = syscall::JOB_ASSIGN;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
            in("rsi") child_process_id,
        );
    }
    decode(result)
}

pub fn job_try_wait(handle: CapabilityHandle) -> Result<JobExit> {
    let mut raw = crate::abi::job::Exit::EMPTY;
    let mut result = syscall::JOB_TRY_WAIT;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
            in("rsi") (&mut raw as *mut crate::abi::job::Exit) as u64,
            in("rdx") size_of::<crate::abi::job::Exit>() as u64,
        );
    }
    decode(result)?;
    if raw.process_id == 0 {
        return Err(Error::IO);
    }
    Ok(JobExit {
        process_id: raw.process_id,
        status: crate::syscall::ChildStatus::from_raw(raw.status),
    })
}

pub fn job_wait(handle: CapabilityHandle) -> Result<JobExit> {
    loop {
        match job_try_wait(handle) {
            Ok(exit) => return Ok(exit),
            Err(error) if error == Error::TRY_AGAIN => {
                if crate::syscall::yield_now().is_err() {
                    return Err(Error::IO);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn job_terminate(handle: CapabilityHandle) -> Result<usize> {
    let mut result = syscall::JOB_TERMINATE;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") handle,
        );
    }
    decode(result).map(|count| count as usize)
}

#[cfg(test)]
mod tests {
    use super::{Deadline, ObjectKind, Rights, Signals, phase1_protection_abi};
    use crate::abi::{capability, syscall};

    #[test]
    fn rights_reject_unknown_bits() {
        assert!(Rights::from_bits(capability::ENDPOINT_RIGHTS).is_some());
        assert!(Rights::from_bits(1 << 63).is_none());
    }

    #[test]
    fn rights_preserve_restrictions() {
        let rights = Rights::SEND | Rights::TRANSFER;
        assert!(rights.contains(Rights::SEND));
        assert!(!rights.contains(Rights::RECEIVE));
    }

    #[test]
    fn object_signal_masks_reject_unknown_bits() {
        let state = Signals::READABLE | Signals::WRITABLE;

        assert!(state.contains(Signals::READABLE));
        assert!(!state.contains(Signals::PEER_CLOSED));
        assert_eq!(Signals::from_bits(state.bits()), Some(state));
        assert_eq!(Signals::from_bits(1 << 63), None);
    }

    #[test]
    fn object_wait_deadlines_preserve_absolute_monotonic_values() {
        assert_eq!(Deadline::IMMEDIATE.as_monotonic_ns(), 0);
        assert_eq!(Deadline::INFINITE.as_monotonic_ns(), u64::MAX);
        assert_eq!(
            Deadline::from_monotonic_ns(123_456).as_monotonic_ns(),
            123_456
        );
    }

    #[test]
    fn object_kinds_decode() {
        assert_eq!(
            ObjectKind::from_raw(capability::KIND_ENDPOINT),
            Some(ObjectKind::Endpoint)
        );
        assert_eq!(
            ObjectKind::from_raw(capability::KIND_KERNEL_EARLY_LOG_READER),
            Some(ObjectKind::KernelEarlyLogReader)
        );
        assert_eq!(
            ObjectKind::from_raw(capability::KIND_JOB),
            Some(ObjectKind::Job)
        );
        assert_eq!(ObjectKind::from_raw(99), None);
    }

    #[test]
    fn job_exit_layout_and_syscall_numbers_are_stable() {
        assert_eq!(core::mem::size_of::<crate::abi::job::Exit>(), 16);
        assert_eq!(core::mem::align_of::<crate::abi::job::Exit>(), 8);
        assert_eq!(syscall::JOB_CREATE, 60);
        assert_eq!(syscall::JOB_ASSIGN, 61);
        assert_eq!(syscall::JOB_TRY_WAIT, 62);
        assert_eq!(syscall::JOB_TERMINATE, 63);
        assert_eq!(syscall::JOB_CREATE_CHILD, 64);
        assert_eq!(syscall::JOB_SET_PROCESS_LIMIT, 65);
        assert_eq!(syscall::JOB_RETIRE, 66);
        assert_eq!(syscall::JOB_GET_PROCESS_LIMIT, 67);
        assert_eq!(syscall::CAPABILITY_REPLACE, 68);
        assert_eq!(syscall::ENDPOINT_SEND_MOVE, 69);
        assert_eq!(syscall::CAPABILITY_SIGNAL_STATE, 70);
        assert_eq!(syscall::MONOTONIC_TIME, 71);
        assert_eq!(syscall::OBJECT_WAIT_ONE, 72);
        assert_eq!(
            crate::syscall::ChildStatus::from_raw(
                crate::abi::child_status::SIGNAL_BASE + crate::abi::signal::KILL,
            )
            .signal(),
            Some(crate::abi::signal::KILL)
        );
    }

    #[test]
    fn protection_bootstrap_syscall_does_not_overlap_endpoint_wait() {
        assert_ne!(
            phase1_protection_abi::syscall::GRANT_CHILD,
            syscall::ENDPOINT_WAIT
        );
        assert_ne!(
            syscall::OPEN_KERNEL_EARLY_LOG_READER,
            syscall::KERNEL_EARLY_LOG_READ
        );
        assert_ne!(
            syscall::OPEN_KERNEL_EARLY_LOG_READER,
            phase1_protection_abi::syscall::GRANT_CHILD
        );
    }
}
