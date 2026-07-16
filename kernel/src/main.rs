#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::{alloc::Layout, panic::PanicInfo};
use x86_64::VirtAddr;

mod allocator;
mod console;
mod gdt;
mod interrupts;
mod keyboard;
mod memory;
mod serial;

static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let Some(physical_memory_offset) = boot_info.physical_memory_offset.into_option() else {
        serial_println!("no physical memory mapping was provided by the bootloader");
        hlt_loop();
    };
    let physical_memory_offset = VirtAddr::new(physical_memory_offset);

    let mut mapper = unsafe { memory::init(physical_memory_offset) };
    let mut frame_allocator =
        unsafe { memory::BootInfoFrameAllocator::init(&boot_info.memory_regions) };

    if let Err(error) = allocator::init_heap(&mut mapper, &mut frame_allocator) {
        serial_println!("failed to initialize the kernel heap: {error:?}");
        hlt_loop();
    }

    let Some(framebuffer) = boot_info.framebuffer.take() else {
        serial_println!("no framebuffer was provided by the bootloader");
        hlt_loop();
    };
    if let Err(error) = console::init(framebuffer) {
        serial_println!("failed to initialize the framebuffer console: {error:?}");
        hlt_loop();
    }

    gdt::init();
    interrupts::init();
    heap_allocation_self_test();

    println!("GalacticOS");
    println!("-------------");
    println!();
    println!("The x86-64 kernel has booted successfully.");
    println!("The framebuffer console is ready for formatted output.");
    println!();
    println!("Physical memory manager ready");
    println!("Kernel heap ready");
    println!("Heap allocation self-test passed");
    println!("Console initialized");
    println!("GDT loaded");
    println!("IDT loaded");
    println!("Interrupts enabled");
    println!();
    println!("Keyboard ready. Type below:");

    let usable_frames = frame_allocator.usable_frame_count();
    let usable_mebibytes = usable_frames.saturating_mul(memory::FRAME_SIZE) / (1024 * 1024);
    serial_println!(
        "physical memory manager initialized: offset={:#x}, usable_frames={}, usable_memory={} MiB",
        physical_memory_offset.as_u64(),
        usable_frames,
        usable_mebibytes
    );
    serial_println!(
        "kernel heap initialized: start={:#x}, size={} KiB, pages={}, allocated_frames={}, remaining_frames={}",
        allocator::HEAP_START,
        allocator::HEAP_SIZE / 1024,
        allocator::HEAP_PAGE_COUNT,
        frame_allocator.allocated_frame_count(),
        frame_allocator.remaining_frame_count()
    );
    serial_println!("framebuffer console initialized");
    serial_println!("kernel entered kernel_main");

    let mut reported_seconds = 0;
    loop {
        x86_64::instructions::hlt();

        while let Some(key) = keyboard::poll_key() {
            match key {
                pc_keyboard::DecodedKey::Unicode(character) => {
                    console::write_char(character);
                    serial_print!("{character}");
                }
                pc_keyboard::DecodedKey::RawKey(key_code) => {
                    serial_print!("<{key_code:?}>");
                }
            }
        }

        let elapsed_seconds = interrupts::timer_ticks() / interrupts::TIMER_HZ;
        if elapsed_seconds > reported_seconds {
            reported_seconds = elapsed_seconds;
            serial_println!("uptime: {elapsed_seconds}s");
        }
    }
}

fn heap_allocation_self_test() {
    const HEAP_VALUE: u64 = 0xC0FF_EE00_D15C_A11C;
    const VECTOR_LENGTH: u64 = 1024;
    const EXPECTED_SUM: u64 = (VECTOR_LENGTH - 1) * VECTOR_LENGTH / 2;

    let heap_value = Box::new(HEAP_VALUE);
    let mut values = Vec::new();
    for value in 0..VECTOR_LENGTH {
        values.push(value);
    }

    assert_eq!(*heap_value, HEAP_VALUE);
    assert_eq!(values.len(), VECTOR_LENGTH as usize);
    assert_eq!(values.iter().copied().sum::<u64>(), EXPECTED_SUM);

    drop(values);
    drop(heap_value);

    let reused_value = Box::new(0xA110_C8ED_u64);
    assert_eq!(*reused_value, 0xA110_C8ED);

    serial_println!(
        "heap allocation self-test passed: vector_len={}, vector_sum={}",
        VECTOR_LENGTH,
        EXPECTED_SUM
    );
}

fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

#[alloc_error_handler]
fn allocation_error(layout: Layout) -> ! {
    serial_println!("KERNEL ALLOCATION ERROR: {layout:?}");
    hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("KERNEL PANIC: {info}");
    hlt_loop();
}
