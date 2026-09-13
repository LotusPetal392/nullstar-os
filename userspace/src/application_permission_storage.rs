//! Fixed-file and live NullFS storage adapters for permission checkpoints.

use crate::{
    application_permission_persistence::{
        APPLICATION_PERMISSION_CHECKPOINT_BYTES, APPLICATION_PERMISSION_SELECTOR_BYTES,
        ApplicationPermissionPersistence,
    },
    filesystem::{self, Node, Session, protocol},
    handle::{OwnedHandle, SharedMemory},
    ipc,
};

pub const APPLICATION_PERMISSION_CHECKPOINT_SLOT_COUNT: usize = 2;
pub const APPLICATION_PERMISSION_SELECTOR_SLOT_COUNT: usize = 2;
pub const APPLICATION_PERMISSION_IO_BUFFER_BYTES: usize = 4096;
pub const APPLICATION_PERMISSION_STORAGE_BYTES: usize = APPLICATION_PERMISSION_CHECKPOINT_SLOT_COUNT
    * APPLICATION_PERMISSION_CHECKPOINT_BYTES
    + APPLICATION_PERMISSION_SELECTOR_SLOT_COUNT * APPLICATION_PERMISSION_SELECTOR_BYTES;

pub trait ApplicationPermissionFile {
    type Error;

    fn read_exact(&mut self, offset: u64, output: &mut [u8]) -> Result<(), Self::Error>;
    fn write_all(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Self::Error>;
    fn sync(&mut self) -> Result<(), Self::Error>;
}

/// Maps the persistence protocol's four logical slots onto one fixed-size trusted file.
pub struct ApplicationPermissionFilePersistence<F> {
    file: F,
}

impl<F> ApplicationPermissionFilePersistence<F> {
    pub const fn new(file: F) -> Self {
        Self { file }
    }

    pub fn file(&self) -> &F {
        &self.file
    }

    pub fn file_mut(&mut self) -> &mut F {
        &mut self.file
    }

    pub fn into_file(self) -> F {
        self.file
    }
}

impl<F: ApplicationPermissionFile> ApplicationPermissionPersistence
    for ApplicationPermissionFilePersistence<F>
{
    type Error = F::Error;

    fn read_checkpoint(
        &mut self,
        slot: usize,
        output: &mut [u8; APPLICATION_PERMISSION_CHECKPOINT_BYTES],
    ) -> Result<(), Self::Error> {
        self.file.read_exact(checkpoint_offset(slot), output)
    }

    fn write_checkpoint(
        &mut self,
        slot: usize,
        bytes: &[u8; APPLICATION_PERMISSION_CHECKPOINT_BYTES],
    ) -> Result<(), Self::Error> {
        self.file.write_all(checkpoint_offset(slot), bytes)
    }

    fn sync_checkpoint(&mut self, _slot: usize) -> Result<(), Self::Error> {
        self.file.sync()
    }

    fn read_selector(
        &mut self,
        slot: usize,
        output: &mut [u8; APPLICATION_PERMISSION_SELECTOR_BYTES],
    ) -> Result<(), Self::Error> {
        self.file.read_exact(selector_offset(slot), output)
    }

    fn write_selector(
        &mut self,
        slot: usize,
        bytes: &[u8; APPLICATION_PERMISSION_SELECTOR_BYTES],
    ) -> Result<(), Self::Error> {
        self.file.write_all(selector_offset(slot), bytes)
    }

    fn sync_selector(&mut self, _slot: usize) -> Result<(), Self::Error> {
        self.file.sync()
    }
}

const fn checkpoint_offset(slot: usize) -> u64 {
    assert!(slot < APPLICATION_PERMISSION_CHECKPOINT_SLOT_COUNT);
    (slot * APPLICATION_PERMISSION_CHECKPOINT_BYTES) as u64
}

const fn selector_offset(slot: usize) -> u64 {
    assert!(slot < APPLICATION_PERMISSION_SELECTOR_SLOT_COUNT);
    (APPLICATION_PERMISSION_CHECKPOINT_SLOT_COUNT * APPLICATION_PERMISSION_CHECKPOINT_BYTES
        + slot * APPLICATION_PERMISSION_SELECTOR_BYTES) as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NullFsApplicationPermissionFileError {
    InvalidConfiguration,
    RequestIdExhausted,
    Filesystem(filesystem::Error),
    SharedMemory(ipc::Error),
    WrongFileKind,
    WrongFileSize,
    ShortRead,
    ShortWrite,
}

/// Live writable filesystem file used by the fixed permission-persistence layout.
pub struct NullFsApplicationPermissionFile {
    session: Session,
    file: Node,
    shared: OwnedHandle<SharedMemory>,
    buffer_id: u64,
    next_request_id: u64,
}

impl NullFsApplicationPermissionFile {
    /// Attaches an already-formatted file without mutating its size or contents.
    pub fn attach_existing(
        session: Session,
        file: Node,
        buffer_id: u64,
        first_request_id: u64,
    ) -> Result<Self, NullFsApplicationPermissionFileError> {
        Self::attach(session, file, buffer_id, first_request_id, false)
    }

    /// Formats a newly created, empty file to the exact persistence layout.
    pub fn format_new(
        session: Session,
        file: Node,
        buffer_id: u64,
        first_request_id: u64,
    ) -> Result<Self, NullFsApplicationPermissionFileError> {
        Self::attach(session, file, buffer_id, first_request_id, true)
    }

    fn attach(
        session: Session,
        file: Node,
        buffer_id: u64,
        first_request_id: u64,
        format_new: bool,
    ) -> Result<Self, NullFsApplicationPermissionFileError> {
        if !session.is_writable() || buffer_id == 0 || first_request_id == 0 {
            return Err(NullFsApplicationPermissionFileError::InvalidConfiguration);
        }
        let attach_request = first_request_id
            .checked_add(if format_new { 3 } else { 1 })
            .ok_or(NullFsApplicationPermissionFileError::RequestIdExhausted)?;
        let next_request_id = attach_request
            .checked_add(1)
            .ok_or(NullFsApplicationPermissionFileError::RequestIdExhausted)?;
        let attributes = session
            .attributes(first_request_id, file)
            .map_err(NullFsApplicationPermissionFileError::Filesystem)?;
        if attributes.node_id != file.id() || attributes.kind != protocol::node_kind::FILE {
            return Err(NullFsApplicationPermissionFileError::WrongFileKind);
        }
        if format_new {
            if attributes.size != 0 {
                return Err(NullFsApplicationPermissionFileError::WrongFileSize);
            }
            session
                .truncate(
                    first_request_id
                        .checked_add(1)
                        .ok_or(NullFsApplicationPermissionFileError::RequestIdExhausted)?,
                    file,
                    APPLICATION_PERMISSION_STORAGE_BYTES as u64,
                )
                .map_err(NullFsApplicationPermissionFileError::Filesystem)?;
            session
                .sync(
                    first_request_id
                        .checked_add(2)
                        .ok_or(NullFsApplicationPermissionFileError::RequestIdExhausted)?,
                )
                .map_err(NullFsApplicationPermissionFileError::Filesystem)?;
        } else if attributes.size != APPLICATION_PERMISSION_STORAGE_BYTES as u64 {
            return Err(NullFsApplicationPermissionFileError::WrongFileSize);
        }
        let shared = OwnedHandle::<SharedMemory>::create(APPLICATION_PERMISSION_IO_BUFFER_BYTES)
            .map_err(NullFsApplicationPermissionFileError::SharedMemory)?;
        session
            .attach_shared_buffer(
                attach_request,
                buffer_id,
                shared.as_raw(),
                APPLICATION_PERMISSION_IO_BUFFER_BYTES,
            )
            .map_err(NullFsApplicationPermissionFileError::Filesystem)?;
        Ok(Self {
            session,
            file,
            shared,
            buffer_id,
            next_request_id,
        })
    }

    pub fn detach(mut self) -> Result<(), NullFsApplicationPermissionFileError> {
        let request_id = self.take_request_id()?;
        self.session
            .detach_shared_buffer(request_id, self.buffer_id)
            .map_err(NullFsApplicationPermissionFileError::Filesystem)
    }

    fn take_request_id(&mut self) -> Result<u64, NullFsApplicationPermissionFileError> {
        let request_id = self.next_request_id;
        self.next_request_id = request_id
            .checked_add(1)
            .ok_or(NullFsApplicationPermissionFileError::RequestIdExhausted)?;
        Ok(request_id)
    }
}

impl ApplicationPermissionFile for NullFsApplicationPermissionFile {
    type Error = NullFsApplicationPermissionFileError;

    fn read_exact(&mut self, offset: u64, output: &mut [u8]) -> Result<(), Self::Error> {
        if output.is_empty() || output.len() > APPLICATION_PERMISSION_CHECKPOINT_BYTES {
            return Err(Self::Error::InvalidConfiguration);
        }
        let mut completed = 0;
        while completed < output.len() {
            let end = output
                .len()
                .min(completed + APPLICATION_PERMISSION_IO_BUFFER_BYTES);
            let length = end - completed;
            let file_offset = offset
                .checked_add(completed as u64)
                .ok_or(Self::Error::InvalidConfiguration)?;
            let request_id = self.take_request_id()?;
            let count = self
                .session
                .read_to_shared_buffer(
                    request_id,
                    self.file,
                    file_offset,
                    protocol::BulkBuffer {
                        buffer_id: self.buffer_id,
                        offset: 0,
                        length: length as u64,
                    },
                )
                .map_err(Self::Error::Filesystem)?;
            if count != length {
                return Err(Self::Error::ShortRead);
            }
            if self.shared.read(0, &mut output[completed..end]).ok() != Some(length) {
                return Err(Self::Error::SharedMemory(ipc::Error::IO));
            }
            completed = end;
        }
        Ok(())
    }

    fn write_all(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Self::Error> {
        if bytes.is_empty() || bytes.len() > APPLICATION_PERMISSION_CHECKPOINT_BYTES {
            return Err(Self::Error::InvalidConfiguration);
        }
        let mut completed = 0;
        while completed < bytes.len() {
            let end = bytes
                .len()
                .min(completed + APPLICATION_PERMISSION_IO_BUFFER_BYTES);
            let chunk = &bytes[completed..end];
            if self.shared.write(0, chunk).ok() != Some(chunk.len()) {
                return Err(Self::Error::SharedMemory(ipc::Error::IO));
            }
            let file_offset = offset
                .checked_add(completed as u64)
                .ok_or(Self::Error::InvalidConfiguration)?;
            let request_id = self.take_request_id()?;
            let count = self
                .session
                .write_from_shared_buffer(
                    request_id,
                    self.file,
                    file_offset,
                    protocol::BulkBuffer {
                        buffer_id: self.buffer_id,
                        offset: 0,
                        length: chunk.len() as u64,
                    },
                    false,
                )
                .map_err(Self::Error::Filesystem)?;
            if count != chunk.len() {
                return Err(Self::Error::ShortWrite);
            }
            completed = end;
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<(), Self::Error> {
        let request_id = self.take_request_id()?;
        self.session
            .sync(request_id)
            .map_err(Self::Error::Filesystem)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application_permission::ApplicationPermissionStore,
        application_permission_persistence::{
            commit_application_permission_store, recover_application_permission_store,
        },
    };

    struct MemoryFile {
        bytes: [u8; APPLICATION_PERMISSION_STORAGE_BYTES],
        syncs: usize,
    }

    impl MemoryFile {
        fn new() -> Self {
            Self {
                bytes: [0; APPLICATION_PERMISSION_STORAGE_BYTES],
                syncs: 0,
            }
        }
    }

    impl ApplicationPermissionFile for MemoryFile {
        type Error = ();

        fn read_exact(&mut self, offset: u64, output: &mut [u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            output.copy_from_slice(&self.bytes[start..start + output.len()]);
            Ok(())
        }

        fn write_all(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            self.bytes[start..start + bytes.len()].copy_from_slice(bytes);
            Ok(())
        }

        fn sync(&mut self) -> Result<(), Self::Error> {
            self.syncs += 1;
            Ok(())
        }
    }

    #[test]
    fn fixed_file_layout_commits_and_recovers_without_overlap() {
        let mut persistence = ApplicationPermissionFilePersistence::new(MemoryFile::new());
        let store = ApplicationPermissionStore::new();
        let first = commit_application_permission_store(&mut persistence, &store, None).unwrap();
        let second =
            commit_application_permission_store(&mut persistence, &store, Some(first)).unwrap();
        assert_eq!(persistence.file().syncs, 4);
        let recovered = recover_application_permission_store(&mut persistence).unwrap();
        assert_eq!(recovered.commit, second);
        assert_eq!(recovered.store.records().count(), 0);
        assert_eq!(
            selector_offset(1) + APPLICATION_PERMISSION_SELECTOR_BYTES as u64,
            APPLICATION_PERMISSION_STORAGE_BYTES as u64
        );
    }
}
