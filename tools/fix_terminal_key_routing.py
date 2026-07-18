from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    target = Path(path)
    source = target.read_text()
    if old not in source:
        raise SystemExit(f"{path}: patch target was not found")
    target.write_text(source.replace(old, new, 1))


replace_once(
    "kernel/src/process/terminal.rs",
    '''pub fn poll_keyboard() -> usize {
    if foreground_process().is_none() {
        return 0;
    }
    let mut handled = 0usize;
    while let Some(key) = keyboard::poll_key() {
        x86_64::instructions::interrupts::without_interrupts(|| TERMINAL.lock().handle_key(key));
        handled = handled.saturating_add(1);
    }
    handled
}
''',
    '''pub fn handle_key(key: DecodedKey) -> bool {
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
''',
)

replace_once(
    "kernel/src/process/userspace.rs",
    '''    pub fn poll(&mut self) -> Result<usize, Error> {
        terminal::poll_keyboard();
        service_terminal_reads(self.physical_memory_offset)?;
        reap(&mut self.frame_allocator)
    }

    pub fn reap(&mut self) -> Result<usize, Error> {
''',
    '''    pub fn poll(&mut self) -> Result<usize, Error> {
        terminal::poll_keyboard();
        service_terminal_reads(self.physical_memory_offset)?;
        reap(&mut self.frame_allocator)
    }

    pub fn terminal_active(&self) -> bool {
        terminal::foreground_process().is_some()
    }

    pub fn handle_terminal_key(
        &mut self,
        key: pc_keyboard::DecodedKey,
    ) -> Result<bool, Error> {
        let handled = terminal::handle_key(key);
        if handled {
            service_terminal_reads(self.physical_memory_offset)?;
        }
        Ok(handled)
    }

    pub fn reap(&mut self) -> Result<usize, Error> {
''',
)

replace_once(
    "kernel/src/shell.rs",
    '''    pub fn handle_key(&mut self, key: DecodedKey) -> ShellAction {
        match key {
            DecodedKey::Unicode(character) => self.handle_character(character),
            DecodedKey::RawKey(key_code) => self.handle_raw_key(key_code),
        }
    }
''',
    '''    pub fn handle_key(&mut self, key: DecodedKey) -> ShellAction {
        if self.runtime.terminal_active() {
            if let Err(error) = self.runtime.handle_terminal_key(key) {
                crate::serial_println!("terminal key routing failed: {error}");
            }
            return ShellAction::Continue;
        }

        match key {
            DecodedKey::Unicode(character) => self.handle_character(character),
            DecodedKey::RawKey(key_code) => self.handle_raw_key(key_code),
        }
    }
''',
)

print("foreground terminal key routing patched")
