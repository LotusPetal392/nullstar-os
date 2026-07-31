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

impl Error {
    #[allow(non_upper_case_globals)]
    pub const OutcomeUnknown: Self = Self::Service(protocol::status::OUTCOME_UNKNOWN);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    service: CapabilityHandle,
    reply_endpoint: CapabilityHandle,
    id: u64,
    generation: u64,
    features: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryBatch {
    pub count: usize,
    pub end: bool,
}

impl Session {
    pub const fn from_reply(reply: &protocol::Reply) -> Option<Self> {
        if reply.status == protocol::status::OK
            && reply.session_id != protocol::INVALID_ID
            && reply.generation != 0
            && reply.value & !protocol::session_features::ALL == 0
        {
            Some(Self {
                service: 0,
                reply_endpoint: 0,
                id: reply.session_id,
                generation: reply.generation,
                features: reply.value,
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

    pub const fn features(self) -> u64 {
        self.features
    }

    pub const fn is_writable(self) -> bool {
        self.features & protocol::session_features::WRITE != 0
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

    pub fn detach_shared_buffer(self, request_id: u64, buffer_id: u64) -> Result<(), Error> {
        if buffer_id == protocol::INVALID_ID {
            return Err(Error::InvalidBuffer);
        }
        let mut request = self.request(protocol::operation::DETACH_BUFFER, request_id)?;
        request.bulk.buffer_id = buffer_id;
        self.exchange(&request, None).map(|_| ())
    }

    pub fn read_to_shared_buffer(
        self,
        request_id: u64,
        node: Node,
        file_offset: u64,
        bulk: protocol::BulkBuffer,
    ) -> Result<usize, Error> {
        let request = self.read(request_id, node, file_offset, bulk)?;
        let reply = self.exchange(&request, None)?;
        usize::try_from(reply.value).map_err(|_| Error::Range)
    }

    pub fn write_from_shared_buffer(
        self,
        request_id: u64,
        node: Node,
        file_offset: u64,
        bulk: protocol::BulkBuffer,
        append: bool,
    ) -> Result<usize, Error> {
        let flags = if append {
            protocol::request_flags::APPEND
        } else {
            0
        };
        let request = self.write(request_id, node, file_offset, bulk, flags)?;
        let reply = self.mutation_exchange(&request, None)?;
        write_count(&reply, bulk)
    }

    pub fn create_file(
        self,
        request_id: u64,
        directory: Node,
        name: &[u8],
        exclusive: bool,
        truncate: bool,
    ) -> Result<Node, Error> {
        let request = self.create_file_request(request_id, directory, name, exclusive, truncate)?;
        let reply = self.mutation_exchange(&request, None)?;
        node_from_reply_with_kind(self, &reply, protocol::node_kind::FILE)
            .ok_or(Error::OutcomeUnknown)
    }

    pub fn create_directory(
        self,
        request_id: u64,
        directory: Node,
        name: &[u8],
    ) -> Result<Node, Error> {
        let request = self.create_directory_request(request_id, directory, name)?;
        let reply = self.mutation_exchange(&request, None)?;
        node_from_reply_with_kind(self, &reply, protocol::node_kind::DIRECTORY)
            .ok_or(Error::OutcomeUnknown)
    }

    pub fn truncate(self, request_id: u64, node: Node, size: u64) -> Result<(), Error> {
        let request = self.truncate_request(request_id, node, size)?;
        self.mutation_exchange(&request, None).map(|_| ())
    }

    pub fn rmdir(self, request_id: u64, directory: Node, name: &[u8]) -> Result<(), Error> {
        let request = self.rmdir_request(request_id, directory, name)?;
        self.mutation_exchange(&request, None).map(|_| ())
    }

    pub fn rename(
        self,
        request_id: u64,
        old_directory: Node,
        old_name: &[u8],
        new_directory: Node,
        new_name: protocol::BulkBuffer,
    ) -> Result<(), Error> {
        let request =
            self.rename_request(request_id, old_directory, old_name, new_directory, new_name)?;
        self.mutation_exchange(&request, None).map(|_| ())
    }

    pub fn sync(self, request_id: u64) -> Result<(), Error> {
        let request = self.sync_request(request_id)?;
        self.mutation_exchange(&request, None).map(|_| ())
    }

    pub fn open_node(self, request_id: u64, node: Node, flags: u32) -> Result<Node, Error> {
        let allowed = protocol::request_flags::READ
            | protocol::request_flags::WRITE
            | protocol::request_flags::APPEND
            | protocol::request_flags::TRUNCATE;
        if flags & !allowed != 0
            || flags & (protocol::request_flags::APPEND | protocol::request_flags::TRUNCATE) != 0
                && flags & protocol::request_flags::WRITE == 0
        {
            return Err(Error::InvalidFlags);
        }
        let mut request = self.request(protocol::operation::OPEN, request_id)?;
        request.node_id = node.id_for(self)?;
        request.flags = flags;
        let reply = if flags & protocol::request_flags::TRUNCATE != 0 {
            self.mutation_exchange(&request, None)?
        } else {
            self.exchange(&request, None)?
        };
        Node::from_reply(self, &reply).ok_or(if flags & protocol::request_flags::TRUNCATE != 0 {
            Error::OutcomeUnknown
        } else {
            Error::Transport
        })
    }

    pub fn close_node(self, request_id: u64, node: Node) -> Result<(), Error> {
        let request = self.close_node_request(request_id, node)?;
        self.mutation_exchange(&request, None).map(|_| ())
    }

    pub fn close_node_request(
        self,
        request_id: u64,
        node: Node,
    ) -> Result<protocol::Request, Error> {
        let mut request = self.request(protocol::operation::CLOSE_NODE, request_id)?;
        request.node_id = node.id_for(self)?;
        Ok(request)
    }

    pub fn read_directory_to_shared_buffer(
        self,
        request_id: u64,
        directory: Node,
        cookie: u64,
        bulk: protocol::BulkBuffer,
    ) -> Result<DirectoryBatch, Error> {
        validate_bulk(bulk)?;
        let mut request = self.request(protocol::operation::READ_DIRECTORY, request_id)?;
        request.node_id = directory.id_for(self)?;
        request.file_offset = cookie;
        request.bulk = bulk;
        let reply = self.exchange(&request, None)?;
        Ok(DirectoryBatch {
            count: usize::try_from(reply.value).map_err(|_| Error::Range)?,
            end: reply.flags & protocol::reply_flags::END_OF_DIRECTORY != 0,
        })
    }

    pub fn unlink(self, request_id: u64, directory: Node, name: &[u8]) -> Result<(), Error> {
        let mut request = self.request(protocol::operation::UNLINK, request_id)?;
        request.node_id = directory.id_for(self)?;
        set_name(&mut request, name)?;
        self.mutation_exchange(&request, None).map(|_| ())
    }

    pub fn disconnect(self, request_id: u64) -> Result<(), Error> {
        let request = self.request(protocol::operation::DISCONNECT, request_id)?;
        let result = self.mutation_exchange(&request, None).map(|_| ());
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

    fn mutation_exchange(
        self,
        request: &protocol::Request,
        transfer: Option<Transfer>,
    ) -> Result<protocol::Reply, Error> {
        if self.service == 0 || self.reply_endpoint == 0 {
            return Err(Error::Transport);
        }
        ipc::send(self.service, bytes_of(request), transfer).map_err(|_| Error::Transport)?;
        mutation_reply_result(receive_reply(self.reply_endpoint, request))
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

    pub fn create_file_request(
        self,
        request_id: u64,
        directory: Node,
        name: &[u8],
        exclusive: bool,
        truncate: bool,
    ) -> Result<protocol::Request, Error> {
        let mut request = self.request(protocol::operation::CREATE_FILE, request_id)?;
        request.node_id = directory.id_for(self)?;
        request.flags = if exclusive {
            protocol::request_flags::EXCLUSIVE
        } else {
            0
        } | if truncate {
            protocol::request_flags::TRUNCATE
        } else {
            0
        };
        set_name(&mut request, name)?;
        Ok(request)
    }

    pub fn create_directory_request(
        self,
        request_id: u64,
        directory: Node,
        name: &[u8],
    ) -> Result<protocol::Request, Error> {
        let mut request = self.request(protocol::operation::CREATE_DIRECTORY, request_id)?;
        request.node_id = directory.id_for(self)?;
        set_name(&mut request, name)?;
        Ok(request)
    }

    pub fn truncate_request(
        self,
        request_id: u64,
        node: Node,
        size: u64,
    ) -> Result<protocol::Request, Error> {
        let mut request = self.request(protocol::operation::TRUNCATE, request_id)?;
        request.node_id = node.id_for(self)?;
        request.file_offset = size;
        Ok(request)
    }

    pub fn rmdir_request(
        self,
        request_id: u64,
        directory: Node,
        name: &[u8],
    ) -> Result<protocol::Request, Error> {
        let mut request = self.request(protocol::operation::RMDIR, request_id)?;
        request.node_id = directory.id_for(self)?;
        set_name(&mut request, name)?;
        Ok(request)
    }

    pub fn rename_request(
        self,
        request_id: u64,
        old_directory: Node,
        old_name: &[u8],
        new_directory: Node,
        new_name: protocol::BulkBuffer,
    ) -> Result<protocol::Request, Error> {
        validate_rename_bulk(new_name)?;
        let mut request = self.request(protocol::operation::RENAME, request_id)?;
        request.node_id = old_directory.id_for(self)?;
        request.secondary_node_id = new_directory.id_for(self)?;
        request.bulk = new_name;
        set_name(&mut request, old_name)?;
        Ok(request)
    }

    pub fn sync_request(self, request_id: u64) -> Result<protocol::Request, Error> {
        self.request(protocol::operation::SYNC, request_id)
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
    connect_with_flags(request_id, 0)
}

pub fn connect_writable(request_id: u64) -> Result<protocol::Request, Error> {
    connect_with_flags(request_id, protocol::connect_flags::WRITE)
}

pub fn connect_with_flags(request_id: u64, flags: u32) -> Result<protocol::Request, Error> {
    if request_id == protocol::INVALID_ID {
        return Err(Error::InvalidRequestId);
    }
    if flags & !protocol::connect_flags::ALL != 0 {
        return Err(Error::InvalidFlags);
    }
    let mut request = protocol::Request::EMPTY;
    request.operation = protocol::operation::CONNECT;
    request.request_id = request_id;
    request.flags = flags;
    Ok(request)
}

pub fn connect_service(service: CapabilityHandle, request_id: u64) -> Result<Session, Error> {
    connect_service_with_request(service, connect(request_id)?)
}

pub fn connect_writable_service(
    service: CapabilityHandle,
    request_id: u64,
) -> Result<Session, Error> {
    connect_service_with_request(service, connect_writable(request_id)?)
}

fn connect_service_with_request(
    service: CapabilityHandle,
    request: protocol::Request,
) -> Result<Session, Error> {
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
    let reply = match receive_reply(reply_endpoint, &request) {
        Ok(reply) => reply,
        Err(error) => {
            let _ = ipc::close(reply_endpoint);
            return Err(error);
        }
    };
    let Some(mut session) = negotiated_session(&request, &reply) else {
        let _ = ipc::close(reply_endpoint);
        return Err(Error::Transport);
    };
    session.service = service;
    session.reply_endpoint = reply_endpoint;
    Ok(session)
}

fn negotiated_session(request: &protocol::Request, reply: &protocol::Reply) -> Option<Session> {
    if request.operation != protocol::operation::CONNECT || !valid_reply(request, reply) {
        return None;
    }
    let expected_features = match request.flags {
        0 => 0,
        protocol::connect_flags::WRITE => protocol::session_features::WRITE,
        _ => return None,
    };
    let session = Session::from_reply(reply)?;
    (session.features == expected_features).then_some(session)
}

pub fn valid_reply(request: &protocol::Request, reply: &protocol::Reply) -> bool {
    if reply.version != protocol::VERSION
        || reply.operation != request.operation
        || reply.request_id != request.request_id
        || reply.data_length as usize > protocol::MAX_INLINE_DATA_BYTES
        || reply.flags & !protocol::reply_flags::ALL != 0
        || reply.reserved != [0; 2]
        || !defined_status(reply.status)
        || (request.operation != protocol::operation::CONNECT
            || reply.status != protocol::status::OK)
            && (reply.session_id != request.session_id || reply.generation != request.generation)
    {
        return false;
    }

    if reply.status != protocol::status::OK {
        return canonical_empty_reply(reply);
    }

    match request.operation {
        protocol::operation::CONNECT => canonical_connect_reply(request, reply),
        protocol::operation::ATTACH_BUFFER
        | protocol::operation::DETACH_BUFFER
        | protocol::operation::UNLINK
        | protocol::operation::CLOSE_NODE
        | protocol::operation::DISCONNECT
        | protocol::operation::TRUNCATE
        | protocol::operation::RMDIR
        | protocol::operation::RENAME
        | protocol::operation::SYNC => canonical_empty_reply(reply),
        protocol::operation::LOOKUP | protocol::operation::OPEN => {
            canonical_node_reply(reply, None)
        }
        protocol::operation::GET_ATTRIBUTES => canonical_attributes_reply(request, reply),
        protocol::operation::READ => canonical_count_reply(request, reply),
        protocol::operation::WRITE => canonical_write_reply(request, reply),
        protocol::operation::READ_DIRECTORY => canonical_directory_reply(request, reply),
        protocol::operation::CREATE_FILE => {
            canonical_node_reply(reply, Some(protocol::node_kind::FILE))
        }
        protocol::operation::CREATE_DIRECTORY => {
            canonical_node_reply(reply, Some(protocol::node_kind::DIRECTORY))
        }
        _ => false,
    }
}

fn defined_status(status: i32) -> bool {
    matches!(
        status,
        protocol::status::OK
            | protocol::status::INVALID
            | protocol::status::NOT_FOUND
            | protocol::status::NOT_DIRECTORY
            | protocol::status::IS_DIRECTORY
            | protocol::status::EXISTS
            | protocol::status::PERMISSION
            | protocol::status::NO_SPACE
            | protocol::status::RANGE
            | protocol::status::STALE_SESSION
            | protocol::status::STALE_NODE
            | protocol::status::STALE_BUFFER
            | protocol::status::TRY_AGAIN
            | protocol::status::IO
            | protocol::status::NOT_SUPPORTED
            | protocol::status::CANCELLED
            | protocol::status::NOT_EMPTY
            | protocol::status::WOULD_CYCLE
            | protocol::status::OUTCOME_UNKNOWN
    )
}

fn canonical_connect_reply(request: &protocol::Request, reply: &protocol::Reply) -> bool {
    let expected_features = match request.flags {
        0 => 0,
        protocol::connect_flags::WRITE => protocol::session_features::WRITE,
        _ => return false,
    };
    reply.flags == 0
        && reply.session_id != protocol::INVALID_ID
        && reply.generation != 0
        && reply.node_id == protocol::ROOT_NODE_ID
        && reply.value == expected_features
        && reply.data_length == 0
        && reply.node_kind == protocol::node_kind::DIRECTORY
        && reply.data == [0; protocol::MAX_INLINE_DATA_BYTES]
}

fn canonical_empty_reply(reply: &protocol::Reply) -> bool {
    canonical_empty_payload(reply) && reply.value == 0
}

fn canonical_empty_payload(reply: &protocol::Reply) -> bool {
    reply.flags == 0
        && reply.node_id == protocol::INVALID_ID
        && reply.data_length == 0
        && reply.node_kind == protocol::node_kind::UNKNOWN
        && reply.data == [0; protocol::MAX_INLINE_DATA_BYTES]
}

fn canonical_node_reply(reply: &protocol::Reply, expected_kind: Option<u16>) -> bool {
    reply.flags == 0
        && reply.node_id != protocol::INVALID_ID
        && reply.data_length == 0
        && defined_node_kind(reply.node_kind)
        && expected_kind.is_none_or(|kind| reply.node_kind == kind)
        && reply.data == [0; protocol::MAX_INLINE_DATA_BYTES]
}

fn canonical_attributes_reply(request: &protocol::Request, reply: &protocol::Reply) -> bool {
    if reply.flags != 0
        || reply.node_id == protocol::INVALID_ID
        || reply.node_id != request.node_id
        || reply.value != 0
        || usize::from(reply.data_length) != size_of::<protocol::NodeAttributes>()
        || !defined_node_kind(reply.node_kind)
    {
        return false;
    }
    let attributes = unsafe {
        core::ptr::read_unaligned(reply.data.as_ptr() as *const protocol::NodeAttributes)
    };
    attributes.node_id == reply.node_id && attributes.kind == reply.node_kind
}

fn canonical_count_reply(request: &protocol::Request, reply: &protocol::Reply) -> bool {
    canonical_empty_payload(reply) && reply.value <= request.bulk.length
}

fn canonical_write_reply(request: &protocol::Request, reply: &protocol::Reply) -> bool {
    if reply.flags != 0
        || reply.node_id != protocol::INVALID_ID
        || reply.node_kind != protocol::node_kind::UNKNOWN
        || reply.value > request.bulk.length
    {
        return false;
    }
    let Some(resulting_offset) = protocol::decode_write_reply_offset(reply) else {
        return false;
    };

    match request.flags {
        0 => request.file_offset.checked_add(reply.value) == Some(resulting_offset),
        protocol::request_flags::APPEND => resulting_offset.checked_sub(reply.value).is_some(),
        _ => false,
    }
}

fn canonical_directory_reply(request: &protocol::Request, reply: &protocol::Reply) -> bool {
    let entry_bytes = size_of::<protocol::DirectoryEntry>() as u64;
    reply.flags & !protocol::reply_flags::END_OF_DIRECTORY == 0
        && reply.node_id == protocol::INVALID_ID
        && reply.value <= request.bulk.length / entry_bytes
        && reply.data_length == 0
        && reply.node_kind == protocol::node_kind::UNKNOWN
        && reply.data == [0; protocol::MAX_INLINE_DATA_BYTES]
}

fn defined_node_kind(kind: u16) -> bool {
    matches!(
        kind,
        protocol::node_kind::FILE
            | protocol::node_kind::DIRECTORY
            | protocol::node_kind::SYMBOLIC_LINK
    )
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

fn validate_rename_bulk(bulk: protocol::BulkBuffer) -> Result<(), Error> {
    if bulk.length == 0 || bulk.length > protocol::MAX_NAME_BYTES as u64 {
        return Err(Error::InvalidName);
    }
    validate_bulk(bulk)
}

fn node_from_reply_with_kind(
    session: Session,
    reply: &protocol::Reply,
    expected_kind: u16,
) -> Option<Node> {
    if reply.node_kind != expected_kind {
        return None;
    }
    Node::from_reply(session, reply)
}

fn write_count(reply: &protocol::Reply, bulk: protocol::BulkBuffer) -> Result<usize, Error> {
    if reply.value > bulk.length {
        return Err(Error::OutcomeUnknown);
    }
    usize::try_from(reply.value).map_err(|_| Error::Range)
}

fn mutation_reply_result(result: Result<protocol::Reply, Error>) -> Result<protocol::Reply, Error> {
    match result {
        Err(Error::Transport) => Err(Error::OutcomeUnknown),
        result => result,
    }
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
        protocol::status::OUTCOME_UNKNOWN => Err(Error::OutcomeUnknown),
        protocol::status::INVALID
        | protocol::status::NOT_DIRECTORY
        | protocol::status::IS_DIRECTORY
        | protocol::status::EXISTS
        | protocol::status::PERMISSION
        | protocol::status::NO_SPACE
        | protocol::status::RANGE
        | protocol::status::STALE_BUFFER
        | protocol::status::TRY_AGAIN
        | protocol::status::IO
        | protocol::status::NOT_SUPPORTED
        | protocol::status::CANCELLED
        | protocol::status::NOT_EMPTY
        | protocol::status::WOULD_CYCLE => Err(Error::Service(reply.status)),
        _ => Err(Error::Transport),
    }
}

fn bytes_of<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

const _: () = assert!(size_of::<protocol::Request>() == 184);
const _: () = assert!(size_of::<protocol::Reply>() == 136);

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use super::{
        Error, Node, Session, connect, connect_with_flags, connect_writable, mutation_reply_result,
        negotiated_session, node_from_reply_with_kind, protocol, valid_reply, write_count,
    };

    fn session() -> Session {
        let mut reply = protocol::Reply::EMPTY;
        reply.session_id = 7;
        reply.generation = 11;
        Session::from_reply(&reply).expect("valid session")
    }

    fn reply_for(request: &protocol::Request) -> protocol::Reply {
        let mut reply = protocol::Reply::EMPTY;
        reply.operation = request.operation;
        reply.request_id = request.request_id;
        reply.session_id = request.session_id;
        reply.generation = request.generation;
        reply
    }

    #[test]
    fn wire_records_have_exact_stable_sizes() {
        assert_eq!(size_of::<protocol::Request>(), 184);
        assert_eq!(size_of::<protocol::Reply>(), 136);
        assert_eq!(size_of::<protocol::NodeAttributes>(), 64);
        assert_eq!(size_of::<protocol::DirectoryEntry>(), 120);
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
            features: session.features(),
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
        reply.node_id = 42;
        reply.node_kind = protocol::node_kind::FILE;
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
        assert_eq!(request.flags, 0);

        let writable = connect_writable(2).expect("valid writable connect");
        assert_eq!(writable.flags, protocol::connect_flags::WRITE);
        assert_eq!(
            connect_with_flags(3, protocol::connect_flags::WRITE | 1),
            Err(Error::InvalidFlags)
        );
        assert_eq!(connect(0), Err(Error::InvalidRequestId));
    }

    #[test]
    fn writable_connect_requires_explicit_feature_negotiation() {
        let read_only_request = connect(1).expect("read-only request");
        let mut reply = protocol::Reply::EMPTY;
        reply.operation = read_only_request.operation;
        reply.request_id = read_only_request.request_id;
        reply.session_id = 7;
        reply.generation = 11;
        reply.node_id = protocol::ROOT_NODE_ID;
        reply.node_kind = protocol::node_kind::DIRECTORY;
        let read_only = negotiated_session(&read_only_request, &reply).expect("read-only session");
        assert_eq!(read_only.features(), 0);
        assert!(!read_only.is_writable());

        let writable_request = connect_writable(2).expect("writable request");
        reply.request_id = writable_request.request_id;
        assert!(negotiated_session(&writable_request, &reply).is_none());
        reply.value = protocol::session_features::WRITE;
        let writable = negotiated_session(&writable_request, &reply).expect("writable session");
        assert_eq!(writable.features(), protocol::session_features::WRITE);
        assert!(writable.is_writable());

        reply.value = protocol::session_features::ALL | (1 << 63);
        assert!(negotiated_session(&writable_request, &reply).is_none());
    }

    #[test]
    fn create_open_and_directory_requests_preserve_typed_identity() {
        let session = session();
        let root = Node::root(session);
        let create = session
            .create_file(1, root, b"notes.txt", true, false)
            .unwrap_err();
        assert_eq!(create, Error::Transport);

        assert_eq!(
            session.open_node(2, root, protocol::request_flags::APPEND),
            Err(Error::InvalidFlags)
        );

        let request = session
            .request(protocol::operation::READ_DIRECTORY, 3)
            .expect("valid directory request");
        assert_eq!(request.session_id, session.id());
        assert_eq!(request.generation, session.generation());
    }

    #[test]
    fn mutation_request_builders_use_canonical_fields() {
        let session = session();
        let old_parent = Node::root(session);
        let new_parent = Node {
            id: 42,
            session_id: session.id(),
            generation: session.generation(),
        };

        let create = session
            .create_directory_request(10, old_parent, b"documents")
            .expect("create directory request");
        assert_eq!(create.operation, protocol::operation::CREATE_DIRECTORY);
        assert_eq!(create.node_id, old_parent.id());
        assert_eq!(&create.name[..create.name_length as usize], b"documents");

        let truncate = session
            .truncate_request(11, new_parent, u64::MAX)
            .expect("truncate request");
        assert_eq!(truncate.operation, protocol::operation::TRUNCATE);
        assert_eq!(truncate.node_id, new_parent.id());
        assert_eq!(truncate.file_offset, u64::MAX);

        let rmdir = session
            .rmdir_request(12, old_parent, b"empty")
            .expect("rmdir request");
        assert_eq!(rmdir.operation, protocol::operation::RMDIR);
        assert_eq!(&rmdir.name[..rmdir.name_length as usize], b"empty");

        let sync = session.sync_request(13).expect("sync request");
        let mut expected = protocol::Request::EMPTY;
        expected.operation = protocol::operation::SYNC;
        expected.request_id = 13;
        expected.session_id = session.id();
        expected.generation = session.generation();
        assert_eq!(sync, expected);
    }

    #[test]
    fn rename_supports_two_maximum_length_names_and_checks_bulk_bounds() {
        let session = session();
        let old_parent = Node::root(session);
        let new_parent = Node {
            id: 42,
            session_id: session.id(),
            generation: session.generation(),
        };
        let old_name = [b'o'; protocol::MAX_NAME_BYTES];
        let new_name = [b'n'; protocol::MAX_NAME_BYTES];
        let bulk = protocol::BulkBuffer {
            buffer_id: 3,
            offset: 128,
            length: new_name.len() as u64,
        };
        let rename = session
            .rename_request(14, old_parent, &old_name, new_parent, bulk)
            .expect("rename request");
        assert_eq!(rename.operation, protocol::operation::RENAME);
        assert_eq!(rename.node_id, old_parent.id());
        assert_eq!(rename.secondary_node_id, new_parent.id());
        assert_eq!(rename.bulk, bulk);
        assert_eq!(rename.name_length as usize, old_name.len());
        assert_eq!(&rename.name, &old_name);

        assert_eq!(
            session.rename_request(
                15,
                old_parent,
                &old_name,
                new_parent,
                protocol::BulkBuffer { length: 0, ..bulk },
            ),
            Err(Error::InvalidName)
        );
        assert_eq!(
            session.rename_request(
                16,
                old_parent,
                &old_name,
                new_parent,
                protocol::BulkBuffer {
                    length: protocol::MAX_NAME_BYTES as u64 + 1,
                    ..bulk
                },
            ),
            Err(Error::InvalidName)
        );
        assert_eq!(
            session.rename_request(
                17,
                old_parent,
                &old_name,
                new_parent,
                protocol::BulkBuffer {
                    offset: u64::MAX - 1,
                    length: new_name.len() as u64,
                    ..bulk
                },
            ),
            Err(Error::Range)
        );
    }

    #[test]
    fn create_kind_write_count_and_mutation_outcome_are_validated() {
        let session = session();
        let mut reply = protocol::Reply::EMPTY;
        reply.session_id = session.id();
        reply.generation = session.generation();
        reply.node_id = 42;
        reply.node_kind = protocol::node_kind::FILE;
        assert!(node_from_reply_with_kind(session, &reply, protocol::node_kind::FILE).is_some());
        assert!(
            node_from_reply_with_kind(session, &reply, protocol::node_kind::DIRECTORY).is_none()
        );

        let bulk = protocol::BulkBuffer {
            buffer_id: 1,
            offset: 0,
            length: 64,
        };
        reply.value = bulk.length;
        assert_eq!(write_count(&reply, bulk), Ok(64));
        reply.value += 1;
        assert_eq!(write_count(&reply, bulk), Err(Error::OutcomeUnknown));

        assert_eq!(
            mutation_reply_result(Err(Error::Transport)),
            Err(Error::OutcomeUnknown)
        );
        assert_eq!(
            mutation_reply_result(Err(Error::NotFound)),
            Err(Error::NotFound)
        );
        assert_eq!(
            Error::OutcomeUnknown,
            Error::Service(protocol::status::OUTCOME_UNKNOWN)
        );
    }

    #[test]
    fn close_node_request_preserves_typed_identity() {
        let session = session();
        let node = Node {
            id: 42,
            session_id: session.id(),
            generation: session.generation(),
        };
        let request = session
            .close_node_request(5, node)
            .expect("valid close request");
        let mut expected = protocol::Request::EMPTY;
        expected.operation = protocol::operation::CLOSE_NODE;
        expected.request_id = 5;
        expected.session_id = session.id();
        expected.generation = session.generation();
        expected.node_id = 42;
        assert_eq!(request, expected);

        let stale_node = Node {
            generation: session.generation() + 1,
            ..node
        };
        assert_eq!(
            session.close_node_request(6, stale_node),
            Err(Error::InvalidSession)
        );
        assert_eq!(
            session.close_node(7, stale_node),
            Err(Error::InvalidSession)
        );
    }

    #[test]
    fn canonical_error_replies_reject_payload_flags_and_undefined_statuses() {
        let session = session();
        let request = session
            .lookup(20, Node::root(session), b"missing")
            .expect("lookup request");
        let mut reply = reply_for(&request);
        reply.status = protocol::status::NOT_FOUND;
        assert!(valid_reply(&request, &reply));

        reply.flags = protocol::reply_flags::END_OF_DIRECTORY;
        assert!(!valid_reply(&request, &reply));
        reply.flags = 0;
        reply.node_id = 42;
        assert!(!valid_reply(&request, &reply));
        reply.node_id = protocol::INVALID_ID;
        reply.value = 1;
        assert!(!valid_reply(&request, &reply));
        reply.value = 0;
        reply.data_length = 1;
        assert!(!valid_reply(&request, &reply));
        reply.data_length = 0;
        reply.node_kind = protocol::node_kind::FILE;
        assert!(!valid_reply(&request, &reply));
        reply.node_kind = protocol::node_kind::UNKNOWN;
        reply.data[0] = 1;
        assert!(!valid_reply(&request, &reply));
        reply.data[0] = 0;
        reply.status = protocol::status::OUTCOME_UNKNOWN + 1;
        assert!(!valid_reply(&request, &reply));
    }

    #[test]
    fn node_success_replies_have_operation_specific_kinds_and_empty_data() {
        let session = session();
        for operation in [protocol::operation::LOOKUP, protocol::operation::OPEN] {
            let request = session.request(operation, u64::from(operation)).unwrap();
            let mut reply = reply_for(&request);
            reply.node_id = 42;
            reply.node_kind = protocol::node_kind::SYMBOLIC_LINK;
            reply.value = 99;
            assert!(valid_reply(&request, &reply));

            reply.node_kind = protocol::node_kind::UNKNOWN;
            assert!(!valid_reply(&request, &reply));
        }

        for (operation, expected_kind) in [
            (protocol::operation::CREATE_FILE, protocol::node_kind::FILE),
            (
                protocol::operation::CREATE_DIRECTORY,
                protocol::node_kind::DIRECTORY,
            ),
        ] {
            let request = session.request(operation, u64::from(operation)).unwrap();
            let mut reply = reply_for(&request);
            reply.node_id = 42;
            reply.node_kind = expected_kind;
            assert!(valid_reply(&request, &reply));

            reply.node_kind = if expected_kind == protocol::node_kind::FILE {
                protocol::node_kind::DIRECTORY
            } else {
                protocol::node_kind::FILE
            };
            assert!(!valid_reply(&request, &reply));
            reply.node_kind = expected_kind;
            reply.data[0] = 1;
            assert!(!valid_reply(&request, &reply));
        }
    }

    #[test]
    fn read_counts_are_bounded_and_payload_free() {
        let session = session();
        let bulk = protocol::BulkBuffer {
            buffer_id: 1,
            offset: 0,
            length: 64,
        };
        let request = session.read(30, Node::root(session), 0, bulk).unwrap();
        let mut reply = reply_for(&request);
        reply.value = bulk.length;
        assert!(valid_reply(&request, &reply));

        reply.value += 1;
        assert!(!valid_reply(&request, &reply));
        reply.value = bulk.length;
        protocol::encode_write_reply_offset(&mut reply, bulk.length);
        assert!(!valid_reply(&request, &reply));
    }

    #[test]
    fn non_append_write_replies_require_exact_offset_payload_and_bounded_count() {
        let session = session();
        let bulk = protocol::BulkBuffer {
            buffer_id: 1,
            offset: 0,
            length: 64,
        };
        let request = session
            .write(31, Node::root(session), 100, bulk, 0)
            .unwrap();
        let mut reply = reply_for(&request);
        reply.value = 16;
        protocol::encode_write_reply_offset(&mut reply, 116);
        assert!(valid_reply(&request, &reply));
        assert_eq!(
            &reply.data[..protocol::WRITE_REPLY_OFFSET_BYTES],
            &116_u64.to_le_bytes()
        );
        assert!(
            reply.data[protocol::WRITE_REPLY_OFFSET_BYTES..]
                .iter()
                .all(|byte| *byte == 0)
        );

        reply.data_length = (protocol::WRITE_REPLY_OFFSET_BYTES - 1) as u16;
        assert!(!valid_reply(&request, &reply));
        reply.data_length = (protocol::WRITE_REPLY_OFFSET_BYTES + 1) as u16;
        assert!(!valid_reply(&request, &reply));
        reply.data_length = protocol::WRITE_REPLY_OFFSET_BYTES as u16;
        reply.data[protocol::WRITE_REPLY_OFFSET_BYTES] = 1;
        assert!(!valid_reply(&request, &reply));

        protocol::encode_write_reply_offset(&mut reply, 117);
        assert!(!valid_reply(&request, &reply));

        reply.value = bulk.length + 1;
        let oversized_resulting_offset = request.file_offset + reply.value;
        protocol::encode_write_reply_offset(&mut reply, oversized_resulting_offset);
        assert!(!valid_reply(&request, &reply));

        let overflow_request = session
            .write(32, Node::root(session), u64::MAX, bulk, 0)
            .unwrap();
        let mut overflow_reply = reply_for(&overflow_request);
        overflow_reply.value = 1;
        protocol::encode_write_reply_offset(&mut overflow_reply, u64::MAX);
        assert!(!valid_reply(&overflow_request, &overflow_reply));
    }

    #[test]
    fn append_write_replies_carry_a_sane_resulting_offset() {
        let session = session();
        let bulk = protocol::BulkBuffer {
            buffer_id: 1,
            offset: 0,
            length: 64,
        };
        let request = session
            .write(
                33,
                Node::root(session),
                u64::MAX,
                bulk,
                protocol::request_flags::APPEND,
            )
            .unwrap();
        let mut reply = reply_for(&request);
        reply.value = 16;
        protocol::encode_write_reply_offset(&mut reply, 1_016);
        assert!(valid_reply(&request, &reply));
        assert_eq!(protocol::decode_write_reply_offset(&reply), Some(1_016));

        let invalid_resulting_offset = reply.value - 1;
        protocol::encode_write_reply_offset(&mut reply, invalid_resulting_offset);
        assert!(!valid_reply(&request, &reply));
    }

    #[test]
    fn write_errors_remain_canonical_empty_replies() {
        let session = session();
        let bulk = protocol::BulkBuffer {
            buffer_id: 1,
            offset: 0,
            length: 64,
        };
        let request = session
            .write(34, Node::root(session), 100, bulk, 0)
            .unwrap();
        let mut reply = reply_for(&request);
        reply.status = protocol::status::OUTCOME_UNKNOWN;
        assert!(valid_reply(&request, &reply));

        protocol::encode_write_reply_offset(&mut reply, 100);
        assert!(!valid_reply(&request, &reply));
    }

    #[test]
    fn directory_reply_count_fits_whole_entry_capacity() {
        let session = session();
        let entry_bytes = size_of::<protocol::DirectoryEntry>() as u64;
        let mut request = session
            .request(protocol::operation::READ_DIRECTORY, 40)
            .unwrap();
        request.bulk = protocol::BulkBuffer {
            buffer_id: 1,
            offset: 0,
            length: 2 * entry_bytes + entry_bytes - 1,
        };
        let mut reply = reply_for(&request);
        reply.value = 2;
        reply.flags = protocol::reply_flags::END_OF_DIRECTORY;
        assert!(valid_reply(&request, &reply));

        reply.value = 3;
        assert!(!valid_reply(&request, &reply));
        reply.value = 2;
        reply.node_kind = protocol::node_kind::DIRECTORY;
        assert!(!valid_reply(&request, &reply));
    }

    #[test]
    fn attributes_success_has_exact_mirrored_shape() {
        let session = session();
        let mut request = session
            .request(protocol::operation::GET_ATTRIBUTES, 50)
            .unwrap();
        request.node_id = 42;
        let mut attributes = protocol::NodeAttributes::EMPTY;
        attributes.node_id = request.node_id;
        attributes.kind = protocol::node_kind::DIRECTORY;
        let mut reply = reply_for(&request);
        reply.node_id = attributes.node_id;
        reply.node_kind = attributes.kind;
        reply.data_length = size_of::<protocol::NodeAttributes>() as u16;
        reply.data.copy_from_slice(super::bytes_of(&attributes));
        assert!(valid_reply(&request, &reply));

        reply.node_id += 1;
        assert!(!valid_reply(&request, &reply));
        reply.node_id = attributes.node_id;
        reply.node_kind = protocol::node_kind::FILE;
        assert!(!valid_reply(&request, &reply));
        reply.node_kind = attributes.kind;
        reply.data_length -= 1;
        assert!(!valid_reply(&request, &reply));
    }

    #[test]
    fn empty_success_operations_reject_all_payload() {
        let session = session();
        for operation in [
            protocol::operation::ATTACH_BUFFER,
            protocol::operation::DETACH_BUFFER,
            protocol::operation::UNLINK,
            protocol::operation::CLOSE_NODE,
            protocol::operation::DISCONNECT,
            protocol::operation::TRUNCATE,
            protocol::operation::RMDIR,
            protocol::operation::RENAME,
            protocol::operation::SYNC,
        ] {
            let request = session.request(operation, u64::from(operation)).unwrap();
            let mut reply = reply_for(&request);
            assert!(valid_reply(&request, &reply));

            reply.value = 1;
            assert!(!valid_reply(&request, &reply));
        }
    }

    #[test]
    fn close_node_replies_are_canonical() {
        let session = session();
        let node = Node {
            id: 42,
            session_id: session.id(),
            generation: session.generation(),
        };
        let request = session.close_node_request(5, node).expect("close request");
        let mut reply = reply_for(&request);
        assert!(valid_reply(&request, &reply));

        reply.flags = protocol::reply_flags::END_OF_DIRECTORY;
        assert!(!valid_reply(&request, &reply));
        reply.flags = 0;
        reply.node_id = 42;
        assert!(!valid_reply(&request, &reply));
        reply.node_id = protocol::INVALID_ID;
        reply.data_length = 1;
        assert!(!valid_reply(&request, &reply));
    }

    #[test]
    fn directory_completion_is_the_only_defined_reply_flag() {
        let session = session();
        let mut request = session
            .request(protocol::operation::READ_DIRECTORY, 4)
            .expect("valid request");
        request.bulk = protocol::BulkBuffer {
            buffer_id: 1,
            offset: 0,
            length: size_of::<protocol::DirectoryEntry>() as u64,
        };
        let mut reply = protocol::Reply::EMPTY;
        reply.operation = request.operation;
        reply.request_id = request.request_id;
        reply.session_id = request.session_id;
        reply.generation = request.generation;
        reply.flags = protocol::reply_flags::END_OF_DIRECTORY;
        assert!(valid_reply(&request, &reply));

        reply.flags |= 1 << 31;
        assert!(!valid_reply(&request, &reply));
    }

    #[test]
    fn end_of_directory_is_rejected_on_non_directory_success() {
        let session = session();
        let request = session
            .request(protocol::operation::LOOKUP, 5)
            .expect("valid request");
        let mut reply = protocol::Reply::EMPTY;
        reply.operation = request.operation;
        reply.request_id = request.request_id;
        reply.session_id = request.session_id;
        reply.generation = request.generation;
        reply.flags = protocol::reply_flags::END_OF_DIRECTORY;
        assert!(!valid_reply(&request, &reply));

        reply.status = protocol::status::NOT_FOUND;
        assert!(!valid_reply(&request, &reply));
    }
}
