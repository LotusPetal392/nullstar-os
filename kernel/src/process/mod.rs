pub(crate) mod elf;
pub(crate) mod pipe;
mod terminal;

mod userspace_legacy {
    include!("userspace.rs");
    include!("userspace_platform/entry.rs");
    include!("userspace_platform/filesystem.rs");
    include!("userspace_platform/descriptors.rs");
    include!("userspace_platform/process.rs");
}

pub(crate) mod userspace {
    pub use super::userspace_legacy::*;

    pub fn syscall_interrupt_entry_address() -> x86_64::VirtAddr {
        let _legacy_entry: fn() -> x86_64::VirtAddr =
            super::userspace_legacy::syscall_interrupt_entry_address;
        super::userspace_legacy::platform_syscall_interrupt_entry_address()
    }
}
