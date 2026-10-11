//! Monitored objects: hosts, services, comments, downtimes and groups.

use serde::{Deserialize, Serialize};

use crate::name::{HostName, ObjectKey, ServiceKey};
use crate::perfdata::Perfdata;
use crate::state::{HostState, ServiceState, StateType};
use crate::time::Timestamp;

/// Custom variables (`vars`), kept as JSON so filters can address any
/// nested key (`host.vars.disks["/var"].warn`).
pub type Vars = serde_json::Map<String, serde_json::Value>;

/// Acknowledgement state of a problem (`acknowledgement`: 0, 1, 2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AckKind {
    /// Not acknowledged.
    #[default]
    None,
    /// Acknowledged until the next state change.
    Normal,
    /// Acknowledged until the object recovers.
    Sticky,
}

impl AckKind {
    /// Maps Icinga's numeric `acknowledgement`.
    #[must_use]
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Normal,
            2 => Self::Sticky,
            _ => Self::None,
        }
    }

    /// Whether the problem is acknowledged.
    #[must_use]
    pub fn is_acknowledged(self) -> bool {
        self != Self::None
    }
}

/// Icinga's per-object feature switches. Read-only here: the client shows
/// them but never changes them (configuration is managed elsewhere).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors Icinga's independent enable_* flags one to one"
)]
pub struct Features {
    /// `enable_active_checks`.
    pub active_checks: bool,
    /// `enable_passive_checks`.
    pub passive_checks: bool,
    /// `enable_notifications`.
    pub notifications: bool,
    /// `enable_event_handler`.
    pub event_handler: bool,
    /// `enable_flapping`.
    pub flap_detection: bool,
    /// `enable_perfdata`.
    pub perfdata: bool,
}

impl Default for Features {
    fn default() -> Self {
        Self {
            active_checks: true,
            passive_checks: true,
            notifications: true,
            event_handler: true,
            flap_detection: false,
            perfdata: true,
        }
    }
}

/// The latest check result (`last_check_result`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    /// First line of the plugin output.
    pub output: String,
    /// Remaining lines of the plugin output (may be empty).
    pub long_output: String,
    /// Parsed performance data.
    pub perfdata: Vec<Perfdata>,
    /// Plugin exit status.
    pub exit_status: i32,
    /// When the check was scheduled to run.
    pub schedule_start: Timestamp,
    /// When the check started executing.
    pub execution_start: Timestamp,
    /// When the check finished executing.
    pub execution_end: Timestamp,
    /// The endpoint that executed the check.
    pub check_source: String,
    /// Whether the result came from an active check.
    pub active: bool,
}

impl CheckResult {
    /// Splits raw plugin output into the first line and the rest.
    #[must_use]
    pub fn split_output(raw: &str) -> (String, String) {
        let raw = raw.trim_end();
        match raw.split_once('\n') {
            Some((first, rest)) => (first.trim_end().to_owned(), rest.to_owned()),
            None => (raw.to_owned(), String::new()),
        }
    }

    /// Seconds the check took to run.
    #[must_use]
    pub fn execution_time(&self) -> f64 {
        (self.execution_end.as_unix_seconds() - self.execution_start.as_unix_seconds()).max(0.0)
    }

    /// Seconds between scheduling and execution start.
    #[must_use]
    pub fn latency(&self) -> f64 {
        (self.execution_start.as_unix_seconds() - self.schedule_start.as_unix_seconds()).max(0.0)
    }
}

/// State and check details shared by hosts and services.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckInfo {
    /// Soft or hard.
    pub state_type: StateType,
    /// When the state last changed (soft or hard).
    pub last_state_change: Timestamp,
    /// When the hard state last changed.
    pub last_hard_state_change: Timestamp,
    /// When the last check ran; `None` if never checked.
    pub last_check: Option<Timestamp>,
    /// When the next check is scheduled.
    pub next_check: Option<Timestamp>,
    /// Current attempt (`check_attempt`).
    pub attempt: u32,
    /// `max_check_attempts`.
    pub max_attempts: u32,
    /// The latest check result; `None` while pending.
    pub result: Option<CheckResult>,
    /// Acknowledgement state.
    pub acknowledgement: AckKind,
    /// When the acknowledgement expires, if it does.
    pub acknowledgement_expiry: Option<Timestamp>,
    /// Number of active downtimes (`downtime_depth`).
    pub downtime_depth: u32,
    /// Whether the object is flapping.
    pub flapping: bool,
    /// Current flapping value in percent.
    pub flapping_current: f64,
    /// Whether all dependencies allowed the last check (`last_reachable`).
    pub reachable: bool,
    /// The check command name.
    pub check_command: String,
    /// Check interval in seconds.
    pub check_interval: f64,
    /// Retry interval in seconds.
    pub retry_interval: f64,
    /// `command_endpoint`, if the check runs remotely.
    pub command_endpoint: Option<String>,
    /// The zone the object belongs to.
    pub zone: Option<String>,
    /// Feature switches (read-only).
    pub features: Features,
}

impl Default for CheckInfo {
    fn default() -> Self {
        Self {
            state_type: StateType::Hard,
            last_state_change: Timestamp::EPOCH,
            last_hard_state_change: Timestamp::EPOCH,
            last_check: None,
            next_check: None,
            attempt: 1,
            max_attempts: 1,
            result: None,
            acknowledgement: AckKind::None,
            acknowledgement_expiry: None,
            downtime_depth: 0,
            flapping: false,
            flapping_current: 0.0,
            reachable: true,
            check_command: String::new(),
            check_interval: 300.0,
            retry_interval: 60.0,
            command_endpoint: None,
            zone: None,
            features: Features::default(),
        }
    }
}

impl CheckInfo {
    /// Whether the object is acknowledged or in downtime.
    #[must_use]
    pub fn is_acknowledged_or_in_downtime(&self) -> bool {
        self.acknowledgement.is_acknowledged() || self.in_downtime()
    }

    /// Whether a downtime is in effect for the object (`downtime_depth`).
    /// A downtime that hasn't started (scheduled for later, or flexible
    /// and not triggered) doesn't count.
    #[must_use]
    pub fn in_downtime(&self) -> bool {
        self.downtime_depth > 0
    }

    /// First line of the plugin output, or empty while pending.
    #[must_use]
    pub fn output(&self) -> &str {
        self.result.as_ref().map_or("", |result| &result.output)
    }
}

/// Free-form presentation attributes from the object config.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Links {
    /// `notes`.
    pub notes: String,
    /// `notes_url`.
    pub notes_url: String,
    /// `action_url`.
    pub action_url: String,
    /// `icon_image`.
    pub icon_image: String,
}

/// A monitored host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Host {
    /// Unique name.
    pub name: HostName,
    /// `display_name` (falls back to the name in Icinga).
    pub display_name: String,
    /// IPv4 address or FQDN.
    pub address: String,
    /// IPv6 address.
    pub address6: String,
    /// Current state.
    pub state: HostState,
    /// Check details.
    pub check: CheckInfo,
    /// Host group names.
    pub groups: Vec<String>,
    /// Custom variables.
    pub vars: Vars,
    /// Notes and links.
    pub links: Links,
}

impl Host {
    /// A pending host with defaults, for tests and scenario data.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: HostName::new(name),
            display_name: name.to_owned(),
            address: String::new(),
            address6: String::new(),
            state: HostState::Pending,
            check: CheckInfo::default(),
            groups: Vec::new(),
            vars: Vars::new(),
            links: Links::default(),
        }
    }

    /// The object key.
    #[must_use]
    pub fn key(&self) -> ObjectKey {
        ObjectKey::Host {
            name: self.name.clone(),
        }
    }

    /// Whether this is a problem: down or unreachable.
    #[must_use]
    pub fn is_problem(&self) -> bool {
        self.state.is_problem()
    }

    /// Icinga 2's `handled`: a problem that is acknowledged or in downtime.
    #[must_use]
    pub fn is_handled(&self) -> bool {
        self.is_problem() && self.check.is_acknowledged_or_in_downtime()
    }

    /// Whether icygui counts the host as handled: drawn as a hollow mark,
    /// left out by *hide handled*. Icinga's `handled`, and a host whose
    /// downtime is in effect whatever its state (an UP host in downtime
    /// is a hollow green ring). Filters keep Icinga's `handled`
    /// ([`Host::is_handled`]).
    #[must_use]
    pub fn counts_as_handled(&self) -> bool {
        self.is_handled() || self.check.in_downtime()
    }

    /// Icinga 2's severity (see [`crate::severity`]).
    #[must_use]
    pub fn severity(&self) -> u32 {
        crate::severity::host_severity(self)
    }
}

/// A monitored service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Service {
    /// Host and short name.
    pub key: ServiceKey,
    /// `display_name`.
    pub display_name: String,
    /// Current state.
    pub state: ServiceState,
    /// Check details.
    pub check: CheckInfo,
    /// Service group names.
    pub groups: Vec<String>,
    /// Custom variables.
    pub vars: Vars,
    /// Notes and links.
    pub links: Links,
}

impl Service {
    /// A pending service with defaults, for tests and scenario data.
    #[must_use]
    pub fn new(host: &str, name: &str) -> Self {
        Self {
            key: ServiceKey::new(host, name),
            display_name: name.to_owned(),
            state: ServiceState::Pending,
            check: CheckInfo::default(),
            groups: Vec::new(),
            vars: Vars::new(),
            links: Links::default(),
        }
    }

    /// The object key.
    #[must_use]
    pub fn object_key(&self) -> ObjectKey {
        ObjectKey::Service {
            key: self.key.clone(),
        }
    }

    /// Whether this is a problem: warning, critical or unknown.
    #[must_use]
    pub fn is_problem(&self) -> bool {
        self.state.is_problem()
    }

    /// Icinga 2's `handled`: a problem that is acknowledged, in downtime, or
    /// whose host has a problem. `host_problem` is the host's `is_problem()`.
    #[must_use]
    pub fn is_handled(&self, host_problem: bool) -> bool {
        self.is_problem() && (self.check.is_acknowledged_or_in_downtime() || host_problem)
    }

    /// Whether icygui counts the service as handled: drawn as a hollow
    /// mark, left out by *hide handled*. Icinga's `handled`, and a service
    /// whose downtime is in effect whatever its state (an OK service in
    /// downtime is a hollow green ring). A downtime of its host that
    /// doesn't cover the service (scheduled without `all_services`) leaves
    /// it alone, as in Icinga Web. Filters keep Icinga's `handled`
    /// ([`Service::is_handled`]).
    #[must_use]
    pub fn counts_as_handled(&self, host_problem: bool) -> bool {
        self.is_handled(host_problem) || self.check.in_downtime()
    }

    /// Icinga 2's severity (see [`crate::severity`]).
    #[must_use]
    pub fn severity(&self) -> u32 {
        crate::severity::service_severity(self)
    }
}

/// Why a comment exists (`entry_type`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommentKind {
    /// Added by a user.
    #[default]
    User,
    /// Added for a downtime.
    Downtime,
    /// Added for flapping.
    Flapping,
    /// Added with an acknowledgement.
    Acknowledgement,
}

impl CommentKind {
    /// Maps Icinga's numeric `entry_type` (1–4).
    #[must_use]
    pub fn from_code(code: u8) -> Self {
        match code {
            2 => Self::Downtime,
            3 => Self::Flapping,
            4 => Self::Acknowledgement,
            _ => Self::User,
        }
    }
}

/// A comment on a host or service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    /// Full object name (`host!service!uuid` or `host!uuid`), used to remove it.
    pub name: String,
    /// The host or service it belongs to.
    pub object: ObjectKey,
    /// Author.
    pub author: String,
    /// Comment text.
    pub text: String,
    /// Why it exists.
    pub kind: CommentKind,
    /// When it was added.
    pub entry_time: Timestamp,
    /// When it expires, if it does.
    pub expire_time: Option<Timestamp>,
    /// Whether it survives the end of an acknowledgement.
    pub persistent: bool,
}

/// A scheduled downtime on a host or service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Downtime {
    /// Full object name, used to remove it.
    pub name: String,
    /// The host or service it belongs to.
    pub object: ObjectKey,
    /// Author.
    pub author: String,
    /// Comment text.
    pub comment: String,
    /// Window start.
    pub start_time: Timestamp,
    /// Window end.
    pub end_time: Timestamp,
    /// Fixed (whole window) or flexible (`duration` once triggered).
    pub fixed: bool,
    /// Length of a flexible downtime, in seconds.
    pub duration: f64,
    /// When it was added.
    pub entry_time: Timestamp,
    /// When a flexible downtime was triggered; `None` if not yet.
    pub trigger_time: Option<Timestamp>,
    /// The downtime that triggers this one, if any.
    pub triggered_by: Option<String>,
    /// The parent downtime for child downtimes (`all_services`, child hosts).
    pub parent: Option<String>,
    /// Whether it's in effect right now.
    pub in_effect: bool,
    /// Whether it was created by a `ScheduledDowntime` config object
    /// (Icinga refuses to remove those; they come back with the config).
    pub config_owned: bool,
    /// The short name of the `ScheduledDowntime` that created it
    /// (`weekly-patching`), when Icinga says.
    #[serde(default)]
    pub schedule: Option<String>,
}

/// Where a downtime stands right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DowntimePhase {
    /// In effect: the object is handled.
    InEffect,
    /// Flexible, its window open, waiting for a problem to start it.
    Waiting,
    /// Its window hasn't begun (or it is about to take effect).
    Upcoming,
    /// Over: its window passed without it taking effect, or it ran out.
    Over,
}

impl Downtime {
    /// Where the downtime stands at `now`. Icinga's `in_effect` decides
    /// while it's in effect (Icinga reports when it starts and ends);
    /// otherwise its window does.
    #[must_use]
    pub fn phase(&self, now: Timestamp) -> DowntimePhase {
        if self.in_effect {
            DowntimePhase::InEffect
        } else if now < self.start_time {
            DowntimePhase::Upcoming
        } else if now >= self.end_time {
            DowntimePhase::Over
        } else if self.fixed {
            // Its window began; Icinga's report that it started is on
            // its way.
            DowntimePhase::Upcoming
        } else if self.trigger_time.is_none() {
            DowntimePhase::Waiting
        } else {
            DowntimePhase::Over
        }
    }

    /// When it takes effect: the window's start for a fixed downtime, the
    /// trigger for a flexible one (`None` while it waits).
    #[must_use]
    pub fn effective_start(&self) -> Option<Timestamp> {
        if self.fixed {
            Some(self.start_time)
        } else {
            self.trigger_time
        }
    }

    /// When it stops being in effect: the window's end for a fixed
    /// downtime, `duration` after the trigger for a flexible one (Icinga's
    /// `Downtime::IsInEffect`); `None` while a flexible one waits.
    #[must_use]
    pub fn effective_end(&self) -> Option<Timestamp> {
        if self.fixed {
            Some(self.end_time)
        } else {
            self.trigger_time.map(|trigger| {
                Timestamp::from_unix_seconds(trigger.as_unix_seconds() + self.duration.max(0.0))
            })
        }
    }

    /// How much of it has passed at `now`, 0 to 1: of the window for a
    /// fixed downtime, of `duration` since the trigger for a flexible one;
    /// 0 until it takes effect.
    #[must_use]
    pub fn progress(&self, now: Timestamp) -> f32 {
        let (Some(start), Some(end)) = (self.effective_start(), self.effective_end()) else {
            return 0.0;
        };
        let length = end.as_unix_seconds() - start.as_unix_seconds();
        if length <= 0.0 {
            return 1.0;
        }
        let done = ((now.as_unix_seconds() - start.as_unix_seconds()) / length).clamp(0.0, 1.0);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a fraction between 0 and 1 for a progress bar"
        )]
        let done = done as f32;
        done
    }
}

/// A host group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostGroup {
    /// Unique name.
    pub name: String,
    /// Display name.
    pub display_name: String,
}

/// A service group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceGroup {
    /// Unique name.
    pub name: String,
    /// Display name.
    pub display_name: String,
}

/// A dependency between checkables (parent → child), from `Dependency` objects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    /// Full object name.
    pub name: String,
    /// The object that depends on `parent`.
    pub child: ObjectKey,
    /// The object `child` depends on.
    pub parent: ObjectKey,
}

/// A cluster zone (`Zone` object, `lib/remote/zone.ti`): its member
/// endpoints and its place in the zone tree. An endpoint is a member of
/// exactly one zone (Icinga refuses a configuration where it isn't); a node
/// in a top-level zone (no parent, not global) has every object of the
/// cluster, a node in a child zone only its zone's and those below.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    /// Zone name (`master`, `ams`).
    pub name: String,
    /// The parent zone; `None` for a top-level zone.
    pub parent: Option<String>,
    /// The member endpoints' names.
    pub endpoints: Vec<String>,
    /// A global zone: configuration synced to every node; it has no
    /// endpoints.
    pub global: bool,
}

/// A cluster endpoint and whether the API sees it connected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// Endpoint name (usually the node's FQDN).
    pub name: String,
    /// The zone it belongs to.
    pub zone: String,
    /// Whether it's connected to the endpoint we talk to.
    pub connected: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_multi_line_output() {
        let (first, rest) =
            CheckResult::split_output("CRITICAL - lag 412s\nprimary db-01\nstandby db-03\n");
        assert_eq!(first, "CRITICAL - lag 412s");
        assert_eq!(rest, "primary db-01\nstandby db-03");
        assert_eq!(
            CheckResult::split_output("OK"),
            ("OK".to_owned(), String::new())
        );
    }

    #[test]
    fn handled_follows_icinga() {
        let mut service = Service::new("h", "s");
        service.state = ServiceState::Critical;
        assert!(!service.is_handled(false));
        assert!(
            service.is_handled(true),
            "a host problem handles its services"
        );
        service.check.downtime_depth = 1;
        assert!(service.is_handled(false));
        service.state = ServiceState::Ok;
        assert!(!service.is_handled(true), "only problems can be handled");

        let mut host = Host::new("h");
        host.state = HostState::Down;
        assert!(!host.is_handled());
        host.check.acknowledgement = AckKind::Sticky;
        assert!(host.is_handled());
    }

    #[test]
    fn a_downtime_in_effect_counts_as_handled_whatever_the_state() {
        let mut service = Service::new("h", "s");
        service.state = ServiceState::Ok;
        assert!(!service.counts_as_handled(false));
        service.check.downtime_depth = 1;
        assert!(
            service.counts_as_handled(false),
            "an OK service in downtime is a hollow green ring"
        );
        assert!(!service.is_handled(false), "filters keep Icinga's handled");
        service.check.downtime_depth = 0;
        service.state = ServiceState::Critical;
        service.check.acknowledgement = AckKind::Normal;
        assert!(service.counts_as_handled(false));
        service.check.acknowledgement = AckKind::None;
        assert!(service.counts_as_handled(true), "behind a host problem");
        assert!(!service.counts_as_handled(false));

        let mut host = Host::new("h");
        host.state = HostState::Up;
        assert!(!host.counts_as_handled());
        host.check.downtime_depth = 2;
        assert!(host.counts_as_handled());
        assert!(!host.is_handled());
    }

    fn downtime(fixed: bool) -> Downtime {
        Downtime {
            name: "h!d".to_owned(),
            object: ObjectKey::Host {
                name: HostName::new("h"),
            },
            author: "a".to_owned(),
            comment: "c".to_owned(),
            start_time: Timestamp::from_unix_seconds(1_000.0),
            end_time: Timestamp::from_unix_seconds(5_000.0),
            fixed,
            duration: 1_000.0,
            entry_time: Timestamp::from_unix_seconds(900.0),
            trigger_time: None,
            triggered_by: None,
            parent: None,
            in_effect: false,
            config_owned: false,
            schedule: None,
        }
    }

    #[test]
    fn downtime_phases_follow_the_window() {
        let at = Timestamp::from_unix_seconds;
        let mut fixed = downtime(true);
        assert_eq!(fixed.phase(at(500.0)), DowntimePhase::Upcoming);
        assert_eq!(fixed.phase(at(6_000.0)), DowntimePhase::Over);
        fixed.in_effect = true;
        assert_eq!(fixed.phase(at(2_000.0)), DowntimePhase::InEffect);
        assert!((fixed.progress(at(2_000.0)) - 0.25).abs() < 0.001);
        assert_eq!(fixed.effective_end(), Some(at(5_000.0)));

        let mut flexible = downtime(false);
        assert_eq!(flexible.phase(at(2_000.0)), DowntimePhase::Waiting);
        assert_eq!(flexible.effective_end(), None);
        assert!(flexible.progress(at(2_000.0)).abs() < f32::EPSILON);
        flexible.trigger_time = Some(at(2_000.0));
        flexible.in_effect = true;
        assert_eq!(flexible.phase(at(2_500.0)), DowntimePhase::InEffect);
        assert_eq!(flexible.effective_end(), Some(at(3_000.0)));
        assert!((flexible.progress(at(2_500.0)) - 0.5).abs() < 0.001);
        flexible.in_effect = false;
        assert_eq!(flexible.phase(at(3_500.0)), DowntimePhase::Over);
    }

    #[test]
    fn check_timing() {
        let result = CheckResult {
            schedule_start: Timestamp::from_unix_seconds(100.0),
            execution_start: Timestamp::from_unix_seconds(100.5),
            execution_end: Timestamp::from_unix_seconds(101.0),
            ..CheckResult::default()
        };
        assert!((result.latency() - 0.5).abs() < f64::EPSILON);
        assert!((result.execution_time() - 0.5).abs() < f64::EPSILON);
    }
}
