use core::{fmt, slice, str};

use x86_64::VirtAddr;

const RSDP_SIGNATURE: &[u8; 8] = b"RSD PTR ";
const RSDP_V1_LENGTH: usize = 20;
const RSDP_V2_MIN_LENGTH: usize = 36;
const MAX_RSDP_LENGTH: usize = 4096;
const SDT_HEADER_LENGTH: usize = 36;
const MAX_ACPI_TABLE_LENGTH: usize = 16 * 1024 * 1024;
const MAX_RECORDED_TABLES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiError {
    AddressOverflow,
    RegionOutsidePhysicalMap { address: u64, length: usize },
    InvalidRsdpSignature,
    InvalidRsdpChecksum,
    InvalidRsdpLength(u32),
    MissingRootTable,
    InvalidRootSignature(Signature),
    InvalidRootEntryLayout,
    InvalidTableLength { address: u64, length: u32 },
    InvalidTableChecksum(Signature),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootTableKind {
    Rsdt,
    Xsdt,
}

impl fmt::Display for RootTableKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rsdt => formatter.write_str("RSDT"),
            Self::Xsdt => formatter.write_str("XSDT"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Signature([u8; 4]);

impl Signature {
    const EMPTY: Self = Self(*b"----");
    const MADT: Self = Self(*b"APIC");
    const FADT: Self = Self(*b"FACP");
    const HPET: Self = Self(*b"HPET");
    const MCFG: Self = Self(*b"MCFG");
    const RSDT: Self = Self(*b"RSDT");
    const XSDT: Self = Self(*b"XSDT");

    fn from_slice(bytes: &[u8]) -> Option<Self> {
        let bytes: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
        Some(Self(bytes))
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            let character = if byte.is_ascii_graphic() || byte == b' ' {
                char::from(byte)
            } else {
                '?'
            };
            write!(formatter, "{character}")?;
        }

        Ok(())
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "\"{self}\"")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerProfile {
    Unspecified,
    Desktop,
    Mobile,
    Workstation,
    EnterpriseServer,
    SohoServer,
    AppliancePc,
    PerformanceServer,
    Tablet,
    Reserved(u8),
}

impl PowerProfile {
    fn from_raw(value: u8) -> Self {
        match value {
            0 => Self::Unspecified,
            1 => Self::Desktop,
            2 => Self::Mobile,
            3 => Self::Workstation,
            4 => Self::EnterpriseServer,
            5 => Self::SohoServer,
            6 => Self::AppliancePc,
            7 => Self::PerformanceServer,
            8 => Self::Tablet,
            other => Self::Reserved(other),
        }
    }
}

impl fmt::Display for PowerProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unspecified => formatter.write_str("unspecified"),
            Self::Desktop => formatter.write_str("desktop"),
            Self::Mobile => formatter.write_str("mobile"),
            Self::Workstation => formatter.write_str("workstation"),
            Self::EnterpriseServer => formatter.write_str("enterprise server"),
            Self::SohoServer => formatter.write_str("SOHO server"),
            Self::AppliancePc => formatter.write_str("appliance PC"),
            Self::PerformanceServer => formatter.write_str("performance server"),
            Self::Tablet => formatter.write_str("tablet"),
            Self::Reserved(value) => write!(formatter, "reserved ({value})"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptPolarity {
    ActiveHigh,
    ActiveLow,
}

impl fmt::Display for InterruptPolarity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveHigh => formatter.write_str("active-high"),
            Self::ActiveLow => formatter.write_str("active-low"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptTriggerMode {
    Edge,
    Level,
}

impl fmt::Display for InterruptTriggerMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Edge => formatter.write_str("edge"),
            Self::Level => formatter.write_str("level"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsaInterruptRoute {
    pub source_irq: u8,
    pub global_system_interrupt: u32,
    pub polarity: InterruptPolarity,
    pub trigger_mode: InterruptTriggerMode,
    pub overridden: bool,
}

impl IsaInterruptRoute {
    const fn legacy(source_irq: u8) -> Self {
        Self {
            source_irq,
            global_system_interrupt: source_irq as u32,
            polarity: InterruptPolarity::ActiveHigh,
            trigger_mode: InterruptTriggerMode::Edge,
            overridden: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct IoApicInfo {
    pub id: u8,
    pub address: u32,
    pub global_system_interrupt_base: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct MadtInfo {
    pub local_apic_address: u64,
    pub supports_legacy_pic: bool,
    pub processor_count: u16,
    pub enabled_processor_count: u16,
    pub online_capable_processor_count: u16,
    pub io_apic_count: u16,
    pub interrupt_override_count: u16,
    pub malformed_entry_count: u16,
    pub first_io_apic: Option<IoApicInfo>,
    pub timer_route: IsaInterruptRoute,
    pub keyboard_route: IsaInterruptRoute,
}

#[derive(Debug, Clone, Copy)]
pub struct FadtInfo {
    pub revision: u8,
    pub preferred_power_profile: PowerProfile,
    pub sci_interrupt: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct HpetInfo {
    pub base_address: u64,
    pub address_space: u8,
    pub comparator_count: u8,
    pub counter_is_64_bit: bool,
    pub legacy_irq_capable: bool,
    pub hpet_number: u8,
    pub minimum_tick: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct McfgRegionInfo {
    pub base_address: u64,
    pub segment_group: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct McfgInfo {
    pub region_count: u16,
    pub first_region: Option<McfgRegionInfo>,
}

#[derive(Debug, Clone, Copy)]
pub struct AcpiInfo {
    pub rsdp_address: u64,
    pub revision: u8,
    pub root_table_kind: RootTableKind,
    pub root_table_address: u64,
    pub total_table_count: u16,
    pub valid_table_count: u16,
    pub invalid_table_count: u16,
    pub madt: Option<MadtInfo>,
    pub fadt: Option<FadtInfo>,
    pub hpet: Option<HpetInfo>,
    pub mcfg: Option<McfgInfo>,
    oem_id: [u8; 6],
    table_signatures: [Signature; MAX_RECORDED_TABLES],
    recorded_table_count: usize,
}

impl AcpiInfo {
    pub fn oem_id(&self) -> &str {
        str::from_utf8(&self.oem_id).unwrap_or("??????").trim_end()
    }

    pub fn table_signatures(&self) -> &[Signature] {
        &self.table_signatures[..self.recorded_table_count]
    }

    fn record_signature(&mut self, signature: Signature) {
        if self.recorded_table_count >= self.table_signatures.len() {
            return;
        }

        self.table_signatures[self.recorded_table_count] = signature;
        self.recorded_table_count += 1;
    }
}

#[derive(Clone, Copy)]
struct PhysicalMemory {
    virtual_offset: u64,
    physical_end: u64,
}

impl PhysicalMemory {
    fn new(virtual_offset: VirtAddr, physical_end: u64) -> Self {
        Self {
            virtual_offset: virtual_offset.as_u64(),
            physical_end,
        }
    }

    fn region(&self, physical_address: u64, length: usize) -> Result<&'static [u8], AcpiError> {
        let length_u64 = u64::try_from(length).map_err(|_| AcpiError::AddressOverflow)?;
        let physical_end = physical_address
            .checked_add(length_u64)
            .ok_or(AcpiError::AddressOverflow)?;

        if physical_end > self.physical_end {
            return Err(AcpiError::RegionOutsidePhysicalMap {
                address: physical_address,
                length,
            });
        }

        let virtual_address = self
            .virtual_offset
            .checked_add(physical_address)
            .ok_or(AcpiError::AddressOverflow)?;
        let virtual_address =
            usize::try_from(virtual_address).map_err(|_| AcpiError::AddressOverflow)?;

        Ok(unsafe { slice::from_raw_parts(virtual_address as *const u8, length) })
    }
}

#[derive(Clone, Copy)]
struct ParsedRsdp {
    revision: u8,
    oem_id: [u8; 6],
    root_table_kind: RootTableKind,
    root_table_address: u64,
}

#[derive(Clone, Copy)]
struct TableHeader {
    signature: Signature,
    length: u32,
    revision: u8,
}

pub fn init(
    rsdp_address: u64,
    physical_memory_offset: VirtAddr,
    physical_memory_end: u64,
) -> Result<AcpiInfo, AcpiError> {
    let memory = PhysicalMemory::new(physical_memory_offset, physical_memory_end);
    let rsdp = parse_rsdp(memory, rsdp_address)?;
    let root_header = read_table_header(memory, rsdp.root_table_address)?;

    let expected_root_signature = match rsdp.root_table_kind {
        RootTableKind::Rsdt => Signature::RSDT,
        RootTableKind::Xsdt => Signature::XSDT,
    };
    if root_header.signature != expected_root_signature {
        return Err(AcpiError::InvalidRootSignature(root_header.signature));
    }

    let root_table = read_complete_table(memory, rsdp.root_table_address, root_header)?;
    let entry_size = match rsdp.root_table_kind {
        RootTableKind::Rsdt => 4,
        RootTableKind::Xsdt => 8,
    };
    let root_entries = &root_table[SDT_HEADER_LENGTH..];
    if root_entries.len() % entry_size != 0 {
        return Err(AcpiError::InvalidRootEntryLayout);
    }

    let mut info = AcpiInfo {
        rsdp_address,
        revision: rsdp.revision,
        root_table_kind: rsdp.root_table_kind,
        root_table_address: rsdp.root_table_address,
        total_table_count: 0,
        valid_table_count: 0,
        invalid_table_count: 0,
        madt: None,
        fadt: None,
        hpet: None,
        mcfg: None,
        oem_id: rsdp.oem_id,
        table_signatures: [Signature::EMPTY; MAX_RECORDED_TABLES],
        recorded_table_count: 0,
    };

    for entry in root_entries.chunks_exact(entry_size) {
        let table_address = match rsdp.root_table_kind {
            RootTableKind::Rsdt => u64::from(read_u32(entry, 0).unwrap_or(0)),
            RootTableKind::Xsdt => read_u64(entry, 0).unwrap_or(0),
        };
        if table_address == 0 {
            continue;
        }

        info.total_table_count = info.total_table_count.saturating_add(1);

        let table_header = match read_table_header(memory, table_address) {
            Ok(header) => header,
            Err(_) => {
                info.invalid_table_count = info.invalid_table_count.saturating_add(1);
                continue;
            }
        };

        let table = match read_complete_table(memory, table_address, table_header) {
            Ok(table) => table,
            Err(_) => {
                info.invalid_table_count = info.invalid_table_count.saturating_add(1);
                continue;
            }
        };

        info.valid_table_count = info.valid_table_count.saturating_add(1);
        info.record_signature(table_header.signature);

        match table_header.signature {
            Signature::MADT if info.madt.is_none() => {
                info.madt = parse_madt(table);
            }
            Signature::FADT if info.fadt.is_none() => {
                info.fadt = parse_fadt(table, table_header.revision);
            }
            Signature::HPET if info.hpet.is_none() => {
                info.hpet = parse_hpet(table);
            }
            Signature::MCFG if info.mcfg.is_none() => {
                info.mcfg = parse_mcfg(table);
            }
            _ => {}
        }
    }

    Ok(info)
}

fn parse_rsdp(memory: PhysicalMemory, rsdp_address: u64) -> Result<ParsedRsdp, AcpiError> {
    let version_one = memory.region(rsdp_address, RSDP_V1_LENGTH)?;
    if version_one.get(..8) != Some(&RSDP_SIGNATURE[..]) {
        return Err(AcpiError::InvalidRsdpSignature);
    }
    if !checksum_is_valid(version_one) {
        return Err(AcpiError::InvalidRsdpChecksum);
    }

    let mut oem_id = [0; 6];
    oem_id.copy_from_slice(
        version_one
            .get(9..15)
            .ok_or(AcpiError::InvalidRsdpLength(RSDP_V1_LENGTH as u32))?,
    );

    let revision = *version_one
        .get(15)
        .ok_or(AcpiError::InvalidRsdpLength(RSDP_V1_LENGTH as u32))?;
    let rsdt_address = u64::from(
        read_u32(version_one, 16).ok_or(AcpiError::InvalidRsdpLength(RSDP_V1_LENGTH as u32))?,
    );

    let (root_table_kind, root_table_address) = if revision == 0 {
        (RootTableKind::Rsdt, rsdt_address)
    } else {
        let version_two_header = memory.region(rsdp_address, RSDP_V2_MIN_LENGTH)?;
        let length = read_u32(version_two_header, 20)
            .ok_or(AcpiError::InvalidRsdpLength(RSDP_V2_MIN_LENGTH as u32))?;
        let length_usize =
            usize::try_from(length).map_err(|_| AcpiError::InvalidRsdpLength(length))?;

        if !(RSDP_V2_MIN_LENGTH..=MAX_RSDP_LENGTH).contains(&length_usize) {
            return Err(AcpiError::InvalidRsdpLength(length));
        }

        let version_two = memory.region(rsdp_address, length_usize)?;
        if !checksum_is_valid(version_two) {
            return Err(AcpiError::InvalidRsdpChecksum);
        }

        let xsdt_address = read_u64(version_two, 24).ok_or(AcpiError::InvalidRsdpLength(length))?;

        if xsdt_address != 0 {
            (RootTableKind::Xsdt, xsdt_address)
        } else {
            (RootTableKind::Rsdt, rsdt_address)
        }
    };

    if root_table_address == 0 {
        return Err(AcpiError::MissingRootTable);
    }

    Ok(ParsedRsdp {
        revision,
        oem_id,
        root_table_kind,
        root_table_address,
    })
}

fn read_table_header(memory: PhysicalMemory, table_address: u64) -> Result<TableHeader, AcpiError> {
    let bytes = memory.region(table_address, SDT_HEADER_LENGTH)?;
    let signature = Signature::from_slice(bytes).ok_or(AcpiError::InvalidTableLength {
        address: table_address,
        length: 0,
    })?;
    let length = read_u32(bytes, 4).ok_or(AcpiError::InvalidTableLength {
        address: table_address,
        length: 0,
    })?;
    let revision = *bytes.get(8).ok_or(AcpiError::InvalidTableLength {
        address: table_address,
        length,
    })?;

    let length_usize = usize::try_from(length).map_err(|_| AcpiError::InvalidTableLength {
        address: table_address,
        length,
    })?;
    if !(SDT_HEADER_LENGTH..=MAX_ACPI_TABLE_LENGTH).contains(&length_usize) {
        return Err(AcpiError::InvalidTableLength {
            address: table_address,
            length,
        });
    }

    Ok(TableHeader {
        signature,
        length,
        revision,
    })
}

fn read_complete_table(
    memory: PhysicalMemory,
    table_address: u64,
    header: TableHeader,
) -> Result<&'static [u8], AcpiError> {
    let length = usize::try_from(header.length).map_err(|_| AcpiError::InvalidTableLength {
        address: table_address,
        length: header.length,
    })?;
    let table = memory.region(table_address, length)?;

    if !checksum_is_valid(table) {
        return Err(AcpiError::InvalidTableChecksum(header.signature));
    }

    Ok(table)
}

fn parse_madt(table: &[u8]) -> Option<MadtInfo> {
    const MADT_HEADER_LENGTH: usize = SDT_HEADER_LENGTH + 8;

    if table.len() < MADT_HEADER_LENGTH {
        return None;
    }

    let mut info = MadtInfo {
        local_apic_address: u64::from(read_u32(table, SDT_HEADER_LENGTH)?),
        supports_legacy_pic: read_u32(table, SDT_HEADER_LENGTH + 4)? & 1 != 0,
        processor_count: 0,
        enabled_processor_count: 0,
        online_capable_processor_count: 0,
        io_apic_count: 0,
        interrupt_override_count: 0,
        malformed_entry_count: 0,
        first_io_apic: None,
        timer_route: IsaInterruptRoute::legacy(0),
        keyboard_route: IsaInterruptRoute::legacy(1),
    };

    let mut cursor = MADT_HEADER_LENGTH;
    while cursor < table.len() {
        if cursor + 2 > table.len() {
            info.malformed_entry_count = info.malformed_entry_count.saturating_add(1);
            break;
        }

        let entry_type = table[cursor];
        let entry_length = usize::from(table[cursor + 1]);
        if entry_length < 2 || cursor.saturating_add(entry_length) > table.len() {
            info.malformed_entry_count = info.malformed_entry_count.saturating_add(1);
            break;
        }

        let entry = &table[cursor..cursor + entry_length];
        match entry_type {
            0 if entry.len() >= 8 => {
                let flags = read_u32(entry, 4)?;
                record_processor(&mut info, flags);
            }
            1 if entry.len() >= 12 => {
                info.io_apic_count = info.io_apic_count.saturating_add(1);
                if info.first_io_apic.is_none() {
                    info.first_io_apic = Some(IoApicInfo {
                        id: entry[2],
                        address: read_u32(entry, 4)?,
                        global_system_interrupt_base: read_u32(entry, 8)?,
                    });
                }
            }
            2 if entry.len() >= 10 => {
                info.interrupt_override_count = info.interrupt_override_count.saturating_add(1);
                match parse_interrupt_override(entry) {
                    Some(route) if route.source_irq == 0 => info.timer_route = route,
                    Some(route) if route.source_irq == 1 => info.keyboard_route = route,
                    Some(_) => {}
                    None => {
                        info.malformed_entry_count = info.malformed_entry_count.saturating_add(1);
                    }
                }
            }
            5 if entry.len() >= 12 => {
                info.local_apic_address = read_u64(entry, 4)?;
            }
            9 if entry.len() >= 16 => {
                let flags = read_u32(entry, 8)?;
                record_processor(&mut info, flags);
            }
            0 | 1 | 2 | 5 | 9 => {
                info.malformed_entry_count = info.malformed_entry_count.saturating_add(1);
            }
            _ => {}
        }

        cursor += entry_length;
    }

    Some(info)
}

fn parse_interrupt_override(entry: &[u8]) -> Option<IsaInterruptRoute> {
    if entry.len() < 10 || entry[2] != 0 {
        return None;
    }

    let source_irq = entry[3];
    let global_system_interrupt = read_u32(entry, 4)?;
    let flags = read_u16(entry, 8)?;

    let polarity = match flags & 0b11 {
        0 | 1 => InterruptPolarity::ActiveHigh,
        3 => InterruptPolarity::ActiveLow,
        _ => return None,
    };
    let trigger_mode = match (flags >> 2) & 0b11 {
        0 | 1 => InterruptTriggerMode::Edge,
        3 => InterruptTriggerMode::Level,
        _ => return None,
    };

    Some(IsaInterruptRoute {
        source_irq,
        global_system_interrupt,
        polarity,
        trigger_mode,
        overridden: true,
    })
}

fn record_processor(info: &mut MadtInfo, flags: u32) {
    info.processor_count = info.processor_count.saturating_add(1);
    if flags & 1 != 0 {
        info.enabled_processor_count = info.enabled_processor_count.saturating_add(1);
    }
    if flags & 2 != 0 {
        info.online_capable_processor_count = info.online_capable_processor_count.saturating_add(1);
    }
}

fn parse_fadt(table: &[u8], revision: u8) -> Option<FadtInfo> {
    if table.len() < 48 {
        return None;
    }

    Some(FadtInfo {
        revision,
        preferred_power_profile: PowerProfile::from_raw(*table.get(45)?),
        sci_interrupt: read_u16(table, 46)?,
    })
}

fn parse_hpet(table: &[u8]) -> Option<HpetInfo> {
    if table.len() < 56 {
        return None;
    }

    let block_id = read_u32(table, 36)?;
    Some(HpetInfo {
        base_address: read_u64(table, 44)?,
        address_space: *table.get(40)?,
        comparator_count: (((block_id >> 8) & 0x1f) as u8).saturating_add(1),
        counter_is_64_bit: block_id & (1 << 13) != 0,
        legacy_irq_capable: block_id & (1 << 15) != 0,
        hpet_number: *table.get(52)?,
        minimum_tick: read_u16(table, 53)?,
    })
}

fn parse_mcfg(table: &[u8]) -> Option<McfgInfo> {
    const MCFG_HEADER_LENGTH: usize = SDT_HEADER_LENGTH + 8;
    const MCFG_ENTRY_LENGTH: usize = 16;

    if table.len() < MCFG_HEADER_LENGTH {
        return None;
    }

    let entries = &table[MCFG_HEADER_LENGTH..];
    let region_count = entries.len() / MCFG_ENTRY_LENGTH;
    let first_region = entries
        .get(..MCFG_ENTRY_LENGTH)
        .map(|entry| McfgRegionInfo {
            base_address: read_u64(entry, 0).unwrap_or(0),
            segment_group: read_u16(entry, 8).unwrap_or(0),
            start_bus: entry[10],
            end_bus: entry[11],
        });

    Some(McfgInfo {
        region_count: u16::try_from(region_count).unwrap_or(u16::MAX),
        first_region,
    })
}

fn checksum_is_valid(bytes: &[u8]) -> bool {
    bytes.iter().copied().fold(0u8, u8::wrapping_add) == 0
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let bytes: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let bytes: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let bytes: [u8; 8] = bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}
