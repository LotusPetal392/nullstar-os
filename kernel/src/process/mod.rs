pub(crate) mod elf;
pub(crate) mod pipe;
mod terminal;

mod userspace_legacy {
    include!("userspace.rs");
    include!("userspace_platform.rs");
}

pub(crate) mod userspace {
    pub use super::userspace_legacy::*;
    pub use super::userspace_legacy::platform_syscall_interrupt_entry_address
        as syscall_interrupt_entry_address;
}
