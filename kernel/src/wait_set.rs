//! Bounded persistent registrations for one wait-set capability object.

use alloc::vec::Vec;

use crate::object::Signals;

/// Stable identity for a waitable kernel object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Target {
    pub object_id: u64,
    pub object_kind: u64,
}

/// One persistent level-triggered registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Registration {
    pub target: Target,
    pub requested: Signals,
    pub key: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddError {
    InvalidTarget,
    InvalidSignals,
    InvalidKey,
    DuplicateKey,
    Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownKey;

/// Insertion-ordered registrations retained by one wait-set object.
#[derive(Debug)]
pub struct State {
    capacity: usize,
    maximum_key: u64,
    registrations: Vec<Registration>,
}

impl State {
    pub fn new(capacity: usize, maximum_key: u64) -> Self {
        Self {
            capacity,
            maximum_key,
            registrations: Vec::new(),
        }
    }

    pub fn add(&mut self, registration: Registration) -> Result<(), AddError> {
        if registration.target.object_id == 0 || registration.target.object_kind == 0 {
            return Err(AddError::InvalidTarget);
        }
        if registration.requested.bits() == 0 {
            return Err(AddError::InvalidSignals);
        }
        if registration.key > self.maximum_key {
            return Err(AddError::InvalidKey);
        }
        if self
            .registrations
            .iter()
            .any(|existing| existing.key == registration.key)
        {
            return Err(AddError::DuplicateKey);
        }
        if self.registrations.len() >= self.capacity {
            return Err(AddError::Full);
        }
        self.registrations.push(registration);
        Ok(())
    }

    pub fn remove(&mut self, key: u64) -> Result<Registration, UnknownKey> {
        let index = self
            .registrations
            .iter()
            .position(|registration| registration.key == key)
            .ok_or(UnknownKey)?;
        Ok(self.registrations.remove(index))
    }

    pub fn registrations(&self) -> impl Iterator<Item = Registration> + '_ {
        self.registrations.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::{AddError, Registration, State, Target};
    use crate::object::Signals;

    const ENDPOINT: Target = Target {
        object_id: 7,
        object_kind: 1,
    };

    #[test]
    fn registrations_are_bounded_keyed_and_insertion_ordered() {
        let mut state = State::new(2, 99);
        state
            .add(Registration {
                target: ENDPOINT,
                requested: Signals::READABLE,
                key: 9,
            })
            .unwrap();
        state
            .add(Registration {
                target: Target {
                    object_id: 8,
                    object_kind: 2,
                },
                requested: Signals::SIGNALED,
                key: 4,
            })
            .unwrap();

        assert_eq!(
            state
                .registrations()
                .map(|item| item.key)
                .collect::<Vec<_>>(),
            vec![9, 4]
        );
        assert_eq!(
            state.add(Registration {
                target: ENDPOINT,
                requested: Signals::WRITABLE,
                key: 9,
            }),
            Err(AddError::DuplicateKey)
        );
        assert_eq!(
            state.add(Registration {
                target: ENDPOINT,
                requested: Signals::WRITABLE,
                key: 5,
            }),
            Err(AddError::Full)
        );
    }

    #[test]
    fn removal_preserves_the_remaining_order_and_returns_the_target() {
        let mut state = State::new(2, 99);
        for key in [2, 3] {
            state
                .add(Registration {
                    target: ENDPOINT,
                    requested: Signals::READABLE,
                    key,
                })
                .unwrap();
        }

        assert_eq!(state.remove(2).unwrap().key, 2);
        assert_eq!(
            state
                .registrations()
                .map(|item| item.key)
                .collect::<Vec<_>>(),
            vec![3]
        );
        assert!(state.remove(2).is_err());
    }

    #[test]
    fn invalid_targets_signals_and_keys_are_rejected() {
        let mut state = State::new(1, 7);
        assert_eq!(
            state.add(Registration {
                target: Target {
                    object_id: 0,
                    object_kind: 1,
                },
                requested: Signals::READABLE,
                key: 1,
            }),
            Err(AddError::InvalidTarget)
        );
        assert_eq!(
            state.add(Registration {
                target: ENDPOINT,
                requested: Signals::NONE,
                key: 1,
            }),
            Err(AddError::InvalidSignals)
        );
        assert_eq!(
            state.add(Registration {
                target: ENDPOINT,
                requested: Signals::READABLE,
                key: 8,
            }),
            Err(AddError::InvalidKey)
        );
    }
}
