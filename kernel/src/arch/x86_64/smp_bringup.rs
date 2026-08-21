//! x86_64 application-processor startup and rendezvous state.
//!
//! This layer owns the architectural INIT/SIPI sequence and the bounded
//! rendezvous state that bridges the low-memory trampoline to per-CPU kernel
//! initialization. Application processors are started sequentially from the
//! MADT topology and remain parked until the live SMP scheduler is attached.

use alloc::{boxed::Box, vec};
use core::sync::atomic::{AtomicU64, Ordering, compiler_fence};

use x86_64::VirtAddr;

use crate::{
    acpi::MadtInfo,
    gdt,
    hpet::{self, Hpet},
    interrupts, serial_println,
};

use super::{
    ap_trampoline::ApTrampoline,
    ipi::{self, IpiError},
};

const INIT_TO_SIPI_DELAY_FEMTOSECONDS: u64 = 10_000_000_000_000;
const BETWEEN_SIPI_DELAY_FEMTOSECONDS: u64 = 200_000_000_000;
const AP_ONLINE_POLL_DELAY_FEMTOSECONDS: u64 = 100_000_000_000;
const AP_ONLINE_TIMER_POLLS: usize = 10_000;
const FALLBACK_INIT_DELAY_ITERATIONS: usize = 2_000_000;
const FALLBACK_SIPI_DELAY_ITERATIONS: usize = 50_000;
const FALLBACK_ONLINE_POLL_LIMIT: usize = 5_000_000;
const STARTUP_VECTOR_MIN: u8 = 1;
const STARTUP_VECTOR_MAX: u8 = 0xff;
const MAX_SMP_CPUS: usize = 64;
const AP_BOOT_STACK_SIZE: usize = 128 * 1024;

static AP_RENDEZVOUS: ApRendezvous = ApRendezvous::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApStartupError {
    InvalidVector,
    InvalidCpu,
    AlreadyOnline,
    Ipi(IpiError),
    Timer(hpet::Error),
}

impl From<IpiError> for ApStartupError {
    fn from(error: IpiError) -> Self {
        Self::Ipi(error)
    }
}

impl From<hpet::Error> for ApStartupError {
    fn from(error: hpet::Error) -> Self {
        Self::Timer(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApStartupStage {
    InitAsserted,
    InitDeasserted,
    StartupSent,
    AwaitingTrampoline,
    TrampolineEntered,
    KernelInitialized,
    Online,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApStartup {
    pub apic_id: u8,
    pub cpu_index: usize,
    pub startup_vector: u8,
    pub stage: ApStartupStage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BringupError {
    TrampolineUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BringupSummary {
    pub candidates: usize,
    pub attempted: usize,
    pub online: usize,
    pub failed: usize,
    pub trampoline_physical_address: u64,
}

/// Per-CPU state shared between the BSP startup coordinator and the AP entry
/// path. Atomic state allows the AP to publish readiness without taking a
/// scheduler lock before its per-CPU environment is initialized.
pub struct ApRendezvous {
    states: [AtomicU64; MAX_SMP_CPUS],
}

impl ApRendezvous {
    pub const fn new() -> Self {
        Self {
            states: [const { AtomicU64::new(StageCode::InitAsserted as u64) }; MAX_SMP_CPUS],
        }
    }

    pub fn reset(&self, cpu_index: usize) -> Result<(), ApStartupError> {
        let state = self.state(cpu_index)?;
        state.store(StageCode::AwaitingTrampoline as u64, Ordering::Release);
        Ok(())
    }

    pub fn publish_trampoline_entry(&self, cpu_index: usize) -> Result<(), ApStartupError> {
        let state = self.state(cpu_index)?;
        state.store(StageCode::TrampolineEntered as u64, Ordering::Release);
        Ok(())
    }

    pub fn publish_kernel_initialized(&self, cpu_index: usize) -> Result<(), ApStartupError> {
        let state = self.state(cpu_index)?;
        state.store(StageCode::KernelInitialized as u64, Ordering::Release);
        Ok(())
    }

    pub fn publish_online(&self, cpu_index: usize) -> Result<(), ApStartupError> {
        let state = self.state(cpu_index)?;
        if state.load(Ordering::Acquire) != StageCode::KernelInitialized as u64 {
            return Err(ApStartupError::InvalidCpu);
        }
        state.store(StageCode::Online as u64, Ordering::Release);
        Ok(())
    }

    pub fn is_online(&self, cpu_index: usize) -> Result<bool, ApStartupError> {
        Ok(self.state(cpu_index)?.load(Ordering::Acquire) == StageCode::Online as u64)
    }

    pub fn stage(&self, cpu_index: usize) -> Result<ApStartupStage, ApStartupError> {
        Ok(decode_stage(self.state(cpu_index)?.load(Ordering::Acquire)))
    }

    /// Fast fallback used when HPET timing is unavailable. Do not use
    /// `spin_loop()` here: under virtualization PAUSE may deliberately yield,
    /// turning an iteration bound into minutes of wall-clock time.
    pub fn wait_online(&self, cpu_index: usize, poll_limit: usize) -> Result<bool, ApStartupError> {
        let state = self.state(cpu_index)?;
        for _ in 0..poll_limit {
            if state.load(Ordering::Acquire) == StageCode::Online as u64 {
                return Ok(true);
            }
            compiler_fence(Ordering::SeqCst);
        }
        Ok(false)
    }

    fn state(&self, cpu_index: usize) -> Result<&AtomicU64, ApStartupError> {
        self.states.get(cpu_index).ok_or(ApStartupError::InvalidCpu)
    }
}

impl Default for ApRendezvous {
    fn default() -> Self {
        Self::new()
    }
}

#[repr(u64)]
#[derive(Clone, Copy)]
enum StageCode {
    InitAsserted = 0,
    AwaitingTrampoline = 1,
    TrampolineEntered = 2,
    KernelInitialized = 3,
    Online = 4,
}

fn decode_stage(value: u64) -> ApStartupStage {
    match value {
        1 => ApStartupStage::AwaitingTrampoline,
        2 => ApStartupStage::TrampolineEntered,
        3 => ApStartupStage::KernelInitialized,
        4 => ApStartupStage::Online,
        _ => ApStartupStage::InitAsserted,
    }
}

/// Start every enabled non-BSP processor represented by the MADT.
///
/// CPUs are assigned dense kernel CPU indices beginning at 1; CPU 0 remains
/// the bootstrap processor. Each AP receives its own bootstrap stack and must
/// complete the rendezvous before the next AP is started, allowing the single
/// trampoline parameter block to be safely reused.
pub fn bring_up_application_processors(
    madt: &MadtInfo,
    bsp_apic_id: u8,
    physical_memory_offset: VirtAddr,
    startup_timer: Option<Hpet>,
) -> Result<BringupSummary, BringupError> {
    let trampoline = ApTrampoline::installed().ok_or(BringupError::TrampolineUnavailable)?;
    let mut summary = BringupSummary {
        candidates: 0,
        attempted: 0,
        online: 0,
        failed: 0,
        trampoline_physical_address: trampoline.physical_address(),
    };
    let mut cpu_index = 1_usize;

    serial_println!(
        "SMP application processor bring-up starting: bsp_lapic_id={}, recorded_processors={}, trampoline={:#x}, hpet_timing={}",
        bsp_apic_id,
        madt.processors().len(),
        trampoline.physical_address(),
        startup_timer.is_some()
    );

    for processor in madt.processors() {
        if !processor.enabled || processor.local_apic_id == u32::from(bsp_apic_id) {
            continue;
        }
        summary.candidates = summary.candidates.saturating_add(1);

        if cpu_index >= MAX_SMP_CPUS || processor.local_apic_id > u32::from(u8::MAX) {
            summary.failed = summary.failed.saturating_add(1);
            cpu_index = cpu_index.saturating_add(1);
            continue;
        }

        let apic_id = processor.local_apic_id as u8;
        serial_println!(
            "application processor startup preparing: cpu={}, lapic_id={}, vector={:#x}",
            cpu_index,
            apic_id,
            trampoline.startup_vector()
        );

        let stack = Box::leak(vec![0_u8; AP_BOOT_STACK_SIZE].into_boxed_slice());
        let stack_top = VirtAddr::from_ptr(stack.as_ptr().wrapping_add(stack.len()));
        let entry = VirtAddr::new(nullstar_ap_kernel_entry as *const () as usize as u64);

        if let Err(error) = trampoline.configure(
            physical_memory_offset,
            cpu_index as u32,
            u32::from(apic_id),
            stack_top,
            entry,
        ) {
            serial_println!(
                "application processor configuration failed: cpu={}, lapic_id={}, error={error:?}",
                cpu_index,
                apic_id
            );
            summary.failed = summary.failed.saturating_add(1);
            cpu_index = cpu_index.saturating_add(1);
            continue;
        }

        serial_println!(
            "application processor trampoline configured: cpu={}, lapic_id={}, stack_top={:#x}, entry={:#x}",
            cpu_index,
            apic_id,
            stack_top.as_u64(),
            entry.as_u64()
        );

        summary.attempted = summary.attempted.saturating_add(1);
        match start_ap(
            physical_memory_offset.as_u64(),
            apic_id,
            cpu_index,
            trampoline.startup_vector(),
            &AP_RENDEZVOUS,
            startup_timer,
        ) {
            Ok(_) => {
                serial_println!(
                    "application processor IPIs delivered: cpu={}, lapic_id={}, awaiting_rendezvous=true",
                    cpu_index,
                    apic_id
                );
                match wait_for_online(cpu_index, startup_timer) {
                    Ok(true) => {
                        summary.online = summary.online.saturating_add(1);
                    }
                    Ok(false) => {
                        serial_println!(
                            "application processor startup timed out: cpu={}, lapic_id={}, stage={:?}",
                            cpu_index,
                            apic_id,
                            AP_RENDEZVOUS
                                .stage(cpu_index)
                                .unwrap_or(ApStartupStage::InitAsserted)
                        );
                        summary.failed = summary.failed.saturating_add(1);
                    }
                    Err(error) => {
                        serial_println!(
                            "application processor rendezvous failed: cpu={}, lapic_id={}, error={error:?}",
                            cpu_index,
                            apic_id
                        );
                        summary.failed = summary.failed.saturating_add(1);
                    }
                }
            }
            Err(error) => {
                serial_println!(
                    "application processor startup failed: cpu={}, lapic_id={}, error={error:?}",
                    cpu_index,
                    apic_id
                );
                summary.failed = summary.failed.saturating_add(1);
            }
        }
        cpu_index = cpu_index.saturating_add(1);
    }

    serial_println!(
        "SMP application processors online: candidates={}, attempted={}, online={}, failed={}, trampoline={:#x}",
        summary.candidates,
        summary.attempted,
        summary.online,
        summary.failed,
        summary.trampoline_physical_address
    );
    Ok(summary)
}

/// Send the architectural INIT/SIPI sequence for one application processor.
///
/// The startup vector is the physical trampoline address divided by 4096 and
/// therefore must identify a page in the low 1 MiB. The caller is responsible
/// for ensuring that the trampoline is present and executable before invoking
/// this function.
pub fn start_ap(
    physical_memory_offset: u64,
    apic_id: u8,
    cpu_index: usize,
    startup_vector: u8,
    rendezvous: &ApRendezvous,
    startup_timer: Option<Hpet>,
) -> Result<ApStartup, ApStartupError> {
    if cpu_index >= MAX_SMP_CPUS {
        return Err(ApStartupError::InvalidCpu);
    }
    if !(STARTUP_VECTOR_MIN..=STARTUP_VECTOR_MAX).contains(&startup_vector) {
        return Err(ApStartupError::InvalidVector);
    }
    if rendezvous.is_online(cpu_index)? {
        return Err(ApStartupError::AlreadyOnline);
    }

    rendezvous.reset(cpu_index)?;
    serial_println!(
        "application processor INIT sending: cpu={}, lapic_id={}",
        cpu_index,
        apic_id
    );
    ipi::send_init(physical_memory_offset, apic_id)?;
    serial_println!(
        "application processor INIT delivered: cpu={}, lapic_id={}",
        cpu_index,
        apic_id
    );
    let mut startup = ApStartup {
        apic_id,
        cpu_index,
        startup_vector,
        stage: ApStartupStage::InitAsserted,
    };

    ipi::send_init_deassert(physical_memory_offset, apic_id)?;
    startup.stage = ApStartupStage::InitDeasserted;
    serial_println!(
        "application processor INIT deassert delivered: cpu={}, lapic_id={}",
        cpu_index,
        apic_id
    );

    startup_delay(
        startup_timer,
        INIT_TO_SIPI_DELAY_FEMTOSECONDS,
        FALLBACK_INIT_DELAY_ITERATIONS,
    )?;
    ipi::send_startup(physical_memory_offset, apic_id, startup_vector)?;
    startup.stage = ApStartupStage::StartupSent;
    serial_println!(
        "application processor SIPI delivered: cpu={}, lapic_id={}, attempt=1, vector={:#x}",
        cpu_index,
        apic_id,
        startup_vector
    );

    startup_delay(
        startup_timer,
        BETWEEN_SIPI_DELAY_FEMTOSECONDS,
        FALLBACK_SIPI_DELAY_ITERATIONS,
    )?;
    ipi::send_startup(physical_memory_offset, apic_id, startup_vector)?;
    startup.stage = ApStartupStage::AwaitingTrampoline;
    serial_println!(
        "application processor SIPI delivered: cpu={}, lapic_id={}, attempt=2, vector={:#x}",
        cpu_index,
        apic_id,
        startup_vector
    );

    Ok(startup)
}

#[unsafe(no_mangle)]
pub extern "C" fn nullstar_ap_kernel_entry(cpu_index: u32, apic_id: u32) -> ! {
    let cpu_index = cpu_index as usize;
    let _ = AP_RENDEZVOUS.publish_trampoline_entry(cpu_index);
    serial_println!(
        "application processor trampoline entered: cpu={}, lapic_id={}",
        cpu_index,
        apic_id
    );

    gdt::init_application_processor();
    interrupts::init_application_processor();

    let _ = AP_RENDEZVOUS.publish_kernel_initialized(cpu_index);
    let _ = AP_RENDEZVOUS.publish_online(cpu_index);
    serial_println!(
        "application processor online: cpu={}, lapic_id={}",
        cpu_index,
        apic_id
    );

    loop {
        x86_64::instructions::hlt();
    }
}

fn startup_delay(
    timer: Option<Hpet>,
    duration_femtoseconds: u64,
    fallback_iterations: usize,
) -> Result<(), ApStartupError> {
    if let Some(timer) = timer {
        timer.measure_duration(duration_femtoseconds)?;
        return Ok(());
    }

    for _ in 0..fallback_iterations {
        compiler_fence(Ordering::SeqCst);
    }
    Ok(())
}

fn wait_for_online(cpu_index: usize, timer: Option<Hpet>) -> Result<bool, ApStartupError> {
    if let Some(timer) = timer {
        for _ in 0..AP_ONLINE_TIMER_POLLS {
            if AP_RENDEZVOUS.is_online(cpu_index)? {
                return Ok(true);
            }
            timer.measure_duration(AP_ONLINE_POLL_DELAY_FEMTOSECONDS)?;
        }
        return Ok(false);
    }

    AP_RENDEZVOUS.wait_online(cpu_index, FALLBACK_ONLINE_POLL_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendezvous_requires_kernel_initialization_before_online() {
        let rendezvous = ApRendezvous::new();
        rendezvous.reset(1).unwrap();
        rendezvous.publish_trampoline_entry(1).unwrap();
        assert_eq!(
            rendezvous.publish_online(1),
            Err(ApStartupError::InvalidCpu)
        );
        rendezvous.publish_kernel_initialized(1).unwrap();
        rendezvous.publish_online(1).unwrap();
        assert!(rendezvous.is_online(1).unwrap());
    }

    #[test]
    fn rendezvous_wait_is_bounded() {
        let rendezvous = ApRendezvous::new();
        rendezvous.reset(2).unwrap();
        assert!(!rendezvous.wait_online(2, 8).unwrap());
        rendezvous.publish_trampoline_entry(2).unwrap();
        rendezvous.publish_kernel_initialized(2).unwrap();
        rendezvous.publish_online(2).unwrap();
        assert!(rendezvous.wait_online(2, 8).unwrap());
    }

    #[test]
    fn rejects_invalid_cpu_and_vector_before_ipi_hardware() {
        let rendezvous = ApRendezvous::new();
        assert_eq!(
            start_ap(0, 1, MAX_SMP_CPUS, 1, &rendezvous, None),
            Err(ApStartupError::InvalidCpu)
        );
        assert_eq!(
            start_ap(0, 1, 1, 0, &rendezvous, None),
            Err(ApStartupError::InvalidVector)
        );
    }
}
