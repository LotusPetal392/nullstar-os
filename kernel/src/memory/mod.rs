pub(crate) mod allocator;
mod physical;

pub(crate) use physical::{BootInfoFrameAllocator, FRAME_SIZE, init};
