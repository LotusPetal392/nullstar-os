//! Client helpers for the supervised userspace tmpfs service.

use core::{mem::size_of, slice};

use crate::ipc::{self, CapabilityHandle, Rights, Transfer};

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
    let message = ipc::receive(reply_endpoint, &mut bytes).map_err(|_| Error::Transport)?;
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
        _ => Err(Error::Transport),
    }
}

fn named(operation: u16, name: &[u8]) -> Result<protocol::Request, Error> {
    if name.is_empty()
        || name.len() > protocol::MAX_NAME_BYTES
        || name.iter().any(|byte| *byte == b'/')
    {
        return Err(Error::Invalid);
    }
    let mut request = protocol::Request::EMPTY;
    request.operation = operation;
    request.name_length = name.len() as u16;
    request.name[..name.len()].copy_from_slice(name);
    Ok(request)
}

pub fn write(service: CapabilityHandle, name: &[u8], bytes: &[u8]) -> Result<usize, Error> {
    if bytes.len() > protocol::MAX_DATA_BYTES {
        return Err(Error::Range);
    }
    let mut request = named(protocol::operation::WRITE, name)?;
    request.data_length = bytes.len() as u16;
    request.data[..bytes.len()].copy_from_slice(bytes);
    request(service, &request).map(|reply| reply.value as usize)
}

pub fn read(
    service: CapabilityHandle,
    name: &[u8],
    offset: usize,
    buffer: &mut [u8],
) -> Result<usize, Error> {
    if buffer.len() > protocol::MAX_DATA_BYTES {
        return Err(Error::Range);
    }
    let mut request_value = named(protocol::operation::READ, name)?;
    request_value.offset = u32::try_from(offset).map_err(|_| Error::Range)?;
    request_value.data_length = buffer.len() as u16;
    let reply = request(service, &request_value)?;
    let count = reply.data_length as usize;
    if count > buffer.len() || count > reply.data.len() {
        return Err(Error::Transport);
    }
    buffer[..count].copy_from_slice(&reply.data[..count]);
    Ok(count)
}

pub fn stat(service: CapabilityHandle, name: &[u8]) -> Result<usize, Error> {
    let request_value = named(protocol::operation::STAT, name)?;
    request(service, &request_value).map(|reply| reply.value as usize)
}

pub fn remove(service: CapabilityHandle, name: &[u8]) -> Result<(), Error> {
    let request_value = named(protocol::operation::REMOVE, name)?;
    request(service, &request_value).map(|_| ())
}

pub fn list(service: CapabilityHandle, buffer: &mut [u8]) -> Result<usize, Error> {
    if buffer.len() > protocol::MAX_DATA_BYTES {
        return Err(Error::Range);
    }
    let mut request_value = protocol::Request::EMPTY;
    request_value.operation = protocol::operation::LIST;
    request_value.data_length = buffer.len() as u16;
    let reply = request(service, &request_value)?;
    let count = reply.data_length as usize;
    if count > buffer.len() || count > reply.data.len() {
        return Err(Error::Transport);
    }
    buffer[..count].copy_from_slice(&reply.data[..count]);
    Ok(count)
}
