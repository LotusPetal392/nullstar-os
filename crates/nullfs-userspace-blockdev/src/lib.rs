#![no_std]

//! NullFS block-device adapter for the userspace partition service client.
//!
//! Keeping this adapter separate prevents `nullfs-blockdev`'s allocation-backed
//! helper types from being linked into allocator-free userspace binaries.

use core::cmp;

use nullfs_blockdev::{BlockDevice, BlockDeviceError};
use nullfs_format::BLOCK_SIZE;
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
    block_count: u64,
    protocol_blocks_per_block: u32,
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
        validate_durability_features(info.is_read_only(), info.features())?;
        let buffer = session
            .registered_buffer()
            .ok_or(ClientError::MissingBuffer)?;
        let geometry = adapter_geometry(
            info.logical_block_size(),
            info.block_count(),
            buffer.length(),
        )?;
        Ok(Self {
            session,
            info,
            buffer,
            next_request_id: Some(first_request_id),
            block_count: geometry.block_count,
            protocol_blocks_per_block: geometry.protocol_blocks_per_block,
            max_transfer_blocks: geometry.max_transfer_blocks,
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
        BLOCK_SIZE
    }

    fn block_count(&self) -> u64 {
        self.block_count
    }

    fn read_blocks(&mut self, first_block: u64, buffer: &mut [u8]) -> Result<(), BlockDeviceError> {
        preflight_range(BLOCK_SIZE, self.block_count, first_block, buffer.len())?;
        let max_bytes = self
            .max_transfer_blocks
            .checked_mul(BLOCK_SIZE)
            .ok_or(BlockDeviceError::ArithmeticOverflow)?;
        let mut block_offset = first_block;
        for chunk in buffer.chunks_mut(max_bytes) {
            let block_count = u32::try_from(chunk.len() / BLOCK_SIZE)
                .map_err(|_| BlockDeviceError::ArithmeticOverflow)?;
            let (protocol_offset, protocol_count) =
                protocol_range(block_offset, block_count, self.protocol_blocks_per_block)?;
            let request_id = self.take_request_id()?;
            let transferred = self
                .session
                .read_to_shared_buffer(request_id, self.info, protocol_offset, protocol_count, 0)
                .map_err(map_block_device_error)?;
            if transferred != protocol_count
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
        preflight_range(BLOCK_SIZE, self.block_count, first_block, buffer.len())?;
        let max_bytes = self
            .max_transfer_blocks
            .checked_mul(BLOCK_SIZE)
            .ok_or(BlockDeviceError::ArithmeticOverflow)?;
        let mut block_offset = first_block;
        for chunk in buffer.chunks(max_bytes) {
            if ipc::shared_memory_write(self.buffer.handle(), 0, chunk).ok() != Some(chunk.len()) {
                return Err(BlockDeviceError::Io);
            }
            let block_count = u32::try_from(chunk.len() / BLOCK_SIZE)
                .map_err(|_| BlockDeviceError::ArithmeticOverflow)?;
            let (protocol_offset, protocol_count) =
                protocol_range(block_offset, block_count, self.protocol_blocks_per_block)?;
            let request_id = self.take_request_id()?;
            let transferred = self
                .session
                .write_from_shared_buffer(request_id, self.info, protocol_offset, protocol_count, 0)
                .map_err(map_block_device_error)?;
            if transferred != protocol_count {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AdapterGeometry {
    block_count: u64,
    protocol_blocks_per_block: u32,
    max_transfer_blocks: usize,
}

fn adapter_geometry(
    protocol_block_size: u32,
    protocol_block_count: u64,
    buffer_length: usize,
) -> Result<AdapterGeometry, ClientError> {
    let protocol_block_size =
        usize::try_from(protocol_block_size).map_err(|_| ClientError::InvalidDeviceInfo)?;
    if protocol_block_size == 0 || !BLOCK_SIZE.is_multiple_of(protocol_block_size) {
        return Err(ClientError::InvalidDeviceInfo);
    }
    let protocol_blocks_per_block = BLOCK_SIZE / protocol_block_size;
    let protocol_blocks_per_block =
        u32::try_from(protocol_blocks_per_block).map_err(|_| ClientError::InvalidDeviceInfo)?;
    let block_count = protocol_block_count / u64::from(protocol_blocks_per_block);
    if block_count == 0 {
        return Err(ClientError::InvalidDeviceInfo);
    }
    let transfer_length = cmp::min(buffer_length, protocol::MAX_TRANSFER_BYTES);
    let max_transfer_blocks = cmp::min(
        transfer_length / BLOCK_SIZE,
        (u32::MAX / protocol_blocks_per_block) as usize,
    );
    if max_transfer_blocks == 0 {
        return Err(ClientError::InvalidBuffer);
    }
    Ok(AdapterGeometry {
        block_count,
        protocol_blocks_per_block,
        max_transfer_blocks,
    })
}

fn protocol_range(
    first_block: u64,
    block_count: u32,
    protocol_blocks_per_block: u32,
) -> Result<(u64, u32), BlockDeviceError> {
    let protocol_offset = first_block
        .checked_mul(u64::from(protocol_blocks_per_block))
        .ok_or(BlockDeviceError::ArithmeticOverflow)?;
    let protocol_count = block_count
        .checked_mul(protocol_blocks_per_block)
        .ok_or(BlockDeviceError::ArithmeticOverflow)?;
    Ok((protocol_offset, protocol_count))
}

fn preflight_range(
    block_size: usize,
    block_count: u64,
    first_block: u64,
    buffer_length: usize,
) -> Result<(), BlockDeviceError> {
    if block_size == 0 {
        return Err(BlockDeviceError::InvalidBlockSize);
    }
    if !buffer_length.is_multiple_of(block_size) {
        return Err(BlockDeviceError::InvalidBufferLength);
    }
    let transfer_blocks = u64::try_from(buffer_length / block_size)
        .map_err(|_| BlockDeviceError::ArithmeticOverflow)?;
    let end = first_block
        .checked_add(transfer_blocks)
        .ok_or(BlockDeviceError::ArithmeticOverflow)?;
    if first_block > block_count || end > block_count {
        return Err(BlockDeviceError::OutOfBounds);
    }
    Ok(())
}

fn validate_durability_features(read_only: bool, features: u64) -> Result<(), ClientError> {
    let required = protocol::features::WRITE | protocol::features::FLUSH;
    if !read_only && features & required != required {
        Err(ClientError::InvalidDeviceInfo)
    } else {
        Ok(())
    }
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

    use super::{
        AdapterGeometry, adapter_geometry, map_block_device_error, preflight_range, protocol_range,
        take_request_id, validate_durability_features,
    };

    #[test]
    fn adapter_geometry_aggregates_protocol_blocks_into_nullfs_blocks() {
        assert_eq!(
            adapter_geometry(protocol::INITIAL_LOGICAL_BLOCK_SIZE, 128, 4095),
            Err(ClientError::InvalidBuffer)
        );
        assert_eq!(
            adapter_geometry(protocol::INITIAL_LOGICAL_BLOCK_SIZE, 128, 4096),
            Ok(AdapterGeometry {
                block_count: 16,
                protocol_blocks_per_block: 8,
                max_transfer_blocks: 1,
            })
        );
    }

    #[test]
    fn adapter_geometry_rejects_incompatible_device_geometry() {
        assert_eq!(
            adapter_geometry(0, 128, 4096),
            Err(ClientError::InvalidDeviceInfo)
        );
        assert_eq!(
            adapter_geometry(768, 128, 4096),
            Err(ClientError::InvalidDeviceInfo)
        );
        assert_eq!(
            adapter_geometry(protocol::INITIAL_LOGICAL_BLOCK_SIZE, 7, 4096),
            Err(ClientError::InvalidDeviceInfo)
        );
    }

    #[test]
    fn adapter_geometry_ignores_incomplete_trailing_nullfs_blocks() {
        assert_eq!(
            adapter_geometry(protocol::INITIAL_LOGICAL_BLOCK_SIZE, 127, 4096),
            Ok(AdapterGeometry {
                block_count: 15,
                protocol_blocks_per_block: 8,
                max_transfer_blocks: 1,
            })
        );
    }

    #[test]
    fn adapter_geometry_caps_transfers_at_one_nullfs_block() {
        assert_eq!(protocol::MAX_TRANSFER_BYTES, 4096);
        assert_eq!(
            adapter_geometry(protocol::INITIAL_LOGICAL_BLOCK_SIZE, 128, 8192),
            Ok(AdapterGeometry {
                block_count: 16,
                protocol_blocks_per_block: 8,
                max_transfer_blocks: 1,
            })
        );
    }

    #[test]
    fn adapter_translates_nullfs_ranges_to_protocol_blocks() {
        assert_eq!(protocol_range(3, 1, 8), Ok((24, 8)));
        assert_eq!(
            protocol_range(u64::MAX, 1, 8),
            Err(BlockDeviceError::ArithmeticOverflow)
        );
        assert_eq!(
            protocol_range(0, u32::MAX, 8),
            Err(BlockDeviceError::ArithmeticOverflow)
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
        assert_eq!(preflight_range(4096, u64::MAX, u64::MAX, 0), Ok(()));
    }

    #[test]
    fn writable_devices_require_write_and_flush_support() {
        let read = protocol::features::READ;
        let write = protocol::features::WRITE;
        let flush = protocol::features::FLUSH;

        assert_eq!(validate_durability_features(true, read), Ok(()));
        assert_eq!(
            validate_durability_features(false, read | write),
            Err(ClientError::InvalidDeviceInfo)
        );
        assert_eq!(
            validate_durability_features(false, read | flush),
            Err(ClientError::InvalidDeviceInfo)
        );
        assert_eq!(
            validate_durability_features(false, read | write | flush),
            Ok(())
        );
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
