//! Performance at production scale (docs/performance.md), measured in
//! whatever profile the tests run in (the dev profile optimises only the
//! dependencies, so the workspace crates run several times slower than in
//! a release build; the bounds below only catch pathologies).
//!
//! - The applier: 50 000 recorded events in under 3 s (release), none
//!   dropped. The events are recorded from `ic-mock` bursts (every object
//!   re-checked at Icinga's pace), then parsed, collapsed and applied in
//!   batches like the engine does, with a snapshot per batch (the
//!   copy-on-write cost included) and, optionally, the dashboards updated
//!   for every batch.
//! - Dashboards: 20 000 services × 10 dashboards, evaluated in full (well
//!   under a second in release) and incrementally (milliseconds).
//! - Memory of the store at 30 000 services, and of 10 dashboards over it,
//!   counted by a counting global allocator (this test binary only).
//!
//! The small runs are part of the normal test suite; the production-size
//! ones are ignored. Run each alone (the allocator counts every thread):
//! `cargo test -p ic-core --lib perf -- --ignored --nocapture --test-threads 1`.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use futures::StreamExt;
use ic_api::{Client, ConnectionSettings, Credentials, Detail, TlsSettings, Url};
use ic_config::{Dashboard, DashboardGroup, Environment, GroupBy, ObjectKind, Sort, SortKey, View};
use ic_mock::{MockConfig, MockServer, scenarios};
use ic_model::{EventKind, ObjectKey, Service, ServiceState, Timestamp};
use secrecy::SecretString;

use super::stream;
use crate::dashboards::{Dashboards, Data};
use crate::store::{Applied, Changes, Store};

/// Counts the bytes allocated and not yet freed, for the memory test.
mod counting {
    #![expect(
        unsafe_code,
        reason = "a global allocator that counts, for measuring memory in tests"
    )]

    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Bytes allocated and not freed.
    pub(super) static LIVE: AtomicUsize = AtomicUsize::new(0);

    /// The system allocator, counting.
    pub(super) struct Counting;

    // SAFETY: every call is forwarded to the system allocator with the same
    // arguments; only the byte count is added on the side.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
            let pointer = unsafe { System.alloc(layout) };
            if !pointer.is_null() {
                LIVE.fetch_add(layout.size(), Ordering::Relaxed);
            }
            pointer
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
            unsafe { System.dealloc(pointer, layout) };
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
            let moved = unsafe { System.realloc(pointer, layout, new_size) };
            if !moved.is_null() {
                LIVE.fetch_add(new_size, Ordering::Relaxed);
                LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            }
            moved
        }
    }

    #[global_allocator]
    static GLOBAL: Counting = Counting;
}

fn live_bytes() -> usize {
    counting::LIVE.load(std::sync::atomic::Ordering::Relaxed)
}

fn megabytes(bytes: usize) -> f64 {
    f64::from(u32::try_from(bytes / 1_024).unwrap_or(u32::MAX)) / 1_024.0
}

/// Records the raw event lines of `bursts` bursts of `hosts` × 15 services.
async fn record(hosts: usize, bursts: usize) -> (MockServer, Vec<Vec<u8>>) {
    let server = MockServer::start(MockConfig {
        check_rate: 20_000.0,
        event_buffer: 200_000,
        ..MockConfig::with_scenario(scenarios::large_with_hosts(hosts, 11))
    })
    .await
    .unwrap();
    let client = Client::new(ConnectionSettings::new(
        Url::parse(&server.url()).unwrap(),
        Credentials::Basic {
            username: "root".to_owned(),
            password: SecretString::from("icinga".to_owned()),
        },
        TlsSettings {
            pinned_sha256: Some(server.cert_sha256()),
            ..TlsSettings::default()
        },
    ))
    .unwrap();
    let mut lines = client
        .events("perf", &EventKind::ALL)
        .await
        .unwrap()
        .into_lines();
    let control = server.control();
    let mut recorded = Vec::new();
    for _ in 0..bursts {
        control.burst();
        loop {
            match tokio::time::timeout(Duration::from_millis(500), lines.next()).await {
                Ok(Some(Ok(line))) => recorded.push(line),
                Ok(other) => panic!("the stream ended: {other:?}"),
                Err(_) if control.queued_checks() == 0 => break,
                Err(_) => {}
            }
        }
    }
    (server, recorded)
}

/// Ten dashboards like a team would set up.
fn ten_dashboards() -> Environment {
    let services = |filter: &str, problems_only: bool| View {
        object_kind: ObjectKind::Services,
        filter: filter.to_owned(),
        problems_only,
        hide_handled: problems_only,
        sort: Sort::default(),
        group_by: GroupBy::None,
    };
    let mut host_problems = services("", true);
    host_problems.object_kind = ObjectKind::Hosts;
    let mut by_group = services(
        "regex(\"^disk\", service.name) && service.state >= 1",
        false,
    );
    by_group.group_by = GroupBy::HostGroup;
    let mut racks = services("host.vars.rack in [\"r01\", \"r02\", \"r03\"]", false);
    racks.group_by = GroupBy::Host;
    racks.sort = Sort {
        key: SortKey::LastStateChange,
        descending: true,
    };
    let views = [
        services("", true),
        host_problems,
        services("", false),
        services("host.vars.role == \"db\"", true),
        services(
            "\"disk\" in service.groups || service.check_command == \"disk\"",
            false,
        ),
        services("match(\"web-*\", host.name) && service.state != 0", false),
        services(
            "host.vars.team == \"team-3\" && service.name in [\"http\", \"cert\"]",
            false,
        ),
        services(
            "service.last_check_result.output.contains(\"CRITICAL\")",
            false,
        ),
        by_group,
        racks,
    ];
    Environment {
        groups: vec![DashboardGroup {
            id: "g".to_owned(),
            name: "team".to_owned(),
            dashboards: views
                .into_iter()
                .enumerate()
                .map(|(index, view)| Dashboard {
                    id: format!("d{index}"),
                    name: format!("d{index}"),
                    view,
                    ..Dashboard::default()
                })
                .collect(),
            ..DashboardGroup::default()
        }],
        ..Environment::default()
    }
}

fn data_of(store: &Store) -> Data {
    Data {
        hosts: Arc::clone(store.hosts()),
        services: Arc::clone(store.services()),
        host_groups: Arc::clone(store.host_groups()),
        service_groups: Arc::clone(store.service_groups()),
        now: Timestamp::now(),
    }
}

/// Applies `lines` to a store holding the mock's objects, with a snapshot
/// per batch and (with `dashboards`) the ten dashboards updated per batch.
/// Returns the time applying took, the time the dashboards took, and how
/// many events were applied.
fn apply(
    server: &MockServer,
    lines: Vec<Vec<u8>>,
    batch: usize,
    dashboards: bool,
) -> (Duration, Duration, usize) {
    let control = server.control();
    let mut store = Store::default();
    store.replace_hosts(control.hosts(), 0);
    store.replace_services(control.services(), Detail::Lean, 0);
    store.take_changes();
    let mut boards = Dashboards::default();
    boards.configure(&ten_dashboards());
    if dashboards {
        boards.update(&data_of(&store), &all(), false, &AtomicBool::new(false));
    }
    // The snapshot the UI holds, so every batch copies on write.
    let mut held = store.snapshot(0, Timestamp::EPOCH, Arc::default(), Arc::default());
    let numbered: Vec<(u64, Vec<u8>)> = (1..).zip(lines).collect();
    let mut applying = Duration::ZERO;
    let mut evaluating = Duration::ZERO;
    let mut applied = 0;
    for (index, chunk) in numbered.chunks(batch).enumerate() {
        let started = Instant::now();
        let events = stream::prepare(chunk.to_vec());
        for (seq, event) in events {
            if matches!(store.apply(seq, &event), Applied::Changed { .. }) {
                applied += 1;
            }
        }
        let changes = store.take_changes();
        held = store.snapshot(
            u64::try_from(index).unwrap_or(u64::MAX),
            Timestamp::EPOCH,
            Arc::default(),
            Arc::default(),
        );
        applying += started.elapsed();
        if dashboards {
            let started = Instant::now();
            boards.update(&data_of(&store), &changes, false, &AtomicBool::new(false));
            evaluating += started.elapsed();
        }
    }
    drop(held);
    // Nothing was lost: the store matches the mock.
    for truth in control.services() {
        let stored = store.check(&truth.object_key()).unwrap();
        assert_eq!(stored.output(), truth.check.output(), "{}", truth.key);
        assert_eq!(stored.state_type, truth.check.state_type, "{}", truth.key);
    }
    (applying, evaluating, applied)
}

fn all() -> Changes {
    Changes {
        any: true,
        all: true,
        ..Changes::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_applier_keeps_up_with_bursts() {
    let (server, lines) = record(200, 2).await;
    let count = lines.len();
    assert!(count >= 6_400, "{count} lines");
    let (took, dashboards, applied) = apply(&server, lines, 5_000, true);
    eprintln!(
        "applied {applied} of {count} recorded events (3 000 services) in {took:?}, \
         ten dashboards updated in {dashboards:?}"
    );
    // Release: well under a second. Unoptimised workspace crates are
    // several times slower; the bound only catches pathologies.
    assert!(took < Duration::from_secs(10), "{took:?}");
    assert!(dashboards < Duration::from_secs(10), "{dashboards:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "production size; run with --ignored --nocapture"]
async fn fifty_thousand_events_at_production_scale() {
    let (server, lines) = record(2_000, 2).await;
    let count = lines.len();
    assert!(count >= 50_000, "{count} lines");
    let lines: Vec<Vec<u8>> = lines.into_iter().take(50_000).collect();
    let count = lines.len();
    let (took, _, applied) = apply(&server, lines.clone(), 5_000, false);
    let per_second = f64::from(u32::try_from(count).unwrap_or(u32::MAX)) / took.as_secs_f64();
    eprintln!(
        "applied {applied} of {count} recorded events (30 000 services) in {took:?} \
         ({per_second:.0} events/s)"
    );
    let (took, dashboards, _) = apply(&server, lines, 5_000, true);
    eprintln!("again with ten dashboards updated per batch of 5 000: {took:?} + {dashboards:?}");
}

/// A store as after the initial load at production scale: hosts in full,
/// services lean, the problems in full.
fn production_store(hosts: usize) -> Store {
    let scenario = scenarios::large_with_hosts(hosts, 7);
    let mut store = Store::default();
    store.replace_hosts(scenario.hosts.clone(), 0);
    let lean: Vec<Service> = scenario
        .services
        .iter()
        .cloned()
        .map(|mut service| {
            service.check.result = None;
            service.links = ic_model::Links::default();
            service
        })
        .collect();
    store.replace_services(lean, Detail::Lean, 0);
    let problems: Vec<Service> = scenario
        .services
        .iter()
        .filter(|service| service.is_problem())
        .cloned()
        .collect();
    store.apply_fetched(Vec::new(), problems, Detail::Full, &[], 1);
    store.set_host_groups(scenario.host_groups.clone());
    store.set_service_groups(scenario.service_groups.clone());
    store.take_changes();
    store
}

/// Evaluates ten dashboards over `hosts` × 15 services in full, then
/// incrementally after state changes of `changed` services; returns the
/// full time and the mean incremental time.
fn dashboards_over(hosts: usize, touched: usize) -> (Duration, Duration) {
    let store = production_store(hosts);
    let mut data = data_of(&store);
    let mut dashboards = Dashboards::default();
    dashboards.configure(&ten_dashboards());
    let started = Instant::now();
    let results = dashboards.update(&data, &all(), false, &AtomicBool::new(false));
    let full = started.elapsed();
    assert_eq!(results.len(), 10);
    assert!(results.values().all(|result| result.error.is_none()));

    let keys: Vec<_> = data.services.keys().cloned().collect();
    let rounds: u32 = 10;
    let mut incremental = Duration::ZERO;
    for round in 0..usize::try_from(rounds).unwrap_or(0) {
        let mut dirty = BTreeSet::new();
        let services = Arc::make_mut(&mut data.services);
        for index in 0..touched {
            let key = &keys[(index * 7_919 + round * 104_729) % keys.len()];
            let mut service = (*services[key]).clone();
            service.state = match service.state {
                ServiceState::Ok => ServiceState::Critical,
                _ => ServiceState::Ok,
            };
            service.check.last_state_change = Timestamp::now();
            services.insert(key.clone(), Arc::new(service));
            dirty.insert(ObjectKey::from(key.clone()));
        }
        let changes = Changes {
            any: true,
            objects: dirty,
            ..Changes::default()
        };
        let started = Instant::now();
        dashboards.update(&data, &changes, false, &AtomicBool::new(false));
        incremental += started.elapsed();
    }
    (full, incremental / rounds)
}

#[test]
fn dashboards_evaluate_quickly() {
    let (full, incremental) = dashboards_over(134, 100);
    eprintln!(
        "2 010 services × 10 dashboards: full {full:?}, 100 changed services {incremental:?}"
    );
    assert!(full < Duration::from_secs(10), "{full:?}");
    assert!(incremental < Duration::from_secs(2), "{incremental:?}");
}

#[test]
#[ignore = "production size; run with --ignored --nocapture"]
fn twenty_thousand_services_times_ten_dashboards() {
    let (full, incremental) = dashboards_over(1_334, 100);
    eprintln!(
        "20 010 services × 10 dashboards: full evaluation {full:?}, \
         incremental (100 changed services) {incremental:?}"
    );
    let (_, single) = dashboards_over(1_334, 1);
    eprintln!("incremental (1 changed service) {single:?}");
}

/// A store as a while after the initial load: every service has its
/// latest check result (each check brings one), and the links of those
/// loaded in full.
fn steady_store(hosts: usize) -> Store {
    let scenario = scenarios::large_with_hosts(hosts, 7);
    let mut store = Store::default();
    store.replace_hosts(scenario.hosts.clone(), 0);
    store.replace_services(scenario.services.clone(), Detail::Full, 0);
    store.set_host_groups(scenario.host_groups.clone());
    store.set_service_groups(scenario.service_groups.clone());
    store.take_changes();
    store
}

#[test]
#[ignore = "measures memory; run alone with --ignored --nocapture --test-threads 1"]
fn memory_at_production_scale() {
    let base = live_bytes();
    let store = production_store(2_000);
    let loaded = live_bytes().saturating_sub(base);
    eprintln!(
        "store after the initial load (2 000 hosts in full, 30 000 services lean, problems in \
         full): {:.1} MB ({} bytes per object)",
        megabytes(loaded),
        loaded / store.object_count().max(1)
    );
    drop(store);

    let base = live_bytes();
    let mut store = steady_store(2_000);
    let steady = live_bytes().saturating_sub(base);
    eprintln!(
        "store with every check result: {:.1} MB ({} bytes per object)",
        megabytes(steady),
        steady / store.object_count().max(1)
    );

    // Ten dashboards over it.
    let base = live_bytes();
    let mut dashboards = Dashboards::default();
    dashboards.configure(&ten_dashboards());
    let results = dashboards.update(&data_of(&store), &all(), false, &AtomicBool::new(false));
    let boards = live_bytes().saturating_sub(base);
    eprintln!(
        "ten dashboards (members, order, rows): {:.1} MB",
        megabytes(boards)
    );

    // A snapshot the UI holds while the store changes: the maps are copied
    // once (pointers, not objects).
    let held = store.snapshot(0, Timestamp::EPOCH, results, Arc::default());
    let base = live_bytes();
    let key = store.services().keys().next().cloned().unwrap();
    store.apply(
        10,
        &ic_model::Event::Flapping {
            object: ObjectKey::from(key),
            flapping: true,
            current: 40.0,
            at: Timestamp::EPOCH,
        },
    );
    let copy = live_bytes().saturating_sub(base);
    eprintln!(
        "copy on write while a snapshot is held: {:.1} MB",
        megabytes(copy)
    );
    drop(held);
    assert!(steady + boards < 400 * 1_024 * 1_024, "the 400 MB budget");
}
