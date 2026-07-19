use alloc::{collections::VecDeque, string::String, vec::Vec};

use pc_keyboard::{DecodedKey, KeyCode};
use spin::Mutex;

use crate::{console, keyboard};

const MAX_EDIT_BYTES: usize = 512;
const MAX_COMMITTED_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    pub foreground_process: Option<u64>,
    pub editing_bytes: usize,
    pub committed_bytes: usize,
    pub keys_received: u64,
    pub lines_committed: u64,
    pub bytes_committed: u64,
    pub dropped_bytes: u64,
    pub blocked_reads: u64,
    pub wakeups: u64,
    pub injected_lines: u64,
    pub interrupts: u64,
    pub suspends: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlSignal {
    Interrupt,
    Suspend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlEvent {
    pub foreground_process: u64,
    pub signal: ControlSignal,
}

struct Terminal {
    foreground_process: Option<u64>,
    editing: String,
    committed: VecDeque<u8>,
    keys_received: u64,
    lines_committed: u64,
    bytes_committed: u64,
    dropped_bytes: u64,
    blocked_reads: u64,
    wakeups: u64,
    injected_lines: u64,
    pending_control: Option<ControlSignal>,
    interrupts: u64,
    suspends: u64,
}

impl Terminal {
    const fn new() -> Self {
        Self {
            foreground_process: None,
            editing: String::new(),
            committed: VecDeque::new(),
            keys_received: 0,
            lines_committed: 0,
            bytes_committed: 0,
            dropped_bytes: 0,
            blocked_reads: 0,
            wakeups: 0,
            injected_lines: 0,
            pending_control: None,
            interrupts: 0,
            suspends: 0,
        }
    }

    fn attach(&mut self, process_id: u64) -> bool {
        if self
            .foreground_process
            .is_some_and(|foreground| foreground != process_id)
        {
            return false;
        }
        self.foreground_process = Some(process_id);
        self.editing.clear();
        self.committed.clear();
        self.pending_control = None;
        true
    }

    fn detach(&mut self, process_id: u64) {
        if self.foreground_process == Some(process_id) {
            self.foreground_process = None;
            self.editing.clear();
            self.committed.clear();
            self.pending_control = None;
        }
    }

    fn transfer(&mut self, current_process: u64, next_process: u64) -> bool {
        if self.foreground_process != Some(current_process) {
            return false;
        }
        self.foreground_process = Some(next_process);
        self.editing.clear();
        self.committed.clear();
        self.pending_control = None;
        true
    }

    fn handle_key(&mut self, key: DecodedKey) {
        self.keys_received = self.keys_received.saturating_add(1);
        match key {
            DecodedKey::Unicode('\u{3}') => self.control(ControlSignal::Interrupt, true),
            DecodedKey::Unicode('\u{1a}') => self.control(ControlSignal::Suspend, true),
            DecodedKey::Unicode('\n' | '\r') => self.commit_editing(true),
            DecodedKey::Unicode('\u{8}' | '\u{7f}') => self.backspace(),
            DecodedKey::Unicode('\t') => self.push_character('\t'),
            DecodedKey::Unicode(character) if character.is_control() => {}
            DecodedKey::Unicode(character) => self.push_character(character),
            DecodedKey::RawKey(KeyCode::Return | KeyCode::NumpadEnter) => self.commit_editing(true),
            DecodedKey::RawKey(KeyCode::Backspace | KeyCode::Delete) => self.backspace(),
            DecodedKey::RawKey(_) => {}
        }
    }

    fn control(&mut self, signal: ControlSignal, echo: bool) {
        if self.foreground_process.is_none() {
            return;
        }
        self.editing.clear();
        self.committed.clear();
        self.committed.push_back(b'\n');
        self.lines_committed = self.lines_committed.saturating_add(1);
        self.bytes_committed = self.bytes_committed.saturating_add(1);
        self.pending_control = Some(signal);
        match signal {
            ControlSignal::Interrupt => self.interrupts = self.interrupts.saturating_add(1),
            ControlSignal::Suspend => self.suspends = self.suspends.saturating_add(1),
        }
        if echo {
            match signal {
                ControlSignal::Interrupt => {
                    crate::print!("^C");
                    crate::serial_print!("^C");
                }
                ControlSignal::Suspend => {
                    crate::print!("^Z");
                    crate::serial_print!("^Z");
                }
            }
            crate::println!();
            crate::serial_println!();
        }
    }

    fn take_control(&mut self) -> Option<ControlEvent> {
        let signal = self.pending_control.take()?;
        Some(ControlEvent {
            foreground_process: self.foreground_process?,
            signal,
        })
    }

    fn push_character(&mut self, character: char) {
        let byte_length = character.len_utf8();
        if self.editing.len().saturating_add(byte_length) > MAX_EDIT_BYTES {
            self.dropped_bytes = self.dropped_bytes.saturating_add(byte_length as u64);
            return;
        }
        self.editing.push(character);
        crate::print!("{character}");
        crate::serial_print!("{character}");
    }

    fn backspace(&mut self) {
        if self.editing.pop().is_none() {
            return;
        }
        console::backspace();
        crate::serial_print!("\u{8} \u{8}");
    }

    fn commit_editing(&mut self, echo: bool) {
        let required = self.editing.len().saturating_add(1);
        if self.committed.len().saturating_add(required) <= MAX_COMMITTED_BYTES {
            self.committed.extend(self.editing.bytes());
            self.committed.push_back(b'\n');
            self.lines_committed = self.lines_committed.saturating_add(1);
            self.bytes_committed = self.bytes_committed.saturating_add(required as u64);
        } else {
            self.dropped_bytes = self.dropped_bytes.saturating_add(required as u64);
        }
        self.editing.clear();
        if echo {
            crate::println!();
            crate::serial_println!();
        }
    }

    fn inject_line(&mut self, line: &str) {
        for character in line.chars() {
            if character == '\n' || character == '\r' {
                break;
            }
            let byte_length = character.len_utf8();
            if self.editing.len().saturating_add(byte_length) > MAX_EDIT_BYTES {
                self.dropped_bytes = self.dropped_bytes.saturating_add(byte_length as u64);
                break;
            }
            self.editing.push(character);
        }
        self.injected_lines = self.injected_lines.saturating_add(1);
        self.commit_editing(false);
    }

    fn take_committed(&mut self, maximum: usize) -> Option<Vec<u8>> {
        if maximum == 0 || self.committed.is_empty() {
            return None;
        }
        let count = maximum.min(self.committed.len());
        let mut bytes = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(byte) = self.committed.pop_front() {
                bytes.push(byte);
            }
        }
        Some(bytes)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            foreground_process: self.foreground_process,
            editing_bytes: self.editing.len(),
            committed_bytes: self.committed.len(),
            keys_received: self.keys_received,
            lines_committed: self.lines_committed,
            bytes_committed: self.bytes_committed,
            dropped_bytes: self.dropped_bytes,
            blocked_reads: self.blocked_reads,
            wakeups: self.wakeups,
            injected_lines: self.injected_lines,
            interrupts: self.interrupts,
            suspends: self.suspends,
        }
    }
}

static TERMINAL: Mutex<Terminal> = Mutex::new(Terminal::new());

pub fn attach(process_id: u64) -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| TERMINAL.lock().attach(process_id))
}

pub fn detach(process_id: u64) {
    x86_64::instructions::interrupts::without_interrupts(|| TERMINAL.lock().detach(process_id));
}

pub fn transfer(current_process: u64, next_process: u64) -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        TERMINAL.lock().transfer(current_process, next_process)
    })
}

pub fn is_foreground(process_id: u64) -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        TERMINAL.lock().foreground_process == Some(process_id)
    })
}

pub fn foreground_process() -> Option<u64> {
    x86_64::instructions::interrupts::without_interrupts(|| TERMINAL.lock().foreground_process)
}

pub fn handle_key(key: DecodedKey) -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut terminal = TERMINAL.lock();
        if terminal.foreground_process.is_none() {
            return false;
        }
        terminal.handle_key(key);
        true
    })
}

pub fn poll_keyboard() -> usize {
    if foreground_process().is_none() {
        return 0;
    }
    let mut handled = 0usize;
    while let Some(key) = keyboard::poll_key() {
        if !handle_key(key) {
            break;
        }
        handled = handled.saturating_add(1);
    }
    handled
}

pub fn inject_line(line: &str) {
    x86_64::instructions::interrupts::without_interrupts(|| TERMINAL.lock().inject_line(line));
}

pub fn take_control() -> Option<ControlEvent> {
    x86_64::instructions::interrupts::without_interrupts(|| TERMINAL.lock().take_control())
}

pub fn take_committed(maximum: usize) -> Option<Vec<u8>> {
    x86_64::instructions::interrupts::without_interrupts(|| TERMINAL.lock().take_committed(maximum))
}

pub fn note_blocked_read() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut terminal = TERMINAL.lock();
        terminal.blocked_reads = terminal.blocked_reads.saturating_add(1);
    });
}

pub fn note_wakeup() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut terminal = TERMINAL.lock();
        terminal.wakeups = terminal.wakeups.saturating_add(1);
    });
}

pub fn snapshot() -> Snapshot {
    x86_64::instructions::interrupts::without_interrupts(|| TERMINAL.lock().snapshot())
}
