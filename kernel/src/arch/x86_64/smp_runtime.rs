//! Live per-CPU execution state for x86_64 SMP.
//!
//! The AP trampoline establishes execution on secondary processors, while this
//! module supplies the first persistent runtime state needed after rendezvous:
//! dense CPU identity, per-CPU interrupt observations, and local-APIC timer
//! activation. Normal tasks still remain on the bootstrap scheduler until the
//! run-queue conversion is complete.

use core::{
    arch::asm,
    ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
};

const MAX_CPUS: usize = 64;
const MAX_XAPIC_IDS: usize = 256;
const UNMAPPED_CPU: u8 = u8::MAX;

const APIC_BASE_MSR: u32 = 0x1b;
const APIC_BASE_ENABLE: u64 = 1 << 11;
const APIC_BASE_X2APIC: u64 = 1 << 10;
const APIC_BASE_ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;

const LOCAL_APIC_ID: usize = 0x020;
const LOCAL_APIC_TASK_PRIORITY: usize = 0x080;
const LOCAL_APIC_SPURIOUS: usize = 0x0f0;
const LOCAL_APIC_ERROR_STATUS: usize = 0x280;
const LOCAL_APIC_LVT_TIMER: usize = 0x320;
const LOCAL_APIC_LVT_THERMAL: usize = 0x330;
const LOCAL_APIC_LVT_PERFORMANCE: usize = 0x340;
const LOCAL_APIC_LVT_LINT0: usize = 0x350;
const LOCAL_APIC_LVT_LINT1: usize = 0x360;
const LOCAL_APIC_LVT_ERROR: usize = 0x370;
const LOCAL_APIC_TIMER_INITIAL_COUNT: usize = 0x380;
const LOCAL_APIC_TIMER_DIVIDE_CONFIGURATION: usize = 0x3e0;
const LOCAL_APIC_SOFTWARE_ENABLE: u32 = 1 << 8;
const LOCAL_APIC_LVT_MASKED: u32 = 1 << 16;
const LOCAL_APIC_TIMER_PERIODIC: u32 = 1 << 17;
const LOCAL_APIC_TIMER_DIVIDE_BY_16_ENCODING: u32 = 0x3;

static PHYSICAL_MEMORY_OFFSET: AtomicU64 = AtomicU64::new(0);
static AP_TIMER_INITIAL_COUNT: AtomicU32 = AtomicU32::new(0);
static CPU_BY_APIC_ID: [AtomicU8; MAX_XAPIC_IDS] =
    [const { AtomicU8::new(UNMAPPED_CPU) }; MAX_XAPIC_IDS];
static CPU_LANES: [CpuLane; MAX_CPUS] = [const { CpuLane::new() }; MAX_CPUS];

struct CpuLane {
    prepared: AtomicBool,
    online: AtomicBool,
    apic_id: AtomicU32,
    timer_ticks: AtomicU64,
    reschedule_ipis: AtomicU64,
}

impl CpuLane {
    const fn new() -> Self {
        Self {
            prepared: AtomicBool::new(false),
            online: AtomicBool::new(false),
            apic_id: AtomicU32::new(u32::MAX),
            timer_ticks: AtomicU64::new(0),
            reschedule_ipis: AtomicU64::new(0),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    InvalidCpu,
    ApicUnavailable,
    X2ApicUnsupported,
    ApicIdMismatch,
    TimerUnavailable,
}

pub fn initialize_bootstrap(
    physical_memory_offset: u64,
    bsp_apic_id: u8,
    timer_initial_count: Option<u32>,
) {
    PHYSICAL_MEMORY_OFFSET.store(physical_memory_offset, Ordering::Release);
    AP_TIMER_INITIAL_COUNT.store(timer_initial_count.unwrap_or(0), Ordering::Release);
    prepare_cpu(0, bsp_apic_id).expect("bootstrap CPU index must be valid");
    CPU_LANES[0].online.store(true, Ordering::Release);
}

pub fn prepare_cpu(cpu_index: usize, apic_id: u8) -> Result<(), RuntimeError> {
    let lane = CPU_LANES.get(cpu_index).ok_or(RuntimeError::InvalidCpu)?;
    lane.apic_id.store(u32::from(apic_id), Ordering::Release);
    lane.prepared.store(true, Ordering::Release);
    CPU_BY_APIC_ID[usize::from(apic_id)].store(cpu_index as u8, Ordering::Release);
    Ok(())
}

pub fn activate_current_application_processor(
    timer_vector: u8,
    spurious_vector: u8,
) -> Result<usize, RuntimeError> {
    let apic_id = current_apic_id()?;
    let cpu_index = mapped_cpu_index(apic_id)?;
    if cpu_index == 0 {
        return Err(RuntimeError::InvalidCpu);
    }
    activate_application_processor(cpu_index, apic_id, timer_vector, spurious_vector)?;
    Ok(cpu_index)
}

pub fn activate_application_processor(
    cpu_index: usize,
    expected_apic_id: u8,
    timer_vector: u8,
    spurious_vector: u8,
) -> Result<(), RuntimeError> {
    let lane = CPU_LANES.get(cpu_index).ok_or(RuntimeError::InvalidCpu)?;
    if !lane.prepared.load(Ordering::Acquire) {
        return Err(RuntimeError::InvalidCpu);
    }

    let base = local_apic_base()?;
    let actual_apic_id = (read_u32(base, LOCAL_APIC_ID) >> 24) as u8;
    if actual_apic_id != expected_apic_id {
        return Err(RuntimeError::ApicIdMismatch);
    }

    write_u32(base, LOCAL_APIC_TASK_PRIORITY, 0);
    mask_lvt(base, LOCAL_APIC_LVT_TIMER);
    mask_lvt(base, LOCAL_APIC_LVT_THERMAL);
    mask_lvt(base, LOCAL_APIC_LVT_PERFORMANCE);
    mask_lvt(base, LOCAL_APIC_LVT_LINT0);
    mask_lvt(base, LOCAL_APIC_LVT_LINT1);
    mask_lvt(base, LOCAL_APIC_LVT_ERROR);
    write_u32(base, LOCAL_APIC_ERROR_STATUS, 0);
    let _ = read_u32(base, LOCAL_APIC_ERROR_STATUS);

    let spurious = read_u32(base, LOCAL_APIC_SPURIOUS);
    write_u32(
        base,
        LOCAL_APIC_SPURIOUS,
        (spurious & !0x1ff) | u32::from(spurious_vector) | LOCAL_APIC_SOFTWARE_ENABLE,
    );

    let initial_count = AP_TIMER_INITIAL_COUNT.load(Ordering::Acquire);
    if initial_count == 0 {
        return Err(RuntimeError::TimerUnavailable);
    }
    write_u32(
        base,
        LOCAL_APIC_TIMER_DIVIDE_CONFIGURATION,
        LOCAL_APIC_TIMER_DIVIDE_BY_16_ENCODING,
    );
    write_u32(
        base,
        LOCAL_APIC_LVT_TIMER,
        u32::from(timer_vector) | LOCAL_APIC_TIMER_PERIODIC,
    );
    write_u32(base, LOCAL_APIC_TIMER_INITIAL_COUNT, initial_count);

    lane.online.store(true, Ordering::Release);
    Ok(())
}

pub fn current_cpu_index() -> usize {
    current_apic_id()
        .and_then(mapped_cpu_index)
        .unwrap_or(0)
}

pub fn record_timer_tick(cpu_index: usize) -> Result<u64, RuntimeError> {
    let lane = CPU_LANES.get(cpu_index).ok_or(RuntimeError::InvalidCpu)?;
    Ok(lane
        .timer_ticks
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1))
}

pub fn record_reschedule_ipi(cpu_index: usize) -> Result<u64, RuntimeError> {
    let lane = CPU_LANES.get(cpu_index).ok_or(RuntimeError::InvalidCpu)?;
    Ok(lane
        .reschedule_ipis
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1))
}

pub fn timer_ticks(cpu_index: usize) -> u64 {
    CPU_LANES
        .get(cpu_index)
        .map(|lane| lane.timer_ticks.load(Ordering::Acquire))
        .unwrap_or(0)
}

pub fn reschedule_ipis(cpu_index: usize) -> u64 {
    CPU_LANES
        .get(cpu_index)
        .map(|lane| lane.reschedule_ipis.load(Ordering::Acquire))
        .unwrap_or(0)
}

pub fn is_online(cpu_index: usize) -> bool {
    CPU_LANES
        .get(cpu_index)
        .map(|lane| lane.online.load(Ordering::Acquire))
        .unwrap_or(false)
}

fn mapped_cpu_index(apic_id: u8) -> Result<usize, RuntimeError> {
    let cpu_index = CPU_BY_APIC_ID[usize::from(apic_id)].load(Ordering::Acquire);
    if cpu_index == UNMAPPED_CPU {
        Err(RuntimeError::InvalidCpu)
    } else {
        Ok(usize::from(cpu_index))
    }
}

fn current_apic_id() -> Result<u8, RuntimeError> {
    let base = local_apic_base()?;
    Ok((read_u32(base, LOCAL_APIC_ID) >> 24) as u8)
}

fn local_apic_base() -> Result<usize, RuntimeError> {
    let physical_memory_offset = PHYSICAL_MEMORY_OFFSET.load(Ordering::Acquire);
    if physical_memory_offset == 0 {
        return Err(RuntimeError::ApicUnavailable);
    }

    let base = unsafe { read_msr(APIC_BASE_MSR) };
    if base & APIC_BASE_X2APIC != 0 {
        return Err(RuntimeError::X2ApicUnsupported);
    }
    if base & APIC_BASE_ENABLE == 0 {
        return Err(RuntimeError::ApicUnavailable);
    }
    let physical = base & APIC_BASE_ADDRESS_MASK;
    if physical == 0 {
        return Err(RuntimeError::ApicUnavailable);
    }
    let virtual_base = physical_memory_offset
        .checked_add(physical)
        .ok_or(RuntimeError::ApicUnavailable)?;
    usize::try_from(virtual_base).map_err(|_| RuntimeError::ApicUnavailable)
}

fn mask_lvt(base: usize, offset: usize) {
    let value = read_u32(base, offset);
    write_u32(base, offset, value | LOCAL_APIC_LVT_MASKED);
}

fn read_u32(base: usize, offset: usize) -> u32 {
    let address = base
        .checked_add(offset)
        .expect("validated local APIC address overflowed");
    unsafe { ptr::read_volatile(address as *const u32) }
}

fn write_u32(base: usize, offset: usize, value: u32) {
    let address = base
        .checked_add(offset)
        .expect("validated local APIC address overflowed");
    unsafe { ptr::write_volatile(address as *mut u32, value) };
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
