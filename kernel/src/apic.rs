use core::{
    arch::x86_64::__cpuid,
    fmt, ptr, slice,
    sync::atomic::{AtomicU64, Ordering},
};

use x86_64::{VirtAddr, registers::model_specific::Msr};

use crate::acpi::{AcpiInfo, RootTableKind};

const IA32_APIC_BASE_MSR: u32 = 0x1b;
const APIC_BASE_ADDRESS_MASK: u64 = 0x0000_000f_ffff_f000;
const APIC_BASE_X2APIC_ENABLE: u64 = 1 << 10;
const APIC_BASE_GLOBAL_ENABLE: u64 = 1 << 11;

const LOCAL_APIC_PAGE_SIZE: usize = 4096;
const LOCAL_APIC_ID_REGISTER: usize = 0x020;
const LOCAL_APIC_VERSION_REGISTER: usize = 0x030;
const LOCAL_APIC_TASK_PRIORITY_REGISTER: usize = 0x080;
const LOCAL_APIC_EOI_REGISTER: usize = 0x0b0;
const LOCAL_APIC_SPURIOUS_REGISTER: usize = 0x0f0;
const LOCAL_APIC_ERROR_STATUS_REGISTER: usize = 0x280;
const LOCAL_APIC_LVT_TIMER_REGISTER: usize = 0x320;
const LOCAL_APIC_LVT_THERMAL_REGISTER: usize = 0x330;
const LOCAL_APIC_LVT_PERFORMANCE_REGISTER: usize = 0x340;
const LOCAL_APIC_LVT_LINT0_REGISTER: usize = 0x350;
const LOCAL_APIC_LVT_LINT1_REGISTER: usize = 0x360;
const LOCAL_APIC_LVT_ERROR_REGISTER: usize = 0x370;
const LOCAL_APIC_SOFTWARE_ENABLE: u32 = 1 << 8;
const LOCAL_APIC_LVT_MASKED: u32 = 1 << 16;

const IO_APIC_PAGE_SIZE: usize = 4096;
const IO_APIC_REGISTER_SELECT: usize = 0x00;
const IO_APIC_REGISTER_WINDOW: usize = 0x10;
const IO_APIC_ID_REGISTER: u32 = 0x00;
const IO_APIC_VERSION_REGISTER: u32 = 0x01;
const IO_APIC_REDIRECTION_BASE: u32 = 0x10;
const IO_APIC_REDIRECTION_MASKED: u32 = 1 << 16;
const IO_APIC_POLARITY_LOW: u32 = 1 << 13;
const IO_APIC_TRIGGER_LEVEL: u32 = 1 << 15;

const SDT_HEADER_LENGTH: usize = 36;
const MADT_FIXED_LENGTH: usize = SDT_HEADER_LENGTH + 8;
const MAX_ACPI_TABLE_LENGTH: usize = 16 * 1024 * 1024;

static LOCAL_APIC_VIRTUAL_BASE: AtomicU64 = AtomicU64::new(0);
static SPURIOUS_INTERRUPT_COUNT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    ActiveHigh,
    ActiveLow,
}

impl fmt::Display for Polarity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveHigh => formatter.write_str("active-high"),
            Self::ActiveLow => formatter.write_str("active-low"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMode {
    Edge,
    Level,
}

impl fmt::Display for TriggerMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Edge => formatter.write_str("edge"),
            Self::Level => formatter.write_str("level"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApicError {
    MissingAcpi,
    MissingMadt,
    MissingIoApic,
    InvalidRootTable,
    InvalidMadt,
    AddressOverflow,
    RegionOutsidePhysicalMap { address: u64, length: usize },
    LocalApicUnsupported,
    X2ApicAlreadyEnabled,
    InvalidLocalApicAddress(u64),
    InvalidInterruptOverride { irq: u8, flags: u16 },
    GsiOutsideIoApic(u32),
    DuplicateLegacyGsi(u32),
}

#[derive(Debug, Clone, Copy)]
pub struct ApicInfo {
    pub local_apic_address: u64,
    pub local_apic_id: u8,
    pub local_apic_version: u8,
    pub io_apic_address: u64,
    pub io_apic_firmware_id: u8,
    pub io_apic_id: u8,
    pub io_apic_version: u8,
    pub io_apic_redirection_entries: u16,
    pub timer_gsi: u32,
    pub timer_polarity: Polarity,
    pub timer_trigger_mode: TriggerMode,
    pub keyboard_gsi: u32,
    pub keyboard_polarity: Polarity,
    pub keyboard_trigger_mode: TriggerMode,
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

    fn region(&self, physical_address: u64, length: usize) -> Result<&'static [u8], ApicError> {
        let virtual_address = self.virtual_address(physical_address, length)?;
        Ok(unsafe { slice::from_raw_parts(virtual_address as *const u8, length) })
    }

    fn virtual_address(&self, physical_address: u64, length: usize) -> Result<usize, ApicError> {
        let length_u64 = u64::try_from(length).map_err(|_| ApicError::AddressOverflow)?;
        let region_end = physical_address
            .checked_add(length_u64)
            .ok_or(ApicError::AddressOverflow)?;
        if region_end > self.physical_end {
            return Err(ApicError::RegionOutsidePhysicalMap {
                address: physical_address,
                length,
            });
        }

        let virtual_address = self
            .virtual_offset
            .checked_add(physical_address)
            .ok_or(ApicError::AddressOverflow)?;
        usize::try_from(virtual_address).map_err(|_| ApicError::AddressOverflow)
    }
}

#[derive(Clone, Copy)]
struct LegacyRoute {
    gsi: u32,
    polarity: Polarity,
    trigger_mode: TriggerMode,
}

struct LocalApic {
    virtual_address: usize,
}

impl LocalApic {
    fn read(&self, offset: usize) -> u32 {
        unsafe { ptr::read_volatile((self.virtual_address + offset) as *const u32) }
    }

    fn write(&self, offset: usize, value: u32) {
        unsafe { ptr::write_volatile((self.virtual_address + offset) as *mut u32, value) }
    }

    fn mask_lvt(&self, offset: usize) {
        self.write(offset, self.read(offset) | LOCAL_APIC_LVT_MASKED);
    }
}

struct IoApic {
    virtual_address: usize,
}

impl IoApic {
    fn read(&self, register: u32) -> u32 {
        unsafe {
            ptr::write_volatile(
                (self.virtual_address + IO_APIC_REGISTER_SELECT) as *mut u32,
                register,
            );
            ptr::read_volatile((self.virtual_address + IO_APIC_REGISTER_WINDOW) as *const u32)
        }
    }

    fn write(&self, register: u32, value: u32) {
        unsafe {
            ptr::write_volatile(
                (self.virtual_address + IO_APIC_REGISTER_SELECT) as *mut u32,
                register,
            );
            ptr::write_volatile(
                (self.virtual_address + IO_APIC_REGISTER_WINDOW) as *mut u32,
                value,
            );
        }
    }

    fn mask_redirection(&self, index: u16) {
        let register = IO_APIC_REDIRECTION_BASE + u32::from(index) * 2;
        self.write(register, self.read(register) | IO_APIC_REDIRECTION_MASKED);
    }

    fn write_redirection(&self, index: u16, low: u32, high: u32) {
        let register = IO_APIC_REDIRECTION_BASE + u32::from(index) * 2;
        self.write(register, low | IO_APIC_REDIRECTION_MASKED);
        self.write(register + 1, high);
        self.write(register, low);
    }
}

pub fn init(
    acpi_info: Option<&AcpiInfo>,
    physical_memory_offset: VirtAddr,
    physical_memory_end: u64,
    timer_vector: u8,
    keyboard_vector: u8,
    spurious_vector: u8,
) -> Result<ApicInfo, ApicError> {
    let acpi_info = acpi_info.ok_or(ApicError::MissingAcpi)?;
    let madt_info = acpi_info.madt.ok_or(ApicError::MissingMadt)?;
    let io_apic_info = madt_info.first_io_apic.ok_or(ApicError::MissingIoApic)?;
    let memory = PhysicalMemory::new(physical_memory_offset, physical_memory_end);
    let madt = read_madt(memory, acpi_info)?;

    let cpuid = unsafe { __cpuid(1) };
    if cpuid.edx & (1 << 9) == 0 {
        return Err(ApicError::LocalApicUnsupported);
    }

    let timer_route = resolve_legacy_route(madt, 0)?;
    let keyboard_route = resolve_legacy_route(madt, 1)?;
    if timer_route.gsi == keyboard_route.gsi {
        return Err(ApicError::DuplicateLegacyGsi(timer_route.gsi));
    }

    let io_apic_address = u64::from(io_apic_info.address);
    let io_apic_virtual_address = memory.virtual_address(io_apic_address, IO_APIC_PAGE_SIZE)?;
    let io_apic = IoApic {
        virtual_address: io_apic_virtual_address,
    };
    let io_apic_id = ((io_apic.read(IO_APIC_ID_REGISTER) >> 24) & 0x0f) as u8;
    let io_apic_version_register = io_apic.read(IO_APIC_VERSION_REGISTER);
    let io_apic_version = (io_apic_version_register & 0xff) as u8;
    let redirection_entries = (((io_apic_version_register >> 16) & 0xff) as u16) + 1;

    for index in 0..redirection_entries {
        io_apic.mask_redirection(index);
    }

    let timer_index = redirection_index(
        io_apic_info.global_system_interrupt_base,
        redirection_entries,
        timer_route.gsi,
    )?;
    let keyboard_index = redirection_index(
        io_apic_info.global_system_interrupt_base,
        redirection_entries,
        keyboard_route.gsi,
    )?;

    let (local_apic_address, local_apic_virtual_address) =
        configure_apic_base(memory, madt_info.local_apic_address)?;
    let local_apic = LocalApic {
        virtual_address: local_apic_virtual_address,
    };
    let local_apic_id = ((local_apic.read(LOCAL_APIC_ID_REGISTER) >> 24) & 0xff) as u8;
    let local_apic_version = (local_apic.read(LOCAL_APIC_VERSION_REGISTER) & 0xff) as u8;

    local_apic.write(LOCAL_APIC_TASK_PRIORITY_REGISTER, 0);
    local_apic.mask_lvt(LOCAL_APIC_LVT_TIMER_REGISTER);
    local_apic.mask_lvt(LOCAL_APIC_LVT_THERMAL_REGISTER);
    local_apic.mask_lvt(LOCAL_APIC_LVT_PERFORMANCE_REGISTER);
    local_apic.mask_lvt(LOCAL_APIC_LVT_LINT0_REGISTER);
    local_apic.mask_lvt(LOCAL_APIC_LVT_LINT1_REGISTER);
    local_apic.mask_lvt(LOCAL_APIC_LVT_ERROR_REGISTER);
    local_apic.write(LOCAL_APIC_ERROR_STATUS_REGISTER, 0);
    let _ = local_apic.read(LOCAL_APIC_ERROR_STATUS_REGISTER);
    local_apic.write(
        LOCAL_APIC_SPURIOUS_REGISTER,
        u32::from(spurious_vector) | LOCAL_APIC_SOFTWARE_ENABLE,
    );

    io_apic.write_redirection(
        timer_index,
        redirection_low(timer_vector, timer_route),
        u32::from(local_apic_id) << 24,
    );
    io_apic.write_redirection(
        keyboard_index,
        redirection_low(keyboard_vector, keyboard_route),
        u32::from(local_apic_id) << 24,
    );

    LOCAL_APIC_VIRTUAL_BASE.store(local_apic_virtual_address as u64, Ordering::Release);

    Ok(ApicInfo {
        local_apic_address,
        local_apic_id,
        local_apic_version,
        io_apic_address,
        io_apic_firmware_id: io_apic_info.id,
        io_apic_id,
        io_apic_version,
        io_apic_redirection_entries: redirection_entries,
        timer_gsi: timer_route.gsi,
        timer_polarity: timer_route.polarity,
        timer_trigger_mode: timer_route.trigger_mode,
        keyboard_gsi: keyboard_route.gsi,
        keyboard_polarity: keyboard_route.polarity,
        keyboard_trigger_mode: keyboard_route.trigger_mode,
    })
}

pub fn end_of_interrupt() {
    let virtual_address = LOCAL_APIC_VIRTUAL_BASE.load(Ordering::Acquire);
    if virtual_address != 0 {
        unsafe {
            ptr::write_volatile(
                (virtual_address as usize + LOCAL_APIC_EOI_REGISTER) as *mut u32,
                0,
            );
        }
    }
}

pub fn record_spurious_interrupt() {
    SPURIOUS_INTERRUPT_COUNT.fetch_add(1, Ordering::Relaxed);
}

pub fn spurious_interrupt_count() -> u64 {
    SPURIOUS_INTERRUPT_COUNT.load(Ordering::Relaxed)
}

fn configure_apic_base(
    memory: PhysicalMemory,
    madt_address: u64,
) -> Result<(u64, usize), ApicError> {
    let mut msr = Msr::new(IA32_APIC_BASE_MSR);
    let current = unsafe { msr.read() };
    if current & APIC_BASE_X2APIC_ENABLE != 0 {
        return Err(ApicError::X2ApicAlreadyEnabled);
    }

    let local_apic_address = if madt_address == 0 {
        current & APIC_BASE_ADDRESS_MASK
    } else {
        madt_address
    };
    if local_apic_address == 0
        || local_apic_address & 0xfff != 0
        || local_apic_address & !APIC_BASE_ADDRESS_MASK != 0
    {
        return Err(ApicError::InvalidLocalApicAddress(local_apic_address));
    }

    let virtual_address = memory.virtual_address(local_apic_address, LOCAL_APIC_PAGE_SIZE)?;
    let configured = (current & !APIC_BASE_ADDRESS_MASK)
        | (local_apic_address & APIC_BASE_ADDRESS_MASK)
        | APIC_BASE_GLOBAL_ENABLE;
    unsafe {
        msr.write(configured);
    }

    Ok((local_apic_address, virtual_address))
}

fn redirection_index(gsi_base: u32, entry_count: u16, gsi: u32) -> Result<u16, ApicError> {
    let index = gsi
        .checked_sub(gsi_base)
        .ok_or(ApicError::GsiOutsideIoApic(gsi))?;
    if index >= u32::from(entry_count) {
        return Err(ApicError::GsiOutsideIoApic(gsi));
    }

    u16::try_from(index).map_err(|_| ApicError::GsiOutsideIoApic(gsi))
}

fn redirection_low(vector: u8, route: LegacyRoute) -> u32 {
    let mut value = u32::from(vector);
    if route.polarity == Polarity::ActiveLow {
        value |= IO_APIC_POLARITY_LOW;
    }
    if route.trigger_mode == TriggerMode::Level {
        value |= IO_APIC_TRIGGER_LEVEL;
    }
    value
}

fn resolve_legacy_route(madt: &[u8], irq: u8) -> Result<LegacyRoute, ApicError> {
    let mut cursor = MADT_FIXED_LENGTH;
    while cursor < madt.len() {
        if cursor + 2 > madt.len() {
            return Err(ApicError::InvalidMadt);
        }
        let entry_type = madt[cursor];
        let entry_length = usize::from(madt[cursor + 1]);
        if entry_length < 2 || cursor.saturating_add(entry_length) > madt.len() {
            return Err(ApicError::InvalidMadt);
        }
        let entry = &madt[cursor..cursor + entry_length];

        if entry_type == 2 && entry.len() >= 10 && entry[2] == 0 && entry[3] == irq {
            let flags = read_u16(entry, 8).ok_or(ApicError::InvalidMadt)?;
            let polarity = match flags & 0b11 {
                0 | 1 => Polarity::ActiveHigh,
                3 => Polarity::ActiveLow,
                _ => return Err(ApicError::InvalidInterruptOverride { irq, flags }),
            };
            let trigger_mode = match (flags >> 2) & 0b11 {
                0 | 1 => TriggerMode::Edge,
                3 => TriggerMode::Level,
                _ => return Err(ApicError::InvalidInterruptOverride { irq, flags }),
            };

            return Ok(LegacyRoute {
                gsi: read_u32(entry, 4).ok_or(ApicError::InvalidMadt)?,
                polarity,
                trigger_mode,
            });
        }

        cursor += entry_length;
    }

    Ok(LegacyRoute {
        gsi: u32::from(irq),
        polarity: Polarity::ActiveHigh,
        trigger_mode: TriggerMode::Edge,
    })
}

fn read_madt(memory: PhysicalMemory, acpi_info: &AcpiInfo) -> Result<&'static [u8], ApicError> {
    let root_table = read_table(memory, acpi_info.root_table_address)?;
    let entry_size = match acpi_info.root_table_kind {
        RootTableKind::Rsdt => 4,
        RootTableKind::Xsdt => 8,
    };
    let expected_signature: &[u8; 4] = match acpi_info.root_table_kind {
        RootTableKind::Rsdt => b"RSDT",
        RootTableKind::Xsdt => b"XSDT",
    };
    if root_table.get(..4) != Some(&expected_signature[..]) {
        return Err(ApicError::InvalidRootTable);
    }

    let entries = root_table
        .get(SDT_HEADER_LENGTH..)
        .ok_or(ApicError::InvalidRootTable)?;
    if entries.len() % entry_size != 0 {
        return Err(ApicError::InvalidRootTable);
    }

    for entry in entries.chunks_exact(entry_size) {
        let table_address = match acpi_info.root_table_kind {
            RootTableKind::Rsdt => {
                u64::from(read_u32(entry, 0).ok_or(ApicError::InvalidRootTable)?)
            }
            RootTableKind::Xsdt => read_u64(entry, 0).ok_or(ApicError::InvalidRootTable)?,
        };
        if table_address == 0 {
            continue;
        }

        let header = match memory.region(table_address, SDT_HEADER_LENGTH) {
            Ok(header) => header,
            Err(_) => continue,
        };
        if header.get(..4) == Some(b"APIC") {
            let table = read_table(memory, table_address)?;
            if table.len() < MADT_FIXED_LENGTH {
                return Err(ApicError::InvalidMadt);
            }
            return Ok(table);
        }
    }

    Err(ApicError::MissingMadt)
}

fn read_table(memory: PhysicalMemory, physical_address: u64) -> Result<&'static [u8], ApicError> {
    let header = memory.region(physical_address, SDT_HEADER_LENGTH)?;
    let length = usize::try_from(read_u32(header, 4).ok_or(ApicError::InvalidRootTable)?)
        .map_err(|_| ApicError::InvalidRootTable)?;
    if !(SDT_HEADER_LENGTH..=MAX_ACPI_TABLE_LENGTH).contains(&length) {
        return Err(ApicError::InvalidRootTable);
    }

    let table = memory.region(physical_address, length)?;
    if table.iter().copied().fold(0u8, u8::wrapping_add) != 0 {
        return Err(ApicError::InvalidRootTable);
    }
    Ok(table)
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
