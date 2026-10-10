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
//!   the checker feature's object paused on one of them.
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
use ic_model::{FeatureState, ObjectKey, ServiceKey, ServiceState};
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
                ..Tuning::default()
            },
            general: ic_config::General::default(),
            now: ic_model::Timestamp::now().as_unix_seconds(),
            data_dir: None,
            start: ic_core::Start::User,
            real_clock: true,
        }
        .start()
    }

    /// How long master-01's Icinga has run.
    async fn uptime(&self) -> f64 {
        let status = self.client(0).status().await.unwrap();
        ic_model::Timestamp::now().as_unix_seconds() - status.program_start.as_unix_seconds()
    }
}

/// Recovers the cluster when the test ends, passed or not.
struct Recover<'a>(&'a Path);

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
    // Icinga reports the checker feature's object paused on one master of
    // the HA zone (its own HA state), running on the other (read as the
    // fixtures' `icygui` user: the recommended icygui-demo user may not
    // read feature objects)...
    let user = std::env::var("ICYGUI_CONTRACT_USER").expect("ICYGUI_CONTRACT_USER");
    let password = std::env::var("ICYGUI_CONTRACT_PASSWORD").expect("ICYGUI_CONTRACT_PASSWORD");
    let mut states = Vec::new();
    for index in 0..2 {
        let read = cluster
            .client_as(index, &user, &password)
            .node_features(&[])
            .await
            .unwrap();
        states.push(read.features.checker);
    }
    states.sort_by_key(|state| format!("{state:?}"));
    assert_eq!(
        states,
        [Some(FeatureState::Paused), Some(FeatureState::Running)],
        "one paused, one running"
    );
    // ...yet both run their share of the master zone's checks.
    let client = cluster.client(0);
    let services = client.services(Detail::Full).await.unwrap();
    let now = ic_model::Timestamp::now().as_unix_seconds();
    let mut sources = std::collections::BTreeSet::new();
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
