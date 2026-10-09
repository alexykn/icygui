//! Trouble alerts (PLAN.md §4.2 A, E, B3; mock-ups 16a3, 16c, 16c2,
//! 16r–16u): what says that icygui can't see, or that Icinga isn't working,
//! rather than that a host or service has a problem.
//!
//! - **No live data** (A): an environment that has had no live data for
//!   more than [`GRACE`] (connection lost, refused credentials, a stalled
//!   stream, a heartbeat the stream doesn't deliver) is *blind*
//!   ([`Trouble::blind`]): one notification naming the environment, the
//!   reason and since when, one when it is live again; every page shows
//!   the banner, the footer `no data 3m`, the tray its blind look.
//! - **Icinga health alerts** (E), from data the health page already has
//!   (no new periodic requests): an endpoint disconnected, a zone without a
//!   connected endpoint, no checks running, the checker or notification
//!   feature off, `IcingaDB` paused with no other master to write, the
//!   relay queue growing. Each must hold for [`GRACE`] (a reload or a short
//!   blip stays quiet).
//! - **Heartbeats** (B3): a dead beat is an alert at once (its time budget
//!   already waited), combined with what it says about the cluster, so one
//!   cause is one line: every beat stopped is *Icinga runs no checks*; a
//!   zone whose beat stopped while one of its endpoints is connected *runs
//!   no checks*; an endpoint that is down and whose beat is lost is one
//!   line (`zone fra: sat-fra-01 disconnected, heartbeat lost`); a beat
//!   that isn't OK is dead with Icinga's output; a beat that discovery no
//!   longer finds has *disappeared*.
//!
//! [`assess`] works out the conditions that hold now ([`Finding`]s); the
//! engine raises one once it has held for its grace and clears it when it
//! no longer holds, each with one desktop notification that no rule, mute,
//! storm limit or quiet hours hold back (only a pause does), and one log
//! line (`warn` raised, `info` cleared). While the environment is blind
//! the other alerts stay as they are: its data is not current.

use std::collections::BTreeMap;
use std::time::Duration;

use ic_model::{FeatureState, ServiceKey, Timestamp, Zone};

use crate::health::ClusterHealth;
use crate::heartbeat::{BeatState, Death, Heartbeat, Heartbeats, Proves};
use crate::topology::{ClusterNode, NodeState};

/// How long a condition must hold before it is an alert: no live data, and
/// every Icinga health alert.
pub const GRACE: Duration = Duration::from_mins(2);

/// The trouble alerts of one environment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trouble {
    /// The raised alerts, worst first (critical before warning, then the
    /// oldest).
    pub alerts: Vec<Alert>,
    /// Since when the environment has had no live data, once that held for
    /// [`GRACE`]; `None` while live.
    pub blind: Option<Blind>,
}

impl Trouble {
    /// The worst raised alert's tone (the block's tone), if any.
    #[must_use]
    pub fn worst(&self) -> Option<AlertTone> {
        self.alerts.iter().map(|alert| alert.tone).max()
    }
}

/// No live data.
#[derive(Clone, Debug, PartialEq)]
pub struct Blind {
    /// When live data stopped (local clock): the banner's and footer's
    /// age counts from it.
    pub since: Timestamp,
    /// Why, in a few words: `event stream stalled`, `connection lost`,
    /// `login refused`.
    pub reason: String,
}

/// How bad an alert is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AlertTone {
    /// Needs a look.
    Warning,
    /// Monitoring is broken somewhere.
    Critical,
}

/// What an alert's link does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AlertAction {
    /// Shows the late checks (of a zone, or all of them).
    LateChecks {
        /// The zone; `None`: every late check.
        zone: Option<String>,
    },
    /// Shows a node's row on the health page.
    ShowNode(String),
    /// Opens the environment's trouble alerts settings.
    Settings,
}

/// A raised alert.
#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    /// Stable for as long as the condition holds (one notification each
    /// way).
    pub key: String,
    /// How bad.
    pub tone: AlertTone,
    /// The block's line: `master-02 disconnected, heartbeat lost`.
    pub title: String,
    /// What it means, for the worst alert's second line.
    pub detail: String,
    /// The worst alert's link.
    pub action: Option<AlertAction>,
    /// Since when the condition holds (local clock; Icinga's times are
    /// taken as they are).
    pub since: Timestamp,
}

/// A condition that holds now, as [`assess`] finds it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Finding {
    pub(crate) key: String,
    pub(crate) tone: AlertTone,
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) action: Option<AlertAction>,
    /// When it began, when the data says (an endpoint's last message, the
    /// last beat); else from when it was first found.
    pub(crate) since: Option<Timestamp>,
    /// How long it must hold before it is raised.
    pub(crate) grace: Duration,
    /// The desktop notification's title when raised, without the
    /// environment (`master-02 disconnected`; [`Finding::notified`]).
    pub(crate) note: String,
    /// The detail of that notification after `since …` (or none).
    pub(crate) note_detail: Option<String>,
    /// The notification's title when it clears (`master-02 connected
    /// again`).
    pub(crate) recovered: String,
    /// The notification is urgent (critical urgency): nothing is being
    /// checked somewhere.
    pub(crate) urgent: bool,
}

/// What [`assess`] works from.
#[derive(Clone, Copy)]
pub(crate) struct Facts<'a> {
    /// The cluster's masters and satellites, with their states.
    pub(crate) nodes: &'a [ClusterNode],
    /// The zones.
    pub(crate) zones: &'a [Zone],
    /// The node the engine talks to.
    pub(crate) connected: Option<&'a str>,
    /// The health page's data: the endpoints' numbers, the listener, the
    /// features, the status polls' trend.
    pub(crate) health: &'a ClusterHealth,
    /// The heartbeats.
    pub(crate) heartbeats: &'a Heartbeats,
    /// How many checks are late, by zone (`None`: an object without one).
    pub(crate) late: &'a BTreeMap<Option<String>, usize>,
    /// Formats a local time as the alerts say it (`02:14`).
    pub(crate) clock: &'a dyn Fn(Timestamp) -> String,
}

impl Facts<'_> {
    fn late_in(&self, zone: Option<&str>) -> usize {
        match zone {
            None => self.late.values().sum(),
            Some(zone) => self
                .late
                .get(&Some(zone.to_owned()))
                .copied()
                .unwrap_or_default(),
        }
    }

    fn zone_of(&self, name: &str) -> Option<&Zone> {
        self.zones.iter().find(|zone| zone.name == name)
    }

    fn is_top(&self, zone: &str) -> bool {
        self.zone_of(zone).is_none_or(|zone| zone.parent.is_none())
    }

    fn nodes_in<'b>(&'b self, zone: &'b str) -> impl Iterator<Item = &'b ClusterNode> + 'b {
        self.nodes.iter().filter(move |node| node.zone == zone)
    }

    /// When an endpoint last talked to the connected node.
    fn last_message(&self, endpoint: &str) -> Option<Timestamp> {
        self.health
            .endpoints
            .get(endpoint)
            .and_then(|stats| stats.last_message.non_zero())
    }
}

/// A beat that is dead or on its way to being found dead: it says that
/// something stopped (a late beat whose REST query hasn't answered counts
/// as lost for an endpoint that is already known to be down).
fn lost(beat: &Heartbeat) -> bool {
    matches!(
        beat.state,
        BeatState::Dead(_) | BeatState::Late | BeatState::Checking
    )
}

/// `zone fra: ` for a child zone, nothing for the top-level zone.
fn zone_prefix(facts: &Facts<'_>, zone: &str) -> String {
    if facts.is_top(zone) {
        String::new()
    } else {
        format!("zone {zone}: ")
    }
}

/// `master-01`, `master-01 and master-02`, `a, b and c`.
fn names(list: &[&str]) -> String {
    match list {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `3,283`.
fn count(value: usize) -> String {
    let digits = value.to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// `1 check`, `3 checks`.
fn checks(value: usize) -> String {
    if value == 1 {
        "1 check".to_owned()
    } else {
        format!("{} checks", count(value))
    }
}

/// The conditions that hold now (see the module notes for the rules and
/// how one cause becomes one line).
#[expect(
    clippy::too_many_lines,
    reason = "the rules in their order of precedence, each with its words"
)]
pub(crate) fn assess(facts: &Facts<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();
    let beats = facts.heartbeats;
    let watched: Vec<&Heartbeat> = beats.watched().collect();
    // The beats that are covered by a line already (one cause, one line).
    let mut covered: Vec<&ServiceKey> = Vec::new();

    // --- No checks: every beat stopped, or the status poll says none ran.
    let stopped: Vec<&Heartbeat> = watched
        .iter()
        .copied()
        .filter(|beat| beat.state == BeatState::Dead(Death::Stopped))
        .collect();
    let all_stopped = !watched.is_empty() && stopped.len() == watched.len();
    let samples = &facts.health.samples;
    let polled_none = samples.len() >= 2
        && samples.back().is_some_and(|sample| sample.active_checks <= 0.0)
        && samples.iter().any(|sample| sample.active_checks > 0.0);
    let no_checks = all_stopped || polled_none;
    if no_checks {
        let last_beat = latest(stopped.iter().filter_map(|beat| beat.last_beat));
        let last_poll = samples
            .iter()
            .rev()
            .find(|sample| sample.active_checks > 0.0)
            .map(|sample| sample.at);
        let since = last_beat.or(last_poll);
        let before = samples
            .iter()
            .rev()
            .find(|sample| sample.active_checks > 0.0)
            .map(|sample| sample.active_checks);
        let late = facts.late_in(None);
        let mut parts = Vec::new();
        if let Some(before) = before {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a positive rate, rounded"
            )]
            let rate = before.round() as usize;
            parts.push(format!("active checks fell from {} a minute to 0", count(rate)));
        }
        let mut late_part = match late {
            0 => String::new(),
            1 => "1 check is late".to_owned(),
            late => format!("{} checks are late", count(late)),
        };
        if all_stopped && let Some(at) = last_beat {
            let beats = format!("every heartbeat stopped at {}", (facts.clock)(at));
            late_part = if late_part.is_empty() {
                beats
            } else {
                format!("{late_part}, and {beats}")
            };
        }
        parts.push(late_part);
        let detail = sentence(&parts);
        let when = since.map_or_else(String::new, |at| format!(" since {}", (facts.clock)(at)));
        findings.push(Finding {
            key: "no-checks".to_owned(),
            tone: AlertTone::Critical,
            title: format!("Icinga runs no checks: none{when}."),
            detail,
            action: (late > 0).then_some(AlertAction::LateChecks { zone: None }),
            since,
            grace: if all_stopped { Duration::ZERO } else { GRACE },
            note: "Icinga runs no checks".to_owned(),
            note_detail: None,
            recovered: "Icinga runs checks again".to_owned(),
            urgent: true,
        });
    }

    // --- Endpoints that are down, and zones cut off, with their beats.
    let mut zones_done: Vec<String> = Vec::new();
    for node in facts.nodes {
        if node.state != NodeState::Disconnected || facts.connected == Some(node.name.as_str()) {
            continue;
        }
        let zone = node.zone.as_str();
        let connected_in_zone: Vec<&str> = facts
            .nodes_in(zone)
            .filter(|other| other.state == NodeState::Connected)
            .map(|other| other.name.as_str())
            .collect();
        let pinned = beats
            .of_endpoint(&node.name)
            .filter(|beat| beat.state.is_watched());
        let zone_beat = beats.of_zone(zone).filter(|beat| beat.state.is_watched());
        if connected_in_zone.is_empty() && !facts.is_top(zone) {
            // The zone is cut off: one line for it and its endpoints.
            if zones_done.iter().any(|done| done == zone) {
                continue;
            }
            zones_done.push(zone.to_owned());
            let down: Vec<&str> = facts
                .nodes_in(zone)
                .filter(|other| other.state == NodeState::Disconnected)
                .map(|other| other.name.as_str())
                .collect();
            let zone_beats: Vec<&Heartbeat> = watched
                .iter()
                .copied()
                .filter(|beat| beat.proves.zone() == Some(zone))
                .collect();
            let beat_lost = zone_beats.iter().any(|beat| lost(beat));
            covered.extend(zone_beats.iter().map(|beat| &beat.key));
            let since = latest(down.iter().filter_map(|name| facts.last_message(name)));
            let late = facts.late_in(Some(zone));
            let relay = relay_growing(facts.health);
            let mut parts = vec![format!("zone {zone}’s results are stale")];
            if late > 0 {
                parts.push(format!(
                    "{} in zone {zone} {} late",
                    checks(late),
                    if late == 1 { "is" } else { "are" }
                ));
            }
            if relay {
                parts.push("the relay queue is growing".to_owned());
            }
            findings.push(Finding {
                key: format!("zone:{zone}"),
                tone: AlertTone::Critical,
                title: format!(
                    "zone {zone}: {} disconnected{}",
                    names(&down),
                    if beat_lost { ", heartbeat lost" } else { "" }
                ),
                detail: sentence(&parts),
                action: Some(if late > 0 {
                    AlertAction::LateChecks {
                        zone: Some(zone.to_owned()),
                    }
                } else {
                    AlertAction::ShowNode(down.first().copied().unwrap_or_default().to_owned())
                }),
                since,
                grace: GRACE,
                note: format!("zone {zone} has no connected endpoint"),
                note_detail: Some(format!("{} disconnected", names(&down))),
                recovered: format!("zone {zone} connected again"),
                urgent: true,
            });
            continue;
        }
        // One endpoint down, its zone still has another (or it is a
        // master): one line, with its pinned beat.
        let prefix = zone_prefix(facts, zone);
        let beat_lost = pinned.is_some_and(lost);
        if let Some(beat) = pinned {
            covered.push(&beat.key);
        }
        let since = facts.last_message(&node.name);
        let others = names(&connected_in_zone);
        let (title, detail, urgent, tone) = match pinned {
            Some(beat) if beat.state == BeatState::Dead(Death::NotOk) => {
                let reason = beat.reason.clone().unwrap_or_default();
                let runs = if others.is_empty() {
                    String::new()
                } else if facts.is_top(zone) {
                    format!("; {others} {} every {zone} check", verb(&connected_in_zone))
                } else {
                    format!("; {others} {} zone {zone}’s checks", verb(&connected_in_zone))
                };
                (
                    format!(
                        "heartbeat {} dead: {}",
                        node.name,
                        first_line(&reason, &node.name)
                    ),
                    format!(
                        "Icinga’s result for the check pinned to {} (UNKNOWN){runs}.",
                        node.name
                    ),
                    true,
                    AlertTone::Critical,
                )
            }
            _ => {
                let detail = if others.is_empty() {
                    String::new()
                } else if facts.is_top(zone) {
                    format!("{others} {} every {zone} check.", verb(&connected_in_zone))
                } else {
                    let zone_beat = match zone_beat {
                        Some(beat) if beat.state == BeatState::OnTime => {
                            format!("; the zone {zone} beat is on time")
                        }
                        _ => String::new(),
                    };
                    format!(
                        "{others} {} zone {zone}’s checks alone{zone_beat}.",
                        verb(&connected_in_zone)
                    )
                };
                (
                    format!(
                        "{prefix}{} disconnected{}",
                        node.name,
                        if beat_lost { ", heartbeat lost" } else { "" }
                    ),
                    detail,
                    beat_lost,
                    if beat_lost {
                        AlertTone::Critical
                    } else {
                        AlertTone::Warning
                    },
                )
            }
        };
        findings.push(Finding {
            key: format!("endpoint:{}", node.name),
            tone,
            title,
            detail,
            action: Some(AlertAction::ShowNode(node.name.clone())),
            since,
            grace: GRACE,
            note: format!("{} disconnected", node.name),
            note_detail: beat_lost.then(|| "heartbeat lost".to_owned()),
            recovered: format!("{} connected again", node.name),
            urgent,
        });
    }

    // --- Zones whose beat stopped while an endpoint of theirs answers.
    for beat in &watched {
        let Proves::Zone(zone) = &beat.proves else {
            continue;
        };
        if covered.contains(&&beat.key) || zones_done.contains(zone) {
            continue;
        }
        if beat.state != BeatState::Dead(Death::Stopped) || no_checks {
            continue;
        }
        let connected: Vec<&str> = facts
            .nodes_in(zone)
            .filter(|node| node.state == NodeState::Connected)
            .map(|node| node.name.as_str())
            .collect();
        let pinned_stopped: Vec<&Heartbeat> = watched
            .iter()
            .copied()
            .filter(|other| {
                other.state == BeatState::Dead(Death::Stopped)
                    && matches!(&other.proves, Proves::Endpoint { endpoint, .. } if connected.contains(&endpoint.as_str()))
            })
            .collect();
        covered.push(&beat.key);
        covered.extend(pinned_stopped.iter().map(|other| &other.key));
        let at = beat.last_beat.map(|at| (facts.clock)(at));
        let late = facts.late_in(Some(zone));
        let who = names(&connected);
        let mut parts = Vec::new();
        if !connected.is_empty() {
            let what = if pinned_stopped.is_empty() {
                format!("the zone {zone} beat")
            } else if pinned_stopped.len() == 1 && connected.len() == 1 {
                format!("its beat and the zone {zone} beat")
            } else {
                format!("their beats and the zone {zone} beat")
            };
            parts.push(format!(
                "{who} {}, but {what} stopped{}",
                if connected.len() == 1 { "answers" } else { "answer" },
                at.as_ref().map_or_else(String::new, |at| format!(" at {at}"))
            ));
        }
        if late > 0 {
            parts.push(format!(
                "{} in zone {zone} {} late",
                checks(late),
                if late == 1 { "is" } else { "are" }
            ));
        }
        findings.push(Finding {
            key: format!("zone-silent:{zone}"),
            tone: AlertTone::Critical,
            title: if connected.is_empty() {
                format!("zone {zone} runs no checks")
            } else {
                format!("zone {zone} runs no checks ({who} connected)")
            },
            detail: sentence(&parts),
            action: (late > 0).then(|| AlertAction::LateChecks {
                zone: Some(zone.clone()),
            }),
            since: beat.last_beat,
            grace: Duration::ZERO,
            note: format!("zone {zone} runs no checks"),
            note_detail: (!connected.is_empty()).then(|| format!("{who} connected")),
            recovered: format!("zone {zone} runs checks again"),
            urgent: true,
        });
    }

    // --- The other dead beats: not OK (Icinga's reason), or a connected
    // endpoint whose own beat stopped.
    for beat in &watched {
        if covered.contains(&&beat.key) {
            continue;
        }
        let subject = beat.proves.subject().to_owned();
        match beat.state {
            BeatState::Dead(Death::NotOk) => {
                let reason = beat.reason.clone().unwrap_or_default();
                findings.push(Finding {
                    key: format!("beat:{}", beat.object()),
                    tone: AlertTone::Critical,
                    title: format!("heartbeat {subject} dead: {}", first_line(&reason, &subject)),
                    detail: format!("Icinga’s result for {} is not OK.", beat.object()),
                    action: Some(match &beat.proves {
                        Proves::Endpoint { endpoint, .. } => AlertAction::ShowNode(endpoint.clone()),
                        Proves::Zone(_) => AlertAction::Settings,
                    }),
                    since: Some(beat.since),
                    // Like the endpoint it may be pinned to: a check pinned
                    // to an endpoint that went away is UNKNOWN at once, and
                    // the endpoint's line takes it in within this grace.
                    grace: GRACE,
                    note: format!("heartbeat {subject} dead"),
                    note_detail: Some(first_line(&reason, &subject)),
                    recovered: format!("heartbeat {subject} back"),
                    urgent: true,
                });
            }
            BeatState::Dead(Death::Stopped) if !no_checks => {
                let Proves::Endpoint { endpoint, zone } = &beat.proves else {
                    // A zone's beat: the zone rule above.
                    continue;
                };
                let prefix = zone
                    .as_deref()
                    .map_or_else(String::new, |zone| zone_prefix(facts, zone));
                let at = beat.last_beat.map(|at| (facts.clock)(at));
                findings.push(Finding {
                    key: format!("endpoint-silent:{endpoint}"),
                    tone: AlertTone::Critical,
                    title: format!("{prefix}{endpoint} runs no checks (connected)"),
                    detail: format!(
                        "{endpoint} answers, but its beat stopped{}.",
                        at.map_or_else(String::new, |at| format!(" at {at}"))
                    ),
                    action: Some(AlertAction::ShowNode(endpoint.clone())),
                    since: beat.last_beat,
                    grace: Duration::ZERO,
                    note: format!("{endpoint} runs no checks"),
                    note_detail: None,
                    recovered: format!("{endpoint} runs checks again"),
                    urgent: true,
                });
            }
            _ => {}
        }
    }

    // --- Heartbeats that disappeared: findings until confirmed.
    for beat in &beats.beats {
        if beat.state != BeatState::Disappeared {
            continue;
        }
        let subject = beat.proves.subject().to_owned();
        let at = (facts.clock)(beat.since);
        findings.push(Finding {
            key: format!("gone:{}", beat.object()),
            tone: AlertTone::Warning,
            title: format!("heartbeat {subject} disappeared since {at}"),
            detail: format!(
                "discovery no longer finds {}; {} is unwatched until it is back or removed.",
                beat.object(),
                beat.proves.label()
            ),
            action: Some(AlertAction::Settings),
            since: Some(beat.since),
            grace: Duration::ZERO,
            note: format!("heartbeat {subject} disappeared"),
            note_detail: None,
            recovered: format!("heartbeat {subject} back"),
            urgent: false,
        });
    }

    // --- The connected node's features.
    if let (Some(node), Some(features)) = (facts.connected, facts.health.features) {
        if features.checker == Some(FeatureState::Off) {
            findings.push(Finding {
                key: "feature:checker".to_owned(),
                tone: AlertTone::Critical,
                title: format!("the checker feature is off on {node}"),
                detail: format!("{node} runs no checks while it is off."),
                action: Some(AlertAction::ShowNode(node.to_owned())),
                since: None,
                grace: GRACE,
                note: format!("checker feature off on {node}"),
                note_detail: None,
                recovered: format!("checker feature running again on {node}"),
                urgent: true,
            });
        }
        if features.notification == Some(FeatureState::Off) {
            findings.push(Finding {
                key: "feature:notification".to_owned(),
                tone: AlertTone::Warning,
                title: format!("the notification feature is off on {node}"),
                detail: format!("Icinga sends no notifications from {node} while it is off."),
                action: Some(AlertAction::ShowNode(node.to_owned())),
                since: None,
                grace: GRACE,
                note: format!("notification feature off on {node}"),
                note_detail: None,
                recovered: format!("notification feature running again on {node}"),
                urgent: false,
            });
        }
        if features.icingadb == Some(FeatureState::Paused) {
            // Paused is normal on the second master of an HA zone while the
            // first writes; it is trouble when no other master is there.
            let zone = facts
                .nodes
                .iter()
                .find(|other| other.name == node)
                .map(|other| other.zone.as_str());
            let partner = zone.is_some_and(|zone| {
                facts
                    .nodes_in(zone)
                    .any(|other| other.name != node && other.state == NodeState::Connected)
            });
            if !partner {
                findings.push(Finding {
                    key: "icingadb".to_owned(),
                    tone: AlertTone::Warning,
                    title: format!("IcingaDB paused on {node}"),
                    detail: "no other master is connected to write to IcingaDB.".to_owned(),
                    action: Some(AlertAction::ShowNode(node.to_owned())),
                    since: None,
                    grace: GRACE,
                    note: format!("IcingaDB paused on {node}"),
                    note_detail: None,
                    recovered: format!("IcingaDB writes again on {node}"),
                    urgent: false,
                });
            }
        }
    }

    // --- The relay queue keeps growing.
    if relay_growing(facts.health) && zones_done.is_empty() {
        let items = facts
            .health
            .listener
            .as_ref()
            .map_or(0.0, |listener| listener.relay_queue);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a positive count, rounded"
        )]
        let items = items.max(0.0).round() as usize;
        findings.push(Finding {
            key: "queues".to_owned(),
            tone: AlertTone::Warning,
            title: format!("the relay queue keeps growing ({} messages)", count(items)),
            detail: "messages for other zones wait on the connected node; a zone may be slow to take them."
                .to_owned(),
            action: None,
            since: None,
            grace: GRACE,
            note: "the relay queue keeps growing".to_owned(),
            note_detail: None,
            recovered: "the relay queue is back to normal".to_owned(),
            urgent: false,
        });
    }
    findings
}

/// The latest of some times.
fn latest(times: impl Iterator<Item = Timestamp>) -> Option<Timestamp> {
    times.max_by(|a, b| a.as_unix_seconds().total_cmp(&b.as_unix_seconds()))
}

/// `runs` or `run`, after one or more names.
fn verb(names: &[&str]) -> &'static str {
    if names.len() == 1 { "runs" } else { "run" }
}

/// Parts joined by `; `, as a sentence (empty without parts).
fn sentence(parts: &[String]) -> String {
    let parts: Vec<&str> = parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("{}.", parts.join("; "))
    }
}

/// The first line of a plugin output, without Icinga's `to '<node>'`
/// tail where it names the node that tried (`Remote Icinga instance
/// 'master-02' is not connected`), or `not OK` when there is none.
fn first_line(output: &str, subject: &str) -> String {
    let line = output.lines().next().unwrap_or_default().trim();
    if line.is_empty() {
        return format!("{subject} is not OK");
    }
    match line.find(" is not connected to '") {
        Some(at) => format!("{} is not connected", &line[..at]),
        None => line.to_owned(),
    }
}

/// Whether the relay queue grew over the last three polls that asked for
/// the listener status.
pub(crate) fn relay_growing(health: &ClusterHealth) -> bool {
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

/// Whether `id` is the id of a trouble alert's notification (raised or
/// cleared): `trouble:<key>:<since>` and `trouble:<key>:<since>:cleared`.
#[must_use]
pub fn is_trouble_id(id: &str) -> bool {
    id.starts_with("trouble:")
}

/// The id of the notification a clearing one ends (`…:cleared` → the
/// raise's id), so the desktop takes that one away.
#[must_use]
pub fn raised_id(id: &str) -> Option<&str> {
    id.strip_suffix(":cleared").filter(|_| is_trouble_id(id))
}

#[cfg(test)]
mod tests;
