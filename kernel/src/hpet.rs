use core::{fmt, hint::spin_loop, ptr};

use x86_64::VirtAddr;

use crate::acpi::HpetInfo;

const HPET_REGION_LENGTH: u64 = 0x100;
const GENERAL_CAPABILITIES: usize = 0x000;
const GENERAL_CONFIGURATION: usize = 0x010;
const MAIN_COUNTER_VALUE: usize = 0x0f0;
const GENERAL_CONFIGURATION_ENABLE: u64 = 1;
const FEMTOSECONDS_PER_SECOND: u128 = 1_000_000_000_000_000;
const MAX_CLOCK_PERIOD_FEMTOSECONDS: u64 = 100_000_000;
const MAX_WAIT_ITERATIONS: u64 = 10_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    UnsupportedAddressSpace,
    InvalidBaseAddress,
    AddressOverflow,
    RegionOutsidePhysicalMap,
    InvalidClockPeriod,
    CounterDidNotAdvance,
    IntervalTooLong,
}

impl Error {
    pub const fn description(self) -> &'static str {
        match self {
            Self::UnsupportedAddressSpace => "HPET is not in system memory",
            Self::InvalidBaseAddress => "HPET base address is invalid",
            Self::AddressOverflow => "HPET address calculation overflowed",
            Self::RegionOutsidePhysicalMap => "HPET MMIO region is outside the physical mapping",
            Self::InvalidClockPeriod => "HPET reported an invalid clock period",
            Self::CounterDidNotAdvance => "HPET main counter did not advance",
            Self::IntervalTooLong => "HPET calibration interval is too long",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.description())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TimerInfo {
    pub period_femtoseconds: u64,
    pub frequency_hz: u64,
    pub counter_is_64_bit: bool,
    pub comparator_count: u8,
    pub legacy_irq_capable: bool,
    pub vendor_id: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct Measurement {
    pub elapsed_ticks: u64,
    pub elapsed_femtoseconds: u128,
}

#[derive(Clone, Copy)]
pub struct Hpet {
    mmio: MmioRegion,
    info: TimerInfo,
}

impl Hpet {
    pub fn new(
        acpi_info: &HpetInfo,
        physical_memory_offset: VirtAddr,
        physical_memory_end: u64,
    ) -> Result<Self, Error> {
        if acpi_info.address_space != 0 {
            return Err(Error::UnsupportedAddressSpace);
        }
        if acpi_info.base_address == 0 || acpi_info.base_address & 0x7 != 0 {
            return Err(Error::InvalidBaseAddress);
        }

        let mmio = MmioRegion::new(
            acpi_info.base_address,
            HPET_REGION_LENGTH,
            physical_memory_offset,
            physical_memory_end,
        )?;
        let capabilities = mmio.read_u64(GENERAL_CAPABILITIES);
        let period_femtoseconds = capabilities >> 32;
        if period_femtoseconds == 0
            || period_femtoseconds > MAX_CLOCK_PERIOD_FEMTOSECONDS
        {
            return Err(Error::InvalidClockPeriod);
        }

        let frequency_hz = u64::try_from(
            FEMTOSECONDS_PER_SECOND / u128::from(period_femtoseconds),
        )
        .map_err(|_| Error::InvalidClockPeriod)?;
        if frequency_hz == 0 {
            return Err(Error::InvalidClockPeriod);
        }

        let timer = Self {
            mmio,
            info: TimerInfo {
                period_femtoseconds,
                frequency_hz,
                counter_is_64_bit: capabilities & (1 << 13) != 0,
                comparator_count: (((capabilities >> 8) & 0x1f) as u8).saturating_add(1),
                legacy_irq_capable: capabilities & (1 << 15) != 0,
                vendor_id: ((capabilities >> 16) & 0xffff) as u16,
            },
        };

        let configuration = timer.mmio.read_u64(GENERAL_CONFIGURATION);
        timer.mmio.write_u64(
            GENERAL_CONFIGURATION,
            configuration | GENERAL_CONFIGURATION_ENABLE,
        );
        timer.wait_ticks(1)?;

        Ok(timer)
    }

    pub const fn info(self) -> TimerInfo {
        self.info
    }

    pub fn measure_duration(self, duration_femtoseconds: u64) -> Result<Measurement, Error> {
        let target_ticks = (u128::from(duration_femtoseconds)
            + u128::from(self.info.period_femtoseconds)
            - 1)
            / u128::from(self.info.period_femtoseconds);
        let target_ticks = u64::try_from(target_ticks.max(1)).map_err(|_| Error::IntervalTooLong)?;
        let elapsed_ticks = self.wait_ticks(target_ticks)?;

        Ok(Measurement {
            elapsed_ticks,
            elapsed_femtoseconds: u128::from(elapsed_ticks)
                * u128::from(self.info.period_femtoseconds),
        })
    }

    fn wait_ticks(self, target_ticks: u64) -> Result<u64, Error> {
        let start = self.read_counter();

        for _ in 0..MAX_WAIT_ITERATIONS {
            let elapsed = self.elapsed_ticks(start, self.read_counter());
            if elapsed >= target_ticks {
                return Ok(elapsed);
            }
            spin_loop();
        }

        Err(Error::CounterDidNotAdvance)
    }

    fn read_counter(self) -> u64 {
        let value = self.mmio.read_u64(MAIN_COUNTER_VALUE);
        if self.info.counter_is_64_bit {
            value
        } else {
            value & u64::from(u32::MAX)
        }
    }

    fn elapsed_ticks(self, start: u64, end: u64) -> u64 {
        if self.info.counter_is_64_bit {
            end.wrapping_sub(start)
        } else {
            u64::from((end as u32).wrapping_sub(start as u32))
        }
    }
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
        let physical_end = physical_address
            .checked_add(length)
            .ok_or(Error::AddressOverflow)?;
        if physical_end > physical_memory_end {
            return Err(Error::RegionOutsidePhysicalMap);
        }

        let virtual_address = physical_memory_offset
            .as_u64()
            .checked_add(physical_address)
            .ok_or(Error::AddressOverflow)?;
        let virtual_base = usize::try_from(virtual_address).map_err(|_| Error::AddressOverflow)?;
        if virtual_base & 0x7 != 0 {
            return Err(Error::InvalidBaseAddress);
        }

        Ok(Self { virtual_base })
    }

    fn read_u64(self, offset: usize) -> u64 {
        let address = self
            .virtual_base
            .checked_add(offset)
            .expect("validated HPET MMIO address overflowed");
        unsafe { ptr::read_volatile(address as *const u64) }
    }

    fn write_u64(self, offset: usize, value: u64) {
        let address = self
            .virtual_base
            .checked_add(offset)
            .expect("validated HPET MMIO address overflowed");
        unsafe { ptr::write_volatile(address as *mut u64, value) };
    }
}
