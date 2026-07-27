#![no_std]

//! NullFS block-device adapter for the userspace partition service client.
//!
//! Keeping this adapter separate prevents `nullfs-blockdev`'s allocation-backed
//! helper types from being linked into allocator-free userspace binaries.

use core::cmp;

use nullfs_blockdev::{BlockDevice, BlockDeviceError, checked_block_range};
use userspace::{
    block_device::{DeviceInfo, Error as ClientError, RegisteredBuffer, Session, protocol},
    ipc,
};

/// `nullfs_blockdev` adapter backed by a connected session and its one
/// registered shared-memory buffer.
pub struct SessionBlockDevice {
    session: Session,
    info: DeviceInfo,
    buffer: RegisteredBuffer,
    next_request_id: Option<u64>,
    block_size: usize,
    max_transfer_blocks: usize,
}

impl SessionBlockDevice {
    pub fn new(
        session: Session,
        info: DeviceInfo,
        first_request_id: u64,
    ) -> Result<Self, ClientError> {
        if first_request_id == protocol::INVALID_ID {
            return Err(ClientError::InvalidRequestId);
        }
        session.validate_device_info(info)?;
        let buffer = session
            .registered_buffer()
            .ok_or(ClientError::MissingBuffer)?;
        let (block_size, max_transfer_blocks) =
            adapter_geometry(info.logical_block_size(), buffer.length())?;
        Ok(Self {
            session,
            info,
            buffer,
            next_request_id: Some(first_request_id),
            block_size,
            max_transfer_blocks,
        })
    }

    pub const fn info(&self) -> DeviceInfo {
        self.info
    }

    pub const fn registered_buffer(&self) -> RegisteredBuffer {
        self.buffer
    }

    pub const fn next_request_id(&self) -> Option<u64> {
        self.next_request_id
    }

    pub fn into_session(self) -> Session {
        self.session
    }

    fn take_request_id(&mut self) -> Result<u64, BlockDeviceError> {
        take_request_id(&mut self.next_request_id)
    }
}

impl BlockDevice for SessionBlockDevice {
    fn block_size(&self) -> usize {
        self.block_size
    }

    fn block_count(&self) -> u64 {
        self.info.block_count()
    }

    fn read_blocks(&mut self, first_block: u64, buffer: &mut [u8]) -> Result<(), BlockDeviceError> {
        preflight_range(
            self.block_size,
            self.info.block_count(),
            first_block,
            buffer.len(),
        )?;
        let max_bytes = self
            .max_transfer_blocks
            .checked_mul(self.block_size)
            .ok_or(BlockDeviceError::ArithmeticOverflow)?;
        let mut block_offset = first_block;
        for chunk in buffer.chunks_mut(max_bytes) {
            let block_count = u32::try_from(chunk.len() / self.block_size)
                .map_err(|_| BlockDeviceError::ArithmeticOverflow)?;
            let request_id = self.take_request_id()?;
            let transferred = self
                .session
                .read_to_shared_buffer(request_id, self.info, block_offset, block_count, 0)
                .map_err(map_block_device_error)?;
            if transferred != block_count
                || ipc::shared_memory_read(self.buffer.handle(), 0, chunk).ok() != Some(chunk.len())
            {
                return Err(BlockDeviceError::Io);
            }
            block_offset = block_offset
                .checked_add(u64::from(block_count))
                .ok_or(BlockDeviceError::ArithmeticOverflow)?;
        }
        Ok(())
    }

    fn write_blocks(&mut self, first_block: u64, buffer: &[u8]) -> Result<(), BlockDeviceError> {
        validate_writable(self.info)?;
        preflight_range(
            self.block_size,
            self.info.block_count(),
            first_block,
            buffer.len(),
        )?;
        let max_bytes = self
            .max_transfer_blocks
            .checked_mul(self.block_size)
            .ok_or(BlockDeviceError::ArithmeticOverflow)?;
        let mut block_offset = first_block;
        for chunk in buffer.chunks(max_bytes) {
            if ipc::shared_memory_write(self.buffer.handle(), 0, chunk).ok() != Some(chunk.len()) {
                return Err(BlockDeviceError::Io);
            }
            let block_count = u32::try_from(chunk.len() / self.block_size)
                .map_err(|_| BlockDeviceError::ArithmeticOverflow)?;
            let request_id = self.take_request_id()?;
            let transferred = self
                .session
                .write_from_shared_buffer(request_id, self.info, block_offset, block_count, 0)
                .map_err(map_block_device_error)?;
            if transferred != block_count {
                return Err(BlockDeviceError::Io);
            }
            block_offset = block_offset
                .checked_add(u64::from(block_count))
                .ok_or(BlockDeviceError::ArithmeticOverflow)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BlockDeviceError> {
        if !self.info.supports(protocol::features::FLUSH) {
            return Ok(());
        }
        let request_id = self.take_request_id()?;
        self.session
            .flush(request_id)
            .map_err(map_block_device_error)
    }
}

fn adapter_geometry(
    logical_block_size: u32,
    buffer_length: usize,
) -> Result<(usize, usize), ClientError> {
    let block_size =
        usize::try_from(logical_block_size).map_err(|_| ClientError::InvalidDeviceInfo)?;
    if block_size == 0 {
        return Err(ClientError::InvalidDeviceInfo);
    }
    let transfer_length = cmp::min(buffer_length, protocol::MAX_TRANSFER_BYTES);
    let max_transfer_blocks = cmp::min(transfer_length / block_size, u32::MAX as usize);
    if max_transfer_blocks == 0 {
        return Err(ClientError::InvalidBuffer);
    }
    Ok((block_size, max_transfer_blocks))
}

fn preflight_range(
    block_size: usize,
    block_count: u64,
    first_block: u64,
    buffer_length: usize,
) -> Result<(), BlockDeviceError> {
    checked_block_range(block_size, block_count, first_block, buffer_length).map(|_| ())
}

fn validate_writable(info: DeviceInfo) -> Result<(), BlockDeviceError> {
    if info.is_read_only() || !info.supports(protocol::features::WRITE) {
        Err(BlockDeviceError::ReadOnly)
    } else {
        Ok(())
    }
}

fn take_request_id(next_request_id: &mut Option<u64>) -> Result<u64, BlockDeviceError> {
    let request_id = next_request_id.ok_or(BlockDeviceError::ArithmeticOverflow)?;
    *next_request_id = request_id.checked_add(1);
    Ok(request_id)
}

fn map_block_device_error(error: ClientError) -> BlockDeviceError {
    match error {
        ClientError::ReadOnly => BlockDeviceError::ReadOnly,
        ClientError::Range => BlockDeviceError::OutOfBounds,
        ClientError::InvalidBlockCount => BlockDeviceError::InvalidBufferLength,
        ClientError::InvalidDeviceInfo => BlockDeviceError::InvalidBlockSize,
        _ => BlockDeviceError::Io,
    }
}

#[cfg(test)]
mod tests {
    use nullfs_blockdev::BlockDeviceError;
    use userspace::block_device::{Error as ClientError, protocol};

    use super::{adapter_geometry, map_block_device_error, preflight_range, take_request_id};

    #[test]
    fn adapter_geometry_requires_one_full_block() {
        assert_eq!(
            adapter_geometry(protocol::INITIAL_LOGICAL_BLOCK_SIZE, 511),
            Err(ClientError::InvalidBuffer)
        );
        assert_eq!(
            adapter_geometry(protocol::INITIAL_LOGICAL_BLOCK_SIZE, 1024),
            Ok((512, 2))
        );
    }

    #[test]
    fn adapter_geometry_caps_large_buffers_at_protocol_transfer_limit() {
        assert_eq!(protocol::MAX_TRANSFER_BYTES, 4096);
        assert_eq!(
            adapter_geometry(protocol::INITIAL_LOGICAL_BLOCK_SIZE, 8192),
            Ok((512, 8))
        );
    }

    #[test]
    fn adapter_preflight_rejects_invalid_io_without_ipc() {
        assert_eq!(
            preflight_range(512, 8, 0, 1),
            Err(BlockDeviceError::InvalidBufferLength)
        );
        assert_eq!(
            preflight_range(512, 8, 7, 1024),
            Err(BlockDeviceError::OutOfBounds)
        );
        assert_eq!(preflight_range(512, 8, 8, 0), Ok(()));
    }

    #[test]
    fn request_ids_are_monotonic_and_detect_exhaustion() {
        let mut next_request_id = Some(u64::MAX);
        assert_eq!(take_request_id(&mut next_request_id), Ok(u64::MAX));
        assert_eq!(
            take_request_id(&mut next_request_id),
            Err(BlockDeviceError::ArithmeticOverflow)
        );
    }

    #[test]
    fn client_errors_map_to_block_device_errors() {
        assert_eq!(
            map_block_device_error(ClientError::ReadOnly),
            BlockDeviceError::ReadOnly
        );
        assert_eq!(
            map_block_device_error(ClientError::Range),
            BlockDeviceError::OutOfBounds
        );
        assert_eq!(
            map_block_device_error(ClientError::Transport),
            BlockDeviceError::Io
        );
    }
}
