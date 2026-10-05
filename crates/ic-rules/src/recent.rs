//! A bounded map: what the engine remembers about many objects, forgetting
//! the entries written longest ago first.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

/// Keeps at most `capacity` entries. Writing an entry makes it the newest;
/// when full, the entry written longest ago is forgotten.
#[derive(Clone, Debug)]
pub(crate) struct Recent<K, V> {
    /// Each value with the sequence number of its latest write.
    entries: HashMap<K, (V, u64)>,
    /// Keys in write order with the write's sequence number. Pairs whose
    /// key was written again or removed since are stale and skipped.
    order: VecDeque<(K, u64)>,
    next: u64,
    capacity: usize,
}

impl<K: Clone + Eq + Hash, V> Recent<K, V> {
    /// An empty map for `capacity` entries (at least one).
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            next: 0,
            capacity: capacity.max(1),
        }
    }

    /// The value for `key`, if remembered.
    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        self.entries.get(key).map(|(value, _)| value)
    }

    /// Remembers `value` for `key` as the newest entry, forgetting the
    /// oldest one when full.
    pub(crate) fn insert(&mut self, key: K, value: V) {
        let sequence = self.next;
        self.next = self.next.wrapping_add(1);
        self.entries.insert(key.clone(), (value, sequence));
        self.order.push_back((key, sequence));
        while self.entries.len() > self.capacity {
            let Some((key, sequence)) = self.order.pop_front() else {
                break;
            };
            if self.is_current(&key, sequence) {
                self.entries.remove(&key);
            }
        }
        // Rewrites leave stale pairs behind; drop them now and then so the
        // order stays proportional to the entries.
        if self.order.len() > 2 * self.capacity + 16 {
            let entries = &self.entries;
            self.order.retain(|(key, sequence)| {
                entries
                    .get(key)
                    .is_some_and(|(_, current)| current == sequence)
            });
        }
    }

    /// Forgets `key`, returning its value.
    pub(crate) fn remove(&mut self, key: &K) -> Option<V> {
        self.entries.remove(key).map(|(value, _)| value)
    }

    fn is_current(&self, key: &K, sequence: u64) -> bool {
        self.entries
            .get(key)
            .is_some_and(|(_, current)| *current == sequence)
    }

    /// Number of remembered entries.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_and_forgets() {
        let mut recent = Recent::new(10);
        recent.insert("a", 1);
        recent.insert("a", 2);
        assert_eq!(recent.get(&"a"), Some(&2));
        assert_eq!(recent.len(), 1);
        assert_eq!(recent.remove(&"a"), Some(2));
        assert_eq!(recent.get(&"a"), None);
        assert_eq!(recent.remove(&"a"), None);
    }

    #[test]
    fn forgets_the_entry_written_longest_ago() {
        let mut recent = Recent::new(3);
        for key in ["a", "b", "c"] {
            recent.insert(key, 0);
        }
        // Rewriting "a" makes it the newest.
        recent.insert("a", 1);
        recent.insert("d", 0);
        assert_eq!(recent.get(&"b"), None, "the oldest write goes first");
        assert_eq!(recent.get(&"a"), Some(&1));
        assert!(recent.get(&"c").is_some() && recent.get(&"d").is_some());
        assert_eq!(recent.len(), 3);
    }

    #[test]
    fn stays_bounded_under_rewrites_and_removals() {
        let mut recent = Recent::new(4);
        for round in 0..10_000_u32 {
            recent.insert(round % 7, round);
            if round % 3 == 0 {
                recent.remove(&(round % 5));
            }
        }
        assert!(recent.len() <= 4);
        assert!(recent.order.len() <= 2 * 4 + 16 + 1);
    }

    #[test]
    fn capacity_is_at_least_one() {
        let mut recent = Recent::new(0);
        recent.insert("a", ());
        assert!(recent.get(&"a").is_some());
        recent.insert("b", ());
        assert!(recent.get(&"a").is_none());
        assert!(recent.get(&"b").is_some());
    }
}
