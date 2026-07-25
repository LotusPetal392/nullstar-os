//! Typed builders for the generic userspace filesystem-service protocol.
//!
//! This module defines the migration contract only. The existing tmpfs client
//! remains on protocol v2 until the service implements sessions and node IDs.

use core::{mem::size_of, slice};

use crate::ipc::{self, CapabilityHandle, Rights, Transfer};

pub mod protocol {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/filesystem_protocol.rs"
    ));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidName,
    InvalidRequestId,
    InvalidSession,
    InvalidNode,
    InvalidBuffer,
    InvalidFlags,
    Range,
    Transport,
    NotFound,
    StaleSession,
    StaleNode,
    Service(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    service: CapabilityHandle,
    reply_endpoint: CapabilityHandle,
    id: u64,
    generation: u64,
}

impl Session {
    pub const fn from_reply(reply: &protocol::Reply) -> Option<Self> {
        if reply.status == protocol::status::OK
            && reply.session_id != protocol::INVALID_ID
            && reply.generation != 0
        {
            Some(Self {
                service: 0,
                reply_endpoint: 0,
                id: reply.session_id,
                generation: reply.generation,
            })
        } else {
            None
        }
    }

    pub const fn id(self) -> u64 {
        self.id
    }

    pub const fn generation(self) -> u64 {
        self.generation
    }

    pub fn lookup_node(self, request_id: u64, directory: Node, name: &[u8]) -> Result<Node, Error> {
        let request = self.lookup(request_id, directory, name)?;
        let reply = self.exchange(&request, None)?;
        Node::from_reply(self, &reply).ok_or(Error::Transport)
    }

    pub fn attributes(
        self,
        request_id: u64,
        node: Node,
    ) -> Result<protocol::NodeAttributes, Error> {
        let mut request = self.request(protocol::operation::GET_ATTRIBUTES, request_id)?;
        request.node_id = node.id_for(self)?;
        let reply = self.exchange(&request, None)?;
        if usize::from(reply.data_length) != size_of::<protocol::NodeAttributes>() {
            return Err(Error::Transport);
        }
        Ok(unsafe {
            core::ptr::read_unaligned(reply.data.as_ptr() as *const protocol::NodeAttributes)
        })
    }

    pub fn attach_shared_buffer(
        self,
        request_id: u64,
        buffer_id: u64,
        handle: CapabilityHandle,
        length: usize,
    ) -> Result<(), Error> {
        let request = self.attach_buffer(request_id, buffer_id, length)?;
        self.exchange(
            &request,
            Some(Transfer {
                handle,
                rights: Rights::READ | Rights::WRITE,
            }),
        )
        .map(|_| ())
    }

    pub fn disconnect(self, request_id: u64) -> Result<(), Error> {
        let request = self.request(protocol::operation::DISCONNECT, request_id)?;
        let result = self.exchange(&request, None).map(|_| ());
        let _ = ipc::close(self.reply_endpoint);
        result
    }

    fn exchange(
        self,
        request: &protocol::Request,
        transfer: Option<Transfer>,
    ) -> Result<protocol::Reply, Error> {
        if self.service == 0 || self.reply_endpoint == 0 {
            return Err(Error::Transport);
        }
        ipc::send(self.service, bytes_of(request), transfer).map_err(|_| Error::Transport)?;
        receive_reply(self.reply_endpoint, request)
    }

    pub fn request(self, operation: u16, request_id: u64) -> Result<protocol::Request, Error> {
        if request_id == protocol::INVALID_ID {
            return Err(Error::InvalidRequestId);
        }
        let mut request = protocol::Request::EMPTY;
        request.operation = operation;
        request.request_id = request_id;
        request.session_id = self.id;
        request.generation = self.generation;
        Ok(request)
    }

    pub fn lookup(
        self,
        request_id: u64,
        directory: Node,
        name: &[u8],
    ) -> Result<protocol::Request, Error> {
        let mut request = self.request(protocol::operation::LOOKUP, request_id)?;
        request.node_id = directory.id_for(self)?;
        set_name(&mut request, name)?;
        Ok(request)
    }

    pub fn attach_buffer(
        self,
        request_id: u64,
        buffer_id: u64,
        length: usize,
    ) -> Result<protocol::Request, Error> {
        if buffer_id == protocol::INVALID_ID || length == 0 {
            return Err(Error::InvalidBuffer);
        }
        let mut request = self.request(protocol::operation::ATTACH_BUFFER, request_id)?;
        request.bulk = protocol::BulkBuffer {
            buffer_id,
            offset: 0,
            length: u64::try_from(length).map_err(|_| Error::Range)?,
        };
        Ok(request)
    }

    pub fn read(
        self,
        request_id: u64,
        node: Node,
        file_offset: u64,
        bulk: protocol::BulkBuffer,
    ) -> Result<protocol::Request, Error> {
        validate_bulk(bulk)?;
        let mut request = self.request(protocol::operation::READ, request_id)?;
        request.node_id = node.id_for(self)?;
        request.file_offset = file_offset;
        request.bulk = bulk;
        Ok(request)
    }

    pub fn write(
        self,
        request_id: u64,
        node: Node,
        file_offset: u64,
        bulk: protocol::BulkBuffer,
        flags: u32,
    ) -> Result<protocol::Request, Error> {
        validate_bulk(bulk)?;
        if flags & !protocol::request_flags::APPEND != 0 {
            return Err(Error::InvalidFlags);
        }
        let mut request = self.request(protocol::operation::WRITE, request_id)?;
        request.node_id = node.id_for(self)?;
        request.file_offset = file_offset;
        request.bulk = bulk;
        request.flags = flags;
        Ok(request)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    id: u64,
    session_id: u64,
    generation: u64,
}

impl Node {
    pub const fn root(session: Session) -> Self {
        Self {
            id: protocol::ROOT_NODE_ID,
            session_id: session.id,
            generation: session.generation,
        }
    }

    pub const fn from_reply(session: Session, reply: &protocol::Reply) -> Option<Self> {
        if reply.status == protocol::status::OK
            && reply.session_id == session.id
            && reply.generation == session.generation
            && reply.node_id != protocol::INVALID_ID
        {
            Some(Self {
                id: reply.node_id,
                session_id: session.id,
                generation: session.generation,
            })
        } else {
            None
        }
    }

    pub const fn id(self) -> u64 {
        self.id
    }

    fn id_for(self, session: Session) -> Result<u64, Error> {
        if self.id == protocol::INVALID_ID {
            Err(Error::InvalidNode)
        } else if self.session_id != session.id || self.generation != session.generation {
            Err(Error::InvalidSession)
        } else {
            Ok(self.id)
        }
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
    if ipc::send(
        service,
        bytes_of(&request),
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
    let reply = receive_reply(reply_endpoint, &request)?;
    let Some(mut session) = Session::from_reply(&reply) else {
        let _ = ipc::close(reply_endpoint);
        return Err(Error::Transport);
    };
    session.service = service;
    session.reply_endpoint = reply_endpoint;
    Ok(session)
}

pub fn valid_reply(request: &protocol::Request, reply: &protocol::Reply) -> bool {
    reply.version == protocol::VERSION
        && reply.operation == request.operation
        && reply.request_id == request.request_id
        && reply.data_length as usize <= protocol::MAX_INLINE_DATA_BYTES
        && reply.reserved == [0; 2]
        && (request.operation == protocol::operation::CONNECT
            || (reply.session_id == request.session_id && reply.generation == request.generation))
}

fn set_name(request: &mut protocol::Request, name: &[u8]) -> Result<(), Error> {
    if name.is_empty()
        || name.len() > protocol::MAX_NAME_BYTES
        || name.contains(&b'/')
        || name.contains(&0)
    {
        return Err(Error::InvalidName);
    }
    request.name_length = name.len() as u16;
    request.name[..name.len()].copy_from_slice(name);
    Ok(())
}

fn validate_bulk(bulk: protocol::BulkBuffer) -> Result<(), Error> {
    if bulk.buffer_id == protocol::INVALID_ID || bulk.length == 0 {
        return Err(Error::InvalidBuffer);
    }
    bulk.end().ok_or(Error::Range)?;
    Ok(())
}

fn receive_reply(
    endpoint: CapabilityHandle,
    request: &protocol::Request,
) -> Result<protocol::Reply, Error> {
    let mut bytes = [0_u8; size_of::<protocol::Reply>()];
    let message = ipc::receive(endpoint, &mut bytes).map_err(|_| Error::Transport)?;
    if message.capability.is_some() || message.bytes != bytes.len() {
        return Err(Error::Transport);
    }
    let reply = unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const protocol::Reply) };
    if !valid_reply(request, &reply) {
        return Err(Error::Transport);
    }
    match reply.status {
        protocol::status::OK => Ok(reply),
        protocol::status::NOT_FOUND => Err(Error::NotFound),
        protocol::status::STALE_SESSION => Err(Error::StaleSession),
        protocol::status::STALE_NODE => Err(Error::StaleNode),
        status => Err(Error::Service(status)),
    }
}

fn bytes_of<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

const _: () = assert!(size_of::<protocol::Request>() <= 256);
const _: () = assert!(size_of::<protocol::Reply>() <= 256);

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use super::{Error, Node, Session, connect, protocol, valid_reply};

    fn session() -> Session {
        let mut reply = protocol::Reply::EMPTY;
        reply.session_id = 7;
        reply.generation = 11;
        Session::from_reply(&reply).expect("valid session")
    }

    #[test]
    fn wire_records_fit_bounded_endpoint_messages() {
        assert!(size_of::<protocol::Request>() <= 256);
        assert!(size_of::<protocol::Reply>() <= 256);
        assert!(size_of::<protocol::NodeAttributes>() <= protocol::MAX_INLINE_DATA_BYTES);
    }

    #[test]
    fn lookup_is_relative_to_a_generation_scoped_directory() {
        let session = session();
        let request = session
            .lookup(3, Node::root(session), b"Library")
            .expect("valid lookup");
        assert_eq!(request.node_id, protocol::ROOT_NODE_ID);
        assert_eq!(&request.name[..request.name_length as usize], b"Library");

        let stale_session = Session {
            service: session.service,
            reply_endpoint: session.reply_endpoint,
            id: session.id(),
            generation: session.generation() + 1,
        };
        assert_eq!(
            stale_session.lookup(4, Node::root(session), b"Users"),
            Err(Error::InvalidSession)
        );
    }

    #[test]
    fn bulk_ranges_require_registered_ids_and_cannot_overflow() {
        let session = session();
        let node = Node::root(session);
        assert_eq!(
            session.read(2, node, 0, protocol::BulkBuffer::NONE),
            Err(Error::InvalidBuffer)
        );
        assert_eq!(
            session.read(
                2,
                node,
                0,
                protocol::BulkBuffer {
                    buffer_id: 1,
                    offset: u64::MAX,
                    length: 2,
                },
            ),
            Err(Error::Range)
        );
    }

    #[test]
    fn replies_are_bound_to_request_and_session_identity() {
        let session = session();
        let request = session
            .lookup(9, Node::root(session), b"Applications")
            .expect("valid lookup");
        let mut reply = protocol::Reply::EMPTY;
        reply.operation = request.operation;
        reply.request_id = request.request_id;
        reply.session_id = request.session_id;
        reply.generation = request.generation;
        assert!(valid_reply(&request, &reply));

        reply.request_id += 1;
        assert!(!valid_reply(&request, &reply));
        reply.request_id = request.request_id;
        reply.generation += 1;
        assert!(!valid_reply(&request, &reply));
    }

    #[test]
    fn connect_reserves_capability_transfer_for_the_reply_endpoint() {
        let request = connect(1).expect("valid connect");
        assert_eq!(request.operation, protocol::operation::CONNECT);
        assert_eq!(request.session_id, protocol::INVALID_ID);
        assert_eq!(connect(0), Err(Error::InvalidRequestId));
    }
}
