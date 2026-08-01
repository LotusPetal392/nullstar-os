#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionError {
    Missing,
    Ambiguous,
}

pub fn select_unique_partition_by_uuid(
    candidates: impl IntoIterator<Item = (u32, [u8; 16])>,
    expected_uuid: [u8; 16],
) -> Result<u32, SelectionError> {
    let mut matching_partition = None;
    for (partition_index, filesystem_uuid) in candidates {
        if filesystem_uuid != expected_uuid {
            continue;
        }
        if matching_partition.is_some() {
            return Err(SelectionError::Ambiguous);
        }
        matching_partition = Some(partition_index);
    }
    matching_partition.ok_or(SelectionError::Missing)
}

#[cfg(test)]
mod tests {
    use super::{SelectionError, select_unique_partition_by_uuid};

    const PRIMARY_UUID: [u8; 16] = [1; 16];
    const OTHER_UUID: [u8; 16] = [2; 16];

    #[test]
    fn selection_uses_uuid_not_candidate_or_partition_order() {
        assert_eq!(
            select_unique_partition_by_uuid([(3, OTHER_UUID), (4, PRIMARY_UUID)], PRIMARY_UUID),
            Ok(4)
        );
        assert_eq!(
            select_unique_partition_by_uuid([(4, PRIMARY_UUID), (3, OTHER_UUID)], PRIMARY_UUID),
            Ok(4)
        );
    }

    #[test]
    fn selection_rejects_missing_uuid_without_falling_back() {
        assert_eq!(
            select_unique_partition_by_uuid([(3, OTHER_UUID)], PRIMARY_UUID),
            Err(SelectionError::Missing)
        );
    }

    #[test]
    fn selection_rejects_duplicate_uuid_as_ambiguous() {
        assert_eq!(
            select_unique_partition_by_uuid([(3, PRIMARY_UUID), (4, PRIMARY_UUID)], PRIMARY_UUID),
            Err(SelectionError::Ambiguous)
        );
    }
}
