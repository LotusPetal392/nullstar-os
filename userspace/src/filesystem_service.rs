//! Bounded state used by userspace filesystem-service implementations.

use crate::ipc::CapabilityHandle;

pub const MAX_SESSIONS: usize = 4;
pub const MAX_BUFFERS_PER_SESSION: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NoSpace,
    StaleSession,
    InvalidBuffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferSlot {
    pub id: u64,
    pub handle: CapabilityHandle,
    pub length: u64,
}

impl BufferSlot {
    const EMPTY: Self = Self {
        id: 0,
        handle: 0,
        length: 0,
    };
}

#[derive(Clone, Copy)]
struct SessionSlot {
    id: u64,
    generation: u64,
    reply_endpoint: CapabilityHandle,
    buffers: [BufferSlot; MAX_BUFFERS_PER_SESSION],
}

impl SessionSlot {
    const EMPTY: Self = Self {
        id: 0,
        generation: 0,
        reply_endpoint: 0,
        buffers: [BufferSlot::EMPTY; MAX_BUFFERS_PER_SESSION],
    };
}

pub struct SessionTable {
    sessions: [SessionSlot; MAX_SESSIONS],
    next_id: u32,
}

pub struct ReleasedSession {
    pub reply_endpoint: CapabilityHandle,
    pub buffer_handles: [CapabilityHandle; MAX_BUFFERS_PER_SESSION],
}

impl SessionTable {
    pub const fn new() -> Self {
        Self {
            sessions: [SessionSlot::EMPTY; MAX_SESSIONS],
            next_id: 1,
        }
    }

    pub fn connect(
        &mut self,
        generation: u64,
        reply_endpoint: CapabilityHandle,
    ) -> Result<u64, Error> {
        if generation == 0 || reply_endpoint == 0 {
            return Err(Error::StaleSession);
        }
        let Some(slot) = self.sessions.iter_mut().find(|slot| slot.id == 0) else {
            return Err(Error::NoSpace);
        };
        let sequence = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = generation.rotate_left(17) ^ u64::from(sequence);
        *slot = SessionSlot {
            id: id.max(1),
            generation,
            reply_endpoint,
            buffers: [BufferSlot::EMPTY; MAX_BUFFERS_PER_SESSION],
        };
        Ok(slot.id)
    }

    pub fn reply_endpoint(&self, id: u64, generation: u64) -> Result<CapabilityHandle, Error> {
        self.session(id, generation)
            .map(|session| session.reply_endpoint)
    }

    pub fn attach_buffer(
        &mut self,
        id: u64,
        generation: u64,
        buffer_id: u64,
        handle: CapabilityHandle,
        length: u64,
    ) -> Result<Option<CapabilityHandle>, Error> {
        if buffer_id == 0 || handle == 0 || length == 0 {
            return Err(Error::InvalidBuffer);
        }
        let session = self.session_mut(id, generation)?;
        if let Some(buffer) = session
            .buffers
            .iter_mut()
            .find(|buffer| buffer.id == buffer_id)
        {
            let replaced = buffer.handle;
            *buffer = BufferSlot {
                id: buffer_id,
                handle,
                length,
            };
            return Ok(Some(replaced));
        }
        let Some(buffer) = session.buffers.iter_mut().find(|buffer| buffer.id == 0) else {
            return Err(Error::NoSpace);
        };
        *buffer = BufferSlot {
            id: buffer_id,
            handle,
            length,
        };
        Ok(None)
    }

    pub fn detach_buffer(
        &mut self,
        id: u64,
        generation: u64,
        buffer_id: u64,
    ) -> Result<CapabilityHandle, Error> {
        let session = self.session_mut(id, generation)?;
        let Some(buffer) = session
            .buffers
            .iter_mut()
            .find(|buffer| buffer.id == buffer_id && buffer.id != 0)
        else {
            return Err(Error::InvalidBuffer);
        };
        let handle = buffer.handle;
        *buffer = BufferSlot::EMPTY;
        Ok(handle)
    }

    pub fn buffer(&self, id: u64, generation: u64, buffer_id: u64) -> Result<BufferSlot, Error> {
        self.session(id, generation)?
            .buffers
            .iter()
            .find(|buffer| buffer.id == buffer_id && buffer.id != 0)
            .copied()
            .ok_or(Error::InvalidBuffer)
    }

    pub fn disconnect(&mut self, id: u64, generation: u64) -> Result<ReleasedSession, Error> {
        let session = self.session_mut(id, generation)?;
        let mut buffer_handles = [0; MAX_BUFFERS_PER_SESSION];
        for (destination, buffer) in buffer_handles.iter_mut().zip(session.buffers) {
            *destination = buffer.handle;
        }
        let released = ReleasedSession {
            reply_endpoint: session.reply_endpoint,
            buffer_handles,
        };
        *session = SessionSlot::EMPTY;
        Ok(released)
    }

    fn session(&self, id: u64, generation: u64) -> Result<&SessionSlot, Error> {
        self.sessions
            .iter()
            .find(|session| session.id == id && session.generation == generation && id != 0)
            .ok_or(Error::StaleSession)
    }

    fn session_mut(&mut self, id: u64, generation: u64) -> Result<&mut SessionSlot, Error> {
        self.sessions
            .iter_mut()
            .find(|session| session.id == id && session.generation == generation && id != 0)
            .ok_or(Error::StaleSession)
    }
}

impl Default for SessionTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, MAX_BUFFERS_PER_SESSION, MAX_SESSIONS, SessionTable};

    #[test]
    fn sessions_are_generation_scoped_and_bounded() {
        let mut table = SessionTable::new();
        let first = table.connect(7, 100).expect("first session");
        assert_eq!(table.reply_endpoint(first, 7), Ok(100));
        assert_eq!(table.reply_endpoint(first, 8), Err(Error::StaleSession));
        for handle in 1..MAX_SESSIONS {
            table
                .connect(7, 100 + handle as u64)
                .expect("bounded session");
        }
        assert_eq!(table.connect(7, 999), Err(Error::NoSpace));
    }

    #[test]
    fn buffer_ids_replace_explicitly_and_detach_returns_ownership() {
        let mut table = SessionTable::new();
        let session = table.connect(5, 10).expect("session");
        assert_eq!(table.attach_buffer(session, 5, 1, 20, 4096), Ok(None));
        assert_eq!(table.attach_buffer(session, 5, 1, 21, 8192), Ok(Some(20)));
        let buffer = table.buffer(session, 5, 1).expect("buffer");
        assert_eq!((buffer.handle, buffer.length), (21, 8192));
        assert_eq!(table.detach_buffer(session, 5, 1), Ok(21));
        assert_eq!(table.buffer(session, 5, 1), Err(Error::InvalidBuffer));
    }

    #[test]
    fn buffer_tables_are_bounded() {
        let mut table = SessionTable::new();
        let session = table.connect(9, 10).expect("session");
        for index in 0..MAX_BUFFERS_PER_SESSION {
            table
                .attach_buffer(session, 9, index as u64 + 1, index as u64 + 20, 64)
                .expect("bounded buffer");
        }
        assert_eq!(
            table.attach_buffer(session, 9, 99, 99, 64),
            Err(Error::NoSpace)
        );
    }

    #[test]
    fn disconnect_releases_reply_and_buffer_handles() {
        let mut table = SessionTable::new();
        let session = table.connect(4, 10).expect("session");
        table
            .attach_buffer(session, 4, 1, 20, 64)
            .expect("first buffer");
        table
            .attach_buffer(session, 4, 2, 21, 64)
            .expect("second buffer");
        let released = table.disconnect(session, 4).expect("disconnect");
        assert_eq!(released.reply_endpoint, 10);
        assert_eq!(&released.buffer_handles[..2], &[20, 21]);
        assert_eq!(table.reply_endpoint(session, 4), Err(Error::StaleSession));
        assert!(table.connect(4, 11).is_ok());
    }
}
