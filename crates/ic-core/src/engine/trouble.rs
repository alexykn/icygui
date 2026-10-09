//! Raising and clearing the trouble alerts ([`crate::trouble`], PLAN.md
//! §4.2 A, E): the engine works out what holds every
//! [`ASSESS_INTERVAL`] (and at once when a heartbeat changes), raises a
//! condition once it held for its grace and clears it when it no longer
//! holds. Each transition is one log line (`warn` raised, `info` cleared)
//! and one notification, which no rule, mute, storm limit or quiet hours
//! hold back: only a pause does (it is recorded silent then, like any
//! paused notification).
//!
//! *No live data* is the engine's own: it begins when a session fails
//! (or, for a stalled stream, when the last line came) and ends when one
//! goes live again; after [`crate::trouble::GRACE`] the environment is *blind*. A beat the
//! stream doesn't deliver even after a reconnect makes it blind too.
//! While the engine has no live connection, the other alerts stay as they
//! are: its data isn't current.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use ic_model::{InstanceStatus, ObjectKey, Timestamp};
use ic_rules::{NotificationIntent, Silence, Tone};
use tokio::time::Instant;

use super::{Engine, Phase};
use crate::command::CoreEvent;
use crate::connect::Failure;
use crate::heartbeat::{BeatState, Death};
use crate::ports::clock_time;
use crate::trouble::{self, Alert, Blind, Facts, Finding, Trouble};

/// How often the conditions are worked out (and at once when a heartbeat
/// changes).
pub(super) const ASSESS_INTERVAL: Duration = Duration::from_secs(5);

/// A condition found, not raised yet.
#[derive(Clone, Copy, Debug)]
struct Pending {
    first: Instant,
    first_wall: Timestamp,
}

/// A raised alert.
#[derive(Clone, Debug)]
struct Raised {
    alert: Alert,
    /// Its notification's id.
    id: String,
    /// The notification's title when it clears.
    recovered: String,
}

/// No live data since.
#[derive(Clone, Debug)]
struct Dark {
    at: Instant,
    since: Timestamp,
    reason: String,
}

/// The trouble alerts' state.
#[derive(Debug, Default)]
pub(super) struct Tracker {
    pending: BTreeMap<String, Pending>,
    raised: BTreeMap<String, Raised>,
    /// No live connection since.
    dark: Option<Dark>,
    /// The raised *no live data* alert: its notification's id, and what
    /// the snapshots show.
    blind: Option<(String, Blind)>,
    /// The next assessment.
    assess_at: Option<Instant>,
    /// Restarts announced (node, start time in seconds).
    restarts: BTreeSet<(String, i64)>,
    /// What the snapshots carry.
    published: Arc<Trouble>,
    /// It changed since the last snapshot.
    pub(super) news: bool,
}

impl Tracker {
    /// What the snapshots carry.
    pub(super) fn published(&self) -> &Arc<Trouble> {
        &self.published
    }

    /// When the next assessment is due.
    pub(super) fn due(&self) -> Option<Instant> {
        self.assess_at
    }
}

/// The few words a failure is told by (`connection lost`).
fn reason_of(failure: &Failure) -> String {
    match failure {
        Failure::Transient(error) if error.contains("stalled") => "event stream stalled".to_owned(),
        Failure::Transient(error) if error.contains("heartbeat") => {
            "event stream lost results".to_owned()
        }
        Failure::Transient(_) | Failure::TransientUntrusted { .. } => "connection lost".to_owned(),
        Failure::MissingSecret => "no password".to_owned(),
        Failure::Misconfigured(_) => "settings can't work".to_owned(),
        Failure::Auth(_) => "login refused".to_owned(),
        Failure::Tls { .. } => "certificate not trusted".to_owned(),
    }
}

/// `45s`, `6m`, `2h 5m`, `3d`.
fn span(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => match (seconds % 3_600) / 60 {
            0 => format!("{}h", seconds / 3_600),
            minutes => format!("{}h {minutes}m", seconds / 3_600),
        },
        _ => format!("{}d", seconds / 86_400),
    }
}

/// Whole milliseconds, for ids.
fn millis(at: Timestamp) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "milliseconds since 1970 fit an i64"
    )]
    let millis = (at.as_unix_seconds() * 1_000.0).round() as i64;
    millis
}

/// A wall clock that moved this much further than the monotonic one
/// between two ticks means the computer slept.
const SLEPT: Duration = Duration::from_secs(30);

impl Engine {
    /// Every tick: says the engine runs ([`CoreEvent::Alive`], every
    /// [`crate::ALIVE_INTERVAL`]) and notices the computer waking up (the
    /// heartbeats get their grace).
    pub(super) fn alive(&mut self, now: Instant) {
        let wall = self.ports.clock.now();
        if let Some((then, then_wall)) = self.last_tick {
            let monotonic = now.saturating_duration_since(then).as_secs_f64();
            let walled = wall.as_unix_seconds() - then_wall.as_unix_seconds();
            if walled - monotonic > SLEPT.as_secs_f64() {
                tracing::info!(slept = %span(Duration::from_secs_f64(walled - monotonic)), "the computer woke up");
                self.beats_grace("the computer woke up");
            }
        }
        self.last_tick = Some((now, wall));
        if self.alive_at <= now {
            self.alive_at = now + crate::ALIVE_INTERVAL;
            // Not held back behind an evaluation: it says the engine runs.
            self.send_event(CoreEvent::Alive(wall));
        }
    }

    /// Works the conditions out at once (a heartbeat changed).
    pub(super) fn trouble_due_now(&mut self) {
        self.trouble.assess_at = Some(Instant::now());
    }

    /// The session failed: no live data from now (a stalled stream: since
    /// its last line), unless that began earlier.
    pub(super) fn go_dark(&mut self, failure: &Failure) {
        let reason = reason_of(failure);
        let stalled = matches!(failure, Failure::Transient(error) if error.contains("stalled"));
        let (at, since) = match self.last_heard {
            Some((at, since)) if stalled => (at, since),
            _ => (Instant::now(), self.ports.clock.now()),
        };
        match &mut self.trouble.dark {
            Some(dark) => {
                if dark.reason != reason {
                    dark.reason = reason;
                    if let Some((_, blind)) = &mut self.trouble.blind {
                        blind.reason.clone_from(&dark.reason);
                        self.trouble.news = true;
                    }
                }
            }
            None => {
                tracing::debug!(%reason, "no live data");
                self.trouble.dark = Some(Dark { at, since, reason });
            }
        }
        self.trouble_due_now();
    }

    /// Live again.
    pub(super) fn go_bright(&mut self) {
        if self.trouble.dark.take().is_some() {
            self.trouble_due_now();
        }
    }

    /// Announces an Icinga restart once (information, not an alert), and
    /// gives the heartbeats their grace.
    pub(super) fn icinga_restarted(&mut self, status: &InstanceStatus) {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "seconds since 1970 fit an i64"
        )]
        let start = status.program_start.as_unix_seconds().round() as i64;
        self.beats_grace("Icinga restarted");
        if !self
            .trouble
            .restarts
            .insert((status.node_name.clone(), start))
        {
            return;
        }
        let node = if status.node_name.is_empty() {
            "Icinga".to_owned()
        } else {
            status.node_name.clone()
        };
        tracing::info!(%node, "Icinga restarted");
        let body = format!("at {}", clock_time(status.program_start));
        let id = format!("trouble:restart:{node}:{start}");
        let title = format!("{}: {node} restarted", self.spec.environment.name);
        self.trouble_notify(id, title, body, Tone::Info);
    }

    /// Works out what holds, raises and clears.
    pub(super) fn assess_trouble(&mut self, now: Instant) {
        self.trouble.assess_at = Some(now + ASSESS_INTERVAL);
        let wall = self.ports.clock.now();
        self.refresh_beats();
        self.assess_blind(now, wall);
        if self.trouble.dark.is_some() || self.phase != Phase::Live {
            // Nothing current to judge by: the alerts stay as they are.
            self.publish_trouble();
            return;
        }
        let findings = self.findings();
        let mut found: BTreeSet<String> = BTreeSet::new();
        for finding in findings {
            found.insert(finding.key.clone());
            self.consider(finding, now, wall);
        }
        // What no longer holds.
        self.trouble.pending.retain(|key, _| found.contains(key));
        let over: Vec<String> = self
            .trouble
            .raised
            .keys()
            .filter(|key| !found.contains(*key))
            .cloned()
            .collect();
        for key in over {
            if let Some(raised) = self.trouble.raised.remove(&key) {
                self.clear_alert(raised, wall);
            }
        }
        self.publish_trouble();
    }

    /// *No live data*: raised once it held for [`crate::trouble::GRACE`], cleared when
    /// live (and every beat delivered) again.
    fn assess_blind(&mut self, now: Instant, wall: Timestamp) {
        let undelivered = self
            .beats
            .published()
            .watched()
            .filter(|beat| beat.state == BeatState::Dead(Death::NotDelivered))
            .map(|beat| (beat.last_beat.unwrap_or(beat.since), beat.proves.subject().to_owned()))
            .min_by(|a, b| a.0.as_unix_seconds().total_cmp(&b.0.as_unix_seconds()));
        let dark = self
            .trouble
            .dark
            .as_ref()
            .map(|dark| {
                let held = now.saturating_duration_since(dark.at);
                (dark.since, dark.reason.clone(), held)
            })
            .or_else(|| {
                undelivered.map(|(since, subject)| {
                    let held = Duration::from_secs_f64(
                        (wall.as_unix_seconds() - since.as_unix_seconds()).max(0.0),
                    );
                    (
                        since,
                        format!("heartbeat {subject} not delivered"),
                        // The reconnect already waited for it.
                        held.max(self.tuning.trouble_grace),
                    )
                })
            });
        match (dark, self.trouble.blind.take()) {
            (Some((since, reason, held)), None) => {
                if held >= self.tuning.trouble_grace {
                    tracing::warn!(
                        since = %clock_time(since),
                        %reason,
                        "no live data"
                    );
                    let id = format!("trouble:blind:{}", millis(since));
                    let title = format!("no live data from {}", self.spec.environment.name);
                    let body = format!("since {} · {reason}", clock_time(since));
                    self.trouble_notify(id.clone(), title, body, Tone::Warning);
                    self.trouble.blind = Some((id, Blind { since, reason }));
                    self.trouble.news = true;
                }
            }
            (Some((_, reason, _)), Some((id, mut blind))) => {
                if blind.reason != reason {
                    blind.reason = reason;
                    self.trouble.news = true;
                }
                self.trouble.blind = Some((id, blind));
            }
            (None, Some((id, blind))) => {
                let after = Duration::from_secs_f64(
                    (wall.as_unix_seconds() - blind.since.as_unix_seconds()).max(0.0),
                );
                tracing::info!(after = %span(after), "live data again");
                let title = format!("{} live again", self.spec.environment.name);
                let body = format!("after {}", span(after));
                self.trouble_notify(format!("{id}:cleared"), title, body, Tone::Recovery);
                self.trouble.news = true;
            }
            (None, None) => {}
        }
    }

    /// The conditions that hold now.
    fn findings(&self) -> Vec<Finding> {
        let Some(conn) = &self.conn else {
            return Vec::new();
        };
        let (endpoints, zones) = self.store.cluster();
        let nodes = crate::topology::cluster_nodes(zones, endpoints, Some(&conn.node));
        let late = self.late_by_zone();
        let health = self.store.health();
        let clock = |at: Timestamp| clock_time(at);
        let facts = Facts {
            nodes: &nodes,
            zones,
            connected: Some(conn.node.name.as_str()),
            health,
            heartbeats: self.beats.published(),
            late: &late,
            clock: &clock,
        };
        trouble::assess(&facts)
    }

    /// How many checks are late, by zone.
    fn late_by_zone(&self) -> BTreeMap<Option<String>, usize> {
        let mut late: BTreeMap<Option<String>, usize> = BTreeMap::new();
        for key in self.watchdog.late().keys() {
            let zone = match key {
                ObjectKey::Host { name } => self
                    .store
                    .hosts()
                    .get(name)
                    .and_then(|host| host.check.zone.clone()),
                ObjectKey::Service { key } => self
                    .store
                    .services()
                    .get(key)
                    .and_then(|service| service.check.zone.clone()),
            };
            *late.entry(zone).or_default() += 1;
        }
        late
    }

    /// One condition that holds: kept up to date if raised, raised once it
    /// held for its grace.
    fn consider(&mut self, finding: Finding, now: Instant, wall: Timestamp) {
        if let Some(raised) = self.trouble.raised.get_mut(&finding.key) {
            let alert = Alert {
                key: finding.key.clone(),
                tone: finding.tone,
                title: finding.title,
                detail: finding.detail,
                action: finding.action,
                since: raised.alert.since,
            };
            if alert != raised.alert {
                raised.alert = alert;
                self.trouble.news = true;
            }
            raised.recovered = finding.recovered;
            return;
        }
        let from_beats = ["no-checks", "zone-silent:", "endpoint-silent:", "beat:"]
            .iter()
            .any(|prefix| finding.key.starts_with(prefix));
        // A zone or an endpoint whose beat stopped waits until every other
        // beat had the time to stop too: if they all do, that is one line
        // (Icinga runs no checks), not one per zone.
        let settling = finding.key.starts_with("zone-silent:")
            || finding.key.starts_with("endpoint-silent:");
        let pending = *self
            .trouble
            .pending
            .entry(finding.key.clone())
            .or_insert(Pending {
                first: now,
                first_wall: wall,
            });
        let since = finding.since.unwrap_or(pending.first_wall);
        let waited = now.saturating_duration_since(pending.first);
        let held = if from_beats {
            // The beats' own times say when they stopped, not how long
            // the verdict has held.
            waited
        } else {
            waited.max(Duration::from_secs_f64(
                (wall.as_unix_seconds() - since.as_unix_seconds()).max(0.0),
            ))
        };
        let grace = if settling {
            self.beats_settle()
        } else {
            finding.grace.min(self.tuning.trouble_grace)
        };
        if held < grace {
            return;
        }
        if from_beats
            && self
                .beats
                .published()
                .watched()
                .any(|beat| matches!(beat.state, BeatState::Late | BeatState::Checking))
        {
            // Another beat's verdict is on its way: one cause is one line
            // once they are all in (every beat stopped, not each zone).
            return;
        }
        self.trouble.pending.remove(&finding.key);
        let id = format!("trouble:{}:{}", finding.key, millis(since));
        let title = format!("{}: {}", self.spec.environment.name, finding.note);
        let body = match &finding.note_detail {
            Some(detail) => format!("since {} · {detail}", clock_time(since)),
            None => format!("since {}", clock_time(since)),
        };
        tracing::warn!(alert = %finding.title, since = %clock_time(since), "trouble alert");
        let tone = if finding.urgent {
            Tone::Critical
        } else {
            Tone::Warning
        };
        self.trouble_notify(id.clone(), title, body, tone);
        self.trouble.raised.insert(
            finding.key.clone(),
            Raised {
                alert: Alert {
                    key: finding.key,
                    tone: finding.tone,
                    title: finding.title,
                    detail: finding.detail,
                    action: finding.action,
                    since,
                },
                id,
                recovered: finding.recovered,
            },
        );
        self.trouble.news = true;
    }

    /// A raised alert no longer holds.
    fn clear_alert(&mut self, raised: Raised, wall: Timestamp) {
        let after = Duration::from_secs_f64(
            (wall.as_unix_seconds() - raised.alert.since.as_unix_seconds()).max(0.0),
        );
        tracing::info!(alert = %raised.alert.title, after = %span(after), "trouble alert cleared");
        let title = format!("{}: {}", self.spec.environment.name, raised.recovered);
        let body = format!("after {}", span(after));
        self.trouble_notify(format!("{}:cleared", raised.id), title, body, Tone::Recovery);
        self.trouble.news = true;
    }

    /// The user confirmed that a disappeared heartbeat is gone: its alert
    /// ends without a notification (the app takes its own away).
    pub(super) fn drop_gone_alert(&mut self, object: &str) {
        let key = format!("gone:{object}");
        self.trouble.pending.remove(&key);
        if self.trouble.raised.remove(&key).is_some() {
            tracing::info!(alert = %key, "trouble alert removed by the user");
            self.publish_trouble();
        }
    }

    /// Sends a trouble notification: always, unless notifications are
    /// paused (then it is recorded, silent).
    fn trouble_notify(&mut self, id: String, title: String, body: String, tone: Tone) {
        let now = self.ports.clock.now();
        let paused = self.notify.paused(now);
        let intent = NotificationIntent {
            id,
            object: None,
            title,
            subtitle: self.spec.environment.name.clone(),
            body,
            tone,
            sound: matches!(tone, Tone::Critical | Tone::Warning),
            silent: paused,
            silenced: paused.then_some(Silence::Paused),
            at: now,
        };
        self.deliver(vec![intent]);
    }

    /// The snapshots' trouble, rebuilt when it changed.
    fn publish_trouble(&mut self) {
        let mut alerts: Vec<Alert> = self
            .trouble
            .raised
            .values()
            .map(|raised| raised.alert.clone())
            .collect();
        alerts.sort_by(|a, b| {
            b.tone
                .cmp(&a.tone)
                .then(a.since.as_unix_seconds().total_cmp(&b.since.as_unix_seconds()))
                .then(a.key.cmp(&b.key))
        });
        let trouble = Trouble {
            alerts,
            blind: self.trouble.blind.as_ref().map(|(_, blind)| blind.clone()),
        };
        if trouble != *self.trouble.published {
            self.trouble.published = Arc::new(trouble);
            self.trouble.news = true;
        }
    }

    /// Another server: nothing raised is about it.
    pub(super) fn reset_trouble(&mut self) {
        self.trouble.pending.clear();
        self.trouble.raised.clear();
        self.publish_trouble();
    }
}
