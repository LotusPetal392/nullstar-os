use alloc::{string::String, vec};

use pc_keyboard::{DecodedKey, KeyCode};

use crate::{
    acpi, ahci, allocator, console, elf, fat, interrupts, memory, partition, pci, userspace, vfs,
};

const PROMPT: &str = "galactic> ";
const DEFAULT_CONSOLE_COLUMNS: usize = 80;
const MAX_COMMAND_LENGTH: usize = 128;
const MAX_PCI_SHELL_FUNCTIONS: usize = 64;
const DISK_PREVIEW_BYTES: usize = 128;
const FILE_PREVIEW_BYTES: usize = 16 * 1024;

macro_rules! shell_print {
    ($($argument:tt)*) => {{
        crate::print!($($argument)*);
        crate::serial_print!($($argument)*);
    }};
}

macro_rules! shell_println {
    () => {{
        crate::println!();
        crate::serial_println!();
    }};
    ($($argument:tt)*) => {{
        crate::println!($($argument)*);
        crate::serial_println!($($argument)*);
    }};
}

#[derive(Debug)]
pub struct SystemInfo {
    usable_frames: u64,
    allocated_frames: u64,
    remaining_frames: u64,
    acpi: Option<acpi::AcpiInfo>,
    interrupt_controller: interrupts::ControllerInfo,
    pci_inventory: Option<pci::Inventory>,
    storage: Option<ahci::DiskInfo>,
    partitions: Option<partition::Inventory>,
    filesystem: Option<fat::VolumeInfo>,
}

impl SystemInfo {
    pub fn new(
        usable_frames: u64,
        allocated_frames: u64,
        remaining_frames: u64,
        acpi: Option<acpi::AcpiInfo>,
        interrupt_controller: interrupts::ControllerInfo,
        pci_inventory: Option<pci::Inventory>,
        storage: Option<ahci::DiskInfo>,
        partitions: Option<partition::Inventory>,
        filesystem: Option<fat::VolumeInfo>,
    ) -> Self {
        Self {
            usable_frames,
            allocated_frames,
            remaining_frames,
            acpi,
            interrupt_controller,
            pci_inventory,
            storage,
            partitions,
            filesystem,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellAction {
    Continue,
    Halt,
}

pub struct Shell {
    input: String,
    max_input_chars: usize,
    system_info: SystemInfo,
}

impl Shell {
    pub fn new(system_info: SystemInfo) -> Self {
        let console_columns = console::text_columns().unwrap_or(DEFAULT_CONSOLE_COLUMNS);
        let max_input_chars = console_columns
            .saturating_sub(PROMPT.len() + 1)
            .min(MAX_COMMAND_LENGTH)
            .max(1);

        Self {
            input: String::new(),
            max_input_chars,
            system_info,
        }
    }

    pub fn start(&self) {
        shell_println!();
        shell_println!("Interactive shell ready. Type `help` for commands.");
        self.print_prompt();
    }

    pub fn handle_key(&mut self, key: DecodedKey) -> ShellAction {
        match key {
            DecodedKey::Unicode(character) => self.handle_character(character),
            DecodedKey::RawKey(key_code) => self.handle_raw_key(key_code),
        }
    }

    fn handle_character(&mut self, character: char) -> ShellAction {
        match character {
            '\n' | '\r' => self.submit(),
            '\u{8}' | '\u{7f}' => {
                self.backspace();
                ShellAction::Continue
            }
            '\t' => ShellAction::Continue,
            character if character.is_control() => ShellAction::Continue,
            character => {
                self.append_character(character);
                ShellAction::Continue
            }
        }
    }

    fn handle_raw_key(&mut self, key_code: KeyCode) -> ShellAction {
        match key_code {
            KeyCode::Return | KeyCode::NumpadEnter => self.submit(),
            KeyCode::Backspace | KeyCode::Delete => {
                self.backspace();
                ShellAction::Continue
            }
            _ => ShellAction::Continue,
        }
    }

    fn append_character(&mut self, character: char) {
        if self.input.chars().count() >= self.max_input_chars {
            return;
        }

        self.input.push(character);
        shell_print!("{character}");
    }

    fn backspace(&mut self) {
        if self.input.pop().is_none() {
            return;
        }

        console::backspace();
        crate::serial_print!("\u{8} \u{8}");
    }

    fn submit(&mut self) -> ShellAction {
        shell_println!();

        let action = self.execute(self.input.trim());
        self.input.clear();

        if action == ShellAction::Continue {
            self.print_prompt();
        }

        action
    }

    fn execute(&self, command_line: &str) -> ShellAction {
        let mut words = command_line.split_whitespace();
        let Some(command) = words.next() else {
            return ShellAction::Continue;
        };

        match command {
            "help" => print_help(),
            "clear" => {
                console::clear();
                crate::serial_println!("screen cleared");
            }
            "echo" => {
                let text = command_line
                    .strip_prefix(command)
                    .unwrap_or_default()
                    .trim_start();
                shell_println!("{text}");
            }
            "uptime" => {
                let ticks = interrupts::timer_ticks();
                let seconds = ticks / interrupts::TIMER_HZ;
                shell_println!("uptime: {seconds}s ({ticks} ticks)");
            }
            "memory" => self.print_memory(),
            "heap" => {
                shell_println!(
                    "heap: start={:#x}, size={} KiB, pages={}",
                    allocator::HEAP_START,
                    allocator::HEAP_SIZE / 1024,
                    allocator::HEAP_PAGE_COUNT
                );
            }
            "acpi" => self.print_acpi(),
            "interrupts" => self.print_interrupts(),
            "pci" => self.print_pci(),
            "disk" => self.handle_disk_command(words.next(), words.next()),
            "partitions" => self.print_partitions(),
            "fs" => self.print_filesystem(),
            "vfs" => self.print_vfs(),
            "ls" => self.list_files(words.next().unwrap_or("/")),
            "cat" => {
                let Some(path) = words.next() else {
                    shell_println!("usage: cat <path>");
                    return ShellAction::Continue;
                };
                self.cat_file(path);
            }
            "elf" => {
                let Some(path) = words.next() else {
                    shell_println!("usage: elf <path>");
                    return ShellAction::Continue;
                };
                self.inspect_elf(path);
            }
            "process" | "userspace" => self.print_userspace(),
            "about" => {
                shell_println!("GalacticOS: an experimental x86-64 kernel written in Rust.");
            }
            "halt" => {
                shell_println!("Halting GalacticOS.");
                return ShellAction::Halt;
            }
            _ => {
                shell_println!("unknown command: {command}");
                shell_println!("type `help` to list available commands");
            }
        }

        ShellAction::Continue
    }

    fn print_memory(&self) {
        let usable_mebibytes = self
            .system_info
            .usable_frames
            .saturating_mul(memory::FRAME_SIZE)
            / (1024 * 1024);

        shell_println!(
            "physical memory: {} MiB usable ({} frames)",
            usable_mebibytes,
            self.system_info.usable_frames
        );
        shell_println!(
            "frames: {} allocated, {} remaining",
            self.system_info.allocated_frames,
            self.system_info.remaining_frames
        );
    }

    fn handle_disk_command(&self, action: Option<&str>, argument: Option<&str>) {
        match action {
            None | Some("info") => self.print_disk_info(),
            Some("read") => {
                let Some(lba) = argument.and_then(parse_u64) else {
                    shell_println!("usage: disk read <logical-block-address>");
                    return;
                };
                self.read_disk_block(lba);
            }
            Some(_) => shell_println!("usage: disk [info | read <logical-block-address>]"),
        }
    }

    fn print_disk_info(&self) {
        let Some(info) = self.system_info.storage.as_ref() else {
            shell_println!("AHCI disk: unavailable");
            return;
        };

        shell_println!(
            "AHCI disk: `{}` at controller {}, port {}",
            info.model,
            info.controller_location,
            info.port
        );
        shell_println!(
            "identity: serial=`{}`, firmware=`{}`, PCI={:04x}:{:04x}",
            info.serial,
            info.firmware,
            info.vendor_id,
            info.device_id
        );
        shell_println!(
            "geometry: {} blocks x {} bytes, capacity={} MiB, LBA48={}",
            info.logical_block_count,
            info.logical_block_size,
            info.capacity_bytes / (1024 * 1024),
            info.lba48
        );
        shell_println!(
            "AHCI: ABAR={:#x}, version={:#010x}, slots={}, PI={:#010x}, DMA64={}",
            info.abar,
            info.hba_version,
            info.command_slots,
            info.implemented_ports,
            info.supports_64_bit_dma
        );
        shell_println!(
            "sector 0: signature={:#06x}, checksum={:#010x}",
            info.sector_zero_signature,
            info.sector_zero_checksum
        );
    }

    fn read_disk_block(&self, logical_block_address: u64) {
        let Some(info) = self.system_info.storage.as_ref() else {
            shell_println!("AHCI disk: unavailable");
            return;
        };

        let block_size = info.logical_block_size as usize;
        let mut block = vec![0_u8; block_size];
        if let Err(error) = ahci::read_block(logical_block_address, &mut block) {
            shell_println!("disk read failed: {error}");
            return;
        }

        shell_println!(
            "disk block {} read successfully; showing first {} bytes:",
            logical_block_address,
            block.len().min(DISK_PREVIEW_BYTES)
        );
        for (line, bytes) in block[..block.len().min(DISK_PREVIEW_BYTES)]
            .chunks(16)
            .enumerate()
        {
            let byte_offset = logical_block_address
                .saturating_mul(block_size as u64)
                .saturating_add((line * 16) as u64);
            shell_print!("{byte_offset:#010x}: ");
            for byte in bytes {
                shell_print!("{byte:02x} ");
            }
            shell_println!();
        }
    }

    fn print_partitions(&self) {
        let Some(inventory) = self.system_info.partitions.as_ref() else {
            shell_println!("partitions: unavailable");
            return;
        };

        shell_println!(
            "partition table: {}, {} partition(s), disk={} blocks x {} bytes",
            inventory.table_kind,
            inventory.partitions().len(),
            inventory.disk_block_count,
            inventory.disk_block_size
        );
        if inventory.table_kind == partition::TableKind::Gpt {
            shell_println!(
                "GPT validation: header CRC={}, entry-array CRC={}, protective MBR={}",
                inventory.header_crc_valid,
                inventory.entry_array_crc_valid,
                inventory.protective_mbr
            );
        }
        for entry in inventory.partitions() {
            shell_println!(
                "{}: {} LBA {}-{} ({} blocks, {} KiB) bootable={}",
                entry.index,
                entry.kind,
                entry.start_lba,
                entry.end_lba_inclusive(),
                entry.block_count,
                entry
                    .block_count
                    .saturating_mul(inventory.disk_block_size as u64)
                    / 1024,
                entry.bootable
            );
            if !entry.name.is_empty() {
                shell_println!("  name: `{}`", entry.name);
            }
            if let Some(type_guid) = entry.type_guid {
                shell_println!("  type GUID: {type_guid}");
            }
            if let Some(unique_guid) = entry.unique_guid {
                shell_println!("  unique GUID: {unique_guid}");
            }
        }
        if inventory.truncated {
            shell_println!("warning: partition inventory reached its configured bound");
        }
    }

    fn print_filesystem(&self) {
        let Some(info) = self.system_info.filesystem.as_ref() else {
            shell_println!("filesystem: unavailable");
            return;
        };

        shell_println!(
            "filesystem: {} on partition {} at LBA {}",
            info.fat_type,
            info.partition_index,
            info.partition_start_lba
        );
        shell_println!(
            "volume: label=`{}`, id={:#010x}, sectors={}",
            info.volume_label,
            info.volume_id,
            info.total_sectors
        );
        shell_println!(
            "geometry: {} bytes/sector, {} sectors/cluster, {} bytes/cluster",
            info.bytes_per_sector,
            info.sectors_per_cluster,
            info.bytes_per_cluster
        );
        shell_println!(
            "FAT: copies={}, sectors/copy={}, clusters={}, root entries={}",
            info.fat_count,
            info.sectors_per_fat,
            info.cluster_count,
            info.root_entry_count
        );
        shell_println!("mount: read-only at /");
    }

    fn list_files(&self, path: &str) {
        let entries = match vfs::read_directory(path) {
            Ok(entries) => entries,
            Err(error) => {
                shell_println!("ls: {error}");
                return;
            }
        };

        shell_println!(
            "{}: {} entr{}",
            path,
            entries.len(),
            if entries.len() == 1 { "y" } else { "ies" }
        );
        for entry in entries {
            let kind = if entry.is_directory() { "d" } else { "-" };
            let suffix = if entry.is_directory() { "/" } else { "" };
            shell_println!(
                "{}{}{}{} {:>10} {}{}",
                kind,
                if entry.read_only { "r" } else { "-" },
                if entry.hidden { "h" } else { "-" },
                if entry.system { "s" } else { "-" },
                entry.size,
                entry.name,
                suffix
            );
        }
    }

    fn cat_file(&self, path: &str) {
        let data = match vfs::read_file(path, FILE_PREVIEW_BYTES) {
            Ok(data) => data,
            Err(error) => {
                shell_println!("cat: {error}");
                return;
            }
        };

        shell_println!(
            "{}: {} bytes{}",
            path,
            data.total_size,
            if data.truncated {
                " (preview truncated)"
            } else {
                ""
            }
        );
        let mut ended_with_newline = true;
        for byte in data.bytes {
            match byte {
                b'\n' => {
                    shell_println!();
                    ended_with_newline = true;
                }
                b'\r' => {}
                b'\t' => {
                    shell_print!("    ");
                    ended_with_newline = false;
                }
                0x20..=0x7e => {
                    shell_print!("{}", char::from(byte));
                    ended_with_newline = false;
                }
                _ => {
                    shell_print!(".");
                    ended_with_newline = false;
                }
            }
        }
        if !ended_with_newline {
            shell_println!();
        }
        if data.truncated {
            shell_println!("cat: output limited to {} bytes", FILE_PREVIEW_BYTES);
        }
    }

    fn print_vfs(&self) {
        let Some(info) = vfs::info() else {
            shell_println!("VFS: unavailable");
            return;
        };

        shell_println!(
            "VFS: {} mounted at {}, read-only={}",
            info.filesystem,
            info.mount_path,
            info.read_only
        );
        shell_println!(
            "backend: volume=`{}`, id={:#010x}, partition={}, start_lba={}",
            info.volume_label,
            info.volume_id,
            info.partition_index,
            info.partition_start_lba
        );
        shell_println!(
            "limits: path_bytes={}, path_components={}, read_window={} KiB",
            vfs::MAX_PATH_BYTES,
            vfs::MAX_PATH_COMPONENTS,
            vfs::MAX_READ_WINDOW_BYTES / 1024
        );
    }

    fn inspect_elf(&self, path: &str) {
        let image = match elf::validate(path) {
            Ok(image) => image,
            Err(error) => {
                shell_println!("elf: {error}");
                return;
            }
        };

        shell_println!("ELF64 x86-64 {}: `{}`", image.image_type, image.path);
        shell_println!(
            "entry={:#018x}, file={} bytes, program headers={}, LOAD segments={}",
            image.entry_point,
            image.file_size,
            image.program_header_count,
            image.load_segments().len()
        );
        shell_println!(
            "dynamic={}, TLS={}, executable stack requested={}",
            image.has_dynamic_segment,
            image.has_tls_segment,
            image.executable_stack_requested
        );
        for segment in image.load_segments() {
            shell_println!(
                "LOAD[{}] {} file={:#x}+{:#x} virtual={:#018x}+{:#x} align={:#x}",
                segment.program_header_index,
                segment.permissions(),
                segment.file_offset,
                segment.file_size,
                segment.virtual_address,
                segment.memory_size,
                segment.alignment
            );
        }
    }

    fn print_userspace(&self) {
        let Some(result) = userspace::last_result() else {
            shell_println!("userspace: no process has completed");
            return;
        };

        shell_println!(
            "userspace: `{}` exited with code {}",
            result.path,
            result.exit_code
        );
        shell_println!(
            "entry={:#018x}, page table={:#x}, mapped pages={}, LOAD segments={}",
            result.entry_point,
            result.page_table_address,
            result.mapped_pages,
            result.load_segments
        );
        shell_println!(
            "stacks: user={} KiB, guard={:#018x}, kernel={} KiB",
            result.user_stack_bytes / 1024,
            result.guard_page_address,
            result.kernel_stack_bytes / 1024
        );
        shell_println!(
            "syscalls: total={}, writes={}, yields={}, bytes written={}",
            result.syscall_count,
            result.write_count,
            result.yield_count,
            result.bytes_written
        );
    }

    fn print_interrupts(&self) {
        let info = self.system_info.interrupt_controller;
        shell_println!("interrupt controller: {}", info.kind);
        shell_println!(
            "vectors: timer={}, keyboard={}, spurious={}",
            info.timer_vector,
            info.keyboard_vector,
            interrupts::SPURIOUS_VECTOR
        );
        shell_println!(
            "timer: source={}, frequency={} Hz, ticks={}, spurious interrupts={}",
            info.timer_source,
            interrupts::TIMER_HZ,
            interrupts::timer_ticks(),
            interrupts::spurious_interrupts()
        );

        if info.timer_source == interrupts::TimerSource::LocalApic {
            shell_println!(
                "local APIC timer: ticks/s={}, initial count={}, divisor={}",
                info.local_apic_timer_ticks_per_second.unwrap_or(0),
                info.local_apic_timer_initial_count.unwrap_or(0),
                info.local_apic_timer_divisor.unwrap_or(0)
            );
            shell_println!(
                "HPET calibration: frequency={} Hz, period={} fs, 64-bit={}",
                info.hpet_frequency_hz.unwrap_or(0),
                info.hpet_period_femtoseconds.unwrap_or(0),
                info.hpet_counter_is_64_bit.unwrap_or(false)
            );
        } else if let Some(reason) = info.timer_fallback_reason {
            shell_println!("local APIC timer fallback: {reason}");
        }

        match info.kind {
            interrupts::ControllerKind::Apic => {
                shell_println!(
                    "local APIC: id={}, version={:#x}, address={:#x}",
                    info.local_apic_id.unwrap_or(0),
                    info.local_apic_version.unwrap_or(0),
                    info.local_apic_address.unwrap_or(0)
                );
                shell_println!(
                    "I/O APIC: id={}, address={:#x}, redirection entries={}",
                    info.io_apic_id.unwrap_or(0),
                    info.io_apic_address.unwrap_or(0),
                    info.io_apic_redirection_entries.unwrap_or(0)
                );
                match info.timer_source {
                    interrupts::TimerSource::LocalApic => {
                        shell_println!(
                            "routes: local APIC timer, keyboard IRQ1 -> GSI {}",
                            info.keyboard_gsi.unwrap_or(0)
                        );
                    }
                    interrupts::TimerSource::Pit => {
                        shell_println!(
                            "routes: PIT IRQ0 -> GSI {}, keyboard IRQ1 -> GSI {}",
                            info.timer_gsi.unwrap_or(0),
                            info.keyboard_gsi.unwrap_or(0)
                        );
                    }
                }
            }
            interrupts::ControllerKind::Pic => {
                shell_println!(
                    "APIC fallback reason: {}",
                    info.fallback_reason.unwrap_or("not recorded")
                );
            }
        }
    }

    fn print_pci(&self) {
        let Some(info) = self.system_info.pci_inventory.as_ref() else {
            shell_println!("PCIe: unavailable");
            return;
        };

        shell_println!(
            "PCIe: regions={}/{}, buses={}, functions={} ({} recorded)",
            info.scanned_region_count,
            info.declared_region_count,
            info.scanned_bus_count,
            info.total_function_count,
            info.recorded_function_count()
        );
        shell_println!(
            "classes: storage={}, network={}, display={}, bridges={}",
            info.class_count(0x01),
            info.class_count(0x02),
            info.class_count(0x03),
            info.bridge_count()
        );

        for function in info.functions().iter().take(MAX_PCI_SHELL_FUNCTIONS) {
            shell_println!(
                "{} {:04x}:{:04x} {:02x}:{:02x}:{:02x} {}",
                function.location,
                function.vendor_id,
                function.device_id,
                function.class_code,
                function.subclass,
                function.programming_interface,
                function.class_description()
            );
            shell_println!(
                "  header={}, revision={:02x}, multifunction={}, IRQ line={}, pin={}",
                function.header_kind,
                function.revision_id,
                function.multifunction,
                function.interrupt_line,
                function.interrupt_pin
            );
            if let Some(subsystem) = function.subsystem {
                shell_println!(
                    "  subsystem={:04x}:{:04x}",
                    subsystem.vendor_id,
                    subsystem.device_id
                );
            }
            if let Some(buses) = function.bridge_buses {
                shell_println!(
                    "  bridge buses: primary={}, secondary={}, subordinate={}",
                    buses.primary,
                    buses.secondary,
                    buses.subordinate
                );
            }
        }

        let displayed = info.recorded_function_count().min(MAX_PCI_SHELL_FUNCTIONS);
        if displayed < info.recorded_function_count() {
            shell_println!(
                "{} additional recorded functions omitted",
                info.recorded_function_count() - displayed
            );
        }
        if info.unscanned_region_count > 0 {
            shell_println!(
                "warning: {} additional MCFG region(s) were not scanned",
                info.unscanned_region_count
            );
        }
        if info.bus_scan_truncated {
            shell_println!("warning: ECAM bus scan reached its configured bound");
        }
        if info.function_list_truncated {
            shell_println!("warning: PCIe function recording reached its configured bound");
        }
    }

    fn print_acpi(&self) {
        let Some(info) = self.system_info.acpi else {
            shell_println!("ACPI: unavailable");
            return;
        };

        shell_println!(
            "ACPI: RSDP revision {}, OEM `{}`, root {} at {:#x}",
            info.revision,
            info.oem_id(),
            info.root_table_kind,
            info.root_table_address
        );
        shell_println!(
            "tables: {} total, {} valid, {} invalid",
            info.total_table_count,
            info.valid_table_count,
            info.invalid_table_count
        );

        shell_print!("signatures:");
        for signature in info.table_signatures() {
            shell_print!(" {signature}");
        }
        shell_println!();

        if let Some(madt) = info.madt {
            shell_println!(
                "MADT: LAPIC={:#x}, CPUs={} ({} enabled, {} online-capable)",
                madt.local_apic_address,
                madt.processor_count,
                madt.enabled_processor_count,
                madt.online_capable_processor_count
            );
            shell_println!(
                "MADT: IO APICs={}, IRQ overrides={}, legacy PIC={}",
                madt.io_apic_count,
                madt.interrupt_override_count,
                madt.supports_legacy_pic
            );
            shell_println!(
                "IRQ0: GSI={}, {}, {}, override={}",
                madt.timer_route.global_system_interrupt,
                madt.timer_route.polarity,
                madt.timer_route.trigger_mode,
                madt.timer_route.overridden
            );
            shell_println!(
                "IRQ1: GSI={}, {}, {}, override={}",
                madt.keyboard_route.global_system_interrupt,
                madt.keyboard_route.polarity,
                madt.keyboard_route.trigger_mode,
                madt.keyboard_route.overridden
            );
            if let Some(io_apic) = madt.first_io_apic {
                shell_println!(
                    "IO APIC: id={}, address={:#x}, GSI base={}",
                    io_apic.id,
                    io_apic.address,
                    io_apic.global_system_interrupt_base
                );
            }
            if madt.malformed_entry_count > 0 {
                shell_println!(
                    "MADT warning: {} malformed entries",
                    madt.malformed_entry_count
                );
            }
        } else {
            shell_println!("MADT: not present");
        }

        if let Some(fadt) = info.fadt {
            shell_println!(
                "FADT: revision={}, profile={}, SCI={}",
                fadt.revision,
                fadt.preferred_power_profile,
                fadt.sci_interrupt
            );
        } else {
            shell_println!("FADT: not present");
        }

        if let Some(hpet) = info.hpet {
            shell_println!(
                "HPET: base={:#x}, address-space={}, comparators={}, 64-bit={}, legacy IRQ={}",
                hpet.base_address,
                hpet.address_space,
                hpet.comparator_count,
                hpet.counter_is_64_bit,
                hpet.legacy_irq_capable
            );
            shell_println!(
                "HPET: number={}, minimum tick={}",
                hpet.hpet_number,
                hpet.minimum_tick
            );
        } else {
            shell_println!("HPET: not present");
        }

        if let Some(mcfg) = info.mcfg {
            shell_println!("MCFG: {} PCIe configuration region(s)", mcfg.region_count);
            if let Some(region) = mcfg.first_region {
                shell_println!(
                    "MCFG[0]: base={:#x}, segment={}, buses={}-{}",
                    region.base_address,
                    region.segment_group,
                    region.start_bus,
                    region.end_bus
                );
            }
        } else {
            shell_println!("MCFG: not present");
        }
    }

    fn print_prompt(&self) {
        shell_print!("{PROMPT}");
    }
}

fn parse_u64(value: &str) -> Option<u64> {
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map(|digits| u64::from_str_radix(digits, 16).ok())
        .unwrap_or_else(|| value.parse().ok())
}

fn print_help() {
    shell_println!("commands:");
    shell_println!("  help             show this command list");
    shell_println!("  clear            clear the framebuffer console");
    shell_println!("  echo <text>      print text");
    shell_println!("  uptime           show timer uptime");
    shell_println!("  memory           show physical frame statistics");
    shell_println!("  heap             show the kernel heap mapping");
    shell_println!("  acpi             show ACPI table and platform data");
    shell_println!("  interrupts       show interrupt-controller routes");
    shell_println!("  pci              list PCIe functions discovered by ECAM");
    shell_println!("  disk             show the AHCI disk");
    shell_println!("  disk read <lba>  read and preview one logical block");
    shell_println!("  partitions       list MBR/GPT partitions");
    shell_println!("  fs               show the mounted FAT volume");
    shell_println!("  vfs              show the root VFS mount");
    shell_println!("  ls [path]        list a VFS directory");
    shell_println!("  cat <path>       preview a VFS file");
    shell_println!("  elf <path>       validate an ELF64 executable");
    shell_println!("  process          show the completed ring-3 process");
    shell_println!("  about            describe GalacticOS");
    shell_println!("  halt             halt the CPU");
}
