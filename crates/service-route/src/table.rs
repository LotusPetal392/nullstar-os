use crate::{ProviderGeneration, RouteKey};

struct Slot<A> {
    key: RouteKey,
    generation: ProviderGeneration,
    authority: Option<A>,
}

/// A currently published route borrowed from a [`RouteTable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishedRoute<A> {
    pub generation: ProviderGeneration,
    pub authority: A,
}

/// Publication failure that retains ownership of the submitted authority.
#[derive(Debug, PartialEq, Eq)]
pub enum PublishError<A> {
    Capacity {
        authority: A,
    },
    GenerationNotNewer {
        authority: A,
        current_generation: ProviderGeneration,
    },
}

impl<A> PublishError<A> {
    pub fn authority(&self) -> &A {
        match self {
            Self::Capacity { authority } | Self::GenerationNotNewer { authority, .. } => authority,
        }
    }

    pub fn into_authority(self) -> A {
        match self {
            Self::Capacity { authority } | Self::GenerationNotNewer { authority, .. } => authority,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WithdrawError {
    UnknownRoute,
    GenerationMismatch {
        current_generation: ProviderGeneration,
    },
    NotPublished,
}

/// Fixed-capacity route storage that retains a key's latest generation after withdrawal.
///
/// A slot is permanently associated with its first key. This is what prevents an old generation
/// from becoming publishable again after a withdrawal, at the cost of bounding the number of
/// distinct keys the table can track over its lifetime.
pub struct RouteTable<A, const N: usize> {
    slots: [Option<Slot<A>>; N],
    len: usize,
}

impl<A, const N: usize> RouteTable<A, N> {
    pub const fn new() -> Self {
        Self {
            slots: [const { None }; N],
            len: 0,
        }
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    /// Number of keys tracked, including withdrawn tombstones.
    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn active_len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.as_ref().is_some_and(|slot| slot.authority.is_some()))
            .count()
    }

    /// Publishes a generation strictly newer than the key's retained generation.
    ///
    /// On success, returns any authority displaced from an active older generation. On failure,
    /// the submitted authority is returned in [`PublishError`].
    pub fn publish(
        &mut self,
        key: RouteKey,
        generation: ProviderGeneration,
        authority: A,
    ) -> Result<Option<A>, PublishError<A>> {
        if let Some(slot) = self
            .slots
            .iter_mut()
            .filter_map(Option::as_mut)
            .find(|slot| slot.key == key)
        {
            if generation <= slot.generation {
                return Err(PublishError::GenerationNotNewer {
                    authority,
                    current_generation: slot.generation,
                });
            }
            slot.generation = generation;
            return Ok(slot.authority.replace(authority));
        }

        let Some(slot) = self.slots.iter_mut().find(|slot| slot.is_none()) else {
            return Err(PublishError::Capacity { authority });
        };
        *slot = Some(Slot {
            key,
            generation,
            authority: Some(authority),
        });
        self.len += 1;
        Ok(None)
    }

    /// Withdraws only the currently published exact generation and returns its authority.
    pub fn withdraw(
        &mut self,
        key: RouteKey,
        generation: ProviderGeneration,
    ) -> Result<A, WithdrawError> {
        let slot = self
            .slots
            .iter_mut()
            .filter_map(Option::as_mut)
            .find(|slot| slot.key == key)
            .ok_or(WithdrawError::UnknownRoute)?;
        if generation != slot.generation {
            return Err(WithdrawError::GenerationMismatch {
                current_generation: slot.generation,
            });
        }
        slot.authority.take().ok_or(WithdrawError::NotPublished)
    }

    /// Returns the retained generation for either an active route or a tombstone.
    pub fn generation(&self, key: RouteKey) -> Option<ProviderGeneration> {
        self.slots
            .iter()
            .filter_map(Option::as_ref)
            .find(|slot| slot.key == key)
            .map(|slot| slot.generation)
    }

    pub fn get(&self, key: RouteKey) -> Option<PublishedRoute<&A>> {
        let slot = self
            .slots
            .iter()
            .filter_map(Option::as_ref)
            .find(|slot| slot.key == key)?;
        Some(PublishedRoute {
            generation: slot.generation,
            authority: slot.authority.as_ref()?,
        })
    }

    pub fn get_mut(&mut self, key: RouteKey) -> Option<PublishedRoute<&mut A>> {
        let slot = self
            .slots
            .iter_mut()
            .filter_map(Option::as_mut)
            .find(|slot| slot.key == key)?;
        Some(PublishedRoute {
            generation: slot.generation,
            authority: slot.authority.as_mut()?,
        })
    }
}

impl<A, const N: usize> Default for RouteTable<A, N> {
    fn default() -> Self {
        Self::new()
    }
}
