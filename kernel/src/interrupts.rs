use core::{
    fmt,
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
};

use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::{
    VirtAddr,
    instructions::port::Port,
    registers::control::Cr2,
    structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode},
};

use crate::{
    acpi::AcpiInfo,
    apic::{self, ApicError, ApicInfo},
    gdt, hlt_loop, keyboard, serial_println,
};

pub const TIMER_VECTOR: u8 = 32;
pub const KEYBOARD_VECTOR: u8 = 33;
pub const SPURIOUS_VECTOR: u8 = 255;

const PIC_1_OFFSET: u8 = TIMER_VECTOR;
const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;
const PIC_1_DATA_PORT: u16 = 0x21;
const PIC_2_DATA_PORT: u16 = 0xa1;
const PIT_COMMAND_PORT: u16 = 0x43;
const PIT_CHANNEL_0_PORT: u16 = 0x40;
const PIT_INPUT_HZ: u32 = 1_193_182;

pub const TIMER_HZ: u64 = 100;

static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);
static ACTIVE_CONTROLLER: AtomicU8 = AtomicU8::new(ControllerKind::Pic as u8);

static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ControllerKind {
    Pic = 0,
    Apic = 1,
}

impl fmt::Display for ControllerKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pic => formatter.write_str("pic"),
            Self::Apic => formatter.write_str("apic"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ControllerInfo {
    pub kind: ControllerKind,
    pub apic: Option<ApicInfo>,
    pub fallback_reason: Option<ApicError>,
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt[TIMER_VECTOR].set_handler_fn(timer_interrupt_handler);
        idt[KEYBOARD_VECTOR].set_handler_fn(keyboard_interrupt_handler);
        idt[SPURIOUS_VECTOR].set_handler_fn(spurious_interrupt_handler);

        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        idt
    };
}

pub fn init(
    acpi_info: Option<&AcpiInfo>,
    physical_memory_offset: VirtAddr,
    physical_memory_end: u64,
) -> ControllerInfo {
    x86_64::instructions::interrupts::disable();
    IDT.load();

    let controller_info = match apic::init(
        acpi_info,
        physical_memory_offset,
        physical_memory_end,
        TIMER_VECTOR,
        KEYBOARD_VECTOR,
        SPURIOUS_VECTOR,
    ) {
        Ok(apic_info) => {
            mask_legacy_pics();
            ACTIVE_CONTROLLER.store(ControllerKind::Apic as u8, Ordering::Release);
            serial_println!(
                "interrupt controller initialized: apic; lapic_id={}, lapic={:#x}, ioapic_id={}, ioapic={:#x}, timer_irq=0->gsi{}, keyboard_irq=1->gsi{}",
                apic_info.local_apic_id,
                apic_info.local_apic_address,
                apic_info.io_apic_id,
                apic_info.io_apic_address,
                apic_info.timer_gsi,
                apic_info.keyboard_gsi
            );

            ControllerInfo {
                kind: ControllerKind::Apic,
                apic: Some(apic_info),
                fallback_reason: None,
            }
        }
        Err(error) => {
            initialize_legacy_pics();
            ACTIVE_CONTROLLER.store(ControllerKind::Pic as u8, Ordering::Release);
            serial_println!("interrupt controller initialized: pic; APIC unavailable: {error:?}");

            ControllerInfo {
                kind: ControllerKind::Pic,
                apic: None,
                fallback_reason: Some(error),
            }
        }
    };

    configure_pit();
    serial_println!(
        "interrupts enabled; controller={}, timer_frequency={} Hz",
        controller_info.kind,
        TIMER_HZ
    );
    x86_64::instructions::interrupts::enable();

    controller_info
}

pub fn timer_ticks() -> u64 {
    TIMER_TICKS.load(Ordering::Relaxed)
}

pub fn wait_for_timer_ticks(tick_count: u64) {
    let start = timer_ticks();
    while timer_ticks().wrapping_sub(start) < tick_count {
        x86_64::instructions::hlt();
    }
}

pub fn controller_kind() -> ControllerKind {
    match ACTIVE_CONTROLLER.load(Ordering::Acquire) {
        value if value == ControllerKind::Apic as u8 => ControllerKind::Apic,
        _ => ControllerKind::Pic,
    }
}

pub fn spurious_interrupt_count() -> u64 {
    apic::spurious_interrupt_count()
}

fn initialize_legacy_pics() {
    unsafe {
        let mut pics = PICS.lock();
        pics.initialize();

        // Unmask IRQ0 (PIT timer) and IRQ1 (PS/2 keyboard). Keeping every
        // other IRQ masked prevents entry into an IDT slot without a handler.
        pics.write_masks(0b1111_1100, 0b1111_1111);
    }
}

fn mask_legacy_pics() {
    let mut primary_data = Port::<u8>::new(PIC_1_DATA_PORT);
    let mut secondary_data = Port::<u8>::new(PIC_2_DATA_PORT);

    unsafe {
        primary_data.write(0xff);
        secondary_data.write(0xff);
    }
}

fn configure_pit() {
    let divisor = (PIT_INPUT_HZ / TIMER_HZ as u32) as u16;
    let mut command = Port::<u8>::new(PIT_COMMAND_PORT);
    let mut channel_0 = Port::<u8>::new(PIT_CHANNEL_0_PORT);

    unsafe {
        // Channel 0, low byte then high byte, square-wave mode, binary count.
        command.write(0x36);
        channel_0.write(divisor as u8);
        channel_0.write((divisor >> 8) as u8);
    }
}

fn notify_end_of_interrupt(vector: u8) {
    match controller_kind() {
        ControllerKind::Apic => apic::end_of_interrupt(),
        ControllerKind::Pic => unsafe {
            PICS.lock().notify_end_of_interrupt(vector);
        },
    }
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: BREAKPOINT\n{stack_frame:#?}");
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    serial_println!("EXCEPTION: PAGE FAULT");
    serial_println!("Address: {:?}", Cr2::read());
    serial_println!("Error code: {error_code:?}");
    serial_println!("{stack_frame:#?}");
    hlt_loop();
}

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    TIMER_TICKS.fetch_add(1, Ordering::Relaxed);
    notify_end_of_interrupt(TIMER_VECTOR);
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let mut keyboard_port = Port::<u8>::new(0x60);
    let scancode = unsafe { keyboard_port.read() };
    keyboard::push_scancode(scancode);
    notify_end_of_interrupt(KEYBOARD_VECTOR);
}

extern "x86-interrupt" fn spurious_interrupt_handler(_stack_frame: InterruptStackFrame) {
    apic::record_spurious_interrupt();
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    serial_println!("EXCEPTION: DOUBLE FAULT (error code {error_code:#x})\n{stack_frame:#?}");
    hlt_loop();
}
