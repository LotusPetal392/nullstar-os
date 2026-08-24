#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod application_identity;
pub mod application_launch;
pub mod application_lifecycle;
pub mod application_permission;
pub mod application_portal;
pub mod application_resource;
pub mod application_resource_forwarding;
pub mod application_selection;
pub mod application_service;
pub mod args;
pub mod async_ipc;
pub mod block_device;
pub mod blocking_ipc;
pub mod blocking_pool;
pub mod early_log;
pub mod endpoint_transport;
pub mod environment;
pub mod filesystem;
pub mod filesystem_service;
pub mod handle;
pub mod heap;
pub mod ipc;
pub mod logctl;
pub mod logging_session;
pub mod managed_startup;
pub mod platform;
pub mod process_start;
pub mod runtime_context;
pub mod service_cleanup;
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

pub mod definition_service_probe {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/definition_service_probe.rs"
    ));
}

pub mod boot_generation_fixture {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/boot_generation_fixture.rs"
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

/// Defines a raw ELF entry point that validates an optional managed-tool
/// startup stream before invoking the program entry function.
///
/// Kernel-direct compatibility launches may omit the bootstrap handle. If the
/// handle is present, malformed authority or descriptive data terminates the
/// process before program code runs.
#[macro_export]
macro_rules! managed_tool_entry {
    ($entry:path) => {
        extern "C" fn __nullstar_managed_tool_entry(initial_stack: *const usize) -> ! {
            if $crate::managed_startup::initialize_managed_tool_start(initial_stack).is_err() {
                $crate::syscall::exit(125);
            }
            $entry(initial_stack)
        }

        $crate::entry!(__nullstar_managed_tool_entry);
    };
}

/// Defines a raw ELF entry point that requires and validates a typed,
/// capability-bearing managed-tool startup stream before program entry.
#[macro_export]
macro_rules! managed_capability_tool_entry {
    ($entry:path, $capacity:expr, $policies:expr) => {
        extern "C" fn __nullstar_managed_capability_tool_entry(initial_stack: *const usize) -> ! {
            let start = match $crate::managed_startup::receive_capability_managed_tool_start::<
                $capacity,
            >(initial_stack, $policies)
            {
                Ok(start) => start,
                Err(_) => $crate::syscall::exit(125),
            };
            $entry(initial_stack, start)
        }

        $crate::entry!(__nullstar_managed_capability_tool_entry);
    };
}

/// Defines a mandatory native-application entry point.
///
/// The application cannot reach its program entry function until the runtime
/// has validated the receive-only bootstrap channel, the typed application
/// capability context, and the trusted descriptive launch record.
#[macro_export]
macro_rules! application_entry {
    ($entry:path, $capacity:expr, $policies:expr) => {
        extern "C" fn __nullstar_application_entry(initial_stack: *const usize) -> ! {
            // SAFETY: this generated entry point receives the untouched initial
            // stack pointer directly from the process-loader entry trampoline.
            let start = match unsafe {
                $crate::application_launch::receive_application_start::<$capacity>(
                    initial_stack,
                    $policies,
                )
            } {
                Ok(start) => start,
                Err(_) => $crate::syscall::exit(125),
            };
            $entry(initial_stack, start)
        }

        $crate::entry!(__nullstar_application_entry);
    };
}

/// Defines a mixed-launch entry point for a tool that is still started
/// directly by a kernel compatibility test as well as by a capability-bearing
/// userspace manager.
#[macro_export]
macro_rules! optional_managed_capability_tool_entry {
    ($entry:path, $capacity:expr, $policies:expr) => {
        extern "C" fn __nullstar_optional_managed_capability_tool_entry(
            initial_stack: *const usize,
        ) -> ! {
            let start =
                match $crate::managed_startup::receive_optional_capability_managed_tool_start::<
                    $capacity,
                >(initial_stack, $policies)
                {
                    Ok(start) => start,
                    Err(_) => $crate::syscall::exit(125),
                };
            $entry(initial_stack, start)
        }

        $crate::entry!(__nullstar_optional_managed_capability_tool_entry);
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
