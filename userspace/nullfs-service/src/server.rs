use core::{cmp, mem::size_of, slice};

use nullfs_blockdev::{BlockDevice, BlockDeviceError};
use nullfs_core::{Error as CoreError, Filesystem, NodeAttributes as CoreNodeAttributes, NodeId};
use nullfs_format::{BLOCK_SIZE, NodeKind, Timestamp};
use nullfs_service::state::{NodeIdentity, NodeMap, NodeMapError, OpenRecord, OpenTable};
use userspace::{
    filesystem::protocol,
    filesystem_service::{BufferSlot, Error as SessionError, NodeReferenceError, SessionTable},
    ipc::{self, ObjectKind, ReceivedCapability, Rights},
    syscall,
};

use crate::REQUEST_HANDLE;

pub fn serve<D: BlockDevice>(
    filesystem: Filesystem<D>,
    generation: u64,
    root_attributes: CoreNodeAttributes,
) -> ! {
    let root = root_attributes.node;
    let node_capacity = filesystem
        .statistics()
        .ok()
        .and_then(|statistics| usize::try_from(statistics.total_inodes).ok())
        .unwrap_or_else(|| fail(32, b"nullfs: inode capacity is not representable\n"));
    let nodes = NodeMap::new(
        generation,
        node_capacity,
        root,
        root_attributes.generation,
        root_attributes.kind,
    )
    .unwrap_or_else(|_| fail(33, b"nullfs: opaque node map allocation failed\n"));
    let mut server = FilesystemServer {
        filesystem,
        generation,
        sessions: SessionTable::new(),
        nodes,
        opens: OpenTable::new(),
    };
    if ipc::send(crate::READY_HANDLE, crate::READY_MESSAGE, None).is_err() {
        fail(30, b"nullfs: readiness send failed\n");
    }
    server.run()
}

struct FilesystemServer<D> {
    filesystem: Filesystem<D>,
    generation: u64,
    sessions: SessionTable,
    nodes: NodeMap,
    opens: OpenTable,
}

impl<D: BlockDevice> FilesystemServer<D> {
    fn run(&mut self) -> ! {
        let mut request_bytes = [0_u8; userspace::abi::limits::MAX_IPC_MESSAGE_BYTES];
        loop {
            let message = match ipc::receive(REQUEST_HANDLE, &mut request_bytes) {
                Ok(message) => message,
                Err(_) => fail(31, b"nullfs: request receive failed\n"),
            };
            if message.bytes != size_of::<protocol::Request>() {
                close_capability(message.capability);
                continue;
            }
            self.dispatch(&request_bytes, message.capability);
        }
    }

    fn dispatch(&mut self, request_bytes: &[u8], capability: Option<ReceivedCapability>) {
        let request = unsafe {
            core::ptr::read_unaligned(request_bytes.as_ptr() as *const protocol::Request)
        };
        if request.version != protocol::VERSION
            || request.request_id == protocol::INVALID_ID
            || request.reserved != [0; 3]
            || request.flags & !protocol::request_flags::ALL != 0
        {
            close_capability(capability);
            return;
        }

        if request.operation == protocol::operation::CONNECT {
            self.connect(&request, capability);
            return;
        }

        let Ok(reply_endpoint) = self
            .sessions
            .reply_endpoint(request.session_id, request.generation)
        else {
            close_capability(capability);
            return;
        };
        let mut reply = filesystem_reply(&request);

        if request.operation == protocol::operation::DISCONNECT {
            self.disconnect(&request, capability, reply_endpoint, &mut reply);
            return;
        }

        match request.operation {
            protocol::operation::ATTACH_BUFFER => {
                self.attach_buffer(&request, capability, &mut reply);
            }
            protocol::operation::DETACH_BUFFER => {
                reject_unexpected_capability(capability, &mut reply);
                if reply.status == protocol::status::OK {
                    self.detach_buffer(&request, &mut reply);
                }
            }
            protocol::operation::LOOKUP => {
                reject_unexpected_capability(capability, &mut reply);
                if reply.status == protocol::status::OK {
                    self.lookup(&request, &mut reply);
                }
            }
            protocol::operation::GET_ATTRIBUTES => {
                reject_unexpected_capability(capability, &mut reply);
                if reply.status == protocol::status::OK {
                    self.get_attributes(&request, &mut reply);
                }
            }
            protocol::operation::OPEN => {
                reject_unexpected_capability(capability, &mut reply);
                if reply.status == protocol::status::OK {
                    self.open(&request, &mut reply);
                }
            }
            protocol::operation::READ => {
                reject_unexpected_capability(capability, &mut reply);
                if reply.status == protocol::status::OK {
                    self.read(&request, &mut reply);
                }
            }
            protocol::operation::READ_DIRECTORY => {
                reject_unexpected_capability(capability, &mut reply);
                if reply.status == protocol::status::OK {
                    self.read_directory(&request, &mut reply);
                }
            }
            protocol::operation::CLOSE_NODE => {
                reject_unexpected_capability(capability, &mut reply);
                if reply.status == protocol::status::OK {
                    self.close_node(&request, &mut reply);
                }
            }
            protocol::operation::WRITE
            | protocol::operation::CREATE_FILE
            | protocol::operation::CREATE_DIRECTORY
            | protocol::operation::UNLINK
            | protocol::operation::RENAME => {
                reject_unexpected_capability(capability, &mut reply);
                if reply.status == protocol::status::OK {
                    reply.status = if canonical_read_only_rejection(&request) {
                        protocol::status::PERMISSION
                    } else {
                        protocol::status::INVALID
                    };
                }
            }
            protocol::operation::CANCEL => {
                reject_unexpected_capability(capability, &mut reply);
                reply.status = if reply.status == protocol::status::OK
                    && canonical_empty_request_fields(&request)
                {
                    protocol::status::NOT_SUPPORTED
                } else {
                    protocol::status::INVALID
                };
            }
            _ => {
                reject_unexpected_capability(capability, &mut reply);
                reply.status = if reply.status == protocol::status::OK
                    && canonical_empty_request_fields(&request)
                {
                    protocol::status::NOT_SUPPORTED
                } else {
                    protocol::status::INVALID
                };
            }
        }
        send_value(reply_endpoint, &reply);
    }

    fn connect(&mut self, request: &protocol::Request, capability: Option<ReceivedCapability>) {
        let Some(capability) = capability else {
            return;
        };
        if capability.rights != Rights::SEND
            || !ipc::info(capability.handle).is_ok_and(|info| info.kind == ObjectKind::Endpoint)
        {
            let _ = ipc::close(capability.handle);
            return;
        }

        let mut reply = filesystem_reply(request);
        if request.session_id != protocol::INVALID_ID
            || request.generation != 0
            || !canonical_empty_request_fields(request)
        {
            reply.status = protocol::status::INVALID;
            send_value(capability.handle, &reply);
            let _ = ipc::close(capability.handle);
            return;
        }

        match self.sessions.connect(self.generation, capability.handle) {
            Ok(session_id) => {
                reply.session_id = session_id;
                reply.generation = self.generation;
                reply.node_id = protocol::ROOT_NODE_ID;
                reply.node_kind = protocol::node_kind::DIRECTORY;
                send_value(capability.handle, &reply);
            }
            Err(error) => {
                reply.status = session_status(error);
                send_value(capability.handle, &reply);
                let _ = ipc::close(capability.handle);
            }
        }
    }

    fn disconnect(
        &mut self,
        request: &protocol::Request,
        capability: Option<ReceivedCapability>,
        reply_endpoint: u64,
        reply: &mut protocol::Reply,
    ) {
        reject_unexpected_capability(capability, reply);
        if reply.status == protocol::status::OK && !canonical_empty_request_fields(request) {
            reply.status = protocol::status::INVALID;
        }
        if reply.status != protocol::status::OK {
            send_value(reply_endpoint, reply);
            return;
        }

        while let Some((record_index, record)) = self
            .opens
            .find_one_for_session(request.session_id, request.generation)
        {
            if let Err(error) =
                self.sessions
                    .close_node(request.session_id, request.generation, record.opaque_node)
            {
                reply.status = node_reference_status(error);
                send_value(reply_endpoint, reply);
                return;
            }
            if let Err(error) = self.filesystem.close_node(record.handle) {
                reply.status = if self
                    .sessions
                    .record_open_node(request.session_id, request.generation, record.opaque_node)
                    .is_ok()
                {
                    core_status(error)
                } else {
                    protocol::status::IO
                };
                send_value(reply_endpoint, reply);
                return;
            }
            let _ = self.opens.remove(record_index);
        }

        match self
            .sessions
            .disconnect(request.session_id, request.generation)
        {
            Ok(released) => {
                if released
                    .node_references
                    .iter()
                    .any(|reference| reference.references != 0)
                {
                    reply.status = protocol::status::IO;
                }
                send_value(released.reply_endpoint, reply);
                for handle in released
                    .buffer_handles
                    .into_iter()
                    .filter(|handle| *handle != 0)
                {
                    let _ = ipc::close(handle);
                }
                let _ = ipc::close(released.reply_endpoint);
            }
            Err(error) => {
                reply.status = session_status(error);
                send_value(reply_endpoint, reply);
            }
        }
    }

    fn attach_buffer(
        &mut self,
        request: &protocol::Request,
        capability: Option<ReceivedCapability>,
        reply: &mut protocol::Reply,
    ) {
        let Some(capability) = capability else {
            reply.status = protocol::status::INVALID;
            return;
        };
        if !canonical_attach_buffer(request) {
            let _ = ipc::close(capability.handle);
            reply.status = protocol::status::INVALID;
            return;
        }

        let required_rights = Rights::READ | Rights::WRITE;
        let Ok(info) = ipc::info(capability.handle) else {
            let _ = ipc::close(capability.handle);
            reply.status = protocol::status::INVALID;
            return;
        };
        if info.kind != ObjectKind::SharedMemory
            || capability.rights != required_rights
            || request.bulk.length > info.size
        {
            let _ = ipc::close(capability.handle);
            reply.status = protocol::status::INVALID;
            return;
        }

        match self.sessions.attach_buffer(
            request.session_id,
            request.generation,
            request.bulk.buffer_id,
            capability.handle,
            request.bulk.length,
        ) {
            Ok(replaced) => {
                if let Some(replaced) = replaced {
                    let _ = ipc::close(replaced);
                }
            }
            Err(error) => {
                let _ = ipc::close(capability.handle);
                reply.status = session_status(error);
            }
        }
    }

    fn detach_buffer(&mut self, request: &protocol::Request, reply: &mut protocol::Reply) {
        if !canonical_detach_buffer(request) {
            reply.status = protocol::status::INVALID;
            return;
        }
        match self.sessions.detach_buffer(
            request.session_id,
            request.generation,
            request.bulk.buffer_id,
        ) {
            Ok(handle) => {
                let _ = ipc::close(handle);
            }
            Err(error) => reply.status = session_status(error),
        }
    }

    fn lookup(&mut self, request: &protocol::Request, reply: &mut protocol::Reply) {
        let Some(name) = canonical_lookup_name(request) else {
            reply.status = protocol::status::INVALID;
            return;
        };
        let directory = match self.resolve_current(request.node_id) {
            Ok((identity, _)) => identity,
            Err(status) => {
                reply.status = status;
                return;
            }
        };
        if directory.kind != NodeKind::Directory {
            reply.status = protocol::status::NOT_DIRECTORY;
            return;
        }

        let node = match self.filesystem.lookup(directory.node, name) {
            Ok(node) => node,
            Err(error) => {
                reply.status = core_status(error);
                return;
            }
        };
        let attributes = match self.filesystem.attributes(node) {
            Ok(attributes) => attributes,
            Err(error) => {
                reply.status = core_status(error);
                return;
            }
        };
        let opaque_id = match self.intern_node(node, &attributes) {
            Ok(opaque_id) => opaque_id,
            Err(status) => {
                reply.status = status;
                return;
            }
        };
        reply.node_id = opaque_id;
        reply.node_kind = protocol_node_kind(attributes.kind);
        reply.value = attributes.size;
    }

    fn get_attributes(&mut self, request: &protocol::Request, reply: &mut protocol::Reply) {
        if !canonical_node_request(request) {
            reply.status = protocol::status::INVALID;
            return;
        }
        let (_, attributes) = match self.resolve_current(request.node_id) {
            Ok(resolved) => resolved,
            Err(status) => {
                reply.status = status;
                return;
            }
        };
        let Some(attributes) = protocol_attributes(request.node_id, &attributes) else {
            reply.status = protocol::status::RANGE;
            return;
        };
        let bytes = value_bytes(&attributes);
        reply.data[..bytes.len()].copy_from_slice(bytes);
        reply.data_length = bytes.len() as u16;
        reply.node_id = attributes.node_id;
        reply.node_kind = attributes.kind;
    }

    fn open(&mut self, request: &protocol::Request, reply: &mut protocol::Reply) {
        if !canonical_open(request) {
            reply.status = protocol::status::INVALID;
            return;
        }
        if request.flags
            & (protocol::request_flags::CREATE
                | protocol::request_flags::EXCLUSIVE
                | protocol::request_flags::TRUNCATE
                | protocol::request_flags::APPEND
                | protocol::request_flags::WRITE)
            != 0
        {
            reply.status = protocol::status::PERMISSION;
            return;
        }

        let (identity, attributes) = if request.name_length == 0 {
            match self.resolve_current(request.node_id) {
                Ok(resolved) => resolved,
                Err(status) => {
                    reply.status = status;
                    return;
                }
            }
        } else {
            let directory = match self.resolve_current(request.node_id) {
                Ok((identity, _)) => identity,
                Err(status) => {
                    reply.status = status;
                    return;
                }
            };
            if directory.kind != NodeKind::Directory {
                reply.status = protocol::status::NOT_DIRECTORY;
                return;
            }
            let name = request_name(request).expect("canonical OPEN name disappeared");
            let node = match self.filesystem.lookup(directory.node, name) {
                Ok(node) => node,
                Err(error) => {
                    reply.status = core_status(error);
                    return;
                }
            };
            let attributes = match self.filesystem.attributes(node) {
                Ok(attributes) => attributes,
                Err(error) => {
                    reply.status = core_status(error);
                    return;
                }
            };
            let opaque_id = match self.intern_node(node, &attributes) {
                Ok(opaque_id) => opaque_id,
                Err(status) => {
                    reply.status = status;
                    return;
                }
            };
            (
                NodeIdentity {
                    opaque_id,
                    node,
                    generation: attributes.generation,
                    kind: attributes.kind,
                },
                attributes,
            )
        };

        let Some(record_index) = self.opens.vacant_index() else {
            reply.status = protocol::status::NO_SPACE;
            return;
        };
        if let Err(error) = self.sessions.record_open_node(
            request.session_id,
            request.generation,
            identity.opaque_id,
        ) {
            reply.status = node_reference_status(error);
            return;
        }
        let handle = match self.filesystem.open_node(identity.node) {
            Ok(handle) => handle,
            Err(error) => {
                reply.status = if self
                    .sessions
                    .close_node(request.session_id, request.generation, identity.opaque_id)
                    .is_ok()
                {
                    core_status(error)
                } else {
                    protocol::status::IO
                };
                return;
            }
        };
        let record = OpenRecord {
            session_id: request.session_id,
            session_generation: request.generation,
            opaque_node: identity.opaque_id,
            handle,
        };
        if self.opens.insert_at(record_index, record).is_err() {
            fail(34, b"nullfs: reserved open-table slot disappeared\n");
        }

        reply.node_id = identity.opaque_id;
        reply.node_kind = protocol_node_kind(identity.kind);
        reply.value = attributes.size;
    }

    fn close_node(&mut self, request: &protocol::Request, reply: &mut protocol::Reply) {
        if !canonical_node_request(request) {
            reply.status = protocol::status::INVALID;
            return;
        }
        let Some((record_index, handle)) =
            self.opens
                .find_one(request.session_id, request.generation, request.node_id)
        else {
            reply.status = protocol::status::STALE_NODE;
            return;
        };

        if let Err(error) =
            self.sessions
                .close_node(request.session_id, request.generation, request.node_id)
        {
            reply.status = node_reference_status(error);
            return;
        }
        if let Err(error) = self.filesystem.close_node(handle) {
            if self
                .sessions
                .record_open_node(request.session_id, request.generation, request.node_id)
                .is_err()
            {
                reply.status = protocol::status::IO;
            } else {
                reply.status = core_status(error);
            }
            return;
        }
        let _ = self.opens.remove(record_index);
    }

    fn read(&mut self, request: &protocol::Request, reply: &mut protocol::Reply) {
        if !canonical_bulk_node_request(request) {
            reply.status = protocol::status::INVALID;
            return;
        }
        let (identity, _) = match self.resolve_current(request.node_id) {
            Ok(resolved) => resolved,
            Err(status) => {
                reply.status = status;
                return;
            }
        };
        let Some((buffer, buffer_offset, requested)) =
            checked_bulk_range(&self.sessions, request, reply)
        else {
            return;
        };

        let mut bytes = [0_u8; BLOCK_SIZE];
        let mut completed = 0usize;
        while completed < requested {
            let Some(file_offset) = request.file_offset.checked_add(completed as u64) else {
                reply.status = protocol::status::RANGE;
                return;
            };
            let chunk_length = cmp::min(bytes.len(), requested - completed);
            let read =
                match self
                    .filesystem
                    .read(identity.node, file_offset, &mut bytes[..chunk_length])
                {
                    Ok(read) if read <= chunk_length => read,
                    Ok(_) => {
                        reply.status = protocol::status::IO;
                        return;
                    }
                    Err(error) => {
                        reply.status = core_status(error);
                        return;
                    }
                };
            if read == 0 {
                break;
            }
            let Some(destination) = buffer_offset.checked_add(completed) else {
                reply.status = protocol::status::RANGE;
                return;
            };
            match ipc::shared_memory_write(buffer.handle, destination, &bytes[..read]) {
                Ok(written) if written == read => completed += read,
                _ => {
                    reply.status = protocol::status::IO;
                    return;
                }
            }
            if read < chunk_length {
                break;
            }
        }
        reply.value = completed as u64;
    }

    fn read_directory(&mut self, request: &protocol::Request, reply: &mut protocol::Reply) {
        if !canonical_bulk_node_request(request) {
            reply.status = protocol::status::INVALID;
            return;
        }
        let (directory, _) = match self.resolve_current(request.node_id) {
            Ok(resolved) => resolved,
            Err(status) => {
                reply.status = status;
                return;
            }
        };
        if directory.kind != NodeKind::Directory {
            reply.status = protocol::status::NOT_DIRECTORY;
            return;
        }
        let Some((buffer, buffer_offset, requested)) =
            checked_bulk_range(&self.sessions, request, reply)
        else {
            return;
        };

        let record_size = size_of::<protocol::DirectoryEntry>();
        let capacity = requested / record_size;
        if capacity == 0 {
            reply.status = protocol::status::RANGE;
            return;
        }

        let mut count = 0usize;
        let mut cookie = request.file_offset;
        let end_of_directory = loop {
            let dot_records = usize::try_from(2_u64.saturating_sub(cookie.min(2))).unwrap_or(0);
            let maximum = (capacity - count).saturating_add(dot_records).max(1);
            let records = match self
                .filesystem
                .read_directory(directory.node, cookie, maximum)
            {
                Ok(records) => records,
                Err(error) => {
                    reply.status = core_status(error);
                    return;
                }
            };
            if records.is_empty() {
                break true;
            }

            let previous_cookie = cookie;
            for record in records {
                cookie = record.next_cookie;
                if record.name == "." || record.name == ".." {
                    continue;
                }
                if record.name.len() > protocol::MAX_NAME_BYTES {
                    reply.status = protocol::status::NOT_SUPPORTED;
                    return;
                }
                let opaque_id = match self
                    .nodes
                    .intern(record.node, record.generation, record.kind)
                {
                    Ok(opaque_id) => opaque_id,
                    Err(error) => {
                        reply.status = node_map_status(error);
                        return;
                    }
                };
                let mut entry = protocol::DirectoryEntry::EMPTY;
                entry.node_id = opaque_id;
                entry.next_cookie = record.next_cookie;
                entry.kind = protocol_node_kind(record.kind);
                entry.name_length = record.name.len() as u16;
                entry.name[..record.name.len()].copy_from_slice(record.name.as_bytes());
                let Some(offset) = count
                    .checked_mul(record_size)
                    .and_then(|offset| buffer_offset.checked_add(offset))
                else {
                    reply.status = protocol::status::RANGE;
                    return;
                };
                match ipc::shared_memory_write(buffer.handle, offset, value_bytes(&entry)) {
                    Ok(written) if written == record_size => count += 1,
                    _ => {
                        reply.status = protocol::status::IO;
                        return;
                    }
                }
                if count == capacity {
                    break;
                }
            }
            if cookie <= previous_cookie {
                reply.status = protocol::status::IO;
                return;
            }
            if count == capacity {
                let more = match self.filesystem.read_directory(directory.node, cookie, 1) {
                    Ok(records) => !records.is_empty(),
                    Err(error) => {
                        reply.status = core_status(error);
                        return;
                    }
                };
                break !more;
            }
        };

        reply.value = count as u64;
        if end_of_directory {
            reply.flags |= protocol::reply_flags::END_OF_DIRECTORY;
        }
    }

    fn resolve_current(
        &mut self,
        opaque_id: u64,
    ) -> Result<(NodeIdentity, CoreNodeAttributes), i32> {
        let identity = self
            .nodes
            .resolve(opaque_id)
            .ok_or(protocol::status::STALE_NODE)?;
        let attributes = self
            .filesystem
            .attributes(identity.node)
            .map_err(core_status)?;
        if attributes.generation != identity.generation || attributes.kind != identity.kind {
            return Err(protocol::status::STALE_NODE);
        }
        Ok((identity, attributes))
    }

    fn intern_node(&mut self, node: NodeId, attributes: &CoreNodeAttributes) -> Result<u64, i32> {
        self.nodes
            .intern(node, attributes.generation, attributes.kind)
            .map_err(node_map_status)
    }
}

fn canonical_attach_buffer(request: &protocol::Request) -> bool {
    request.flags == 0
        && request.node_id == protocol::INVALID_ID
        && request.secondary_node_id == protocol::INVALID_ID
        && request.file_offset == 0
        && request.bulk.buffer_id != protocol::INVALID_ID
        && request.bulk.offset == 0
        && request.bulk.length != 0
        && request.bulk.end().is_some()
        && empty_name(request)
}

fn canonical_detach_buffer(request: &protocol::Request) -> bool {
    request.flags == 0
        && request.node_id == protocol::INVALID_ID
        && request.secondary_node_id == protocol::INVALID_ID
        && request.file_offset == 0
        && request.bulk.buffer_id != protocol::INVALID_ID
        && request.bulk.offset == 0
        && request.bulk.length == 0
        && empty_name(request)
}

fn canonical_lookup_name(request: &protocol::Request) -> Option<&[u8]> {
    (request.flags == 0
        && request.node_id != protocol::INVALID_ID
        && request.secondary_node_id == protocol::INVALID_ID
        && request.file_offset == 0
        && request.bulk == protocol::BulkBuffer::NONE)
        .then(|| request_name(request))
        .flatten()
}

fn canonical_node_request(request: &protocol::Request) -> bool {
    request.flags == 0
        && request.node_id != protocol::INVALID_ID
        && request.secondary_node_id == protocol::INVALID_ID
        && request.file_offset == 0
        && request.bulk == protocol::BulkBuffer::NONE
        && empty_name(request)
}

fn canonical_bulk_node_request(request: &protocol::Request) -> bool {
    request.flags == 0
        && request.node_id != protocol::INVALID_ID
        && request.secondary_node_id == protocol::INVALID_ID
        && request.bulk.buffer_id != protocol::INVALID_ID
        && request.bulk.length != 0
        && empty_name(request)
}

fn canonical_open(request: &protocol::Request) -> bool {
    let name_length = usize::from(request.name_length);
    let named = name_length != 0;
    request.node_id != protocol::INVALID_ID
        && request.secondary_node_id == protocol::INVALID_ID
        && request.file_offset == 0
        && request.bulk == protocol::BulkBuffer::NONE
        && name_length <= protocol::MAX_NAME_BYTES
        && request.name[name_length..].iter().all(|byte| *byte == 0)
        && (!named || request_name(request).is_some())
        && (named
            || request.flags
                & (protocol::request_flags::CREATE | protocol::request_flags::EXCLUSIVE)
                == 0)
        && (request.flags & protocol::request_flags::EXCLUSIVE == 0
            || request.flags & protocol::request_flags::CREATE != 0)
        && (request.flags & (protocol::request_flags::APPEND | protocol::request_flags::TRUNCATE)
            == 0
            || request.flags & protocol::request_flags::WRITE != 0)
}

fn canonical_read_only_rejection(request: &protocol::Request) -> bool {
    match request.operation {
        protocol::operation::WRITE => {
            request.flags & !protocol::request_flags::APPEND == 0
                && request.node_id != protocol::INVALID_ID
                && request.secondary_node_id == protocol::INVALID_ID
                && request.bulk.buffer_id != protocol::INVALID_ID
                && request.bulk.length != 0
                && request.bulk.end().is_some()
                && empty_name(request)
        }
        protocol::operation::CREATE_FILE => {
            request.flags
                & !(protocol::request_flags::EXCLUSIVE | protocol::request_flags::TRUNCATE)
                == 0
                && canonical_named_mutation(request, false)
        }
        protocol::operation::CREATE_DIRECTORY => {
            request.flags & !protocol::request_flags::EXCLUSIVE == 0
                && canonical_named_mutation(request, false)
        }
        protocol::operation::UNLINK => {
            request.flags == 0 && canonical_named_mutation(request, false)
        }
        protocol::operation::RENAME => {
            request.flags == 0 && canonical_named_mutation(request, true)
        }
        _ => false,
    }
}

fn canonical_named_mutation(request: &protocol::Request, secondary_required: bool) -> bool {
    request.node_id != protocol::INVALID_ID
        && (request.secondary_node_id != protocol::INVALID_ID) == secondary_required
        && request.file_offset == 0
        && request.bulk == protocol::BulkBuffer::NONE
        && request_name(request).is_some()
}

fn canonical_empty_request_fields(request: &protocol::Request) -> bool {
    request.flags == 0
        && request.node_id == protocol::INVALID_ID
        && request.secondary_node_id == protocol::INVALID_ID
        && request.file_offset == 0
        && request.bulk == protocol::BulkBuffer::NONE
        && empty_name(request)
}

fn empty_name(request: &protocol::Request) -> bool {
    request.name_length == 0 && request.name == [0; protocol::MAX_NAME_BYTES]
}

fn request_name(request: &protocol::Request) -> Option<&[u8]> {
    let length = usize::from(request.name_length);
    (length != 0
        && length <= protocol::MAX_NAME_BYTES
        && request.name[length..].iter().all(|byte| *byte == 0)
        && !request.name[..length].contains(&b'/')
        && !request.name[..length].contains(&0))
    .then_some(&request.name[..length])
}

fn checked_bulk_range(
    sessions: &SessionTable,
    request: &protocol::Request,
    reply: &mut protocol::Reply,
) -> Option<(BufferSlot, usize, usize)> {
    let buffer = match sessions.buffer(
        request.session_id,
        request.generation,
        request.bulk.buffer_id,
    ) {
        Ok(buffer) => buffer,
        Err(error) => {
            reply.status = session_status(error);
            return None;
        }
    };
    let Some(end) = request.bulk.end() else {
        reply.status = protocol::status::RANGE;
        return None;
    };
    if request.bulk.length == 0 || end > buffer.length {
        reply.status = protocol::status::RANGE;
        return None;
    }
    let Ok(offset) = usize::try_from(request.bulk.offset) else {
        reply.status = protocol::status::RANGE;
        return None;
    };
    let Ok(length) = usize::try_from(request.bulk.length) else {
        reply.status = protocol::status::RANGE;
        return None;
    };
    Some((buffer, offset, length))
}

fn protocol_attributes(
    opaque_id: u64,
    source: &CoreNodeAttributes,
) -> Option<protocol::NodeAttributes> {
    let mut attributes = protocol::NodeAttributes::EMPTY;
    attributes.node_id = opaque_id;
    attributes.size = source.size;
    attributes.allocated_size = source.allocated_blocks.checked_mul(BLOCK_SIZE as u64)?;
    attributes.created_nanoseconds = timestamp_nanoseconds(source.created)?;
    attributes.modified_nanoseconds = timestamp_nanoseconds(source.modified)?;
    attributes.changed_nanoseconds = timestamp_nanoseconds(source.changed)?;
    attributes.kind = protocol_node_kind(source.kind);
    attributes.mode = source.mode;
    attributes.link_count = source.link_count;
    Some(attributes)
}

fn timestamp_nanoseconds(timestamp: Timestamp) -> Option<u64> {
    timestamp
        .seconds
        .checked_mul(1_000_000_000)?
        .checked_add(u64::from(timestamp.nanoseconds))
}

fn protocol_node_kind(kind: NodeKind) -> u16 {
    match kind {
        NodeKind::Free => protocol::node_kind::UNKNOWN,
        NodeKind::Regular => protocol::node_kind::FILE,
        NodeKind::Directory => protocol::node_kind::DIRECTORY,
        NodeKind::Symlink => protocol::node_kind::SYMBOLIC_LINK,
    }
}

fn core_status(error: CoreError) -> i32 {
    match error {
        CoreError::InvalidName => protocol::status::INVALID,
        CoreError::NotFound => protocol::status::NOT_FOUND,
        CoreError::NotDirectory => protocol::status::NOT_DIRECTORY,
        CoreError::IsDirectory => protocol::status::IS_DIRECTORY,
        CoreError::UnsupportedNodeKind => protocol::status::NOT_SUPPORTED,
        CoreError::InvalidCookie | CoreError::ArithmeticOverflow => protocol::status::RANGE,
        CoreError::InvalidNode | CoreError::InvalidHandle => protocol::status::STALE_NODE,
        CoreError::ReadOnly => protocol::status::PERMISSION,
        CoreError::AlreadyExists => protocol::status::EXISTS,
        CoreError::NoSpace | CoreError::ExtentLimit | CoreError::TransactionTooLarge => {
            protocol::status::NO_SPACE
        }
        CoreError::Device(BlockDeviceError::ReadOnly) => protocol::status::PERMISSION,
        CoreError::Device(_)
        | CoreError::Format(_)
        | CoreError::Phase2Required
        | CoreError::CorruptVolume
        | CoreError::Phase3Required
        | CoreError::RedundantSuperblocksDisagree
        | CoreError::CorruptJournal
        | CoreError::ProtectedBlock
        | CoreError::Poisoned
        | CoreError::DirectoryNotEmpty
        | CoreError::DirectoryCycle
        | CoreError::TransactionInProgress
        | CoreError::RecoveryRequired => protocol::status::IO,
    }
}

fn node_map_status(error: NodeMapError) -> i32 {
    match error {
        NodeMapError::NoSpace => protocol::status::NO_SPACE,
        NodeMapError::IdentityMismatch => protocol::status::IO,
    }
}

fn session_status(error: SessionError) -> i32 {
    match error {
        SessionError::NoSpace => protocol::status::NO_SPACE,
        SessionError::StaleSession => protocol::status::STALE_SESSION,
        SessionError::InvalidBuffer => protocol::status::STALE_BUFFER,
    }
}

fn node_reference_status(error: NodeReferenceError) -> i32 {
    match error {
        NodeReferenceError::NoSpace => protocol::status::NO_SPACE,
        NodeReferenceError::StaleSession => protocol::status::STALE_SESSION,
        NodeReferenceError::UnknownNode => protocol::status::STALE_NODE,
    }
}

fn reject_unexpected_capability(
    capability: Option<ReceivedCapability>,
    reply: &mut protocol::Reply,
) {
    if let Some(capability) = capability {
        let _ = ipc::close(capability.handle);
        reply.status = protocol::status::INVALID;
    }
}

fn close_capability(capability: Option<ReceivedCapability>) {
    if let Some(capability) = capability {
        let _ = ipc::close(capability.handle);
    }
}

fn filesystem_reply(request: &protocol::Request) -> protocol::Reply {
    let mut reply = protocol::Reply::EMPTY;
    reply.operation = request.operation;
    reply.request_id = request.request_id;
    reply.session_id = request.session_id;
    reply.generation = request.generation;
    reply
}

fn send_value<T>(endpoint: u64, value: &T) {
    let _ = ipc::send(endpoint, value_bytes(value), None);
}

fn value_bytes<T>(value: &T) -> &[u8] {
    unsafe { slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

fn fail(code: u64, message: &[u8]) -> ! {
    let _ = syscall::write_all(syscall::STDERR, message);
    syscall::exit(code)
}

const _: () = assert!(size_of::<protocol::NodeAttributes>() <= protocol::MAX_INLINE_DATA_BYTES);
const _: () =
    assert!(size_of::<protocol::Request>() <= userspace::abi::limits::MAX_IPC_MESSAGE_BYTES);
const _: () =
    assert!(size_of::<protocol::Reply>() <= userspace::abi::limits::MAX_IPC_MESSAGE_BYTES);
