//! Client helpers for the supervised userspace tmpfs service.

use core::{mem::size_of, slice};

use crate::{
    blocking_ipc,
    ipc::{self, CapabilityHandle, Rights, Transfer},
};

pub mod protocol {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/tmpfs_protocol.rs"
    ));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Transport,
    Invalid,
    NotFound,
    NoSpace,
    Range,
    StaleMount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mount {
    service: CapabilityHandle,
    generation: u32,
}

impl Mount {
    pub fn connect(service: CapabilityHandle) -> Result<Self, Error> {
        let mut request_value = protocol::Request::EMPTY;
        request_value.operation = protocol::operation::MOUNT;
        let reply = request(service, &request_value)?;
        if reply.generation == 0 {
            return Err(Error::Transport);
        }
        Ok(Self {
            service,
            generation: reply.generation,
        })
    }

    pub const fn generation(self) -> u32 {
        self.generation
    }

    pub fn write(self, name: &[u8], bytes: &[u8]) -> Result<usize, Error> {
        if bytes.len() > protocol::MAX_DATA_BYTES {
            return Err(Error::Range);
        }
        let mut request_value = named(protocol::operation::WRITE, self.generation, name)?;
        request_value.data_length = bytes.len() as u16;
        request_value.data[..bytes.len()].copy_from_slice(bytes);
        request(self.service, &request_value).map(|reply| reply.value as usize)
    }

    pub fn read(self, name: &[u8], offset: usize, buffer: &mut [u8]) -> Result<usize, Error> {
        if buffer.len() > protocol::MAX_DATA_BYTES {
            return Err(Error::Range);
        }
        let mut request_value = named(protocol::operation::READ, self.generation, name)?;
        request_value.offset = u32::try_from(offset).map_err(|_| Error::Range)?;
        request_value.data_length = buffer.len() as u16;
        let reply = request(self.service, &request_value)?;
        let count = reply.data_length as usize;
        if count > buffer.len() || count > reply.data.len() {
            return Err(Error::Transport);
        }
        buffer[..count].copy_from_slice(&reply.data[..count]);
        Ok(count)
    }

    pub fn stat(self, name: &[u8]) -> Result<usize, Error> {
        let request_value = named(protocol::operation::STAT, self.generation, name)?;
        request(self.service, &request_value).map(|reply| reply.value as usize)
    }

    pub fn remove(self, name: &[u8]) -> Result<(), Error> {
        let request_value = named(protocol::operation::REMOVE, self.generation, name)?;
        request(self.service, &request_value).map(|_| ())
    }

    pub fn list(self, buffer: &mut [u8]) -> Result<usize, Error> {
        if buffer.len() > protocol::MAX_DATA_BYTES {
            return Err(Error::Range);
        }
        let mut request_value = protocol::Request::EMPTY;
        request_value.operation = protocol::operation::LIST;
        request_value.generation = self.generation;
        request_value.data_length = buffer.len() as u16;
        let reply = request(self.service, &request_value)?;
        let count = reply.data_length as usize;
        if count > buffer.len() || count > reply.data.len() {
            return Err(Error::Transport);
        }
        buffer[..count].copy_from_slice(&reply.data[..count]);
        Ok(count)
    }
}

fn bytes_of<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

fn request(
    service: CapabilityHandle,
    request: &protocol::Request,
) -> Result<protocol::Reply, Error> {
    let reply_endpoint = ipc::endpoint_create().map_err(|_| Error::Transport)?;
    let send_result = ipc::send(
        service,
        bytes_of(request),
        Some(Transfer {
            handle: reply_endpoint,
            rights: Rights::SEND,
        }),
    );
    if send_result.is_err() {
        let _ = ipc::close(reply_endpoint);
        return Err(Error::Transport);
    }

    let mut bytes = [0_u8; size_of::<protocol::Reply>()];
    let message =
        blocking_ipc::receive(reply_endpoint, &mut bytes).map_err(|_| Error::Transport)?;
    let _ = ipc::close(reply_endpoint);
    if message.capability.is_some() || message.bytes != bytes.len() {
        return Err(Error::Transport);
    }
    let reply = unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const protocol::Reply) };
    if reply.version != protocol::VERSION || reply.operation != request.operation {
        return Err(Error::Transport);
    }
    match reply.status {
        protocol::status::OK => Ok(reply),
        protocol::status::INVALID => Err(Error::Invalid),
        protocol::status::NOT_FOUND => Err(Error::NotFound),
        protocol::status::NO_SPACE => Err(Error::NoSpace),
        protocol::status::RANGE => Err(Error::Range),
        protocol::status::STALE_MOUNT => Err(Error::StaleMount),
        _ => Err(Error::Transport),
    }
}

fn named(operation: u16, generation: u32, name: &[u8]) -> Result<protocol::Request, Error> {
    if name.is_empty() || name.len() > protocol::MAX_NAME_BYTES || name.contains(&b'/') {
        return Err(Error::Invalid);
    }
    let mut request = protocol::Request::EMPTY;
    request.operation = operation;
    request.generation = generation;
    request.name_length = name.len() as u16;
    request.name[..name.len()].copy_from_slice(name);
    Ok(request)
}

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use super::{named, protocol};

    #[test]
    fn protocol_records_fit_endpoint_messages() {
        assert!(size_of::<protocol::Request>() <= 256);
        assert!(size_of::<protocol::Reply>() <= 256);
    }

    #[test]
    fn named_requests_preserve_mount_generation() {
        let request = named(protocol::operation::STAT, 41, b"state").expect("valid request");
        assert_eq!(request.version, protocol::VERSION);
        assert_eq!(request.generation, 41);
        assert_eq!(&request.name[..request.name_length as usize], b"state");
    }
}
