use core::{
    fmt,
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
};

use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::{
    PrivilegeLevel, VirtAddr,
    instructions::port::Port,
    structures::idt::{InterruptDescriptorTable, InterruptStackFrame},
};

use crate::{
    acpi::{HpetInfo, MadtInfo},
    apic, gdt, hlt_loop, keyboard, preemption,
    process::userspace,
    scheduler, serial_println,
};

const PIC_1_OFFSET: u8 = 32;
const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;
const PIT_COMMAND_PORT: u16 = 0x43;
const PIT_CHANNEL_0_PORT: u16 = 0x40;
const PIT_INPUT_HZ: u32 = 1_193_182;

const CONTROLLER_UNINITIALIZED: u8 = 0;
const CONTROLLER_PIC: u8 = 1;
const CONTROLLER_APIC: u8 = 2;

pub const TIMER_VECTOR: u8 = PIC_1_OFFSET;
pub const KEYBOARD_VECTOR: u8 = PIC_1_OFFSET + 1;
pub const SPURIOUS_VECTOR: u8 = 0xff;
pub const TIMER_HZ: u64 = 100;
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);
static SPURIOUS_INTERRUPTS: AtomicU64 = AtomicU64::new(0);
static ACTIVE_CONTROLLER: AtomicU8 = AtomicU8::new(CONTROLLER_UNINITIALIZED);

static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControllerKind {
    Pic,
    Apic,
}

impl fmt::Display for ControllerKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pic => formatter.write_str("pic"),
            Self::Apic => formatter.write_str("apic"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerSource {
    Pit,
    LocalApic,
}

impl fmt::Display for TimerSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pit => formatter.write_str("pit"),
            Self::LocalApic => formatter.write_str("lapic"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ControllerInfo {
    pub kind: ControllerKind,
    pub timer_source: TimerSource,
    pub timer_vector: u8,
    pub keyboard_vector: u8,
    pub timer_gsi: Option<u32>,
    pub keyboard_gsi: Option<u32>,
    pub local_apic_id: Option<u8>,
    pub local_apic_address: Option<u64>,
    pub local_apic_version: Option<u8>,
    pub io_apic_id: Option<u8>,
    pub io_apic_address: Option<u32>,
    pub io_apic_redirection_entries: Option<u32>,
    pub local_apic_timer_ticks_per_second: Option<u64>,
    pub local_apic_timer_initial_count: Option<u32>,
    pub local_apic_timer_divisor: Option<u32>,
    pub calibration_hpet_ticks: Option<u64>,
    pub hpet_period_femtoseconds: Option<u64>,
    pub hpet_frequency_hz: Option<u64>,
    pub hpet_counter_is_64_bit: Option<bool>,
    pub timer_fallback_reason: Option<&'static str>,
    pub fallback_reason: Option<&'static str>,
}

impl ControllerInfo {
    fn pic(fallback_reason: Option<&'static str>) -> Self {
        Self {
            kind: ControllerKind::Pic,
            timer_source: TimerSource::Pit,
            timer_vector: TIMER_VECTOR,
            keyboard_vector: KEYBOARD_VECTOR,
            timer_gsi: None,
            keyboard_gsi: None,
            local_apic_id: None,
            local_apic_address: None,
            local_apic_version: None,
            io_apic_id: None,
            io_apic_address: None,
            io_apic_redirection_entries: None,
            local_apic_timer_ticks_per_second: None,
            local_apic_timer_initial_count: None,
            local_apic_timer_divisor: None,
            calibration_hpet_ticks: None,
            hpet_period_femtoseconds: None,
            hpet_frequency_hz: None,
            hpet_counter_is_64_bit: None,
            timer_fallback_reason: None,
            fallback_reason,
        }
    }

    fn apic(info: apic::ControllerInfo) -> Self {
        let timer_source = match info.timer_source {
            apic::TimerSource::Pit => TimerSource::Pit,
            apic::TimerSource::LocalApic => TimerSource::LocalApic,
        };
        let timer_gsi =
            (timer_source == TimerSource::Pit).then_some(info.timer_route.global_system_interrupt);
        let local_timer = info.local_timer;

        Self {
            kind: ControllerKind::Apic,
            timer_source,
            timer_vector: TIMER_VECTOR,
            keyboard_vector: KEYBOARD_VECTOR,
            timer_gsi,
            keyboard_gsi: Some(info.keyboard_route.global_system_interrupt),
            local_apic_id: Some(info.local_apic_id),
            local_apic_address: Some(info.local_apic_address),
            local_apic_version: Some(info.local_apic_version),
            io_apic_id: Some(info.io_apic_id),
            io_apic_address: Some(info.io_apic_address),
            io_apic_redirection_entries: Some(info.io_apic_redirection_entries),
            local_apic_timer_ticks_per_second: local_timer.map(|timer| timer.ticks_per_second),
            local_apic_timer_initial_count: local_timer.map(|timer| timer.initial_count),
            local_apic_timer_divisor: local_timer.map(|timer| timer.divisor),
            calibration_hpet_ticks: local_timer.map(|timer| timer.calibration_hpet_ticks),
            hpet_period_femtoseconds: local_timer.map(|timer| timer.hpet_period_femtoseconds),
            hpet_frequency_hz: local_timer.map(|timer| timer.hpet_frequency_hz),
            hpet_counter_is_64_bit: local_timer.map(|timer| timer.hpet_counter_is_64_bit),
            timer_fallback_reason: info.timer_fallback_reason,
            fallback_reason: None,
        }
    }
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt[KEYBOARD_VECTOR].set_handler_fn(keyboard_interrupt_handler);
        idt[SPURIOUS_VECTOR].set_handler_fn(spurious_interrupt_handler);

        unsafe {
            idt.page_fault
                .set_handler_addr(userspace::page_fault_interrupt_entry_address());
            idt.general_protection_fault
                .set_handler_addr(userspace::general_protection_interrupt_entry_address());
            idt[TIMER_VECTOR].set_handler_addr(scheduler::timer_interrupt_entry_address());
            idt[userspace::SYSCALL_VECTOR]
                .set_handler_addr(userspace::syscall_interrupt_entry_address())
                .set_privilege_level(PrivilegeLevel::Ring3);
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        idt
    };
}

pub fn init(
    madt: Option<&MadtInfo>,
    hpet: Option<&HpetInfo>,
    physical_memory_offset: VirtAddr,
    physical_memory_end: u64,
) -> ControllerInfo {
    x86_64::instructions::interrupts::disable();
    IDT.load();
    initialize_pics(0xff, 0xff);
    configure_pit();

    let controller = match madt {
        Some(madt) => match apic::init(apic::InitConfig {
            madt,
            hpet_info: hpet,
            physical_memory_offset,
            physical_memory_end,
            timer_vector: TIMER_VECTOR,
            keyboard_vector: KEYBOARD_VECTOR,
            spurious_vector: SPURIOUS_VECTOR,
            timer_hz: TIMER_HZ,
        }) {
            Ok(apic_info) => {
                ACTIVE_CONTROLLER.store(CONTROLLER_APIC, Ordering::Release);
                ControllerInfo::apic(apic_info)
            }
            Err(error) => {
                enable_pic_timer_and_keyboard();
                ACTIVE_CONTROLLER.store(CONTROLLER_PIC, Ordering::Release);
                serial_println!("APIC initialization failed: {error}");
                ControllerInfo::pic(Some(error.description()))
            }
        },
        None => {
            enable_pic_timer_and_keyboard();
            ACTIVE_CONTROLLER.store(CONTROLLER_PIC, Ordering::Release);
            ControllerInfo::pic(Some("MADT is unavailable"))
        }
    };

    match controller.kind {
        ControllerKind::Apic => {
            serial_println!(
                "interrupt controller initialized: apic, lapic_id={}, lapic={:#x}, ioapic_id={}, ioapic={:#x}, timer={}, keyboard_gsi={}",
                controller.local_apic_id.unwrap_or(0),
                controller.local_apic_address.unwrap_or(0),
                controller.io_apic_id.unwrap_or(0),
                controller.io_apic_address.unwrap_or(0),
                controller.timer_source,
                controller.keyboard_gsi.unwrap_or(0)
            );
        }
        ControllerKind::Pic => {
            serial_println!(
                "interrupt controller initialized: pic, fallback_reason={}",
                controller.fallback_reason.unwrap_or("none")
            );
        }
    }

    match controller.timer_source {
        TimerSource::LocalApic => {
            serial_println!(
                "timer initialized: source=lapic, frequency={} Hz, lapic_ticks_per_second={}, initial_count={}, divisor={}, hpet_period_fs={}, hpet_frequency_hz={}, hpet_64_bit={}",
                TIMER_HZ,
                controller.local_apic_timer_ticks_per_second.unwrap_or(0),
                controller.local_apic_timer_initial_count.unwrap_or(0),
                controller.local_apic_timer_divisor.unwrap_or(0),
                controller.hpet_period_femtoseconds.unwrap_or(0),
                controller.hpet_frequency_hz.unwrap_or(0),
                controller.hpet_counter_is_64_bit.unwrap_or(false)
            );
        }
        TimerSource::Pit => {
            serial_println!(
                "timer initialized: source=pit, frequency={} Hz, gsi={}, fallback_reason={}",
                TIMER_HZ,
                controller.timer_gsi.unwrap_or(0),
                controller.timer_fallback_reason.unwrap_or("none")
            );
        }
    }

    x86_64::instructions::interrupts::enable();
    controller
}

pub fn timer_ticks() -> u64 {
    TIMER_TICKS.load(Ordering::Relaxed)
}

pub const fn monotonic_time_ns_from_ticks(ticks: u64) -> u64 {
    ticks.saturating_mul(NANOSECONDS_PER_SECOND / TIMER_HZ)
}

pub fn monotonic_time_ns() -> u64 {
    monotonic_time_ns_from_ticks(timer_ticks())
}

pub fn spurious_interrupts() -> u64 {
    SPURIOUS_INTERRUPTS.load(Ordering::Relaxed)
}

pub fn wait_for_timer_tick() {
    let starting_tick = timer_ticks();
    while timer_ticks() == starting_tick {
        x86_64::instructions::hlt();
    }
}

fn initialize_pics(master_mask: u8, slave_mask: u8) {
    unsafe {
        let mut pics = PICS.lock();
        pics.initialize();
        pics.write_masks(master_mask, slave_mask);
    }
}

fn enable_pic_timer_and_keyboard() {
    unsafe {
        PICS.lock().write_masks(0b1111_1100, 0b1111_1111);
    }
}

fn configure_pit() {
    let divisor = (PIT_INPUT_HZ / TIMER_HZ as u32) as u16;
    let mut command = Port::<u8>::new(PIT_COMMAND_PORT);
    let mut channel_0 = Port::<u8>::new(PIT_CHANNEL_0_PORT);

    unsafe {
        command.write(0x36);
        channel_0.write(divisor as u8);
        channel_0.write((divisor >> 8) as u8);
    }
}

fn end_of_interrupt(vector: u8) {
    match ACTIVE_CONTROLLER.load(Ordering::Acquire) {
        CONTROLLER_APIC => apic::end_of_interrupt(),
        CONTROLLER_PIC => unsafe {
            PICS.lock().notify_end_of_interrupt(vector);
        },
        _ => {}
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn galactic_timer_interrupt_dispatch(current_stack_pointer: usize) -> usize {
    let ticks = TIMER_TICKS
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1);
    end_of_interrupt(TIMER_VECTOR);
    if !preemption::is_disabled() {
        userspace::service_object_wait_deadlines(monotonic_time_ns_from_ticks(ticks));
    }
    scheduler::on_timer_interrupt(current_stack_pointer)
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: BREAKPOINT\n{stack_frame:#?}");
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let mut keyboard_port = Port::<u8>::new(0x60);
    let scancode = unsafe { keyboard_port.read() };
    keyboard::push_scancode(scancode);
    end_of_interrupt(KEYBOARD_VECTOR);
}

extern "x86-interrupt" fn spurious_interrupt_handler(_stack_frame: InterruptStackFrame) {
    SPURIOUS_INTERRUPTS.fetch_add(1, Ordering::Relaxed);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    serial_println!("EXCEPTION: DOUBLE FAULT (error code {error_code:#x})\n{stack_frame:#?}");
    hlt_loop();
}
