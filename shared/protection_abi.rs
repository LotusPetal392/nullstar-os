// Phase-one protection ABI extensions shared by the kernel capability entry
// layer and the userspace IPC facade. These constants live separately from the
// original ABI file so the protection layer can evolve without coupling its
// implementation to the legacy process-manager source layout.

pub mod syscall {
    /// Copy a restricted capability into a live direct child's handle table.
    ///
    /// Arguments: target PID, source handle, rights mask, requested child
    /// handle. A requested handle of zero asks the kernel to allocate a slot.
    pub const GRANT_CHILD: u64 = 47;
}
