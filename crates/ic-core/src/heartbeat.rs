//! Heartbeats (PLAN.md §4.2 B, B2, B3; mock-ups 16a, 16a2, 16b): always-OK
//! checks in Icinga whose results prove that a zone, or one endpoint of an
//! HA zone, runs its checks and that the results reach icygui. One per
//! zone that runs checks (a `dummy` service whose host lives in that
//! zone), and one per endpoint of any zone with more than one (pinned with
//! `command_endpoint`): Icinga balances checks between the endpoints of a
//! zone, so a zone's beat keeps beating while one of them is stuck.
//!
//! - **Found, not polled:** in the settings' *find by custom variable*
//!   mode every service whose variable (`vars.icygui_heartbeat` by default)
//!   is set is a heartbeat; in *list* mode the services listed by name.
//!   Both are looked up in the objects the engine already holds (lean
//!   services carry `vars`, `zone`, `command_endpoint` and
//!   `check_interval`), so finding them costs Icinga nothing.
//! - **Watched on the event stream:** a live stream carries every check
//!   result; a quiet one subscribes to the heartbeats' results through the
//!   stream's filter (which needs Icinga's `filter-expression`
//!   permission; without it their `last_check` is read once per interval,
//!   one small request for all of them).
//! - **The time budget (B2):** each beat's result carries Icinga's own
//!   times. The scheduling latency (`execution_start - schedule_start`)
//!   and the delivery delay (arrival - `execution_end`, against the
//!   smallest delay of the window, so the clock offset between icygui and
//!   Icinga cancels out) of the last [`BUDGET_WINDOW`] beats give the
//!   allowance `max(5 s, 3 × p99)`, at most half the interval. The next
//!   beat is due by the last one's arrival + the interval + the allowance:
//!   after that it is *late*, and [`QUERY_AFTER`] later one REST query of
//!   the object decides. Timing alone never raises an alert.
//! - **The REST query decides:** a `last_check` newer than the last beat
//!   that arrived means the check ran but the stream lost it (after a
//!   [`RECHECK_AFTER`] second look, the engine reconnects; only a
//!   reconnect that doesn't bring it back makes it *dead*: not delivered);
//!   an old one means Icinga stopped running it (*dead*: stopped); a
//!   failing query is a broken connection (the engine reconnects). A beat
//!   that isn't OK is dead at once, with Icinga's output as the reason
//!   (a check pinned to an endpoint that isn't connected gets `Remote
//!   Icinga instance 'master-02' is not connected …`).
//! - **Grace:** after a reconnect, the computer waking up or an Icinga
//!   restart, a beat gets [`GRACE_INTERVALS`] intervals before it counts as
//!   late (Icinga reschedules checks when it starts).
//! - **Remembered:** every heartbeat seen is remembered (in the event
//!   log); one that discovery no longer finds is *disappeared*, a finding,
//!   until the user confirms its removal in the settings.
//!
//! The heartbeat objects are left out of lists, counts, rules and
//! notifications ([`crate::snapshot::Snapshot::excluded`]).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use ic_model::{CheckResult, Service, ServiceKey, Timestamp};

/// The most beats whose timings make up the allowance.
pub const BUDGET_WINDOW: usize = 50;

/// The smallest allowance: a beat is late only once it is this much behind
/// its interval at least.
pub const MIN_ALLOWANCE: Duration = Duration::from_secs(5);

/// The allowance is this many times the p99 of the window's latencies and
/// delays.
pub const ALLOWANCE_FACTOR: f64 = 3.0;

/// A late beat's object is asked for this long after its deadline.
pub const QUERY_AFTER: Duration = Duration::from_secs(10);

/// A fresh `last_check` without the beat on the stream: one more look this
/// much later before reconnecting (the event may just have been on its
/// way).
pub const RECHECK_AFTER: Duration = Duration::from_secs(5);

/// After a reconnect, the computer waking up or an Icinga restart, a beat
/// counts as late only this many intervals later.
pub const GRACE_INTERVALS: u32 = 2;

/// The time budget's knobs ([`crate::Tuning::heartbeat`]): the constants
/// above, shorter in tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// A beat whose `check_interval` is shorter is watched at this
    /// interval (10 s).
    pub min_interval: Duration,
    /// The smallest allowance ([`MIN_ALLOWANCE`]).
    pub min_allowance: Duration,
    /// A late beat's object is asked for this long after its deadline
    /// ([`QUERY_AFTER`]).
    pub query_after: Duration,
    /// The second look before a reconnect ([`RECHECK_AFTER`]).
    pub recheck_after: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            min_interval: Duration::from_secs(u64::from(ic_config::MIN_HEARTBEAT_INTERVAL_SECS)),
            min_allowance: MIN_ALLOWANCE,
            query_after: QUERY_AFTER,
            recheck_after: RECHECK_AFTER,
        }
    }
}

/// The heartbeats of one environment, as the engine watches them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Heartbeats {
    /// What the settings ask for.
    pub setup: HeartbeatSetup,
    /// Every heartbeat found, listed or remembered, in the cluster's order
    /// (the top-level zone's endpoints, then each child zone: its own beat,
    /// then its endpoints').
    pub beats: Vec<Heartbeat>,
    /// The quiet stream can't carry them (the API user may not filter the
    /// event stream): while quiet, their last check is read once per
    /// interval instead.
    pub polled: bool,
}

impl Heartbeats {
    /// The beats icygui watches: found or listed, not disappeared or
    /// unknown.
    pub fn watched(&self) -> impl Iterator<Item = &Heartbeat> {
        self.beats.iter().filter(|beat| beat.state.is_watched())
    }

    /// The beat that proves `endpoint` (pinned to it).
    #[must_use]
    pub fn of_endpoint(&self, endpoint: &str) -> Option<&Heartbeat> {
        self.beats.iter().find(|beat| {
            matches!(&beat.proves, Proves::Endpoint { endpoint: name, .. } if name == endpoint)
        })
    }

    /// The beat that proves `zone` (not pinned to an endpoint).
    #[must_use]
    pub fn of_zone(&self, zone: &str) -> Option<&Heartbeat> {
        self.beats
            .iter()
            .find(|beat| matches!(&beat.proves, Proves::Zone(name) if name == zone))
    }
}

/// What the settings ask for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HeartbeatSetup {
    /// No variable to find heartbeats by, or an empty list.
    #[default]
    Off,
    /// Every service with this custom variable.
    Find {
        /// The variable's name, without `vars.`.
        variable: String,
    },
    /// The services listed by name.
    List,
}

/// One heartbeat.
#[derive(Clone, Debug, PartialEq)]
pub struct Heartbeat {
    /// The service.
    pub key: ServiceKey,
    /// What it proves.
    pub proves: Proves,
    /// How often it runs: its `check_interval`, or the settings' override.
    pub interval: Duration,
    /// Where it stands.
    pub state: BeatState,
    /// When the last OK beat arrived (local clock): the age the page shows
    /// is worked out from it against the UI's clock.
    pub last_beat: Option<Timestamp>,
    /// When Icinga ran it last (Icinga's clock), any state: `last 02:11`.
    pub last_check: Option<Timestamp>,
    /// When [`Heartbeat::state`] began (local clock).
    pub since: Timestamp,
    /// The next beat is due by then (local clock): late after it.
    pub deadline: Option<Timestamp>,
    /// The time budget's allowance on top of the interval.
    pub allowance: Duration,
    /// Why it is dead: Icinga's output for a beat that isn't OK, or what
    /// the REST query found.
    pub reason: Option<String>,
}

impl Heartbeat {
    /// The `host!service` the settings show.
    #[must_use]
    pub fn object(&self) -> String {
        format!("{}!{}", self.key.host, self.key.name)
    }
}

/// What a heartbeat proves: that a zone runs its checks, or one endpoint
/// (pinned with `command_endpoint`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Proves {
    /// The zone runs checks (whichever of its endpoints runs this one).
    Zone(String),
    /// This endpoint runs checks.
    Endpoint {
        /// The endpoint.
        endpoint: String,
        /// Its zone, as far as the object says.
        zone: Option<String>,
    },
}

impl Proves {
    /// What the settings' *proves* column says: `zone ams`, `master-01`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Zone(zone) => format!("zone {zone}"),
            Self::Endpoint { endpoint, .. } => endpoint.clone(),
        }
    }

    /// The word the summary row and the alerts name it by: `ams`,
    /// `master-01`.
    #[must_use]
    pub fn subject(&self) -> &str {
        match self {
            Self::Zone(zone) => zone,
            Self::Endpoint { endpoint, .. } => endpoint,
        }
    }

    /// The zone it belongs to, if known.
    #[must_use]
    pub fn zone(&self) -> Option<&str> {
        match self {
            Self::Zone(zone) => Some(zone),
            Self::Endpoint { zone, .. } => zone.as_deref(),
        }
    }
}

impl Proves {
    /// How the event log remembers it (`zone\tfra`,
    /// `endpoint\tmaster-01\tmaster`).
    pub(crate) fn encode(&self) -> String {
        match self {
            Self::Zone(zone) => format!("zone\t{zone}"),
            Self::Endpoint { endpoint, zone } => {
                format!(
                    "endpoint\t{endpoint}\t{}",
                    zone.as_deref().unwrap_or_default()
                )
            }
        }
    }

    /// The other way round ([`Proves::encode`]).
    pub(crate) fn decode(text: &str) -> Option<Self> {
        let mut parts = text.split('\t');
        match parts.next()? {
            "zone" => Some(Self::Zone(parts.next()?.to_owned())),
            "endpoint" => {
                let endpoint = parts.next()?.to_owned();
                let zone = parts
                    .next()
                    .filter(|zone| !zone.is_empty())
                    .map(str::to_owned);
                Some(Self::Endpoint { endpoint, zone })
            }
            _ => None,
        }
    }
}

/// Where a heartbeat stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeatState {
    /// Found, the first beat not due yet (or in its grace after a
    /// reconnect).
    Waiting,
    /// The last beat came in time.
    OnTime,
    /// Past its deadline: the REST query follows.
    Late,
    /// Past its deadline: the REST query is on its way, or the second look
    /// before a reconnect.
    Checking,
    /// Dead: see [`Death`] and [`Heartbeat::reason`].
    Dead(Death),
    /// Discovery no longer finds it (remembered from before): a finding
    /// until its removal is confirmed.
    Disappeared,
    /// Listed in the settings, but Icinga has no such service.
    NotFound,
}

impl BeatState {
    /// Whether icygui watches it (found or listed and known).
    #[must_use]
    pub fn is_watched(self) -> bool {
        !matches!(self, Self::Disappeared | Self::NotFound)
    }

    /// Whether it is dead.
    #[must_use]
    pub fn is_dead(self) -> bool {
        matches!(self, Self::Dead(_))
    }
}

/// Why a heartbeat is dead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Death {
    /// Its result isn't OK (Icinga's output says why).
    NotOk,
    /// Icinga ran no check since the last beat.
    Stopped,
    /// Icinga ran it, but the event stream didn't bring it, even after a
    /// reconnect.
    NotDelivered,
}

/// A heartbeat found in the objects the engine holds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Found {
    pub(crate) key: ServiceKey,
    pub(crate) proves: Proves,
    pub(crate) interval: Duration,
}

/// The services that are heartbeats under `settings`, and (in list mode)
/// the listed ones Icinga doesn't have. `local_zone` stands in for a
/// service without a zone (one in `conf.d`, which belongs to the node's own
/// zone).
pub(crate) fn discover(
    settings: &ic_config::HeartbeatSettings,
    services: &BTreeMap<ServiceKey, Arc<Service>>,
    local_zone: Option<&str>,
    floor: Duration,
) -> (Vec<Found>, Vec<ServiceKey>) {
    let override_interval = settings.interval_secs.map(|secs| {
        Duration::from_secs(u64::from(secs.max(ic_config::MIN_HEARTBEAT_INTERVAL_SECS)))
    });
    let found = |service: &Service| Found {
        key: service.key.clone(),
        proves: proves_of(service, local_zone),
        interval: override_interval.unwrap_or_else(|| interval_of(service, floor)),
    };
    match settings.mode {
        ic_config::HeartbeatMode::Find => {
            let variable = settings.variable_name();
            if variable.is_empty() {
                return (Vec::new(), Vec::new());
            }
            let beats = services
                .values()
                .filter(|service| service.vars.get(variable).is_some_and(truthy))
                .map(|service| found(service))
                .collect();
            (beats, Vec::new())
        }
        ic_config::HeartbeatMode::List => {
            let mut beats = Vec::new();
            let mut missing = Vec::new();
            for key in settings.listed() {
                match services.get(&key) {
                    Some(service) => beats.push(found(service)),
                    None => missing.push(key),
                }
            }
            (beats, missing)
        }
    }
}

/// Whether a custom variable's value marks a heartbeat: set and not
/// `false`, `0`, `"false"`, `"0"` or empty.
fn truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(set) => *set,
        serde_json::Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        serde_json::Value::String(text) => {
            let text = text.trim();
            !text.is_empty() && text != "0" && !text.eq_ignore_ascii_case("false")
        }
        serde_json::Value::Array(items) => !items.is_empty(),
        serde_json::Value::Object(map) => !map.is_empty(),
    }
}

/// What a heartbeat service proves: its `command_endpoint`, else its zone.
fn proves_of(service: &Service, local_zone: Option<&str>) -> Proves {
    let zone = service
        .check
        .zone
        .clone()
        .filter(|zone| !zone.trim().is_empty())
        .or_else(|| local_zone.map(str::to_owned));
    match service
        .check
        .command_endpoint
        .clone()
        .filter(|endpoint| !endpoint.trim().is_empty())
    {
        Some(endpoint) => Proves::Endpoint { endpoint, zone },
        None => Proves::Zone(zone.unwrap_or_default()),
    }
}

/// A service's check interval, at least `floor`.
fn interval_of(service: &Service, floor: Duration) -> Duration {
    let seconds = service.check.check_interval;
    if seconds.is_finite() && seconds >= floor.as_secs_f64() {
        Duration::from_secs_f64(seconds)
    } else {
        floor
    }
}

/// The beats in the cluster's order: zones as `zone_rank` ranks them (the
/// top-level zone first), within a zone its own beat before its
/// endpoints', endpoints by name.
pub(crate) fn order(beats: &mut [Heartbeat], zone_rank: impl Fn(Option<&str>) -> usize) {
    beats.sort_by(|a, b| {
        let key = |beat: &Heartbeat| {
            let pinned = matches!(beat.proves, Proves::Endpoint { .. });
            (
                zone_rank(beat.proves.zone()),
                beat.proves.zone().map(str::to_owned),
                pinned,
                beat.proves.subject().to_owned(),
                beat.key.clone(),
            )
        };
        key(a).cmp(&key(b))
    });
}

/// One beat's timings: scheduling latency and delivery delay, seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    latency: f64,
    delivery: f64,
}

/// A heartbeat's time budget (B2): the timings of its last
/// [`BUDGET_WINDOW`] beats.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Budget {
    window: VecDeque<Sample>,
}

impl Budget {
    /// Adds the beat `result` that arrived at `arrival` (local clock).
    pub(crate) fn record(&mut self, result: &CheckResult, arrival: Timestamp) {
        let schedule = result.schedule_start.as_unix_seconds();
        let start = result.execution_start.as_unix_seconds();
        let end = result.execution_end.as_unix_seconds();
        if schedule <= 0.0 || end <= 0.0 {
            return;
        }
        let latency = (start - schedule).max(0.0);
        let delivery = arrival.as_unix_seconds() - end;
        if !latency.is_finite() || !delivery.is_finite() {
            return;
        }
        self.window.push_back(Sample { latency, delivery });
        while self.window.len() > BUDGET_WINDOW {
            self.window.pop_front();
        }
    }

    /// How many beats it holds.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.window.len()
    }

    /// The allowance for a beat of `interval`: `max(5 s, 3 × p99)` of
    /// latency plus delivery (against the window's smallest delivery, so
    /// the clocks' offset cancels out), at most half the interval.
    pub(crate) fn allowance(&self, interval: Duration, min: Duration) -> Duration {
        let cap = interval / 2;
        let baseline = self
            .window
            .iter()
            .map(|timing| timing.delivery)
            .fold(f64::INFINITY, f64::min);
        let mut spans: Vec<f64> = self
            .window
            .iter()
            .map(|timing| timing.latency + (timing.delivery - baseline).max(0.0))
            .collect();
        let measured = if spans.is_empty() {
            0.0
        } else {
            spans.sort_by(f64::total_cmp);
            // The 99th percentile (of 50 beats: the largest but none).
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "an index into a short window"
            )]
            let index = (((spans.len() - 1) as f64) * 0.99).round() as usize;
            spans[index.min(spans.len() - 1)]
        };
        let allowance =
            Duration::from_secs_f64((measured * ALLOWANCE_FACTOR).clamp(0.0, 3_600.0)).max(min);
        allowance.min(cap.max(Duration::from_millis(100)))
    }
}

/// What one REST query of a late beat found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The check ran after the last beat that arrived: the stream lost it.
    Fresh,
    /// Its result isn't OK: dead, with Icinga's output.
    NotOk(String),
    /// No check since the last beat: Icinga stopped running it.
    Stopped,
}

/// Judges a late beat from its object as a REST query found it:
/// `last_end` is the `execution_end` of the last beat the stream brought
/// (Icinga's clock), if any; `icinga_now` Icinga's clock now, if known.
pub(crate) fn judge(
    service: &Service,
    last_end: Option<Timestamp>,
    interval: Duration,
    allowance: Duration,
    icinga_now: Option<f64>,
    query_after: Duration,
) -> Verdict {
    let ok = matches!(service.state, ic_model::ServiceState::Ok);
    let output = service
        .check
        .result
        .as_ref()
        .map(|result| result.output.clone())
        .unwrap_or_default();
    let Some(last_check) = service.check.last_check else {
        return Verdict::Stopped;
    };
    let fresh = match last_end {
        // Newer than the last beat that arrived (half a second of
        // rounding between the result's end and `last_check`).
        Some(end) => last_check.as_unix_seconds() > end.as_unix_seconds() + 0.5,
        // No beat arrived in this session: fresh if it ran within the
        // interval and its allowance.
        None => icinga_now.is_none_or(|now| {
            now - last_check.as_unix_seconds() <= (interval + allowance + query_after).as_secs_f64()
        }),
    };
    if !ok {
        // Any state but OK is dead, with what Icinga says (a newer result
        // or the last one).
        return Verdict::NotOk(output);
    }
    if fresh {
        Verdict::Fresh
    } else {
        Verdict::Stopped
    }
}

/// The quiet stream's filter for `beats`: every event but check results,
/// and the check results of these objects (Icinga's stream filter sees
/// host and service names, not custom variables). `None` without beats.
#[must_use]
pub(crate) fn stream_filter<'a>(beats: impl IntoIterator<Item = &'a ServiceKey>) -> Option<String> {
    let objects: Vec<String> = beats
        .into_iter()
        .map(|key| {
            format!(
                "event.host == {} && event.service == {}",
                quote(key.host.as_str()),
                quote(&key.name)
            )
        })
        .collect();
    if objects.is_empty() {
        return None;
    }
    Some(format!(
        "event.type != \"CheckResult\" || {}",
        objects
            .iter()
            .map(|object| format!("({object})"))
            .collect::<Vec<_>>()
            .join(" || ")
    ))
}

/// An Icinga DSL string literal.
fn quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests;
