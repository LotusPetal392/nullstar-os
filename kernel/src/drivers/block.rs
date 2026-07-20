use core::fmt;

/// Minimal synchronous block-device interface used by early storage drivers.
///
/// Buffers always describe exactly one logical block as reported by the device.
/// Implementations may block the calling kernel thread until the transfer completes.
pub trait BlockDevice {
    type Error: fmt::Debug;

    /// Number of bytes in one logical block.
    fn block_size(&self) -> usize;

    /// Total addressable logical blocks.
    fn block_count(&self) -> u64;

    /// Read exactly one logical block into `buffer`.
    fn read_block(
        &mut self,
        logical_block_address: u64,
        buffer: &mut [u8],
    ) -> Result<(), Self::Error>;

    /// Write exactly one logical block from `buffer`.
    fn write_block(&mut self, logical_block_address: u64, buffer: &[u8])
    -> Result<(), Self::Error>;

    /// Ensure all previously completed writes are durable on the device.
    fn flush(&mut self) -> Result<(), Self::Error>;
}
