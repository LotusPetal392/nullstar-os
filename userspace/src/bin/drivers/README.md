# Userspace Drivers

This directory contains provisional managed-service entry points for future userspace drivers.
The active hardware implementations still live in the kernel.

## Available Drivers

- **ahci_driver**: AHCI storage controller driver for SATA disk access
- **console_driver**: Console display driver for text output
- **serial_driver**: Serial communication driver for UART interfaces
- **keyboard_driver**: Keyboard input driver for PS/2 or USB keyboards
- **pci_driver**: PCI bus enumeration and management driver

## Driver Structure

Each entry point delegates to the allocation-free `driver_service` harness, which:

1. validates the managed startup record and exact service identity;
2. accepts only send-only readiness and receive-only request endpoints;
3. sends a readiness message after successful validation; and
4. drains request messages and closes any transferred authority until a device protocol exists.

## Implementation Status

These targets are buildable startup skeletons, not hardware drivers:

### ahci_driver
- **Status**: Managed startup skeleton
- **TODO**: Define the block-device hardware authority contract, then migrate AHCI safely

### console_driver  
- **Status**: Managed startup skeleton
- **TODO**: Define compositor/display-device protocols and framebuffer authority

### serial_driver
- **Status**: Managed startup skeleton
- **TODO**: Define UART device and interrupt protocols

### keyboard_driver
- **Status**: Managed startup skeleton
- **TODO**: Define input-device and interrupt protocols

### pci_driver
- **Status**: Managed startup skeleton
- **TODO**: Define PCI configuration-space and device-delegation protocols

## Hardware Access Considerations

In a complete implementation, these drivers would need:
- Proper hardware access mechanisms (MMIO, I/O ports)
- DMA buffer management
- Interrupt handling
- Device-specific initialization sequences
- Error handling and recovery
