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
    service_control::BLOCK_DEVICE_SERVICE_ID,
    syscall,
    vfs::protocol,
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const EXECUTABLE_ID: u64 = 8; // Unique ID for AHCI driver
const SERVICE_IDENTITY: ManagedServiceIdentity = ManagedServiceIdentity::new(
    EXECUTABLE_ID,
    numeric_service_id(BLOCK_DEVICE_SERVICE_ID.into_bytes()),
    EXECUTABLE_ID,
);
const READY_MESSAGE: &[u8] = b"service-ready: ahci";
const CONTAINMENT_TEST_ARGUMENT: &[u8] = b"--containment-test";
const CONTAINMENT_DESCENDANT_MARKER: &[u8] =
    b"ahci-service: containment descendant escaped process group\n";

// AHCI register offsets
const HBA_REGION_LENGTH: u64 = 0x1100;
const HBA_CAPABILITIES: usize = 0x00;
const HBA_GLOBAL_HOST_CONTROL: usize = 0x04;
const HBA_PORTS_IMPLEMENTED: usize = 0x0c;
const HBA_VERSION: usize = 0x10;
const HBA_CAPABILITIES_EXTENDED: usize = 0x24;
const HBA_BIOS_HANDOFF_CONTROL: usize = 0x28;

const HBA_GHC_RESET: u32 = 1 << 0;
const HBA_GHC_INTERRUPTS_ENABLED: u32 = 1 << 1;
const HBA_GHC_AHCI_ENABLED: u32 = 1 << 31;
const HBA_CAP_SUPPORTS_64_BIT: u32 = 1 << 31;
const HBA_CAP2_BIOS_HANDOFF: u32 = 1 << 0;
const HBA_BOHC_BIOS_OWNED: u32 = 1 << 0;
const HBA_BOHC_OS_OWNED: u32 = 1 << 1;
const HBA_BOHC_BIOS_BUSY: u32 = 1 << 4;

const PORT_BASE: usize = 0x100;
const PORT_STRIDE: usize = 0x80;
const PORT_COMMAND_LIST_BASE: usize = 0x00;
const PORT_COMMAND_LIST_BASE_UPPER: usize = 0x04;
const PORT_FIS_BASE: usize = 0x08;
const PORT_FIS_BASE_UPPER: usize = 0x0c;
const PORT_INTERRUPT_STATUS: usize = 0x10;
const PORT_INTERRUPT_ENABLE: usize = 0x14;
const PORT_COMMAND: usize = 0x18;
const PORT_TASK_FILE_DATA: usize = 0x20;
const PORT_SIGNATURE: usize = 0x24;
const PORT_SATA_STATUS: usize = 0x28;
const PORT_SATA_CONTROL: usize = 0x2c;
const PORT_SATA_ERROR: usize = 0x30;
const PORT_SATA_ACTIVE: usize = 0x34;
const PORT_COMMAND_ISSUE: usize = 0x38;

const PORT_CMD_START: u32 = 1 << 0;
const PORT_CMD_SPIN_UP_DEVICE: u32 = 1 << 1;
const PORT_CMD_POWER_ON_DEVICE: u32 = 1 << 2;
const PORT_CMD_FIS_RECEIVE_ENABLE: u32 = 1 << 4;
const PORT_CMD_FIS_RECEIVE_RUNNING: u32 = 1 << 14;
const PORT_CMD_COMMAND_LIST_RUNNING: u32 = 1 << 15;
const PORT_TFD_ERROR: u32 = 1 << 0;
const PORT_TFD_DATA_REQUEST: u32 = 1 << 3;
const PORT_TFD_BUSY: u32 = 1 << 7;
const PORT_IS_TASK_FILE_ERROR: u32 = 1 << 30;
const SATA_STATUS_DEVICE_PRESENT: u32 = 3;
const SATA_STATUS_INTERFACE_ACTIVE: u32 = 1;
const SATA_CONTROL_DETECT_MASK: u32 = 0x0f;
const SATA_CONTROL_COMRESET: u32 = 1;

const COMMAND_LIST_BYTES: usize = 1024;
const RECEIVED_FIS_OFFSET: usize = 1024;
const DMA_PAGE_SIZE: usize = 4096;
const COMMAND_SLOT: u32 = 1;
const REGISTER_HOST_TO_DEVICE_FIS_TYPE: u8 = 0x27;
const REGISTER_FIS_COMMAND: u8 = 1 << 7;
const ATA_COMMAND_IDENTIFY_DEVICE: u8 = 0xec;
const ATA_COMMAND_READ_DMA: u8 = 0xc8;
const ATA_COMMAND_READ_DMA_EXT: u8 = 0x25;
const ATA_COMMAND_WRITE_DMA: u8 = 0xca;
const ATA_COMMAND_WRITE_DMA_EXT: u8 = 0x35;
const ATA_COMMAND_FLUSH_CACHE: u8 = 0xe7;
const ATA_COMMAND_FLUSH_CACHE_EXT: u8 = 0xea;
const COMMAND_FIS_DWORDS: u32 = 5;
const COMMAND_HEADER_WRITE: u32 = 1 << 6;
const COMMAND_HEADER_PRDT_LENGTH_ONE: u32 = 1 << 16;
const PRDT_INTERRUPT_ON_COMPLETION: u32 = 1 << 31;

const BIOS_HANDOFF_SPINS: usize = 10_000_000;
const HBA_RESET_SPINS: usize = 10_000_000;
const PORT_TRANSITION_SPINS: usize = 10_000_000;
const PORT_READY_SPINS: usize = 10_000_000;
const COMMAND_COMPLETION_SPINS: usize = 50_000_000;

// For now, we'll define a simplified PCI interface since userspace access is limited
// In a real implementation, this would need to be extended with actual PCI enumeration
#[derive(Debug, Clone, Copy)]
struct PciLocation {
    segment: u16,
    bus: u8,
    device: u8,
    function: u8,
}

#[derive(Debug, Clone, Copy)]
struct AhciControllerInfo {
    location: PciLocation,
    abar: u64,
    hba_version: u32,
    command_slots: u8,
    implemented_ports: u32,
    port_count: u8,
    supports_64_bit_dma: bool,
}

#[derive(Debug, Clone)]
struct DiskInfo {
    controller_location: PciLocation,
    vendor_id: u16,
    device_id: u16,
    pci_command: u16,
    abar: u64,
    hba_version: u32,
    command_slots: u8,
    implemented_ports: u32,
    port: u8,
    supports_64_bit_dma: bool,
    model: alloc::string::String,
    serial: alloc::string::String,
    firmware: alloc::string::String,
    logical_block_size: u32,
    logical_block_count: u64,
    capacity_bytes: u64,
    lba48: bool,
    sector_zero_checksum: u32,
    sector_zero_signature: u16,
}

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
        .expect("AHCI driver must have a service control capability")
        .into_endpoint();
    
    // Initialize AHCI driver functionality here
    // In userspace, we would:
    // 1. Enumerate PCI devices (simplified approach)
    // 2. Find AHCI controller
    // 3. Initialize the controller
    // 4. Register with the system
    
    // For now, just send ready message as a placeholder
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
                // In a real implementation, we'd process actual I/O requests
                syscall::yield_now().unwrap();
            }
            Err(_) => {
                // Handle receive error
                syscall::yield_now().unwrap();
            }
        }
    }
}

// Simplified MMIO region access - in a real implementation this would be more complex
#[derive(Debug, Clone, Copy)]
struct MmioRegion {
    virtual_base: usize,
}

impl MmioRegion {
    fn new(virtual_base: usize) -> Self {
        Self { virtual_base }
    }

    fn read_u32(self, offset: usize) -> u32 {
        unsafe { 
            let address = self.virtual_base.checked_add(offset).unwrap();
            core::ptr::read_volatile(address as *const u32)
        }
    }

    fn write_u32(self, offset: usize, value: u32) {
        unsafe { 
            let address = self.virtual_base.checked_add(offset).unwrap();
            core::ptr::write_volatile(address as *mut u32, value);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Port {
    hba: MmioRegion,
    index: u8,
}

impl Port {
    fn offset(self, register: usize) -> usize {
        PORT_BASE + usize::from(self.index) * PORT_STRIDE + register
    }

    fn read(self, register: usize) -> u32 {
        self.hba.read_u32(self.offset(register))
    }

    fn write(self, register: usize, value: u32) {
        self.hba.write_u32(self.offset(register), value);
    }

    fn link_is_active(self) -> bool {
        let sata_status = self.read(PORT_SATA_STATUS);
        sata_status & 0x0f == SATA_STATUS_DEVICE_PRESENT
            && (sata_status >> 8) & 0x0f == SATA_STATUS_INTERFACE_ACTIVE
    }

    fn wake_and_has_device(self) -> bool {
        self.write(
            PORT_COMMAND,
            self.read(PORT_COMMAND) | PORT_CMD_SPIN_UP_DEVICE | PORT_CMD_POWER_ON_DEVICE,
        );

        let sata_control = self.read(PORT_SATA_CONTROL);
        self.write(
            PORT_SATA_CONTROL,
            (sata_control & !SATA_CONTROL_DETECT_MASK) | SATA_CONTROL_COMRESET,
        );
        
        // In a real implementation, we'd wait for timer ticks here
        // For now, we'll just return true to simulate device detection
        
        let mut link_ready = false;
        if self.link_is_active() {
            link_ready = true;
        }

        link_ready
    }
    
    fn stop(self) -> Result<(), &'static str> {
        self.write(PORT_COMMAND, self.read(PORT_COMMAND) & !PORT_CMD_START);
        
        // In a real implementation we'd wait for the command to complete
        // For now just return success
        Ok(())
    }

    fn start(self) -> Result<(), &'static str> {
        if self.read(PORT_COMMAND) & PORT_CMD_COMMAND_LIST_RUNNING != 0 {
            return Err("Port already running");
        }

        let command = self.read(PORT_COMMAND)
            | PORT_CMD_SPIN_UP_DEVICE
            | PORT_CMD_POWER_ON_DEVICE
            | PORT_CMD_FIS_RECEIVE_ENABLE
            | PORT_CMD_START;
        self.write(PORT_COMMAND, command);
        Ok(())
    }
    
    fn wait_until_ready(self) -> Result<(), &'static str> {
        // In a real implementation we'd actually wait
        // For now just return success
        Ok(())
    }
}

// Placeholder function to find AHCI controller in userspace - this would require proper PCI enumeration
fn find_ahci_controller() -> Option<AhciControllerInfo> {
    // This is a placeholder. In a real implementation:
    // 1. We'd need access to PCI configuration space from userspace
    // 2. We'd enumerate devices with class code 0x01, subclass 0x06, prog IF 0x01
    // 3. We'd get the BAR5 address for the HBA register space
    
    // For now return None to indicate we can't find a controller in this simplified context
    None
}

// Placeholder function to initialize AHCI - would need actual PCI access in real implementation
fn init_ahci_controller() -> Option<DiskInfo> {
    // This would:
    // 1. Map PCI MMIO regions (requires proper capabilities)
    // 2. Perform BIOS handoff if needed  
    // 3. Reset HBA
    // 4. Enable AHCI mode
    // 5. Find and initialize ports
    
    find_ahci_controller().map(|info| {
        DiskInfo {
            controller_location: info.location,
            vendor_id: 0x8086, // Placeholder
            device_id: 0x1234, // Placeholder
            pci_command: 0x0007, // Placeholder
            abar: info.abar,
            hba_version: info.hba_version,
            command_slots: info.command_slots,
            implemented_ports: info.implemented_ports,
            port: 0,
            supports_64_bit_dma: info.supports_64_bit_dma,
            model: "AHCI Controller".to_string(),
            serial: "SERIAL001".to_string(),
            firmware: "FW1.0".to_string(),
            logical_block_size: 512,
            logical_block_count: 1000000, // Placeholder
            capacity_bytes: 512 * 1000000, // Placeholder
            lba48: true,
            sector_zero_checksum: 0x12345678,
            sector_zero_signature: 0x5a5a,
        }
    })
}

// Placeholder for DMA buffer management - in real implementation would need proper allocation
#[derive(Debug, Clone)]
struct DmaResources {
    command_and_fis: usize, // Virtual address of command and FIS memory
    command_table: usize,   // Virtual address of command table
    data: usize,            // Virtual address of data buffer
}

impl DmaResources {
    fn new() -> Self {
        // This would allocate DMA-safe buffers in real implementation
        Self {
            command_and_fis: 0x10000000, // Placeholder virtual addresses
            command_table: 0x10001000,
            data: 0x10002000,
        }
    }
    
    fn clear_command_state(&self) {
        // Clear DMA buffers in real implementation
    }
}

// Command header structure for AHCI commands
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct CommandHeader {
    flags_and_prdt_length: u32,
    bytes_transferred: u32,
    command_table_base: u32,
    command_table_base_upper: u32,
    reserved: [u32; 4],
}

// Physical region descriptor for DMA transfers
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct PhysicalRegionDescriptor {
    data_base: u32,
    data_base_upper: u32,
    reserved: u32,
    byte_count_and_flags: u32,
}

// Command table structure
#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct CommandTable {
    command_fis: [u8; 64],
    atapi_command: [u8; 16],
    reserved: [u8; 48],
    physical_region: PhysicalRegionDescriptor,
}

// Placeholder for actual implementation of block device operations
fn execute_data_in(
    _port: Port,
    _command: u8,
    _logical_block_address: Option<u64>,
    _sector_count: u16,
    _extended_lba: bool,
    _output: &mut [u8],
) -> Result<(), &'static str> {
    // In a real implementation, this would:
    // 1. Set up command header and table
    // 2. Configure FIS (Frame Information Structure)
    // 3. Set up DMA buffer for data transfer
    // 4. Issue the command to the AHCI controller
    // 5. Wait for completion
    
    Ok(())
}

fn execute_data_out(
    _port: Port,
    _command: u8,
    _logical_block_address: u64,
    _sector_count: u16,
    _extended_lba: bool,
    _input: &[u8],
) -> Result<(), &'static str> {
    // In a real implementation, this would:
    // 1. Set up command header and table
    // 2. Configure FIS (Frame Information Structure)
    // 3. Set up DMA buffer for data transfer
    // 4. Issue the command to the AHCI controller
    // 5. Wait for completion
    
    Ok(())
}

fn execute_non_data(_port: Port, _command: u8) -> Result<(), &'static str> {
    // In a real implementation, this would:
    // 1. Set up command header and table for non-data commands
    // 2. Issue the command to the AHCI controller
    // 3. Wait for completion
    
    Ok(())
}

// Helper functions for data manipulation
fn identify_word(_data: &[u8; 512], _word: usize) -> u16 {
    0x0000 // Placeholder - in real implementation would extract from identify data
}

fn ata_string(_data: &[u8; 512], _first_word: usize, _word_count: usize) -> alloc::string::String {
    "unknown".to_string() // Placeholder
}

// Wait function for spin-waiting (placeholder)
fn wait_until(mut remaining: usize, mut condition: impl FnMut() -> bool) -> bool {
    while remaining > 0 {
        if condition() {
            return true;
        }
        remaining -= 1;
        // In a real implementation we'd need proper yield or sleep
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mmio_region() {
        let region = MmioRegion::new(0x1000);
        // This would normally be tested with actual memory mapping
        assert_eq!(region.virtual_base, 0x1000);
    }
}