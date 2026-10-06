//! Which objects to ask the core for full details (output, perfdata,
//! links), and which were asked for recently.
//!
//! Services load lean (docs/performance.md); the UI asks for the details
//! of the rows on screen that have no output yet and of an opened pane,
//! debounced, so scrolling through 30 000 rows costs a request for the
//! rows it stops on, not for every row it passes. An object asked for
//! within the last five minutes isn't asked for again (a check that never
//! ran still has no output after its details came), so revisiting rows
//! costs nothing.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ic_core::snapshot::Snapshot;
use ic_model::{ObjectKey, ServiceState};

/// An object isn't asked for again within this time.
const AGAIN_AFTER: Duration = Duration::from_mins(5);
/// The most keys remembered; older ones are forgotten first.
const MAX_REMEMBERED: usize = 20_000;
/// The most keys in one `Hydrate` command (a screenful is far less).
pub(crate) const MAX_PER_REQUEST: usize = 500;

/// Whether a list row for `key` lacks details worth fetching: a service
/// that has been checked but whose output isn't loaded (pending services
/// have none to load; hosts always load in full).
pub(crate) fn row_needs_details(snapshot: &Snapshot, key: &ObjectKey) -> bool {
    let ObjectKey::Service { key } = key else {
        return false;
    };
    snapshot.services.get(key).is_some_and(|service| {
        service.check.result.is_none() && service.state != ServiceState::Pending
    })
}

/// Whether an opened pane for `key` should ask for full details: any known
/// service (lean ones lack their links even after a check result came;
/// the core skips services it holds in full).
pub(crate) fn pane_wants_details(snapshot: &Snapshot, key: &ObjectKey) -> bool {
    match key {
        ObjectKey::Service { key } => snapshot.services.contains_key(key),
        ObjectKey::Host { .. } => false,
    }
}

/// Remembers what was asked for.
#[derive(Debug, Default)]
pub(crate) struct Hydration {
    requested: HashMap<ObjectKey, Instant>,
}

impl Hydration {
    /// The keys of `keys` not asked for within the last five minutes (each
    /// once, in order, at most [`MAX_PER_REQUEST`]), now remembered as
    /// asked for at `now`.
    pub(crate) fn take_new(
        &mut self,
        keys: impl IntoIterator<Item = ObjectKey>,
        now: Instant,
    ) -> Vec<ObjectKey> {
        let mut fresh = Vec::new();
        for key in keys {
            if fresh.len() >= MAX_PER_REQUEST {
                break;
            }
            let recent = self
                .requested
                .get(&key)
                .is_some_and(|at| now.saturating_duration_since(*at) < AGAIN_AFTER);
            if !recent {
                self.requested.insert(key.clone(), now);
                fresh.push(key);
            }
        }
        if self.requested.len() > MAX_REMEMBERED {
            self.requested
                .retain(|_, at| now.saturating_duration_since(*at) < AGAIN_AFTER);
        }
        if self.requested.len() > MAX_REMEMBERED {
            // Still too many within five minutes: start over.
            self.requested.clear();
        }
        fresh
    }

    /// Forgets everything (another server, or a reload replaced the
    /// objects).
    pub(crate) fn forget(&mut self) {
        self.requested.clear();
    }

    /// How many keys are remembered.
    #[cfg(test)]
    pub(crate) fn remembered(&self) -> usize {
        self.requested.len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ic_model::{CheckResult, Host, Service};

    use super::*;

    fn snapshot() -> Snapshot {
        let mut lean = Service::new("h", "lean");
        lean.state = ServiceState::Ok;
        let mut full = Service::new("h", "full");
        full.state = ServiceState::Critical;
        full.check.result = Some(CheckResult::default());
        let pending = Service::new("h", "pending");
        Snapshot {
            hosts: Arc::new(
                [(Host::new("h").name.clone(), Arc::new(Host::new("h")))]
                    .into_iter()
                    .collect(),
            ),
            services: Arc::new(
                [lean, full, pending]
                    .into_iter()
                    .map(|service| (service.key.clone(), Arc::new(service)))
                    .collect(),
            ),
            ..Snapshot::default()
        }
    }

    #[test]
    fn rows_without_output_need_details() {
        let snapshot = snapshot();
        assert!(row_needs_details(
            &snapshot,
            &ObjectKey::service("h", "lean")
        ));
        assert!(!row_needs_details(
            &snapshot,
            &ObjectKey::service("h", "full")
        ));
        assert!(
            !row_needs_details(&snapshot, &ObjectKey::service("h", "pending")),
            "nothing to load before the first check"
        );
        assert!(!row_needs_details(&snapshot, &ObjectKey::host("h")));
        assert!(!row_needs_details(
            &snapshot,
            &ObjectKey::service("h", "gone")
        ));
    }

    #[test]
    fn panes_ask_for_every_known_service() {
        let snapshot = snapshot();
        assert!(pane_wants_details(
            &snapshot,
            &ObjectKey::service("h", "full")
        ));
        assert!(!pane_wants_details(&snapshot, &ObjectKey::host("h")));
        assert!(!pane_wants_details(
            &snapshot,
            &ObjectKey::service("h", "gone")
        ));
    }

    #[test]
    fn recent_requests_are_not_repeated() {
        let mut hydration = Hydration::default();
        let start = Instant::now();
        let a = ObjectKey::service("h", "a");
        let b = ObjectKey::service("h", "b");
        assert_eq!(
            hydration.take_new([a.clone(), a.clone(), b.clone()], start),
            [a.clone(), b.clone()]
        );
        assert!(
            hydration
                .take_new([a.clone()], start + Duration::from_mins(1))
                .is_empty()
        );
        assert_eq!(
            hydration.take_new([a.clone()], start + AGAIN_AFTER),
            std::slice::from_ref(&a),
            "asked again after five minutes"
        );
        hydration.forget();
        assert_eq!(hydration.take_new([b.clone()], start), [b]);
    }

    #[test]
    fn requests_and_memory_are_bounded() {
        let mut hydration = Hydration::default();
        let start = Instant::now();
        let keys = (0..2_000).map(|index| ObjectKey::service("h", &format!("s{index}")));
        assert_eq!(hydration.take_new(keys, start).len(), MAX_PER_REQUEST);
        for round in 0..50 {
            let keys = (0..MAX_PER_REQUEST)
                .map(|index| ObjectKey::service("h", &format!("r{round}-{index}")));
            hydration.take_new(keys, start + Duration::from_secs(round));
        }
        assert!(hydration.remembered() <= MAX_REMEMBERED);
    }
}
