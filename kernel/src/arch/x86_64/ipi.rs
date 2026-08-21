//! Local APIC inter-processor interrupt primitives used by SMP bring-up.
//!
//! This module owns the xAPIC ICR programming needed to send fixed, INIT, and
//! startup IPIs. It deliberately does not choose the AP trampoline address or
//! manage scheduler state; those belong to the SMP bring-up coordinator.

use core::{arch::asm, ptr};

const APIC_BASE_MSR: u32 = 0x1b;
const APIC_BASE_ENABLE: u64 = 1 << 11;
const APIC_BASE_X2APIC: u64 = 1 << 10;
const APIC_BASE_ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;

const LOCAL_APIC_ICR_LOW: usize = 0x300;
const LOCAL_APIC_ICR_HIGH: usize = 0x310;

const ICR_DELIVERY_MODE_FIXED: u32 = 0;
const ICR_DELIVERY_MODE_INIT: u32 = 5 << 8;
const ICR_DELIVERY_MODE_STARTUP: u32 = 6 << 8;
const ICR_DELIVERY_STATUS: u32 = 1 << 12;
const ICR_LEVEL_ASSERT: u32 = 1 << 14;
const ICR_TRIGGER_LEVEL: u32 = 1 << 15;

const STARTUP_VECTOR_MIN: u8 = 1;
const STARTUP_VECTOR_MAX: u8 = 0xff;
const DELIVERY_WAIT_LIMIT: usize = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpiError {
    ApicUnavailable,
    X2ApicUnsupported,
    InvalidVector,
    DeliveryTimeout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpiKind {
    Fixed { vector: u8 },
    InitAssert,
    InitDeassert,
    Startup { vector: u8 },
}

/// Send an IPI through the already-initialized local APIC.
///
/// The caller must provide the physical-to-virtual mapping offset established
/// during boot. Startup vectors are physical page numbers in the low 1 MiB;
/// the bring-up coordinator owns the trampoline allocation and validation.
pub fn send(
    physical_memory_offset: u64,
    destination_apic_id: u8,
    kind: IpiKind,
) -> Result<(), IpiError> {
    let vector = match kind {
        IpiKind::Fixed { vector } => vector,
        IpiKind::InitAssert | IpiKind::InitDeassert => 0,
        IpiKind::Startup { vector } => {
            if !(STARTUP_VECTOR_MIN..=STARTUP_VECTOR_MAX).contains(&vector) {
                return Err(IpiError::InvalidVector);
            }
            vector
        }
    };

    let base = local_apic_base(physical_memory_offset)?;
    write_u32(
        base,
        LOCAL_APIC_ICR_HIGH,
        u32::from(destination_apic_id) << 24,
    );

    let low = match kind {
        IpiKind::Fixed { .. } => ICR_DELIVERY_MODE_FIXED | u32::from(vector),
        IpiKind::InitAssert => ICR_DELIVERY_MODE_INIT | ICR_LEVEL_ASSERT | ICR_TRIGGER_LEVEL,
        IpiKind::InitDeassert => ICR_DELIVERY_MODE_INIT | ICR_TRIGGER_LEVEL,
        IpiKind::Startup { .. } => ICR_DELIVERY_MODE_STARTUP | u32::from(vector),
    };

    write_u32(base, LOCAL_APIC_ICR_LOW, low);
    wait_for_delivery(base)
}

/// Perform the architectural INIT portion of an AP startup sequence.
pub fn send_init(physical_memory_offset: u64, destination_apic_id: u8) -> Result<(), IpiError> {
    send(
        physical_memory_offset,
        destination_apic_id,
        IpiKind::InitAssert,
    )
}

/// Release the INIT level after the required inter-IPI delay.
pub fn send_init_deassert(
    physical_memory_offset: u64,
    destination_apic_id: u8,
) -> Result<(), IpiError> {
    send(
        physical_memory_offset,
        destination_apic_id,
        IpiKind::InitDeassert,
    )
}

/// Send a startup IPI for a low-memory trampoline page.
pub fn send_startup(
    physical_memory_offset: u64,
    destination_apic_id: u8,
    startup_vector: u8,
) -> Result<(), IpiError> {
    send(
        physical_memory_offset,
        destination_apic_id,
        IpiKind::Startup {
            vector: startup_vector,
        },
    )
}

/// Send a fixed-vector IPI, normally used for reschedule or TLB-shootdown
/// notifications once APs are online.
pub fn send_fixed(
    physical_memory_offset: u64,
    destination_apic_id: u8,
    vector: u8,
) -> Result<(), IpiError> {
    send(
        physical_memory_offset,
        destination_apic_id,
        IpiKind::Fixed { vector },
    )
}

fn local_apic_base(physical_memory_offset: u64) -> Result<usize, IpiError> {
    let base = unsafe { read_msr(APIC_BASE_MSR) };
    if base & APIC_BASE_X2APIC != 0 {
        return Err(IpiError::X2ApicUnsupported);
    }
    if base & APIC_BASE_ENABLE == 0 {
        return Err(IpiError::ApicUnavailable);
    }
    let physical = base & APIC_BASE_ADDRESS_MASK;
    if physical == 0 {
        return Err(IpiError::ApicUnavailable);
    }
    let virtual_base = physical_memory_offset
        .checked_add(physical)
        .ok_or(IpiError::ApicUnavailable)?;
    usize::try_from(virtual_base).map_err(|_| IpiError::ApicUnavailable)
}

fn wait_for_delivery(base: usize) -> Result<(), IpiError> {
    // The volatile MMIO read itself prevents this loop from being optimized
    // away. Avoid `spin_loop()`/PAUSE here: virtual CPUs may interpret PAUSE as
    // a scheduler yield, turning a bounded hardware timeout into minutes of
    // host wall-clock time when an IPI cannot be delivered.
    for _ in 0..DELIVERY_WAIT_LIMIT {
        if read_u32(base, LOCAL_APIC_ICR_LOW) & ICR_DELIVERY_STATUS == 0 {
            return Ok(());
        }
    }
    Err(IpiError::DeliveryTimeout)
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
