// Shared Phase 5 blocking endpoint ABI additions.

pub const ABI_VERSION_MINOR: u64 = 3;
pub const FEATURE_ENDPOINT_WAIT: u64 = 1 << 11;

pub mod syscall {
    pub const ENDPOINT_WAIT: u64 = 47;
}
