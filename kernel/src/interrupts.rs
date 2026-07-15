use core::sync::atomic::{AtomicU64, Ordering};

use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::{gdt, hlt_loop, keyboard, serial_println};

const PIC_1_OFFSET: u8 = 32;
const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;
const PIT_COMMAND_PORT: u16 = 0x43;
const PIT_CHANNEL_0_PORT: u16 = 0x40;
const PIT_INPUT_HZ: u32 = 1_193_182;

pub const TIMER_HZ: u64 = 100;

static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);

static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[derive(Clone, Copy)]
#[repr(u8)]
enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
}

impl InterruptIndex {
    const fn as_u8(self) -> u8 {
        self as u8
    }
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);

        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        idt
    };
}

pub fn init() {
    IDT.load();

    unsafe {
        let mut pics = PICS.lock();
        pics.initialize();

        // Unmask IRQ0 (PIT timer) and IRQ1 (PS/2 keyboard). Keeping every
        // other IRQ masked prevents entry into an IDT slot without a handler.
        pics.write_masks(0b1111_1100, 0b1111_1111);
    }

    configure_pit();

    serial_println!("interrupts initialized; timer frequency = {TIMER_HZ} Hz");
    x86_64::instructions::interrupts::enable();
}

pub fn timer_ticks() -> u64 {
    TIMER_TICKS.load(Ordering::Relaxed)
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

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let mut keyboard_port = Port::<u8>::new(0x60);
    let scancode = unsafe { keyboard_port.read() };
    keyboard::push_scancode(scancode);

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    serial_println!("EXCEPTION: DOUBLE FAULT (error code {error_code:#x})\n{stack_frame:#?}");
    hlt_loop();
}
