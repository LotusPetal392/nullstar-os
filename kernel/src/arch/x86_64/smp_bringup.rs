//! x86_64 application-processor startup sequencing.
//!
//! This layer owns the architectural INIT/SIPI sequence. It intentionally
//! stops at the point where the AP enters the low-memory trampoline: the
//! trampoline and its rendezvous state are a separate concern, so callers can
//! establish the AP's stack and page-table environment before declaring it
//! online.

use core::hint::spin_loop;

use super::ipi::{self, IpiError};

const INIT_DEASSERT_DELAY: usize = 20_000;
const STARTUP_IPI_DELAY: usize = 20_000;
const STARTUP_VECTOR_MIN: u8 = 1;
const STARTUP_VECTOR_MAX: u8 = 0xff;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApStartupError {
    InvalidVector,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApStartup {
    pub apic_id: u8,
    pub startup_vector: u8,
    pub stage: ApStartupStage,
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
    startup_vector: u8,
) -> Result<ApStartup, ApStartupError> {
    if !(STARTUP_VECTOR_MIN..=STARTUP_VECTOR_MAX).contains(&startup_vector) {
        return Err(ApStartupError::InvalidVector);
    }

    ipi::send_init(physical_memory_offset, apic_id)?;
    let mut startup = ApStartup {
        apic_id,
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
    fn rejects_non_startup_vectors_before_touching_ipi_hardware() {
        assert_eq!(start_ap(0, 1, 0), Err(ApStartupError::InvalidVector));
    }
}
