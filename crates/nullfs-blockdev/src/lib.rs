#![cfg_attr(not(feature = "std"), no_std)]

//! Checked block-device interfaces shared by NullFS core code and host tools.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

#[cfg(feature = "std")]
mod file;
#[cfg(feature = "std")]
pub use file::FileBlockDevice;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockDeviceError {
    InvalidBlockSize,
    InvalidBufferLength,
    OutOfBounds,
    ReadOnly,
    ArithmeticOverflow,
    Io,
}

impl fmt::Display for BlockDeviceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidBlockSize => "invalid block size",
            Self::InvalidBufferLength => "buffer length is not a whole number of blocks",
            Self::OutOfBounds => "block access is outside the device",
            Self::ReadOnly => "block device is read-only",
            Self::ArithmeticOverflow => "block address arithmetic overflowed",
            Self::Io => "block-device I/O failed",
        })
    }
}

pub trait BlockDevice {
    fn block_size(&self) -> usize;
    fn block_count(&self) -> u64;
    fn read_blocks(&mut self, first_block: u64, buffer: &mut [u8]) -> Result<(), BlockDeviceError>;
    fn write_blocks(&mut self, first_block: u64, buffer: &[u8]) -> Result<(), BlockDeviceError>;
    fn flush(&mut self) -> Result<(), BlockDeviceError>;
}

pub fn checked_block_range(
    block_size: usize,
    block_count: u64,
    first_block: u64,
    buffer_length: usize,
) -> Result<core::ops::Range<usize>, BlockDeviceError> {
    if block_size == 0 {
        return Err(BlockDeviceError::InvalidBlockSize);
    }
    if !buffer_length.is_multiple_of(block_size) {
        return Err(BlockDeviceError::InvalidBufferLength);
    }
    let transfer_blocks = u64::try_from(buffer_length / block_size)
        .map_err(|_| BlockDeviceError::ArithmeticOverflow)?;
    let end_block = first_block
        .checked_add(transfer_blocks)
        .ok_or(BlockDeviceError::ArithmeticOverflow)?;
    if end_block > block_count {
        return Err(BlockDeviceError::OutOfBounds);
    }
    let start = usize::try_from(first_block)
        .ok()
        .and_then(|block| block.checked_mul(block_size))
        .ok_or(BlockDeviceError::ArithmeticOverflow)?;
    let end = start
        .checked_add(buffer_length)
        .ok_or(BlockDeviceError::ArithmeticOverflow)?;
    Ok(start..end)
}

#[derive(Debug, Clone)]
pub struct MemoryBlockDevice {
    block_size: usize,
    block_count: u64,
    bytes: Vec<u8>,
}

impl MemoryBlockDevice {
    pub fn new(block_size: usize, block_count: u64) -> Result<Self, BlockDeviceError> {
        if block_size == 0 {
            return Err(BlockDeviceError::InvalidBlockSize);
        }
        let length = usize::try_from(block_count)
            .ok()
            .and_then(|count| count.checked_mul(block_size))
            .ok_or(BlockDeviceError::ArithmeticOverflow)?;
        Ok(Self {
            block_size,
            block_count,
            bytes: vec![0; length],
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl BlockDevice for MemoryBlockDevice {
    fn block_size(&self) -> usize {
        self.block_size
    }

    fn block_count(&self) -> u64 {
        self.block_count
    }

    fn read_blocks(&mut self, first_block: u64, buffer: &mut [u8]) -> Result<(), BlockDeviceError> {
        let range =
            checked_block_range(self.block_size, self.block_count, first_block, buffer.len())?;
        buffer.copy_from_slice(&self.bytes[range]);
        Ok(())
    }

    fn write_blocks(&mut self, first_block: u64, buffer: &[u8]) -> Result<(), BlockDeviceError> {
        let range =
            checked_block_range(self.block_size, self.block_count, first_block, buffer.len())?;
        self.bytes[range].copy_from_slice(buffer);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BlockDeviceError> {
        Ok(())
    }
}

pub struct ReadOnlyBlockDevice<D> {
    inner: D,
}

impl<D> ReadOnlyBlockDevice<D> {
    pub const fn new(inner: D) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> D {
        self.inner
    }
}

impl<D: BlockDevice> BlockDevice for ReadOnlyBlockDevice<D> {
    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    fn block_count(&self) -> u64 {
        self.inner.block_count()
    }

    fn read_blocks(&mut self, first_block: u64, buffer: &mut [u8]) -> Result<(), BlockDeviceError> {
        self.inner.read_blocks(first_block, buffer)
    }

    fn write_blocks(&mut self, _first_block: u64, _buffer: &[u8]) -> Result<(), BlockDeviceError> {
        Err(BlockDeviceError::ReadOnly)
    }

    fn flush(&mut self) -> Result<(), BlockDeviceError> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockDevice, BlockDeviceError, MemoryBlockDevice, ReadOnlyBlockDevice};

    #[test]
    fn memory_device_checks_ranges_and_round_trips() {
        let mut device = MemoryBlockDevice::new(4096, 4).expect("memory device");
        let input = [0x5a; 4096];
        device.write_blocks(2, &input).expect("write");
        let mut output = [0; 4096];
        device.read_blocks(2, &mut output).expect("read");
        assert_eq!(output, input);
        assert_eq!(
            device.read_blocks(4, &mut output),
            Err(BlockDeviceError::OutOfBounds)
        );
        assert_eq!(
            device.write_blocks(0, &[0; 3]),
            Err(BlockDeviceError::InvalidBufferLength)
        );
    }

    #[test]
    fn read_only_adapter_rejects_writes() {
        let device = MemoryBlockDevice::new(4096, 1).expect("memory device");
        let mut device = ReadOnlyBlockDevice::new(device);
        assert_eq!(
            device.write_blocks(0, &[0; 4096]),
            Err(BlockDeviceError::ReadOnly)
        );
    }
}
