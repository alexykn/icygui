//! The engine against the demo cluster (`demo/docker-compose.yml`: two
//! masters, the satellite zone ams and the HA satellite zone fra), with
//! nodes stopped by `demo/scenario.sh`: what a real Icinga 2.15 cluster
//! does when an endpoint goes, which the mock had wrong before (the stage 4
//! review's `mock-drift` findings in docs/review/ledger.md), and what
//! icygui makes of it.
//!
//! - an endpoint that is gone: Icinga's relay queue stays flat, and icygui
//!   raises one alert for it, none about the queue;
//! - a check pinned to that endpoint (its heartbeat): Icinga answers it
//!   UNKNOWN, *Remote Icinga instance 'X' is not connected to 'Y'*, but
//!   only once the checking node has run for 5 minutes (its cold-start
//!   window; before that the check stays silent);
//! - a zone with no endpoint left: its checks and beats go silent (no
//!   other node takes them over, nothing turns UNKNOWN), its
//!   `cluster-zone` check turns critical, and icygui raises one alert for
//!   the zone;
//! - an HA master zone: both masters run checks, although Icinga reports
//!   the checker feature's object paused on one of them;
//! - a checker off on the other master: its share of the zone's checks
//!   goes late, nothing else shows it, and icygui raises the zone's late
//!   checks as one alert;
//! - downtimes and acknowledgements added and removed around a quiet
//!   engine's wake, and a fixed downtime that Icinga counts from its start
//!   before it reports it: the store's downtime depth and acknowledgement
//!   match Icinga's afterwards, in quiet mode where no check result would
//!   correct them.
//!
//! They run when `demo/up.sh`'s variables are set (`ICYGUI_CLUSTER_SCENARIO`
//! and the `ICYGUI_CONTRACT_*` ones), and otherwise pass without checking
//! anything unless `ICYGUI_CLUSTER_REQUIRED` is set. Before anything they
//! make sure it is that disposable cluster: both URLs on this machine, the
//! fixture-only `viewer` user logs in, the node is `master-01`, and the
//! compose project next to the scenario script runs. One test at a time
//! (they stop nodes), each leaves the cluster recovered, and each takes a
//! few minutes (Icinga's own timeouts).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use ic_api::{Client, ConnectionSettings, Credentials, Detail, TlsSettings, Url};
use ic_config::{ApiUrl, AuthConfig, Environment};
use ic_core::heartbeat::{BeatState, Death};
use ic_core::snapshot::Snapshot;
use ic_core::{CoreEvent, Tuning};
use ic_model::{
    Action, ActionTarget, ChildOptions, DowntimeMode, Event, EventKind, FeatureState, ObjectKey,
    ServiceKey, ServiceState, StateType, Timestamp,
};
use secrecy::SecretString;

use crate::support::{ENV_ID, Engine, FakeSecrets, Launch};

/// The cluster's tests stop and start nodes: one at a time.
static CLUSTER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The heartbeats of the demo cluster (PLAN.md §4.2 B3).
const BEATS: usize = 6;

/// How long the checking node's cold-start window lasts: Icinga makes up
/// the UNKNOWN result of a check pinned to a disconnected endpoint only
/// once it has run this long (`Checkable::ExecuteCheck`).
const COLD_START: f64 = 300.0;

struct Cluster {
    urls: [(String, String); 2],
    ca_file: PathBuf,
    user: String,
    password: String,
    scenario: PathBuf,
}

/// The cluster from `demo/up.sh`'s variables, or `None` (the test passes
/// trivially) when they aren't set and the cluster isn't required.
fn cluster() -> Option<Cluster> {
    let Ok(scenario) = std::env::var("ICYGUI_CLUSTER_SCENARIO") else {
        assert!(
            std::env::var_os("ICYGUI_CLUSTER_REQUIRED").is_none(),
            "ICYGUI_CLUSTER_REQUIRED is set but ICYGUI_CLUSTER_SCENARIO is not: \
             export the variables demo/up.sh prints"
        );
        return None;
    };
    let var = |name: &str| std::env::var(name).expect(name);
    Some(Cluster {
        urls: [
            (
                var("ICYGUI_CONTRACT_URL"),
                var("ICYGUI_CONTRACT_SERVER_NAME"),
            ),
            (
                var("ICYGUI_CLUSTER_URL_2"),
                var("ICYGUI_CLUSTER_SERVER_NAME_2"),
            ),
        ],
        ca_file: PathBuf::from(var("ICYGUI_CONTRACT_CA_FILE")),
        user: var("ICYGUI_CLUSTER_USER"),
        password: var("ICYGUI_CLUSTER_PASSWORD"),
        scenario: PathBuf::from(scenario),
    })
}

impl Cluster {
    fn client_as(&self, index: usize, user: &str, password: &str) -> Client {
        let (url, server_name) = &self.urls[index];
        Client::new(ConnectionSettings::new(
            Url::parse(url).unwrap(),
            Credentials::Basic {
                username: user.to_owned(),
                password: SecretString::from(password.to_owned()),
            },
            TlsSettings {
                ca_pem: Some(std::fs::read(&self.ca_file).expect("CA file")),
                server_name: Some(server_name.clone()),
                ..TlsSettings::default()
            },
        ))
        .unwrap()
    }

    fn client(&self, index: usize) -> Client {
        self.client_as(index, &self.user, &self.password)
    }

    /// The demo's admin user on master-01 (`ICYGUI_CONTRACT_ADMIN_*`, a
    /// documented demo value): passive results, which icygui's own user
    /// may not submit.
    fn admin(&self) -> Client {
        let var = |name: &str| std::env::var(name).expect(name);
        self.client_as(
            0,
            &var("ICYGUI_CONTRACT_ADMIN_USER"),
            &var("ICYGUI_CONTRACT_ADMIN_PASSWORD"),
        )
    }

    /// Undoes what a test adds to `keys` when it ends, passed or not.
    fn undo(&self, keys: &[ObjectKey]) -> Undo {
        let var = |name: &str| std::env::var(name).expect(name);
        let (url, server_name) = &self.urls[0];
        Undo {
            settings: Some(ConnectionSettings::new(
                Url::parse(url).unwrap(),
                Credentials::Basic {
                    username: var("ICYGUI_CONTRACT_ADMIN_USER"),
                    password: SecretString::from(var("ICYGUI_CONTRACT_ADMIN_PASSWORD")),
                },
                TlsSettings {
                    ca_pem: Some(std::fs::read(&self.ca_file).expect("CA file")),
                    server_name: Some(server_name.clone()),
                    ..TlsSettings::default()
                },
            )),
            keys: keys.to_vec(),
            downtimes: Vec::new(),
        }
    }

    /// Refuses anything but the disposable demo cluster, before any load.
    async fn check_disposable(&self) {
        const REFUSED: &str = "not the disposable demo cluster of demo/docker-compose.yml; \
                               the cluster tests stop nodes and never run elsewhere";
        for (index, (url, _)) in self.urls.iter().enumerate() {
            let url = Url::parse(url).unwrap();
            let host = url.host_str().unwrap_or_default();
            let host = host.trim_start_matches('[').trim_end_matches(']');
            let loopback = host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback());
            assert!(loopback, "{REFUSED}: {host} is not this machine");
            let viewer = self.client_as(index, "viewer", "viewer-test").info().await;
            assert!(
                matches!(&viewer, Ok(info) if info.user == "viewer"),
                "{REFUSED}: its fixture-only viewer can't log in ({viewer:?})"
            );
        }
        let node = self.client(0).status().await.unwrap().node_name;
        assert_eq!(node, "master-01", "{REFUSED}");
        let compose = self.scenario.with_file_name("docker-compose.yml");
        let output = Process::new("docker")
            .args(["compose", "-f"])
            .arg(&compose)
            .args(["ps", "-q", "master-01"])
            .output()
            .expect("docker");
        assert!(
            output.status.success() && !output.stdout.is_empty(),
            "{REFUSED}: {} runs no master-01",
            compose.display()
        );
    }

    /// `demo/scenario.sh <args>`.
    fn scenario(&self, args: &[&str]) {
        let status = Process::new("bash")
            .arg(&self.scenario)
            .args(args)
            .status()
            .expect("demo/scenario.sh");
        assert!(status.success(), "demo/scenario.sh {args:?} failed");
    }

    fn environment(&self) -> Environment {
        let mut environment = Environment::new(
            "demo",
            &self.urls[0].0,
            AuthConfig::Basic {
                username: self.user.clone(),
            },
        );
        ENV_ID.clone_into(&mut environment.id);
        environment.urls = self
            .urls
            .iter()
            .map(|(url, server_name)| ApiUrl {
                server_name: Some(server_name.clone()),
                ..ApiUrl::new(url)
            })
            .collect();
        environment.tls.use_system_roots = false;
        environment.tls.ca_file = Some(self.ca_file.clone());
        environment.author = Some("icygui-test".to_owned());
        environment
    }

    /// The engine against the cluster, on the wall clock, with the status
    /// polled every few seconds and a short grace before notifications;
    /// everything else as in production.
    fn engine(&self) -> Engine {
        Launch {
            environment: self.environment(),
            secrets: FakeSecrets::with(ENV_ID, &self.password),
            tuning: Tuning {
                status_interval: Duration::from_secs(3),
                trouble_grace: Duration::from_secs(5),
                seed: Some(crate::support::SEED),
                ..Tuning::default()
            },
            general: ic_config::General::default(),
            now: Timestamp::now().as_unix_seconds(),
            data_dir: None,
            start: ic_core::Start::User,
            real_clock: true,
        }
        .start()
    }

    /// How long master-01's Icinga has run.
    async fn uptime(&self) -> f64 {
        let status = self.client(0).status().await.unwrap();
        Timestamp::now().as_unix_seconds() - status.program_start.as_unix_seconds()
    }
}

/// Recovers the cluster when the test ends, passed or not.
struct Recover<'a>(&'a Path);

/// What a test added to services, undone when it ends, passed or not (on a
/// thread and client of its own: a failed test's runtime may be gone): its
/// `downtimes` removed, and `keys` unacknowledged and OK again.
struct Undo {
    settings: Option<ConnectionSettings>,
    keys: Vec<ObjectKey>,
    downtimes: Vec<String>,
}

impl Drop for Undo {
    fn drop(&mut self) {
        let Some(settings) = self.settings.take() else {
            return;
        };
        let keys = std::mem::take(&mut self.keys);
        let downtimes = std::mem::take(&mut self.downtimes);
        let undone = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let admin = Client::new(settings).unwrap();
                if !downtimes.is_empty() {
                    let _ = admin
                        .run_action(
                            &Action::RemoveAllDowntimes,
                            &ActionTarget::Downtimes(downtimes),
                            "icygui-test",
                        )
                        .await;
                }
                if !keys.is_empty() {
                    let _ = admin
                        .run_action(
                            &Action::RemoveAcknowledgement,
                            &ActionTarget::Objects(keys.clone()),
                            "icygui-test",
                        )
                        .await;
                    submit(&admin, &keys, 0, 1).await;
                }
            });
        });
        let _ = undone.join();
    }
}

impl Drop for Recover<'_> {
    fn drop(&mut self) {
        let _ = Process::new("bash").arg(self.0).arg("recover").status();
    }
}

/// The cluster, checked, or `None` when its variables aren't set.
async fn setup() -> Option<Cluster> {
    let cluster = cluster()?;
    cluster.check_disposable().await;
    Some(cluster)
}

/// Waits up to `patience` for a snapshot `accept` accepts (the latest one
/// may be it), keeping every event.
async fn wait_snapshot(
    engine: &mut Engine,
    patience: Duration,
    what: &str,
    mut accept: impl FnMut(&Snapshot) -> bool,
) -> Arc<Snapshot> {
    if let Some(latest) = engine.latest().filter(|snapshot| accept(snapshot)) {
        return latest;
    }
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        let event = tokio::time::timeout_at(deadline, engine.events.next())
            .await
            .unwrap_or_else(|_| {
                let latest = engine.latest();
                panic!(
                    "{what}: not within {patience:?}; alerts {:?}, beats {:?}",
                    latest.as_ref().map(|s| &s.trouble.alerts),
                    latest.as_ref().map(|s| s
                        .heartbeats
                        .beats
                        .iter()
                        .map(|beat| (beat.object(), beat.state))
                        .collect::<Vec<_>>())
                )
            })
            .expect("the engine stopped");
        let matched = match &event {
            CoreEvent::Snapshot(snapshot) if accept(snapshot) => Some(Arc::clone(snapshot)),
            _ => None,
        };
        if !matches!(event, CoreEvent::Alive(_)) {
            engine.seen.push(event);
        }
        if let Some(snapshot) = matched {
            return snapshot;
        }
    }
}

fn on_time(snapshot: &Snapshot) -> usize {
    snapshot
        .heartbeats
        .beats
        .iter()
        .filter(|beat| beat.state == BeatState::OnTime)
        .count()
}

/// Connected, every beat on time, no alert: the healthy cluster.
async fn healthy(engine: &mut Engine) {
    engine.connected().await;
    wait_snapshot(
        engine,
        Duration::from_mins(3),
        "a healthy cluster",
        |snapshot| on_time(snapshot) == BEATS && snapshot.trouble.alerts.is_empty(),
    )
    .await;
}

/// The alerts that name `word`.
fn naming<'a>(snapshot: &'a Snapshot, word: &str) -> Vec<&'a ic_core::trouble::Alert> {
    snapshot
        .trouble
        .alerts
        .iter()
        .filter(|alert| alert.title.contains(word) || alert.detail.contains(word))
        .collect()
}

/// The number an alert's detail gives for `zone`'s late checks
/// (`… 1,204 checks in zone fra are late`), if it gives one.
fn late_in_detail(detail: &str, zone: &str) -> Option<usize> {
    let end = detail.find(&format!(" in zone {zone} "))?;
    let words: Vec<&str> = detail[..end].split(' ').collect();
    let number = words.iter().rev().nth(1)?;
    number.replace(',', "").parse().ok()
}

/// How many late checks the snapshot has in `zone`.
fn late_in_zone(snapshot: &Snapshot, zone: &str) -> usize {
    snapshot
        .late
        .keys()
        .filter(|key| {
            let check = match key {
                ObjectKey::Host { name } => snapshot.hosts.get(name).map(|host| &host.check),
                ObjectKey::Service { key } => {
                    snapshot.services.get(key).map(|service| &service.check)
                }
            };
            check.and_then(|check| check.zone.as_deref()) == Some(zone)
        })
        .count()
}

#[test]
fn a_late_count_is_read_from_an_alert() {
    assert_eq!(
        late_in_detail(
            "zone fra’s results are stale; 1,204 checks in zone fra are late.",
            "fra"
        ),
        Some(1_204)
    );
    assert_eq!(
        late_in_detail("1 check in zone ams is late.", "ams"),
        Some(1)
    );
    assert_eq!(late_in_detail("zone fra’s results are stale.", "fra"), None);
}

/// Lets the engine run for `period` (its events kept), then returns the
/// latest snapshot.
async fn run_for(engine: &mut Engine, period: Duration) -> Arc<Snapshot> {
    let until = tokio::time::Instant::now() + period;
    while let Ok(Some(event)) = tokio::time::timeout_at(until, engine.events.next()).await {
        if !matches!(event, CoreEvent::Alive(_)) {
            engine.seen.push(event);
        }
    }
    engine.latest().expect("a snapshot")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_endpoint_gone_is_one_alert_and_the_relay_queue_stays_flat() {
    let _turn = CLUSTER.lock().await;
    let Some(cluster) = setup().await else {
        return;
    };
    let mut engine = cluster.engine();
    healthy(&mut engine).await;
    let master = cluster.client(0);
    let before = master.listener_status().await.unwrap().relay_queue;

    let recover = Recover(&cluster.scenario);
    cluster.scenario(&["satellite-down"]);
    // Within Icinga's cluster timeout and a beat or two: the endpoint is
    // gone, its pinned beat is UNKNOWN (or, in sat-fra-02's first 5
    // minutes, silent and judged lost).
    wait_snapshot(
        &mut engine,
        Duration::from_mins(7),
        "an alert about sat-fra-01",
        |snapshot| !naming(snapshot, "sat-fra-01").is_empty(),
    )
    .await;
    // Through several status polls and graces: still one alert for the
    // one cause, none about a growing relay queue, and the queue flat.
    let mut queue = Vec::new();
    for _ in 0..8 {
        run_for(&mut engine, Duration::from_secs(5)).await;
        queue.push(master.listener_status().await.unwrap().relay_queue);
    }
    let snapshot = engine.latest().unwrap();
    let alerts = naming(&snapshot, "sat-fra-01");
    assert_eq!(alerts.len(), 1, "{:?}", snapshot.trouble.alerts);
    assert!(
        alerts[0].title
            == "heartbeat sat-fra-01 dead: Remote Icinga instance 'sat-fra-01' is not connected"
            || alerts[0].title == "zone fra: sat-fra-01 disconnected, heartbeat lost",
        "{:?}",
        alerts[0]
    );
    assert_eq!(
        snapshot.trouble.alerts.len(),
        1,
        "{:?}",
        snapshot.trouble.alerts
    );
    assert!(
        snapshot
            .trouble
            .alerts
            .iter()
            .all(|alert| !alert.detail.contains("relay queue")),
        "{:?}",
        snapshot.trouble.alerts
    );
    let most = queue.iter().copied().fold(before, f64::max);
    assert!(
        most <= before + 5.0,
        "Icinga 2.15 queues nothing for an endpoint that is gone: {before} then {queue:?}"
    );
    // The zone's other satellite runs zone fra's checks: its beat stays on time.
    let zone_beat = snapshot
        .heartbeats
        .beats
        .iter()
        .find(|beat| beat.key == ServiceKey::new("icygui-hb-fra", "beat"))
        .unwrap();
    assert_eq!(zone_beat.state, BeatState::OnTime);

    drop(recover);
    wait_snapshot(
        &mut engine,
        Duration::from_mins(5),
        "sat-fra-01 back",
        |snapshot| naming(snapshot, "sat-fra-01").is_empty(),
    )
    .await;
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_pinned_check_of_a_gone_endpoint_turns_unknown() {
    let _turn = CLUSTER.lock().await;
    let Some(cluster) = setup().await else {
        return;
    };
    let mut engine = cluster.engine();
    healthy(&mut engine).await;
    let uptime = cluster.uptime().await;
    let beat = ObjectKey::service("icygui-hb-master", "beat-master-02");
    let master = cluster.client(0);

    let recover = Recover(&cluster.scenario);
    cluster.scenario(&["master-down"]);
    if uptime + 120.0 < COLD_START - 30.0 {
        // In master-01's first 5 minutes the pinned check stays silent:
        // no result at all, its last one stays OK.
        run_for(&mut engine, Duration::from_secs(90)).await;
        let fetched = master
            .objects(std::slice::from_ref(&beat), Detail::Full)
            .await
            .unwrap();
        assert_eq!(fetched.services[0].state, ServiceState::Ok, "cold start");
    }
    let patience = Duration::from_secs_f64((COLD_START - uptime).max(0.0) + 240.0);
    let snapshot = wait_snapshot(
        &mut engine,
        patience,
        "the pinned beat UNKNOWN",
        |snapshot| {
            snapshot.heartbeats.beats.iter().any(|beat| {
                beat.key == ServiceKey::new("icygui-hb-master", "beat-master-02")
                    && beat.state == BeatState::Dead(Death::NotOk)
            })
        },
    )
    .await;
    let pinned = snapshot
        .heartbeats
        .beats
        .iter()
        .find(|beat| beat.key == ServiceKey::new("icygui-hb-master", "beat-master-02"))
        .unwrap();
    let reason = pinned.reason.clone().unwrap_or_default();
    assert!(
        reason.starts_with("Remote Icinga instance 'master-02' is not connected to 'master-01'"),
        "{reason:?}"
    );
    let fetched = master.objects(&[beat], Detail::Full).await.unwrap();
    assert_eq!(fetched.services[0].state, ServiceState::Unknown);
    // One line for the endpoint and its beat, with Icinga's words.
    let snapshot = run_for(&mut engine, Duration::from_secs(10)).await;
    let alerts = naming(&snapshot, "master-02");
    assert_eq!(alerts.len(), 1, "{:?}", snapshot.trouble.alerts);
    assert_eq!(
        alerts[0].title,
        "heartbeat master-02 dead: Remote Icinga instance 'master-02' is not connected"
    );
    // master-01 runs every master check meanwhile: its own beat on time.
    let own = snapshot
        .heartbeats
        .beats
        .iter()
        .find(|beat| beat.key == ServiceKey::new("icygui-hb-master", "beat-master-01"))
        .unwrap();
    assert_eq!(own.state, BeatState::OnTime);
    drop(recover);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_cut_off_zone_goes_silent() {
    let _turn = CLUSTER.lock().await;
    let Some(cluster) = setup().await else {
        return;
    };
    let mut engine = cluster.engine();
    healthy(&mut engine).await;
    let master = cluster.client(0);
    let beats = [
        ObjectKey::service("icygui-hb-fra", "beat"),
        ObjectKey::service("icygui-hb-fra", "beat-sat-fra-01"),
        ObjectKey::service("icygui-hb-fra", "beat-sat-fra-02"),
    ];

    let recover = Recover(&cluster.scenario);
    cluster.scenario(&["zone-cut-off"]);
    let started = Instant::now();
    let seen_before = engine.seen.len();
    let snapshot = wait_snapshot(
        &mut engine,
        Duration::from_mins(5),
        "the zone fra alert",
        |snapshot| {
            snapshot.trouble.alerts.iter().any(|alert| {
                alert
                    .title
                    .starts_with("zone fra: sat-fra-01 and sat-fra-02 disconnected")
            })
        },
    )
    .await;
    let cut_at = master
        .objects(&beats, Detail::Full)
        .await
        .unwrap()
        .services
        .iter()
        .map(|service| service.check.last_check)
        .collect::<Vec<_>>();
    // Two of the zone beat's intervals later: no new result for any of
    // them (no other node runs zone fra's checks), nothing UNKNOWN.
    let rest = Duration::from_secs(70).saturating_sub(started.elapsed());
    let snapshot = if rest.is_zero() {
        snapshot
    } else {
        run_for(&mut engine, rest.max(Duration::from_secs(65))).await
    };
    let later = master.objects(&beats, Detail::Full).await.unwrap().services;
    for (service, before) in later.iter().zip(&cut_at) {
        assert_eq!(
            service.check.last_check,
            *before,
            "{}",
            service.key.full_name()
        );
        assert_eq!(
            service.state,
            ServiceState::Ok,
            "{}",
            service.key.full_name()
        );
    }
    // The masters see the zone gone.
    let zone_check = master
        .objects(
            &[ObjectKey::service("sat-fra-01", "cluster-zone")],
            Detail::Full,
        )
        .await
        .unwrap();
    assert_eq!(zone_check.services[0].state, ServiceState::Critical);
    // One alert for the zone and its endpoints.
    let alerts = naming(&snapshot, "fra");
    assert_eq!(alerts.len(), 1, "{:?}", snapshot.trouble.alerts);
    assert!(alerts[0].key == "zone:fra", "{:?}", alerts[0]);
    // Whenever the alert says how many of the zone's checks are late, it is
    // what the same snapshot's late flags say (the health page's late tile):
    // one moment, one number.
    let mut counted = 0;
    for event in &engine.seen[seen_before..] {
        let CoreEvent::Snapshot(snapshot) = event else {
            continue;
        };
        let Some(said) = snapshot
            .trouble
            .alerts
            .iter()
            .find(|alert| alert.key == "zone:fra")
            .and_then(|alert| late_in_detail(&alert.detail, "fra"))
        else {
            continue;
        };
        assert_eq!(
            said,
            late_in_zone(snapshot, "fra"),
            "{:?}",
            snapshot.trouble
        );
        counted += 1;
    }
    eprintln!("{counted} snapshots quoted zone fra's late checks");
    drop(recover);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_ha_masters_share_the_checks() {
    let _turn = CLUSTER.lock().await;
    let Some(cluster) = setup().await else {
        return;
    };
    // Icinga reports the checker's and the notification feature's objects
    // paused on one master of the HA zone (their own HA state), running on
    // the other (read as the recommended icygui-demo user, who may read
    // them)...
    let mut states = Vec::new();
    for index in 0..2 {
        let read = cluster.client(index).node_features(&[]).await.unwrap();
        assert!(read.forbidden.is_empty(), "{:?}", read.forbidden);
        states.push((read.features.checker, read.features.notification));
    }
    states.sort_by_key(|state| format!("{state:?}"));
    assert_eq!(
        states,
        [
            (Some(FeatureState::Paused), Some(FeatureState::Paused)),
            (Some(FeatureState::Running), Some(FeatureState::Running))
        ],
        "one paused, one running"
    );
    // ...yet both run their share of the master zone's checks.
    let client = cluster.client(0);
    let services = client.services(Detail::Full).await.unwrap();
    let now = Timestamp::now().as_unix_seconds();
    let mut sources = BTreeSet::new();
    for service in &services {
        let recent = service
            .check
            .last_check
            .is_some_and(|at| now - at.as_unix_seconds() < 180.0);
        if service.check.zone.as_deref() == Some("master")
            && recent
            && let Some(result) = &service.check.result
        {
            sources.insert(result.check_source.clone());
        }
    }
    assert!(
        sources.contains("master-01") && sources.contains("master-02"),
        "{sources:?}"
    );
}

/// The small fix 2 review: master-02's checker off while icygui talks to
/// master-01. Icinga doesn't hand master-02's share of the master zone's
/// checks to master-01, yet master-01's own features read as before (its
/// checker `paused`, Icinga's HA bookkeeping), both masters stay connected
/// and master-02's pinned beat keeps beating (master-01 schedules it). The
/// zone's late checks are the only sign: icygui raises them as one
/// critical line for the zone, and nothing else (one cause, one alert).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_a_checker_off_on_the_other_master_shows_as_the_zones_late_checks() {
    let _turn = CLUSTER.lock().await;
    let Some(cluster) = setup().await else {
        return;
    };
    let mut engine = cluster.engine();
    engine.send(ic_core::Command::WatchHealth(true));
    healthy(&mut engine).await;
    let recover = Recover(&cluster.scenario);
    let started = Instant::now();
    cluster.scenario(&["checker-off", "master-02"]);
    let snapshot = wait_snapshot(
        &mut engine,
        Duration::from_mins(8),
        "the master zone's late checks as an alert",
        |snapshot| snapshot.trouble.zone_short_of_checks("master").is_some(),
    )
    .await;
    eprintln!(
        "the zone's late checks were raised {:?} after the checker went off ({} late)",
        started.elapsed(),
        late_in_zone(&snapshot, "master")
    );
    let alerts: Vec<&str> = snapshot
        .trouble
        .alerts
        .iter()
        .map(|alert| alert.title.as_str())
        .collect();
    assert_eq!(alerts.len(), 1, "one cause, one alert: {alerts:?}");
    let alert = &snapshot.trouble.alerts[0];
    assert_eq!(alert.key, "zone-late:master");
    assert_eq!(alert.tone, ic_core::trouble::AlertTone::Critical);
    assert!(
        alert.title.starts_with("zone master: ")
            && alert
                .title
                .ends_with(" late (master-01 and master-02 connected)"),
        "{}",
        alert.title
    );
    // What the page reads of master-01 says nothing is wrong: the late
    // checks are the only way to see it.
    let features = snapshot.health.features.expect("master-01's features");
    assert!(
        matches!(
            features.checker,
            Some(FeatureState::Running | FeatureState::Paused)
        ),
        "{features:?}"
    );
    assert!(
        snapshot
            .heartbeats
            .beats
            .iter()
            .all(|beat| beat.state == BeatState::OnTime),
        "the beats keep beating"
    );
    drop(recover);
    engine.shutdown();
}

/// Ledger row 74: an API user set up exactly as the user guide recommends
/// (the demo's icygui-demo has the guide's list, checked here) sees every
/// field the health page shows: the status, the endpoints' numbers, the
/// listener's queues and the node's three features, none of them refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_the_documented_api_user_sees_every_field_of_the_health_page() {
    let _turn = CLUSTER.lock().await;
    let Some(cluster) = setup().await else {
        return;
    };
    // The guide's snippet, as the demo's user has it.
    let guide = std::fs::read_to_string(
        cluster
            .scenario
            .parent()
            .unwrap()
            .join("../docs/user-guide.md"),
    )
    .expect("docs/user-guide.md");
    let start = guide
        .find("object ApiUser \"icygui\" {")
        .expect("the snippet");
    let snippet = &guide[start..];
    let snippet = &snippet[..snippet.find("\n}").unwrap()];
    let mut documented: Vec<String> = snippet
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix('"')
                .and_then(|line| line.strip_suffix("\","))
                .map(str::to_owned)
        })
        .collect();
    documented.sort();
    let info = cluster.client(0).info().await.unwrap();
    let mut granted = info.permissions.clone();
    granted.sort();
    assert_eq!(
        granted, documented,
        "icygui-demo has the guide's permissions"
    );

    let mut engine = cluster.engine();
    engine.send(ic_core::Command::WatchHealth(true));
    let snapshot = wait_snapshot(
        &mut engine,
        Duration::from_mins(2),
        "the health page's data",
        |snapshot| {
            let health = &snapshot.health;
            snapshot.status.is_some()
                && !health.endpoints.is_empty()
                && health.listener.is_some()
                && health.features.is_some_and(|features| {
                    features.checker.is_some()
                        && features.notification.is_some()
                        && features.icingadb.is_some()
                })
        },
    )
    .await;
    assert!(
        snapshot.health.features_forbidden.is_empty(),
        "{:?}",
        snapshot.health.features_forbidden
    );
    engine.shutdown();
}

/// Ledger row (quiet/live overlap): against the real Icinga, an engine
/// switched between quiet and live over and over while a problem storm
/// lands (40 services hard CRITICAL at once, three results each) logs each
/// state change once and notifies each problem once: no object's log goes
/// back from a hard state to the soft one before it, or has the same state
/// twice in a row, and no notification comes twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_switching_modes_during_a_problem_storm_logs_and_notifies_each_change_once() {
    use ic_core::Command;

    let _turn = CLUSTER.lock().await;
    let Some(cluster) = setup().await else {
        return;
    };
    let mut engine = cluster.engine();
    engine.connected().await;
    // Settled: the first load and Icinga's notifications are in.
    wait_snapshot(&mut engine, Duration::from_mins(2), "loaded", |snapshot| {
        !snapshot.icinga_notifications.is_empty()
    })
    .await;
    let before = engine.seen.len();
    let scenario = cluster.scenario.clone();
    let storm = std::thread::spawn(move || {
        // Let the first switches begin, then the storm.
        std::thread::sleep(Duration::from_millis(700));
        let status = Process::new("bash")
            .arg(&scenario)
            .args(["problem-storm", "40"])
            .status()
            .expect("demo/scenario.sh");
        assert!(status.success(), "problem-storm failed");
    });
    let mut quiet = false;
    for _ in 0..14 {
        quiet = !quiet;
        engine.send(Command::SetQuiet(quiet));
        let wanted = quiet;
        wait_snapshot(
            &mut engine,
            Duration::from_secs(30),
            "the switched stream",
            |snapshot| snapshot.quiet == wanted,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    storm.join().unwrap();
    if quiet {
        engine.send(Command::SetQuiet(false));
        wait_snapshot(&mut engine, Duration::from_secs(30), "live", |snapshot| {
            !snapshot.quiet
        })
        .await;
    }
    // Whatever was on its way is in.
    run_for(&mut engine, Duration::from_secs(10)).await;

    // The log: each change once, in order, none missing.
    let logged = check_storm_log(&engine.history(None).await);
    // Notifications: never the same one twice in a row for an object (a
    // problem that recovered and came back has another one between).
    let mut last: std::collections::BTreeMap<ObjectKey, String> = std::collections::BTreeMap::new();
    let mut all = 0;
    for event in &engine.seen[before..] {
        if let CoreEvent::Notification(record) = event
            && let Some(object) = &record.intent.object
        {
            all += 1;
            let previous = last.insert(object.clone(), record.intent.title.clone());
            assert_ne!(
                previous.as_deref(),
                Some(record.intent.title.as_str()),
                "{object}: notified twice"
            );
        }
    }
    eprintln!("{logged} objects with state changes, {all} notifications, none twice");
    engine.shutdown();
}

/// The storm test's log, per object, oldest first: never the same state
/// twice in a row, never back from hard to the soft state before it, the
/// storm's changes there, and every storm problem logged hard logged soft
/// first (a lost attempt leaves only what a reload found: Icinga writes
/// each stream every 5 s, and the switch waits for the old stream's
/// batch). Returns how many objects changed state.
fn check_storm_log(history: &[ic_core::LogEntry]) -> usize {
    use ic_core::LogKind;
    use ic_model::{CheckableState, StateType};
    use std::collections::BTreeMap;

    let mut states: BTreeMap<String, Vec<(CheckableState, StateType)>> = BTreeMap::new();
    let mut texts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in history.iter().rev() {
        if let LogKind::State { state, state_type } = entry.kind {
            let object = entry.object.to_string();
            states
                .entry(object.clone())
                .or_default()
                .push((state, state_type));
            texts.entry(object).or_default().push(format!(
                "{:.3} {state:?}/{state_type:?} {}",
                entry.at.as_unix_seconds(),
                entry.text
            ));
        }
    }
    let critical = CheckableState::Service(ServiceState::Critical);
    let mut stormed = 0;
    for (object, entries) in &states {
        if entries.iter().any(|(state, _)| *state == critical) {
            stormed += 1;
        }
        for pair in entries.windows(2) {
            let ((a, a_type), (b, b_type)) = (pair[0], pair[1]);
            assert!(
                !(a == b && a_type == b_type),
                "{object}: the same state twice in a row: {:#?}",
                texts[object]
            );
            assert!(
                !(a == b && a_type == StateType::Hard && b_type == StateType::Soft),
                "{object}: back from hard to soft: {:#?}",
                texts[object]
            );
        }
    }
    // (A service still critical from an earlier storm changes nothing.)
    assert!(stormed >= 10, "the storm's changes are logged: {stormed}");
    let unseen: Vec<&String> = texts
        .iter()
        .filter(|(_, lines)| {
            let storm = |kind: &str| {
                lines
                    .iter()
                    .position(|line| line.contains(kind) && line.contains("problem storm"))
            };
            storm("/Hard").is_some_and(|hard| storm("/Soft").is_none_or(|soft| soft > hard))
        })
        .map(|(object, _)| object)
        .collect();
    assert!(unseen.is_empty(), "soft attempts never seen: {unseen:?}");
    states.len()
}

/// Services nothing else touches during a test: OK on a host that is up,
/// no downtime (none scheduled either), no acknowledgement, not a
/// heartbeat, not in `used`, active checks every 30 minutes or more with
/// the next one at least [`UNTOUCHED_FOR`] away (a passive result doesn't
/// move it), those whose next check is furthest away first. Asked of
/// Icinga, not the store (a quiet store's `next_check` doesn't move with
/// a check that changes nothing). The demo's checks all started together,
/// so the long ones fall due together every half hour: then this waits
/// (the engine running) until they ran and are half an hour away again.
async fn untouched(
    admin: &Client,
    engine: &mut Engine,
    count: usize,
    used: &BTreeSet<ServiceKey>,
) -> Vec<ServiceKey> {
    let deadline = tokio::time::Instant::now() + Duration::from_mins(10);
    loop {
        let services = admin.services(Detail::Lean).await.unwrap();
        let snapshot = engine.latest().unwrap();
        let later = Timestamp::now().as_unix_seconds() + UNTOUCHED_FOR.as_secs_f64();
        let mut fit: Vec<(f64, ServiceKey)> = services
            .iter()
            .filter(|service| {
                let check = &service.check;
                service.state == ServiceState::Ok
                    && check.features.active_checks
                    && check.check_interval >= 1_800.0
                    && check.downtime_depth == 0
                    && !check.acknowledgement.is_acknowledged()
                    && snapshot.services.contains_key(&service.key)
                    && !snapshot.excluded.contains(&service.key)
                    && !used.contains(&service.key)
                    && snapshot
                        .downtimes
                        .get(&ObjectKey::from(service.key.clone()))
                        .is_none_or(Vec::is_empty)
                    && snapshot
                        .host_of(&service.key)
                        .is_some_and(|host| host.state == ic_model::HostState::Up)
            })
            .filter_map(|service| {
                let next = service.check.next_check?.as_unix_seconds();
                (next > later).then(|| (next, service.key.clone()))
            })
            .collect();
        if fit.len() >= count {
            fit.sort_by(|a, b| b.0.total_cmp(&a.0));
            return fit.into_iter().take(count).map(|(_, key)| key).collect();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "not {count} untouched services within 10 minutes, {} now",
            fit.len()
        );
        run_for(engine, Duration::from_secs(20)).await;
    }
}

/// How long a test's targets must stay clear of their active checks (a
/// round takes about a minute).
const UNTOUCHED_FOR: Duration = Duration::from_mins(4);

/// Submits `exit_status` for `keys` as the admin, `times` times (3: past
/// `max_check_attempts`, a hard state).
async fn submit(admin: &Client, keys: &[ObjectKey], exit_status: u8, times: usize) {
    for _ in 0..times {
        admin
            .run_action(
                &Action::ProcessCheckResult {
                    exit_status,
                    output: format!("icygui cluster test (downtime depth): {exit_status}"),
                    perfdata: Vec::new(),
                    ttl: None,
                },
                &ActionTarget::Objects(keys.to_vec()),
                "icygui-test",
            )
            .await
            .unwrap();
    }
}

/// Schedules a fixed downtime from `start` to `end` on each of `keys`;
/// returns their names, in order.
async fn schedule(admin: &Client, keys: &[ObjectKey], start: f64, end: f64) -> Vec<String> {
    let results = admin
        .run_action(
            &Action::ScheduleDowntime {
                comment: "icygui cluster test (downtime depth)".to_owned(),
                start: Timestamp::from_unix_seconds(start),
                end: Timestamp::from_unix_seconds(end),
                mode: DowntimeMode::Fixed,
                all_services: false,
                child_options: ChildOptions::None,
                trigger_name: None,
            },
            &ActionTarget::Objects(keys.to_vec()),
            "icygui-test",
        )
        .await
        .unwrap();
    let names: Vec<String> = results
        .into_iter()
        .filter_map(|result| result.name)
        .collect();
    assert_eq!(names.len(), keys.len(), "a downtime each");
    names
}

/// Whether the snapshot lists the downtime `name` of `key`.
fn lists(snapshot: &Snapshot, key: &ObjectKey, name: &str) -> bool {
    snapshot
        .downtimes
        .get(key)
        .is_some_and(|list| list.iter().any(|downtime| downtime.name == name))
}

/// The store's downtime depth and acknowledgement of every one of `keys`
/// against Icinga's, asked now.
async fn matches_icinga(admin: &Client, engine: &Engine, keys: &[ObjectKey], when: &str) {
    let icinga = admin.objects(keys, Detail::Lean).await.unwrap();
    assert_eq!(icinga.services.len(), keys.len(), "{when}: Icinga answers");
    let snapshot = engine.latest().unwrap();
    let differ: Vec<String> = icinga
        .services
        .iter()
        .filter_map(|service| {
            let stored = &snapshot.services[&service.key];
            let theirs = (
                service.check.downtime_depth,
                service.check.acknowledgement.is_acknowledged(),
            );
            let ours = (
                stored.check.downtime_depth,
                stored.check.acknowledgement.is_acknowledged(),
            );
            (theirs != ours).then(|| {
                format!(
                    "{}: Icinga depth {} acknowledged {}, store {} {}",
                    service.key, theirs.0, theirs.1, ours.0, ours.1
                )
            })
        })
        .collect();
    assert!(differ.is_empty(), "{when}: {differ:#?}");
}

/// Waits until the engine is in `quiet` mode.
async fn mode(engine: &mut Engine, quiet: bool) {
    engine.send(ic_core::Command::SetQuiet(quiet));
    wait_snapshot(engine, Duration::from_secs(30), "the mode", |snapshot| {
        snapshot.quiet == quiet
    })
    .await;
}

/// One round of [`cluster_downtimes_and_acknowledgements_around_a_wake_match_icinga`]
/// on six new targets; returns how many of the downtimes' answers came
/// before their lines.
#[expect(clippy::too_many_lines, reason = "one scenario, step by step")]
async fn around_a_wake(
    cluster: &Cluster,
    admin: &Client,
    engine: &mut Engine,
    used: &mut BTreeSet<ServiceKey>,
) -> usize {
    mode(engine, true).await;
    let targets = untouched(admin, engine, 6, used).await;
    used.extend(targets.iter().cloned());
    let keys: Vec<ObjectKey> = targets.iter().cloned().map(ObjectKey::from).collect();
    let mut undo = cluster.undo(&keys);
    // Problems (hard): the refresh after waking asks for them by name.
    submit(admin, &keys, 2, 3).await;
    wait_snapshot(engine, Duration::from_mins(1), "the problems", |snapshot| {
        targets.iter().all(|key| {
            let service = &snapshot.services[key];
            service.state == ServiceState::Critical && service.check.state_type == StateType::Hard
        })
    })
    .await;
    let (with_downtime, acknowledged) = keys.split_at(4);

    // Around the wake: the changes, and at once the wake. The quiet
    // stream brings their lines with its next batch (Icinga writes every
    // 5 s); the refresh's answers usually come first.
    let now = Timestamp::now().as_unix_seconds();
    let downtimes = schedule(admin, with_downtime, now - 60.0, now + 3_600.0).await;
    undo.downtimes.clone_from(&downtimes);
    admin
        .run_action(
            &Action::Acknowledge {
                comment: "icygui cluster test (acknowledgement)".to_owned(),
                sticky: false,
                persistent: false,
                expiry: None,
            },
            &ActionTarget::Objects(acknowledged.to_vec()),
            "icygui-test",
        )
        .await
        .unwrap();
    engine.send(ic_core::Command::SetQuiet(false));
    let mut early = BTreeSet::new();
    wait_snapshot(
        engine,
        Duration::from_mins(1),
        "the changes' lines",
        |snapshot| {
            for (key, name) in with_downtime.iter().zip(&downtimes) {
                let service = key.as_service().unwrap();
                if !lists(snapshot, key, name)
                    && snapshot.services[service].check.downtime_depth > 0
                {
                    early.insert(key.clone());
                }
            }
            !snapshot.quiet
                && with_downtime
                    .iter()
                    .zip(&downtimes)
                    .all(|(key, name)| lists(snapshot, key, name))
                && acknowledged.iter().all(|key| {
                    snapshot.services[key.as_service().unwrap()]
                        .check
                        .acknowledgement
                        .is_acknowledged()
                })
        },
    )
    .await;
    // The switch's window ends, Icinga is asked again where needed.
    run_for(engine, Duration::from_secs(10)).await;
    matches_icinga(admin, engine, &keys, "after the wake").await;

    // Back to quiet, where no check result brings Icinga's count: the
    // downtimes and acknowledgements go.
    mode(engine, true).await;
    admin
        .run_action(
            &Action::RemoveAllDowntimes,
            &ActionTarget::Downtimes(downtimes.clone()),
            "icygui-test",
        )
        .await
        .unwrap();
    admin
        .run_action(
            &Action::RemoveAcknowledgement,
            &ActionTarget::Objects(acknowledged.to_vec()),
            "icygui-test",
        )
        .await
        .unwrap();
    undo.downtimes.clear();
    wait_snapshot(engine, Duration::from_mins(1), "the removals", |snapshot| {
        with_downtime
            .iter()
            .zip(&downtimes)
            .all(|(key, name)| !lists(snapshot, key, name))
            && acknowledged.iter().all(|key| {
                !snapshot.services[key.as_service().unwrap()]
                    .check
                    .acknowledgement
                    .is_acknowledged()
            })
    })
    .await;
    run_for(engine, Duration::from_secs(8)).await;
    matches_icinga(admin, engine, &keys, "after the removals in quiet mode").await;
    let snapshot = engine.latest().unwrap();
    assert!(
        targets
            .iter()
            .all(|key| !snapshot.services[key].is_handled(false)),
        "unhandled problems again, none hidden"
    );
    // The targets recover (`undo`).
    early.len()
}

/// Ledger row (small fix 2 fixer, false green): around a quiet engine's
/// wake, downtimes and acknowledgements are added on problems that the
/// refresh after waking asks for by name, and removed in quiet mode
/// afterwards. Icinga writes each stream every 5 s, so the refresh's
/// answers come before the quiet stream's lines of the same changes
/// (counted; another round with new targets if none did). The store's
/// downtime depth and acknowledgement of every target match Icinga's after
/// the wake and after the removals, where no check result would correct
/// them: before the fix an overtaken downtime counted twice and its
/// problem stayed in downtime, hidden, after the removal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_downtimes_and_acknowledgements_around_a_wake_match_icinga() {
    let _turn = CLUSTER.lock().await;
    let Some(cluster) = setup().await else {
        return;
    };
    let admin = cluster.admin();
    let mut engine = cluster.engine();
    engine.connected().await;
    wait_snapshot(&mut engine, Duration::from_mins(2), "loaded", |snapshot| {
        !snapshot.icinga_notifications.is_empty()
    })
    .await;
    let mut used = BTreeSet::new();
    let mut overtaken = 0;
    let mut rounds = 0;
    while overtaken == 0 && rounds < 3 {
        rounds += 1;
        overtaken += around_a_wake(&cluster, &admin, &mut engine, &mut used).await;
    }
    eprintln!("{overtaken} downtimes answered before their lines, in {rounds} rounds");
    assert!(
        overtaken > 0,
        "in {rounds} rounds no answer came before its downtime's line: \
         the case this test is about never happened"
    );
    engine.shutdown();
}

/// Icinga counts a fixed downtime from its start (`Downtime::IsInEffect`
/// asks the clock) but reports it (`DowntimeStarted`) from a timer every
/// 5 s, so a state change in between already carries the count: checked
/// here on Icinga's own event stream, for downtimes starting one after
/// another, each followed 0.3 s after its start by a recovery (an OK
/// result triggers no downtime). The store's depth matches Icinga's once
/// they started and after their removal, in quiet mode; before the fix the
/// started line added 1 again (2), and 1 stayed after the removal.
#[expect(clippy::too_many_lines, reason = "one scenario, step by step")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cluster_a_fixed_downtime_counts_from_its_start_like_icinga() {
    let _turn = CLUSTER.lock().await;
    let Some(cluster) = setup().await else {
        return;
    };
    let admin = cluster.admin();
    let mut engine = cluster.engine();
    engine.connected().await;
    wait_snapshot(&mut engine, Duration::from_mins(2), "loaded", |snapshot| {
        !snapshot.icinga_notifications.is_empty()
    })
    .await;
    mode(&mut engine, true).await;
    let targets = untouched(&admin, &mut engine, 4, &BTreeSet::new()).await;
    let keys: Vec<ObjectKey> = targets.iter().cloned().map(ObjectKey::from).collect();
    let mut undo = cluster.undo(&keys);
    submit(&admin, &keys, 2, 3).await;
    wait_snapshot(
        &mut engine,
        Duration::from_mins(1),
        "the problems",
        |snapshot| {
            targets
                .iter()
                .all(|key| snapshot.services[key].state == ServiceState::Critical)
        },
    )
    .await;

    // Icinga's own stream, for the order of its events.
    let mut events = admin
        .events(
            "icygui-cluster-downtime-start",
            &[EventKind::StateChange, EventKind::DowntimeStarted],
        )
        .await
        .unwrap();
    let first = Timestamp::now().as_unix_seconds() + 4.0;
    let starts: Vec<f64> = (0..keys.len())
        .map(|index| first + 1.3 * f64::from(u32::try_from(index).unwrap()))
        .collect();
    let mut downtimes = Vec::new();
    for (key, start) in keys.iter().zip(&starts) {
        downtimes
            .extend(schedule(&admin, std::slice::from_ref(key), *start, start + 3_600.0).await);
    }
    undo.downtimes.clone_from(&downtimes);
    for (key, start) in keys.iter().zip(&starts) {
        let wait = start + 0.3 - Timestamp::now().as_unix_seconds();
        if wait > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(wait)).await;
        }
        submit(&admin, std::slice::from_ref(key), 0, 1).await;
    }
    // Every start reported (the timer runs every 5 s); the order per target.
    let mut recovered = BTreeSet::new();
    let mut started = BTreeSet::new();
    let mut counted_first = BTreeSet::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while started.len() < keys.len() {
        let event = tokio::time::timeout_at(deadline, events.next())
            .await
            .expect("every downtime start reported within 30 s")
            .expect("the stream")
            .unwrap();
        match event {
            Event::StateChange {
                object,
                downtime_depth,
                ..
            } if keys.contains(&object) => {
                if !started.contains(&object) && downtime_depth.unwrap_or(0) > 0 {
                    counted_first.insert(object.clone());
                }
                recovered.insert(object);
            }
            Event::DowntimeStarted { downtime, .. } if keys.contains(&downtime.object) => {
                started.insert(downtime.object);
            }
            _ => {}
        }
    }
    drop(events);
    eprintln!(
        "{} of {} recoveries carried the downtime before Icinga reported it started",
        counted_first.len(),
        keys.len()
    );
    assert!(
        !counted_first.is_empty(),
        "Icinga's start timer reported every downtime within 0.3 s of its start: \
         the case this test is about never happened"
    );
    run_for(&mut engine, Duration::from_secs(8)).await;
    matches_icinga(&admin, &engine, &keys, "after the starts").await;
    let snapshot = engine.latest().unwrap();
    assert!(
        targets
            .iter()
            .all(|key| snapshot.services[key].check.downtime_depth == 1),
        "one downtime each"
    );

    admin
        .run_action(
            &Action::RemoveAllDowntimes,
            &ActionTarget::Downtimes(downtimes.clone()),
            "icygui-test",
        )
        .await
        .unwrap();
    undo.downtimes.clear();
    wait_snapshot(
        &mut engine,
        Duration::from_mins(1),
        "the removals",
        |snapshot| {
            keys.iter()
                .zip(&downtimes)
                .all(|(key, name)| !lists(snapshot, key, name))
        },
    )
    .await;
    run_for(&mut engine, Duration::from_secs(8)).await;
    matches_icinga(&admin, &engine, &keys, "after the removals in quiet mode").await;
    let snapshot = engine.latest().unwrap();
    assert!(
        targets
            .iter()
            .all(|key| snapshot.services[key].check.downtime_depth == 0),
        "none left"
    );
    engine.shutdown();
}
