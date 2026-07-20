use alloc::string::String;
use core::{
    fmt,
    hint::spin_loop,
    mem::size_of,
    ptr,
    sync::atomic::{Ordering, compiler_fence},
};

use spin::Mutex;
use x86_64::{VirtAddr, structures::paging::FrameAllocator};

use crate::{
    acpi::McfgInfo,
    memory::{BootInfoFrameAllocator, FRAME_SIZE},
};

use super::{
    block::BlockDevice,
    pci::{self, Function, Inventory, Location},
};

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

static DEVICE: Mutex<Option<Controller>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    AlreadyInitialized,
    NotInitialized,
    ControllerNotFound,
    InvalidAbar,
    UnsupportedAbar,
    Pci(pci::Error),
    AddressOverflow,
    HbaOutsidePhysicalMap,
    BiosHandoffTimeout,
    HbaResetTimeout,
    AhciEnableFailed,
    NoSataDisk,
    PortStopTimeout,
    FrameAllocationFailed,
    DmaAddressUnsupported,
    PortBusyTimeout,
    CommandSlotBusy,
    CommandTimeout,
    TaskFileError,
    TransferIncomplete { expected: usize, actual: usize },
    IdentifyDataInvalid,
    UnsupportedLogicalBlockSize(u32),
    BufferLength { expected: usize, actual: usize },
    LbaOutOfRange,
}

impl Error {
    pub const fn description(self) -> &'static str {
        match self {
            Self::AlreadyInitialized => "AHCI storage is already initialized",
            Self::NotInitialized => "AHCI storage is not initialized",
            Self::ControllerNotFound => "no AHCI SATA controller was found",
            Self::InvalidAbar => "AHCI controller BAR5 is invalid",
            Self::UnsupportedAbar => "AHCI controller BAR5 is not a supported 32-bit memory BAR",
            Self::Pci(error) => error.description(),
            Self::AddressOverflow => "AHCI address calculation overflowed",
            Self::HbaOutsidePhysicalMap => {
                "AHCI register region is outside the bootloader physical mapping"
            }
            Self::BiosHandoffTimeout => "AHCI BIOS ownership handoff timed out",
            Self::HbaResetTimeout => "AHCI host reset timed out",
            Self::AhciEnableFailed => "AHCI mode could not be enabled",
            Self::NoSataDisk => "AHCI controller did not expose a ready SATA disk",
            Self::PortStopTimeout => "AHCI port command engine did not stop",
            Self::FrameAllocationFailed => "physical frame allocation for AHCI DMA failed",
            Self::DmaAddressUnsupported => "AHCI controller cannot address an allocated DMA frame",
            Self::PortBusyTimeout => "AHCI port remained busy before command submission",
            Self::CommandSlotBusy => "AHCI command slot zero is busy",
            Self::CommandTimeout => "AHCI command did not complete",
            Self::TaskFileError => "AHCI reported an ATA task-file error",
            Self::TransferIncomplete { .. } => "AHCI completed an incomplete DMA transfer",
            Self::IdentifyDataInvalid => "ATA IDENTIFY data is invalid",
            Self::UnsupportedLogicalBlockSize(_) => "ATA logical block size is unsupported",
            Self::BufferLength { .. } => {
                "block read buffer length does not match the disk block size"
            }
            Self::LbaOutOfRange => "logical block address is outside the disk",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pci(error) => write!(formatter, "PCI configuration error: {error}"),
            Self::TransferIncomplete { expected, actual } => {
                write!(formatter, "AHCI transferred {actual} of {expected} bytes")
            }
            Self::UnsupportedLogicalBlockSize(bytes) => {
                write!(
                    formatter,
                    "unsupported ATA logical block size: {bytes} bytes"
                )
            }
            Self::BufferLength { expected, actual } => {
                write!(
                    formatter,
                    "block buffer is {actual} bytes; expected {expected}"
                )
            }
            _ => formatter.write_str(self.description()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub controller_location: Location,
    pub vendor_id: u16,
    pub device_id: u16,
    pub pci_command: u16,
    pub abar: u64,
    pub hba_version: u32,
    pub command_slots: u8,
    pub implemented_ports: u32,
    pub port: u8,
    pub supports_64_bit_dma: bool,
    pub model: String,
    pub serial: String,
    pub firmware: String,
    pub logical_block_size: u32,
    pub logical_block_count: u64,
    pub capacity_bytes: u64,
    pub lba48: bool,
    pub sector_zero_checksum: u32,
    pub sector_zero_signature: u16,
}

#[derive(Clone, Copy)]
struct MmioRegion {
    virtual_base: usize,
}

impl MmioRegion {
    fn new(
        physical_address: u64,
        length: u64,
        physical_memory_offset: VirtAddr,
        physical_memory_end: u64,
    ) -> Result<Self, Error> {
        let end = physical_address
            .checked_add(length)
            .ok_or(Error::AddressOverflow)?;
        if physical_address == 0 || end > physical_memory_end {
            return Err(Error::HbaOutsidePhysicalMap);
        }

        let virtual_address = physical_memory_offset
            .as_u64()
            .checked_add(physical_address)
            .ok_or(Error::AddressOverflow)?;
        let virtual_base = usize::try_from(virtual_address).map_err(|_| Error::AddressOverflow)?;
        if virtual_base & 0x3 != 0 {
            return Err(Error::AddressOverflow);
        }

        Ok(Self { virtual_base })
    }

    fn read_u32(self, offset: usize) -> u32 {
        let address = self
            .virtual_base
            .checked_add(offset)
            .expect("validated AHCI MMIO address overflowed");
        unsafe { ptr::read_volatile(address as *const u32) }
    }

    fn write_u32(self, offset: usize, value: u32) {
        let address = self
            .virtual_base
            .checked_add(offset)
            .expect("validated AHCI MMIO address overflowed");
        unsafe { ptr::write_volatile(address as *mut u32, value) };
    }
}

#[derive(Debug, Clone, Copy)]
struct DmaPage {
    physical_address: u64,
    virtual_base: usize,
}

impl DmaPage {
    fn allocate(
        frame_allocator: &mut BootInfoFrameAllocator,
        physical_memory_offset: VirtAddr,
        physical_memory_end: u64,
        supports_64_bit_dma: bool,
    ) -> Result<Self, Error> {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(Error::FrameAllocationFailed)?;
        let physical_address = frame.start_address().as_u64();
        let physical_end = physical_address
            .checked_add(FRAME_SIZE)
            .ok_or(Error::AddressOverflow)?;
        if physical_end > physical_memory_end {
            return Err(Error::HbaOutsidePhysicalMap);
        }
        if !supports_64_bit_dma && physical_end.saturating_sub(1) > u64::from(u32::MAX) {
            return Err(Error::DmaAddressUnsupported);
        }

        let virtual_address = physical_memory_offset
            .as_u64()
            .checked_add(physical_address)
            .ok_or(Error::AddressOverflow)?;
        let virtual_base = usize::try_from(virtual_address).map_err(|_| Error::AddressOverflow)?;
        if virtual_base & (DMA_PAGE_SIZE - 1) != 0 {
            return Err(Error::AddressOverflow);
        }

        unsafe { ptr::write_bytes(virtual_base as *mut u8, 0, DMA_PAGE_SIZE) };
        Ok(Self {
            physical_address,
            virtual_base,
        })
    }

    fn zero(self) {
        unsafe { ptr::write_bytes(self.virtual_base as *mut u8, 0, DMA_PAGE_SIZE) };
    }
}

#[derive(Debug, Clone, Copy)]
struct DmaResources {
    command_and_fis: DmaPage,
    command_table: DmaPage,
    data: DmaPage,
}

impl DmaResources {
    fn allocate(
        frame_allocator: &mut BootInfoFrameAllocator,
        physical_memory_offset: VirtAddr,
        physical_memory_end: u64,
        supports_64_bit_dma: bool,
    ) -> Result<Self, Error> {
        Ok(Self {
            command_and_fis: DmaPage::allocate(
                frame_allocator,
                physical_memory_offset,
                physical_memory_end,
                supports_64_bit_dma,
            )?,
            command_table: DmaPage::allocate(
                frame_allocator,
                physical_memory_offset,
                physical_memory_end,
                supports_64_bit_dma,
            )?,
            data: DmaPage::allocate(
                frame_allocator,
                physical_memory_offset,
                physical_memory_end,
                supports_64_bit_dma,
            )?,
        })
    }

    fn command_header_ptr(self) -> *mut CommandHeader {
        self.command_and_fis.virtual_base as *mut CommandHeader
    }

    fn command_table_ptr(self) -> *mut CommandTable {
        self.command_table.virtual_base as *mut CommandTable
    }

    fn data_ptr(self) -> *mut u8 {
        self.data.virtual_base as *mut u8
    }

    fn received_fis_physical(self) -> u64 {
        self.command_and_fis.physical_address + RECEIVED_FIS_OFFSET as u64
    }

    fn clear_command_state(self) {
        self.command_and_fis.zero();
        self.command_table.zero();
        self.data.zero();
    }
}

#[derive(Clone, Copy)]
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
        crate::interrupts::wait_for_timer_tick();
        self.write(PORT_SATA_CONTROL, sata_control & !SATA_CONTROL_DETECT_MASK);
        self.write(PORT_SATA_ERROR, u32::MAX);

        let mut link_ready = false;
        for _ in 0..100 {
            if self.link_is_active() {
                link_ready = true;
                break;
            }
            crate::interrupts::wait_for_timer_tick();
        }

        crate::serial_println!(
            "AHCI port {}: ssts={:#010x}, sig={:#010x}, cmd={:#010x}, tfd={:#010x}",
            self.index,
            self.read(PORT_SATA_STATUS),
            self.read(PORT_SIGNATURE),
            self.read(PORT_COMMAND),
            self.read(PORT_TASK_FILE_DATA)
        );

        link_ready
    }
    fn link_is_active(self) -> bool {
        let sata_status = self.read(PORT_SATA_STATUS);
        sata_status & 0x0f == SATA_STATUS_DEVICE_PRESENT
            && (sata_status >> 8) & 0x0f == SATA_STATUS_INTERFACE_ACTIVE
    }
    fn stop(self) -> Result<(), Error> {
        self.write(PORT_COMMAND, self.read(PORT_COMMAND) & !PORT_CMD_START);
        if !wait_until(PORT_TRANSITION_SPINS, || {
            self.read(PORT_COMMAND) & PORT_CMD_COMMAND_LIST_RUNNING == 0
        }) {
            return Err(Error::PortStopTimeout);
        }

        self.write(
            PORT_COMMAND,
            self.read(PORT_COMMAND) & !PORT_CMD_FIS_RECEIVE_ENABLE,
        );
        if !wait_until(PORT_TRANSITION_SPINS, || {
            self.read(PORT_COMMAND) & PORT_CMD_FIS_RECEIVE_RUNNING == 0
        }) {
            return Err(Error::PortStopTimeout);
        }

        Ok(())
    }

    fn start(self) -> Result<(), Error> {
        if self.read(PORT_COMMAND) & PORT_CMD_COMMAND_LIST_RUNNING != 0 {
            return Err(Error::PortStopTimeout);
        }

        let command = self.read(PORT_COMMAND)
            | PORT_CMD_SPIN_UP_DEVICE
            | PORT_CMD_POWER_ON_DEVICE
            | PORT_CMD_FIS_RECEIVE_ENABLE
            | PORT_CMD_START;
        self.write(PORT_COMMAND, command);
        Ok(())
    }

    fn wait_until_ready(self) -> Result<(), Error> {
        if wait_until(PORT_READY_SPINS, || {
            self.read(PORT_TASK_FILE_DATA) & (PORT_TFD_BUSY | PORT_TFD_DATA_REQUEST) == 0
        }) {
            Ok(())
        } else {
            Err(Error::PortBusyTimeout)
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CommandHeader {
    flags_and_prdt_length: u32,
    bytes_transferred: u32,
    command_table_base: u32,
    command_table_base_upper: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PhysicalRegionDescriptor {
    data_base: u32,
    data_base_upper: u32,
    reserved: u32,
    byte_count_and_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CommandTable {
    command_fis: [u8; 64],
    atapi_command: [u8; 16],
    reserved: [u8; 48],
    physical_region: PhysicalRegionDescriptor,
}

struct IdentifyData {
    model: String,
    serial: String,
    firmware: String,
    logical_block_size: u32,
    logical_block_count: u64,
    lba48: bool,
}

struct Controller {
    hba: MmioRegion,
    port: Port,
    dma: DmaResources,
    logical_block_size: usize,
    logical_block_count: u64,
    lba48: bool,
    info: Option<DiskInfo>,
}

impl Controller {
    fn new(
        function: Function,
        mcfg: &McfgInfo,
        frame_allocator: &mut BootInfoFrameAllocator,
        physical_memory_offset: VirtAddr,
        physical_memory_end: u64,
    ) -> Result<Self, Error> {
        let abar = ahci_base_address(function)?;
        let pci_command = pci::enable_memory_and_bus_mastering(
            mcfg,
            function.location,
            physical_memory_offset,
            physical_memory_end,
        )
        .map_err(Error::Pci)?;
        let hba = MmioRegion::new(
            abar,
            HBA_REGION_LENGTH,
            physical_memory_offset,
            physical_memory_end,
        )?;

        perform_bios_handoff(hba)?;
        reset_hba(hba)?;

        let capabilities = hba.read_u32(HBA_CAPABILITIES);
        let supports_64_bit_dma = capabilities & HBA_CAP_SUPPORTS_64_BIT != 0;
        let command_slots = (((capabilities >> 8) & 0x1f) + 1) as u8;
        let port_count = ((capabilities & 0x1f) + 1).min(32) as u8;
        let implemented_ports = hba.read_u32(HBA_PORTS_IMPLEMENTED);
        crate::serial_println!(
            "AHCI controller: abar={:#x}, version={:#010x}, cap={:#010x}, pi={:#010x}, ports={}, slots={}, dma64={}",
            abar,
            hba.read_u32(HBA_VERSION),
            capabilities,
            implemented_ports,
            port_count,
            command_slots,
            supports_64_bit_dma
        );

        let mut selected_port = None;
        for index in 0..port_count {
            if implemented_ports & (1 << index) == 0 {
                continue;
            }
            let port = Port { hba, index };
            if port.wake_and_has_device() {
                selected_port = Some(port);
                break;
            }
        }
        let port = selected_port.ok_or(Error::NoSataDisk)?;

        let dma = DmaResources::allocate(
            frame_allocator,
            physical_memory_offset,
            physical_memory_end,
            supports_64_bit_dma,
        )?;

        let mut controller = Self {
            hba,
            port,
            dma,
            logical_block_size: 0,
            logical_block_count: 0,
            lba48: false,
            info: None,
        };
        controller.configure_port()?;

        let identify = controller.identify()?;
        controller.logical_block_size = identify.logical_block_size as usize;
        controller.logical_block_count = identify.logical_block_count;
        controller.lba48 = identify.lba48;

        let mut sector_zero = [0_u8; DMA_PAGE_SIZE];
        let block_size = controller.logical_block_size;
        controller.read_block(0, &mut sector_zero[..block_size])?;
        let sector_zero_signature = if block_size >= 512 {
            u16::from_le_bytes([sector_zero[510], sector_zero[511]])
        } else {
            0
        };
        let sector_zero_checksum = fnv1a32(&sector_zero[..block_size]);

        let info = DiskInfo {
            controller_location: function.location,
            vendor_id: function.vendor_id,
            device_id: function.device_id,
            pci_command,
            abar,
            hba_version: hba.read_u32(HBA_VERSION),
            command_slots,
            implemented_ports,
            port: port.index,
            supports_64_bit_dma,
            model: identify.model,
            serial: identify.serial,
            firmware: identify.firmware,
            logical_block_size: identify.logical_block_size,
            logical_block_count: identify.logical_block_count,
            capacity_bytes: identify
                .logical_block_count
                .saturating_mul(u64::from(identify.logical_block_size)),
            lba48: identify.lba48,
            sector_zero_checksum,
            sector_zero_signature,
        };
        controller.info = Some(info);

        Ok(controller)
    }

    fn configure_port(&mut self) -> Result<(), Error> {
        self.port.stop()?;
        self.dma.clear_command_state();

        let command_list = self.dma.command_and_fis.physical_address;
        let received_fis = self.dma.received_fis_physical();
        self.port.write(PORT_COMMAND_LIST_BASE, command_list as u32);
        self.port
            .write(PORT_COMMAND_LIST_BASE_UPPER, (command_list >> 32) as u32);
        self.port.write(PORT_FIS_BASE, received_fis as u32);
        self.port
            .write(PORT_FIS_BASE_UPPER, (received_fis >> 32) as u32);
        self.port.write(PORT_INTERRUPT_ENABLE, 0);
        self.port.write(PORT_INTERRUPT_STATUS, u32::MAX);
        self.port.write(PORT_SATA_ERROR, u32::MAX);
        self.port.start()?;
        self.port.wait_until_ready()
    }

    fn identify(&mut self) -> Result<IdentifyData, Error> {
        let mut identify_data = [0_u8; 512];
        self.execute_data_in(
            ATA_COMMAND_IDENTIFY_DEVICE,
            None,
            0,
            false,
            &mut identify_data,
        )?;
        parse_identify_data(&identify_data)
    }

    fn execute_data_in(
        &mut self,
        command: u8,
        logical_block_address: Option<u64>,
        sector_count: u16,
        extended_lba: bool,
        output: &mut [u8],
    ) -> Result<(), Error> {
        if output.is_empty() || output.len() > DMA_PAGE_SIZE {
            return Err(Error::BufferLength {
                expected: DMA_PAGE_SIZE,
                actual: output.len(),
            });
        }
        if self.port.read(PORT_COMMAND_ISSUE) & COMMAND_SLOT != 0
            || self.port.read(PORT_SATA_ACTIVE) & COMMAND_SLOT != 0
        {
            return Err(Error::CommandSlotBusy);
        }
        self.port.wait_until_ready()?;

        self.dma.command_and_fis.zero();
        self.dma.command_table.zero();
        self.dma.data.zero();

        let command_table_address = self.dma.command_table.physical_address;
        let header = CommandHeader {
            flags_and_prdt_length: COMMAND_FIS_DWORDS | COMMAND_HEADER_PRDT_LENGTH_ONE,
            bytes_transferred: 0,
            command_table_base: command_table_address as u32,
            command_table_base_upper: (command_table_address >> 32) as u32,
            reserved: [0; 4],
        };

        let mut command_fis = [0_u8; 64];
        command_fis[0] = REGISTER_HOST_TO_DEVICE_FIS_TYPE;
        command_fis[1] = REGISTER_FIS_COMMAND;
        command_fis[2] = command;
        if let Some(lba) = logical_block_address {
            command_fis[4] = lba as u8;
            command_fis[5] = (lba >> 8) as u8;
            command_fis[6] = (lba >> 16) as u8;
            command_fis[7] = 1 << 6;
            if extended_lba {
                command_fis[8] = (lba >> 24) as u8;
                command_fis[9] = (lba >> 32) as u8;
                command_fis[10] = (lba >> 40) as u8;
                command_fis[12] = sector_count as u8;
                command_fis[13] = (sector_count >> 8) as u8;
            } else {
                command_fis[7] |= ((lba >> 24) as u8) & 0x0f;
                command_fis[12] = sector_count as u8;
            }
        }

        let data_address = self.dma.data.physical_address;
        let command_table = CommandTable {
            command_fis,
            atapi_command: [0; 16],
            reserved: [0; 48],
            physical_region: PhysicalRegionDescriptor {
                data_base: data_address as u32,
                data_base_upper: (data_address >> 32) as u32,
                reserved: 0,
                byte_count_and_flags: (output.len() as u32 - 1) | PRDT_INTERRUPT_ON_COMPLETION,
            },
        };

        unsafe {
            self.dma.command_header_ptr().write(header);
            self.dma.command_table_ptr().write(command_table);
        }
        compiler_fence(Ordering::Release);

        self.port.write(PORT_INTERRUPT_STATUS, u32::MAX);
        self.port.write(PORT_SATA_ERROR, u32::MAX);
        self.port.write(PORT_COMMAND_ISSUE, COMMAND_SLOT);

        for _ in 0..COMMAND_COMPLETION_SPINS {
            if self.port.read(PORT_INTERRUPT_STATUS) & PORT_IS_TASK_FILE_ERROR != 0 {
                return Err(Error::TaskFileError);
            }
            if self.port.read(PORT_COMMAND_ISSUE) & COMMAND_SLOT == 0 {
                compiler_fence(Ordering::Acquire);
                if self.port.read(PORT_TASK_FILE_DATA) & PORT_TFD_ERROR != 0 {
                    return Err(Error::TaskFileError);
                }

                let transferred = unsafe {
                    ptr::read_volatile(&(*self.dma.command_header_ptr()).bytes_transferred)
                } as usize;
                if transferred < output.len() {
                    return Err(Error::TransferIncomplete {
                        expected: output.len(),
                        actual: transferred,
                    });
                }

                unsafe {
                    ptr::copy_nonoverlapping(
                        self.dma.data_ptr(),
                        output.as_mut_ptr(),
                        output.len(),
                    );
                }
                self.port.write(PORT_INTERRUPT_STATUS, u32::MAX);
                return Ok(());
            }
            spin_loop();
        }

        Err(Error::CommandTimeout)
    }

    fn execute_data_out(
        &mut self,
        command: u8,
        logical_block_address: u64,
        sector_count: u16,
        extended_lba: bool,
        input: &[u8],
    ) -> Result<(), Error> {
        if input.is_empty() || input.len() > DMA_PAGE_SIZE {
            return Err(Error::BufferLength {
                expected: DMA_PAGE_SIZE,
                actual: input.len(),
            });
        }
        if self.port.read(PORT_COMMAND_ISSUE) & COMMAND_SLOT != 0
            || self.port.read(PORT_SATA_ACTIVE) & COMMAND_SLOT != 0
        {
            return Err(Error::CommandSlotBusy);
        }
        self.port.wait_until_ready()?;

        self.dma.command_and_fis.zero();
        self.dma.command_table.zero();
        self.dma.data.zero();
        unsafe {
            ptr::copy_nonoverlapping(input.as_ptr(), self.dma.data_ptr(), input.len());
        }

        let command_table_address = self.dma.command_table.physical_address;
        let header = CommandHeader {
            flags_and_prdt_length: COMMAND_FIS_DWORDS
                | COMMAND_HEADER_WRITE
                | COMMAND_HEADER_PRDT_LENGTH_ONE,
            bytes_transferred: 0,
            command_table_base: command_table_address as u32,
            command_table_base_upper: (command_table_address >> 32) as u32,
            reserved: [0; 4],
        };

        let mut command_fis = [0_u8; 64];
        command_fis[0] = REGISTER_HOST_TO_DEVICE_FIS_TYPE;
        command_fis[1] = REGISTER_FIS_COMMAND;
        command_fis[2] = command;
        command_fis[4] = logical_block_address as u8;
        command_fis[5] = (logical_block_address >> 8) as u8;
        command_fis[6] = (logical_block_address >> 16) as u8;
        command_fis[7] = 1 << 6;
        if extended_lba {
            command_fis[8] = (logical_block_address >> 24) as u8;
            command_fis[9] = (logical_block_address >> 32) as u8;
            command_fis[10] = (logical_block_address >> 40) as u8;
            command_fis[12] = sector_count as u8;
            command_fis[13] = (sector_count >> 8) as u8;
        } else {
            command_fis[7] |= ((logical_block_address >> 24) as u8) & 0x0f;
            command_fis[12] = sector_count as u8;
        }

        let data_address = self.dma.data.physical_address;
        let command_table = CommandTable {
            command_fis,
            atapi_command: [0; 16],
            reserved: [0; 48],
            physical_region: PhysicalRegionDescriptor {
                data_base: data_address as u32,
                data_base_upper: (data_address >> 32) as u32,
                reserved: 0,
                byte_count_and_flags: (input.len() as u32 - 1) | PRDT_INTERRUPT_ON_COMPLETION,
            },
        };

        unsafe {
            self.dma.command_header_ptr().write(header);
            self.dma.command_table_ptr().write(command_table);
        }
        compiler_fence(Ordering::Release);

        self.port.write(PORT_INTERRUPT_STATUS, u32::MAX);
        self.port.write(PORT_SATA_ERROR, u32::MAX);
        self.port.write(PORT_COMMAND_ISSUE, COMMAND_SLOT);

        for _ in 0..COMMAND_COMPLETION_SPINS {
            if self.port.read(PORT_INTERRUPT_STATUS) & PORT_IS_TASK_FILE_ERROR != 0 {
                return Err(Error::TaskFileError);
            }
            if self.port.read(PORT_COMMAND_ISSUE) & COMMAND_SLOT == 0 {
                compiler_fence(Ordering::Acquire);
                if self.port.read(PORT_TASK_FILE_DATA) & PORT_TFD_ERROR != 0 {
                    return Err(Error::TaskFileError);
                }
                let transferred = unsafe {
                    ptr::read_volatile(&(*self.dma.command_header_ptr()).bytes_transferred)
                } as usize;
                if transferred < input.len() {
                    return Err(Error::TransferIncomplete {
                        expected: input.len(),
                        actual: transferred,
                    });
                }
                self.port.write(PORT_INTERRUPT_STATUS, u32::MAX);
                return Ok(());
            }
            spin_loop();
        }

        Err(Error::CommandTimeout)
    }

    fn execute_non_data(&mut self, command: u8) -> Result<(), Error> {
        if self.port.read(PORT_COMMAND_ISSUE) & COMMAND_SLOT != 0
            || self.port.read(PORT_SATA_ACTIVE) & COMMAND_SLOT != 0
        {
            return Err(Error::CommandSlotBusy);
        }
        self.port.wait_until_ready()?;
        self.dma.command_and_fis.zero();
        self.dma.command_table.zero();

        let command_table_address = self.dma.command_table.physical_address;
        let header = CommandHeader {
            flags_and_prdt_length: COMMAND_FIS_DWORDS,
            bytes_transferred: 0,
            command_table_base: command_table_address as u32,
            command_table_base_upper: (command_table_address >> 32) as u32,
            reserved: [0; 4],
        };
        let mut command_fis = [0_u8; 64];
        command_fis[0] = REGISTER_HOST_TO_DEVICE_FIS_TYPE;
        command_fis[1] = REGISTER_FIS_COMMAND;
        command_fis[2] = command;
        let command_table = CommandTable {
            command_fis,
            atapi_command: [0; 16],
            reserved: [0; 48],
            physical_region: PhysicalRegionDescriptor {
                data_base: 0,
                data_base_upper: 0,
                reserved: 0,
                byte_count_and_flags: 0,
            },
        };
        unsafe {
            self.dma.command_header_ptr().write(header);
            self.dma.command_table_ptr().write(command_table);
        }
        compiler_fence(Ordering::Release);
        self.port.write(PORT_INTERRUPT_STATUS, u32::MAX);
        self.port.write(PORT_SATA_ERROR, u32::MAX);
        self.port.write(PORT_COMMAND_ISSUE, COMMAND_SLOT);
        for _ in 0..COMMAND_COMPLETION_SPINS {
            if self.port.read(PORT_INTERRUPT_STATUS) & PORT_IS_TASK_FILE_ERROR != 0 {
                return Err(Error::TaskFileError);
            }
            if self.port.read(PORT_COMMAND_ISSUE) & COMMAND_SLOT == 0 {
                compiler_fence(Ordering::Acquire);
                if self.port.read(PORT_TASK_FILE_DATA) & PORT_TFD_ERROR != 0 {
                    return Err(Error::TaskFileError);
                }
                self.port.write(PORT_INTERRUPT_STATUS, u32::MAX);
                return Ok(());
            }
            spin_loop();
        }
        Err(Error::CommandTimeout)
    }

    fn info(&self) -> &DiskInfo {
        self.info
            .as_ref()
            .expect("initialized AHCI controller is missing disk metadata")
    }
}

impl BlockDevice for Controller {
    type Error = Error;

    fn block_size(&self) -> usize {
        self.logical_block_size
    }

    fn block_count(&self) -> u64 {
        self.logical_block_count
    }

    fn read_block(
        &mut self,
        logical_block_address: u64,
        buffer: &mut [u8],
    ) -> Result<(), Self::Error> {
        if buffer.len() != self.block_size() {
            return Err(Error::BufferLength {
                expected: self.block_size(),
                actual: buffer.len(),
            });
        }
        if logical_block_address >= self.block_count() {
            return Err(Error::LbaOutOfRange);
        }

        if self.lba48 {
            self.execute_data_in(
                ATA_COMMAND_READ_DMA_EXT,
                Some(logical_block_address),
                1,
                true,
                buffer,
            )
        } else {
            if logical_block_address > 0x0fff_ffff {
                return Err(Error::LbaOutOfRange);
            }
            self.execute_data_in(
                ATA_COMMAND_READ_DMA,
                Some(logical_block_address),
                1,
                false,
                buffer,
            )
        }
    }

    fn write_block(
        &mut self,
        logical_block_address: u64,
        buffer: &[u8],
    ) -> Result<(), Self::Error> {
        if buffer.len() != self.block_size() {
            return Err(Error::BufferLength {
                expected: self.block_size(),
                actual: buffer.len(),
            });
        }
        if logical_block_address >= self.block_count() {
            return Err(Error::LbaOutOfRange);
        }
        if self.lba48 {
            self.execute_data_out(
                ATA_COMMAND_WRITE_DMA_EXT,
                logical_block_address,
                1,
                true,
                buffer,
            )
        } else {
            if logical_block_address > 0x0fff_ffff {
                return Err(Error::LbaOutOfRange);
            }
            self.execute_data_out(
                ATA_COMMAND_WRITE_DMA,
                logical_block_address,
                1,
                false,
                buffer,
            )
        }
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.execute_non_data(if self.lba48 {
            ATA_COMMAND_FLUSH_CACHE_EXT
        } else {
            ATA_COMMAND_FLUSH_CACHE
        })
    }
}

pub fn init(
    inventory: &Inventory,
    mcfg: &McfgInfo,
    frame_allocator: &mut BootInfoFrameAllocator,
    physical_memory_offset: VirtAddr,
    physical_memory_end: u64,
) -> Result<DiskInfo, Error> {
    let mut device = DEVICE.lock();
    if device.is_some() {
        return Err(Error::AlreadyInitialized);
    }

    let function = inventory
        .functions()
        .iter()
        .copied()
        .find(is_ahci_controller)
        .ok_or(Error::ControllerNotFound)?;
    let controller = Controller::new(
        function,
        mcfg,
        frame_allocator,
        physical_memory_offset,
        physical_memory_end,
    )?;
    let info = controller.info().clone();
    *device = Some(controller);
    Ok(info)
}

pub fn info() -> Option<DiskInfo> {
    DEVICE.lock().as_ref().map(|device| device.info().clone())
}

pub fn read_block(logical_block_address: u64, buffer: &mut [u8]) -> Result<(), Error> {
    let mut device = DEVICE.lock();
    let device = device.as_mut().ok_or(Error::NotInitialized)?;
    device.read_block(logical_block_address, buffer)
}

pub fn write_block(logical_block_address: u64, buffer: &[u8]) -> Result<(), Error> {
    let mut device = DEVICE.lock();
    let device = device.as_mut().ok_or(Error::NotInitialized)?;
    device.write_block(logical_block_address, buffer)
}

pub fn flush() -> Result<(), Error> {
    let mut device = DEVICE.lock();
    let device = device.as_mut().ok_or(Error::NotInitialized)?;
    device.flush()
}

fn is_ahci_controller(function: &Function) -> bool {
    function.class_code == 0x01
        && function.subclass == 0x06
        && function.programming_interface == 0x01
}

fn ahci_base_address(function: Function) -> Result<u64, Error> {
    let raw = function.bars[5];
    if raw == 0 || raw == u32::MAX {
        return Err(Error::InvalidAbar);
    }
    if raw & 1 != 0 || (raw >> 1) & 0x3 != 0 {
        return Err(Error::UnsupportedAbar);
    }

    let address = u64::from(raw & !0x0f);
    if address == 0 {
        return Err(Error::InvalidAbar);
    }
    Ok(address)
}

fn perform_bios_handoff(hba: MmioRegion) -> Result<(), Error> {
    if hba.read_u32(HBA_CAPABILITIES_EXTENDED) & HBA_CAP2_BIOS_HANDOFF == 0 {
        return Ok(());
    }

    hba.write_u32(
        HBA_BIOS_HANDOFF_CONTROL,
        hba.read_u32(HBA_BIOS_HANDOFF_CONTROL) | HBA_BOHC_OS_OWNED,
    );
    if wait_until(BIOS_HANDOFF_SPINS, || {
        hba.read_u32(HBA_BIOS_HANDOFF_CONTROL) & (HBA_BOHC_BIOS_OWNED | HBA_BOHC_BIOS_BUSY) == 0
    }) {
        Ok(())
    } else {
        Err(Error::BiosHandoffTimeout)
    }
}

fn reset_hba(hba: MmioRegion) -> Result<(), Error> {
    let control = (hba.read_u32(HBA_GLOBAL_HOST_CONTROL) | HBA_GHC_AHCI_ENABLED)
        & !HBA_GHC_INTERRUPTS_ENABLED;
    hba.write_u32(HBA_GLOBAL_HOST_CONTROL, control | HBA_GHC_RESET);
    if !wait_until(HBA_RESET_SPINS, || {
        hba.read_u32(HBA_GLOBAL_HOST_CONTROL) & HBA_GHC_RESET == 0
    }) {
        return Err(Error::HbaResetTimeout);
    }

    hba.write_u32(
        HBA_GLOBAL_HOST_CONTROL,
        (hba.read_u32(HBA_GLOBAL_HOST_CONTROL) | HBA_GHC_AHCI_ENABLED)
            & !HBA_GHC_INTERRUPTS_ENABLED,
    );
    if hba.read_u32(HBA_GLOBAL_HOST_CONTROL) & HBA_GHC_AHCI_ENABLED == 0 {
        return Err(Error::AhciEnableFailed);
    }
    Ok(())
}

fn parse_identify_data(data: &[u8; 512]) -> Result<IdentifyData, Error> {
    let capabilities = identify_word(data, 49);
    if capabilities & (1 << 9) == 0 {
        return Err(Error::IdentifyDataInvalid);
    }

    let lba48 = identify_word(data, 83) & (1 << 10) != 0;
    let lba28_count =
        u64::from(identify_word(data, 60)) | (u64::from(identify_word(data, 61)) << 16);
    let lba48_count = u64::from(identify_word(data, 100))
        | (u64::from(identify_word(data, 101)) << 16)
        | (u64::from(identify_word(data, 102)) << 32)
        | (u64::from(identify_word(data, 103)) << 48);
    let logical_block_count = if lba48 && lba48_count != 0 {
        lba48_count
    } else {
        lba28_count
    };
    if logical_block_count == 0 {
        return Err(Error::IdentifyDataInvalid);
    }

    let sector_size_descriptor = identify_word(data, 106);
    let logical_words =
        if sector_size_descriptor & 0xc000 == 0x4000 && sector_size_descriptor & (1 << 12) != 0 {
            u32::from(identify_word(data, 117)) | (u32::from(identify_word(data, 118)) << 16)
        } else {
            256
        };
    let logical_block_size = logical_words
        .checked_mul(2)
        .ok_or(Error::IdentifyDataInvalid)?;
    if !(512..=DMA_PAGE_SIZE as u32).contains(&logical_block_size)
        || !logical_block_size.is_power_of_two()
    {
        return Err(Error::UnsupportedLogicalBlockSize(logical_block_size));
    }

    let mut model = ata_string(data, 27, 20);
    if model.is_empty() {
        model.push_str("unknown ATA disk");
    }

    Ok(IdentifyData {
        model,
        serial: ata_string(data, 10, 10),
        firmware: ata_string(data, 23, 4),
        logical_block_size,
        logical_block_count,
        lba48,
    })
}

fn identify_word(data: &[u8; 512], word: usize) -> u16 {
    let offset = word * 2;
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn ata_string(data: &[u8; 512], first_word: usize, word_count: usize) -> String {
    let mut value = String::new();
    for word in first_word..first_word + word_count {
        let offset = word * 2;
        for byte in [data[offset + 1], data[offset]] {
            if byte == 0 {
                continue;
            }
            let character = if byte.is_ascii_graphic() || byte == b' ' {
                char::from(byte)
            } else {
                '?'
            };
            value.push(character);
        }
    }
    while value.ends_with(' ') {
        value.pop();
    }
    value
}

fn fnv1a32(bytes: &[u8]) -> u32 {
    bytes.iter().copied().fold(0x811c_9dc5, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

fn wait_until(mut remaining: usize, mut condition: impl FnMut() -> bool) -> bool {
    while remaining > 0 {
        if condition() {
            return true;
        }
        remaining -= 1;
        spin_loop();
    }
    false
}

const _: () = assert!(size_of::<CommandHeader>() == 32);
const _: () = assert!(size_of::<CommandTable>() == 144);
const _: () = assert!(COMMAND_LIST_BYTES == 32 * size_of::<CommandHeader>());
const _: () = assert!(FRAME_SIZE as usize == DMA_PAGE_SIZE);
