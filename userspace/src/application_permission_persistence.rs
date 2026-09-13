//! Crash-safe two-slot persistence for the application permission store.
//!
//! A commit writes and synchronizes an inactive checkpoint before publishing a checksummed selector
//! in the inactive selector slot. Recovery considers only checkpoints referenced by a valid
//! selector, so a crash before selector publication cannot expose an uncommitted store. Keeping two
//! selector copies preserves the previous commit if publication is torn.

use core::array;

use nullfs_format::crc32c;

use crate::application_permission::{
    APPLICATION_GRANT_RECORD_BYTES, ApplicationGrantDecodeError, ApplicationGrantRecord,
    ApplicationPermissionLoadError, ApplicationPermissionStore, MAX_APPLICATION_GRANTS,
};

pub const APPLICATION_PERMISSION_CHECKPOINT_MAGIC: [u8; 4] = *b"NSGC";
pub const APPLICATION_PERMISSION_CHECKPOINT_VERSION: u16 = 1;
pub const APPLICATION_PERMISSION_CHECKPOINT_HEADER_BYTES: usize = 64;
pub const APPLICATION_PERMISSION_CHECKPOINT_BYTES: usize =
    APPLICATION_PERMISSION_CHECKPOINT_HEADER_BYTES
        + MAX_APPLICATION_GRANTS * APPLICATION_GRANT_RECORD_BYTES;
pub const APPLICATION_PERMISSION_SELECTOR_MAGIC: [u8; 4] = *b"NSGS";
pub const APPLICATION_PERMISSION_SELECTOR_VERSION: u16 = 1;
pub const APPLICATION_PERMISSION_SELECTOR_BYTES: usize = 64;

const CHECKPOINT_CHECKSUM_OFFSET: usize = 60;
const SELECTOR_CHECKSUM_OFFSET: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationPermissionCommit {
    sequence: u64,
    checkpoint_generation: u64,
    checkpoint_slot: u8,
    selector_slot: u8,
}

impl ApplicationPermissionCommit {
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    pub const fn checkpoint_generation(self) -> u64 {
        self.checkpoint_generation
    }

    pub const fn checkpoint_slot(self) -> usize {
        self.checkpoint_slot as usize
    }

    pub const fn selector_slot(self) -> usize {
        self.selector_slot as usize
    }
}

pub struct RecoveredApplicationPermissionStore {
    pub store: ApplicationPermissionStore,
    pub commit: ApplicationPermissionCommit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPermissionCheckpointError {
    Length,
    Magic,
    Version,
    Header,
    Checksum,
    Record(ApplicationGrantDecodeError),
    Store(ApplicationPermissionLoadError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPermissionSelectorError {
    Length,
    Magic,
    Version,
    Canonical,
    Checksum,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApplicationPermissionPersistenceError<E> {
    Storage(E),
    NoCommittedCheckpoint,
    GenerationExhausted,
    AmbiguousSelector,
}

pub trait ApplicationPermissionPersistence {
    type Error;

    fn read_checkpoint(
        &mut self,
        slot: usize,
        output: &mut [u8; APPLICATION_PERMISSION_CHECKPOINT_BYTES],
    ) -> Result<(), Self::Error>;
    fn write_checkpoint(
        &mut self,
        slot: usize,
        bytes: &[u8; APPLICATION_PERMISSION_CHECKPOINT_BYTES],
    ) -> Result<(), Self::Error>;
    fn sync_checkpoint(&mut self, slot: usize) -> Result<(), Self::Error>;
    fn read_selector(
        &mut self,
        slot: usize,
        output: &mut [u8; APPLICATION_PERMISSION_SELECTOR_BYTES],
    ) -> Result<(), Self::Error>;
    fn write_selector(
        &mut self,
        slot: usize,
        bytes: &[u8; APPLICATION_PERMISSION_SELECTOR_BYTES],
    ) -> Result<(), Self::Error>;
    fn sync_selector(&mut self, slot: usize) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selector {
    sequence: u64,
    checkpoint_generation: u64,
    checkpoint_checksum: u32,
    checkpoint_slot: u8,
}

pub fn encode_application_permission_checkpoint(
    store: &ApplicationPermissionStore,
    generation: u64,
) -> Result<[u8; APPLICATION_PERMISSION_CHECKPOINT_BYTES], ApplicationPermissionCheckpointError> {
    if generation == 0 {
        return Err(ApplicationPermissionCheckpointError::Header);
    }
    let record_count = store.records().count();
    let mut bytes = [0; APPLICATION_PERMISSION_CHECKPOINT_BYTES];
    bytes[..4].copy_from_slice(&APPLICATION_PERMISSION_CHECKPOINT_MAGIC);
    bytes[4..6].copy_from_slice(&APPLICATION_PERMISSION_CHECKPOINT_VERSION.to_le_bytes());
    bytes[6..8]
        .copy_from_slice(&(APPLICATION_PERMISSION_CHECKPOINT_HEADER_BYTES as u16).to_le_bytes());
    put_u64(&mut bytes, 8, generation);
    put_u64(&mut bytes, 16, store.next_grant_id());
    put_u64(&mut bytes, 24, store.next_revision());
    bytes[32..34].copy_from_slice(&(record_count as u16).to_le_bytes());
    bytes[34..36].copy_from_slice(&(APPLICATION_GRANT_RECORD_BYTES as u16).to_le_bytes());
    let payload_bytes = record_count * APPLICATION_GRANT_RECORD_BYTES;
    bytes[36..40].copy_from_slice(&(payload_bytes as u32).to_le_bytes());
    for (index, record) in store.records().enumerate() {
        let start =
            APPLICATION_PERMISSION_CHECKPOINT_HEADER_BYTES + index * APPLICATION_GRANT_RECORD_BYTES;
        bytes[start..start + APPLICATION_GRANT_RECORD_BYTES].copy_from_slice(&record.encode());
    }
    let payload_checksum = crc32c(&bytes[APPLICATION_PERMISSION_CHECKPOINT_HEADER_BYTES..]);
    bytes[40..44].copy_from_slice(&payload_checksum.to_le_bytes());
    let checksum = crc32c(&bytes[..CHECKPOINT_CHECKSUM_OFFSET]);
    bytes[CHECKPOINT_CHECKSUM_OFFSET..APPLICATION_PERMISSION_CHECKPOINT_HEADER_BYTES]
        .copy_from_slice(&checksum.to_le_bytes());
    Ok(bytes)
}

pub fn decode_application_permission_checkpoint(
    bytes: &[u8],
) -> Result<(u64, ApplicationPermissionStore), ApplicationPermissionCheckpointError> {
    if bytes.len() != APPLICATION_PERMISSION_CHECKPOINT_BYTES {
        return Err(ApplicationPermissionCheckpointError::Length);
    }
    if bytes[..4] != APPLICATION_PERMISSION_CHECKPOINT_MAGIC {
        return Err(ApplicationPermissionCheckpointError::Magic);
    }
    if read_u16(bytes, 4) != APPLICATION_PERMISSION_CHECKPOINT_VERSION {
        return Err(ApplicationPermissionCheckpointError::Version);
    }
    let generation = read_u64(bytes, 8);
    let record_count = usize::from(read_u16(bytes, 32));
    let payload_bytes = read_u32(bytes, 36) as usize;
    if read_u16(bytes, 6) as usize != APPLICATION_PERMISSION_CHECKPOINT_HEADER_BYTES
        || generation == 0
        || record_count > MAX_APPLICATION_GRANTS
        || read_u16(bytes, 34) as usize != APPLICATION_GRANT_RECORD_BYTES
        || payload_bytes != record_count * APPLICATION_GRANT_RECORD_BYTES
        || bytes[44..CHECKPOINT_CHECKSUM_OFFSET]
            .iter()
            .any(|byte| *byte != 0)
        || bytes[APPLICATION_PERMISSION_CHECKPOINT_HEADER_BYTES + payload_bytes..]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(ApplicationPermissionCheckpointError::Header);
    }
    if crc32c(&bytes[..CHECKPOINT_CHECKSUM_OFFSET]) != read_u32(bytes, CHECKPOINT_CHECKSUM_OFFSET)
        || crc32c(&bytes[APPLICATION_PERMISSION_CHECKPOINT_HEADER_BYTES..]) != read_u32(bytes, 40)
    {
        return Err(ApplicationPermissionCheckpointError::Checksum);
    }
    let mut records: [Option<ApplicationGrantRecord>; MAX_APPLICATION_GRANTS] =
        array::from_fn(|_| None);
    for (index, slot) in records.iter_mut().enumerate().take(record_count) {
        let start =
            APPLICATION_PERMISSION_CHECKPOINT_HEADER_BYTES + index * APPLICATION_GRANT_RECORD_BYTES;
        *slot = Some(
            ApplicationGrantRecord::decode(&bytes[start..start + APPLICATION_GRANT_RECORD_BYTES])
                .map_err(ApplicationPermissionCheckpointError::Record)?,
        );
    }
    let store = ApplicationPermissionStore::restore_checkpoint_slots(
        records,
        record_count,
        read_u64(bytes, 16),
        read_u64(bytes, 24),
    )
    .map_err(ApplicationPermissionCheckpointError::Store)?;
    Ok((generation, store))
}

/// Publishes one complete store snapshot. The returned commit metadata must be retained in memory;
/// after a restart it is reconstructed by [`recover_application_permission_store`].
pub fn commit_application_permission_store<B: ApplicationPermissionPersistence>(
    backend: &mut B,
    store: &ApplicationPermissionStore,
    previous: Option<ApplicationPermissionCommit>,
) -> Result<ApplicationPermissionCommit, ApplicationPermissionPersistenceError<B::Error>> {
    let sequence = previous
        .map_or(Ok(1), |commit| commit.sequence.checked_add(1).ok_or(()))
        .map_err(|()| ApplicationPermissionPersistenceError::GenerationExhausted)?;
    let checkpoint_generation = previous
        .map_or(Ok(1), |commit| {
            commit.checkpoint_generation.checked_add(1).ok_or(())
        })
        .map_err(|()| ApplicationPermissionPersistenceError::GenerationExhausted)?;
    let checkpoint_slot = previous.map_or(0, |commit| 1 - commit.checkpoint_slot());
    let selector_slot = previous.map_or(0, |commit| 1 - commit.selector_slot());
    let checkpoint = encode_application_permission_checkpoint(store, checkpoint_generation)
        .expect("validated permission store always encodes");
    backend
        .write_checkpoint(checkpoint_slot, &checkpoint)
        .map_err(ApplicationPermissionPersistenceError::Storage)?;
    backend
        .sync_checkpoint(checkpoint_slot)
        .map_err(ApplicationPermissionPersistenceError::Storage)?;

    let selector = Selector {
        sequence,
        checkpoint_generation,
        checkpoint_checksum: crc32c(&checkpoint),
        checkpoint_slot: checkpoint_slot as u8,
    };
    let encoded_selector = encode_selector(selector);
    backend
        .write_selector(selector_slot, &encoded_selector)
        .map_err(ApplicationPermissionPersistenceError::Storage)?;
    backend
        .sync_selector(selector_slot)
        .map_err(ApplicationPermissionPersistenceError::Storage)?;
    Ok(ApplicationPermissionCommit {
        sequence,
        checkpoint_generation,
        checkpoint_slot: checkpoint_slot as u8,
        selector_slot: selector_slot as u8,
    })
}

pub fn recover_application_permission_store<B: ApplicationPermissionPersistence>(
    backend: &mut B,
) -> Result<RecoveredApplicationPermissionStore, ApplicationPermissionPersistenceError<B::Error>> {
    let mut candidates: [Option<(Selector, usize)>; 2] = [None, None];
    for (slot, candidate) in candidates.iter_mut().enumerate() {
        let mut bytes = [0; APPLICATION_PERMISSION_SELECTOR_BYTES];
        backend
            .read_selector(slot, &mut bytes)
            .map_err(ApplicationPermissionPersistenceError::Storage)?;
        if let Ok(selector) = decode_selector(&bytes) {
            *candidate = Some((selector, slot));
        }
    }
    if let (Some((left, _)), Some((right, _))) = (candidates[0], candidates[1])
        && left.sequence == right.sequence
        && left != right
    {
        return Err(ApplicationPermissionPersistenceError::AmbiguousSelector);
    }
    if candidates[1].map(|(selector, _)| selector.sequence)
        > candidates[0].map(|(selector, _)| selector.sequence)
    {
        candidates.swap(0, 1);
    }
    for (selector, selector_slot) in candidates.into_iter().flatten() {
        let mut checkpoint = [0; APPLICATION_PERMISSION_CHECKPOINT_BYTES];
        backend
            .read_checkpoint(selector.checkpoint_slot as usize, &mut checkpoint)
            .map_err(ApplicationPermissionPersistenceError::Storage)?;
        if crc32c(&checkpoint) != selector.checkpoint_checksum {
            continue;
        }
        let Ok((generation, store)) = decode_application_permission_checkpoint(&checkpoint) else {
            continue;
        };
        if generation != selector.checkpoint_generation {
            continue;
        }
        return Ok(RecoveredApplicationPermissionStore {
            store,
            commit: ApplicationPermissionCommit {
                sequence: selector.sequence,
                checkpoint_generation: selector.checkpoint_generation,
                checkpoint_slot: selector.checkpoint_slot,
                selector_slot: selector_slot as u8,
            },
        });
    }
    Err(ApplicationPermissionPersistenceError::NoCommittedCheckpoint)
}

fn encode_selector(selector: Selector) -> [u8; APPLICATION_PERMISSION_SELECTOR_BYTES] {
    let mut bytes = [0; APPLICATION_PERMISSION_SELECTOR_BYTES];
    bytes[..4].copy_from_slice(&APPLICATION_PERMISSION_SELECTOR_MAGIC);
    bytes[4..6].copy_from_slice(&APPLICATION_PERMISSION_SELECTOR_VERSION.to_le_bytes());
    bytes[6] = selector.checkpoint_slot;
    put_u64(&mut bytes, 8, selector.sequence);
    put_u64(&mut bytes, 16, selector.checkpoint_generation);
    bytes[24..28].copy_from_slice(&selector.checkpoint_checksum.to_le_bytes());
    let checksum = crc32c(&bytes[..SELECTOR_CHECKSUM_OFFSET]);
    bytes[SELECTOR_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

fn decode_selector(bytes: &[u8]) -> Result<Selector, ApplicationPermissionSelectorError> {
    if bytes.len() != APPLICATION_PERMISSION_SELECTOR_BYTES {
        return Err(ApplicationPermissionSelectorError::Length);
    }
    if bytes[..4] != APPLICATION_PERMISSION_SELECTOR_MAGIC {
        return Err(ApplicationPermissionSelectorError::Magic);
    }
    if read_u16(bytes, 4) != APPLICATION_PERMISSION_SELECTOR_VERSION {
        return Err(ApplicationPermissionSelectorError::Version);
    }
    if bytes[6] > 1
        || bytes[7] != 0
        || read_u64(bytes, 8) == 0
        || read_u64(bytes, 16) == 0
        || bytes[28..SELECTOR_CHECKSUM_OFFSET]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(ApplicationPermissionSelectorError::Canonical);
    }
    if crc32c(&bytes[..SELECTOR_CHECKSUM_OFFSET]) != read_u32(bytes, SELECTOR_CHECKSUM_OFFSET) {
        return Err(ApplicationPermissionSelectorError::Checksum);
    }
    Ok(Selector {
        checkpoint_slot: bytes[6],
        sequence: read_u64(bytes, 8),
        checkpoint_generation: read_u64(bytes, 16),
        checkpoint_checksum: read_u32(bytes, 24),
    })
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application_identity::{
            ApplicationInstallScope, ApplicationInstallation, ApplicationLaunchSelection,
            ApplicationProfile, ApplicationProfileSet, ApplicationTrustClass,
            InstalledApplicationComponent, PackageVerification, authorize_application_launch,
        },
        application_permission::{
            ApplicationGrantRights, ApplicationGrantScope, ApplicationResourceIdentity,
            ApplicationResourceKind,
        },
    };

    struct MemoryPersistence {
        checkpoints: [[u8; APPLICATION_PERMISSION_CHECKPOINT_BYTES]; 2],
        selectors: [[u8; APPLICATION_PERMISSION_SELECTOR_BYTES]; 2],
    }

    impl MemoryPersistence {
        fn new() -> Self {
            Self {
                checkpoints: [[0; APPLICATION_PERMISSION_CHECKPOINT_BYTES]; 2],
                selectors: [[0; APPLICATION_PERMISSION_SELECTOR_BYTES]; 2],
            }
        }
    }

    impl ApplicationPermissionPersistence for MemoryPersistence {
        type Error = ();

        fn read_checkpoint(
            &mut self,
            slot: usize,
            output: &mut [u8; APPLICATION_PERMISSION_CHECKPOINT_BYTES],
        ) -> Result<(), Self::Error> {
            *output = self.checkpoints[slot];
            Ok(())
        }
        fn write_checkpoint(
            &mut self,
            slot: usize,
            bytes: &[u8; APPLICATION_PERMISSION_CHECKPOINT_BYTES],
        ) -> Result<(), Self::Error> {
            self.checkpoints[slot] = *bytes;
            Ok(())
        }
        fn sync_checkpoint(&mut self, _slot: usize) -> Result<(), Self::Error> {
            Ok(())
        }
        fn read_selector(
            &mut self,
            slot: usize,
            output: &mut [u8; APPLICATION_PERMISSION_SELECTOR_BYTES],
        ) -> Result<(), Self::Error> {
            *output = self.selectors[slot];
            Ok(())
        }
        fn write_selector(
            &mut self,
            slot: usize,
            bytes: &[u8; APPLICATION_PERMISSION_SELECTOR_BYTES],
        ) -> Result<(), Self::Error> {
            self.selectors[slot] = *bytes;
            Ok(())
        }
        fn sync_selector(&mut self, _slot: usize) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct FailingPersistence {
        memory: MemoryPersistence,
        fail_at: Option<usize>,
        operation: usize,
    }

    impl FailingPersistence {
        fn new() -> Self {
            Self {
                memory: MemoryPersistence::new(),
                fail_at: None,
                operation: 0,
            }
        }

        fn arm(&mut self, fail_at: usize) {
            self.fail_at = Some(fail_at);
            self.operation = 0;
        }

        fn mutation(&mut self) -> Result<(), ()> {
            let current = self.operation;
            self.operation += 1;
            if self.fail_at == Some(current) {
                Err(())
            } else {
                Ok(())
            }
        }
    }

    impl ApplicationPermissionPersistence for FailingPersistence {
        type Error = ();

        fn read_checkpoint(
            &mut self,
            slot: usize,
            output: &mut [u8; APPLICATION_PERMISSION_CHECKPOINT_BYTES],
        ) -> Result<(), Self::Error> {
            self.memory.read_checkpoint(slot, output)
        }

        fn write_checkpoint(
            &mut self,
            slot: usize,
            bytes: &[u8; APPLICATION_PERMISSION_CHECKPOINT_BYTES],
        ) -> Result<(), Self::Error> {
            self.mutation()?;
            self.memory.write_checkpoint(slot, bytes)
        }

        fn sync_checkpoint(&mut self, slot: usize) -> Result<(), Self::Error> {
            self.mutation()?;
            self.memory.sync_checkpoint(slot)
        }

        fn read_selector(
            &mut self,
            slot: usize,
            output: &mut [u8; APPLICATION_PERMISSION_SELECTOR_BYTES],
        ) -> Result<(), Self::Error> {
            self.memory.read_selector(slot, output)
        }

        fn write_selector(
            &mut self,
            slot: usize,
            bytes: &[u8; APPLICATION_PERMISSION_SELECTOR_BYTES],
        ) -> Result<(), Self::Error> {
            self.mutation()?;
            self.memory.write_selector(slot, bytes)
        }

        fn sync_selector(&mut self, slot: usize) -> Result<(), Self::Error> {
            self.mutation()?;
            self.memory.sync_selector(slot)
        }
    }

    fn authorization() -> crate::application_identity::AuthorizedApplication {
        let components = [InstalledApplicationComponent::new(
            21,
            b"/app",
            ApplicationProfileSet::DESKTOP,
            true,
        )];
        authorize_application_launch(
            PackageVerification {
                package: 11,
                package_generation: 12,
                application: 13,
                publisher: 14,
                signing_lineage: 15,
                trust_class: ApplicationTrustClass::Repository,
                system_application: false,
                components: &components,
            },
            ApplicationInstallation {
                installation: 16,
                package: 11,
                package_generation: 12,
                application: 13,
                publisher: 14,
                signing_lineage: 15,
                trust_class: ApplicationTrustClass::Repository,
                scope: ApplicationInstallScope::User,
                owner_user: 17,
                system_application: false,
            },
            ApplicationLaunchSelection {
                component: 21,
                user: 17,
                session: 18,
                profile: ApplicationProfile::Desktop,
            },
        )
        .unwrap()
    }

    #[test]
    fn checkpoint_round_trip_preserves_records_and_counters() {
        let mut store = ApplicationPermissionStore::new();
        store
            .issue(
                authorization(),
                ApplicationResourceIdentity::new([9; 16], 2, 3, ApplicationResourceKind::File)
                    .unwrap(),
                ApplicationGrantRights::READ,
                ApplicationGrantScope::Persistent,
            )
            .unwrap();
        let bytes = encode_application_permission_checkpoint(&store, 4).unwrap();
        let (generation, restored) = decode_application_permission_checkpoint(&bytes).unwrap();
        assert_eq!(generation, 4);
        assert_eq!(restored.records().count(), store.records().count());
        assert!(
            restored
                .records()
                .zip(store.records())
                .all(|(left, right)| left == right)
        );
        assert_eq!(restored.next_grant_id(), store.next_grant_id());
        assert_eq!(restored.next_revision(), store.next_revision());
    }

    #[test]
    fn selector_publication_is_the_commit_point_and_recovery_falls_back() {
        let store = ApplicationPermissionStore::new();
        let mut backend = MemoryPersistence::new();
        let first = commit_application_permission_store(&mut backend, &store, None).unwrap();
        let second =
            commit_application_permission_store(&mut backend, &store, Some(first)).unwrap();
        let recovered = recover_application_permission_store(&mut backend).unwrap();
        assert_eq!(recovered.commit, second);

        backend.checkpoints[second.checkpoint_slot()][0] ^= 1;
        let fallback = recover_application_permission_store(&mut backend).unwrap();
        assert_eq!(fallback.commit, first);

        let inactive_checkpoint = 1 - fallback.commit.checkpoint_slot();
        backend.checkpoints[inactive_checkpoint] =
            encode_application_permission_checkpoint(&store, 99).unwrap();
        assert_eq!(
            recover_application_permission_store(&mut backend)
                .unwrap()
                .commit,
            first
        );
    }

    #[test]
    fn every_prepublication_failure_preserves_the_previous_commit() {
        let store = ApplicationPermissionStore::new();
        for fail_at in 0..3 {
            let mut backend = FailingPersistence::new();
            let first = commit_application_permission_store(&mut backend, &store, None).unwrap();
            backend.arm(fail_at);
            assert!(matches!(
                commit_application_permission_store(&mut backend, &store, Some(first)),
                Err(ApplicationPermissionPersistenceError::Storage(()))
            ));
            assert_eq!(
                recover_application_permission_store(&mut backend)
                    .unwrap()
                    .commit,
                first
            );
        }

        let mut backend = FailingPersistence::new();
        let first = commit_application_permission_store(&mut backend, &store, None).unwrap();
        backend.arm(3);
        assert!(commit_application_permission_store(&mut backend, &store, Some(first)).is_err());
        assert_eq!(
            recover_application_permission_store(&mut backend)
                .unwrap()
                .commit
                .sequence(),
            first.sequence() + 1
        );
    }
}
