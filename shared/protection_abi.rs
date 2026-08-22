// Phase-one protection ABI extensions shared by the kernel capability entry
// layer and the userspace IPC facade. These constants live separately from the
// original ABI file so the protection layer can evolve without coupling its
// implementation to the legacy process-manager source layout.

pub mod syscall {
    /// Copy a restricted capability into a live direct child's handle table.
    ///
    /// Arguments: target PID, source handle, rights mask, requested child
    /// slot. A requested slot of zero asks the kernel to allocate any free slot.
    /// The return value is the child's opaque generation-checked handle.
    pub const GRANT_CHILD: u64 = 48;
}
