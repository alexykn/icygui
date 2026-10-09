//! The cluster health page's content (topic 06), worked out from a
//! snapshot without drawing anything: the health line, the banner, the
//! zones with their endpoints, the stat tiles with their trends, Icinga's
//! global switches and the node's features. The page draws it; the
//! sidebar's *health* dot is [`assess`]'s verdict, by the same rules.
//!
//! When a value is wrong (and only then) it turns warning or critical:
//!
//! - an endpoint the connected node talks to is down: critical (it, its
//!   zone, the cluster connections tile, the cluster);
//! - a connected endpoint sent nothing for [`LAG_AFTER`]: warning;
//! - late checks: warning; critical from 1 % of the checks, or while a
//!   zone is cut off;
//! - active checks a minute fell by more than a quarter against the
//!   30-minute median: warning (critical at none);
//! - average latency at least five times its median and above 50 ms, or
//!   above a second: warning; above ten seconds: critical;
//! - the relay queue grew over the last three polls: warning; critical
//!   beyond 10 000 messages;
//! - a global switch that is off: warning (on the page only; switches are
//!   settings, not health, so they leave the sidebar's dot alone).

use std::collections::BTreeMap;
use std::time::Duration;

use ic_config::HealthTile;
use ic_core::health::{ClusterHealth, HealthSample};
use ic_core::snapshot::Snapshot;
use ic_core::{ClusterNode, NodeState};
use ic_model::{FeatureState, InstanceStatus, ObjectKey, Timestamp, Version, format_two_units};

use super::ClusterState;

/// A connected endpoint that sent nothing for this long lags.
pub(crate) const LAG_AFTER: Duration = Duration::from_mins(1);

/// How wrong a value is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Tone {
    /// Fine (the normal text colour).
    #[default]
    Normal,
    /// Needs a look.
    Warning,
    /// Broken.
    Critical,
}

/// The page, worked out.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Report {
    /// The node the numbers come from (`seen from master-01`).
    pub(crate) seen_from: Option<String>,
    /// When the latest status poll answered, and how often it runs.
    pub(crate) updated: Option<Timestamp>,
    pub(crate) interval: Duration,
    /// The environment is quiet (`quiet: every 5 min`).
    pub(crate) quiet: bool,
    /// The health line: endpoints connected and not.
    pub(crate) connected: usize,
    pub(crate) not_connected: usize,
    /// Its end: `Icinga r2.14.3-1 · up 41d 6h`.
    pub(crate) instance: String,
    /// The heartbeat row (pinned under the health line).
    pub(crate) beats: super::beats::BeatRow,
    /// The alert block (pinned under the heartbeat row): the raised
    /// trouble alerts, worst first.
    pub(crate) alerts: Vec<super::beats::AlertLine>,
    /// The masters' and satellites' zones, top-level first.
    pub(crate) zones: Vec<ZoneGroup>,
    /// The global zones' names (configuration only).
    pub(crate) global_zones: Vec<String>,
    /// The checks tiles and the queues and connections tiles.
    pub(crate) checks: Vec<Tile>,
    pub(crate) queues: Vec<Tile>,
    /// Icinga's global switches, read-only.
    pub(crate) switches: Vec<Switch>,
    /// The connected node's features (empty before the page asked).
    pub(crate) features: Vec<Feature>,
    /// The late checks the worst alert's *show the late checks* lists
    /// (its zone's, or every one), most overdue first.
    pub(crate) late: Vec<ObjectKey>,
    /// The verdict (the sidebar's dot).
    pub(crate) state: ClusterState,
}

/// A zone and its endpoints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ZoneGroup {
    pub(crate) name: String,
    /// Its worst endpoint's state.
    pub(crate) state: NodeState,
    /// `top level · 2 endpoints · checks shared between them`,
    /// `parent master · 1 endpoint · 188 hosts`.
    pub(crate) detail: String,
    pub(crate) endpoints: Vec<EndpointRow>,
    /// The zone's own heartbeat, if it has one.
    pub(crate) beat: Option<super::beats::BeatCell>,
}

/// An endpoint's line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EndpointRow {
    pub(crate) name: String,
    pub(crate) zone: String,
    pub(crate) state: NodeState,
    /// The node icygui talks to (the selected-row background).
    pub(crate) this_node: bool,
    pub(crate) version: String,
    pub(crate) last_message: String,
    pub(crate) traffic: String,
    pub(crate) status: String,
    /// The status's colour.
    pub(crate) tone: Tone,
    /// The heartbeat pinned to it, if it has one.
    pub(crate) beat: Option<super::beats::BeatCell>,
}

/// A stat tile: the label, the value, what it counts, and its trend.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Tile {
    /// Which tile it is (the views pick theirs).
    pub(crate) kind: Option<HealthTile>,
    pub(crate) label: &'static str,
    pub(crate) value: String,
    pub(crate) detail: String,
    pub(crate) tone: Tone,
    /// The trend, oldest first; fewer than two points draw none.
    pub(crate) trend: Vec<f64>,
    /// Two columns wide (uptime without the `IcingaDB` tile).
    pub(crate) wide: bool,
}

/// One of Icinga's global switches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Switch {
    pub(crate) label: &'static str,
    pub(crate) on: bool,
}

/// One of the node's features.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Feature {
    pub(crate) label: &'static str,
    pub(crate) state: FeatureState,
}

impl Feature {
    /// What the page says about it.
    pub(crate) fn word(&self) -> &'static str {
        match self.state {
            FeatureState::Running => "running",
            FeatureState::Paused => "paused here",
            FeatureState::Off => "off",
        }
    }
}

/// The page for `snapshot` at `now`; `connected`: the engine is connected.
#[must_use]
pub(crate) fn report(snapshot: &Snapshot, connected: bool, now: Timestamp) -> Report {
    let health = &snapshot.health;
    let nodes = snapshot.cluster_nodes();
    let seen_from = snapshot.node.as_ref().map(|node| node.name.clone());
    let status = snapshot.status.as_deref();
    let masters_version = masters_version(&nodes, snapshot, status);
    let zones = zone_groups(&nodes, snapshot, status, masters_version, now);
    let connected_count = nodes
        .iter()
        .filter(|node| node.state == NodeState::Connected)
        .count();
    let not_connected = nodes
        .iter()
        .filter(|node| node.state == NodeState::Disconnected)
        .count();
    let cut_off = cut_off_zones(&zones);
    let late_zone = cut_off.first().cloned();
    let late_in_zone = late_in(snapshot, late_zone.as_deref());
    let checks = check_tiles(
        snapshot,
        status,
        health,
        late_zone.as_deref(),
        &late_in_zone,
        !cut_off.is_empty(),
    );
    let queues = queue_tiles(health, status, now);
    let alerts = super::beats::alert_lines(snapshot, now);
    // The worst alert's late checks: its zone's, or every one.
    let late = match alerts.first().and_then(|alert| alert.link.as_ref()) {
        Some((_, ic_core::trouble::AlertAction::LateChecks { zone: Some(zone) })) => {
            late_in(snapshot, Some(zone))
        }
        Some((_, ic_core::trouble::AlertAction::LateChecks { zone: None })) => all_late(snapshot),
        _ => Vec::new(),
    };
    let mut global_zones: Vec<String> = snapshot
        .zones
        .iter()
        .filter(|zone| zone.global)
        .map(|zone| zone.name.clone())
        .collect();
    global_zones.sort();
    let instance = match status {
        Some(status) => {
            let uptime = status
                .program_start
                .non_zero()
                .map(|start| format!(" · up {}", format_two_units(start.elapsed_until(now))));
            format!("Icinga {}{}", status.version, uptime.unwrap_or_default())
        }
        None => String::new(),
    };
    let mut report = Report {
        seen_from,
        updated: health.latest().map(|sample| sample.at),
        interval: health.interval,
        quiet: snapshot.quiet,
        connected: connected_count,
        not_connected,
        instance,
        beats: super::beats::row(&snapshot.heartbeats, now),
        alerts,
        zones,
        global_zones,
        checks,
        queues,
        switches: status.map(switches).unwrap_or_default(),
        features: health.features.map(features).unwrap_or_default(),
        late,
        state: ClusterState::Unknown,
    };
    report.state = if connected && !nodes.is_empty() {
        verdict(&report, now)
    } else {
        ClusterState::Unknown
    };
    report
}

/// The cluster's state as the sidebar's *health* dot shows it, by the
/// page's rules (cheaper than the whole [`report`]: no zone is worked
/// out for the late checks).
#[must_use]
pub(crate) fn assess(snapshot: &Snapshot, connected: bool, now: Timestamp) -> ClusterState {
    let nodes = snapshot.cluster_nodes();
    if !connected || nodes.is_empty() {
        return ClusterState::Unknown;
    }
    if nodes
        .iter()
        .any(|node| node.state == NodeState::Disconnected)
    {
        return ClusterState::Critical;
    }
    let health = &snapshot.health;
    let status = snapshot.status.as_deref();
    // The trouble alerts and the heartbeats count too (no false green).
    let trouble = match snapshot.trouble.worst() {
        Some(ic_core::trouble::AlertTone::Critical) => Tone::Critical,
        Some(ic_core::trouble::AlertTone::Warning) => Tone::Warning,
        None if snapshot.trouble.blind.is_some() => Tone::Warning,
        None => Tone::Normal,
    };
    let beats = super::beats::row(&snapshot.heartbeats, now).tone.text();
    let tones = [
        late_tone(snapshot.late.len(), status, false),
        active_checks_tone(health),
        latency_tone(health),
        relay_tone(health),
        trouble,
        beats,
    ];
    let lagging = nodes.iter().any(|node| lags(snapshot, node, now));
    match tones.iter().max().copied().unwrap_or_default() {
        Tone::Critical => ClusterState::Critical,
        Tone::Warning => ClusterState::Warning,
        Tone::Normal if lagging => ClusterState::Warning,
        Tone::Normal => ClusterState::Ok,
    }
}

/// The page's verdict: critical with a node down or a critical tile,
/// warning with a lagging node or a warning tile (switches don't count).
fn verdict(report: &Report, _now: Timestamp) -> ClusterState {
    let rows = report.zones.iter().flat_map(|zone| &zone.endpoints);
    let worst = rows
        .map(|row| row.tone)
        .chain(report.checks.iter().map(|tile| tile.tone))
        .chain(report.queues.iter().map(|tile| tile.tone))
        .chain(report.alerts.iter().map(|alert| alert.tone))
        .chain(std::iter::once(report.beats.tone.text()))
        .max()
        .unwrap_or_default();
    match worst {
        Tone::Critical => ClusterState::Critical,
        Tone::Warning => ClusterState::Warning,
        Tone::Normal => ClusterState::Ok,
    }
}

/// Whether a connected endpoint (not the node itself) had sent nothing
/// for [`LAG_AFTER`] when its numbers came (as of `now` without them).
fn lags(snapshot: &Snapshot, node: &ClusterNode, now: Timestamp) -> bool {
    let as_of = snapshot.health.endpoints_at.unwrap_or(now);
    let this_node = snapshot
        .node
        .as_ref()
        .is_some_and(|connected| connected.name == node.name);
    node.state == NodeState::Connected
        && !this_node
        && snapshot
            .health
            .endpoints
            .get(&node.name)
            .and_then(|stats| stats.last_message.non_zero())
            .is_some_and(|last| last.elapsed_until(as_of) > LAG_AFTER)
}

/// The version the masters run: the newest among the top-level zones'
/// endpoints (the node's own from `/v1/status`).
fn masters_version(
    nodes: &[ClusterNode],
    snapshot: &Snapshot,
    status: Option<&InstanceStatus>,
) -> Option<Version> {
    let top: Vec<&str> = snapshot
        .zones
        .iter()
        .filter(|zone| zone.parent.is_none() && !zone.global)
        .map(|zone| zone.name.as_str())
        .collect();
    nodes
        .iter()
        .filter(|node| top.contains(&node.zone.as_str()))
        .filter_map(|node| version_of(snapshot, status, &node.name))
        .max()
}

/// An endpoint's version: the node's own from its status, the others'
/// from their numbers.
fn version_of(snapshot: &Snapshot, status: Option<&InstanceStatus>, name: &str) -> Option<Version> {
    let this_node = snapshot.node.as_ref().is_some_and(|node| node.name == name);
    if this_node {
        status.and_then(|status| Version::parse(&status.version))
    } else {
        snapshot
            .health
            .endpoints
            .get(name)
            .and_then(|stats| Version::from_number(stats.version))
    }
}

/// The zones of `nodes` in their order, each with its endpoints.
fn zone_groups(
    nodes: &[ClusterNode],
    snapshot: &Snapshot,
    status: Option<&InstanceStatus>,
    masters: Option<Version>,
    now: Timestamp,
) -> Vec<ZoneGroup> {
    // Hosts per zone, for a child zone's line.
    let mut hosts: BTreeMap<&str, usize> = BTreeMap::new();
    for host in snapshot.hosts.values() {
        if let Some(zone) = host.check.zone.as_deref() {
            *hosts.entry(zone).or_default() += 1;
        }
    }
    let mut groups: Vec<ZoneGroup> = Vec::new();
    for node in nodes {
        let row = endpoint_row(node, snapshot, status, masters, now);
        match groups.last_mut() {
            Some(group) if group.name == node.zone => group.endpoints.push(row),
            _ => groups.push(ZoneGroup {
                name: node.zone.clone(),
                state: NodeState::Connected,
                detail: String::new(),
                endpoints: vec![row],
                beat: super::beats::zone_cell(&snapshot.heartbeats, &node.zone, now),
            }),
        }
    }
    for group in &mut groups {
        group.state = group
            .endpoints
            .iter()
            .map(|row| row.state)
            .max_by_key(|state| node_rank(*state))
            .unwrap_or(NodeState::Unknown);
        let count = group.endpoints.len();
        let endpoints = if count == 1 {
            "1 endpoint".to_owned()
        } else {
            format!("{count} endpoints")
        };
        let parent = snapshot
            .zones
            .iter()
            .find(|zone| zone.name == group.name)
            .and_then(|zone| zone.parent.clone());
        group.detail = match parent {
            None if count > 1 => format!("top level · {endpoints} · checks shared between them"),
            None => format!("top level · {endpoints}"),
            Some(parent) => {
                let hosts = hosts.get(group.name.as_str()).copied().unwrap_or_default();
                let hosts = if hosts == 1 {
                    "1 host".to_owned()
                } else {
                    format!("{} hosts", count_text(hosts as u64))
                };
                format!("parent {parent} · {endpoints} · {hosts}")
            }
        };
    }
    groups
}

/// Worst first: down, unknown, connected.
fn node_rank(state: NodeState) -> u8 {
    match state {
        NodeState::Disconnected => 2,
        NodeState::Unknown => 1,
        NodeState::Connected => 0,
    }
}

/// One endpoint's line.
fn endpoint_row(
    node: &ClusterNode,
    snapshot: &Snapshot,
    status: Option<&InstanceStatus>,
    masters: Option<Version>,
    now: Timestamp,
) -> EndpointRow {
    let this_node = snapshot
        .node
        .as_ref()
        .is_some_and(|connected| connected.name == node.name);
    let numbers = snapshot.health.endpoints.get(&node.name);
    let version = version_of(snapshot, status, &node.name);
    let older = matches!((version, masters), (Some(version), Some(masters)) if version < masters);
    let last = if this_node {
        // Its own messages are the event stream's.
        snapshot.last_event_at
    } else {
        numbers.and_then(|numbers| numbers.last_message.non_zero())
    };
    // A connected endpoint's last message as of when its numbers came (it
    // keeps talking between polls); a gone one's ages from then on.
    let as_of = match snapshot.health.endpoints_at {
        Some(at) if node.state == NodeState::Connected && !this_node => at,
        _ => now,
    };
    let last_message = match (node.state, last) {
        (NodeState::Unknown, _) => "—".to_owned(),
        (_, Some(at)) => format!("{} ago", format_two_units(at.elapsed_until(as_of))),
        // Its numbers came, without a message: none ever arrived.
        (_, None) if numbers.is_some() => "never".to_owned(),
        // Not asked yet (the first status poll brings them).
        (_, None) => "—".to_owned(),
    };
    let traffic = match (this_node, node.state, numbers) {
        (true, ..) | (_, NodeState::Unknown, _) | (_, _, None) => "—".to_owned(),
        (false, _, Some(numbers)) => format!(
            "{}/s · {}/s",
            count_text(rate(numbers.messages_in)),
            count_text(rate(numbers.messages_out))
        ),
    };
    let lagging = lags(snapshot, node, now);
    let (status_text, tone) = match node.state {
        NodeState::Connected if this_node => ("connected · this node".to_owned(), Tone::Normal),
        NodeState::Connected if lagging => ("connected · no message".to_owned(), Tone::Warning),
        NodeState::Connected if older => ("connected · older version".to_owned(), Tone::Normal),
        NodeState::Connected => ("connected".to_owned(), Tone::Normal),
        NodeState::Disconnected if numbers.is_some_and(|numbers| numbers.connecting) => {
            ("not connected · retrying".to_owned(), Tone::Critical)
        }
        NodeState::Disconnected => ("not connected".to_owned(), Tone::Critical),
        NodeState::Unknown => ("not seen from here".to_owned(), Tone::Normal),
    };
    EndpointRow {
        name: node.name.clone(),
        zone: node.zone.clone(),
        state: node.state,
        this_node,
        version: version.map_or_else(|| "—".to_owned(), |version| version.to_string()),
        last_message,
        traffic,
        status: status_text,
        tone,
        beat: super::beats::endpoint_cell(&snapshot.heartbeats, &node.name, now),
    }
}

/// A rate rounded to whole messages a second.
fn rate(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        // Message rates are far below 2^53.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a finite, positive rate, rounded"
        )]
        let rate = value.round() as u64;
        rate
    } else {
        0
    }
}

/// The zones none of whose endpoints is connected.
fn cut_off_zones(zones: &[ZoneGroup]) -> Vec<String> {
    zones
        .iter()
        .filter(|zone| {
            zone.endpoints
                .iter()
                .all(|row| row.state == NodeState::Disconnected)
        })
        .map(|zone| zone.name.clone())
        .collect()
}

/// The late checks, most overdue first: those in `zone` (a host's zone, or
/// its service's), or none without one.
fn late_in(snapshot: &Snapshot, zone: Option<&str>) -> Vec<ObjectKey> {
    let Some(zone) = zone else {
        return Vec::new();
    };
    let mut late: Vec<(&ObjectKey, Timestamp)> = snapshot
        .late
        .iter()
        .filter(|(key, _)| zone_of(snapshot, key) == Some(zone))
        .map(|(key, due)| (key, *due))
        .collect();
    late.sort_by(|a, b| {
        a.1.as_unix_seconds()
            .total_cmp(&b.1.as_unix_seconds())
            .then_with(|| a.0.cmp(b.0))
    });
    late.into_iter().map(|(key, _)| key.clone()).collect()
}

/// Every late check, most overdue first.
fn all_late(snapshot: &Snapshot) -> Vec<ObjectKey> {
    let mut late: Vec<(&ObjectKey, Timestamp)> =
        snapshot.late.iter().map(|(key, due)| (key, *due)).collect();
    late.sort_by(|a, b| {
        a.1.as_unix_seconds()
            .total_cmp(&b.1.as_unix_seconds())
            .then_with(|| a.0.cmp(b.0))
    });
    late.into_iter().map(|(key, _)| key.clone()).collect()
}

/// The zone a host or service belongs to.
fn zone_of<'a>(snapshot: &'a Snapshot, key: &ObjectKey) -> Option<&'a str> {
    match key {
        ObjectKey::Host { name } => snapshot
            .hosts
            .get(name)
            .and_then(|host| host.check.zone.as_deref()),
        ObjectKey::Service { key } => snapshot
            .services
            .get(key)
            .and_then(|service| service.check.zone.as_deref()),
    }
}

/// The checks tiles.
fn check_tiles(
    snapshot: &Snapshot,
    status: Option<&InstanceStatus>,
    health: &ClusterHealth,
    late_zone: Option<&str>,
    late_in_zone: &[ObjectKey],
    cut_off: bool,
) -> Vec<Tile> {
    let samples = &health.samples;
    let series =
        |pick: fn(&HealthSample) -> f64| -> Vec<f64> { samples.iter().map(pick).collect() };
    let Some(status) = status else {
        return [
            (HealthTile::ActiveChecks, "active checks / min"),
            (HealthTile::PassiveChecks, "passive checks / min"),
            (HealthTile::Latency, "average latency"),
            (HealthTile::Execution, "average execution"),
            (HealthTile::Pending, "pending"),
            (HealthTile::Late, "late"),
        ]
        .into_iter()
        .map(|(kind, label)| Tile {
            kind: Some(kind),
            label,
            value: "—".to_owned(),
            detail: "no status yet".to_owned(),
            ..Tile::default()
        })
        .collect();
    };
    let objects = u64::from(status.counts.hosts()) + u64::from(status.counts.services());
    let late = snapshot.late.len();
    let pending_hosts = status.counts.hosts_pending;
    let pending_services = status.counts.services_pending;
    let pending_detail = match (pending_hosts, pending_services) {
        (0, 0) => "not checked yet".to_owned(),
        (0, services) => plural(u64::from(services), "service", "services"),
        (hosts, 0) => plural(u64::from(hosts), "host", "hosts"),
        (hosts, services) => format!(
            "{} · {}",
            plural(u64::from(hosts), "host", "hosts"),
            plural(u64::from(services), "service", "services")
        ),
    };
    let late_detail = match late_zone {
        Some(zone) if late > 0 && late_in_zone.len() == late => format!("all in zone {zone}"),
        Some(zone) if !late_in_zone.is_empty() => {
            format!("{} in zone {zone}", count_text(late_in_zone.len() as u64))
        }
        _ => "overdue checks".to_owned(),
    };
    let late_trend = series(|sample| f64::from(sample.late));
    vec![
        Tile {
            label: "active checks / min",
            kind: Some(HealthTile::ActiveChecks),
            value: count_text(rate(status.checks_per_minute)),
            detail: format!("of {} objects", count_text(objects)),
            tone: active_checks_tone(health),
            trend: series(|sample| sample.active_checks),
            wide: false,
        },
        Tile {
            label: "passive checks / min",
            kind: Some(HealthTile::PassiveChecks),
            value: count_text(rate(status.passive_checks_per_minute)),
            detail: "results sent in".to_owned(),
            tone: Tone::Normal,
            trend: series(|sample| sample.passive_checks),
            wide: false,
        },
        Tile {
            label: "average latency",
            kind: Some(HealthTile::Latency),
            value: crate::format::seconds(status.avg_latency),
            detail: format!("max {}", crate::format::seconds(status.max_latency)),
            tone: latency_tone(health),
            trend: series(|sample| sample.latency),
            wide: false,
        },
        Tile {
            label: "average execution",
            kind: Some(HealthTile::Execution),
            value: crate::format::seconds(status.avg_execution_time),
            detail: format!("max {}", crate::format::seconds(status.max_execution_time)),
            tone: Tone::Normal,
            trend: series(|sample| sample.execution),
            wide: false,
        },
        Tile {
            label: "pending",
            kind: Some(HealthTile::Pending),
            value: count_text(u64::from(pending_hosts) + u64::from(pending_services)),
            detail: pending_detail,
            tone: Tone::Normal,
            trend: Vec::new(),
            wide: false,
        },
        Tile {
            label: "late",
            kind: Some(HealthTile::Late),
            value: count_text(late as u64),
            detail: late_detail,
            tone: late_tone(late, Some(status), cut_off),
            // A flat zero says nothing.
            trend: if late_trend.iter().any(|late| *late > 0.0) {
                late_trend
            } else {
                Vec::new()
            },
            wide: false,
        },
    ]
}

/// The queues and connections tiles; the `IcingaDB` tile only while Icinga
/// reports the feature enabled (else uptime takes its column too).
#[expect(
    clippy::too_many_lines,
    reason = "the tiles in their order, each with its rule"
)]
fn queue_tiles(
    health: &ClusterHealth,
    status: Option<&InstanceStatus>,
    now: Timestamp,
) -> Vec<Tile> {
    let samples = &health.samples;
    let listener = health.listener.as_ref();
    let missing = || "—".to_owned();
    let waiting = "while the page is open";
    let relay_trend: Vec<f64> = samples
        .iter()
        .filter_map(|sample| sample.relay_queue)
        .collect();
    let work_trend: Vec<f64> = samples
        .iter()
        .filter_map(|sample| sample.work_queue_rate)
        .collect();
    let icingadb = health
        .features
        .and_then(|features| features.icingadb)
        .filter(|state| *state != FeatureState::Off);
    let mut tiles = vec![
        Tile {
            label: "API work queue",
            kind: Some(HealthTile::WorkQueue),
            value: listener.map_or_else(missing, |listener| {
                format!("{}/s", count_text(rate(listener.work_queue_rate)))
            }),
            detail: if listener.is_some() {
                "messages processed".to_owned()
            } else {
                waiting.to_owned()
            },
            tone: Tone::Normal,
            trend: work_trend,
            wide: false,
        },
        Tile {
            label: "relay queue",
            kind: Some(HealthTile::RelayQueue),
            value: listener.map_or_else(missing, |listener| count_text(rate(listener.relay_queue))),
            detail: match listener {
                Some(_) if relay_growing(health) => "growing".to_owned(),
                Some(_) => "for other zones".to_owned(),
                None => waiting.to_owned(),
            },
            tone: relay_tone(health),
            trend: relay_trend,
            wide: false,
        },
        Tile {
            label: "cluster connections",
            kind: Some(HealthTile::Connections),
            value: listener.map_or_else(missing, |listener| {
                format!(
                    "{} of {}",
                    count_text(u64::from(listener.connected_endpoints)),
                    count_text(u64::from(listener.endpoints))
                )
            }),
            detail: if listener.is_some() {
                "JSON-RPC endpoints".to_owned()
            } else {
                waiting.to_owned()
            },
            tone: match listener {
                Some(listener) if listener.connected_endpoints < listener.endpoints => {
                    Tone::Critical
                }
                _ => Tone::Normal,
            },
            trend: Vec::new(),
            wide: false,
        },
        Tile {
            label: "HTTP clients",
            kind: Some(HealthTile::HttpClients),
            value: listener.map_or_else(missing, |listener| {
                count_text(u64::from(listener.http_clients))
            }),
            detail: if listener.is_some() {
                "API sessions".to_owned()
            } else {
                waiting.to_owned()
            },
            tone: Tone::Normal,
            trend: Vec::new(),
            wide: false,
        },
    ];
    if let Some(state) = icingadb {
        tiles.push(Tile {
            label: "IcingaDB",
            kind: Some(HealthTile::IcingaDb),
            value: if state == FeatureState::Running {
                "on".to_owned()
            } else {
                "paused".to_owned()
            },
            detail: if state == FeatureState::Running {
                "writes to Redis here".to_owned()
            } else {
                "another master writes".to_owned()
            },
            tone: Tone::Normal,
            trend: Vec::new(),
            wide: false,
        });
    }
    let start = status.and_then(|status| status.program_start.non_zero());
    tiles.push(Tile {
        label: "uptime",
        kind: Some(HealthTile::Uptime),
        value: start.map_or_else(missing, |start| format_two_units(start.elapsed_until(now))),
        detail: start.map_or_else(String::new, |start| {
            crate::format::date_time(start, &chrono::Local).map_or_else(String::new, |at| {
                format!("since {}", at.format("%a %-d %b %H:%M"))
            })
        }),
        tone: Tone::Normal,
        trend: Vec::new(),
        wide: icingadb.is_none(),
    });
    tiles
}

/// Late checks: warning when any; critical from 1 % of the checks or
/// while a zone is cut off.
fn late_tone(late: usize, status: Option<&InstanceStatus>, cut_off: bool) -> Tone {
    if late == 0 {
        return Tone::Normal;
    }
    let objects = status.map_or(0, |status| {
        u64::from(status.counts.hosts()) + u64::from(status.counts.services())
    });
    if cut_off || (objects > 0 && (late as u64) * 100 >= objects) {
        Tone::Critical
    } else {
        Tone::Warning
    }
}

/// The median of `values` (`None` when empty).
fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[values.len() / 2])
}

/// Active checks a minute against the trend's median: warning after a fall
/// by more than a quarter, critical at none (only with four samples or
/// more, and checks before).
fn active_checks_tone(health: &ClusterHealth) -> Tone {
    let samples = &health.samples;
    if samples.len() < 4 {
        return Tone::Normal;
    }
    let Some(latest) = samples.back().map(|sample| sample.active_checks) else {
        return Tone::Normal;
    };
    let Some(median) = median(samples.iter().map(|sample| sample.active_checks).collect()) else {
        return Tone::Normal;
    };
    if median <= 0.0 {
        Tone::Normal
    } else if latest <= 0.0 {
        Tone::Critical
    } else if latest < median * 0.75 {
        Tone::Warning
    } else {
        Tone::Normal
    }
}

/// Average latency: warning at five times its median and above 50 ms, or
/// above a second; critical above ten seconds.
fn latency_tone(health: &ClusterHealth) -> Tone {
    let Some(latest) = health.latest().map(|sample| sample.latency) else {
        return Tone::Normal;
    };
    if latest > 10.0 {
        return Tone::Critical;
    }
    let jumped = health.samples.len() >= 4
        && median(health.samples.iter().map(|sample| sample.latency).collect())
            .is_some_and(|median| latest >= median * 5.0 && latest > 0.05);
    if latest > 1.0 || jumped {
        Tone::Warning
    } else {
        Tone::Normal
    }
}

/// Whether the relay queue grew over the last three polls that asked.
fn relay_growing(health: &ClusterHealth) -> bool {
    let relay: Vec<f64> = health
        .samples
        .iter()
        .filter_map(|sample| sample.relay_queue)
        .collect();
    relay.len() >= 3
        && relay[relay.len() - 3..]
            .windows(2)
            .all(|pair| pair[1] > pair[0])
}

/// The relay queue: warning while it grows; critical beyond 10 000
/// messages.
fn relay_tone(health: &ClusterHealth) -> Tone {
    let items = health
        .listener
        .as_ref()
        .map_or(0.0, |listener| listener.relay_queue);
    if items > 10_000.0 {
        Tone::Critical
    } else if relay_growing(health) {
        Tone::Warning
    } else {
        Tone::Normal
    }
}

/// Icinga's global switches.
fn switches(status: &InstanceStatus) -> Vec<Switch> {
    vec![
        Switch {
            label: "notifications",
            on: status.notifications_enabled,
        },
        Switch {
            label: "active host checks",
            on: status.host_checks_enabled,
        },
        Switch {
            label: "active service checks",
            on: status.service_checks_enabled,
        },
        Switch {
            label: "event handlers",
            on: status.event_handlers_enabled,
        },
        Switch {
            label: "flap detection",
            on: status.flap_detection_enabled,
        },
        Switch {
            label: "performance data",
            on: status.perfdata_enabled,
        },
    ]
}

/// The node's features, as far as the API user may read them.
fn features(features: ic_model::NodeFeatures) -> Vec<Feature> {
    [
        ("checker", features.checker),
        ("notification", features.notification),
        ("IcingaDB", features.icingadb),
    ]
    .into_iter()
    .filter_map(|(label, state)| state.map(|state| Feature { label, state }))
    .collect()
}

/// `count` with thousands separated by commas (`3,283`).
pub(crate) fn count_text(count: u64) -> String {
    let digits = count.to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// `1 service`, `3 services`.
fn plural(count: u64, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", count_text(count))
    }
}

#[cfg(test)]
mod tests;
