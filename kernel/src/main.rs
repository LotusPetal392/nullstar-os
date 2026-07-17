#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::{alloc::Layout, panic::PanicInfo};
use x86_64::VirtAddr;

mod arch;
mod drivers;
mod memory;
mod scheduler;
mod shell;
mod storage;

pub(crate) use arch::x86_64::{acpi, apic, gdt, hpet, interrupts};
pub(crate) use drivers::{ahci, console, keyboard, pci, serial};
pub(crate) use memory::allocator;
pub(crate) use storage::{fat, partition};

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

    let pci_inventory = match acpi_info.as_ref().and_then(|info| info.mcfg.as_ref()) {
        Some(mcfg) => match pci::enumerate(mcfg, physical_memory_offset, physical_memory_end) {
            Ok(inventory) => {
                serial_println!(
                    "PCIe initialized: regions={}/{}, buses={}, functions={}, recorded={}, storage={}, network={}, display={}, bridges={}, truncated={}",
                    inventory.scanned_region_count,
                    inventory.declared_region_count,
                    inventory.scanned_bus_count,
                    inventory.total_function_count,
                    inventory.recorded_function_count(),
                    inventory.class_count(0x01),
                    inventory.class_count(0x02),
                    inventory.class_count(0x03),
                    inventory.bridge_count(),
                    inventory.is_truncated()
                );
                for function in inventory.functions() {
                    serial_println!(
                        "PCIe function: {} {:04x}:{:04x} class={:02x}:{:02x}:{:02x} revision={:02x} header={} irq_line={} irq_pin={} description={}",
                        function.location,
                        function.vendor_id,
                        function.device_id,
                        function.class_code,
                        function.subclass,
                        function.programming_interface,
                        function.revision_id,
                        function.header_kind,
                        function.interrupt_line,
                        function.interrupt_pin,
                        function.class_description()
                    );
                }
                Some(inventory)
            }
            Err(error) => {
                serial_println!("PCIe initialization failed: {error}");
                None
            }
        },
        None => {
            serial_println!("PCIe unavailable: ACPI did not provide an MCFG table");
            None
        }
    };

    let storage_info = match (
        pci_inventory.as_ref(),
        acpi_info.as_ref().and_then(|info| info.mcfg.as_ref()),
    ) {
        (Some(inventory), Some(mcfg)) => match ahci::init(
            inventory,
            mcfg,
            &mut frame_allocator,
            physical_memory_offset,
            physical_memory_end,
        ) {
            Ok(info) => {
                serial_println!(
                    "AHCI storage verified: controller={}, port={}, model=`{}`, serial=`{}`, firmware=`{}`, blocks={}, block_size={}, capacity_bytes={}, lba48={}, dma64={}, abar={:#x}, sector0_signature={:#06x}, sector0_checksum={:#010x}",
                    info.controller_location,
                    info.port,
                    info.model,
                    info.serial,
                    info.firmware,
                    info.logical_block_count,
                    info.logical_block_size,
                    info.capacity_bytes,
                    info.lba48,
                    info.supports_64_bit_dma,
                    info.abar,
                    info.sector_zero_signature,
                    info.sector_zero_checksum
                );
                Some(info)
            }
            Err(error) => {
                serial_println!("AHCI storage initialization failed: {error}");
                None
            }
        },
        _ => {
            serial_println!("AHCI storage unavailable: PCIe inventory or MCFG is missing");
            None
        }
    };

    let partition_inventory = if storage_info.is_some() {
        match partition::scan() {
            Ok(inventory) => {
                serial_println!(
                    "partition table initialized: kind={}, partitions={}, protective_mbr={}, header_crc_valid={}, entry_crc_valid={}, truncated={}",
                    inventory.table_kind,
                    inventory.partitions().len(),
                    inventory.protective_mbr,
                    inventory.header_crc_valid,
                    inventory.entry_array_crc_valid,
                    inventory.truncated
                );
                for partition in inventory.partitions() {
                    serial_println!(
                        "partition: index={}, kind={}, start_lba={}, end_lba={}, blocks={}, bootable={}, name=`{}`",
                        partition.index,
                        partition.kind,
                        partition.start_lba,
                        partition.end_lba_inclusive(),
                        partition.block_count,
                        partition.bootable,
                        partition.name
                    );
                }
                Some(inventory)
            }
            Err(error) => {
                serial_println!("partition discovery failed: {error}");
                None
            }
        }
    } else {
        serial_println!("partition discovery unavailable: AHCI storage is missing");
        None
    };

    let filesystem_info = match partition_inventory.as_ref() {
        Some(partitions) => match fat::init(partitions) {
            Ok(info) => {
                let root_entries = fat::list_directory("/").unwrap_or_default();
                let file_probe = root_entries
                    .iter()
                    .find(|entry| !entry.is_directory() && entry.size != 0)
                    .and_then(|entry| fat::read_file(&entry.name, 64).ok());
                let probe_bytes = file_probe
                    .as_ref()
                    .map(|data| data.bytes.len())
                    .unwrap_or(0);
                let probe_checksum = file_probe
                    .as_ref()
                    .map(|data| {
                        data.bytes
                            .iter()
                            .copied()
                            .fold(0x811c_9dc5_u32, |hash, byte| {
                                (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
                            })
                    })
                    .unwrap_or(0);
                serial_println!(
                    "FAT filesystem mounted: type={}, partition={}, start_lba={}, label=`{}`, sectors={}, cluster_bytes={}, root_entries={}, file_probe_bytes={}, file_probe_checksum={:#010x}",
                    info.fat_type,
                    info.partition_index,
                    info.partition_start_lba,
                    info.volume_label,
                    info.total_sectors,
                    info.bytes_per_cluster,
                    root_entries.len(),
                    probe_bytes,
                    probe_checksum
                );
                Some(info)
            }
            Err(error) => {
                serial_println!("FAT filesystem mount failed: {error}");
                None
            }
        },
        None => {
            serial_println!("FAT filesystem unavailable: no partition inventory");
            None
        }
    };

    let scheduler_initial = match scheduler::init() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            serial_println!(
                "failed to initialize the scheduler: {}",
                error.description()
            );
            hlt_loop();
        }
    };
    serial_println!(
        "scheduler initialized: tasks={}, quantum_ticks={}",
        scheduler_initial.task_count,
        scheduler_initial.quantum_ticks
    );

    let scheduler_verified = scheduler::wait_for_self_test();
    serial_println!(
        "scheduler verified: tasks={}, switches={}, preemptions={}, probe_a={}, probe_b={}",
        scheduler_verified.task_count,
        scheduler_verified.context_switches,
        scheduler_verified.preemptions,
        scheduler_verified.probe_a_heartbeats,
        scheduler_verified.probe_b_heartbeats
    );

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
    println!("Preemptive scheduler ready");
    if acpi_info.is_some() {
        println!("ACPI tables loaded");
    } else {
        println!("ACPI unavailable");
    }
    if pci_inventory.is_some() {
        println!("PCIe functions enumerated");
    } else {
        println!("PCIe enumeration unavailable");
    }
    if storage_info.is_some() {
        println!("AHCI block storage ready");
    } else {
        println!("AHCI block storage unavailable");
    }
    if partition_inventory.is_some() {
        println!("Partition table discovered");
    } else {
        println!("Partition table unavailable");
    }
    if filesystem_info.is_some() {
        println!("Read-only FAT filesystem mounted");
    } else {
        println!("FAT filesystem unavailable");
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
    serial_println!("preemptive scheduler initialized");
    serial_println!("interactive shell initialized");
    serial_println!("kernel entered kernel_main");

    let system_info = shell::SystemInfo::new(
        usable_frames,
        allocated_frames,
        remaining_frames,
        acpi_info,
        interrupt_controller,
        pci_inventory,
        storage_info,
        partition_inventory,
        filesystem_info,
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
