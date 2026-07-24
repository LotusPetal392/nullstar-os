//! Scheduler-integrated endpoint readiness waits.

use core::arch::asm;

use crate::ipc::{self, CapabilityHandle, ReceivedMessage};

pub fn endpoint_wait(endpoint: CapabilityHandle) -> ipc::Result<()> {
    let mut result = crate::abi::syscall::ENDPOINT_WAIT;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") endpoint,
        );
    }
    let signed = result as i64;
    if signed < 0 {
        let code = (-signed) as i32;
        Err(match code {
            2 => ipc::Error::NO_ENTRY,
            3 => ipc::Error::NO_PROCESS,
            5 => ipc::Error::IO,
            9 => ipc::Error::BAD_FILE_DESCRIPTOR,
            11 => ipc::Error::TRY_AGAIN,
            13 => ipc::Error::PERMISSION,
            14 => ipc::Error::BAD_ADDRESS,
            22 => ipc::Error::INVALID_ARGUMENT,
            28 => ipc::Error::NO_SPACE,
            34 => ipc::Error::RANGE,
            _ => ipc::Error::NOT_IMPLEMENTED,
        })
    } else {
        Ok(())
    }
}

pub fn receive(endpoint: CapabilityHandle, buffer: &mut [u8]) -> ipc::Result<ReceivedMessage> {
    loop {
        match ipc::try_receive(endpoint, buffer) {
            Ok(message) => return Ok(message),
            Err(error) if error == ipc::Error::TRY_AGAIN => endpoint_wait(endpoint)?,
            Err(error) => return Err(error),
        }
    }
}
