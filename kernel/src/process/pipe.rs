use alloc::{collections::VecDeque, vec::Vec};
use core::fmt;

use spin::Mutex;

pub type PipeId = u64;

pub const PIPE_CAPACITY_BYTES: usize = 4096;
pub const MAX_PIPES: usize = 32;

static PIPE_MANAGER: Mutex<PipeManager> = Mutex::new(PipeManager::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    TooManyPipes,
    NotFound(PipeId),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyPipes => formatter.write_str("the kernel pipe limit was reached"),
            Self::NotFound(pipe_id) => write!(formatter, "pipe {pipe_id} was not found"),
        }
    }
}

#[derive(Debug)]
pub enum ReadOutcome {
    Data(Vec<u8>),
    Empty,
    EndOfFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written(usize),
    Full,
    NoReaders,
}

#[derive(Debug, Clone, Copy)]
pub struct PipeSnapshot {
    pub id: PipeId,
    pub buffered_bytes: usize,
    pub readers: usize,
    pub writers: usize,
    pub read_calls: u64,
    pub write_calls: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub blocked_reads: u64,
    pub blocked_writes: u64,
    pub reader_wakeups: u64,
    pub writer_wakeups: u64,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub active_pipes: usize,
    pub total_created: u64,
    pub total_destroyed: u64,
    pub total_read_calls: u64,
    pub total_write_calls: u64,
    pub total_bytes_read: u64,
    pub total_bytes_written: u64,
    pub total_blocked_reads: u64,
    pub total_blocked_writes: u64,
    pub total_reader_wakeups: u64,
    pub total_writer_wakeups: u64,
    pub pipes: Vec<PipeSnapshot>,
}

struct Pipe {
    id: PipeId,
    buffer: VecDeque<u8>,
    readers: usize,
    writers: usize,
    read_calls: u64,
    write_calls: u64,
    bytes_read: u64,
    bytes_written: u64,
    blocked_reads: u64,
    blocked_writes: u64,
    reader_wakeups: u64,
    writer_wakeups: u64,
}

impl Pipe {
    fn new(id: PipeId) -> Self {
        Self {
            id,
            buffer: VecDeque::with_capacity(PIPE_CAPACITY_BYTES),
            readers: 1,
            writers: 1,
            read_calls: 0,
            write_calls: 0,
            bytes_read: 0,
            bytes_written: 0,
            blocked_reads: 0,
            blocked_writes: 0,
            reader_wakeups: 0,
            writer_wakeups: 0,
        }
    }

    fn snapshot(&self) -> PipeSnapshot {
        PipeSnapshot {
            id: self.id,
            buffered_bytes: self.buffer.len(),
            readers: self.readers,
            writers: self.writers,
            read_calls: self.read_calls,
            write_calls: self.write_calls,
            bytes_read: self.bytes_read,
            bytes_written: self.bytes_written,
            blocked_reads: self.blocked_reads,
            blocked_writes: self.blocked_writes,
            reader_wakeups: self.reader_wakeups,
            writer_wakeups: self.writer_wakeups,
        }
    }
}

struct PipeManager {
    next_pipe_id: PipeId,
    pipes: Vec<Pipe>,
    total_created: u64,
    total_destroyed: u64,
    total_read_calls: u64,
    total_write_calls: u64,
    total_bytes_read: u64,
    total_bytes_written: u64,
    total_blocked_reads: u64,
    total_blocked_writes: u64,
    total_reader_wakeups: u64,
    total_writer_wakeups: u64,
}

impl PipeManager {
    const fn new() -> Self {
        Self {
            next_pipe_id: 1,
            pipes: Vec::new(),
            total_created: 0,
            total_destroyed: 0,
            total_read_calls: 0,
            total_write_calls: 0,
            total_bytes_read: 0,
            total_bytes_written: 0,
            total_blocked_reads: 0,
            total_blocked_writes: 0,
            total_reader_wakeups: 0,
            total_writer_wakeups: 0,
        }
    }

    fn create_pair(&mut self) -> Result<PipeId, Error> {
        if self.pipes.len() >= MAX_PIPES {
            return Err(Error::TooManyPipes);
        }
        let pipe_id = self.next_pipe_id;
        self.next_pipe_id = self.next_pipe_id.saturating_add(1);
        self.pipes.push(Pipe::new(pipe_id));
        self.total_created = self.total_created.saturating_add(1);
        Ok(pipe_id)
    }

    fn pipe_mut(&mut self, pipe_id: PipeId) -> Result<&mut Pipe, Error> {
        self.pipes
            .iter_mut()
            .find(|pipe| pipe.id == pipe_id)
            .ok_or(Error::NotFound(pipe_id))
    }

    fn close_reader(&mut self, pipe_id: PipeId) -> Result<(), Error> {
        let pipe = self.pipe_mut(pipe_id)?;
        pipe.readers = pipe.readers.saturating_sub(1);
        self.remove_if_closed(pipe_id);
        Ok(())
    }

    fn close_writer(&mut self, pipe_id: PipeId) -> Result<(), Error> {
        let pipe = self.pipe_mut(pipe_id)?;
        pipe.writers = pipe.writers.saturating_sub(1);
        self.remove_if_closed(pipe_id);
        Ok(())
    }

    fn remove_if_closed(&mut self, pipe_id: PipeId) {
        let Some(index) = self
            .pipes
            .iter()
            .position(|pipe| pipe.id == pipe_id && pipe.readers == 0 && pipe.writers == 0)
        else {
            return;
        };
        self.pipes.remove(index);
        self.total_destroyed = self.total_destroyed.saturating_add(1);
    }

    fn discard_pair(&mut self, pipe_id: PipeId) -> Result<(), Error> {
        let index = self
            .pipes
            .iter()
            .position(|pipe| pipe.id == pipe_id)
            .ok_or(Error::NotFound(pipe_id))?;
        self.pipes.remove(index);
        self.total_destroyed = self.total_destroyed.saturating_add(1);
        Ok(())
    }

    fn read(&mut self, pipe_id: PipeId, maximum: usize) -> Result<ReadOutcome, Error> {
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

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            active_pipes: self.pipes.len(),
            total_created: self.total_created,
            total_destroyed: self.total_destroyed,
            total_read_calls: self.total_read_calls,
            total_write_calls: self.total_write_calls,
            total_bytes_read: self.total_bytes_read,
            total_bytes_written: self.total_bytes_written,
            total_blocked_reads: self.total_blocked_reads,
            total_blocked_writes: self.total_blocked_writes,
            total_reader_wakeups: self.total_reader_wakeups,
            total_writer_wakeups: self.total_writer_wakeups,
            pipes: self.pipes.iter().map(Pipe::snapshot).collect(),
        }
    }
}

pub fn create_pair() -> Result<PipeId, Error> {
    x86_64::instructions::interrupts::without_interrupts(|| PIPE_MANAGER.lock().create_pair())
}

pub fn discard_pair(pipe_id: PipeId) -> Result<(), Error> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        PIPE_MANAGER.lock().discard_pair(pipe_id)
    })
}

pub fn close_reader(pipe_id: PipeId) -> Result<(), Error> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        PIPE_MANAGER.lock().close_reader(pipe_id)
    })
}

pub fn close_writer(pipe_id: PipeId) -> Result<(), Error> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        PIPE_MANAGER.lock().close_writer(pipe_id)
    })
}

pub fn read(pipe_id: PipeId, maximum: usize) -> Result<ReadOutcome, Error> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        PIPE_MANAGER.lock().read(pipe_id, maximum)
    })
}

pub fn write(pipe_id: PipeId, bytes: &[u8]) -> Result<WriteOutcome, Error> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        PIPE_MANAGER.lock().write(pipe_id, bytes)
    })
}

pub fn note_blocked_read(pipe_id: PipeId) -> Result<(), Error> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        PIPE_MANAGER.lock().note_blocked_read(pipe_id)
    })
}

pub fn note_blocked_write(pipe_id: PipeId) -> Result<(), Error> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        PIPE_MANAGER.lock().note_blocked_write(pipe_id)
    })
}

pub fn note_reader_wakeup(pipe_id: PipeId) -> Result<(), Error> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        PIPE_MANAGER.lock().note_reader_wakeup(pipe_id)
    })
}

pub fn note_writer_wakeup(pipe_id: PipeId) -> Result<(), Error> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        PIPE_MANAGER.lock().note_writer_wakeup(pipe_id)
    })
}

pub fn snapshot() -> Snapshot {
    x86_64::instructions::interrupts::without_interrupts(|| PIPE_MANAGER.lock().snapshot())
}
