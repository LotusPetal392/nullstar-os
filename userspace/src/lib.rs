#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod args;
pub mod block_device;
pub mod blocking_ipc;
pub mod early_log;
pub mod endpoint_transport;
pub mod environment;
pub mod filesystem;
pub mod filesystem_service;
pub mod heap;
pub mod ipc;
pub mod logctl;
pub mod logging_session;
pub mod platform;
pub mod service_control;
pub mod service_route;
pub mod supervisor;
pub mod sv;
#[path = "syscall_facade.rs"]
pub mod syscall;
#[path = "syscall.rs"]
mod syscall_legacy;
pub mod tmpfs;
pub mod vfs;

pub mod abi {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/userspace_abi.rs"
    ));
}

pub mod nullfs_primary_volume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/nullfs_primary_volume.rs"
    ));
}

/// Defines the raw ELF entry point for a Rust userspace program.
///
/// The supplied function receives the original kernel-built initial stack pointer.
#[macro_export]
macro_rules! entry {
    ($entry:path) => {
        core::arch::global_asm!(
            r#"
            .section .text._start,"ax"
            .p2align 4
            .global _start
            .type _start,@function
        _start:
            cld
            mov rdi, rsp
            and rsp, -16
            call {entry}
            ud2
        .size _start, .-_start
        "#,
            entry = sym $entry,
        );
    };
}

/// Installs a minimal panic handler that terminates the current process.
#[macro_export]
macro_rules! panic_handler {
    () => {
        #[panic_handler]
        fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
            $crate::syscall::exit(101)
        }
    };
}
