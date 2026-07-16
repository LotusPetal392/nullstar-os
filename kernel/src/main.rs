#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::{alloc::Layout, panic::PanicInfo};
use x86_64::VirtAddr;

mod acpi;
mod allocator;
mod apic;
mod console;
mod gdt;
mod hpet;
mod interrupts;
mod keyboard;
mod memory;
mod serial;
mod shell;

const BOOTLOADER_MINIMUM_PHYSICAL_MAPPING_END: u64 = 0x1_0000_0000;

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
    let physical_memory_end = boot_info
        .memory_regions
        .iter()
        .map(|region| region.end)
        .max()
        .unwrap_or(0)
        .max(BOOTLOADER_MINIMUM_PHYSICAL_MAPPING_END);
    let rsdp_address = boot_info.rsdp_addr.into_option();

    let mut mapper = unsafe { memory::init(physical_memory_offset) };
    let mut frame_allocator =
        unsafe { memory::BootInfoFrameAllocator::init(&boot_info.memory_regions) };

    if let Err(error) = allocator::init_heap(&mut mapper, &mut frame_allocator) {
        serial_println!("failed to initialize the kernel heap: {error:?}");
        hlt_loop();
    }

    let acpi_info = match rsdp_address {
        Some(rsdp_address) => {
            match acpi::init(rsdp_address, physical_memory_offset, physical_memory_end) {
                Ok(info) => {
                    serial_println!(
                        "ACPI initialized: rsdp={:#x}, revision={}, root={}@{:#x}, tables={}, valid={}, invalid={}, madt={}, fadt={}, hpet={}, mcfg={}",
                        info.rsdp_address,
                        info.revision,
                        info.root_table_kind,
                        info.root_table_address,
                        info.total_table_count,
                        info.valid_table_count,
                        info.invalid_table_count,
                        info.madt.is_some(),
                        info.fadt.is_some(),
                        info.hpet.is_some(),
                        info.mcfg.is_some()
                    );
                    Some(info)
                }
                Err(error) => {
                    serial_println!("ACPI initialization failed: {error:?}");
                    None
                }
            }
        }
        None => {
            serial_println!("ACPI unavailable: bootloader did not provide an RSDP");
            None
        }
    };

    let Some(framebuffer) = boot_info.framebuffer.take() else {
        serial_println!("no framebuffer was provided by the bootloader");
        hlt_loop();
    };
    if let Err(error) = console::init(framebuffer) {
        serial_println!("failed to initialize the framebuffer console: {error:?}");
        hlt_loop();
    }

    gdt::init();
    let interrupt_controller = interrupts::init(
        acpi_info.as_ref().and_then(|info| info.madt.as_ref()),
        acpi_info.as_ref().and_then(|info| info.hpet.as_ref()),
        physical_memory_offset,
        physical_memory_end,
    );
    interrupts::wait_for_timer_tick();
    serial_println!(
        "interrupt timer verified: controller={}, source={}, ticks={}",
        interrupt_controller.kind,
        interrupt_controller.timer_source,
        interrupts::timer_ticks()
    );
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
    println!("Interrupt controller: {}", interrupt_controller.kind);
    println!("Timer source: {}", interrupt_controller.timer_source);
    println!("Timer interrupts verified");
    if acpi_info.is_some() {
        println!("ACPI tables loaded");
    } else {
        println!("ACPI unavailable");
    }
    println!("Interactive shell initialized");

    let usable_frames = frame_allocator.usable_frame_count();
    let allocated_frames = frame_allocator.allocated_frame_count();
    let remaining_frames = frame_allocator.remaining_frame_count();
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
        allocated_frames,
        remaining_frames
    );
    serial_println!("framebuffer console initialized");
    serial_println!("interactive shell initialized");
    serial_println!("kernel entered kernel_main");

    let system_info = shell::SystemInfo::new(
        usable_frames,
        allocated_frames,
        remaining_frames,
        acpi_info,
        interrupt_controller,
    );
    let mut interactive_shell = shell::Shell::new(system_info);
    interactive_shell.start();

    let mut reported_seconds = 0;
    loop {
        x86_64::instructions::hlt();

        while let Some(key) = keyboard::poll_key() {
            match interactive_shell.handle_key(key) {
                shell::ShellAction::Continue => {}
                shell::ShellAction::Halt => {
                    serial_println!("halt requested by interactive shell");
                    x86_64::instructions::interrupts::disable();
                    hlt_loop();
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
