# Userspace Drivers

This directory contains userspace implementations of various system drivers for the nullstar-os operating system.

## Available Drivers

- **ahci_driver**: AHCI storage controller driver for SATA disk access
- **console_driver**: Console display driver for text output
- **serial_driver**: Serial communication driver for UART interfaces
- **keyboard_driver**: Keyboard input driver for PS/2 or USB keyboards
- **pci_driver**: PCI bus enumeration and management driver

## Driver Structure

Each driver follows a common pattern:
1. Implements the standard service interface with `service_control` capability
2. Uses `managed_startup` to initialize properly in the userspace environment
3. Sends a "service-ready" message upon successful initialization
4. Runs an event loop to handle service control requests

## Implementation Status

These are skeleton implementations that demonstrate the expected structure and interfaces:

### ahci_driver
- **Status**: Partial implementation with register definitions and data structures
- **TODO**: Add actual PCI enumeration, MMIO access, and command execution

### console_driver  
- **Status**: Basic skeleton with placeholder for framebuffer access
- **TODO**: Implement actual display initialization and text output

### serial_driver
- **Status**: Basic skeleton with UART register definitions
- **TODO**: Add actual UART hardware access and configuration

### keyboard_driver
- **Status**: Basic skeleton with PS/2 scan code definitions  
- **TODO**: Add actual keyboard hardware interface and scan code processing

### pci_driver
- **Status**: Basic skeleton with PCI configuration space definitions
- **TODO**: Add actual PCI enumeration capabilities for userspace

## Hardware Access Considerations

In a complete implementation, these drivers would need:
- Proper hardware access mechanisms (MMIO, I/O ports)
- DMA buffer management
- Interrupt handling
- Device-specific initialization sequences
- Error handling and recovery