use alloc::string::String;

use pc_keyboard::{DecodedKey, KeyCode};

use crate::{allocator, console, interrupts, memory};

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
}

impl SystemInfo {
    pub const fn new(usable_frames: u64, allocated_frames: u64, remaining_frames: u64) -> Self {
        Self {
            usable_frames,
            allocated_frames,
            remaining_frames,
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
    shell_println!("  about            describe GalacticOS");
    shell_println!("  halt             halt the CPU");
}
