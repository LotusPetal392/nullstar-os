use alloc::string::String;

use pc_keyboard::{DecodedKey, KeyCode};

use crate::{acpi, allocator, console, interrupts, memory};

const PROMPT: &str = "galactic> ";
const DEFAULT_CONSOLE_COLUMNS: usize = 80;
const MAX_COMMAND_LENGTH: usize = 128;

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

#[derive(Debug, Clone, Copy)]
pub struct SystemInfo {
    usable_frames: u64,
    allocated_frames: u64,
    remaining_frames: u64,
    acpi: Option<acpi::AcpiInfo>,
}

impl SystemInfo {
    pub const fn new(
        usable_frames: u64,
        allocated_frames: u64,
        remaining_frames: u64,
        acpi: Option<acpi::AcpiInfo>,
    ) -> Self {
        Self {
            usable_frames,
            allocated_frames,
            remaining_frames,
            acpi,
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

fn print_help() {
    shell_println!("commands:");
    shell_println!("  help             show this command list");
    shell_println!("  clear            clear the framebuffer console");
    shell_println!("  echo <text>      print text");
    shell_println!("  uptime           show timer uptime");
    shell_println!("  memory           show physical frame statistics");
    shell_println!("  heap             show the kernel heap mapping");
    shell_println!("  acpi             show ACPI table and platform data");
    shell_println!("  about            describe GalacticOS");
    shell_println!("  halt             halt the CPU");
}
