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

pub struct NodeMap {
    entries: Vec<Option<NodeIdentity>>,
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
        entries.resize(capacity, None);
        entries[0] = Some(NodeIdentity {
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
        self.entries
            .iter()
            .flatten()
            .find(|entry| entry.opaque_id == opaque_id)
            .copied()
    }

    pub fn intern(
        &mut self,
        node: NodeId,
        generation: u64,
        kind: NodeKind,
    ) -> Result<u64, NodeMapError> {
        if let Some(existing) = self
            .entries
            .iter()
            .flatten()
            .find(|entry| entry.node == node && entry.generation == generation)
        {
            return (existing.kind == kind)
                .then_some(existing.opaque_id)
                .ok_or(NodeMapError::IdentityMismatch);
        }

        let slot = self
            .entries
            .iter()
            .position(Option::is_none)
            .ok_or(NodeMapError::NoSpace)?;
        let opaque_id = self.next_opaque_id();
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(NodeMapError::NoSpace)?;
        self.entries[slot] = Some(NodeIdentity {
            opaque_id,
            node,
            generation,
            kind,
        });
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
        self.records.iter().enumerate().find_map(|(index, record)| {
            let record = record.as_ref()?;
            (record.session_id == session_id
                && record.session_generation == session_generation
                && record.opaque_node == opaque_node)
                .then_some((index, record.handle))
        })
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
    }
}
