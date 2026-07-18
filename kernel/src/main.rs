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
mod process;
mod scheduler;
mod shell;
mod storage;
mod vfs;

pub(crate) use arch::x86_64::{acpi, apic, gdt, hpet, interrupts};
pub(crate) use drivers::{ahci, console, keyboard, pci, serial};
pub(crate) use memory::allocator;
pub(crate) use process::{elf, userspace};
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

    let process_frame_baseline = frame_allocator.allocated_frame_count();
    let init_image = match elf::validate("/init") {
        Ok(image) => image,
        Err(error) => {
            serial_println!("userspace init validation failed: {error}");
            hlt_loop();
        }
    };
    let fault_image = match elf::validate("/fault-probe") {
        Ok(image) => image,
        Err(error) => {
            serial_println!("userspace fault-probe validation failed: {error}");
            hlt_loop();
        }
    };

    let init_spawn = match userspace::spawn(
        "/init",
        "user-init",
        &init_image,
        &mut mapper,
        &mut frame_allocator,
        physical_memory_offset,
    ) {
        Ok(info) => info,
        Err(error) => {
            serial_println!("failed to spawn /init: {error}");
            hlt_loop();
        }
    };
    serial_println!(
        "userspace process spawned: pid={}, task={}, path={}, entry={:#018x}, page_table={:#x}, mapped_pages={}, owned_frames={}",
        init_spawn.process_id,
        init_spawn.task_id,
        init_spawn.path,
        init_spawn.entry_point,
        init_spawn.page_table_address,
        init_spawn.mapped_pages,
        init_spawn.owned_frames
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

    let process_snapshot = userspace::wait_for_all(&mut frame_allocator, 2);
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

    let init_result = process_snapshot
        .results
        .iter()
        .find(|result| result.path == "/init");
    let fault_result = process_snapshot
        .results
        .iter()
        .find(|result| result.path == "/fault-probe");
    let init_valid = init_result
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
        && init_valid
        && fault_valid
        && frame_balance;
    if !process_verified {
        serial_println!(
            "process isolation verification failed: spawned={}, active={}, exited={}, faulted={}, reaped={}, baseline_frames={}, final_frames={}, init_valid={}, fault_valid={}",
            process_snapshot.spawned,
            process_snapshot.active,
            process_snapshot.exited,
            process_snapshot.faulted,
            process_snapshot.reaped,
            process_frame_baseline,
            process_frame_after,
            init_valid,
            fault_valid
        );
        hlt_loop();
    }
    let init_result = init_result.expect("validated init result disappeared");
    serial_println!(
        "process isolation verified: spawned={}, exited={}, faulted={}, reaped={}, frames_reclaimed={}, frame_balance={}, init_schedules={}, init_runtime_ticks={}",
        process_snapshot.spawned,
        process_snapshot.exited,
        process_snapshot.faulted,
        process_snapshot.reaped,
        process_snapshot.frames_reclaimed,
        frame_balance,
        init_result.scheduled_count,
        init_result.runtime_ticks
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

    const PIPE_TEST_BYTES: u64 = 42;
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

    let userspace_pipeline_before = userspace_runtime.pipe_snapshot();
    if let Err(error) =
        userspace_runtime.inject_terminal_line("pipe-producer | upper | pipe-consumer")
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
        && userspace_shell_result.pipe_pair_count == 2
        && userspace_shell_result.pipe_descriptor_close_count == 4
        && userspace_shell_result.pipe_descriptor_inherit_count == 4
        && created_pipe_delta == 2
        && destroyed_pipe_delta == 2
        && reader_retain_delta == 2
        && writer_retain_delta == 2
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

    let userspace_shell_verified = userspace_shell_result.exit_code() == Some(0)
        && userspace_shell_result.child_spawn_count == 4
        && userspace_shell_result.child_wait_count == 4
        && shell_child.parent_process_id == Some(userspace_shell.process_id)
        && shell_child.exit_code() == Some(0)
        && shell_child.open_count == 1
        && shell_child.bytes_read > 0
        && userspace_pipeline_verified
        && userspace_runtime
            .terminal_snapshot()
            .foreground_process
            .is_none();
    if !userspace_shell_verified {
        serial_println!(
            "userspace shell verification failed: shell_exit={:?}, spawns={}, waits={}, child_parent={:?}, child_exit={:?}, child_opens={}, child_bytes={}, foreground={:?}",
            userspace_shell_result.exit_code(),
            userspace_shell_result.child_spawn_count,
            userspace_shell_result.child_wait_count,
            shell_child.parent_process_id,
            shell_child.exit_code(),
            shell_child.open_count,
            shell_child.bytes_read,
            userspace_runtime.terminal_snapshot().foreground_process
        );
        hlt_loop();
    }
    serial_println!(
        "userspace shell verified: shell_pid={}, child_pid={}, spawns={}, waits={}, child_exit=0, child_bytes={}",
        userspace_shell_result.process_id,
        shell_child.process_id,
        userspace_shell_result.child_spawn_count,
        userspace_shell_result.child_wait_count,
        shell_child.bytes_read
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
    if vfs_info.is_some() {
        println!("Virtual filesystem mounted");
    } else {
        println!("Virtual filesystem unavailable");
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
    if userspace_pipeline_verified {
        println!("Userspace multi-stage descriptor pipelines verified");
    } else {
        println!("Userspace descriptor pipelines unavailable");
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
