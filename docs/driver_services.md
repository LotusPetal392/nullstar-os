# Adding Driver Services to Userspace

This document outlines the process for moving kernel drivers to userspace and creating corresponding service implementations.

## Overview

The NullStar OS project is gradually moving drivers, filesystems, and services from kernel space to userspace to improve system stability and security. This process involves:

1. Creating userspace service binaries for each driver
2. Defining service IDs in the service control system
3. Integrating with the existing `sv` service management system

## Service Structure

Each driver service follows a standard structure based on existing services like VFS, tmpfs, and logging.

### 1. Service ID Definition

Add a new service ID to `userspace/src/service_control.rs`:

```rust
pub const PCI_SERVICE_ID: service_control::ServiceId =
    match service_control::ServiceId::from_bytes([
        0xXX, 0xXX, 0xXX, 0xXX, 0xXX, 0xXX, 0xXX, 0xXX,
        0xXX, 0xXX, 0xXX, 0xXX, 0xXX, 0xXX, 0xXX, 0xXX,
    ]) {
        Ok(id) => id,
        Err(_) => panic!("PCI service ID must be a canonical UUIDv4"),
    };
```

### 2. Service Binary

Create a new binary in `userspace/src/bin/drivers/`:

```rust
#![no_std]
#![no_main]

use core::{mem::size_of, slice};

use userspace::{
    args::Args,
    handle::Endpoint,
    ipc::{self, ObjectKind, Rights},
    managed_startup::{ManagedServiceIdentity, numeric_service_id, receive_managed_service_start},
    nullfs_primary_volume, platform,
    runtime_context::{CapabilityRole, StartupCapabilityPolicy},
    service_control::PCI_SERVICE_ID,
    syscall,
    vfs::protocol,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const EXECUTABLE_ID: u64 = 5; // Unique ID for PCI driver
const SERVICE_IDENTITY: ManagedServiceIdentity = ManagedServiceIdentity::new(
    EXECUTABLE_ID,
    numeric_service_id(PCI_SERVICE_ID.into_bytes()),
    EXECUTABLE_ID,
);
const READY_MESSAGE: &[u8] = b"service-ready: pci";
const CONTAINMENT_TEST_ARGUMENT: &[u8] = b"--containment-test";
const CONTAINMENT_DESCENDANT_MARKER: &[u8] =
    b"pci-service: containment descendant escaped process group\n";

fn rust_main(args: Args) -> ! {
    // Handle containment test if requested
    if args.contains(CONTAINMENT_TEST_ARGUMENT) {
        platform::containment_test(
            CONTAINMENT_DESCENDANT_MARKER,
            &SERVICE_IDENTITY,
            &StartupCapabilityPolicy::new(),
        );
    }

    let startup = receive_managed_service_start(&SERVICE_IDENTITY);
    
    // Open the service control endpoint
    let service_control_endpoint = startup
        .capability_by_role(CapabilityRole::ServiceControl)
        .expect("PCI service must have a service control capability")
        .into_endpoint();
    
    // Initialize PCI driver functionality here
    // This would typically involve:
    // 1. Setting up PCI enumeration
    // 2. Initializing driver resources
    // 3. Registering with the system
    
    // Send ready message to supervisor
    syscall::write_all(syscall::STDOUT, READY_MESSAGE).unwrap();
    
    // Enter main service loop - this would handle driver requests
    main_service_loop(service_control_endpoint);
}

fn main_service_loop(endpoint: Endpoint) -> ! {
    loop {
        // Process incoming service control requests
        match endpoint.receive() {
            Ok(request) => {
                // Handle service control requests
                // For now, just yield to allow other tasks to run
                syscall::yield_now().unwrap();
            }
            Err(_) => {
                // Handle receive error
                syscall::yield_now().unwrap();
            }
        }
    }
}
```

### 3. Integration with Service Management

Once created, the service can be managed using the `sv` command:

```bash
# Start the PCI driver service
sv start pci

# Check status
sv status pci

# Stop the service
sv stop pci

# Restart the service
sv restart pci
```

## Driver Migration Process

1. **Analyze kernel driver**: Understand what the driver does and how it interfaces with hardware
2. **Create userspace interface**: Define how the userspace service will communicate with the kernel (if needed)
3. **Implement service logic**: Add the core functionality in the service binary
4. **Add service ID**: Register the new service ID
5. **Update build configuration**: Add the new binary to Cargo.toml
6. **Test integration**: Verify that the service works with the supervisor

## Example Drivers to Move

The following drivers are candidates for userspace migration:

1. PCI driver (`kernel/src/drivers/pci.rs`)
2. AHCI driver (`kernel/src/drivers/ahci.rs`)  
3. Console driver (`kernel/src/drivers/console.rs`)
4. Serial driver (`kernel/src/drivers/serial.rs`)
5. Keyboard driver (`kernel/src/drivers/keyboard.rs`)

The following drivers have been moved to userspace:

1. PCI driver (`userspace/src/bin/drivers/pci_driver.rs`)
2. AHCI driver (`userspace/src/bin/drivers/ahci_driver.rs`)
3. Console driver (`userspace/src/bin/drivers/console_driver.rs`)
4. Serial driver (`userspace/src/bin/drivers/serial_driver.rs`)
5. Keyboard driver (`userspace/src/bin/drivers/keyboard_driver.rs`)

## Best Practices

1. **Use existing patterns**: Follow the same structure as other services
2. **Handle errors gracefully**: Services should be robust and not crash the system
3. **Implement proper logging**: Use the logging service for debugging
4. **Support service control**: Implement proper handling of start/stop/restart commands
5. **Containment testing**: Include containment tests to ensure service isolation

## Testing

Services can be tested using:
- The `sv` command-line interface
- Integration tests in the kernel that verify service communication
- Containment tests to ensure proper isolation