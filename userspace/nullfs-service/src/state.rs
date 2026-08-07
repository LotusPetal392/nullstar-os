use alloc::vec::Vec;

use nullfs_core::{NodeId, OpenHandle};
use nullfs_format::NodeKind;
use userspace::filesystem_service::{MAX_NODE_REFERENCES_PER_SESSION, MAX_SESSIONS};

pub const MAX_OPEN_RECORDS: usize = MAX_SESSIONS * MAX_NODE_REFERENCES_PER_SESSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeIdentity {
    pub opaque_id: u64,
    pub node: NodeId,
    pub generation: u64,
    pub kind: NodeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeMapError {
    NoSpace,
    IdentityMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeReservation {
    index: usize,
    opaque_id: u64,
}

impl NodeReservation {
    pub const fn opaque_id(self) -> u64 {
        self.opaque_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeSlot {
    Vacant,
    Reserved(u64),
    Occupied(NodeIdentity),
}

pub struct NodeMap {
    entries: Vec<NodeSlot>,
    service_generation: u64,
    next_sequence: u32,
}

impl NodeMap {
    pub fn new(
        service_generation: u64,
        capacity: usize,
        root: NodeId,
        root_generation: u64,
        root_kind: NodeKind,
    ) -> Result<Self, NodeMapError> {
        if capacity == 0 {
            return Err(NodeMapError::NoSpace);
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| NodeMapError::NoSpace)?;
        entries.resize(capacity, NodeSlot::Vacant);
        entries[0] = NodeSlot::Occupied(NodeIdentity {
            opaque_id: userspace::filesystem::protocol::ROOT_NODE_ID,
            node: root,
            generation: root_generation,
            kind: root_kind,
        });
        Ok(Self {
            entries,
            service_generation,
            next_sequence: 2,
        })
    }

    pub fn resolve(&self, opaque_id: u64) -> Option<NodeIdentity> {
        if opaque_id == userspace::filesystem::protocol::INVALID_ID {
            return None;
        }
        self.entries.iter().find_map(|slot| match slot {
            NodeSlot::Occupied(entry) if entry.opaque_id == opaque_id => Some(*entry),
            _ => None,
        })
    }

    pub fn vacant_index(&self) -> Option<usize> {
        self.next_sequence.checked_add(1)?;
        self.entries
            .iter()
            .position(|slot| matches!(slot, NodeSlot::Vacant))
    }

    pub fn reserve(&mut self) -> Result<NodeReservation, NodeMapError> {
        let index = self.vacant_index().ok_or(NodeMapError::NoSpace)?;
        let opaque_id = self.allocate_opaque_id()?;
        self.entries[index] = NodeSlot::Reserved(opaque_id);
        Ok(NodeReservation { index, opaque_id })
    }

    pub fn rollback(&mut self, reservation: NodeReservation) -> bool {
        let Some(slot) = self.entries.get_mut(reservation.index) else {
            return false;
        };
        if !matches!(slot, NodeSlot::Reserved(opaque_id) if *opaque_id == reservation.opaque_id) {
            return false;
        }
        *slot = NodeSlot::Vacant;
        true
    }

    pub fn install(
        &mut self,
        reservation: NodeReservation,
        node: NodeId,
        generation: u64,
        kind: NodeKind,
    ) -> Result<u64, NodeMapError> {
        if !matches!(
            self.entries.get(reservation.index),
            Some(NodeSlot::Reserved(opaque_id)) if *opaque_id == reservation.opaque_id
        ) {
            return Err(NodeMapError::NoSpace);
        }

        if let Some(existing) = self.find_exact(node, generation) {
            let _ = self.rollback(reservation);
            return (existing.kind == kind)
                .then_some(existing.opaque_id)
                .ok_or(NodeMapError::IdentityMismatch);
        }

        self.entries[reservation.index] = NodeSlot::Occupied(NodeIdentity {
            opaque_id: reservation.opaque_id,
            node,
            generation,
            kind,
        });
        Ok(reservation.opaque_id)
    }

    pub fn insert_at(
        &mut self,
        index: usize,
        node: NodeId,
        generation: u64,
        kind: NodeKind,
    ) -> Result<u64, NodeMapError> {
        if !matches!(self.entries.get(index), Some(NodeSlot::Vacant)) {
            return Err(NodeMapError::NoSpace);
        }
        let opaque_id = self.allocate_opaque_id()?;
        self.entries[index] = NodeSlot::Reserved(opaque_id);
        self.install(NodeReservation { index, opaque_id }, node, generation, kind)
    }

    pub fn intern(
        &mut self,
        node: NodeId,
        generation: u64,
        kind: NodeKind,
    ) -> Result<u64, NodeMapError> {
        if let Some(existing) = self.find_exact(node, generation) {
            return (existing.kind == kind)
                .then_some(existing.opaque_id)
                .ok_or(NodeMapError::IdentityMismatch);
        }

        let reservation = self.reserve()?;
        self.install(reservation, node, generation, kind)
    }

    pub fn retire(&mut self, opaque_id: u64) -> Option<NodeIdentity> {
        let index = self
            .entries
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(index, slot)| {
                matches!(slot, NodeSlot::Occupied(identity) if identity.opaque_id == opaque_id)
                    .then_some(index)
            })?;
        self.retire_index(index)
    }

    pub fn retire_exact(&mut self, node: NodeId, generation: u64) -> Option<NodeIdentity> {
        let index = self.entries.iter().enumerate().skip(1).find_map(|(index, slot)| {
            matches!(slot, NodeSlot::Occupied(identity) if identity.node == node && identity.generation == generation)
                .then_some(index)
        })?;
        self.retire_index(index)
    }

    fn find_exact(&self, node: NodeId, generation: u64) -> Option<NodeIdentity> {
        self.entries.iter().find_map(|slot| match slot {
            NodeSlot::Occupied(entry) if entry.node == node && entry.generation == generation => {
                Some(*entry)
            }
            _ => None,
        })
    }

    fn retire_index(&mut self, index: usize) -> Option<NodeIdentity> {
        let slot = self.entries.get_mut(index)?;
        let NodeSlot::Occupied(identity) = *slot else {
            return None;
        };
        *slot = NodeSlot::Vacant;
        Some(identity)
    }

    fn allocate_opaque_id(&mut self) -> Result<u64, NodeMapError> {
        let opaque_id = self.next_opaque_id();
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(NodeMapError::NoSpace)?;
        Ok(opaque_id)
    }

    fn next_opaque_id(&self) -> u64 {
        const OPAQUE_TAG: u64 = 1 << 63;
        let generation_tag = (self.service_generation & 0x7fff_ffff) << 32;
        OPAQUE_TAG | generation_tag | u64::from(self.next_sequence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenRecord {
    pub session_id: u64,
    pub session_generation: u64,
    pub opaque_node: u64,
    pub handle: OpenHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenTableError {
    NoSpace,
}

pub struct OpenTable {
    records: [Option<OpenRecord>; MAX_OPEN_RECORDS],
}

impl OpenTable {
    pub const fn new() -> Self {
        Self {
            records: [None; MAX_OPEN_RECORDS],
        }
    }

    pub fn vacant_index(&self) -> Option<usize> {
        self.records.iter().position(Option::is_none)
    }

    pub fn first(&self) -> Option<(usize, OpenRecord)> {
        self.records
            .iter()
            .enumerate()
            .find_map(|(index, record)| record.map(|record| (index, record)))
    }

    pub fn insert_at(&mut self, index: usize, record: OpenRecord) -> Result<(), OpenTableError> {
        let slot = self
            .records
            .get_mut(index)
            .filter(|slot| slot.is_none())
            .ok_or(OpenTableError::NoSpace)?;
        *slot = Some(record);
        Ok(())
    }

    pub fn find_one(
        &self,
        session_id: u64,
        session_generation: u64,
        opaque_node: u64,
    ) -> Option<(usize, OpenHandle)> {
        self.find_one_record(session_id, session_generation, opaque_node)
            .map(|(index, record)| (index, record.handle))
    }

    pub fn find_one_record(
        &self,
        session_id: u64,
        session_generation: u64,
        opaque_node: u64,
    ) -> Option<(usize, OpenRecord)> {
        self.records.iter().enumerate().find_map(|(index, record)| {
            let record = record.as_ref()?;
            (record.session_id == session_id
                && record.session_generation == session_generation
                && record.opaque_node == opaque_node)
                .then_some((index, *record))
        })
    }

    pub fn is_open(&self, node: NodeId, generation: u64) -> bool {
        self.records_for_identity(node, generation).next().is_some()
    }

    pub fn records_for_identity(
        &self,
        node: NodeId,
        generation: u64,
    ) -> impl Iterator<Item = OpenRecord> + '_ {
        self.records
            .iter()
            .flatten()
            .copied()
            .filter(move |record| {
                record.handle.node == node && record.handle.generation == generation
            })
    }

    pub fn is_open_for_session(
        &self,
        session_id: u64,
        session_generation: u64,
        opaque_node: u64,
    ) -> bool {
        self.find_one_record(session_id, session_generation, opaque_node)
            .is_some()
    }

    pub fn remove(&mut self, index: usize) -> Option<OpenRecord> {
        self.records.get_mut(index)?.take()
    }

    pub fn find_one_for_session(
        &self,
        session_id: u64,
        session_generation: u64,
    ) -> Option<(usize, OpenRecord)> {
        self.records.iter().enumerate().find_map(|(index, record)| {
            let record = record.as_ref()?;
            (record.session_id == session_id && record.session_generation == session_generation)
                .then_some((index, *record))
        })
    }

    pub fn count_for_session(&self, session_id: u64, session_generation: u64) -> usize {
        self.records
            .iter()
            .flatten()
            .filter(|record| {
                record.session_id == session_id && record.session_generation == session_generation
            })
            .count()
    }
}

impl Default for OpenTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use nullfs_core::{NodeId, OpenHandle};
    use nullfs_format::NodeKind;

    use super::{NodeMap, NodeMapError, OpenRecord, OpenTable};

    #[test]
    fn opaque_nodes_are_generation_local_deduplicated_and_not_core_ids() {
        let mut nodes = NodeMap::new(9, 4, NodeId(1), 4, NodeKind::Directory).expect("node map");
        assert_eq!(nodes.resolve(1).expect("root").node, NodeId(1));

        let first = nodes
            .intern(NodeId(2), 7, NodeKind::Regular)
            .expect("first opaque node");
        let duplicate = nodes
            .intern(NodeId(2), 7, NodeKind::Regular)
            .expect("deduplicated opaque node");
        let replacement = nodes
            .intern(NodeId(2), 8, NodeKind::Regular)
            .expect("new inode generation");

        assert_eq!(first, duplicate);
        assert_ne!(first, replacement);
        assert_ne!(first, 2);
        assert_ne!(first, 0);
        assert_ne!(first, 1);
    }

    #[test]
    fn opaque_node_map_is_bounded_and_rejects_kind_disagreement() {
        const CAPACITY: usize = 4;
        let mut nodes =
            NodeMap::new(11, CAPACITY, NodeId(1), 1, NodeKind::Directory).expect("node map");
        assert_eq!(
            nodes.intern(NodeId(1), 1, NodeKind::Regular),
            Err(NodeMapError::IdentityMismatch)
        );
        for index in 1..CAPACITY {
            nodes
                .intern(NodeId(index as u64 + 1), 1, NodeKind::Regular)
                .expect("bounded mapping");
        }
        assert_eq!(
            nodes.intern(NodeId(10_000), 1, NodeKind::Regular),
            Err(NodeMapError::NoSpace)
        );
    }

    #[test]
    fn node_reservations_can_be_rolled_back_and_installed() {
        let mut nodes = NodeMap::new(3, 2, NodeId(1), 1, NodeKind::Directory).expect("node map");

        let abandoned = nodes.reserve().expect("reserve only non-root slot");
        assert_eq!(nodes.resolve(abandoned.opaque_id()), None);
        assert_eq!(nodes.reserve(), Err(NodeMapError::NoSpace));
        assert!(nodes.rollback(abandoned));
        assert!(!nodes.rollback(abandoned));

        let committed = nodes.reserve().expect("reserve rolled-back slot");
        assert_ne!(committed.opaque_id(), abandoned.opaque_id());
        let opaque_id = nodes
            .install(committed, NodeId(2), 5, NodeKind::Regular)
            .expect("install reserved identity");
        assert_eq!(opaque_id, committed.opaque_id());
        assert_eq!(
            nodes.resolve(opaque_id).expect("installed node").node,
            NodeId(2)
        );
        assert_eq!(nodes.resolve(abandoned.opaque_id()), None);
    }

    #[test]
    fn retired_opaque_ids_are_stale_and_slots_are_reusable() {
        let mut nodes = NodeMap::new(5, 2, NodeId(1), 1, NodeKind::Directory).expect("node map");
        let mut retired_ids = Vec::new();

        for generation in 1..=16 {
            let opaque_id = nodes
                .intern(NodeId(2), generation, NodeKind::Regular)
                .expect("reuse non-root slot");
            assert!(!retired_ids.contains(&opaque_id));
            assert_eq!(
                nodes.resolve(opaque_id).expect("live node").generation,
                generation
            );

            let retired = if generation % 2 == 0 {
                nodes.retire_exact(NodeId(2), generation)
            } else {
                nodes.retire(opaque_id)
            }
            .expect("retire live node");
            assert_eq!(retired.opaque_id, opaque_id);
            assert_eq!(nodes.resolve(opaque_id), None);
            retired_ids.push(opaque_id);
        }

        assert_eq!(retired_ids.len(), 16);
    }

    #[test]
    fn root_identity_cannot_be_retired() {
        let mut nodes = NodeMap::new(7, 2, NodeId(11), 9, NodeKind::Directory).expect("node map");

        assert_eq!(
            nodes.retire(userspace::filesystem::protocol::ROOT_NODE_ID),
            None
        );
        assert_eq!(nodes.retire_exact(NodeId(11), 9), None);
        assert_eq!(
            nodes
                .resolve(userspace::filesystem::protocol::ROOT_NODE_ID)
                .expect("root remains live")
                .node,
            NodeId(11)
        );
    }

    #[test]
    fn exact_open_identity_is_detected_across_sessions() {
        let mut opens = OpenTable::new();
        for (index, session_id, opaque_node) in [(0, 7, 90), (1, 8, 91)] {
            opens
                .insert_at(
                    index,
                    OpenRecord {
                        session_id,
                        session_generation: 4,
                        opaque_node,
                        handle: OpenHandle {
                            id: index as u64 + 1,
                            node: NodeId(6),
                            generation: 12,
                            kind: NodeKind::Regular,
                        },
                    },
                )
                .expect("open slot");
        }

        assert!(opens.is_open(NodeId(6), 12));
        assert!(!opens.is_open(NodeId(6), 13));
        assert_eq!(
            opens
                .records_for_identity(NodeId(6), 12)
                .map(|record| record.session_id)
                .collect::<Vec<_>>(),
            vec![7, 8]
        );
        assert!(opens.records_for_identity(NodeId(6), 13).next().is_none());
        assert!(opens.is_open_for_session(7, 4, 90));
        assert!(!opens.is_open_for_session(7, 4, 91));
        let (first_index, first_record) = opens.find_one_record(7, 4, 90).expect("full record");
        assert_eq!(first_index, 0);
        assert_eq!(first_record.handle.id, 1);

        assert_eq!(opens.remove(0), Some(first_record));
        assert!(opens.is_open(NodeId(6), 12));
        assert!(opens.remove(1).is_some());
        assert!(!opens.is_open(NodeId(6), 12));
    }

    #[test]
    fn open_table_closes_duplicates_one_at_a_time_and_drains_by_session() {
        let mut opens = OpenTable::new();
        for id in 1..=3 {
            opens
                .insert_at(
                    id as usize - 1,
                    OpenRecord {
                        session_id: if id == 3 { 8 } else { 7 },
                        session_generation: 5,
                        opaque_node: 99,
                        handle: OpenHandle {
                            id,
                            node: NodeId(4),
                            generation: 3,
                            kind: NodeKind::Regular,
                        },
                    },
                )
                .expect("open slot");
        }

        let (index, handle) = opens.find_one(7, 5, 99).expect("first duplicate");
        assert_eq!(handle.id, 1);
        assert_eq!(opens.remove(index).expect("remove one").handle.id, 1);
        assert_eq!(opens.count_for_session(7, 5), 1);
        let (index, record) = opens.find_one_for_session(7, 5).expect("find remaining");
        assert_eq!(record.handle.id, 2);
        assert_eq!(opens.remove(index), Some(record));
        assert_eq!(opens.count_for_session(7, 5), 0);

        let mut drained = Vec::new();
        while let Some((index, record)) = opens.first() {
            assert_eq!(opens.remove(index), Some(record));
            drained.push(record.handle.id);
        }
        assert_eq!(drained, vec![3]);
        assert_eq!(opens.first(), None);
    }
}
