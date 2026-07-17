use core::fmt;

/// Minimal synchronous block-device interface used by early storage drivers.
pub trait BlockDevice {
    type Error: fmt::Debug;

    fn block_size(&self) -> usize;
    fn block_count(&self) -> u64;
    fn read_block(&mut self, logical_block_address: u64, buffer: &mut [u8])
        -> Result<(), Self::Error>;
}
