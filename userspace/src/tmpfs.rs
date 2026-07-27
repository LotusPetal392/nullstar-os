//! Compatibility helpers for the supervised userspace tmpfs service.
//!
//! The public `Mount` API retains the original bounded tmpfs interface while
//! translating every operation to the generic filesystem-service protocol.

use core::mem::size_of;

use crate::{
    filesystem::{self, Node},
    ipc::{self, CapabilityHandle},
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
    session: filesystem::Session,
    generation: u32,
}

impl Mount {
    pub fn connect(service: CapabilityHandle) -> Result<Self, Error> {
        let session = filesystem::connect_service(service, 1).map_err(map_error)?;
        let generation = u32::try_from(session.generation()).map_err(|_| Error::Transport)?;
        Ok(Self {
            session,
            generation,
        })
    }

    pub const fn generation(self) -> u32 {
        self.generation
    }

    pub fn write(self, name: &[u8], bytes: &[u8]) -> Result<usize, Error> {
        if bytes.len() > protocol::MAX_DATA_BYTES {
            return Err(Error::Range);
        }
        validate_name(name)?;
        let node = match self.session.lookup_node(2, Node::root(self.session), name) {
            Ok(node) => node,
            Err(filesystem::Error::NotFound) => self
                .session
                .create_file(3, Node::root(self.session), name, true, false)
                .map_err(map_error)?,
            Err(error) => return Err(map_error(error)),
        };
        if bytes.is_empty() {
            return Ok(0);
        }
        self.with_shared_buffer(bytes.len(), |handle, bulk| {
            if ipc::shared_memory_write(handle, 0, bytes).ok() != Some(bytes.len()) {
                return Err(Error::Transport);
            }
            self.session
                .write_from_shared_buffer(5, node, 0, bulk, false)
                .map_err(map_error)
        })
    }

    pub fn read(self, name: &[u8], offset: usize, buffer: &mut [u8]) -> Result<usize, Error> {
        if buffer.len() > protocol::MAX_DATA_BYTES {
            return Err(Error::Range);
        }
        validate_name(name)?;
        if buffer.is_empty() {
            return Ok(0);
        }
        let node = self
            .session
            .lookup_node(2, Node::root(self.session), name)
            .map_err(map_error)?;
        self.with_shared_buffer(buffer.len(), |handle, bulk| {
            let count = self
                .session
                .read_to_shared_buffer(5, node, offset as u64, bulk)
                .map_err(map_error)?;
            if count > buffer.len()
                || ipc::shared_memory_read(handle, 0, &mut buffer[..count]).ok() != Some(count)
            {
                return Err(Error::Transport);
            }
            Ok(count)
        })
    }

    pub fn stat(self, name: &[u8]) -> Result<usize, Error> {
        validate_name(name)?;
        let node = self
            .session
            .lookup_node(2, Node::root(self.session), name)
            .map_err(map_error)?;
        let attributes = self.session.attributes(3, node).map_err(map_error)?;
        usize::try_from(attributes.size).map_err(|_| Error::Range)
    }

    pub fn remove(self, name: &[u8]) -> Result<(), Error> {
        validate_name(name)?;
        self.session
            .unlink(2, Node::root(self.session), name)
            .map_err(map_error)
    }

    pub fn list(self, buffer: &mut [u8]) -> Result<usize, Error> {
        if buffer.len() > protocol::MAX_DATA_BYTES {
            return Err(Error::Range);
        }
        if buffer.is_empty() {
            return Ok(0);
        }
        let directory_bytes =
            protocol::MAX_FILES * size_of::<filesystem::protocol::DirectoryEntry>();
        self.with_shared_buffer(directory_bytes, |handle, bulk| {
            let batch = self
                .session
                .read_directory_to_shared_buffer(5, Node::root(self.session), 0, bulk)
                .map_err(map_error)?;
            if !batch.end || batch.count > protocol::MAX_FILES {
                return Err(Error::Transport);
            }
            let mut entry_bytes = [0_u8; size_of::<filesystem::protocol::DirectoryEntry>()];
            let mut cursor = 0usize;
            for index in 0..batch.count {
                let offset = index * entry_bytes.len();
                if ipc::shared_memory_read(handle, offset, &mut entry_bytes).ok()
                    != Some(entry_bytes.len())
                {
                    return Err(Error::Transport);
                }
                let entry = unsafe {
                    core::ptr::read_unaligned(
                        entry_bytes.as_ptr() as *const filesystem::protocol::DirectoryEntry
                    )
                };
                let name_length = usize::from(entry.name_length);
                if name_length == 0 || name_length > entry.name.len() {
                    return Err(Error::Transport);
                }
                let separator = usize::from(cursor != 0);
                let Some(end) = cursor
                    .checked_add(separator)
                    .and_then(|value| value.checked_add(name_length))
                else {
                    return Err(Error::Range);
                };
                if end > buffer.len() {
                    break;
                }
                if separator != 0 {
                    buffer[cursor] = b'\n';
                    cursor += 1;
                }
                buffer[cursor..end].copy_from_slice(&entry.name[..name_length]);
                cursor = end;
            }
            Ok(cursor)
        })
    }

    pub fn disconnect(self) -> Result<(), Error> {
        self.session.disconnect(7).map_err(map_error)
    }

    fn with_shared_buffer<T>(
        self,
        length: usize,
        operation: impl FnOnce(CapabilityHandle, filesystem::protocol::BulkBuffer) -> Result<T, Error>,
    ) -> Result<T, Error> {
        if length == 0 {
            return Err(Error::Range);
        }
        const BUFFER_ID: u64 = 1;
        let handle = ipc::shared_memory_create(length).map_err(|_| Error::Transport)?;
        if let Err(error) = self
            .session
            .attach_shared_buffer(4, BUFFER_ID, handle, length)
            .map_err(map_error)
        {
            let _ = ipc::close(handle);
            return Err(error);
        }
        let bulk = filesystem::protocol::BulkBuffer {
            buffer_id: BUFFER_ID,
            offset: 0,
            length: length as u64,
        };
        let result = operation(handle, bulk);
        let detached = self
            .session
            .detach_shared_buffer(6, BUFFER_ID)
            .map_err(map_error);
        let closed = ipc::close(handle).map_err(|_| Error::Transport);
        detached?;
        closed?;
        result
    }
}

fn validate_name(name: &[u8]) -> Result<(), Error> {
    if name.is_empty() || name.len() > protocol::MAX_NAME_BYTES || name.contains(&b'/') {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn map_error(error: filesystem::Error) -> Error {
    match error {
        filesystem::Error::InvalidName
        | filesystem::Error::InvalidRequestId
        | filesystem::Error::InvalidSession
        | filesystem::Error::InvalidNode
        | filesystem::Error::InvalidBuffer
        | filesystem::Error::InvalidFlags => Error::Invalid,
        filesystem::Error::Range => Error::Range,
        filesystem::Error::NotFound => Error::NotFound,
        filesystem::Error::StaleSession | filesystem::Error::StaleNode => Error::StaleMount,
        filesystem::Error::Service(filesystem::protocol::status::NO_SPACE) => Error::NoSpace,
        filesystem::Error::Transport | filesystem::Error::Service(_) => Error::Transport,
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, protocol, validate_name};

    #[test]
    fn compatibility_names_keep_the_legacy_bound() {
        assert_eq!(validate_name(b""), Err(Error::Invalid));
        assert_eq!(validate_name(b"a/b"), Err(Error::Invalid));
        assert_eq!(
            validate_name(&[b'x'; protocol::MAX_NAME_BYTES + 1]),
            Err(Error::Invalid)
        );
        assert_eq!(validate_name(b"state"), Ok(()));
    }
}
