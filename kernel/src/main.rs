#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate alloc;

use alloc::{boxed::Box, vec, vec::Vec};
use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::{alloc::Layout, panic::PanicInfo};
use kernel::early_log;
use nswp_logging::{EventId, LogSeverity, PrivacyClass};
use x86_64::VirtAddr;

mod arch;
mod boot_mode;
mod drivers;
mod memory;
mod nullfs_volume_selection;
mod preemption;
mod process;
mod process_completion;
mod scheduler;
mod shell;
mod storage;
mod tmpfs_abi;
mod vfs;

pub(crate) use arch::x86_64::{acpi, apic, gdt, hpet, interrupts};
pub(crate) use drivers::{ahci, console, keyboard, pci, serial};
pub(crate) use memory::allocator;
pub(crate) use process::{elf, userspace};
pub(crate) use storage::{fat, partition};

const BOOTLOADER_MINIMUM_PHYSICAL_MAPPING_END: u64 = 0x1_0000_0000;
// The interactive kernel shell synchronously services userspace process-control
// requests on the bootstrap stack, so reserve explicit headroom beyond the
// bootloader's 80 KiB default.
const BOOTSTRAP_KERNEL_STACK_SIZE: u64 = 256 * 1024;

const KERNEL_ENTRY_EVENT_ID: EventId = match EventId::from_bytes([
    0xac, 0x95, 0xb7, 0x92, 0xf9, 0x3f, 0x47, 0x3c, 0xae, 0x52, 0x9b, 0xe6, 0xaa, 0x3f, 0xd8, 0x1e,
]) {
    Ok(id) => id,
    Err(_) => panic!("kernel-entry event ID must be canonical"),
};
const KERNEL_ACPI_INIT_FAILED_EVENT_ID: EventId = match EventId::from_bytes([
    0x7d, 0x47, 0x53, 0xa1, 0x6d, 0xc3, 0x47, 0x6a, 0x9b, 0x2e, 0xb9, 0x4e, 0x0c, 0x49, 0x6c, 0x34,
]) {
    Ok(id) => id,
    Err(_) => panic!("kernel-ACPI-init-failed event ID must be canonical"),
};
const KERNEL_ACPI_UNAVAILABLE_EVENT_ID: EventId = match EventId::from_bytes([
    0xaf, 0x08, 0x7d, 0x18, 0xbc, 0x37, 0x4f, 0xfd, 0x98, 0xbc, 0x3e, 0xaf, 0xb3, 0x1d, 0x20, 0xdc,
]) {
    Ok(id) => id,
    Err(_) => panic!("kernel-ACPI-unavailable event ID must be canonical"),
};
const KERNEL_INTERRUPTS_READY_EVENT_ID: EventId = match EventId::from_bytes([
    0xf0, 0x5c, 0x63, 0xdf, 0x9b, 0xd8, 0x47, 0x2d, 0xb9, 0x76, 0xce, 0x58, 0xb9, 0xe9, 0xd1, 0x80,
]) {
    Ok(id) => id,
    Err(_) => panic!("kernel-interrupts-ready event ID must be canonical"),
};
const KERNEL_STORAGE_READY_EVENT_ID: EventId = match EventId::from_bytes([
    0xd2, 0xed, 0xa2, 0x28, 0x00, 0x99, 0x46, 0x70, 0xae, 0xe2, 0x45, 0x64, 0x8d, 0x14, 0x18, 0x16,
]) {
    Ok(id) => id,
    Err(_) => panic!("kernel-storage-ready event ID must be canonical"),
};
const KERNEL_USERSPACE_INIT_STARTED_EVENT_ID: EventId = match EventId::from_bytes([
    0x9b, 0xa3, 0xf4, 0x2b, 0x1a, 0x9a, 0x48, 0xee, 0x99, 0x0e, 0xc4, 0xfe, 0x16, 0x05, 0xa9, 0xcb,
]) {
    Ok(id) => id,
    Err(_) => panic!("kernel-userspace-init-started event ID must be canonical"),
};
const KERNEL_USERSPACE_INIT_FAILED_EVENT_ID: EventId = match EventId::from_bytes([
    0x8f, 0x34, 0xc4, 0x5c, 0xd0, 0xd7, 0x4e, 0x83, 0xbf, 0x34, 0x50, 0xd7, 0x26, 0x8c, 0x26, 0x6e,
]) {
    Ok(id) => id,
    Err(_) => panic!("kernel-userspace-init-failed event ID must be canonical"),
};
const KERNEL_ALLOCATION_FAILURE_EVENT_ID: EventId = match EventId::from_bytes([
    0xb1, 0x88, 0xfb, 0xda, 0x1b, 0x94, 0x4c, 0x5d, 0x81, 0x84, 0x47, 0x1c, 0x27, 0xb0, 0xc2, 0x01,
]) {
    Ok(id) => id,
    Err(_) => panic!("kernel-allocation-failure event ID must be canonical"),
};
const KERNEL_PANIC_EVENT_ID: EventId = match EventId::from_bytes([
    0x65, 0xcf, 0xdd, 0x9e, 0x3a, 0x89, 0x47, 0x17, 0xbb, 0x5f, 0xdb, 0xa9, 0x74, 0x51, 0x51, 0x3b,
]) {
    Ok(id) => id,
    Err(_) => panic!("kernel-panic event ID must be canonical"),
};

static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config.kernel_stack_size = BOOTSTRAP_KERNEL_STACK_SIZE;
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let _ = early_log::initialize_kernel_early_log(early_log::BootIdentity::Unavailable);
    record_kernel_event(
        KERNEL_ENTRY_EVENT_ID,
        LogSeverity::Info,
        PrivacyClass::Public,
        early_log::EarlySource::KERNEL,
        "kernel.boot",
        "kernel entry reached",
    );

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
    let bootstrap_stack_bottom = boot_info.kernel_stack_bottom;
    let bootstrap_stack_len = boot_info.kernel_stack_len;
    let Some(bootstrap_stack_top) = bootstrap_stack_bottom.checked_add(bootstrap_stack_len) else {
        serial_println!("bootstrap kernel stack address overflowed");
        hlt_loop();
    };
    if bootstrap_stack_len < BOOTSTRAP_KERNEL_STACK_SIZE {
        serial_println!(
            "bootstrap kernel stack is too small: configured={}, provided={}",
            BOOTSTRAP_KERNEL_STACK_SIZE,
            bootstrap_stack_len
        );
        hlt_loop();
    }
    serial_println!(
        "bootstrap kernel stack initialized: bottom={:#x}, top={:#x}, bytes={}",
        bootstrap_stack_bottom,
        bootstrap_stack_top,
        bootstrap_stack_len
    );

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
                    record_kernel_event(
                        KERNEL_ACPI_INIT_FAILED_EVENT_ID,
                        LogSeverity::Warning,
                        PrivacyClass::Public,
                        early_log::EarlySource::KERNEL,
                        "kernel.arch",
                        "ACPI initialization failed",
                    );
                    serial_println!("ACPI initialization failed: {error:?}");
                    None
                }
            }
        }
        None => {
            record_kernel_event(
                KERNEL_ACPI_UNAVAILABLE_EVENT_ID,
                LogSeverity::Warning,
                PrivacyClass::Public,
                early_log::EarlySource::KERNEL,
                "kernel.arch",
                "ACPI RSDP unavailable",
            );
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
    record_kernel_event(
        KERNEL_INTERRUPTS_READY_EVENT_ID,
        LogSeverity::Info,
        PrivacyClass::Public,
        early_log::EarlySource {
            cpu_id: interrupt_controller.local_apic_id.map(u32::from),
            process_id: None,
            thread_id: None,
        },
        "kernel.arch",
        "interrupt controller and timer ready",
    );
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
                record_kernel_event(
                    KERNEL_STORAGE_READY_EVENT_ID,
                    LogSeverity::Info,
                    PrivacyClass::Public,
                    early_log::EarlySource::KERNEL,
                    "kernel.storage",
                    "AHCI storage ready",
                );
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

    if let Some(partitions) = partition_inventory.as_ref() {
        let (configured, writable_nullfs) = userspace::configure_block_device_endpoints(partitions);
        serial_println!(
            "block-device endpoints configured: partitions={configured}, writable_nullfs={writable_nullfs}"
        );
    }

    let mut filesystem_info = match partition_inventory.as_ref() {
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

    let vfs_info = if filesystem_info.is_some() {
        match vfs::mount_fat_root() {
            Ok(info) => {
                serial_println!(
                    "VFS initialized: root={}, filesystem={}, read_only={}, label=`{}`, volume_id={:#010x}, partition={}, start_lba={}",
                    info.mount_path,
                    info.filesystem,
                    info.read_only,
                    info.volume_label,
                    info.volume_id,
                    info.partition_index,
                    info.partition_start_lba
                );
                Some(info)
            }
            Err(error) => {
                serial_println!("VFS initialization failed: {error}");
                None
            }
        }
    } else {
        serial_println!("VFS unavailable: no FAT filesystem is mounted");
        None
    };

    let boot_mode = detect_boot_mode();
    serial_println!("boot mode selected: {}", boot_mode.description());
    if boot_mode.is_smoke_test() {
        heap_allocation_self_test();
    }

    if boot_mode.is_smoke_test() && vfs_info.is_some() {
        match persistent_fat_self_test() {
            Ok(PersistentFatPhase::Prepared) => {
                let writes = fat::write_info().expect("mounted FAT volume lost write accounting");
                let checksum = persistent_checksum(PERSISTENT_CHAIN_PATH).unwrap_or(0);
                serial_println!(
                    "persistent FAT write prepared: files=4, chain_bytes={}, chain_checksum={:#010x}, creates={}, truncates={}, writes={}, bytes={}, clusters=+{}/-{}, FAT_updates={}, directory_updates={}, flushes={}, mirrored=true",
                    PERSISTENT_CHAIN_BYTES,
                    checksum,
                    writes.creates,
                    writes.truncates,
                    writes.writes,
                    writes.bytes_written,
                    writes.clusters_allocated,
                    writes.clusters_freed,
                    writes.fat_entry_updates,
                    writes.directory_updates,
                    writes.flushes
                );
            }
            Ok(PersistentFatPhase::Verified) => {
                let checksum = persistent_checksum(PERSISTENT_CHAIN_PATH).unwrap_or(0);
                serial_println!(
                    "persistent FAT write verified: files=4, text_bytes={}, chain_bytes={}, chain_checksum={:#010x}, truncate_bytes={}, hole_bytes={}, mirrored=true, reboot_persistence=true",
                    PERSISTENT_TEXT_FIRST.len() + PERSISTENT_TEXT_SECOND.len(),
                    PERSISTENT_CHAIN_BYTES,
                    checksum,
                    PERSISTENT_TRUNCATED.len(),
                    PERSISTENT_HOLE_OFFSET + PERSISTENT_HOLE_TAIL.len()
                );
            }
            Err(error) => {
                serial_println!("persistent FAT write verification failed: {error}");
                hlt_loop();
            }
        }
        filesystem_info = fat::info();
    }

    let tmpfs_info = if vfs_info.is_some() {
        match vfs::mount_tmpfs() {
            Ok(info) => {
                serial_println!(
                    "tmpfs mounted: path={}, files={}/{}, bytes={}/{}, per_file_limit={}",
                    info.mount_path,
                    info.file_count,
                    info.maximum_files,
                    info.total_bytes,
                    info.maximum_total_bytes,
                    info.maximum_file_bytes
                );
                Some(info)
            }
            Err(error) => {
                serial_println!("tmpfs mount failed: {error}");
                None
            }
        }
    } else {
        serial_println!("tmpfs unavailable: VFS root is not initialized");
        None
    };

    let scheduler_mode = if boot_mode.is_smoke_test() {
        scheduler::InitMode::SmokeTest
    } else {
        scheduler::InitMode::Normal
    };
    let scheduler_initial = match scheduler::init(scheduler_mode) {
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
        "scheduler initialized: tasks={}, quantum_ticks={}, mode={}",
        scheduler_initial.task_count,
        scheduler_initial.quantum_ticks,
        boot_mode.description()
    );
    if let Ok(snapshot) = early_log::try_kernel_early_log_stats() {
        serial_println!(
            "kernel early log ready: capacity={}, retained={}, overwritten={}, dropped={}, rejected={}, busy_drops={}, boot_id={}",
            snapshot.stats.capacity,
            snapshot.stats.retained,
            snapshot.stats.overwritten,
            snapshot.stats.dropped,
            snapshot.stats.rejected,
            snapshot.busy_drops,
            if matches!(snapshot.stats.boot_identity, early_log::BootIdentity::Id(_)) {
                "available"
            } else {
                "unavailable"
            }
        );
    }

    if !boot_mode.is_smoke_test() {
        let userspace_runtime =
            userspace::Runtime::new(mapper, frame_allocator, physical_memory_offset);
        let runtime_memory = userspace_runtime.memory_stats();
        let usable_frames = runtime_memory.usable_frames;
        let allocated_frames = runtime_memory.allocated_frames;
        let remaining_frames = runtime_memory.remaining_frames;

        println!("NullStar OS");
        println!("-------------");
        println!();
        println!("Normal boot complete. Smoke tests were not run.");
        println!("Kernel services are ready. Starting userspace.");
        serial_println!(
            "normal boot ready: usable_frames={}, allocated_frames={}, remaining_frames={}",
            usable_frames,
            allocated_frames,
            remaining_frames
        );

        let system_info = shell::SystemInfo::new(
            acpi_info,
            interrupt_controller,
            pci_inventory,
            storage_info,
            partition_inventory,
            filesystem_info,
        );
        enter_userspace(system_info, userspace_runtime);
    }

    let elf_image = if vfs_info.is_some() {
        match elf::validate_first_in_directory("/") {
            Ok(image) => {
                serial_println!(
                    "ELF image validated: path=`{}`, type={}, machine=x86_64, entry={:#018x}, file_bytes={}, program_headers={}, load_segments={}, dynamic={}, tls={}, executable_stack={}",
                    image.path,
                    image.image_type,
                    image.entry_point,
                    image.file_size,
                    image.program_header_count,
                    image.load_segments().len(),
                    image.has_dynamic_segment,
                    image.has_tls_segment,
                    image.executable_stack_requested
                );
                for segment in image.load_segments() {
                    serial_println!(
                        "ELF LOAD: index={}, file={:#x}+{:#x}, virtual={:#018x}+{:#x}, align={:#x}, permissions={}",
                        segment.program_header_index,
                        segment.file_offset,
                        segment.file_size,
                        segment.virtual_address,
                        segment.memory_size,
                        segment.alignment,
                        segment.permissions()
                    );
                }
                Some(image)
            }
            Err(error) => {
                serial_println!("ELF validation failed: {error}");
                None
            }
        }
    } else {
        serial_println!("ELF validation unavailable: VFS is not initialized");
        None
    };

    let scheduler_verified = scheduler::wait_for_self_test();
    serial_println!(
        "scheduler verified: tasks={}, switches={}, preemptions={}, probe_a={}, probe_b={}",
        scheduler_verified.task_count,
        scheduler_verified.context_switches,
        scheduler_verified.preemptions,
        scheduler_verified.probe_a_heartbeats,
        scheduler_verified.probe_b_heartbeats
    );

    let process_frame_baseline = frame_allocator.allocated_frame_count();
    let process_probe_image = match elf::load("/process-probe") {
        Ok(image) => image,
        Err(error) => {
            serial_println!("userspace process-probe validation failed: {error}");
            hlt_loop();
        }
    };
    let fault_image = match elf::load("/fault-probe") {
        Ok(image) => image,
        Err(error) => {
            serial_println!("userspace fault-probe validation failed: {error}");
            hlt_loop();
        }
    };

    let process_probe_spawn = match userspace::spawn(
        "/process-probe",
        "user-process-probe",
        &process_probe_image,
        &mut mapper,
        &mut frame_allocator,
        physical_memory_offset,
    ) {
        Ok(info) => info,
        Err(error) => {
            serial_println!("failed to spawn /process-probe: {error}");
            hlt_loop();
        }
    };
    serial_println!(
        "userspace process spawned: pid={}, task={}, path={}, entry={:#018x}, page_table={:#x}, mapped_pages={}, owned_frames={}",
        process_probe_spawn.process_id,
        process_probe_spawn.task_id,
        process_probe_spawn.path,
        process_probe_spawn.entry_point,
        process_probe_spawn.page_table_address,
        process_probe_spawn.mapped_pages,
        process_probe_spawn.owned_frames
    );

    let fault_spawn = match userspace::spawn(
        "/fault-probe",
        "user-fault-probe",
        &fault_image,
        &mut mapper,
        &mut frame_allocator,
        physical_memory_offset,
    ) {
        Ok(info) => info,
        Err(error) => {
            serial_println!("failed to spawn /fault-probe: {error}");
            hlt_loop();
        }
    };
    serial_println!(
        "userspace process spawned: pid={}, task={}, path={}, entry={:#018x}, page_table={:#x}, mapped_pages={}, owned_frames={}",
        fault_spawn.process_id,
        fault_spawn.task_id,
        fault_spawn.path,
        fault_spawn.entry_point,
        fault_spawn.page_table_address,
        fault_spawn.mapped_pages,
        fault_spawn.owned_frames
    );

    let process_snapshot = userspace::wait_for_processes(
        &mut frame_allocator,
        &[process_probe_spawn.process_id, fault_spawn.process_id],
    );
    let process_frame_after = frame_allocator.allocated_frame_count();
    for result in &process_snapshot.results {
        serial_println!(
            "process result: pid={}, task={}, path={}, termination={}, schedules={}, runtime_ticks={}, syscalls={}, writes={}, yields={}, bytes_written={}, frames_reclaimed={}",
            result.process_id,
            result.task_id,
            result.path,
            result.termination,
            result.scheduled_count,
            result.runtime_ticks,
            result.syscall_count,
            result.write_count,
            result.yield_count,
            result.bytes_written,
            result.frames_reclaimed
        );
    }

    let process_probe_result = process_snapshot
        .results
        .iter()
        .find(|result| result.path == "/process-probe");
    let fault_result = process_snapshot
        .results
        .iter()
        .find(|result| result.path == "/fault-probe");
    let process_probe_valid = process_probe_result
        .map(|result| {
            result.exit_code() == Some(42)
                && result.scheduled_count >= 2
                && result.runtime_ticks > 0
        })
        .unwrap_or(false);
    let fault_valid = fault_result
        .and_then(|result| result.fault())
        .map(|fault| fault.vector == 14 && fault.address == 0x0000_0000_dead_0000)
        .unwrap_or(false);
    let frame_balance = process_frame_after == process_frame_baseline;
    let process_verified = process_snapshot.spawned == 2
        && process_snapshot.exited == 1
        && process_snapshot.faulted == 1
        && process_snapshot.reaped == 2
        && process_snapshot.waitable_zombies == 0
        && process_probe_valid
        && fault_valid
        && frame_balance;
    if !process_verified {
        serial_println!(
            "process isolation verification failed: spawned={}, active={}, zombies={}, exited={}, faulted={}, reaped={}, baseline_frames={}, final_frames={}, process_probe_valid={}, fault_valid={}",
            process_snapshot.spawned,
            process_snapshot.active,
            process_snapshot.waitable_zombies,
            process_snapshot.exited,
            process_snapshot.faulted,
            process_snapshot.reaped,
            process_frame_baseline,
            process_frame_after,
            process_probe_valid,
            fault_valid
        );
        hlt_loop();
    }
    let process_probe_result =
        process_probe_result.expect("validated process-probe result disappeared");
    serial_println!(
        "process isolation verified: spawned={}, exited={}, faulted={}, reaped={}, zombies=0, frames_reclaimed={}, frame_balance={}, probe_schedules={}, probe_runtime_ticks={}",
        process_snapshot.spawned,
        process_snapshot.exited,
        process_snapshot.faulted,
        process_snapshot.reaped,
        process_snapshot.frames_reclaimed,
        frame_balance,
        process_probe_result.scheduled_count,
        process_probe_result.runtime_ticks
    );

    let mut userspace_runtime =
        userspace::Runtime::new(mapper, frame_allocator, physical_memory_offset);
    let cat_result = match userspace_runtime.run("/cat", &["/hello.txt"]) {
        Ok(result) => result,
        Err(error) => {
            serial_println!("userspace file-I/O validation failed: {error}");
            hlt_loop();
        }
    };
    let file_io_verified = cat_result.exit_code() == Some(0)
        && cat_result.open_count == 1
        && cat_result.read_count >= 1
        && cat_result.close_count == 1
        && cat_result.bytes_read > 0;
    if !file_io_verified {
        serial_println!(
            "userspace file-I/O verification failed: exit={:?}, opens={}, reads={}, closes={}, bytes_read={}",
            cat_result.exit_code(),
            cat_result.open_count,
            cat_result.read_count,
            cat_result.close_count,
            cat_result.bytes_read
        );
        hlt_loop();
    }
    serial_println!(
        "userspace file I/O verified: pid={}, path={}, opens={}, reads={}, closes={}, bytes_read={}, exit_code=0",
        cat_result.process_id,
        cat_result.path,
        cat_result.open_count,
        cat_result.read_count,
        cat_result.close_count,
        cat_result.bytes_read
    );

    const USERSPACE_RUNTIME_PROBE_BYTES: u64 =
        b"userspace Rust runtime probe passed\n".len() as u64;
    let runtime_probe_result = match userspace_runtime.run("/runtime-probe", &["runtime-smoke"]) {
        Ok(result) => result,
        Err(error) => {
            serial_println!("userspace Rust runtime validation failed to launch: {error}");
            hlt_loop();
        }
    };
    let userspace_rust_runtime_verified = runtime_probe_result.exit_code() == Some(0)
        && runtime_probe_result.path == "/runtime-probe"
        && runtime_probe_result.syscall_count == 107
        && runtime_probe_result.write_count == 1
        && runtime_probe_result.bytes_written == USERSPACE_RUNTIME_PROBE_BYTES;
    if !userspace_rust_runtime_verified {
        serial_println!(
            "userspace Rust runtime verification failed: exit={:?}, path={}, syscalls={}, writes={}, bytes={}",
            runtime_probe_result.exit_code(),
            runtime_probe_result.path,
            runtime_probe_result.syscall_count,
            runtime_probe_result.write_count,
            runtime_probe_result.bytes_written
        );
        hlt_loop();
    }
    serial_println!(
        "userspace Rust runtime verified: pid={}, syscalls={}, writes={}, bytes={}, heap=4096, argv=2",
        runtime_probe_result.process_id,
        runtime_probe_result.syscall_count,
        runtime_probe_result.write_count,
        runtime_probe_result.bytes_written
    );

    let exec_before = userspace::snapshot();
    let exec_memory_before = userspace_runtime.memory_stats();
    let exec_source_spawn = match userspace_runtime.spawn_foreground("/exec-source", &[]) {
        Ok(info) => info,
        Err(error) => {
            serial_println!("transactional exec source spawn failed: {error}");
            hlt_loop();
        }
    };
    if let Err(error) =
        userspace_runtime.wait_until_process_path(exec_source_spawn.process_id, "/exec-target")
    {
        serial_println!("transactional exec target did not replace the source: {error}");
        hlt_loop();
    }
    let exec_source_terminal = userspace_runtime.terminal_snapshot();
    let exec_source_result = match userspace_runtime.wait(exec_source_spawn.process_id) {
        Ok(result) => result,
        Err(error) => {
            serial_println!("transactional exec source wait failed: {error}");
            hlt_loop();
        }
    };

    let exec_launcher_spawn =
        match userspace_runtime.spawn_foreground("/exec", &["/cat", "/hello.txt"]) {
            Ok(info) => info,
            Err(error) => {
                serial_println!("userspace exec launcher spawn failed: {error}");
                hlt_loop();
            }
        };
    if let Err(error) =
        userspace_runtime.wait_until_process_path(exec_launcher_spawn.process_id, "/cat")
    {
        serial_println!("userspace exec launcher did not replace itself with /cat: {error}");
        hlt_loop();
    }
    let exec_launcher_terminal = userspace_runtime.terminal_snapshot();
    let exec_launcher_result = match userspace_runtime.wait(exec_launcher_spawn.process_id) {
        Ok(result) => result,
        Err(error) => {
            serial_println!("userspace exec launcher wait failed: {error}");
            hlt_loop();
        }
    };
    let exec_memory_after = userspace_runtime.memory_stats();
    let exec_after = userspace::snapshot();

    let exec_preserved = match vfs::read_file("/tmp/exec-preserved.txt", 256) {
        Ok(data) => data,
        Err(error) => {
            serial_println!("transactional exec preserved-descriptor read failed: {error}");
            hlt_loop();
        }
    };
    let exec_closed = match vfs::read_file("/tmp/exec-closed.txt", 256) {
        Ok(data) => data,
        Err(error) => {
            serial_println!("transactional exec close-on-exec read failed: {error}");
            hlt_loop();
        }
    };
    let expected_exec_preserved =
        b"source-before-failed-exec\nsource-after-failed-exec\ntarget-after-exec\n";
    let expected_exec_closed = b"cloexec-before-failed-exec\ncloexec-after-failed-exec\n";
    let exec_delta = exec_after.execs.saturating_sub(exec_before.execs);
    let exec_failure_delta = exec_after
        .exec_failures
        .saturating_sub(exec_before.exec_failures);
    let userspace_exec_verified = exec_source_result.process_id == exec_source_spawn.process_id
        && exec_source_result.process_group_id == exec_source_spawn.process_group_id
        && exec_source_result.path == "/exec-target"
        && exec_source_result.exit_code() == Some(23)
        && exec_source_result.page_table_address != exec_source_spawn.page_table_address
        && exec_source_result.exec_count == 1
        && exec_source_result.exec_failure_count == 1
        && exec_source_result.close_on_exec_count == 1
        && exec_source_result.exec_frames_reclaimed == exec_source_spawn.owned_frames as u64
        && exec_source_result.open_count == 2
        && exec_source_result.file_write_count == 5
        && exec_source_terminal.foreground_process == Some(exec_source_spawn.process_id)
        && exec_launcher_result.process_id == exec_launcher_spawn.process_id
        && exec_launcher_result.process_group_id == exec_launcher_spawn.process_group_id
        && exec_launcher_result.path == "/cat"
        && exec_launcher_result.exit_code() == Some(0)
        && exec_launcher_result.page_table_address != exec_launcher_spawn.page_table_address
        && exec_launcher_result.exec_count == 1
        && exec_launcher_result.exec_failure_count == 0
        && exec_launcher_result.close_on_exec_count == 0
        && exec_launcher_result.exec_frames_reclaimed == exec_launcher_spawn.owned_frames as u64
        && exec_launcher_result.open_count == 1
        && exec_launcher_result.bytes_read > 0
        && exec_launcher_terminal.foreground_process == Some(exec_launcher_spawn.process_id)
        && exec_preserved.bytes.as_slice() == expected_exec_preserved
        && exec_closed.bytes.as_slice() == expected_exec_closed
        && exec_delta == 2
        && exec_failure_delta == 1
        && exec_memory_after.allocated_frames == exec_memory_before.allocated_frames
        && userspace_runtime
            .terminal_snapshot()
            .foreground_process
            .is_none();
    if !userspace_exec_verified {
        serial_println!(
            "userspace transactional exec verification failed: source={}/{}/{:?}, source_group={}/{}, source_tables={:#x}/{:#x}, source_exec={}/{}/{}/{}, source_io={}/{}, source_terminal={:?}, launcher={}/{}/{:?}, launcher_group={}/{}, launcher_tables={:#x}/{:#x}, launcher_exec={}/{}/{}, launcher_io={}/{}, launcher_terminal={:?}, files={}/{}, deltas={}/{}, frames={}/{}",
            exec_source_spawn.process_id,
            exec_source_result.process_id,
            exec_source_result.exit_code(),
            exec_source_spawn.process_group_id,
            exec_source_result.process_group_id,
            exec_source_spawn.page_table_address,
            exec_source_result.page_table_address,
            exec_source_result.exec_count,
            exec_source_result.exec_failure_count,
            exec_source_result.close_on_exec_count,
            exec_source_result.exec_frames_reclaimed,
            exec_source_result.open_count,
            exec_source_result.file_write_count,
            exec_source_terminal.foreground_process,
            exec_launcher_spawn.process_id,
            exec_launcher_result.process_id,
            exec_launcher_result.exit_code(),
            exec_launcher_spawn.process_group_id,
            exec_launcher_result.process_group_id,
            exec_launcher_spawn.page_table_address,
            exec_launcher_result.page_table_address,
            exec_launcher_result.exec_count,
            exec_launcher_result.exec_failure_count,
            exec_launcher_result.close_on_exec_count,
            exec_launcher_result.open_count,
            exec_launcher_result.bytes_read,
            exec_launcher_terminal.foreground_process,
            exec_preserved.bytes.len(),
            exec_closed.bytes.len(),
            exec_delta,
            exec_failure_delta,
            exec_memory_before.allocated_frames,
            exec_memory_after.allocated_frames
        );
        hlt_loop();
    }
    serial_println!(
        "userspace transactional exec verified: source_pid={}, launcher_pid={}, execs={}, failures={}, close_on_exec={}, source_frames={}, launcher_frames={}, argv=3, pid_preserved=true, group_preserved=true, terminal_preserved=true, descriptors_preserved=true",
        exec_source_result.process_id,
        exec_launcher_result.process_id,
        exec_delta,
        exec_failure_delta,
        exec_source_result.close_on_exec_count,
        exec_source_result.exec_frames_reclaimed,
        exec_launcher_result.exec_frames_reclaimed
    );

    let fork_before = userspace::snapshot();
    let fork_memory_before = userspace_runtime.memory_stats();
    let fork_spawn = match userspace_runtime.spawn_foreground("/fork-probe", &[]) {
        Ok(info) => info,
        Err(error) => {
            serial_println!("copy-on-write fork probe spawn failed: {error}");
            hlt_loop();
        }
    };
    let fork_parent_result = match userspace_runtime.wait(fork_spawn.process_id) {
        Ok(result) => result,
        Err(error) => {
            serial_println!("copy-on-write fork parent wait failed: {error}");
            hlt_loop();
        }
    };
    let fork_after = userspace::snapshot();
    let fork_memory_after = userspace_runtime.memory_stats();
    let fork_child_result = fork_after.results.iter().find(|result| {
        result.parent_process_id == Some(fork_spawn.process_id) && result.path == "/fork-target"
    });
    let fork_output = match vfs::read_file("/tmp/fork-shared.txt", 256) {
        Ok(data) => data,
        Err(error) => {
            serial_println!("copy-on-write fork output read failed: {error}");
            hlt_loop();
        }
    };
    let fork_delta = fork_after.forks.saturating_sub(fork_before.forks);
    let cow_fault_delta = fork_after.cow_faults.saturating_sub(fork_before.cow_faults);
    let cow_copy_delta = fork_after.cow_copies.saturating_sub(fork_before.cow_copies);
    let expected_fork_output = b"child-before-exec\ntarget-after-exec\nparent-after-wait\n";
    let userspace_fork_verified = fork_parent_result.exit_code() == Some(0)
        && fork_parent_result.process_id == fork_spawn.process_id
        && fork_parent_result.fork_count == 1
        && fork_child_result.is_some_and(|child| {
            child.exit_code() == Some(17)
                && child.process_group_id == fork_spawn.process_group_id
                && child.exec_count == 1
                && child.file_write_count == 2
        })
        && fork_delta == 1
        && cow_fault_delta >= 2
        && cow_copy_delta >= 2
        && fork_after.shared_frames == 0
        && fork_after.shared_references == 0
        && fork_output.bytes.as_slice() == expected_fork_output
        && fork_memory_after.allocated_frames == fork_memory_before.allocated_frames;
    if !userspace_fork_verified {
        serial_println!(
            "userspace copy-on-write fork verification failed: parent={}/{:?}, parent_forks={}, child={:?}, forks={}, cow_faults={}, cow_copies={}, shared={}/{}, output={}, frames={}/{}",
            fork_parent_result.process_id,
            fork_parent_result.exit_code(),
            fork_parent_result.fork_count,
            fork_child_result.map(|child| (
                child.process_id,
                child.exit_code(),
                child.process_group_id,
                child.exec_count,
                child.file_write_count
            )),
            fork_delta,
            cow_fault_delta,
            cow_copy_delta,
            fork_after.shared_frames,
            fork_after.shared_references,
            fork_output.bytes.len(),
            fork_memory_before.allocated_frames,
            fork_memory_after.allocated_frames
        );
        hlt_loop();
    }
    let fork_child_result = fork_child_result.expect("validated fork child result disappeared");
    serial_println!(
        "userspace copy-on-write fork verified: parent_pid={}, child_pid={}, group={}, forks={}, cow_faults={}, cow_copies={}, peak_shared_frames={}, peak_shared_references={}, child_exec=1, descriptors_preserved=true, frame_balance=true",
        fork_parent_result.process_id,
        fork_child_result.process_id,
        fork_parent_result.process_group_id,
        fork_delta,
        cow_fault_delta,
        cow_copy_delta,
        fork_after.peak_shared_frames,
        fork_after.peak_shared_references
    );

    let handled_signal_before = userspace::snapshot();
    let handled_signal_memory_before = userspace_runtime.memory_stats();
    let handled_signal_terminal_before = userspace_runtime.terminal_snapshot();
    let handled_signal_spawn =
        match userspace_runtime.spawn_foreground("/signal-handler-probe", &[]) {
            Ok(info) => info,
            Err(error) => {
                serial_println!("userspace handled-signal probe spawn failed: {error}");
                hlt_loop();
            }
        };
    if let Err(error) = userspace_runtime.wait_until_terminal_read(handled_signal_spawn.process_id)
    {
        serial_println!("userspace handled-signal probe did not block in terminal read: {error}");
        hlt_loop();
    }
    let masked_terminate_deliveries = match userspace_runtime.signal_process_group(
        handled_signal_spawn.process_group_id,
        userspace::SIGNAL_TERMINATE,
    ) {
        Ok(count) => count,
        Err(error) => {
            serial_println!("userspace masked SIGTERM delivery failed: {error}");
            hlt_loop();
        }
    };
    let pending_after_terminate = userspace::snapshot().pending_signals;
    let handled_interrupt_deliveries = match userspace_runtime.inject_terminal_interrupt() {
        Ok(count) => count,
        Err(error) => {
            serial_println!("userspace handled SIGINT injection failed: {error}");
            hlt_loop();
        }
    };
    let handled_signal_result = match userspace_runtime.wait(handled_signal_spawn.process_id) {
        Ok(result) => result,
        Err(error) => {
            serial_println!("userspace handled-signal probe wait failed: {error}");
            hlt_loop();
        }
    };
    let handled_signal_after = userspace::snapshot();
    let handled_signal_memory_after = userspace_runtime.memory_stats();
    let handled_signal_terminal_after = userspace_runtime.terminal_snapshot();
    let handled_signal_verified = handled_signal_result.exit_code() == Some(0)
        && handled_signal_result.signal_received_count == 2
        && handled_signal_result.signal_handler_count == 2
        && handled_signal_result.signal_return_count == 2
        && handled_signal_result.signal_interrupted_syscall_count == 1
        && handled_signal_result.signal_frame_failure_count == 0
        && handled_signal_result.pending_signal_peak >= 2
        && masked_terminate_deliveries == 1
        && handled_interrupt_deliveries == 1
        && pending_after_terminate >= 1
        && handled_signal_after
            .signal_handlers
            .saturating_sub(handled_signal_before.signal_handlers)
            == 2
        && handled_signal_after
            .signal_returns
            .saturating_sub(handled_signal_before.signal_returns)
            == 2
        && handled_signal_after
            .signal_interruptions
            .saturating_sub(handled_signal_before.signal_interruptions)
            == 1
        && handled_signal_after
            .signals_sent
            .saturating_sub(handled_signal_before.signals_sent)
            == 2
        && handled_signal_after.signal_frame_failures
            == handled_signal_before.signal_frame_failures
        && handled_signal_after.pending_signals == 0
        && handled_signal_terminal_after.interrupts
            == handled_signal_terminal_before.interrupts.saturating_add(1)
        && handled_signal_terminal_after.foreground_process.is_none()
        && handled_signal_memory_after.allocated_frames
            == handled_signal_memory_before.allocated_frames;
    if !handled_signal_verified {
        serial_println!(
            "userspace handled-signal verification failed: exit={:?}, received={}, handlers={}, returns={}, interrupted={}, frame_failures={}, pending_peak={}, deliveries={}/{}, pending_after_term={}, manager_handlers={}/{}, manager_returns={}/{}, manager_interruptions={}/{}, manager_pending={}, terminal_interrupts={}/{}, foreground={:?}, frames={}/{}",
            handled_signal_result.exit_code(),
            handled_signal_result.signal_received_count,
            handled_signal_result.signal_handler_count,
            handled_signal_result.signal_return_count,
            handled_signal_result.signal_interrupted_syscall_count,
            handled_signal_result.signal_frame_failure_count,
            handled_signal_result.pending_signal_peak,
            masked_terminate_deliveries,
            handled_interrupt_deliveries,
            pending_after_terminate,
            handled_signal_before.signal_handlers,
            handled_signal_after.signal_handlers,
            handled_signal_before.signal_returns,
            handled_signal_after.signal_returns,
            handled_signal_before.signal_interruptions,
            handled_signal_after.signal_interruptions,
            handled_signal_after.pending_signals,
            handled_signal_terminal_before.interrupts,
            handled_signal_terminal_after.interrupts,
            handled_signal_terminal_after.foreground_process,
            handled_signal_memory_before.allocated_frames,
            handled_signal_memory_after.allocated_frames
        );
        hlt_loop();
    }

    let signal_lifecycle_before = userspace::snapshot();
    let signal_lifecycle_memory_before = userspace_runtime.memory_stats();
    let signal_lifecycle_spawn =
        match userspace_runtime.spawn_foreground("/signal-lifecycle-probe", &[]) {
            Ok(info) => info,
            Err(error) => {
                serial_println!("userspace signal-lifecycle probe spawn failed: {error}");
                hlt_loop();
            }
        };
    let signal_lifecycle_result = match userspace_runtime.wait(signal_lifecycle_spawn.process_id) {
        Ok(result) => result,
        Err(error) => {
            serial_println!("userspace signal-lifecycle probe wait failed: {error}");
            hlt_loop();
        }
    };
    let signal_lifecycle_after = userspace::snapshot();
    let signal_lifecycle_memory_after = userspace_runtime.memory_stats();
    let signal_lifecycle_child = signal_lifecycle_after.results.iter().find(|result| {
        result.parent_process_id == Some(signal_lifecycle_spawn.process_id)
            && result.path == "/signal-lifecycle-target"
    });
    let signal_lifecycle_verified = signal_lifecycle_result.exit_code() == Some(0)
        && signal_lifecycle_result.fork_count == 1
        && signal_lifecycle_child.is_some_and(|child| {
            child.exit_code() == Some(19)
                && child.process_group_id == signal_lifecycle_spawn.process_group_id
                && child.exec_count == 1
                && child.signal_handler_count == 0
                && child.signal_return_count == 0
        })
        && signal_lifecycle_after
            .forks
            .saturating_sub(signal_lifecycle_before.forks)
            == 1
        && signal_lifecycle_after
            .execs
            .saturating_sub(signal_lifecycle_before.execs)
            == 1
        && signal_lifecycle_after.pending_signals == 0
        && signal_lifecycle_memory_after.allocated_frames
            == signal_lifecycle_memory_before.allocated_frames
        && userspace_runtime
            .terminal_snapshot()
            .foreground_process
            .is_none();
    if !signal_lifecycle_verified {
        serial_println!(
            "userspace signal-lifecycle verification failed: parent={}/{:?}, forks={}, child={:?}, fork_delta={}, exec_delta={}, pending={}, frames={}/{}",
            signal_lifecycle_result.process_id,
            signal_lifecycle_result.exit_code(),
            signal_lifecycle_result.fork_count,
            signal_lifecycle_child.map(|child| (
                child.process_id,
                child.exit_code(),
                child.process_group_id,
                child.exec_count,
                child.signal_handler_count,
                child.signal_return_count
            )),
            signal_lifecycle_after
                .forks
                .saturating_sub(signal_lifecycle_before.forks),
            signal_lifecycle_after
                .execs
                .saturating_sub(signal_lifecycle_before.execs),
            signal_lifecycle_after.pending_signals,
            signal_lifecycle_memory_before.allocated_frames,
            signal_lifecycle_memory_after.allocated_frames
        );
        hlt_loop();
    }
    let signal_lifecycle_child =
        signal_lifecycle_child.expect("validated signal lifecycle child disappeared");
    let userspace_handled_signals_verified = handled_signal_verified && signal_lifecycle_verified;
    serial_println!(
        "userspace handled signals verified: handler_pid={}, lifecycle_parent={}, lifecycle_child={}, handlers=2, returns=2, interrupted_syscalls=1, pending_peak={}, fork_inherited=true, exec_reset=true, mask_preserved=true, frame_balance=true",
        handled_signal_result.process_id,
        signal_lifecycle_result.process_id,
        signal_lifecycle_child.process_id,
        handled_signal_result.pending_signal_peak
    );

    let environment_before = userspace::snapshot();
    let environment_memory_before = userspace_runtime.memory_stats();
    let environment_spawn = match userspace_runtime.spawn_foreground_with_environment(
        "/environment-probe",
        &[],
        &["BASE=seed", "REMOVE=gone"],
    ) {
        Ok(info) => info,
        Err(error) => {
            serial_println!("userspace environment probe spawn failed: {error}");
            hlt_loop();
        }
    };
    let environment_parent = match userspace_runtime.wait(environment_spawn.process_id) {
        Ok(result) => result,
        Err(error) => {
            serial_println!("userspace environment probe wait failed: {error}");
            hlt_loop();
        }
    };
    let environment_after = userspace::snapshot();
    let environment_memory_after = userspace_runtime.memory_stats();
    let environment_child = environment_after.results.iter().find(|result| {
        result.parent_process_id == Some(environment_spawn.process_id)
            && result.path == "/environment-target"
    });
    let environment_change_delta = environment_after
        .environment_changes
        .saturating_sub(environment_before.environment_changes);
    let environment_fork_delta = environment_after
        .forks
        .saturating_sub(environment_before.forks);
    let environment_exec_delta = environment_after
        .execs
        .saturating_sub(environment_before.execs);
    let environment_wait_delta = environment_after
        .child_waits
        .saturating_sub(environment_before.child_waits);
    let userspace_environments_verified = environment_parent.process_id
        == environment_spawn.process_id
        && environment_parent.path == "/environment-target"
        && environment_parent.exit_code() == Some(32)
        && environment_parent.fork_count == 1
        && environment_parent.child_wait_count == 1
        && environment_parent.exec_count == 1
        && environment_parent.environment_count == 2
        && environment_parent.environment_change_count == 3
        && environment_child.is_some_and(|child| {
            child.exit_code() == Some(31)
                && child.process_group_id == environment_spawn.process_group_id
                && child.exec_count == 1
                && child.environment_count == 2
                && child.environment_change_count == 0
        })
        && environment_change_delta == 3
        && environment_fork_delta == 1
        && environment_exec_delta == 2
        && environment_wait_delta == 1
        && environment_after.active == environment_before.active
        && environment_memory_after.allocated_frames == environment_memory_before.allocated_frames
        && userspace_runtime
            .terminal_snapshot()
            .foreground_process
            .is_none();
    if !userspace_environments_verified {
        serial_println!(
            "userspace environment verification failed: parent={}/{}/{:?}, parent_fork={}, parent_wait={}, parent_exec={}, parent_env={}/{}, child={:?}, deltas={}/{}/{}/{}, active={}/{}, frames={}/{}",
            environment_parent.process_id,
            environment_parent.path,
            environment_parent.exit_code(),
            environment_parent.fork_count,
            environment_parent.child_wait_count,
            environment_parent.exec_count,
            environment_parent.environment_count,
            environment_parent.environment_change_count,
            environment_child.map(|child| (
                child.process_id,
                child.exit_code(),
                child.process_group_id,
                child.exec_count,
                child.environment_count,
                child.environment_change_count
            )),
            environment_change_delta,
            environment_fork_delta,
            environment_exec_delta,
            environment_wait_delta,
            environment_before.active,
            environment_after.active,
            environment_memory_before.allocated_frames,
            environment_memory_after.allocated_frames
        );
        hlt_loop();
    }
    let environment_child =
        environment_child.expect("validated environment lifecycle child disappeared");
    serial_println!(
        "userspace environments verified: parent_pid={}, child_pid={}, changes={}, variables=2, envp_initial=true, fork_inherited=true, exec_preserved=true, frame_balance=true",
        environment_parent.process_id,
        environment_child.process_id,
        environment_change_delta
    );

    const TERMINAL_TEST_LINE: &str = "hello from canonical stdin";
    let terminal_spawn = match userspace_runtime.spawn_foreground("/readline", &[]) {
        Ok(info) => info,
        Err(error) => {
            serial_println!("userspace terminal validation spawn failed: {error}");
            hlt_loop();
        }
    };
    if let Err(error) = userspace_runtime.wait_until_blocked(terminal_spawn.process_id) {
        serial_println!("userspace terminal did not block: {error}");
        hlt_loop();
    }
    let blocked_scheduler = scheduler::snapshot();
    let blocked_terminal = userspace_runtime.terminal_snapshot();
    if let Err(error) = userspace_runtime.inject_terminal_line(TERMINAL_TEST_LINE) {
        serial_println!("userspace terminal injection failed: {error}");
        hlt_loop();
    }
    let terminal_result = match userspace_runtime.wait(terminal_spawn.process_id) {
        Ok(result) => result,
        Err(error) => {
            serial_println!("userspace terminal wait failed: {error}");
            hlt_loop();
        }
    };
    let terminal_snapshot = userspace_runtime.terminal_snapshot();
    let expected_terminal_bytes = TERMINAL_TEST_LINE.len().saturating_add(1) as u64;
    let terminal_verified = terminal_result.exit_code() == Some(0)
        && terminal_result.terminal_read_count == 1
        && terminal_result.terminal_bytes_read == expected_terminal_bytes
        && terminal_result.blocked_read_count >= 1
        && blocked_scheduler.blocked_task_count >= 1
        && terminal_snapshot.wakeups > blocked_terminal.wakeups
        && terminal_snapshot.foreground_process.is_none();
    if !terminal_verified {
        serial_println!(
            "userspace terminal verification failed: exit={:?}, reads={}, bytes={}, blocked_reads={}, scheduler_blocked={}, wakeups_before={}, wakeups_after={}, foreground={:?}",
            terminal_result.exit_code(),
            terminal_result.terminal_read_count,
            terminal_result.terminal_bytes_read,
            terminal_result.blocked_read_count,
            blocked_scheduler.blocked_task_count,
            blocked_terminal.wakeups,
            terminal_snapshot.wakeups,
            terminal_snapshot.foreground_process
        );
        hlt_loop();
    }
    serial_println!(
        "userspace terminal verified: pid={}, blocked_reads={}, wakeups={}, bytes_read={}, exit_code=0",
        terminal_result.process_id,
        terminal_result.blocked_read_count,
        terminal_snapshot.wakeups,
        terminal_result.terminal_bytes_read
    );

    const PIPE_TEST_BYTES: u64 = 43;
    let pipe_before = userspace_runtime.pipe_snapshot();
    let pipeline_result =
        match userspace_runtime.pipeline("/pipe-producer", &[], "/pipe-consumer", &[]) {
            Ok(result) => result,
            Err(error) => {
                serial_println!("userspace pipe validation failed: {error}");
                hlt_loop();
            }
        };
    let pipe_after = userspace_runtime.pipe_snapshot();
    let pipe_verified = pipeline_result.producer.exit_code() == Some(0)
        && pipeline_result.consumer.exit_code() == Some(0)
        && pipeline_result.producer.pipe_write_count >= 1
        && pipeline_result.producer.pipe_bytes_written == PIPE_TEST_BYTES
        && pipeline_result.consumer.pipe_read_count >= 2
        && pipeline_result.consumer.pipe_bytes_read == PIPE_TEST_BYTES
        && pipeline_result.consumer.blocked_pipe_read_count >= 1
        && pipe_after.total_reader_wakeups > pipe_before.total_reader_wakeups
        && pipe_after.active_pipes == 0;
    if !pipe_verified {
        serial_println!(
            "userspace pipe verification failed: producer_exit={:?}, consumer_exit={:?}, writes={}, written={}, reads={}, read={}, blocked_reads={}, wakeups_before={}, wakeups_after={}, active={}",
            pipeline_result.producer.exit_code(),
            pipeline_result.consumer.exit_code(),
            pipeline_result.producer.pipe_write_count,
            pipeline_result.producer.pipe_bytes_written,
            pipeline_result.consumer.pipe_read_count,
            pipeline_result.consumer.pipe_bytes_read,
            pipeline_result.consumer.blocked_pipe_read_count,
            pipe_before.total_reader_wakeups,
            pipe_after.total_reader_wakeups,
            pipe_after.active_pipes
        );
        hlt_loop();
    }
    serial_println!(
        "userspace pipe verified: pipe={}, bytes={}, producer_writes={}, consumer_reads={}, blocked_reads={}, reader_wakeups={}, active_pipes=0",
        pipeline_result.pipe_id,
        pipeline_result.consumer.pipe_bytes_read,
        pipeline_result.producer.pipe_write_count,
        pipeline_result.consumer.pipe_read_count,
        pipeline_result.consumer.blocked_pipe_read_count,
        pipe_after.total_reader_wakeups
    );

    let userspace_shell = match userspace_runtime.spawn_foreground("/ush", &[]) {
        Ok(info) => info,
        Err(error) => {
            serial_println!("userspace shell validation spawn failed: {error}");
            hlt_loop();
        }
    };
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace shell did not reach its prompt: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.inject_terminal_line("cat /hello.txt") {
        serial_println!("userspace shell command injection failed: {error}");
        hlt_loop();
    }
    let shell_child =
        match userspace_runtime.wait_for_child_path(userspace_shell.process_id, "/cat") {
            Ok(result) => result,
            Err(error) => {
                serial_println!("userspace shell child wait failed: {error}");
                hlt_loop();
            }
        };
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace shell did not regain its prompt: {error}");
        hlt_loop();
    }

    if let Err(error) = userspace_runtime.inject_terminal_line("SHELL_PATH=/hello.txt") {
        serial_println!("userspace shell variable assignment injection failed: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!(
            "userspace shell variable assignment did not return to the prompt: {error}"
        );
        hlt_loop();
    }
    if let Err(error) =
        userspace_runtime.inject_terminal_line("SHELL_OUTPUT=/tmp/shell-variable.txt")
    {
        serial_println!("userspace shell output variable assignment failed: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace shell output assignment did not return to the prompt: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.inject_terminal_line("cat ${SHELL_PATH} > $SHELL_OUTPUT")
    {
        serial_println!("userspace shell variable expansion injection failed: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace shell variable expansion did not return to the prompt: {error}");
        hlt_loop();
    }
    let shell_variable_output = match vfs::read_file("/tmp/shell-variable.txt", 256) {
        Ok(data) => data,
        Err(error) => {
            serial_println!("userspace shell variable output read failed: {error}");
            hlt_loop();
        }
    };
    let shell_variable_expansion_verified = shell_variable_output.bytes.as_slice()
        == b"Hello from a NullStar OS userspace file descriptor.\n";
    if !shell_variable_expansion_verified {
        serial_println!(
            "userspace shell variable expansion verification failed: output_bytes={}",
            shell_variable_output.bytes.len()
        );
        hlt_loop();
    }

    if let Err(error) = userspace_runtime.inject_terminal_line("export SHELL_VALUE=expanded") {
        serial_println!("userspace shell export injection failed: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace shell export did not return to the prompt: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.inject_terminal_line("environment-target shell") {
        serial_println!("userspace shell environment child injection failed: {error}");
        hlt_loop();
    }
    let shell_environment_child = match userspace_runtime
        .wait_for_child_path(userspace_shell.process_id, "/environment-target")
    {
        Ok(result) => result,
        Err(error) => {
            serial_println!("userspace shell environment child wait failed: {error}");
            hlt_loop();
        }
    };
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!(
            "userspace shell did not regain its prompt after environment child: {error}"
        );
        hlt_loop();
    }
    let shell_environment_child_verified = shell_environment_child.parent_process_id
        == Some(userspace_shell.process_id)
        && shell_environment_child.path == "/environment-target"
        && shell_environment_child.exit_code() == Some(0)
        && shell_environment_child.environment_count == 1
        && shell_environment_child.environment_change_count == 0;
    if !shell_environment_child_verified {
        serial_println!(
            "userspace shell environment child verification failed: parent={:?}/{}, path={}, exit={:?}, environment={}/{},",
            shell_environment_child.parent_process_id,
            userspace_shell.process_id,
            shell_environment_child.path,
            shell_environment_child.exit_code(),
            shell_environment_child.environment_count,
            shell_environment_child.environment_change_count
        );
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.inject_terminal_line("unset SHELL_VALUE") {
        serial_println!("userspace shell unset injection failed: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace shell unset did not return to the prompt: {error}");
        hlt_loop();
    }

    if let Err(error) =
        userspace_runtime.inject_terminal_line("exec runtime-probe manual-argv > /tmp/exec.txt")
    {
        serial_println!("userspace shell exec/redirection injection failed: {error}");
        hlt_loop();
    }
    let shell_exec_child =
        match userspace_runtime.wait_for_child_path(userspace_shell.process_id, "/runtime-probe") {
            Ok(result) => result,
            Err(error) => {
                serial_println!("userspace shell exec child wait failed: {error}");
                hlt_loop();
            }
        };
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace shell did not regain its prompt after exec: {error}");
        hlt_loop();
    }
    let shell_exec_output = match vfs::read_file("/tmp/exec.txt", 256) {
        Ok(data) => data,
        Err(error) => {
            serial_println!("userspace shell exec output read failed: {error}");
            hlt_loop();
        }
    };
    let userspace_shell_exec_verified = shell_exec_child.parent_process_id
        == Some(userspace_shell.process_id)
        && shell_exec_child.path == "/runtime-probe"
        && shell_exec_child.exit_code() == Some(0)
        && shell_exec_child.exec_count == 2
        && shell_exec_child.exec_failure_count == 0
        && shell_exec_child.close_on_exec_count == 0
        && shell_exec_child.file_write_count == 1
        && shell_exec_output.bytes.as_slice() == b"userspace Rust runtime probe passed\n";
    if !userspace_shell_exec_verified {
        serial_println!(
            "userspace shell exec verification failed: parent={:?}/{}, path={}, exit={:?}, exec={}/{}, cloexec={}, file_writes={}, output={}",
            shell_exec_child.parent_process_id,
            userspace_shell.process_id,
            shell_exec_child.path,
            shell_exec_child.exit_code(),
            shell_exec_child.exec_count,
            shell_exec_child.exec_failure_count,
            shell_exec_child.close_on_exec_count,
            shell_exec_child.file_write_count,
            shell_exec_output.bytes.len()
        );
        hlt_loop();
    }

    let userspace_pipeline_before = userspace_runtime.pipe_snapshot();
    if let Err(error) =
        userspace_runtime.inject_terminal_line("pipe-producer managed | upper | pipe-consumer")
    {
        serial_println!("userspace multi-stage pipeline command injection failed: {error}");
        hlt_loop();
    }
    let shell_producer =
        match userspace_runtime.wait_for_child_path(userspace_shell.process_id, "/pipe-producer") {
            Ok(result) => result,
            Err(error) => {
                serial_println!("userspace pipeline producer wait failed: {error}");
                hlt_loop();
            }
        };
    let shell_filter =
        match userspace_runtime.wait_for_child_path(userspace_shell.process_id, "/upper") {
            Ok(result) => result,
            Err(error) => {
                serial_println!("userspace pipeline filter wait failed: {error}");
                hlt_loop();
            }
        };
    let shell_consumer =
        match userspace_runtime.wait_for_child_path(userspace_shell.process_id, "/pipe-consumer") {
            Ok(result) => result,
            Err(error) => {
                serial_println!("userspace pipeline consumer wait failed: {error}");
                hlt_loop();
            }
        };
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace shell did not regain its prompt after pipeline: {error}");
        hlt_loop();
    }
    let userspace_pipeline_after = userspace_runtime.pipe_snapshot();

    for command in [
        "pipe-producer > /tmp/message.txt",
        "pipe-producer >> /tmp/message.txt",
        "upper < /tmp/message.txt > /tmp/upper.txt",
        "cat /hello.txt | upper > /tmp/pipeline.txt",
        "stderr-probe 2> /tmp/errors.txt",
        "stderr-probe > /tmp/all.txt 2>&1",
        "pipe-producer > /UFAT.TXT",
        "pipe-producer >> /UFAT.TXT",
        "upper < /UFAT.TXT > /UFATUP.TXT",
    ] {
        if let Err(error) = userspace_runtime.inject_terminal_line(command) {
            serial_println!("userspace redirection command `{command}` injection failed: {error}");
            hlt_loop();
        }
        if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
            serial_println!("userspace redirection command `{command}` did not finish: {error}");
            hlt_loop();
        }
    }

    let tmpfs_message = match vfs::read_file("/tmp/message.txt", 256) {
        Ok(data) => data,
        Err(error) => {
            serial_println!("tmpfs message verification read failed: {error}");
            hlt_loop();
        }
    };
    let tmpfs_upper = match vfs::read_file("/tmp/upper.txt", 256) {
        Ok(data) => data,
        Err(error) => {
            serial_println!("tmpfs upper verification read failed: {error}");
            hlt_loop();
        }
    };
    let tmpfs_pipeline = match vfs::read_file("/tmp/pipeline.txt", 256) {
        Ok(data) => data,
        Err(error) => {
            serial_println!("tmpfs pipeline verification read failed: {error}");
            hlt_loop();
        }
    };
    let tmpfs_errors = match vfs::read_file("/tmp/errors.txt", 256) {
        Ok(data) => data,
        Err(error) => {
            serial_println!("tmpfs stderr verification read failed: {error}");
            hlt_loop();
        }
    };
    let tmpfs_all = match vfs::read_file("/tmp/all.txt", 256) {
        Ok(data) => data,
        Err(error) => {
            serial_println!("tmpfs combined-stream verification read failed: {error}");
            hlt_loop();
        }
    };

    let fat_message = match vfs::read_file("/UFAT.TXT", 256) {
        Ok(data) => data,
        Err(error) => {
            serial_println!("persistent FAT message verification read failed: {error}");
            hlt_loop();
        }
    };
    let fat_upper = match vfs::read_file("/UFATUP.TXT", 256) {
        Ok(data) => data,
        Err(error) => {
            serial_println!("persistent FAT upper verification read failed: {error}");
            hlt_loop();
        }
    };

    let capacity_options = vfs::OpenOptions {
        read: false,
        write: true,
        create: true,
        truncate: true,
        append: false,
    };
    if let Err(error) = vfs::open("/tmp/capacity.bin", capacity_options) {
        serial_println!("tmpfs capacity fixture creation failed: {error}");
        hlt_loop();
    }
    let oversized_write = vfs::write_at(
        "/tmp/capacity.bin",
        0,
        &vec![0_u8; vfs::TMPFS_MAX_FILE_BYTES.saturating_add(1)],
    );
    let capacity_size = vfs::metadata("/tmp/capacity.bin")
        .map(|metadata| metadata.size)
        .unwrap_or(u64::MAX);
    let invalid_fat_name_rejected = matches!(
        vfs::open("/TOO-LONG-NAME.TXT", capacity_options),
        Err(vfs::Error::InvalidPath)
    );
    let fat_mirrors_valid = fat::verify_file_fat_copies("/UFAT.TXT").unwrap_or(false)
        && fat::verify_file_fat_copies("/UFATUP.TXT").unwrap_or(false);

    let mut expected_message = Vec::new();
    expected_message.extend_from_slice(b"Hello through a blocking NullStar OS pipe.\n");
    expected_message.extend_from_slice(b"Hello through a blocking NullStar OS pipe.\n");
    let mut expected_upper = Vec::new();
    expected_upper.extend_from_slice(b"HELLO THROUGH A BLOCKING NULLSTAR OS PIPE.\n");
    expected_upper.extend_from_slice(b"HELLO THROUGH A BLOCKING NULLSTAR OS PIPE.\n");
    let tmpfs_snapshot = vfs::tmpfs_info().expect("mounted tmpfs disappeared");
    let userspace_redirection_verified = tmpfs_message.bytes == expected_message
        && tmpfs_upper.bytes == expected_upper
        && tmpfs_pipeline.bytes == b"HELLO FROM A NULLSTAR OS USERSPACE FILE DESCRIPTOR.\n"
        && tmpfs_errors.bytes == b"stderr probe line\n"
        && tmpfs_all.bytes == b"stdout probe line\nstderr probe line\n"
        && fat_message.bytes == expected_message
        && fat_upper.bytes == expected_upper
        && matches!(oversized_write, Err(vfs::Error::FileTooLarge))
        && capacity_size == 0
        && invalid_fat_name_rejected
        && fat_mirrors_valid
        && tmpfs_snapshot.file_count >= 6
        && tmpfs_snapshot.rejected_writes >= 1;
    if !userspace_redirection_verified {
        serial_println!(
            "userspace tmpfs/redirection verification failed: message={}, upper={}, pipeline={}, errors={}, all={}, fat={}/{}, capacity={:?}/{}, invalid_name={}, mirrors={}, files={}, bytes={}, rejected={}",
            tmpfs_message.bytes.len(),
            tmpfs_upper.bytes.len(),
            tmpfs_pipeline.bytes.len(),
            tmpfs_errors.bytes.len(),
            tmpfs_all.bytes.len(),
            fat_message.bytes.len(),
            fat_upper.bytes.len(),
            oversized_write,
            capacity_size,
            invalid_fat_name_rejected,
            fat_mirrors_valid,
            tmpfs_snapshot.file_count,
            tmpfs_snapshot.total_bytes,
            tmpfs_snapshot.rejected_writes
        );
        hlt_loop();
    }
    let fat_write_snapshot = fat::write_info().expect("mounted FAT write accounting disappeared");
    serial_println!(
        "userspace tmpfs redirection verified: files={}, bytes={}, creates={}, truncates={}, writes={}, written={}, rejected={}, stdin_eof=true, shared_offset=true, fat_writable=true, fat_writes={}, fat_bytes={}, mirrors=true",
        tmpfs_snapshot.file_count,
        tmpfs_snapshot.total_bytes,
        tmpfs_snapshot.creates,
        tmpfs_snapshot.truncates,
        tmpfs_snapshot.writes,
        tmpfs_snapshot.bytes_written,
        tmpfs_snapshot.rejected_writes,
        fat_write_snapshot.writes,
        fat_write_snapshot.bytes_written
    );
    let userspace_redirection_pipe_after = userspace_runtime.pipe_snapshot();

    const BACKGROUND_DELAY_YIELDS: u64 = 64;
    if let Err(error) = userspace_runtime.inject_terminal_line("delay &") {
        serial_println!("userspace background command injection failed: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!(
            "userspace shell did not regain its prompt after background spawn: {error}"
        );
        hlt_loop();
    }
    let background_was_active =
        match userspace_runtime.child_is_active(userspace_shell.process_id, "/delay") {
            Ok(active) => active,
            Err(error) => {
                serial_println!("userspace background child inspection failed: {error}");
                hlt_loop();
            }
        };
    if !background_was_active {
        serial_println!("userspace background child completed before the shell prompt returned");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.inject_terminal_line("jobs") {
        serial_println!("userspace jobs command injection failed: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace jobs command did not return to the prompt: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.inject_terminal_line("wait") {
        serial_println!("userspace background wait injection failed: {error}");
        hlt_loop();
    }
    let background_child =
        match userspace_runtime.wait_for_child_path(userspace_shell.process_id, "/delay") {
            Ok(result) => result,
            Err(error) => {
                serial_println!("userspace background child wait failed: {error}");
                hlt_loop();
            }
        };
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!(
            "userspace shell did not regain its prompt after waiting for jobs: {error}"
        );
        hlt_loop();
    }

    serial_println!("userspace stopped-job test starting");
    let stopped_jobs_before = userspace::snapshot();
    if let Err(error) =
        userspace_runtime.inject_terminal_line("signal-probe | upper | pipe-consumer")
    {
        serial_println!("userspace stopped-job pipeline injection failed: {error}");
        hlt_loop();
    }
    let foreground_signal_group = match userspace_runtime.wait_until_foreground_child_group(
        userspace_shell.process_id,
        "/pipe-consumer",
        3,
    ) {
        Ok(group) => group,
        Err(error) => {
            serial_println!("userspace stopped-job foreground group did not start: {error}");
            hlt_loop();
        }
    };
    serial_println!(
        "userspace stopped-job foreground group ready: group={}, members={}",
        foreground_signal_group.process_group_id,
        foreground_signal_group.process_ids.len()
    );
    let terminal_before_suspend = userspace_runtime.terminal_snapshot();
    let foreground_stop_deliveries = match userspace_runtime.inject_terminal_suspend() {
        Ok(count) => count,
        Err(error) => {
            serial_println!("userspace terminal suspend injection failed: {error}");
            hlt_loop();
        }
    };
    let stopped_group = match userspace_runtime.wait_until_child_group_stopped(
        userspace_shell.process_id,
        foreground_signal_group.process_group_id,
        3,
    ) {
        Ok(group) => group,
        Err(error) => {
            serial_println!("userspace foreground group did not stop: {error}");
            hlt_loop();
        }
    };
    let stopped_scheduler = scheduler::snapshot();
    let terminal_after_suspend = userspace_runtime.terminal_snapshot();
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace shell did not regain its prompt after Ctrl-Z: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.inject_terminal_line("jobs") {
        serial_println!("userspace stopped-jobs command injection failed: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace stopped-jobs command did not return to the prompt: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.inject_terminal_line("bg %1") {
        serial_println!("userspace background-resume command injection failed: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!(
            "userspace background-resume command did not return to the prompt: {error}"
        );
        hlt_loop();
    }
    let resumed_group = match userspace_runtime.wait_until_child_group_resumed(
        userspace_shell.process_id,
        foreground_signal_group.process_group_id,
        3,
    ) {
        Ok(group) => group,
        Err(error) => {
            serial_println!("userspace stopped group did not resume in the background: {error}");
            hlt_loop();
        }
    };
    if let Err(error) = userspace_runtime.inject_terminal_line("fg %1") {
        serial_println!("userspace foreground-resume command injection failed: {error}");
        hlt_loop();
    }
    let foreground_resumed_group = match userspace_runtime.wait_until_foreground_child_group(
        userspace_shell.process_id,
        "/pipe-consumer",
        3,
    ) {
        Ok(group) => group,
        Err(error) => {
            serial_println!("userspace resumed group did not regain the foreground: {error}");
            hlt_loop();
        }
    };
    let terminal_before_interrupt = userspace_runtime.terminal_snapshot();
    let foreground_signal_deliveries = match userspace_runtime.inject_terminal_interrupt() {
        Ok(count) => count,
        Err(error) => {
            serial_println!("userspace terminal interrupt injection failed: {error}");
            hlt_loop();
        }
    };
    let foreground_signal_results = match userspace_runtime.wait_for_child_group(
        userspace_shell.process_id,
        foreground_signal_group.process_group_id,
    ) {
        Ok(results) => results,
        Err(error) => {
            serial_println!("userspace resumed foreground group wait failed: {error}");
            hlt_loop();
        }
    };
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace shell did not regain its prompt after resumed Ctrl-C: {error}");
        hlt_loop();
    }

    if let Err(error) = userspace_runtime.inject_terminal_line("signal-probe &") {
        serial_println!("userspace background signal probe injection failed: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace shell did not regain its prompt for kill test: {error}");
        hlt_loop();
    }
    let background_signal_group = match userspace_runtime.wait_until_child_group(
        userspace_shell.process_id,
        "/signal-probe",
        1,
    ) {
        Ok(group) => group,
        Err(error) => {
            serial_println!("userspace background signal group did not start: {error}");
            hlt_loop();
        }
    };
    if let Err(error) = userspace_runtime.inject_terminal_line("kill %1") {
        serial_println!("userspace kill command injection failed: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace kill command did not return to the prompt: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.inject_terminal_line("jobs") {
        serial_println!("userspace signaled-jobs command injection failed: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace signaled-jobs command did not return to the prompt: {error}");
        hlt_loop();
    }
    if let Err(error) = userspace_runtime.inject_terminal_line("wait") {
        serial_println!("userspace killed-job wait injection failed: {error}");
        hlt_loop();
    }
    let background_signal_results = match userspace_runtime.wait_for_child_group(
        userspace_shell.process_id,
        background_signal_group.process_group_id,
    ) {
        Ok(results) => results,
        Err(error) => {
            serial_println!("userspace killed-job wait failed: {error}");
            hlt_loop();
        }
    };
    if let Err(error) = userspace_runtime.wait_until_terminal_read(userspace_shell.process_id) {
        serial_println!("userspace shell did not regain its prompt after kill wait: {error}");
        hlt_loop();
    }
    let userspace_signal_pipe_after = userspace_runtime.pipe_snapshot();

    if let Err(error) = userspace_runtime.inject_terminal_line("exit") {
        serial_println!("userspace shell exit injection failed: {error}");
        hlt_loop();
    }
    let userspace_shell_result = match userspace_runtime.wait(userspace_shell.process_id) {
        Ok(result) => result,
        Err(error) => {
            serial_println!("userspace shell wait failed: {error}");
            hlt_loop();
        }
    };
    let stopped_jobs_after = userspace::snapshot();
    let created_pipe_delta = userspace_pipeline_after
        .total_created
        .saturating_sub(userspace_pipeline_before.total_created);
    let destroyed_pipe_delta = userspace_pipeline_after
        .total_destroyed
        .saturating_sub(userspace_pipeline_before.total_destroyed);
    let reader_retain_delta = userspace_pipeline_after
        .total_reader_retains
        .saturating_sub(userspace_pipeline_before.total_reader_retains);
    let writer_retain_delta = userspace_pipeline_after
        .total_writer_retains
        .saturating_sub(userspace_pipeline_before.total_writer_retains);
    let pipe_bytes_read_delta = userspace_pipeline_after
        .total_bytes_read
        .saturating_sub(userspace_pipeline_before.total_bytes_read);
    let pipe_bytes_written_delta = userspace_pipeline_after
        .total_bytes_written
        .saturating_sub(userspace_pipeline_before.total_bytes_written);
    let userspace_pipeline_verified = shell_producer.parent_process_id
        == Some(userspace_shell.process_id)
        && shell_filter.parent_process_id == Some(userspace_shell.process_id)
        && shell_consumer.parent_process_id == Some(userspace_shell.process_id)
        && shell_producer.exit_code() == Some(0)
        && shell_filter.exit_code() == Some(0)
        && shell_consumer.exit_code() == Some(0)
        && shell_producer.pipe_write_count >= 1
        && shell_producer.pipe_bytes_written == PIPE_TEST_BYTES
        && shell_filter.pipe_read_count >= 2
        && shell_filter.pipe_write_count >= 1
        && shell_filter.pipe_bytes_read == PIPE_TEST_BYTES
        && shell_filter.pipe_bytes_written == PIPE_TEST_BYTES
        && shell_filter.blocked_pipe_read_count >= 1
        && shell_consumer.pipe_read_count >= 2
        && shell_consumer.pipe_bytes_read == PIPE_TEST_BYTES
        && shell_consumer.blocked_pipe_read_count >= 1
        && userspace_shell_result.pipe_pair_count
            == userspace_shell_result.child_spawn_count.saturating_add(8)
        && userspace_shell_result.pipe_descriptor_close_count
            == userspace_shell_result
                .child_spawn_count
                .saturating_mul(2)
                .saturating_add(16)
        && userspace_shell_result.pipe_descriptor_inherit_count == 0
        && created_pipe_delta == 6
        && destroyed_pipe_delta == 6
        && reader_retain_delta == 14
        && writer_retain_delta == 14
        && pipe_bytes_read_delta == PIPE_TEST_BYTES.saturating_mul(2)
        && pipe_bytes_written_delta == PIPE_TEST_BYTES.saturating_mul(2)
        && userspace_pipeline_after.active_pipes == 0;
    if !userspace_pipeline_verified {
        serial_println!(
            "userspace multi-stage pipeline verification failed: producer_exit={:?}, filter_exit={:?}, consumer_exit={:?}, parents={:?}/{:?}/{:?}, producer_written={}, filter_read={}, filter_written={}, consumer_read={}, blocked_reads={}/{}, pairs={}, closes={}, inherited={}, created={}, destroyed={}, retains={}/{}, pipe_bytes={}/{}, active={}",
            shell_producer.exit_code(),
            shell_filter.exit_code(),
            shell_consumer.exit_code(),
            shell_producer.parent_process_id,
            shell_filter.parent_process_id,
            shell_consumer.parent_process_id,
            shell_producer.pipe_bytes_written,
            shell_filter.pipe_bytes_read,
            shell_filter.pipe_bytes_written,
            shell_consumer.pipe_bytes_read,
            shell_filter.blocked_pipe_read_count,
            shell_consumer.blocked_pipe_read_count,
            userspace_shell_result.pipe_pair_count,
            userspace_shell_result.pipe_descriptor_close_count,
            userspace_shell_result.pipe_descriptor_inherit_count,
            created_pipe_delta,
            destroyed_pipe_delta,
            reader_retain_delta,
            writer_retain_delta,
            pipe_bytes_read_delta,
            pipe_bytes_written_delta,
            userspace_pipeline_after.active_pipes
        );
        hlt_loop();
    }
    serial_println!(
        "userspace multi-stage pipeline verified: shell_pid={}, producer_pid={}, filter_pid={}, consumer_pid={}, stages=3, bytes={}, pairs={}, closes={}, inherited={}, active_pipes=0",
        userspace_shell_result.process_id,
        shell_producer.process_id,
        shell_filter.process_id,
        shell_consumer.process_id,
        shell_consumer.pipe_bytes_read,
        userspace_shell_result.pipe_pair_count,
        userspace_shell_result.pipe_descriptor_close_count,
        userspace_shell_result.pipe_descriptor_inherit_count
    );

    let userspace_background_verified = background_was_active
        && background_child.parent_process_id == Some(userspace_shell.process_id)
        && background_child.exit_code() == Some(0)
        && background_child.yield_count == BACKGROUND_DELAY_YIELDS
        && userspace_shell_result.child_poll_count >= 1
        && userspace_shell_result.child_poll_pending_count >= 1;
    if !userspace_background_verified {
        serial_println!(
            "userspace background job verification failed: active_at_prompt={}, child_parent={:?}, child_exit={:?}, yields={}, polls={}, pending_polls={}",
            background_was_active,
            background_child.parent_process_id,
            background_child.exit_code(),
            background_child.yield_count,
            userspace_shell_result.child_poll_count,
            userspace_shell_result.child_poll_pending_count
        );
        hlt_loop();
    }
    serial_println!(
        "userspace background jobs verified: shell_pid={}, child_pid={}, active_at_prompt=true, yields={}, polls={}, pending_polls={}, jobs=1",
        userspace_shell_result.process_id,
        background_child.process_id,
        background_child.yield_count,
        userspace_shell_result.child_poll_count,
        userspace_shell_result.child_poll_pending_count
    );

    let terminal_after_interrupt = userspace_runtime.terminal_snapshot();
    let signal_pipe_created = userspace_signal_pipe_after
        .total_created
        .saturating_sub(userspace_redirection_pipe_after.total_created);
    let signal_pipe_destroyed = userspace_signal_pipe_after
        .total_destroyed
        .saturating_sub(userspace_redirection_pipe_after.total_destroyed);
    let stop_delivery_delta = stopped_jobs_after
        .stop_deliveries
        .saturating_sub(stopped_jobs_before.stop_deliveries);
    let continue_delivery_delta = stopped_jobs_after
        .continue_deliveries
        .saturating_sub(stopped_jobs_before.continue_deliveries);
    let userspace_stopped_jobs_verified = foreground_stop_deliveries == 3
        && stopped_group.process_group_id == foreground_signal_group.process_group_id
        && stopped_group.process_ids.len() == 3
        && stopped_group.stopped == 3
        && stopped_group.runnable == 0
        && stopped_group.blocked == 0
        && stopped_scheduler.stopped_task_count >= 3
        && terminal_after_suspend.suspends == terminal_before_suspend.suspends.saturating_add(1)
        && terminal_after_suspend.foreground_process == Some(userspace_shell.process_id)
        && resumed_group.process_group_id == foreground_signal_group.process_group_id
        && resumed_group.process_ids.len() == 3
        && resumed_group.stopped == 0
        && resumed_group.runnable.saturating_add(resumed_group.blocked) == 3
        && foreground_resumed_group.process_group_id == foreground_signal_group.process_group_id
        && foreground_resumed_group.process_ids.len() == 3
        && stop_delivery_delta == 3
        && continue_delivery_delta == 3;
    if !userspace_stopped_jobs_verified {
        serial_println!(
            "userspace stopped-job verification failed: stop_deliveries={}, group={}/{}, members={}, stopped={}/{}, scheduler_stopped={}, foreground_after_stop={:?}, terminal_suspends={}/{}, resumed={}/{}/{}, foreground_resumed={}/{}, delivery_deltas={}/{}",
            foreground_stop_deliveries,
            stopped_group.process_group_id,
            foreground_signal_group.process_group_id,
            stopped_group.process_ids.len(),
            stopped_group.stopped,
            3,
            stopped_scheduler.stopped_task_count,
            terminal_after_suspend.foreground_process,
            terminal_before_suspend.suspends,
            terminal_after_suspend.suspends,
            resumed_group.runnable,
            resumed_group.blocked,
            resumed_group.stopped,
            foreground_resumed_group.process_group_id,
            foreground_resumed_group.process_ids.len(),
            stop_delivery_delta,
            continue_delivery_delta
        );
        hlt_loop();
    }
    serial_println!(
        "userspace stopped jobs verified: shell_pid={}, group={}, members=3, stopped=3, resumed_runnable={}, resumed_blocked={}, stop_deliveries={}, continue_deliveries={}, terminal_suspends={}",
        userspace_shell_result.process_id,
        stopped_group.process_group_id,
        resumed_group.runnable,
        resumed_group.blocked,
        stop_delivery_delta,
        continue_delivery_delta,
        terminal_after_suspend.suspends
    );

    let foreground_signal_verified = foreground_signal_deliveries == 3
        && foreground_signal_group.process_ids.len() == 3
        && foreground_signal_results.len() == 3
        && foreground_signal_results.iter().all(|result| {
            result.parent_process_id == Some(userspace_shell.process_id)
                && result.process_group_id == foreground_signal_group.process_group_id
                && result.signal() == Some(userspace::SIGNAL_INTERRUPT)
                && result.signal_received_count == 3
                && result.stop_count == 1
                && result.continue_count == 1
        })
        && foreground_signal_results
            .iter()
            .any(|result| result.path == "/signal-probe")
        && foreground_signal_results
            .iter()
            .any(|result| result.path == "/upper")
        && foreground_signal_results
            .iter()
            .any(|result| result.path == "/pipe-consumer")
        && terminal_after_interrupt.interrupts
            == terminal_before_interrupt.interrupts.saturating_add(1)
        && signal_pipe_created == 8
        && signal_pipe_destroyed == 8
        && userspace_signal_pipe_after.active_pipes == 0;
    let background_signal_verified = background_signal_group.process_ids.len() == 1
        && background_signal_results.len() == 1
        && background_signal_results.iter().all(|result| {
            result.parent_process_id == Some(userspace_shell.process_id)
                && result.process_group_id == background_signal_group.process_group_id
                && result.path == "/signal-probe"
                && result.signal() == Some(userspace::SIGNAL_TERMINATE)
                && result.signal_received_count == 1
                && result.stop_count == 0
                && result.continue_count == 0
        })
        && userspace_shell_result.signal_sent_count == 4;
    let userspace_signals_verified = foreground_signal_verified && background_signal_verified;
    if !userspace_signals_verified {
        serial_println!(
            "userspace signal verification failed: foreground_deliveries={}, foreground_members={}/{}, foreground_results={}, foreground_ok={}, terminal_interrupts={}/{}, signal_pipes={}/{}/active{}, background_members={}, background_results={}, background_ok={}, shell_signals={}",
            foreground_signal_deliveries,
            foreground_signal_group.process_ids.len(),
            3,
            foreground_signal_results.len(),
            foreground_signal_verified,
            terminal_before_interrupt.interrupts,
            terminal_after_interrupt.interrupts,
            signal_pipe_created,
            signal_pipe_destroyed,
            userspace_signal_pipe_after.active_pipes,
            background_signal_group.process_ids.len(),
            background_signal_results.len(),
            background_signal_verified,
            userspace_shell_result.signal_sent_count
        );
        hlt_loop();
    }
    serial_println!(
        "userspace process groups and signals verified: shell_pid={}, foreground_group={}, foreground_members=3, ctrl_c_deliveries={}, background_group={}, kill_deliveries=1, terminal_interrupts={}",
        userspace_shell_result.process_id,
        foreground_signal_group.process_group_id,
        foreground_signal_deliveries,
        background_signal_group.process_group_id,
        terminal_after_interrupt.interrupts
    );

    let userspace_shell_environment_verified = shell_variable_expansion_verified
        && shell_environment_child_verified
        && userspace_shell_result.environment_count == 0
        && userspace_shell_result.environment_change_count == 2;
    if !userspace_shell_environment_verified {
        serial_println!(
            "userspace shell environment verification failed: expansion={}, child={}, final_environment={}/{}, child_environment={}/{}",
            shell_variable_expansion_verified,
            shell_environment_child_verified,
            userspace_shell_result.environment_count,
            userspace_shell_result.environment_change_count,
            shell_environment_child.environment_count,
            shell_environment_child.environment_change_count
        );
        hlt_loop();
    }
    serial_println!(
        "userspace shell variables verified: shell_pid={}, child_pid={}, changes=2, local_expansion=true, redirect_expansion=true, export_inherited=true, unset=true",
        userspace_shell_result.process_id,
        shell_environment_child.process_id
    );

    let userspace_shell_verified = userspace_shell_result.exit_code() == Some(0)
        && userspace_shell_result.child_spawn_count == 22
        && userspace_shell_result.fork_count == userspace_shell_result.child_spawn_count
        && userspace_shell_result.child_wait_count
            == userspace_shell_result
                .child_spawn_count
                .saturating_add(foreground_stop_deliveries as u64)
        && userspace_shell_result.child_poll_count == 5
        && userspace_shell_result.child_poll_pending_count >= 1
        && userspace_shell_result.child_poll_pending_count
            < userspace_shell_result.child_poll_count
        && userspace_shell_result.signal_sent_count == 4
        && userspace_shell_result.open_count == 13
        && userspace_shell_result.file_descriptor_inherit_count == 0
        && shell_child.parent_process_id == Some(userspace_shell.process_id)
        && shell_child.exit_code() == Some(0)
        && shell_child.open_count == 1
        && shell_child.bytes_read > 0
        && userspace_pipeline_verified
        && userspace_redirection_verified
        && userspace_exec_verified
        && userspace_shell_exec_verified
        && userspace_shell_environment_verified
        && userspace_background_verified
        && userspace_stopped_jobs_verified
        && userspace_signals_verified
        && stopped_jobs_after.waitable_zombies == 0
        && userspace_runtime
            .terminal_snapshot()
            .foreground_process
            .is_none();
    if !userspace_shell_verified {
        serial_println!(
            "userspace shell verification failed: shell_exit={:?}, spawns={}, forks={}, waits={}, polls={}, pending_polls={}, zombies={}, signals={}, opens={}, inherited_files={}, environment={}/{}, stopped_jobs={}, exec_child={}, environment_child={}, child_parent={:?}, child_exit={:?}, child_opens={}, child_bytes={}, foreground={:?}",
            userspace_shell_result.exit_code(),
            userspace_shell_result.child_spawn_count,
            userspace_shell_result.fork_count,
            userspace_shell_result.child_wait_count,
            userspace_shell_result.child_poll_count,
            userspace_shell_result.child_poll_pending_count,
            stopped_jobs_after.waitable_zombies,
            userspace_shell_result.signal_sent_count,
            userspace_shell_result.open_count,
            userspace_shell_result.file_descriptor_inherit_count,
            userspace_shell_result.environment_count,
            userspace_shell_result.environment_change_count,
            userspace_stopped_jobs_verified,
            userspace_shell_exec_verified,
            userspace_shell_environment_verified,
            shell_child.parent_process_id,
            shell_child.exit_code(),
            shell_child.open_count,
            shell_child.bytes_read,
            userspace_runtime.terminal_snapshot().foreground_process
        );
        hlt_loop();
    }
    serial_println!(
        "userspace shell verified: shell_pid={}, child_pid={}, spawns={}, forks={}, waits={}, polls={}, pending_polls={}, signals={}, environment_changes=2, stopped_jobs=1, exec_jobs=1, child_exit=0, child_bytes={}",
        userspace_shell_result.process_id,
        shell_child.process_id,
        userspace_shell_result.child_spawn_count,
        userspace_shell_result.fork_count,
        userspace_shell_result.child_wait_count,
        userspace_shell_result.child_poll_count,
        userspace_shell_result.child_poll_pending_count,
        userspace_shell_result.signal_sent_count,
        shell_child.bytes_read
    );

    println!("NullStar OS");
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
        println!("FAT filesystem mounted");
    } else {
        println!("FAT filesystem unavailable");
    }
    if vfs_info.is_some() {
        println!("Virtual filesystem mounted");
    } else {
        println!("Virtual filesystem unavailable");
    }
    if tmpfs_info.is_some() {
        println!("Writable /tmp tmpfs mounted");
    } else {
        println!("Writable tmpfs unavailable");
    }
    if elf_image.is_some() {
        println!("ELF64 image validated");
    } else {
        println!("ELF64 validation unavailable");
    }
    if process_verified {
        println!("Process scheduling and fault isolation verified");
    } else {
        println!("Userspace process verification unavailable");
    }
    if file_io_verified {
        println!("Userspace file descriptors verified");
    } else {
        println!("Userspace file descriptors unavailable");
    }
    if userspace_rust_runtime_verified {
        println!("Shared Rust userspace runtime verified");
    } else {
        println!("Rust userspace runtime unavailable");
    }
    if userspace_exec_verified {
        println!("Transactional userspace exec verified");
    } else {
        println!("Userspace exec unavailable");
    }
    if userspace_environments_verified {
        println!("Userspace process environments verified");
    } else {
        println!("Userspace process environments unavailable");
    }
    if userspace_handled_signals_verified {
        println!("Userspace signal handlers verified");
    } else {
        println!("Userspace signal handlers unavailable");
    }
    if terminal_verified {
        println!("Blocking userspace terminal verified");
    } else {
        println!("Userspace terminal unavailable");
    }
    if pipe_verified {
        println!("Blocking userspace pipes verified");
    } else {
        println!("Userspace pipes unavailable");
    }
    if userspace_shell_verified {
        println!("Userspace process-control shell verified");
    } else {
        println!("Userspace process-control shell unavailable");
    }
    if userspace_redirection_verified {
        println!("Writable tmpfs and userspace redirection verified");
    } else {
        println!("Userspace redirection unavailable");
    }
    if userspace_pipeline_verified {
        println!("Userspace multi-stage descriptor pipelines verified");
    } else {
        println!("Userspace descriptor pipelines unavailable");
    }
    if userspace_background_verified {
        println!("Userspace background jobs verified");
    } else {
        println!("Userspace background jobs unavailable");
    }
    if userspace_stopped_jobs_verified {
        println!("Userspace stopped-job control verified");
    } else {
        println!("Userspace stopped-job control unavailable");
    }
    if userspace_signals_verified {
        println!("Userspace process groups and signals verified");
    } else {
        println!("Userspace process groups and signals unavailable");
    }
    println!("Interactive shell initialized");

    let runtime_memory = userspace_runtime.memory_stats();
    let usable_frames = runtime_memory.usable_frames;
    let allocated_frames = runtime_memory.allocated_frames;
    let remaining_frames = runtime_memory.remaining_frames;
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
    serial_println!(
        "physical frame recycling: reclaimed={}, recycled={}, reused={}",
        runtime_memory.reclaimed_frames,
        runtime_memory.recycled_frames,
        runtime_memory.reused_frames
    );
    serial_println!("framebuffer console initialized");
    serial_println!("preemptive scheduler initialized");
    serial_println!("interactive shell initialized");
    serial_println!("kernel entered kernel_main");

    let system_info = shell::SystemInfo::new(
        acpi_info,
        interrupt_controller,
        pci_inventory,
        storage_info,
        partition_inventory,
        filesystem_info,
    );
    enter_interactive_shell(system_info, userspace_runtime);
}

fn detect_boot_mode() -> boot_mode::BootMode {
    match vfs::read_file(boot_mode::BOOT_MODE_PATH, 32) {
        Ok(file) => match boot_mode::BootMode::parse(&file.bytes) {
            Some(mode) => mode,
            None => {
                serial_println!(
                    "invalid {} contents; defaulting to normal boot",
                    boot_mode::BOOT_MODE_PATH
                );
                boot_mode::BootMode::Normal
            }
        },
        Err(error) => {
            serial_println!(
                "could not read {} ({error}); defaulting to normal boot",
                boot_mode::BOOT_MODE_PATH
            );
            boot_mode::BootMode::Normal
        }
    }
}

fn enter_userspace(system_info: shell::SystemInfo, mut userspace_runtime: userspace::Runtime) -> ! {
    let init = match userspace_runtime.spawn_foreground("/init", &[]) {
        Ok(init) => init,
        Err(error) => {
            record_kernel_event(
                KERNEL_USERSPACE_INIT_FAILED_EVENT_ID,
                LogSeverity::Error,
                PrivacyClass::Public,
                early_log::EarlySource::KERNEL,
                "kernel.process",
                "userspace init failed to start",
            );
            serial_println!("failed to start userspace init: {error}");
            println!("Userspace init failed to start.");
            println!("Entering the emergency kernel shell.");
            enter_interactive_shell(system_info, userspace_runtime);
        }
    };
    if init.process_id != userspace::INIT_PROCESS_ID {
        serial_println!(
            "userspace init received unexpected pid: expected={}, actual={}",
            userspace::INIT_PROCESS_ID,
            init.process_id
        );
        hlt_loop();
    }
    record_kernel_event(
        KERNEL_USERSPACE_INIT_STARTED_EVENT_ID,
        LogSeverity::Notice,
        PrivacyClass::Public,
        early_log::EarlySource::KERNEL,
        "kernel.process",
        "userspace init started",
    );
    serial_println!(
        "userspace init started: pid={}, group={}, task={}, path={}, entry={:#018x}",
        init.process_id,
        init.process_group_id,
        init.task_id,
        init.path,
        init.entry_point
    );

    let mut reported_seconds = 0;
    loop {
        x86_64::instructions::hlt();
        if let Err(error) = userspace_runtime.poll() {
            serial_println!("userspace runtime poll failed: {error}");
        }
        if !userspace_runtime.process_is_active(init.process_id) {
            serial_println!(
                "userspace init terminated: pid={}; entering emergency kernel shell",
                init.process_id
            );
            println!();
            println!("Userspace init terminated.");
            println!("Entering the emergency kernel shell.");
            enter_interactive_shell(system_info, userspace_runtime);
        }

        let elapsed_seconds = interrupts::timer_ticks() / interrupts::TIMER_HZ;
        if elapsed_seconds > reported_seconds {
            reported_seconds = elapsed_seconds;
            serial_println!("uptime: {elapsed_seconds}s");
        }
    }
}

fn enter_interactive_shell(
    system_info: shell::SystemInfo,
    userspace_runtime: userspace::Runtime,
) -> ! {
    let mut interactive_shell = shell::Shell::new(system_info, userspace_runtime);
    interactive_shell.start();

    let mut reported_seconds = 0;
    loop {
        x86_64::instructions::hlt();
        interactive_shell.poll();

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistentFatPhase {
    Prepared,
    Verified,
}

const PERSISTENT_TEXT_PATH: &str = "/PERSIST.TXT";
const PERSISTENT_CHAIN_PATH: &str = "/CHAIN.BIN";
const PERSISTENT_TRUNCATE_PATH: &str = "/TRUNC.BIN";
const PERSISTENT_HOLE_PATH: &str = "/HOLE.BIN";
const PERSISTENT_TEXT_FIRST: &[u8] = b"persistent-fat-line-one\n";
const PERSISTENT_TEXT_SECOND: &[u8] = b"persistent-fat-line-two\n";
const PERSISTENT_TRUNCATED: &[u8] = b"small-after-truncate\n";
const PERSISTENT_HOLE_OFFSET: usize = 4096;
const PERSISTENT_HOLE_TAIL: &[u8] = b"tail";
const PERSISTENT_CHAIN_BYTES: usize = 7000;
const PERSISTENT_TRUNCATE_SEED_BYTES: usize = 9000;

fn persistent_fat_self_test() -> Result<PersistentFatPhase, vfs::Error> {
    match vfs::metadata(PERSISTENT_TEXT_PATH) {
        Ok(_) => {
            verify_persistent_fat_files()?;
            Ok(PersistentFatPhase::Verified)
        }
        Err(vfs::Error::NotFound) => {
            prepare_persistent_fat_files()?;
            verify_persistent_fat_files()?;
            Ok(PersistentFatPhase::Prepared)
        }
        Err(error) => Err(error),
    }
}

fn prepare_persistent_fat_files() -> Result<(), vfs::Error> {
    create_truncated_fat_file(PERSISTENT_TEXT_PATH)?;
    write_vfs_bytes(PERSISTENT_TEXT_PATH, 0, PERSISTENT_TEXT_FIRST)?;
    let (append_offset, appended) = vfs::append(PERSISTENT_TEXT_PATH, PERSISTENT_TEXT_SECOND)?;
    if append_offset != PERSISTENT_TEXT_FIRST.len() as u64
        || appended != PERSISTENT_TEXT_SECOND.len()
    {
        return Err(vfs::Error::ShortRead {
            expected: PERSISTENT_TEXT_SECOND.len(),
            actual: appended,
        });
    }

    create_truncated_fat_file(PERSISTENT_CHAIN_PATH)?;
    let chain = persistent_chain_bytes();
    write_vfs_bytes(PERSISTENT_CHAIN_PATH, 0, &chain)?;

    create_truncated_fat_file(PERSISTENT_TRUNCATE_PATH)?;
    let truncate_seed = persistent_pattern(PERSISTENT_TRUNCATE_SEED_BYTES, 29, 7);
    write_vfs_bytes(PERSISTENT_TRUNCATE_PATH, 0, &truncate_seed)?;
    vfs::open(
        PERSISTENT_TRUNCATE_PATH,
        vfs::OpenOptions {
            read: false,
            write: true,
            create: false,
            truncate: true,
            append: false,
        },
    )?;
    write_vfs_bytes(PERSISTENT_TRUNCATE_PATH, 0, PERSISTENT_TRUNCATED)?;

    create_truncated_fat_file(PERSISTENT_HOLE_PATH)?;
    write_vfs_bytes(
        PERSISTENT_HOLE_PATH,
        PERSISTENT_HOLE_OFFSET as u64,
        PERSISTENT_HOLE_TAIL,
    )?;
    Ok(())
}

fn create_truncated_fat_file(path: &str) -> Result<(), vfs::Error> {
    vfs::open(
        path,
        vfs::OpenOptions {
            read: false,
            write: true,
            create: true,
            truncate: true,
            append: false,
        },
    )?;
    Ok(())
}

fn write_vfs_bytes(path: &str, offset: u64, bytes: &[u8]) -> Result<(), vfs::Error> {
    let actual = vfs::write_at(path, offset, bytes)?;
    if actual != bytes.len() {
        return Err(vfs::Error::ShortRead {
            expected: bytes.len(),
            actual,
        });
    }
    Ok(())
}

fn verify_persistent_fat_files() -> Result<(), vfs::Error> {
    let mut text = Vec::with_capacity(PERSISTENT_TEXT_FIRST.len() + PERSISTENT_TEXT_SECOND.len());
    text.extend_from_slice(PERSISTENT_TEXT_FIRST);
    text.extend_from_slice(PERSISTENT_TEXT_SECOND);
    verify_vfs_file(PERSISTENT_TEXT_PATH, &text)?;

    let chain = persistent_chain_bytes();
    verify_vfs_file(PERSISTENT_CHAIN_PATH, &chain)?;
    verify_vfs_file(PERSISTENT_TRUNCATE_PATH, PERSISTENT_TRUNCATED)?;

    let mut hole = vec![0_u8; PERSISTENT_HOLE_OFFSET + PERSISTENT_HOLE_TAIL.len()];
    hole[PERSISTENT_HOLE_OFFSET..].copy_from_slice(PERSISTENT_HOLE_TAIL);
    verify_vfs_file(PERSISTENT_HOLE_PATH, &hole)?;

    for path in [
        PERSISTENT_TEXT_PATH,
        PERSISTENT_CHAIN_PATH,
        PERSISTENT_TRUNCATE_PATH,
        PERSISTENT_HOLE_PATH,
    ] {
        if !fat::verify_file_fat_copies(path)? {
            return Err(vfs::Error::Fat(fat::Error::CorruptDirectory));
        }
    }
    Ok(())
}

fn verify_vfs_file(path: &str, expected: &[u8]) -> Result<(), vfs::Error> {
    let data = vfs::read_file(path, fat::MAX_FILE_WRITE_BYTES)?;
    if data.truncated || data.total_size != expected.len() as u64 || data.bytes != expected {
        return Err(vfs::Error::ShortRead {
            expected: expected.len(),
            actual: data.bytes.len(),
        });
    }
    Ok(())
}

fn persistent_chain_bytes() -> Vec<u8> {
    persistent_pattern(PERSISTENT_CHAIN_BYTES, 17, 3)
}

fn persistent_pattern(length: usize, multiplier: usize, addend: usize) -> Vec<u8> {
    (0..length)
        .map(|index| index.wrapping_mul(multiplier).wrapping_add(addend) as u8)
        .collect()
}

fn persistent_checksum(path: &str) -> Result<u32, vfs::Error> {
    let data = vfs::read_file(path, fat::MAX_FILE_WRITE_BYTES)?;
    Ok(data
        .bytes
        .iter()
        .copied()
        .fold(0x811c_9dc5_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        }))
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

fn record_kernel_event(
    event_id: EventId,
    severity: LogSeverity,
    privacy: PrivacyClass,
    source: early_log::EarlySource,
    subsystem: &str,
    message: &str,
) {
    let monotonic_time_ns = interrupts::timer_ticks().saturating_mul(
        1_000_000_000_u64
            .checked_div(interrupts::TIMER_HZ)
            .unwrap_or(0),
    );
    let _ = early_log::try_record_kernel_early_log(early_log::EarlyLogInput {
        event_id,
        severity,
        privacy,
        monotonic_time_ns,
        source,
        subsystem,
        message,
    });
}

fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

#[alloc_error_handler]
fn allocation_error(layout: Layout) -> ! {
    record_kernel_event(
        KERNEL_ALLOCATION_FAILURE_EVENT_ID,
        LogSeverity::Emergency,
        PrivacyClass::Public,
        early_log::EarlySource::KERNEL,
        "kernel.memory",
        "kernel allocation failure",
    );
    serial_println!("KERNEL ALLOCATION ERROR: {layout:?}");
    hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    record_kernel_event(
        KERNEL_PANIC_EVENT_ID,
        LogSeverity::Emergency,
        PrivacyClass::Public,
        early_log::EarlySource::KERNEL,
        "kernel.panic",
        "kernel panic",
    );
    serial_println!("KERNEL PANIC: {info}");
    hlt_loop();
}
