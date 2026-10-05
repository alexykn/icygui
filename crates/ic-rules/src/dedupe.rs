//! A bounded memory of emitted intent ids.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

/// Remembers the most recent `capacity` ids; older ones are forgotten
/// first in, first out.
#[derive(Clone, Debug)]
pub(crate) struct Dedupe {
    seen: HashSet<Arc<str>>,
    order: VecDeque<Arc<str>>,
    capacity: usize,
}

impl Dedupe {
    /// An empty memory for `capacity` ids (at least one).
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Whether `id` was inserted and not forgotten yet.
    pub(crate) fn contains(&self, id: &str) -> bool {
        self.seen.contains(id)
    }

    /// Remembers `id`, forgetting the oldest id when full.
    pub(crate) fn insert(&mut self, id: &str) {
        if self.seen.contains(id) {
            return;
        }
        let id: Arc<str> = Arc::from(id);
        self.seen.insert(Arc::clone(&id));
        self.order.push_back(id);
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
    }

    /// Number of remembered ids.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.seen.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_ids() {
        let mut dedupe = Dedupe::new(10);
        assert!(!dedupe.contains("a"));
        dedupe.insert("a");
        dedupe.insert("a");
        assert!(dedupe.contains("a"));
        assert_eq!(dedupe.len(), 1);
    }

    #[test]
    fn forgets_the_oldest_first_and_stays_bounded() {
        let mut dedupe = Dedupe::new(3);
        for id in ["a", "b", "c", "d"] {
            dedupe.insert(id);
        }
        assert!(!dedupe.contains("a"));
        assert!(dedupe.contains("b") && dedupe.contains("c") && dedupe.contains("d"));

        for n in 0..1_000 {
            dedupe.insert(&n.to_string());
        }
        assert_eq!(dedupe.len(), 3);
        assert_eq!(dedupe.order.len(), 3);
        assert!(dedupe.contains("999"));
    }

    #[test]
    fn capacity_is_at_least_one() {
        let mut dedupe = Dedupe::new(0);
        dedupe.insert("a");
        assert!(dedupe.contains("a"));
        dedupe.insert("b");
        assert!(!dedupe.contains("a"));
        assert!(dedupe.contains("b"));
    }
}
