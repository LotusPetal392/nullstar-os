use alloc::{collections::VecDeque, vec::Vec};

/// Stores waitable process results separately from bounded diagnostic history.
///
/// A result leaves `pending` exactly once when its owner waits for it. History is
/// observational only and cannot make an already-consumed result waitable again.
pub struct CompletionQueue<T> {
    pending: Vec<T>,
    history: VecDeque<T>,
    history_limit: usize,
}

impl<T> CompletionQueue<T> {
    pub const fn new(history_limit: usize) -> Self {
        Self {
            pending: Vec::new(),
            history: VecDeque::new(),
            history_limit,
        }
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn history(&self) -> &VecDeque<T> {
        &self.history
    }

    pub fn take_pending_where(&mut self, predicate: impl FnMut(&T) -> bool) -> Option<T> {
        let index = self.pending.iter().position(predicate)?;
        Some(self.pending.remove(index))
    }

    pub fn discard_pending_where(&mut self, mut predicate: impl FnMut(&T) -> bool) -> usize {
        let previous_len = self.pending.len();
        self.pending.retain(|item| !predicate(item));
        previous_len.saturating_sub(self.pending.len())
    }
}

impl<T: Clone> CompletionQueue<T> {
    pub fn record(&mut self, result: T, waitable: bool) {
        if waitable {
            self.pending.push(result.clone());
        }

        if self.history_limit == 0 {
            return;
        }
        if self.history.len() == self.history_limit {
            self.history.pop_front();
        }
        self.history.push_back(result);
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::CompletionQueue;

    #[test]
    fn completion_is_consumed_exactly_once() {
        let mut queue = CompletionQueue::new(4);
        queue.record((7_u64, 42_u64), true);

        assert_eq!(
            queue.take_pending_where(|(process_id, _)| *process_id == 7),
            Some((7, 42))
        );
        assert_eq!(
            queue.take_pending_where(|(process_id, _)| *process_id == 7),
            None
        );
        assert_eq!(
            queue.history().iter().copied().collect::<Vec<_>>(),
            [(7, 42)]
        );
    }

    #[test]
    fn non_waitable_result_is_history_only() {
        let mut queue = CompletionQueue::new(4);
        queue.record(11_u64, false);

        assert_eq!(queue.pending_len(), 0);
        assert_eq!(queue.history().back(), Some(&11));
    }

    #[test]
    fn diagnostic_history_is_bounded() {
        let mut queue = CompletionQueue::new(2);
        queue.record(1_u64, false);
        queue.record(2_u64, false);
        queue.record(3_u64, false);

        assert_eq!(queue.history().iter().copied().collect::<Vec<_>>(), [2, 3]);
    }

    #[test]
    fn orphan_cleanup_discards_matching_pending_results() {
        let mut queue = CompletionQueue::new(4);
        queue.record((10_u64, 1_u64), true);
        queue.record((11_u64, 2_u64), true);

        assert_eq!(
            queue.discard_pending_where(|(_, parent_id)| *parent_id == 1),
            1
        );
        assert_eq!(queue.pending_len(), 1);
    }
}
