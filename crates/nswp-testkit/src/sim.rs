use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard},
};

use nswp_runtime::{TryRecvError, TrySendError, TryTransport};

#[derive(Default)]
struct Direction {
    queued: VecDeque<Vec<u8>>,
    held: Vec<Vec<u8>>,
    hold: bool,
}

struct Link {
    directions: [Direction; 2],
    closed: [bool; 2],
    side_handles: [usize; 2],
}

impl Link {
    fn new() -> Self {
        Self {
            directions: [Direction::default(), Direction::default()],
            closed: [false; 2],
            side_handles: [1; 2],
        }
    }
}

pub struct SimEndpoint<const QUEUE: usize> {
    link: Arc<Mutex<Link>>,
    side: usize,
}

pub fn channel_pair<const QUEUE: usize>() -> (SimEndpoint<QUEUE>, SimEndpoint<QUEUE>) {
    assert!(QUEUE > 0, "simulated queue capacity must be nonzero");
    let link = Arc::new(Mutex::new(Link::new()));
    (
        SimEndpoint {
            link: Arc::clone(&link),
            side: 0,
        },
        SimEndpoint { link, side: 1 },
    )
}

impl<const QUEUE: usize> Clone for SimEndpoint<QUEUE> {
    fn clone(&self) -> Self {
        let mut link = self.lock();
        link.side_handles[self.side] += 1;
        drop(link);
        Self {
            link: Arc::clone(&self.link),
            side: self.side,
        }
    }
}

impl<const QUEUE: usize> Drop for SimEndpoint<QUEUE> {
    fn drop(&mut self) {
        let mut link = self.lock();
        link.side_handles[self.side] = link.side_handles[self.side].saturating_sub(1);
        if link.side_handles[self.side] == 0 {
            link.closed[self.side] = true;
        }
    }
}

impl<const QUEUE: usize> SimEndpoint<QUEUE> {
    pub fn queued_incoming(&self) -> usize {
        self.lock().directions[self.side].queued.len()
    }

    pub fn held_incoming(&self) -> usize {
        self.lock().directions[self.side].held.len()
    }

    pub fn set_hold_incoming(&self, hold: bool) {
        self.lock().directions[self.side].hold = hold;
    }

    pub fn release_held(&self, index: usize) -> bool {
        let mut link = self.lock();
        let direction = &mut link.directions[self.side];
        if index >= direction.held.len() || direction.queued.len() >= QUEUE {
            return false;
        }
        let packet = direction.held.remove(index);
        direction.queued.push_back(packet);
        true
    }

    pub fn release_all_held(&self) -> usize {
        let mut released = 0;
        while self.release_held(0) {
            released += 1;
        }
        released
    }

    pub fn corrupt_held(&self, index: usize, offset: usize, value: u8) -> bool {
        let mut link = self.lock();
        let Some(byte) = link.directions[self.side]
            .held
            .get_mut(index)
            .and_then(|packet| packet.get_mut(offset))
        else {
            return false;
        };
        *byte = value;
        true
    }

    pub fn discard_incoming(&self) -> Option<Vec<u8>> {
        self.lock().directions[self.side].queued.pop_front()
    }

    pub fn incoming_packet(&self, index: usize) -> Option<Vec<u8>> {
        self.lock().directions[self.side].queued.get(index).cloned()
    }

    pub fn inject_incoming(&self, packet: Vec<u8>) -> bool {
        let mut link = self.lock();
        let direction = &mut link.directions[self.side];
        if direction.queued.len() + direction.held.len() >= QUEUE {
            return false;
        }
        if direction.hold {
            direction.held.push(packet);
        } else {
            direction.queued.push_back(packet);
        }
        true
    }

    pub fn peer_closed(&self) -> bool {
        self.lock().closed[1 - self.side]
    }

    fn lock(&self) -> MutexGuard<'_, Link> {
        self.link
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl<const QUEUE: usize> TryTransport for SimEndpoint<QUEUE> {
    fn try_send(&mut self, packet: &[u8]) -> Result<(), TrySendError> {
        let destination = 1 - self.side;
        let mut link = self.lock();
        if link.closed[self.side] || link.closed[destination] {
            return Err(TrySendError::PeerClosed);
        }
        let direction = &mut link.directions[destination];
        if direction.queued.len() + direction.held.len() >= QUEUE {
            return Err(TrySendError::Full);
        }
        if direction.hold {
            direction.held.push(packet.to_vec());
        } else {
            direction.queued.push_back(packet.to_vec());
        }
        Ok(())
    }

    fn try_recv(&mut self, output: &mut [u8]) -> Result<usize, TryRecvError> {
        let mut link = self.lock();
        let direction = &mut link.directions[self.side];
        let Some(packet) = direction.queued.front() else {
            return if link.closed[1 - self.side] {
                Err(TryRecvError::PeerClosed)
            } else {
                Err(TryRecvError::Empty)
            };
        };
        if packet.len() > output.len() {
            return Err(TryRecvError::MessageTooLarge {
                bytes: packet.len(),
            });
        }
        let packet = direction.queued.pop_front().expect("front packet exists");
        output[..packet.len()].copy_from_slice(&packet);
        Ok(packet.len())
    }

    fn close(&mut self) {
        self.lock().closed[self.side] = true;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ManualClock {
    now_ns: u64,
}

impl ManualClock {
    pub const fn new(now_ns: u64) -> Self {
        Self { now_ns }
    }

    pub const fn now_ns(self) -> u64 {
        self.now_ns
    }

    pub fn set(&mut self, now_ns: u64) {
        self.now_ns = now_ns;
    }

    pub fn advance(&mut self, duration_ns: u64) {
        self.now_ns = self.now_ns.saturating_add(duration_ns);
    }
}
