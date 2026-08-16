//! Bounded queued edge delivery for persistent object-signal registrations.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::object::Signals;

/// Stable identity for a waitable kernel object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Target {
    pub object_id: u64,
    pub object_kind: u64,
}

/// One persistent edge-triggered registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Registration {
    pub target: Target,
    pub requested: Signals,
    pub key: u64,
    observed: Signals,
}

/// One queued event returned to userspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Event {
    pub key: u64,
    pub signals: Signals,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddError {
    InvalidTarget,
    InvalidSignals,
    InvalidKey,
    DuplicateKey,
    Full,
    EventQueueFull,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownKey;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventQueueFull;

/// Persistent registrations plus a FIFO of coalesced rising-edge events.
#[derive(Debug)]
pub struct State {
    registration_capacity: usize,
    event_capacity: usize,
    maximum_key: u64,
    registrations: Vec<Registration>,
    events: VecDeque<Event>,
}

impl State {
    pub fn new(registration_capacity: usize, event_capacity: usize, maximum_key: u64) -> Self {
        Self {
            registration_capacity,
            event_capacity,
            maximum_key,
            registrations: Vec::new(),
            events: VecDeque::new(),
        }
    }

    pub fn add(
        &mut self,
        target: Target,
        requested: Signals,
        key: u64,
        current: Signals,
    ) -> Result<(), AddError> {
        if target.object_id == 0 || target.object_kind == 0 {
            return Err(AddError::InvalidTarget);
        }
        if requested.bits() == 0 {
            return Err(AddError::InvalidSignals);
        }
        if key > self.maximum_key {
            return Err(AddError::InvalidKey);
        }
        if self
            .registrations
            .iter()
            .any(|registration| registration.key == key)
        {
            return Err(AddError::DuplicateKey);
        }
        if self.registrations.len() >= self.registration_capacity {
            return Err(AddError::Full);
        }

        let observed = Signals::from_bits(current.bits() & requested.bits());
        if observed.bits() != 0 {
            self.queue_event(key, observed)
                .map_err(|_| AddError::EventQueueFull)?;
        }
        self.registrations.push(Registration {
            target,
            requested,
            key,
            observed,
        });
        Ok(())
    }

    pub fn remove(&mut self, key: u64) -> Result<Registration, UnknownKey> {
        let index = self
            .registrations
            .iter()
            .position(|registration| registration.key == key)
            .ok_or(UnknownKey)?;
        let registration = self.registrations.remove(index);
        self.events.retain(|event| event.key != key);
        Ok(registration)
    }

    /// Observe the current state of one target and queue newly asserted bits.
    pub fn observe(&mut self, target: Target, current: Signals) -> Result<(), EventQueueFull> {
        for index in 0..self.registrations.len() {
            let registration = self.registrations[index];
            if registration.target != target {
                continue;
            }
            let asserted = Signals::from_bits(current.bits() & registration.requested.bits());
            let rising = Signals::from_bits(asserted.bits() & !registration.observed.bits());
            if rising.bits() != 0 {
                self.queue_event(registration.key, rising)?;
            }
            self.registrations[index].observed = asserted;
        }
        Ok(())
    }

    fn queue_event(&mut self, key: u64, signals: Signals) -> Result<(), EventQueueFull> {
        if let Some(event) = self.events.iter_mut().find(|event| event.key == key) {
            event.signals = event.signals.union(signals);
            return Ok(());
        }
        if self.events.len() >= self.event_capacity {
            return Err(EventQueueFull);
        }
        self.events.push_back(Event { key, signals });
        Ok(())
    }

    pub fn pop_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    pub fn registrations(&self) -> impl Iterator<Item = Registration> + '_ {
        self.registrations.iter().copied()
    }

    pub fn registration_len(&self) -> usize {
        self.registrations.len()
    }

    pub fn queued_len(&self) -> usize {
        self.events.len()
    }

    pub fn is_readable(&self) -> bool {
        !self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{AddError, Event, State, Target};
    use crate::object::Signals;

    const ENDPOINT: Target = Target {
        object_id: 7,
        object_kind: 1,
    };

    #[test]
    fn initially_asserted_state_queues_one_event() {
        let mut state = State::new(2, 2, 99);
        state
            .add(
                ENDPOINT,
                Signals::READABLE.union(Signals::PEER_CLOSED),
                9,
                Signals::READABLE,
            )
            .unwrap();

        assert_eq!(
            state.pop_event(),
            Some(Event {
                key: 9,
                signals: Signals::READABLE,
            })
        );
        assert_eq!(state.pop_event(), None);
    }

    #[test]
    fn edges_rearm_only_after_deassertion() {
        let mut state = State::new(1, 1, 99);
        state
            .add(ENDPOINT, Signals::READABLE, 4, Signals::NONE)
            .unwrap();

        state.observe(ENDPOINT, Signals::READABLE).unwrap();
        state.observe(ENDPOINT, Signals::READABLE).unwrap();
        assert_eq!(state.queued_len(), 1);
        assert_eq!(state.pop_event().unwrap().signals, Signals::READABLE);
        state.observe(ENDPOINT, Signals::READABLE).unwrap();
        assert_eq!(state.pop_event(), None);

        state.observe(ENDPOINT, Signals::NONE).unwrap();
        state.observe(ENDPOINT, Signals::READABLE).unwrap();
        assert_eq!(state.pop_event().unwrap().key, 4);
    }

    #[test]
    fn multiple_signal_edges_for_one_key_coalesce_in_place() {
        let mut state = State::new(1, 1, 99);
        state
            .add(
                ENDPOINT,
                Signals::READABLE.union(Signals::PEER_CLOSED),
                5,
                Signals::NONE,
            )
            .unwrap();
        state.observe(ENDPOINT, Signals::READABLE).unwrap();
        state
            .observe(ENDPOINT, Signals::READABLE.union(Signals::PEER_CLOSED))
            .unwrap();

        assert_eq!(
            state.pop_event().unwrap().signals,
            Signals::READABLE.union(Signals::PEER_CLOSED)
        );
    }

    #[test]
    fn removal_purges_pending_events_and_capacity_is_bounded() {
        let mut state = State::new(1, 1, 7);
        state
            .add(ENDPOINT, Signals::READABLE, 2, Signals::READABLE)
            .unwrap();
        assert_eq!(
            state.add(ENDPOINT, Signals::READABLE, 3, Signals::NONE),
            Err(AddError::Full)
        );
        assert_eq!(state.remove(2).unwrap().key, 2);
        assert_eq!(state.queued_len(), 0);
        assert!(state.remove(2).is_err());
    }

    #[test]
    fn invalid_registration_fields_are_rejected() {
        let mut state = State::new(1, 1, 7);
        assert_eq!(
            state.add(
                Target {
                    object_id: 0,
                    object_kind: 1,
                },
                Signals::READABLE,
                1,
                Signals::NONE,
            ),
            Err(AddError::InvalidTarget)
        );
        assert_eq!(
            state.add(ENDPOINT, Signals::NONE, 1, Signals::NONE),
            Err(AddError::InvalidSignals)
        );
        assert_eq!(
            state.add(ENDPOINT, Signals::READABLE, 8, Signals::NONE),
            Err(AddError::InvalidKey)
        );
    }
}
