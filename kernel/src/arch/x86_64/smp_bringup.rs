//! x86_64 application-processor startup and rendezvous state.
//!
//! This layer owns the architectural INIT/SIPI sequence and the bounded
//! rendezvous state that bridges AP entry to per-CPU kernel initialization.
//! The actual low-memory trampoline remains architecture/bootloader work; an
//! AP is never reported online until it explicitly completes the rendezvous.

use core::{
    hint::spin_loop,
    sync::atomic::{AtomicU64, Ordering},
};

use super::ipi::{self, IpiError};

const INIT_DEASSERT_DELAY: usize = 20_000;
const STARTUP_IPI_DELAY: usize = 20_000;
const STARTUP_VECTOR_MIN: u8 = 1;
const STARTUP_VECTOR_MAX: u8 = 0xff;
const MAX_SMP_CPUS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApStartupError {
    InvalidVector,
    InvalidCpu,
    AlreadyOnline,
    Ipi(IpiError),
}

impl From<IpiError> for ApStartupError {
    fn from(error: IpiError) -> Self {
        Self::Ipi(error)
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

/// Per-CPU state shared between the BSP startup coordinator and the AP entry
/// path. Atomic state allows the AP to publish readiness without taking a
/// scheduler lock before its per-CPU environment is initialized.
pub struct ApRendezvous {
    states: [AtomicU64; MAX_SMP_CPUS],
}

impl ApRendezvous {
    pub const fn new() -> Self {
        Self {
            states: [const { AtomicU64::new(ApStartupStage::InitAsserted as u64) }; MAX_SMP_CPUS],
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

    /// Bounded BSP-side wait used during bring-up. A timeout leaves the AP
    /// offline so scheduler admission cannot observe partially initialized CPU
    /// state.
    pub fn wait_online(
        &self,
        cpu_index: usize,
        spin_limit: usize,
    ) -> Result<bool, ApStartupError> {
        let state = self.state(cpu_index)?;
        for _ in 0..spin_limit {
            if state.load(Ordering::Acquire) == StageCode::Online as u64 {
                return Ok(true);
            }
            spin_loop();
        }
        Ok(false)
    }

    fn state(&self, cpu_index: usize) -> Result<&AtomicU64, ApStartupError> {
        self.states
            .get(cpu_index)
            .ok_or(ApStartupError::InvalidCpu)
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
    InitDeasserted = 1,
    StartupSent = 2,
    AwaitingTrampoline = 3,
    TrampolineEntered = 4,
    KernelInitialized = 5,
    Online = 6,
}

fn decode_stage(value: u64) -> ApStartupStage {
    match value {
        1 => ApStartupStage::InitDeasserted,
        2 => ApStartupStage::StartupSent,
        3 => ApStartupStage::AwaitingTrampoline,
        4 => ApStartupStage::TrampolineEntered,
        5 => ApStartupStage::KernelInitialized,
        6 => ApStartupStage::Online,
        _ => ApStartupStage::InitAsserted,
    }
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
    ipi::send_init(physical_memory_offset, apic_id)?;
    let mut startup = ApStartup {
        apic_id,
        cpu_index,
        startup_vector,
        stage: ApStartupStage::InitAsserted,
    };

    delay(INIT_DEASSERT_DELAY);
    ipi::send_init_deassert(physical_memory_offset, apic_id)?;
    startup.stage = ApStartupStage::InitDeasserted;

    delay(STARTUP_IPI_DELAY);
    ipi::send_startup(physical_memory_offset, apic_id, startup_vector)?;
    startup.stage = ApStartupStage::StartupSent;

    // A second SIPI is required by the architectural startup protocol for
    // compatibility with processors that did not observe the first one.
    delay(STARTUP_IPI_DELAY);
    ipi::send_startup(physical_memory_offset, apic_id, startup_vector)?;
    startup.stage = ApStartupStage::AwaitingTrampoline;

    Ok(startup)
}

fn delay(iterations: usize) {
    for _ in 0..iterations {
        spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendezvous_requires_kernel_initialization_before_online() {
        let rendezvous = ApRendezvous::new();
        rendezvous.reset(1).unwrap();
        rendezvous.publish_trampoline_entry(1).unwrap();
        assert_eq!(rendezvous.publish_online(1), Err(ApStartupError::InvalidCpu));
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
            start_ap(0, 1, MAX_SMP_CPUS, 1, &rendezvous),
            Err(ApStartupError::InvalidCpu)
        );
        assert_eq!(
            start_ap(0, 1, 1, 0, &rendezvous),
            Err(ApStartupError::InvalidVector)
        );
    }
}
