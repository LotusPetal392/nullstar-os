use core::{
    arch::{asm, x86_64::__cpuid},
    fmt, ptr,
    sync::atomic::{AtomicU64, Ordering},
};

use x86_64::VirtAddr;

use crate::acpi::{InterruptPolarity, InterruptTriggerMode, IsaInterruptRoute, MadtInfo};

const APIC_BASE_MSR: u32 = 0x1b;
const APIC_BASE_ENABLE: u64 = 1 << 11;
const APIC_BASE_X2APIC: u64 = 1 << 10;
const APIC_BASE_ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;

const LOCAL_APIC_REGION_LENGTH: u64 = 0x400;
const LOCAL_APIC_ID: usize = 0x020;
const LOCAL_APIC_VERSION: usize = 0x030;
const LOCAL_APIC_TASK_PRIORITY: usize = 0x080;
const LOCAL_APIC_EOI: usize = 0x0b0;
const LOCAL_APIC_SPURIOUS: usize = 0x0f0;
const LOCAL_APIC_ERROR_STATUS: usize = 0x280;
const LOCAL_APIC_LVT_CMCI: usize = 0x2f0;
const LOCAL_APIC_LVT_TIMER: usize = 0x320;
const LOCAL_APIC_LVT_THERMAL: usize = 0x330;
const LOCAL_APIC_LVT_PERFORMANCE: usize = 0x340;
const LOCAL_APIC_LVT_LINT0: usize = 0x350;
const LOCAL_APIC_LVT_LINT1: usize = 0x360;
const LOCAL_APIC_LVT_ERROR: usize = 0x370;
const LOCAL_APIC_SOFTWARE_ENABLE: u32 = 1 << 8;
const LOCAL_APIC_LVT_MASKED: u32 = 1 << 16;

const IO_APIC_REGION_LENGTH: u64 = 0x20;
const IO_APIC_REGISTER_SELECT: usize = 0x00;
const IO_APIC_REGISTER_WINDOW: usize = 0x10;
const IO_APIC_ID: u32 = 0x00;
const IO_APIC_VERSION: u32 = 0x01;
const IO_APIC_REDIRECTION_BASE: u32 = 0x10;
const IO_APIC_REDIRECTION_MASKED: u32 = 1 << 16;
const IO_APIC_ACTIVE_LOW: u32 = 1 << 13;
const IO_APIC_LEVEL_TRIGGERED: u32 = 1 << 15;

static LOCAL_APIC_VIRTUAL_BASE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    CpuDoesNotSupportApic,
    X2ApicModeUnsupported,
    InvalidLocalApicAddress,
    LocalApicAddressMismatch,
    AddressOverflow,
    RegionOutsidePhysicalMap,
    MissingIoApic,
    InvalidIoApic,
    TimerRouteUnavailable,
    KeyboardRouteUnavailable,
    DuplicateInterruptRoute,
}

impl InitError {
    pub const fn description(self) -> &'static str {
        match self {
            Self::CpuDoesNotSupportApic => "CPU does not advertise APIC support",
            Self::X2ApicModeUnsupported => "x2APIC mode is already active",
            Self::InvalidLocalApicAddress => "MADT local APIC address is invalid",
            Self::LocalApicAddressMismatch => "MADT and APIC-base MSR addresses disagree",
            Self::AddressOverflow => "APIC address calculation overflowed",
            Self::RegionOutsidePhysicalMap => "APIC MMIO region is outside the physical mapping",
            Self::MissingIoApic => "MADT did not provide an I/O APIC",
            Self::InvalidIoApic => "I/O APIC registers reported invalid capabilities",
            Self::TimerRouteUnavailable => "PIT interrupt route is not handled by the I/O APIC",
            Self::KeyboardRouteUnavailable => {
                "keyboard interrupt route is not handled by the I/O APIC"
            }
            Self::DuplicateInterruptRoute => "timer and keyboard resolve to the same GSI",
        }
    }
}

impl fmt::Display for InitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.description())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ControllerInfo {
    pub local_apic_id: u8,
    pub local_apic_address: u64,
    pub local_apic_version: u8,
    pub io_apic_id: u8,
    pub io_apic_address: u32,
    pub io_apic_redirection_entries: u32,
    pub timer_route: IsaInterruptRoute,
    pub keyboard_route: IsaInterruptRoute,
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
    ) -> Result<Self, InitError> {
        let physical_end = physical_address
            .checked_add(length)
            .ok_or(InitError::AddressOverflow)?;
        if physical_end > physical_memory_end {
            return Err(InitError::RegionOutsidePhysicalMap);
        }

        let virtual_address = physical_memory_offset
            .as_u64()
            .checked_add(physical_address)
            .ok_or(InitError::AddressOverflow)?;
        let virtual_base =
            usize::try_from(virtual_address).map_err(|_| InitError::AddressOverflow)?;
        if virtual_base & 0x3 != 0 {
            return Err(InitError::AddressOverflow);
        }

        Ok(Self { virtual_base })
    }

    fn read_u32(self, offset: usize) -> u32 {
        let address = self
            .virtual_base
            .checked_add(offset)
            .expect("validated APIC MMIO address overflowed");
        unsafe { ptr::read_volatile(address as *const u32) }
    }

    fn write_u32(self, offset: usize, value: u32) {
        let address = self
            .virtual_base
            .checked_add(offset)
            .expect("validated APIC MMIO address overflowed");
        unsafe { ptr::write_volatile(address as *mut u32, value) };
    }
}

#[derive(Clone, Copy)]
struct LocalApic {
    mmio: MmioRegion,
}

impl LocalApic {
    fn configure(self, spurious_vector: u8) -> (u8, u8) {
        self.mmio.write_u32(LOCAL_APIC_TASK_PRIORITY, 0);

        let version_register = self.mmio.read_u32(LOCAL_APIC_VERSION);
        let version = (version_register & 0xff) as u8;
        let maximum_lvt_entry = ((version_register >> 16) & 0xff) as u8;

        self.mask_lvt(LOCAL_APIC_LVT_TIMER);
        if maximum_lvt_entry >= 1 {
            self.mask_lvt(LOCAL_APIC_LVT_THERMAL);
        }
        if maximum_lvt_entry >= 2 {
            self.mask_lvt(LOCAL_APIC_LVT_PERFORMANCE);
        }
        if maximum_lvt_entry >= 3 {
            self.mask_lvt(LOCAL_APIC_LVT_LINT0);
        }
        if maximum_lvt_entry >= 4 {
            self.mask_lvt(LOCAL_APIC_LVT_LINT1);
        }
        if maximum_lvt_entry >= 5 {
            self.mask_lvt(LOCAL_APIC_LVT_ERROR);
            self.mmio.write_u32(LOCAL_APIC_ERROR_STATUS, 0);
            let _ = self.mmio.read_u32(LOCAL_APIC_ERROR_STATUS);
        }
        if maximum_lvt_entry >= 6 {
            self.mask_lvt(LOCAL_APIC_LVT_CMCI);
        }

        let spurious = self.mmio.read_u32(LOCAL_APIC_SPURIOUS);
        self.mmio.write_u32(
            LOCAL_APIC_SPURIOUS,
            (spurious & !0x1ff) | u32::from(spurious_vector) | LOCAL_APIC_SOFTWARE_ENABLE,
        );

        let local_apic_id = (self.mmio.read_u32(LOCAL_APIC_ID) >> 24) as u8;
        (local_apic_id, version)
    }

    fn mask_lvt(self, offset: usize) {
        let value = self.mmio.read_u32(offset);
        self.mmio.write_u32(offset, value | LOCAL_APIC_LVT_MASKED);
    }
}

#[derive(Clone, Copy)]
struct IoApic {
    mmio: MmioRegion,
    global_system_interrupt_base: u32,
    redirection_entries: u32,
}

impl IoApic {
    fn discover(
        physical_address: u32,
        global_system_interrupt_base: u32,
        physical_memory_offset: VirtAddr,
        physical_memory_end: u64,
    ) -> Result<Self, InitError> {
        let mmio = MmioRegion::new(
            u64::from(physical_address),
            IO_APIC_REGION_LENGTH,
            physical_memory_offset,
            physical_memory_end,
        )?;
        let version = read_io_apic_register(mmio, IO_APIC_VERSION);
        let redirection_entries = ((version >> 16) & 0xff).saturating_add(1);
        if redirection_entries == 0 {
            return Err(InitError::InvalidIoApic);
        }

        Ok(Self {
            mmio,
            global_system_interrupt_base,
            redirection_entries,
        })
    }

    fn id(self) -> u8 {
        (read_io_apic_register(self.mmio, IO_APIC_ID) >> 24) as u8
    }

    fn contains(self, global_system_interrupt: u32) -> bool {
        global_system_interrupt >= self.global_system_interrupt_base
            && global_system_interrupt
                < self
                    .global_system_interrupt_base
                    .saturating_add(self.redirection_entries)
    }

    fn mask_all(self) {
        for index in 0..self.redirection_entries {
            let register = IO_APIC_REDIRECTION_BASE + index * 2;
            let low = read_io_apic_register(self.mmio, register);
            write_io_apic_register(self.mmio, register, low | IO_APIC_REDIRECTION_MASKED);
        }
    }

    fn route(self, route: IsaInterruptRoute, vector: u8, destination_apic_id: u8) {
        let index = route
            .global_system_interrupt
            .saturating_sub(self.global_system_interrupt_base);
        let register = IO_APIC_REDIRECTION_BASE + index * 2;

        write_io_apic_register(self.mmio, register, IO_APIC_REDIRECTION_MASKED);
        write_io_apic_register(
            self.mmio,
            register + 1,
            u32::from(destination_apic_id) << 24,
        );

        let mut low = u32::from(vector);
        if route.polarity == InterruptPolarity::ActiveLow {
            low |= IO_APIC_ACTIVE_LOW;
        }
        if route.trigger_mode == InterruptTriggerMode::Level {
            low |= IO_APIC_LEVEL_TRIGGERED;
        }
        write_io_apic_register(self.mmio, register, low);
    }
}

pub fn init(
    madt: &MadtInfo,
    physical_memory_offset: VirtAddr,
    physical_memory_end: u64,
    timer_vector: u8,
    keyboard_vector: u8,
    spurious_vector: u8,
) -> Result<ControllerInfo, InitError> {
    let features = unsafe { __cpuid(1) };
    if features.edx & (1 << 9) == 0 {
        return Err(InitError::CpuDoesNotSupportApic);
    }

    let apic_base_msr = unsafe { read_msr(APIC_BASE_MSR) };
    if apic_base_msr & APIC_BASE_X2APIC != 0 {
        return Err(InitError::X2ApicModeUnsupported);
    }

    let madt_local_apic_address = madt.local_apic_address;
    if madt_local_apic_address == 0
        || madt_local_apic_address & 0xfff != 0
        || madt_local_apic_address & !APIC_BASE_ADDRESS_MASK != 0
    {
        return Err(InitError::InvalidLocalApicAddress);
    }

    let msr_local_apic_address = apic_base_msr & APIC_BASE_ADDRESS_MASK;
    if msr_local_apic_address != madt_local_apic_address {
        return Err(InitError::LocalApicAddressMismatch);
    }

    let local_apic = LocalApic {
        mmio: MmioRegion::new(
            madt_local_apic_address,
            LOCAL_APIC_REGION_LENGTH,
            physical_memory_offset,
            physical_memory_end,
        )?,
    };

    let io_apic_info = madt.first_io_apic.ok_or(InitError::MissingIoApic)?;
    let io_apic = IoApic::discover(
        io_apic_info.address,
        io_apic_info.global_system_interrupt_base,
        physical_memory_offset,
        physical_memory_end,
    )?;

    let timer_route = madt.timer_route;
    if !io_apic.contains(timer_route.global_system_interrupt) {
        return Err(InitError::TimerRouteUnavailable);
    }

    let keyboard_route = madt.keyboard_route;
    if !io_apic.contains(keyboard_route.global_system_interrupt) {
        return Err(InitError::KeyboardRouteUnavailable);
    }
    if timer_route.global_system_interrupt == keyboard_route.global_system_interrupt {
        return Err(InitError::DuplicateInterruptRoute);
    }

    if apic_base_msr & APIC_BASE_ENABLE == 0 {
        unsafe { write_msr(APIC_BASE_MSR, apic_base_msr | APIC_BASE_ENABLE) };
    }

    let (local_apic_id, local_apic_version) = local_apic.configure(spurious_vector);
    io_apic.mask_all();
    io_apic.route(timer_route, timer_vector, local_apic_id);
    io_apic.route(keyboard_route, keyboard_vector, local_apic_id);

    LOCAL_APIC_VIRTUAL_BASE.store(local_apic.mmio.virtual_base as u64, Ordering::Release);

    Ok(ControllerInfo {
        local_apic_id,
        local_apic_address: madt_local_apic_address,
        local_apic_version,
        io_apic_id: io_apic.id(),
        io_apic_address: io_apic_info.address,
        io_apic_redirection_entries: io_apic.redirection_entries,
        timer_route,
        keyboard_route,
    })
}

pub fn end_of_interrupt() {
    let virtual_base = LOCAL_APIC_VIRTUAL_BASE.load(Ordering::Acquire);
    if virtual_base == 0 {
        return;
    }

    let address = virtual_base
        .checked_add(LOCAL_APIC_EOI as u64)
        .expect("local APIC EOI address overflowed");
    unsafe { ptr::write_volatile(address as *mut u32, 0) };
}

fn read_io_apic_register(mmio: MmioRegion, register: u32) -> u32 {
    mmio.write_u32(IO_APIC_REGISTER_SELECT, register);
    mmio.read_u32(IO_APIC_REGISTER_WINDOW)
}

fn write_io_apic_register(mmio: MmioRegion, register: u32, value: u32) {
    mmio.write_u32(IO_APIC_REGISTER_SELECT, register);
    mmio.write_u32(IO_APIC_REGISTER_WINDOW, value);
}

unsafe fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nostack, preserves_flags)
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}

unsafe fn write_msr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nostack, preserves_flags)
        );
    }
}
