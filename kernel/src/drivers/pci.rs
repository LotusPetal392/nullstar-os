use alloc::vec::Vec;
use core::{fmt, ptr};

use x86_64::VirtAddr;

use crate::acpi::{McfgInfo, McfgRegionInfo};

const ECAM_BYTES_PER_BUS: u64 = 1 << 20;
const ECAM_BYTES_PER_DEVICE: u64 = 1 << 15;
const ECAM_BYTES_PER_FUNCTION: u64 = 1 << 12;
const MAX_SCANNED_BUSES: usize = 256;
const MAX_RECORDED_FUNCTIONS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    MissingConfigurationRegion,
    InvalidBusRange,
    InvalidBaseAddress,
    AddressOverflow,
    RegionOutsidePhysicalMap,
    NoFunctionsFound,
}

impl Error {
    pub const fn description(self) -> &'static str {
        match self {
            Self::MissingConfigurationRegion => "MCFG did not provide a configuration region",
            Self::InvalidBusRange => "MCFG configuration region has an invalid bus range",
            Self::InvalidBaseAddress => "MCFG ECAM base address is invalid",
            Self::AddressOverflow => "PCIe ECAM address calculation overflowed",
            Self::RegionOutsidePhysicalMap => {
                "PCIe ECAM region is outside the bootloader physical mapping"
            }
            Self::NoFunctionsFound => "PCIe enumeration did not find any functions",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.description())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl fmt::Display for Location {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04x}:{:02x}:{:02x}.{}",
            self.segment, self.bus, self.device, self.function
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderKind {
    Device,
    PciBridge,
    CardBusBridge,
    Unknown(u8),
}

impl HeaderKind {
    fn from_raw(value: u8) -> Self {
        match value {
            0x00 => Self::Device,
            0x01 => Self::PciBridge,
            0x02 => Self::CardBusBridge,
            other => Self::Unknown(other),
        }
    }
}

impl fmt::Display for HeaderKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device => formatter.write_str("device"),
            Self::PciBridge => formatter.write_str("PCI bridge"),
            Self::CardBusBridge => formatter.write_str("CardBus bridge"),
            Self::Unknown(value) => write!(formatter, "unknown header {value:#x}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubsystemId {
    pub vendor_id: u16,
    pub device_id: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeBuses {
    pub primary: u8,
    pub secondary: u8,
    pub subordinate: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct Function {
    pub location: Location,
    pub vendor_id: u16,
    pub device_id: u16,
    pub command: u16,
    pub status: u16,
    pub revision_id: u8,
    pub programming_interface: u8,
    pub subclass: u8,
    pub class_code: u8,
    pub header_kind: HeaderKind,
    pub multifunction: bool,
    pub subsystem: Option<SubsystemId>,
    pub bridge_buses: Option<BridgeBuses>,
    pub interrupt_line: u8,
    pub interrupt_pin: u8,
    pub bars: [u32; 6],
}

impl Function {
    pub fn class_description(&self) -> &'static str {
        match (self.class_code, self.subclass) {
            (0x00, 0x00) => "unclassified device",
            (0x00, 0x01) => "VGA-compatible unclassified device",
            (0x01, 0x00) => "SCSI storage controller",
            (0x01, 0x01) => "IDE controller",
            (0x01, 0x02) => "floppy controller",
            (0x01, 0x04) => "RAID controller",
            (0x01, 0x05) => "ATA controller",
            (0x01, 0x06) => "SATA controller",
            (0x01, 0x07) => "SAS controller",
            (0x01, 0x08) => "NVMe controller",
            (0x01, _) => "mass-storage controller",
            (0x02, 0x00) => "Ethernet controller",
            (0x02, 0x01) => "token-ring controller",
            (0x02, 0x02) => "FDDI controller",
            (0x02, 0x03) => "ATM controller",
            (0x02, _) => "network controller",
            (0x03, 0x00) => "VGA-compatible display controller",
            (0x03, 0x01) => "XGA display controller",
            (0x03, 0x02) => "3D display controller",
            (0x03, _) => "display controller",
            (0x04, 0x00) => "multimedia video controller",
            (0x04, 0x01) => "multimedia audio controller",
            (0x04, 0x03) => "audio device",
            (0x04, _) => "multimedia controller",
            (0x05, _) => "memory controller",
            (0x06, 0x00) => "host bridge",
            (0x06, 0x01) => "ISA bridge",
            (0x06, 0x02) => "EISA bridge",
            (0x06, 0x04) => "PCI-to-PCI bridge",
            (0x06, 0x07) => "CardBus bridge",
            (0x06, _) => "bridge device",
            (0x07, _) => "communication controller",
            (0x08, _) => "system peripheral",
            (0x09, _) => "input controller",
            (0x0a, _) => "docking station",
            (0x0b, _) => "processor",
            (0x0c, 0x00) => "FireWire controller",
            (0x0c, 0x03) => "USB controller",
            (0x0c, 0x05) => "SMBus controller",
            (0x0c, _) => "serial-bus controller",
            (0x0d, _) => "wireless controller",
            (0x0e, _) => "intelligent I/O controller",
            (0x0f, _) => "satellite communication controller",
            (0x10, _) => "encryption controller",
            (0x11, _) => "signal-processing controller",
            (0x12, _) => "processing accelerator",
            (0x13, _) => "instrumentation device",
            (0x40, _) => "co-processor",
            (0xff, _) => "unassigned class",
            _ => "unknown PCI class",
        }
    }
}

#[derive(Debug)]
pub struct Inventory {
    pub declared_region_count: u16,
    pub scanned_region_count: u16,
    pub unscanned_region_count: u16,
    pub scanned_bus_count: u16,
    pub total_function_count: u32,
    pub bus_scan_truncated: bool,
    pub function_list_truncated: bool,
    functions: Vec<Function>,
}

impl Inventory {
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }

    pub fn recorded_function_count(&self) -> usize {
        self.functions.len()
    }

    pub fn class_count(&self, class_code: u8) -> usize {
        self.functions
            .iter()
            .filter(|function| function.class_code == class_code)
            .count()
    }

    pub fn bridge_count(&self) -> usize {
        self.class_count(0x06)
    }

    pub fn is_truncated(&self) -> bool {
        self.unscanned_region_count > 0
            || self.bus_scan_truncated
            || self.function_list_truncated
    }

    fn record(&mut self, function: Function) {
        self.total_function_count = self.total_function_count.saturating_add(1);
        if self.functions.len() < MAX_RECORDED_FUNCTIONS {
            self.functions.push(function);
        } else {
            self.function_list_truncated = true;
        }
    }
}

#[derive(Clone, Copy)]
struct EcamRegion {
    info: McfgRegionInfo,
    virtual_base: usize,
    scan_end_bus: u8,
}

impl EcamRegion {
    fn new(
        info: McfgRegionInfo,
        physical_memory_offset: VirtAddr,
        physical_memory_end: u64,
    ) -> Result<(Self, bool), Error> {
        if info.start_bus > info.end_bus {
            return Err(Error::InvalidBusRange);
        }
        if info.base_address == 0 || info.base_address & (ECAM_BYTES_PER_BUS - 1) != 0 {
            return Err(Error::InvalidBaseAddress);
        }

        let region_bus_count = usize::from(info.end_bus - info.start_bus) + 1;
        let scanned_bus_count = region_bus_count.min(MAX_SCANNED_BUSES);
        let scan_end_bus = info
            .start_bus
            .checked_add((scanned_bus_count - 1) as u8)
            .ok_or(Error::AddressOverflow)?;
        let region_length = u64::try_from(scanned_bus_count)
            .map_err(|_| Error::AddressOverflow)?
            .checked_mul(ECAM_BYTES_PER_BUS)
            .ok_or(Error::AddressOverflow)?;
        let region_end = info
            .base_address
            .checked_add(region_length)
            .ok_or(Error::AddressOverflow)?;
        if region_end > physical_memory_end {
            return Err(Error::RegionOutsidePhysicalMap);
        }

        let virtual_address = physical_memory_offset
            .as_u64()
            .checked_add(info.base_address)
            .ok_or(Error::AddressOverflow)?;
        let virtual_base = usize::try_from(virtual_address).map_err(|_| Error::AddressOverflow)?;

        Ok((
            Self {
                info,
                virtual_base,
                scan_end_bus,
            },
            scanned_bus_count < region_bus_count,
        ))
    }

    fn config_function(self, bus: u8, device: u8, function: u8) -> Option<ConfigFunction> {
        let bus_index = u64::from(bus.checked_sub(self.info.start_bus)?);
        let offset = bus_index
            .checked_mul(ECAM_BYTES_PER_BUS)?
            .checked_add(u64::from(device).checked_mul(ECAM_BYTES_PER_DEVICE)?)?
            .checked_add(u64::from(function).checked_mul(ECAM_BYTES_PER_FUNCTION)?)?;
        let virtual_address = u64::try_from(self.virtual_base).ok()?.checked_add(offset)?;
        let virtual_base = usize::try_from(virtual_address).ok()?;

        Some(ConfigFunction {
            location: Location {
                segment: self.info.segment_group,
                bus,
                device,
                function,
            },
            virtual_base,
        })
    }
}

#[derive(Clone, Copy)]
struct ConfigFunction {
    location: Location,
    virtual_base: usize,
}

impl ConfigFunction {
    fn read_u8(self, offset: usize) -> u8 {
        let address = self
            .virtual_base
            .checked_add(offset)
            .expect("validated PCIe configuration address overflowed");
        unsafe { ptr::read_volatile(address as *const u8) }
    }

    fn read_u16(self, offset: usize) -> u16 {
        let address = self
            .virtual_base
            .checked_add(offset)
            .expect("validated PCIe configuration address overflowed");
        unsafe { ptr::read_volatile(address as *const u16) }
    }

    fn read_u32(self, offset: usize) -> u32 {
        let address = self
            .virtual_base
            .checked_add(offset)
            .expect("validated PCIe configuration address overflowed");
        unsafe { ptr::read_volatile(address as *const u32) }
    }

    fn vendor_id(self) -> u16 {
        self.read_u16(0x00)
    }

    fn read_function(self) -> Function {
        let raw_header_type = self.read_u8(0x0e);
        let header_kind = HeaderKind::from_raw(raw_header_type & 0x7f);
        let multifunction = raw_header_type & 0x80 != 0;

        let mut bars = [0; 6];
        let bar_count = match header_kind {
            HeaderKind::Device => 6,
            HeaderKind::PciBridge => 2,
            HeaderKind::CardBusBridge | HeaderKind::Unknown(_) => 0,
        };
        for (index, bar) in bars.iter_mut().take(bar_count).enumerate() {
            *bar = self.read_u32(0x10 + index * 4);
        }

        let subsystem = (header_kind == HeaderKind::Device).then(|| SubsystemId {
            vendor_id: self.read_u16(0x2c),
            device_id: self.read_u16(0x2e),
        });
        let bridge_buses = (header_kind == HeaderKind::PciBridge).then(|| BridgeBuses {
            primary: self.read_u8(0x18),
            secondary: self.read_u8(0x19),
            subordinate: self.read_u8(0x1a),
        });

        Function {
            location: self.location,
            vendor_id: self.vendor_id(),
            device_id: self.read_u16(0x02),
            command: self.read_u16(0x04),
            status: self.read_u16(0x06),
            revision_id: self.read_u8(0x08),
            programming_interface: self.read_u8(0x09),
            subclass: self.read_u8(0x0a),
            class_code: self.read_u8(0x0b),
            header_kind,
            multifunction,
            subsystem,
            bridge_buses,
            interrupt_line: self.read_u8(0x3c),
            interrupt_pin: self.read_u8(0x3d),
            bars,
        }
    }
}

pub fn enumerate(
    mcfg: &McfgInfo,
    physical_memory_offset: VirtAddr,
    physical_memory_end: u64,
) -> Result<Inventory, Error> {
    let region = mcfg
        .first_region
        .ok_or(Error::MissingConfigurationRegion)?;
    let (region, bus_scan_truncated) =
        EcamRegion::new(region, physical_memory_offset, physical_memory_end)?;

    let mut inventory = Inventory {
        declared_region_count: mcfg.region_count,
        scanned_region_count: 1,
        unscanned_region_count: mcfg.region_count.saturating_sub(1),
        scanned_bus_count: u16::from(region.scan_end_bus - region.info.start_bus) + 1,
        total_function_count: 0,
        bus_scan_truncated,
        function_list_truncated: false,
        functions: Vec::new(),
    };

    for bus in region.info.start_bus..=region.scan_end_bus {
        for device in 0..32 {
            let Some(function_zero) = region.config_function(bus, device, 0) else {
                inventory.bus_scan_truncated = true;
                continue;
            };
            if function_zero.vendor_id() == 0xffff {
                continue;
            }

            let function_zero = function_zero.read_function();
            let multifunction = function_zero.multifunction;
            inventory.record(function_zero);

            if !multifunction {
                continue;
            }

            for function in 1..8 {
                let Some(config) = region.config_function(bus, device, function) else {
                    inventory.bus_scan_truncated = true;
                    continue;
                };
                if config.vendor_id() != 0xffff {
                    inventory.record(config.read_function());
                }
            }
        }
    }

    if inventory.total_function_count == 0 {
        return Err(Error::NoFunctionsFound);
    }

    Ok(inventory)
}
