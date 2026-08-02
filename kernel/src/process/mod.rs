pub(crate) mod elf;
pub(crate) mod pipe;
mod terminal;

mod userspace_legacy {
    include!("userspace.rs");
    include!("userspace_platform/entry.rs");
    include!("userspace_platform/filesystem.rs");
    include!("userspace_platform/descriptors.rs");
    include!("userspace_platform/process.rs");
    include!("userspace_platform/process_group_entry.rs");
    include!("userspace_platform/capability_entry.rs");
    include!("userspace_platform/block_device_endpoint.rs");
    include!("userspace_platform/capability_grant_entry.rs");
    include!("userspace_platform/early_log_reader.rs");
    include!("userspace_platform/blocking_ipc_entry.rs");
}

pub(crate) mod userspace {
    pub use super::userspace_legacy::*;

    pub fn syscall_interrupt_entry_address() -> x86_64::VirtAddr {
        let _legacy_entry: fn() -> x86_64::VirtAddr =
            super::userspace_legacy::syscall_interrupt_entry_address;
        let _platform_entry: fn() -> x86_64::VirtAddr =
            super::userspace_legacy::platform_syscall_interrupt_entry_address;
        let _process_group_entry: fn() -> x86_64::VirtAddr =
            super::userspace_legacy::process_group_syscall_interrupt_entry_address;
        let _capability_entry: fn() -> x86_64::VirtAddr =
            super::userspace_legacy::capability_syscall_interrupt_entry_address;
        let _capability_grant_entry: fn() -> x86_64::VirtAddr =
            super::userspace_legacy::capability_grant_syscall_interrupt_entry_address;
        super::userspace_legacy::blocking_ipc_syscall_interrupt_entry_address()
    }
}
