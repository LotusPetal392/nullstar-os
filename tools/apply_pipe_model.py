from pipe_patch_common import replace_once

# Register the new pipe module.
replace_once(
    "kernel/src/process/mod.rs",
    "pub(crate) mod elf;\nmod terminal;\npub(crate) mod userspace;\n",
    "pub(crate) mod elf;\nmod pipe;\nmod terminal;\npub(crate) mod userspace;\n",
)

# Fix borrow scopes in the standalone pipe manager before compiling it.
replace_once(
    "kernel/src/process/pipe.rs",
    '''    fn read(&mut self, pipe_id: PipeId, maximum: usize) -> Result<ReadOutcome, Error> {
        let pipe = self.pipe_mut(pipe_id)?;
        pipe.read_calls = pipe.read_calls.saturating_add(1);
        self.total_read_calls = self.total_read_calls.saturating_add(1);

        if !pipe.buffer.is_empty() {
            let count = maximum.min(pipe.buffer.len());
            let mut bytes = Vec::with_capacity(count);
            for _ in 0..count {
                if let Some(byte) = pipe.buffer.pop_front() {
                    bytes.push(byte);
                }
            }
            pipe.bytes_read = pipe.bytes_read.saturating_add(bytes.len() as u64);
            self.total_bytes_read = self.total_bytes_read.saturating_add(bytes.len() as u64);
            return Ok(ReadOutcome::Data(bytes));
        }

        if pipe.writers == 0 {
            Ok(ReadOutcome::EndOfFile)
        } else {
            Ok(ReadOutcome::Empty)
        }
    }

    fn write(&mut self, pipe_id: PipeId, bytes: &[u8]) -> Result<WriteOutcome, Error> {
        let pipe = self.pipe_mut(pipe_id)?;
        pipe.write_calls = pipe.write_calls.saturating_add(1);
        self.total_write_calls = self.total_write_calls.saturating_add(1);

        if pipe.readers == 0 {
            return Ok(WriteOutcome::NoReaders);
        }
        let available = PIPE_CAPACITY_BYTES.saturating_sub(pipe.buffer.len());
        if available == 0 {
            return Ok(WriteOutcome::Full);
        }
        let count = available.min(bytes.len());
        pipe.buffer.extend(bytes[..count].iter().copied());
        pipe.bytes_written = pipe.bytes_written.saturating_add(count as u64);
        self.total_bytes_written = self.total_bytes_written.saturating_add(count as u64);
        Ok(WriteOutcome::Written(count))
    }

    fn note_blocked_read(&mut self, pipe_id: PipeId) -> Result<(), Error> {
        let pipe = self.pipe_mut(pipe_id)?;
        pipe.blocked_reads = pipe.blocked_reads.saturating_add(1);
        self.total_blocked_reads = self.total_blocked_reads.saturating_add(1);
        Ok(())
    }

    fn note_blocked_write(&mut self, pipe_id: PipeId) -> Result<(), Error> {
        let pipe = self.pipe_mut(pipe_id)?;
        pipe.blocked_writes = pipe.blocked_writes.saturating_add(1);
        self.total_blocked_writes = self.total_blocked_writes.saturating_add(1);
        Ok(())
    }

    fn note_reader_wakeup(&mut self, pipe_id: PipeId) -> Result<(), Error> {
        let pipe = self.pipe_mut(pipe_id)?;
        pipe.reader_wakeups = pipe.reader_wakeups.saturating_add(1);
        self.total_reader_wakeups = self.total_reader_wakeups.saturating_add(1);
        Ok(())
    }

    fn note_writer_wakeup(&mut self, pipe_id: PipeId) -> Result<(), Error> {
        let pipe = self.pipe_mut(pipe_id)?;
        pipe.writer_wakeups = pipe.writer_wakeups.saturating_add(1);
        self.total_writer_wakeups = self.total_writer_wakeups.saturating_add(1);
        Ok(())
    }
''',
    '''    fn pipe_index(&self, pipe_id: PipeId) -> Result<usize, Error> {
        self.pipes
            .iter()
            .position(|pipe| pipe.id == pipe_id)
            .ok_or(Error::NotFound(pipe_id))
    }

    fn read(&mut self, pipe_id: PipeId, maximum: usize) -> Result<ReadOutcome, Error> {
        let index = self.pipe_index(pipe_id)?;
        self.total_read_calls = self.total_read_calls.saturating_add(1);
        let pipe = &mut self.pipes[index];
        pipe.read_calls = pipe.read_calls.saturating_add(1);

        if !pipe.buffer.is_empty() {
            let count = maximum.min(pipe.buffer.len());
            let mut bytes = Vec::with_capacity(count);
            for _ in 0..count {
                if let Some(byte) = pipe.buffer.pop_front() {
                    bytes.push(byte);
                }
            }
            pipe.bytes_read = pipe.bytes_read.saturating_add(bytes.len() as u64);
            self.total_bytes_read = self.total_bytes_read.saturating_add(bytes.len() as u64);
            return Ok(ReadOutcome::Data(bytes));
        }

        if pipe.writers == 0 {
            Ok(ReadOutcome::EndOfFile)
        } else {
            Ok(ReadOutcome::Empty)
        }
    }

    fn write(&mut self, pipe_id: PipeId, bytes: &[u8]) -> Result<WriteOutcome, Error> {
        let index = self.pipe_index(pipe_id)?;
        self.total_write_calls = self.total_write_calls.saturating_add(1);
        let pipe = &mut self.pipes[index];
        pipe.write_calls = pipe.write_calls.saturating_add(1);

        if pipe.readers == 0 {
            return Ok(WriteOutcome::NoReaders);
        }
        let available = PIPE_CAPACITY_BYTES.saturating_sub(pipe.buffer.len());
        if available == 0 {
            return Ok(WriteOutcome::Full);
        }
        let count = available.min(bytes.len());
        pipe.buffer.extend(bytes[..count].iter().copied());
        pipe.bytes_written = pipe.bytes_written.saturating_add(count as u64);
        self.total_bytes_written = self.total_bytes_written.saturating_add(count as u64);
        Ok(WriteOutcome::Written(count))
    }

    fn note_blocked_read(&mut self, pipe_id: PipeId) -> Result<(), Error> {
        let index = self.pipe_index(pipe_id)?;
        self.total_blocked_reads = self.total_blocked_reads.saturating_add(1);
        let pipe = &mut self.pipes[index];
        pipe.blocked_reads = pipe.blocked_reads.saturating_add(1);
        Ok(())
    }

    fn note_blocked_write(&mut self, pipe_id: PipeId) -> Result<(), Error> {
        let index = self.pipe_index(pipe_id)?;
        self.total_blocked_writes = self.total_blocked_writes.saturating_add(1);
        let pipe = &mut self.pipes[index];
        pipe.blocked_writes = pipe.blocked_writes.saturating_add(1);
        Ok(())
    }

    fn note_reader_wakeup(&mut self, pipe_id: PipeId) -> Result<(), Error> {
        let index = self.pipe_index(pipe_id)?;
        self.total_reader_wakeups = self.total_reader_wakeups.saturating_add(1);
        let pipe = &mut self.pipes[index];
        pipe.reader_wakeups = pipe.reader_wakeups.saturating_add(1);
        Ok(())
    }

    fn note_writer_wakeup(&mut self, pipe_id: PipeId) -> Result<(), Error> {
        let index = self.pipe_index(pipe_id)?;
        self.total_writer_wakeups = self.total_writer_wakeups.saturating_add(1);
        let pipe = &mut self.pipes[index];
        pipe.writer_wakeups = pipe.writer_wakeups.saturating_add(1);
        Ok(())
    }
''',
)

# Userspace process model and pipe-aware standard streams.
replace_once(
    "kernel/src/process/userspace.rs",
    '''use super::{
    elf::{self, Image, ImageType, LoadSegment},
    terminal,
};

pub use super::terminal::Snapshot as TerminalSnapshot;
''',
    '''use super::{
    elf::{self, Image, ImageType, LoadSegment},
    pipe::{self, PipeId},
    terminal,
};

pub use super::{pipe::Snapshot as PipeSnapshot, terminal::Snapshot as TerminalSnapshot};
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    "const ERR_TOO_MANY_OPEN_FILES: i64 = -24;\nconst ERR_NOT_IMPLEMENTED: i64 = -38;",
    "const ERR_TOO_MANY_OPEN_FILES: i64 = -24;\nconst ERR_BROKEN_PIPE: i64 = -32;\nconst ERR_NOT_IMPLEMENTED: i64 = -38;",
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''    pub terminal_read_count: u64,
    pub terminal_bytes_read: u64,
    pub blocked_read_count: u64,
    pub scheduled_count: u64,
''',
    '''    pub terminal_read_count: u64,
    pub terminal_bytes_read: u64,
    pub blocked_read_count: u64,
    pub pipe_read_count: u64,
    pub pipe_write_count: u64,
    pub pipe_bytes_read: u64,
    pub pipe_bytes_written: u64,
    pub blocked_pipe_read_count: u64,
    pub blocked_pipe_write_count: u64,
    pub scheduled_count: u64,
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    "    TerminalBusy,\n    ProcessNotFound(u64),",
    "    TerminalBusy,\n    Pipe(pipe::Error),\n    ProcessNotFound(u64),",
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''            Self::TerminalBusy => "another userspace process owns the terminal",
            Self::ProcessNotFound(_) => "userspace process bookkeeping is missing",
''',
    '''            Self::TerminalBusy => "another userspace process owns the terminal",
            Self::Pipe(_) => "kernel pipe operation failed",
            Self::ProcessNotFound(_) => "userspace process bookkeeping is missing",
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''            Self::Scheduler(error) => formatter.write_str(error.description()),
            Self::Elf(error) => write!(formatter, "ELF error: {error}"),
            Self::Vfs(error) => write!(formatter, "VFS error: {error}"),
''',
    '''            Self::Scheduler(error) => formatter.write_str(error.description()),
            Self::Elf(error) => write!(formatter, "ELF error: {error}"),
            Self::Pipe(error) => write!(formatter, "pipe error: {error}"),
            Self::Vfs(error) => write!(formatter, "VFS error: {error}"),
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''impl From<elf::Error> for Error {
    fn from(error: elf::Error) -> Self {
        Self::Elf(error)
    }
}

impl From<scheduler::InitError> for Error {
''',
    '''impl From<elf::Error> for Error {
    fn from(error: elf::Error) -> Self {
        Self::Elf(error)
    }
}

impl From<pipe::Error> for Error {
    fn from(error: pipe::Error) -> Self {
        Self::Pipe(error)
    }
}

impl From<scheduler::InitError> for Error {
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''struct PendingTerminalRead {
    address: u64,
    length: usize,
    stack_pointer: usize,
}

#[derive(Debug, Clone)]
struct OpenFile {
''',
    '''struct PendingTerminalRead {
    address: u64,
    length: usize,
    stack_pointer: usize,
}

#[derive(Debug, Clone, Copy)]
struct PendingPipeRead {
    pipe_id: PipeId,
    address: u64,
    length: usize,
    stack_pointer: usize,
}

#[derive(Debug, Clone, Copy)]
struct PendingPipeWrite {
    pipe_id: PipeId,
    address: u64,
    length: usize,
    stack_pointer: usize,
}

#[derive(Debug, Clone)]
struct OpenFile {
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''    owned_frames: Vec<PhysFrame<Size4KiB>>,
    open_files: Vec<OpenFile>,
    pending_terminal_read: Option<PendingTerminalRead>,
    syscall_count: u64,
''',
    '''    owned_frames: Vec<PhysFrame<Size4KiB>>,
    open_files: Vec<OpenFile>,
    stdin_pipe: Option<PipeId>,
    stdout_pipe: Option<PipeId>,
    pending_terminal_read: Option<PendingTerminalRead>,
    pending_pipe_read: Option<PendingPipeRead>,
    pending_pipe_write: Option<PendingPipeWrite>,
    syscall_count: u64,
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''    terminal_read_count: u64,
    terminal_bytes_read: u64,
    blocked_read_count: u64,
}
''',
    '''    terminal_read_count: u64,
    terminal_bytes_read: u64,
    blocked_read_count: u64,
    pipe_read_count: u64,
    pipe_write_count: u64,
    pipe_bytes_read: u64,
    pipe_bytes_written: u64,
    blocked_pipe_read_count: u64,
    blocked_pipe_write_count: u64,
}
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''            terminal_read_count: self.terminal_read_count,
            terminal_bytes_read: self.terminal_bytes_read,
            blocked_read_count: self.blocked_read_count,
            scheduled_count,
''',
    '''            terminal_read_count: self.terminal_read_count,
            terminal_bytes_read: self.terminal_bytes_read,
            blocked_read_count: self.blocked_read_count,
            pipe_read_count: self.pipe_read_count,
            pipe_write_count: self.pipe_write_count,
            pipe_bytes_read: self.pipe_bytes_read,
            pipe_bytes_written: self.pipe_bytes_written,
            blocked_pipe_read_count: self.blocked_pipe_read_count,
            blocked_pipe_write_count: self.blocked_pipe_write_count,
            scheduled_count,
''',
)

# Thread optional pipe endpoints through process creation.
replace_once(
    "kernel/src/process/userspace.rs",
    '''        arguments,
        false,
        kernel_mapper,
''',
    '''        arguments,
        false,
        None,
        None,
        kernel_mapper,
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''    arguments: &[&str],
    foreground: bool,
    kernel_mapper: &mut OffsetPageTable<'_>,
''',
    '''    arguments: &[&str],
    foreground: bool,
    stdin_pipe: Option<PipeId>,
    stdout_pipe: Option<PipeId>,
    kernel_mapper: &mut OffsetPageTable<'_>,
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''        owned_frames: core::mem::take(&mut address_space.owned_frames),
        open_files: Vec::new(),
        pending_terminal_read: None,
        syscall_count: 0,
''',
    '''        owned_frames: core::mem::take(&mut address_space.owned_frames),
        open_files: Vec::new(),
        stdin_pipe,
        stdout_pipe,
        pending_terminal_read: None,
        pending_pipe_read: None,
        pending_pipe_write: None,
        syscall_count: 0,
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''        terminal_read_count: 0,
        terminal_bytes_read: 0,
        blocked_read_count: 0,
    });
''',
    '''        terminal_read_count: 0,
        terminal_bytes_read: 0,
        blocked_read_count: 0,
        pipe_read_count: 0,
        pipe_write_count: 0,
        pipe_bytes_read: 0,
        pipe_bytes_written: 0,
        blocked_pipe_read_count: 0,
        blocked_pipe_write_count: 0,
    });
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''            &argv,
            foreground,
            &mut self.mapper,
''',
    '''            &argv,
            foreground,
            None,
            None,
            &mut self.mapper,
''',
)

# Add pipeline result and runtime API.
replace_once(
    "kernel/src/process/userspace.rs",
    '''pub struct Runtime {
    mapper: OffsetPageTable<'static>,
''',
    '''#[derive(Debug, Clone)]
pub struct PipelineResult {
    pub pipe_id: PipeId,
    pub producer: ProcessResult,
    pub consumer: ProcessResult,
}

pub struct Runtime {
    mapper: OffsetPageTable<'static>,
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''    pub fn wait(&mut self, process_id: u64) -> Result<ProcessResult, Error> {
''',
    '''    fn spawn_streams(
        &mut self,
        path: &str,
        arguments: &[&str],
        stdin_pipe: Option<PipeId>,
        stdout_pipe: Option<PipeId>,
    ) -> Result<SpawnInfo, Error> {
        let image = elf::validate(path)?;
        let mut argv = Vec::with_capacity(arguments.len().saturating_add(1));
        argv.push(path);
        argv.extend_from_slice(arguments);
        spawn_with_mode(
            path,
            SHELL_PROCESS_TASK_NAME,
            &image,
            &argv,
            false,
            stdin_pipe,
            stdout_pipe,
            &mut self.mapper,
            &mut self.frame_allocator,
            self.physical_memory_offset,
        )
    }

    pub fn pipeline(
        &mut self,
        producer_path: &str,
        producer_arguments: &[&str],
        consumer_path: &str,
        consumer_arguments: &[&str],
    ) -> Result<PipelineResult, Error> {
        let pipe_id = pipe::create_pair()?;
        let consumer = match self.spawn_streams(
            consumer_path,
            consumer_arguments,
            Some(pipe_id),
            None,
        ) {
            Ok(info) => info,
            Err(error) => {
                let _ = pipe::discard_pair(pipe_id);
                return Err(error);
            }
        };
        let producer = match self.spawn_streams(
            producer_path,
            producer_arguments,
            None,
            Some(pipe_id),
        ) {
            Ok(info) => info,
            Err(error) => {
                let _ = pipe::close_writer(pipe_id);
                let _ = self.wait(consumer.process_id);
                return Err(error);
            }
        };

        let producer_result = self.wait(producer.process_id)?;
        let consumer_result = self.wait(consumer.process_id)?;
        Ok(PipelineResult {
            pipe_id,
            producer: producer_result,
            consumer: consumer_result,
        })
    }

    pub fn wait(&mut self, process_id: u64) -> Result<ProcessResult, Error> {
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''    pub fn poll(&mut self) -> Result<usize, Error> {
        terminal::poll_keyboard();
        service_terminal_reads(self.physical_memory_offset)?;
        reap(&mut self.frame_allocator)
    }
''',
    '''    pub fn poll(&mut self) -> Result<usize, Error> {
        terminal::poll_keyboard();
        let reaped = reap(&mut self.frame_allocator)?;
        service_terminal_reads(self.physical_memory_offset)?;
        service_pipe_waiters(self.physical_memory_offset)?;
        Ok(reaped)
    }
''',
)
replace_once(
    "kernel/src/process/userspace.rs",
    '''    pub fn terminal_snapshot(&self) -> TerminalSnapshot {
        terminal::snapshot()
    }

    pub fn memory_stats(&self) -> MemoryStats {
''',
    '''    pub fn terminal_snapshot(&self) -> TerminalSnapshot {
        terminal::snapshot()
    }

    pub fn pipe_snapshot(&self) -> PipeSnapshot {
        pipe::snapshot()
    }

    pub fn memory_stats(&self) -> MemoryStats {
''',
)

# Close pipe endpoints while reaping a process.
replace_once(
    "kernel/src/process/userspace.rs",
    '''        terminal::detach(process_id);
        let frames_reclaimed = process.owned_frames.len();
''',
    '''        terminal::detach(process_id);
        if let Some(pipe_id) = process.stdin_pipe.take() {
            let _ = pipe::close_reader(pipe_id);
        }
        if let Some(pipe_id) = process.stdout_pipe.take() {
            let _ = pipe::close_writer(pipe_id);
        }
        let frames_reclaimed = process.owned_frames.len();
''',
)
