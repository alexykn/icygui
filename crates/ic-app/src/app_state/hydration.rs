//! Which objects to ask the core for full details (output, perfdata,
//! links), and which were asked for recently.
//!
//! Services load lean (docs/performance.md); the UI offers the engine the
//! rows on screen (and a host pane's service rows), debounced, so
//! scrolling through 30 000 rows costs a request for the rows it stops on,
//! not for every row it passes. The engine fetches those it doesn't hold
//! current: lean services (no output yet) and, after quiet mode, those
//! whose result a check may have replaced meanwhile (ahead of its refresh
//! of the other problems); the others cost nothing. The object a pane
//! shows is asked for on its own, ahead of these (`Command::Focus`,
//! `AppState::focus`). An object asked for within the last five minutes
//! isn't asked for again (a check that never ran still has no output after
//! its details came), so revisiting rows costs nothing; waking up from
//! quiet mode forgets them (the engine dropped what was asked for
//! meanwhile, and what it held may have gone stale).

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

/// Whether a list row for `key` is worth offering the engine: a service
/// that has been checked (pending ones have nothing to load) or a host
/// with a result. The engine decides what to fetch (see the module notes).
pub(crate) fn row_worth_asking(snapshot: &Snapshot, key: &ObjectKey) -> bool {
    match key {
        ObjectKey::Service { key } => snapshot
            .services
            .get(key)
            .is_some_and(|service| service.state != ServiceState::Pending),
        ObjectKey::Host { name } => snapshot
            .hosts
            .get(name)
            .is_some_and(|host| host.check.result.is_some()),
    }
}

/// Whether a list row for `key` lacks its output: a service that has been
/// checked but whose output isn't loaded (pending services have none to
/// load; hosts always load in full).
#[cfg(test)]
pub(crate) fn row_needs_details(snapshot: &Snapshot, key: &ObjectKey) -> bool {
    let ObjectKey::Service { key } = key else {
        return false;
    };
    snapshot.services.get(key).is_some_and(|service| {
        service.check.result.is_none() && service.state != ServiceState::Pending
    })
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

    /// Forgets everything: the environment woke up from quiet mode, whose
    /// engine dropped what was asked for meanwhile, so the rows on screen
    /// are asked for again.
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
    fn checked_rows_are_offered() {
        let mut snapshot = snapshot();
        let mut host = Host::new("h");
        host.check.result = Some(CheckResult::default());
        snapshot.hosts = Arc::new([(host.name.clone(), Arc::new(host))].into_iter().collect());
        for (key, offered) in [
            (ObjectKey::service("h", "lean"), true),
            (ObjectKey::service("h", "full"), true),
            (ObjectKey::service("h", "pending"), false),
            (ObjectKey::service("h", "gone"), false),
            (ObjectKey::host("h"), true),
            (ObjectKey::host("unknown"), false),
        ] {
            assert_eq!(row_worth_asking(&snapshot, &key), offered, "{key}");
        }
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
