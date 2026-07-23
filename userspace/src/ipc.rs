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

use crate::abi::{capability as abi_capability, errno as abi_errno, syscall};

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

    pub const ENDPOINT: Self = Self(abi_capability::ENDPOINT_RIGHTS);
    pub const NOTIFICATION: Self = Self(abi_capability::NOTIFICATION_RIGHTS);
    pub const SHARED_MEMORY: Self = Self(abi_capability::SHARED_MEMORY_RIGHTS);

    pub const fn from_bits(bits: u64) -> Option<Self> {
        let all = abi_capability::ENDPOINT_RIGHTS
            | abi_capability::NOTIFICATION_RIGHTS
            | abi_capability::SHARED_MEMORY_RIGHTS;
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
pub enum ObjectKind {
    Endpoint,
    Notification,
    SharedMemory,
}

impl ObjectKind {
    const fn from_raw(raw: u64) -> Option<Self> {
        match raw {
            abi_capability::KIND_ENDPOINT => Some(Self::Endpoint),
            abi_capability::KIND_NOTIFICATION => Some(Self::Notification),
            abi_capability::KIND_SHARED_MEMORY => Some(Self::SharedMemory),
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

pub fn wait_for_handle(handle: CapabilityHandle) -> Result<CapabilityInfo> {
    loop {
        match info(handle) {
            Ok(info) => return Ok(info),
            Err(error)
                if error == Error::BAD_FILE_DESCRIPTOR || error == Error::TRY_AGAIN =>
            {
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

pub fn send(
    endpoint: CapabilityHandle,
    bytes: &[u8],
    transfer: Option<Transfer>,
) -> Result<()> {
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

pub fn shared_memory_write(
    handle: CapabilityHandle,
    offset: usize,
    bytes: &[u8],
) -> Result<usize> {
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

#[cfg(test)]
mod tests {
    use super::{ObjectKind, Rights};
    use crate::abi::capability;

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
    fn object_kinds_decode() {
        assert_eq!(
            ObjectKind::from_raw(capability::KIND_ENDPOINT),
            Some(ObjectKind::Endpoint)
        );
        assert_eq!(ObjectKind::from_raw(99), None);
    }
}
