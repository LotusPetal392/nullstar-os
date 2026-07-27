use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use crate::{BlockDevice, BlockDeviceError, checked_block_range};

pub struct FileBlockDevice {
    file: File,
    block_size: usize,
    block_count: u64,
    writable: bool,
}

impl FileBlockDevice {
    pub fn open(
        path: impl AsRef<Path>,
        block_size: usize,
        writable: bool,
    ) -> Result<Self, BlockDeviceError> {
        if block_size == 0 {
            return Err(BlockDeviceError::InvalidBlockSize);
        }
        let file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(path)
            .map_err(|_| BlockDeviceError::Io)?;
        let length = file.metadata().map_err(|_| BlockDeviceError::Io)?.len();
        let block_size_u64 =
            u64::try_from(block_size).map_err(|_| BlockDeviceError::InvalidBlockSize)?;
        Ok(Self {
            file,
            block_size,
            block_count: length / block_size_u64,
            writable,
        })
    }

    pub fn byte_length(&self) -> Result<u64, BlockDeviceError> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|_| BlockDeviceError::Io)
    }

    fn seek_to_block(&mut self, first_block: u64) -> Result<(), BlockDeviceError> {
        let offset = first_block
            .checked_mul(
                u64::try_from(self.block_size).map_err(|_| BlockDeviceError::ArithmeticOverflow)?,
            )
            .ok_or(BlockDeviceError::ArithmeticOverflow)?;
        self.file
            .seek(SeekFrom::Start(offset))
            .map(|_| ())
            .map_err(|_| BlockDeviceError::Io)
    }
}

impl BlockDevice for FileBlockDevice {
    fn block_size(&self) -> usize {
        self.block_size
    }

    fn block_count(&self) -> u64 {
        self.block_count
    }

    fn read_blocks(&mut self, first_block: u64, buffer: &mut [u8]) -> Result<(), BlockDeviceError> {
        checked_block_range(self.block_size, self.block_count, first_block, buffer.len())?;
        self.seek_to_block(first_block)?;
        self.file
            .read_exact(buffer)
            .map_err(|_| BlockDeviceError::Io)
    }

    fn write_blocks(&mut self, first_block: u64, buffer: &[u8]) -> Result<(), BlockDeviceError> {
        if !self.writable {
            return Err(BlockDeviceError::ReadOnly);
        }
        checked_block_range(self.block_size, self.block_count, first_block, buffer.len())?;
        self.seek_to_block(first_block)?;
        self.file
            .write_all(buffer)
            .map_err(|_| BlockDeviceError::Io)
    }

    fn flush(&mut self) -> Result<(), BlockDeviceError> {
        self.file.sync_all().map_err(|_| BlockDeviceError::Io)
    }
}
