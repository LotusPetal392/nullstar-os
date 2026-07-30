//! Typed client for the capability-based partition block-device protocol.
//!
//! A session uses one persistent reply endpoint and at most one registered
//! shared-memory buffer. All block offsets are relative to the partition exposed
//! by the service.

use core::{mem::size_of, slice};

use crate::ipc::{self, CapabilityHandle, Rights, Transfer};

pub mod protocol {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/block_device_protocol.rs"
    ));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidRequestId,
    InvalidSession,
    InvalidBuffer,
    BufferAlreadyAttached,
    MissingBuffer,
    InvalidDeviceInfo,
    InvalidBlockCount,
    Range,
    ReadOnly,
    Permission,
    StaleSession,
    StaleBuffer,
    NotSupported,
    TryAgain,
    Transport,
    Service(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    logical_block_size: u32,
    block_count: u64,
    features: u64,
    flags: u32,
    session_id: u64,
    generation: u64,
}

impl DeviceInfo {
    pub const fn logical_block_size(self) -> u32 {
        self.logical_block_size
    }

    pub const fn block_count(self) -> u64 {
        self.block_count
    }

    pub const fn features(self) -> u64 {
        self.features
    }

    pub const fn flags(self) -> u32 {
        self.flags
    }

    pub const fn is_read_only(self) -> bool {
        self.flags & protocol::device_flags::READ_ONLY != 0
    }

    pub const fn supports(self, feature: u64) -> bool {
        self.features & feature == feature
    }

    fn from_info_reply(
        session: &Session,
        request: &protocol::Request,
        reply: &protocol::Reply,
    ) -> Option<Self> {
        if request.operation != protocol::operation::INFO
            || reply.status != protocol::status::OK
            || !valid_reply(request, reply)
        {
            return None;
        }
        Some(Self {
            logical_block_size: reply.logical_block_size,
            block_count: reply.block_count,
            features: reply.features,
            flags: reply.device_flags,
            session_id: session.id,
            generation: session.generation,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisteredBuffer {
    handle: CapabilityHandle,
    id: u64,
    length: usize,
    session_id: u64,
    generation: u64,
}

impl RegisteredBuffer {
    pub const fn handle(self) -> CapabilityHandle {
        self.handle
    }

    pub const fn id(self) -> u64 {
        self.id
    }

    pub const fn length(self) -> usize {
        self.length
    }
}

#[derive(Debug)]
pub struct Session {
    service: CapabilityHandle,
    reply_endpoint: CapabilityHandle,
    id: u64,
    generation: u64,
    buffer: Option<RegisteredBuffer>,
}

impl Session {
    fn from_connect_reply(request: &protocol::Request, reply: &protocol::Reply) -> Option<Self> {
        if request.operation != protocol::operation::CONNECT
            || reply.status != protocol::status::OK
            || !valid_reply(request, reply)
        {
            return None;
        }
        Some(Self {
            service: 0,
            reply_endpoint: 0,
            id: reply.session_id,
            generation: reply.generation,
            buffer: None,
        })
    }

    pub const fn id(&self) -> u64 {
        self.id
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn registered_buffer(&self) -> Option<RegisteredBuffer> {
        self.buffer
    }

    pub fn attach_shared_buffer(
        &mut self,
        request_id: u64,
        buffer_id: u64,
        handle: CapabilityHandle,
        length: usize,
    ) -> Result<RegisteredBuffer, Error> {
        let request = self.attach_buffer_request(request_id, buffer_id, length)?;
        self.exchange(
            &request,
            Some(Transfer {
                handle,
                rights: Rights::READ | Rights::WRITE,
            }),
        )?;
        let buffer = RegisteredBuffer {
            handle,
            id: buffer_id,
            length,
            session_id: self.id,
            generation: self.generation,
        };
        self.buffer = Some(buffer);
        Ok(buffer)
    }

    pub fn info(&self, request_id: u64) -> Result<DeviceInfo, Error> {
        let request = self.info_request(request_id)?;
        let reply = self.exchange(&request, None)?;
        DeviceInfo::from_info_reply(self, &request, &reply).ok_or(Error::Transport)
    }

    pub fn read_to_shared_buffer(
        &self,
        request_id: u64,
        info: DeviceInfo,
        block_offset: u64,
        block_count: u32,
        buffer_offset: usize,
    ) -> Result<u32, Error> {
        let request =
            self.read_request(request_id, info, block_offset, block_count, buffer_offset)?;
        let reply = self.exchange(&request, None)?;
        Ok(reply.transferred_blocks)
    }

    pub fn write_from_shared_buffer(
        &self,
        request_id: u64,
        info: DeviceInfo,
        block_offset: u64,
        block_count: u32,
        buffer_offset: usize,
    ) -> Result<u32, Error> {
        if info.is_read_only() || !info.supports(protocol::features::WRITE) {
            return Err(Error::ReadOnly);
        }
        let request =
            self.write_request(request_id, info, block_offset, block_count, buffer_offset)?;
        let reply = self.exchange(&request, None)?;
        Ok(reply.transferred_blocks)
    }

    pub fn flush(&self, request_id: u64) -> Result<(), Error> {
        let request = self.flush_request(request_id)?;
        self.exchange(&request, None).map(|_| ())
    }

    pub fn exchange_protocol_request(
        &self,
        request: &protocol::Request,
    ) -> Result<protocol::Reply, Error> {
        if !valid_request(request)
            || request.operation == protocol::operation::CONNECT
            || request.session_id != self.id
            || request.generation != self.generation
        {
            return Err(Error::InvalidSession);
        }
        self.exchange(request, None)
    }

    pub fn disconnect(&mut self, request_id: u64) -> Result<(), Error> {
        let request = self.disconnect_request(request_id)?;
        self.exchange(&request, None)?;

        let reply_endpoint = self.reply_endpoint;
        self.service = 0;
        self.reply_endpoint = 0;
        self.id = protocol::INVALID_ID;
        self.generation = 0;
        self.buffer = None;
        ipc::close(reply_endpoint).map_err(|_| Error::Transport)
    }

    pub fn attach_buffer_request(
        &self,
        request_id: u64,
        buffer_id: u64,
        length: usize,
    ) -> Result<protocol::Request, Error> {
        if self.buffer.is_some() {
            return Err(Error::BufferAlreadyAttached);
        }
        if buffer_id == protocol::INVALID_ID || length == 0 {
            return Err(Error::InvalidBuffer);
        }
        let mut request = self.request(protocol::operation::ATTACH_BUFFER, request_id)?;
        request.buffer_id = buffer_id;
        request.buffer_length = u64::try_from(length).map_err(|_| Error::Range)?;
        Ok(request)
    }

    pub fn info_request(&self, request_id: u64) -> Result<protocol::Request, Error> {
        self.request(protocol::operation::INFO, request_id)
    }

    pub fn read_request(
        &self,
        request_id: u64,
        info: DeviceInfo,
        block_offset: u64,
        block_count: u32,
        buffer_offset: usize,
    ) -> Result<protocol::Request, Error> {
        self.transfer_request(
            protocol::operation::READ,
            request_id,
            info,
            block_offset,
            block_count,
            buffer_offset,
        )
    }

    pub fn write_request(
        &self,
        request_id: u64,
        info: DeviceInfo,
        block_offset: u64,
        block_count: u32,
        buffer_offset: usize,
    ) -> Result<protocol::Request, Error> {
        if info.is_read_only() || !info.supports(protocol::features::WRITE) {
            return Err(Error::ReadOnly);
        }
        self.transfer_request(
            protocol::operation::WRITE,
            request_id,
            info,
            block_offset,
            block_count,
            buffer_offset,
        )
    }

    pub fn flush_request(&self, request_id: u64) -> Result<protocol::Request, Error> {
        self.request(protocol::operation::FLUSH, request_id)
    }

    pub fn disconnect_request(&self, request_id: u64) -> Result<protocol::Request, Error> {
        self.request(protocol::operation::DISCONNECT, request_id)
    }

    fn transfer_request(
        &self,
        operation: u16,
        request_id: u64,
        info: DeviceInfo,
        block_offset: u64,
        block_count: u32,
        buffer_offset: usize,
    ) -> Result<protocol::Request, Error> {
        self.validate_device_info(info)?;
        if block_count == 0 {
            return Err(Error::InvalidBlockCount);
        }
        block_offset
            .checked_add(u64::from(block_count))
            .filter(|end| *end <= info.block_count)
            .ok_or(Error::Range)?;

        let buffer = self.buffer.ok_or(Error::MissingBuffer)?;
        self.validate_buffer(buffer)?;
        let byte_length = u64::from(block_count)
            .checked_mul(u64::from(info.logical_block_size))
            .filter(|length| *length <= protocol::MAX_TRANSFER_BYTES as u64)
            .ok_or(Error::Range)?;
        let buffer_offset = u64::try_from(buffer_offset).map_err(|_| Error::Range)?;
        let buffer_end = buffer_offset.checked_add(byte_length).ok_or(Error::Range)?;
        if buffer_end > u64::try_from(buffer.length).map_err(|_| Error::Range)? {
            return Err(Error::Range);
        }

        let mut request = self.request(operation, request_id)?;
        request.buffer_id = buffer.id;
        request.buffer_offset = buffer_offset;
        request.buffer_length = byte_length;
        request.block_offset = block_offset;
        request.block_count = block_count;
        Ok(request)
    }

    fn request(&self, operation: u16, request_id: u64) -> Result<protocol::Request, Error> {
        if request_id == protocol::INVALID_ID {
            return Err(Error::InvalidRequestId);
        }
        if self.id == protocol::INVALID_ID || self.generation == 0 {
            return Err(Error::InvalidSession);
        }
        let mut request = protocol::Request::EMPTY;
        request.operation = operation;
        request.request_id = request_id;
        request.session_id = self.id;
        request.generation = self.generation;
        Ok(request)
    }

    pub fn validate_device_info(&self, info: DeviceInfo) -> Result<(), Error> {
        if info.session_id != self.id || info.generation != self.generation {
            Err(Error::InvalidSession)
        } else if info.logical_block_size == 0
            || info.features & !protocol::features::ALL != 0
            || info.flags & !protocol::device_flags::ALL != 0
            || !info.supports(protocol::features::READ)
            || info.is_read_only() && info.supports(protocol::features::WRITE)
        {
            Err(Error::InvalidDeviceInfo)
        } else {
            Ok(())
        }
    }

    fn validate_buffer(&self, buffer: RegisteredBuffer) -> Result<(), Error> {
        if buffer.id == protocol::INVALID_ID || buffer.length == 0 {
            Err(Error::InvalidBuffer)
        } else if buffer.session_id != self.id || buffer.generation != self.generation {
            Err(Error::InvalidSession)
        } else {
            Ok(())
        }
    }

    fn exchange(
        &self,
        request: &protocol::Request,
        transfer: Option<Transfer>,
    ) -> Result<protocol::Reply, Error> {
        if self.service == 0 || self.reply_endpoint == 0 {
            return Err(Error::Transport);
        }
        send_request(self.service, request, transfer)?;
        receive_reply(self.reply_endpoint, request)
    }
}

pub fn connect(request_id: u64) -> Result<protocol::Request, Error> {
    if request_id == protocol::INVALID_ID {
        return Err(Error::InvalidRequestId);
    }
    let mut request = protocol::Request::EMPTY;
    request.operation = protocol::operation::CONNECT;
    request.request_id = request_id;
    Ok(request)
}

pub fn connect_service(service: CapabilityHandle, request_id: u64) -> Result<Session, Error> {
    let request = connect(request_id)?;
    let reply_endpoint = ipc::endpoint_create().map_err(|_| Error::Transport)?;
    if send_request(
        service,
        &request,
        Some(Transfer {
            handle: reply_endpoint,
            rights: Rights::SEND,
        }),
    )
    .is_err()
    {
        let _ = ipc::close(reply_endpoint);
        return Err(Error::Transport);
    }
    let reply = match receive_reply(reply_endpoint, &request) {
        Ok(reply) => reply,
        Err(error) => {
            let _ = ipc::close(reply_endpoint);
            return Err(error);
        }
    };
    let Some(mut session) = Session::from_connect_reply(&request, &reply) else {
        let _ = ipc::close(reply_endpoint);
        return Err(Error::Transport);
    };
    session.service = service;
    session.reply_endpoint = reply_endpoint;
    Ok(session)
}

pub fn valid_request(request: &protocol::Request) -> bool {
    if request.version != protocol::VERSION
        || !protocol::operation::is_defined(request.operation)
        || request.flags & !protocol::request_flags::ALL != 0
        || request.request_id == protocol::INVALID_ID
        || request.reserved != [0; 3]
    {
        return false;
    }

    let empty_transfer = request.buffer_id == protocol::INVALID_ID
        && request.buffer_offset == 0
        && request.buffer_length == 0
        && request.block_offset == 0
        && request.block_count == 0;
    match request.operation {
        protocol::operation::CONNECT => {
            request.session_id == protocol::INVALID_ID && request.generation == 0 && empty_transfer
        }
        protocol::operation::ATTACH_BUFFER => {
            request.session_id != protocol::INVALID_ID
                && request.generation != 0
                && request.buffer_id != protocol::INVALID_ID
                && request.buffer_offset == 0
                && request.buffer_length != 0
                && request.block_offset == 0
                && request.block_count == 0
        }
        protocol::operation::INFO
        | protocol::operation::FLUSH
        | protocol::operation::DISCONNECT => {
            request.session_id != protocol::INVALID_ID && request.generation != 0 && empty_transfer
        }
        protocol::operation::READ | protocol::operation::WRITE => {
            request.session_id != protocol::INVALID_ID
                && request.generation != 0
                && request.buffer_id != protocol::INVALID_ID
                && request.buffer_length != 0
                && request.block_count != 0
                && request.buffer_end().is_some()
        }
        _ => false,
    }
}

pub fn valid_reply(request: &protocol::Request, reply: &protocol::Reply) -> bool {
    if !valid_request(request)
        || reply.version != protocol::VERSION
        || reply.operation != request.operation
        || reply.request_id != request.request_id
        || reply.reserved != [0; 3]
    {
        return false;
    }

    if request.operation == protocol::operation::CONNECT {
        if reply.status == protocol::status::OK {
            if reply.session_id == protocol::INVALID_ID || reply.generation == 0 {
                return false;
            }
        } else if reply.session_id != protocol::INVALID_ID || reply.generation != 0 {
            return false;
        }
    } else if reply.session_id != request.session_id || reply.generation != request.generation {
        return false;
    }

    if reply.status != protocol::status::OK {
        return empty_reply_payload(reply);
    }

    match request.operation {
        protocol::operation::CONNECT
        | protocol::operation::FLUSH
        | protocol::operation::DISCONNECT => empty_reply_payload(reply),
        protocol::operation::ATTACH_BUFFER => {
            reply.features == 0
                && reply.block_count == 0
                && reply.buffer_id == request.buffer_id
                && reply.logical_block_size == 0
                && reply.transferred_blocks == 0
                && reply.device_flags == 0
        }
        protocol::operation::INFO => valid_info_payload(reply),
        protocol::operation::READ | protocol::operation::WRITE => {
            reply.features == 0
                && reply.block_count == 0
                && reply.buffer_id == request.buffer_id
                && reply.logical_block_size == 0
                && reply.transferred_blocks <= request.block_count
                && reply.device_flags == 0
        }
        _ => false,
    }
}

fn valid_info_payload(reply: &protocol::Reply) -> bool {
    reply.features & !protocol::features::ALL == 0
        && reply.features & protocol::features::READ != 0
        && reply.block_count != 0
        && reply.buffer_id == protocol::INVALID_ID
        && reply.logical_block_size != 0
        && reply.transferred_blocks == 0
        && reply.device_flags & !protocol::device_flags::ALL == 0
        && !(reply.device_flags & protocol::device_flags::READ_ONLY != 0
            && reply.features & protocol::features::WRITE != 0)
}

fn empty_reply_payload(reply: &protocol::Reply) -> bool {
    reply.features == 0
        && reply.block_count == 0
        && reply.buffer_id == protocol::INVALID_ID
        && reply.logical_block_size == 0
        && reply.transferred_blocks == 0
        && reply.device_flags == 0
}

fn send_request(
    endpoint: CapabilityHandle,
    request: &protocol::Request,
    transfer: Option<Transfer>,
) -> Result<(), Error> {
    send_with_retry(
        || ipc::send(endpoint, bytes_of(request), transfer),
        || crate::syscall::yield_now().is_ok(),
    )
}

fn send_with_retry(
    mut send: impl FnMut() -> ipc::Result<()>,
    mut yield_now: impl FnMut() -> bool,
) -> Result<(), Error> {
    loop {
        match send() {
            Ok(()) => return Ok(()),
            Err(error) if error == ipc::Error::TRY_AGAIN => {
                if !yield_now() {
                    return Err(Error::Transport);
                }
            }
            Err(_) => return Err(Error::Transport),
        }
    }
}

fn receive_reply(
    endpoint: CapabilityHandle,
    request: &protocol::Request,
) -> Result<protocol::Reply, Error> {
    let mut bytes = [0_u8; size_of::<protocol::Reply>()];
    let message = ipc::receive(endpoint, &mut bytes).map_err(|_| Error::Transport)?;
    if let Some(capability) = message.capability {
        let _ = ipc::close(capability.handle);
        return Err(Error::Transport);
    }
    if !valid_reply_message_metadata(&message, bytes.len()) {
        return Err(Error::Transport);
    }
    let reply = unsafe { core::ptr::read_unaligned(bytes.as_ptr().cast::<protocol::Reply>()) };
    if !valid_reply(request, &reply) {
        return Err(Error::Transport);
    }
    match reply.status {
        protocol::status::OK => Ok(reply),
        protocol::status::PERMISSION => Err(Error::Permission),
        protocol::status::RANGE => Err(Error::Range),
        protocol::status::STALE_SESSION => Err(Error::StaleSession),
        protocol::status::STALE_BUFFER => Err(Error::StaleBuffer),
        protocol::status::READ_ONLY => Err(Error::ReadOnly),
        protocol::status::TRY_AGAIN => Err(Error::TryAgain),
        protocol::status::NOT_SUPPORTED => Err(Error::NotSupported),
        status => Err(Error::Service(status)),
    }
}

fn valid_reply_message_metadata(message: &ipc::ReceivedMessage, expected_bytes: usize) -> bool {
    message.sender_process_id == 0
        && message.bytes == expected_bytes
        && message.capability.is_none()
}

fn bytes_of<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

const _: () = assert!(size_of::<protocol::Request>() <= protocol::MAX_MESSAGE_BYTES);
const _: () = assert!(size_of::<protocol::Reply>() <= protocol::MAX_MESSAGE_BYTES);

#[cfg(test)]
mod tests {
    use core::mem::{align_of, offset_of, size_of};

    use super::{
        DeviceInfo, Error, RegisteredBuffer, Session, connect, protocol, send_with_retry,
        valid_reply, valid_reply_message_metadata, valid_request,
    };
    use crate::ipc;

    fn session_without_buffer() -> Session {
        let request = connect(1).expect("connect request");
        let mut reply = protocol::Reply::EMPTY;
        reply.operation = request.operation;
        reply.request_id = request.request_id;
        reply.session_id = 7;
        reply.generation = 11;
        Session::from_connect_reply(&request, &reply).expect("valid session")
    }

    fn session_with_buffer(buffer_length: usize) -> Session {
        let mut session = session_without_buffer();
        session.buffer = Some(RegisteredBuffer {
            handle: 99,
            id: 13,
            length: buffer_length,
            session_id: session.id,
            generation: session.generation,
        });
        session
    }

    fn read_only_info() -> DeviceInfo {
        DeviceInfo {
            logical_block_size: protocol::INITIAL_LOGICAL_BLOCK_SIZE,
            block_count: 8,
            features: protocol::features::READ,
            flags: protocol::device_flags::READ_ONLY,
            session_id: 7,
            generation: 11,
        }
    }

    fn writable_info() -> DeviceInfo {
        DeviceInfo {
            logical_block_size: protocol::INITIAL_LOGICAL_BLOCK_SIZE,
            block_count: 8,
            features: protocol::features::READ
                | protocol::features::WRITE
                | protocol::features::FLUSH,
            flags: 0,
            session_id: 7,
            generation: 11,
        }
    }

    #[test]
    fn wire_records_have_fixed_bounded_shape() {
        assert_eq!(size_of::<protocol::Request>(), 80);
        assert_eq!(size_of::<protocol::Reply>(), 80);
        assert_eq!(align_of::<protocol::Request>(), 8);
        assert_eq!(align_of::<protocol::Reply>(), 8);
        assert_eq!(offset_of!(protocol::Request, request_id), 8);
        assert_eq!(offset_of!(protocol::Request, block_offset), 56);
        assert_eq!(offset_of!(protocol::Reply, request_id), 8);
        assert_eq!(offset_of!(protocol::Reply, logical_block_size), 56);
        assert!(size_of::<protocol::Request>() <= protocol::MAX_MESSAGE_BYTES);
        assert!(size_of::<protocol::Reply>() <= protocol::MAX_MESSAGE_BYTES);
    }

    #[test]
    fn connect_and_session_requests_preserve_identity() {
        let request = connect(3).expect("connect request");
        assert!(valid_request(&request));
        assert_eq!(request.session_id, protocol::INVALID_ID);
        assert_eq!(connect(0), Err(Error::InvalidRequestId));

        let session = session_with_buffer(1024);
        let info = session.info_request(4).expect("info request");
        assert_eq!(info.session_id, 7);
        assert_eq!(info.generation, 11);
        assert!(valid_request(&info));
    }

    #[test]
    fn transfer_builders_check_partition_and_buffer_bounds() {
        let session = session_with_buffer(1024);
        let info = read_only_info();
        let request = session
            .read_request(5, info, 6, 2, 0)
            .expect("bounded request");
        assert_eq!(request.block_offset, 6);
        assert_eq!(request.block_count, 2);
        assert_eq!(request.buffer_length, 1024);
        assert!(valid_request(&request));

        assert_eq!(session.read_request(6, info, 7, 2, 0), Err(Error::Range));
        assert_eq!(
            session.read_request(7, info, 0, 0, 0),
            Err(Error::InvalidBlockCount)
        );
        assert_eq!(session.read_request(8, info, 0, 2, 1), Err(Error::Range));
        assert_eq!(
            session.write_request(9, info, 0, 1, 0),
            Err(Error::ReadOnly)
        );

        let session = session_with_buffer(8192);
        let mut info = read_only_info();
        info.block_count = 32;
        assert_eq!(session.read_request(10, info, 0, 9, 0), Err(Error::Range));
    }

    #[test]
    fn writable_metadata_builds_bounded_write_requests() {
        let session = session_with_buffer(1024);
        let info = writable_info();
        let request = session
            .write_request(9, info, 6, 2, 0)
            .expect("bounded writable request");
        assert_eq!(request.block_offset, 6);
        assert_eq!(request.block_count, 2);
        assert_eq!(request.buffer_length, 1024);
        assert!(valid_request(&request));
        assert_eq!(session.write_request(10, info, 7, 2, 0), Err(Error::Range));
    }

    #[test]
    fn replies_are_canonical_and_bound_to_request_identity() {
        let session = session_with_buffer(1024);
        let request = session
            .read_request(9, read_only_info(), 0, 1, 0)
            .expect("read request");
        let mut reply = protocol::Reply::EMPTY;
        reply.operation = request.operation;
        reply.request_id = request.request_id;
        reply.session_id = request.session_id;
        reply.generation = request.generation;
        reply.buffer_id = request.buffer_id;
        reply.transferred_blocks = 1;
        assert!(valid_reply(&request, &reply));

        reply.request_id += 1;
        assert!(!valid_reply(&request, &reply));
        reply.request_id = request.request_id;
        reply.generation += 1;
        assert!(!valid_reply(&request, &reply));
        reply.generation = request.generation;
        reply.transferred_blocks = 2;
        assert!(!valid_reply(&request, &reply));
    }

    #[test]
    fn info_reply_validates_features_and_read_only_consistency() {
        let session = session_with_buffer(1024);
        let request = session.info_request(10).expect("info request");
        let mut reply = protocol::Reply::EMPTY;
        reply.operation = request.operation;
        reply.request_id = request.request_id;
        reply.session_id = request.session_id;
        reply.generation = request.generation;
        reply.features = protocol::features::READ;
        reply.block_count = 8;
        reply.logical_block_size = protocol::INITIAL_LOGICAL_BLOCK_SIZE;
        reply.device_flags = protocol::device_flags::READ_ONLY;
        assert!(valid_reply(&request, &reply));
        assert!(DeviceInfo::from_info_reply(&session, &request, &reply).is_some());

        reply.features =
            protocol::features::READ | protocol::features::WRITE | protocol::features::FLUSH;
        reply.device_flags = 0;
        assert!(valid_reply(&request, &reply));
        assert_eq!(
            DeviceInfo::from_info_reply(&session, &request, &reply),
            Some(writable_info())
        );

        reply.generation += 1;
        assert!(DeviceInfo::from_info_reply(&session, &request, &reply).is_none());
        reply.generation = request.generation;

        reply.block_count = 0;
        assert!(!valid_reply(&request, &reply));
        assert!(DeviceInfo::from_info_reply(&session, &request, &reply).is_none());
        reply.block_count = 8;
        reply.device_flags = protocol::device_flags::READ_ONLY;
        reply.features = protocol::features::READ | protocol::features::WRITE;
        assert!(!valid_reply(&request, &reply));
        reply.features = protocol::features::READ;
        reply.reserved[0] = 1;
        assert!(!valid_reply(&request, &reply));
    }

    #[test]
    fn constructors_reject_fabricated_noncanonical_replies() {
        let request = connect(2).expect("connect request");
        let mut reply = protocol::Reply::EMPTY;
        reply.operation = request.operation;
        reply.request_id = request.request_id;
        reply.session_id = 7;
        reply.generation = 11;
        assert!(Session::from_connect_reply(&request, &reply).is_some());

        reply.version += 1;
        assert!(Session::from_connect_reply(&request, &reply).is_none());
        reply.version = protocol::VERSION;
        reply.reserved[0] = 1;
        assert!(Session::from_connect_reply(&request, &reply).is_none());
        reply.reserved = [0; 3];
        reply.features = protocol::features::READ;
        assert!(Session::from_connect_reply(&request, &reply).is_none());
    }

    #[test]
    fn failed_disconnect_preserves_the_session() {
        let mut session = session_without_buffer();
        assert_eq!(session.disconnect(3), Err(Error::Transport));
        assert_eq!(session.id(), 7);
        assert_eq!(session.generation(), 11);
        assert!(session.info_request(4).is_ok());
    }

    #[test]
    fn request_send_retries_try_again_after_cooperative_yields() {
        let mut attempts = 0;
        let mut yields = 0;
        assert_eq!(
            send_with_retry(
                || {
                    attempts += 1;
                    if attempts < 3 {
                        Err(ipc::Error::TRY_AGAIN)
                    } else {
                        Ok(())
                    }
                },
                || {
                    yields += 1;
                    true
                },
            ),
            Ok(())
        );
        assert_eq!(attempts, 3);
        assert_eq!(yields, 2);
    }

    #[test]
    fn reply_messages_must_come_from_the_kernel() {
        let mut message = ipc::ReceivedMessage {
            sender_process_id: 0,
            bytes: size_of::<protocol::Reply>(),
            capability: None,
        };
        assert!(valid_reply_message_metadata(
            &message,
            size_of::<protocol::Reply>()
        ));
        message.sender_process_id = 1;
        assert!(!valid_reply_message_metadata(
            &message,
            size_of::<protocol::Reply>()
        ));
    }

    #[test]
    fn read_only_status_is_a_canonical_error_reply() {
        let session = session_with_buffer(1024);
        let mut info = read_only_info();
        info.features |= protocol::features::WRITE;
        info.flags = 0;
        let request = session
            .write_request(11, info, 0, 1, 0)
            .expect("write request");
        let mut reply = protocol::Reply::EMPTY;
        reply.operation = request.operation;
        reply.status = protocol::status::READ_ONLY;
        reply.request_id = request.request_id;
        reply.session_id = request.session_id;
        reply.generation = request.generation;
        assert!(valid_reply(&request, &reply));
    }
}
