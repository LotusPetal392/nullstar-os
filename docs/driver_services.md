# Userspace driver-service scaffolding

NullStar intends to move suitable drivers out of the kernel, but the active PCI, AHCI, console,
serial, and keyboard implementations still run in the kernel. Their userspace binaries are
buildable managed-startup scaffolds; they do not yet own hardware or replace the kernel drivers.

## Current contract

Each provisional binary delegates to
`userspace::driver_service::run_provisional_driver_service`. The allocation-free harness:

1. validates the complete PID 1-authenticated managed-start record and exact service identity;
2. accepts exactly one send-only readiness endpoint and one receive-only request endpoint;
3. sends readiness only after both capabilities have been adopted; and
4. drains request packets and immediately closes attached capabilities until a device-specific
   protocol exists.

PCI, AHCI, console, serial, and keyboard each have a distinct canonical UUIDv4 in
`userspace/src/service_control.rs`. These IDs are stable routing names, not authority.

An entry point remains deliberately small:

```rust
#![no_std]
#![no_main]

use userspace::{
    driver_service::run_provisional_driver_service,
    managed_startup::{ManagedServiceIdentity, numeric_service_id},
    service_control::PCI_SERVICE_ID,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const EXECUTABLE_ID: u64 = 11;
const SERVICE_IDENTITY: ManagedServiceIdentity = ManagedServiceIdentity::new(
    EXECUTABLE_ID,
    numeric_service_id(PCI_SERVICE_ID.into_bytes()),
    EXECUTABLE_ID,
);

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    unsafe {
        run_provisional_driver_service(
            initial_stack,
            SERVICE_IDENTITY,
            b"service-ready: pci",
        )
    }
}
```

## Migration requirements

A skeleton must not be treated as a completed driver migration. Each driver still needs:

1. a bounded, versioned device protocol;
2. kernel-delegated MMIO, I/O-port, interrupt, and DMA capabilities as appropriate;
3. driver-specific request validation and recovery behavior;
4. a service definition and supervisor launch path;
5. integration with dependent services without fallback ambient hardware access; and
6. protocol, containment, device-loss, and restart acceptance tests.

Only after those pieces exist should the corresponding in-kernel implementation be retired and the
service be exposed through `sv`.

## Compile-time gates

```bash
cargo +nightly-2026-02-01 check -p userspace --bins --target x86_64-unknown-none --locked
cargo +nightly-2026-02-01 clippy -p userspace --bins --target x86_64-unknown-none --locked -- -D warnings
```

Runtime `sv` and hardware tests must be added as part of each actual migration.
