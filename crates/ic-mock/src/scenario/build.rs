//! Helpers to build scenarios tersely.

use std::time::Duration;

use ic_model::{
    AckKind, CheckResult, Comment, CommentKind, Dependency, Downtime, Endpoint, Features, Host,
    HostState, ObjectKey, Perfdata, Service, ServiceState, StateType, Timestamp, Vars,
    format_number, parse_perfdata_entry,
};

use super::Scenario;
use crate::outputs;
use crate::rng::Rng;

/// Builds an `ic_model::CheckResult` from raw plugin output (first line =
/// output, the rest = long output) and performance data entries as Icinga
/// stores them (`label=value[unit];warn;crit;min;max`).
#[must_use]
pub fn raw_check_result(
    output: &str,
    perfdata: &[&str],
    exit_status: i32,
    at: Timestamp,
) -> CheckResult {
    let (output, long_output) = CheckResult::split_output(output);
    CheckResult {
        output,
        long_output,
        perfdata: perfdata
            .iter()
            .filter_map(|entry| parse_perfdata_entry(entry))
            .collect(),
        exit_status,
        schedule_start: Timestamp::from_unix_seconds(at.as_unix_seconds() - 0.5),
        execution_start: Timestamp::from_unix_seconds(at.as_unix_seconds() - 0.4),
        execution_end: at,
        check_source: String::new(),
        active: true,
    }
}

/// The wire form of a parsed perfdata value (`label=412s;60;300`).
pub(crate) fn format_perfdata(p: &Perfdata) -> String {
    let label = if p.label.contains([' ', '=', '\'']) {
        format!("'{}'", p.label.replace('\'', "''"))
    } else {
        p.label.clone()
    };
    let value = p.value.map_or_else(
        || "U".to_owned(),
        |v| format!("{}{}", format_number(v), p.unit),
    );
    let mut fields = vec![
        value,
        p.warn.as_ref().map(|t| t.raw.clone()).unwrap_or_default(),
        p.crit.as_ref().map(|t| t.raw.clone()).unwrap_or_default(),
        p.min.map(format_number).unwrap_or_default(),
        p.max.map(format_number).unwrap_or_default(),
    ];
    while fields.len() > 1 && fields.last().is_some_and(String::is_empty) {
        fields.pop();
    }
    format!("{label}={}", fields.join(";"))
}

/// Vars from `(key, value)` pairs.
pub(crate) fn vars(pairs: &[(&str, serde_json::Value)]) -> Vars {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

/// A scenario under construction, with a seeded generator for realistic
/// values and a reference time ("now").
pub(crate) struct Builder {
    pub(crate) scenario: Scenario,
    pub(crate) rng: Rng,
    now: f64,
    /// Check source for objects added next (`None` = the node itself).
    pub(crate) check_source: Option<String>,
    /// Zone for objects added next.
    pub(crate) zone: Option<String>,
    /// `(check_interval, retry_interval)` for objects added next; `None`
    /// picks per check command (hosts 60 s, most services 5 minutes).
    pub(crate) intervals: Option<(f64, f64)>,
}

impl Builder {
    pub(crate) fn new(name: &str, node_name: &str, version: &str, seed: u64) -> Self {
        let mut scenario = Scenario::new(name);
        node_name.clone_into(&mut scenario.status.node_name);
        version.clone_into(&mut scenario.status.version);
        let now = scenario.time_base.as_unix_seconds();
        scenario.status.program_start =
            Timestamp::from_unix_seconds(now - 3.0 * 86_400.0 - 7_212.0);
        Self {
            scenario,
            rng: Rng::new(seed),
            now,
            check_source: None,
            zone: None,
            intervals: None,
        }
    }

    pub(crate) fn finish(self) -> Scenario {
        self.scenario
    }

    pub(crate) fn ago(&self, duration: Duration) -> Timestamp {
        Timestamp::from_unix_seconds(self.now - duration.as_secs_f64())
    }

    pub(crate) fn later(&self, duration: Duration) -> Timestamp {
        Timestamp::from_unix_seconds(self.now + duration.as_secs_f64())
    }

    /// A random age between `min` and `max` days.
    pub(crate) fn days_ago(&mut self, min: f64, max: f64) -> Timestamp {
        let days = self.rng.range(min, max);
        Timestamp::from_unix_seconds(self.now - days * 86_400.0 - self.rng.range(0.0, 3_600.0))
    }

    fn timing(&mut self, interval: f64) -> (Timestamp, Timestamp) {
        let since_last = self.rng.range(1.0, interval.max(2.0) - 0.5);
        let last = self.now - since_last;
        (
            Timestamp::from_unix_seconds(last),
            Timestamp::from_unix_seconds(last + interval),
        )
    }

    fn result(
        &self,
        output: &str,
        perfdata: &[String],
        exit_status: i32,
        at: Timestamp,
    ) -> CheckResult {
        let refs: Vec<&str> = perfdata.iter().map(String::as_str).collect();
        let mut result = raw_check_result(output, &refs, exit_status, at);
        if let Some(source) = &self.check_source {
            result.check_source.clone_from(source);
        }
        result
    }

    /// An UP host checked with `hostalive`.
    pub(crate) fn host(
        &mut self,
        name: &str,
        address: &str,
        groups: &[&str],
        vars: Vars,
    ) -> &mut Host {
        let mut host = Host::new(name);
        address.clone_into(&mut host.address);
        host.groups = groups.iter().map(|g| (*g).to_owned()).collect();
        host.vars = vars;
        host.state = HostState::Up;
        "hostalive".clone_into(&mut host.check.check_command);
        let (interval, retry) = self.intervals.unwrap_or((60.0, 30.0));
        host.check.check_interval = interval;
        host.check.retry_interval = retry;
        host.check.max_attempts = 3;
        host.check.state_type = StateType::Hard;
        host.check.zone.clone_from(&self.zone);
        let since = self.days_ago(5.0, 60.0);
        host.check.last_state_change = since;
        host.check.last_hard_state_change = since;
        let (last, next) = self.timing(interval);
        let ping = outputs::host_output(address, true, &mut self.rng);
        host.check.result = Some(self.result(&ping.output, &ping.perfdata, 0, last));
        host.check.last_check = Some(last);
        host.check.next_check = Some(next);
        self.scenario.hosts.push(host);
        let index = self.scenario.hosts.len() - 1;
        &mut self.scenario.hosts[index]
    }

    /// Sets a host down (`reachable = false`: unreachable behind a parent).
    pub(crate) fn host_down(&mut self, name: &str, output: &str, since: Duration, reachable: bool) {
        let since_ts = self.ago(since);
        let interval = self
            .scenario
            .hosts
            .iter()
            .find(|h| h.name.as_str() == name)
            .map_or(30.0, |h| h.check.check_interval);
        let (last, next) = self.timing(interval);
        let result = self.result(
            output,
            &[
                "rta=0.000000s;3.000000;5.000000;0.000000".to_owned(),
                "pl=100%;80;100;0".to_owned(),
            ],
            2,
            last,
        );
        let mut result = result;
        if let Some(host) = self
            .scenario
            .hosts
            .iter_mut()
            .find(|h| h.name.as_str() == name)
        {
            if let Some(previous) = &host.check.result
                && !previous.check_source.is_empty()
            {
                result.check_source.clone_from(&previous.check_source);
            }
            host.state = if reachable {
                HostState::Down
            } else {
                HostState::Unreachable
            };
            host.check.reachable = reachable;
            host.check.last_state_change = since_ts;
            host.check.last_hard_state_change = since_ts;
            host.check.state_type = StateType::Hard;
            host.check.attempt = 1;
            host.check.result = Some(result);
            host.check.last_check = Some(last);
            host.check.next_check = Some(next);
        }
    }

    /// An OK service with realistic output for its check command.
    pub(crate) fn service(
        &mut self,
        host: &str,
        name: &str,
        command: &str,
        groups: &[&str],
        vars: Vars,
    ) -> &mut Service {
        let (interval, retry) = self.intervals.unwrap_or_else(|| {
            let interval = match command {
                "ping4" | "http" | "load" => 60.0,
                "apt" => 3_600.0,
                _ => 300.0,
            };
            (interval, (interval / 5.0).clamp(15.0, 60.0))
        });
        let mut service = Service::new(host, name);
        name.clone_into(&mut service.display_name);
        service.groups = groups.iter().map(|g| (*g).to_owned()).collect();
        service.vars = vars;
        service.state = ServiceState::Ok;
        command.clone_into(&mut service.check.check_command);
        service.check.check_interval = interval;
        service.check.retry_interval = retry;
        service.check.max_attempts = 3;
        service.check.state_type = StateType::Hard;
        service.check.zone.clone_from(&self.zone);
        service.check.features = Features {
            flap_detection: true,
            ..Features::default()
        };
        let since = self.days_ago(2.0, 45.0);
        service.check.last_state_change = since;
        service.check.last_hard_state_change = since;
        let (last, next) = self.timing(interval);
        let ok = outputs::service_output(name, command, 0, &mut self.rng);
        service.check.result = Some(self.result(&ok.output, &ok.perfdata, 0, last));
        service.check.last_check = Some(last);
        service.check.next_check = Some(next);
        self.scenario.services.push(service);
        let index = self.scenario.services.len() - 1;
        &mut self.scenario.services[index]
    }

    fn service_mut(&mut self, host: &str, name: &str) -> Option<&mut Service> {
        self.scenario
            .services
            .iter_mut()
            .find(|s| s.key.host.as_str() == host && &*s.key.name == name)
    }

    fn service_index(&self, host: &str, name: &str) -> Option<usize> {
        self.scenario
            .services
            .iter()
            .position(|s| s.key.host.as_str() == host && &*s.key.name == name)
    }

    /// Puts a service into a hard (or soft, with `attempt`) problem state.
    #[expect(
        clippy::too_many_arguments,
        reason = "a problem is described by all of these"
    )]
    pub(crate) fn problem(
        &mut self,
        host: &str,
        name: &str,
        state: ServiceState,
        output: &str,
        perfdata: &[&str],
        since: Duration,
        soft_attempt: Option<u32>,
    ) {
        if let Some(index) = self.service_index(host, name) {
            let perfdata: Vec<String> = perfdata.iter().map(|p| (*p).to_owned()).collect();
            self.apply_problem(index, state, output, &perfdata, since, soft_attempt);
        }
    }

    /// [`Self::problem`] for the service at `index`.
    fn apply_problem(
        &mut self,
        index: usize,
        state: ServiceState,
        output: &str,
        perfdata: &[String],
        since: Duration,
        soft_attempt: Option<u32>,
    ) {
        let since_ts = self.ago(since);
        let code = match state {
            ServiceState::Ok => 0,
            ServiceState::Warning => 1,
            ServiceState::Critical => 2,
            ServiceState::Unknown | ServiceState::Pending => 3,
        };
        let Some(existing) = self.scenario.services.get(index) else {
            return;
        };
        let interval = if soft_attempt.is_some() {
            existing.check.retry_interval
        } else {
            existing.check.check_interval
        };
        let previous_source = existing
            .check
            .result
            .as_ref()
            .map(|r| r.check_source.clone())
            .unwrap_or_default();
        let (last, next) = self.timing(interval.min(since.as_secs_f64().max(2.0)));
        let mut result = self.result(output, perfdata, code, last);
        if !previous_source.is_empty() {
            result.check_source = previous_source;
        }
        let service = &mut self.scenario.services[index];
        service.state = state;
        service.check.last_state_change = since_ts;
        if let Some(attempt) = soft_attempt {
            service.check.state_type = StateType::Soft;
            service.check.attempt = attempt;
        } else {
            service.check.state_type = StateType::Hard;
            service.check.attempt = 1;
            service.check.last_hard_state_change = since_ts;
        }
        service.check.result = Some(result);
        service.check.last_check = Some(last);
        service.check.next_check = Some(next);
    }

    /// A service created in `state` with generated output (no lookups, for
    /// big scenarios).
    #[expect(clippy::too_many_arguments, reason = "creation plus state in one call")]
    pub(crate) fn service_in_state(
        &mut self,
        host: &str,
        name: &str,
        command: &str,
        groups: &[&str],
        vars: Vars,
        state: ServiceState,
        since: Duration,
    ) {
        self.service(host, name, command, groups, vars);
        if state == ServiceState::Ok {
            return;
        }
        let code = match state {
            ServiceState::Warning => 1,
            ServiceState::Critical => 2,
            _ => 3,
        };
        let generated = outputs::service_output(name, command, code, &mut self.rng);
        let index = self.scenario.services.len() - 1;
        self.apply_problem(
            index,
            state,
            &generated.output,
            &generated.perfdata,
            since,
            None,
        );
    }

    /// A problem with output generated for its check command.
    pub(crate) fn generated_problem(
        &mut self,
        host: &str,
        name: &str,
        state: ServiceState,
        since: Duration,
    ) {
        let Some(index) = self.service_index(host, name) else {
            return;
        };
        let command = self.scenario.services[index].check.check_command.clone();
        let code = match state {
            ServiceState::Warning => 1,
            ServiceState::Critical => 2,
            _ => 3,
        };
        let generated = outputs::service_output(name, &command, code, &mut self.rng);
        self.apply_problem(
            index,
            state,
            &generated.output,
            &generated.perfdata,
            since,
            None,
        );
    }

    /// A user comment.
    pub(crate) fn comment(
        &mut self,
        object: ObjectKey,
        author: &str,
        text: &str,
        ago: Duration,
        expires_in: Option<Duration>,
    ) {
        let id = self.rng.uuid();
        let name = format!("{}!{id}", object.full_name());
        let expire_time = expires_in.map(|d| self.later(d));
        let entry_time = self.ago(ago);
        self.scenario.comments.push(Comment {
            name,
            object,
            author: author.to_owned(),
            text: text.to_owned(),
            kind: CommentKind::User,
            entry_time,
            expire_time,
            persistent: false,
        });
    }

    /// Acknowledges the service added last (no lookup, for big scenarios).
    pub(crate) fn acknowledge_last_service(
        &mut self,
        author: &str,
        text: &str,
        ago: Duration,
        sticky: bool,
    ) {
        let Some(service) = self.scenario.services.last_mut() else {
            return;
        };
        service.check.acknowledgement = if sticky {
            AckKind::Sticky
        } else {
            AckKind::Normal
        };
        let object = ObjectKey::Service {
            key: service.key.clone(),
        };
        self.push_ack_comment(object, author, text, ago);
    }

    fn push_ack_comment(&mut self, object: ObjectKey, author: &str, text: &str, ago: Duration) {
        let id = self.rng.uuid();
        let entry_time = self.ago(ago);
        self.scenario.comments.push(Comment {
            name: format!("{}!{id}", object.full_name()),
            object,
            author: author.to_owned(),
            text: text.to_owned(),
            kind: CommentKind::Acknowledgement,
            entry_time,
            expire_time: None,
            persistent: false,
        });
    }

    /// Acknowledges a problem and adds its acknowledgement comment.
    pub(crate) fn acknowledge(
        &mut self,
        object: &ObjectKey,
        author: &str,
        text: &str,
        ago: Duration,
        sticky: bool,
    ) {
        let kind = if sticky {
            AckKind::Sticky
        } else {
            AckKind::Normal
        };
        match object {
            ObjectKey::Host { name } => {
                if let Some(host) = self.scenario.hosts.iter_mut().find(|h| &h.name == name) {
                    host.check.acknowledgement = kind;
                }
            }
            ObjectKey::Service { key } => {
                if let Some(service) = self.service_mut(key.host.as_str(), &key.name) {
                    service.check.acknowledgement = kind;
                }
            }
        }
        self.push_ack_comment(object.clone(), author, text, ago);
    }

    /// A downtime; returns its name.
    #[expect(
        clippy::too_many_arguments,
        reason = "a downtime is described by all of these"
    )]
    pub(crate) fn downtime(
        &mut self,
        object: ObjectKey,
        author: &str,
        comment: &str,
        start: Timestamp,
        end: Timestamp,
        fixed: bool,
        duration: f64,
        parent: Option<String>,
        config_owned: bool,
    ) -> String {
        let id = self.rng.uuid();
        let name = format!("{}!{id}", object.full_name());
        let entry_time =
            Timestamp::from_unix_seconds(start.as_unix_seconds().min(self.now) - 600.0);
        let in_effect =
            fixed && start.as_unix_seconds() <= self.now && self.now < end.as_unix_seconds();
        self.scenario.downtimes.push(Downtime {
            name: name.clone(),
            object,
            author: author.to_owned(),
            comment: comment.to_owned(),
            start_time: start,
            end_time: end,
            fixed,
            duration,
            entry_time,
            trigger_time: in_effect.then_some(start),
            triggered_by: None,
            parent,
            in_effect,
            config_owned,
            schedule: config_owned.then(|| "maintenance-window".to_owned()),
        });
        name
    }

    /// A downtime described in full ([`ScenarioDowntime`]): a flexible one
    /// that a problem already started, when it was set and by which
    /// `ScheduledDowntime`. Returns its full name.
    pub(crate) fn downtime_with(&mut self, spec: ScenarioDowntime) -> String {
        let id = self.rng.uuid();
        let name = format!("{}!{id}", spec.object.full_name());
        let start = spec.start.as_unix_seconds();
        let entry_time = spec
            .entry
            .unwrap_or_else(|| Timestamp::from_unix_seconds(start.min(self.now) - 600.0));
        let trigger_time = if spec.fixed {
            (start <= self.now && self.now < spec.end.as_unix_seconds()).then_some(spec.start)
        } else {
            spec.trigger
        };
        let in_effect = trigger_time.is_some_and(|trigger| {
            let until = if spec.fixed {
                spec.end.as_unix_seconds()
            } else {
                trigger.as_unix_seconds() + spec.duration
            };
            trigger.as_unix_seconds() <= self.now && self.now < until
        });
        self.scenario.downtimes.push(Downtime {
            name: name.clone(),
            object: spec.object,
            author: spec.author.to_owned(),
            comment: spec.comment.to_owned(),
            start_time: spec.start,
            end_time: spec.end,
            fixed: spec.fixed,
            duration: spec.duration,
            entry_time,
            trigger_time,
            triggered_by: None,
            parent: spec.parent,
            in_effect,
            config_owned: spec.schedule.is_some(),
            schedule: spec.schedule.map(str::to_owned),
        });
        name
    }

    /// A dependency of `child` on `parent`.
    pub(crate) fn depends(&mut self, child: ObjectKey, parent: ObjectKey, name: &str) {
        self.scenario.dependencies.push(Dependency {
            name: format!("{}!{name}", child.full_name()),
            child,
            parent,
        });
    }

    /// A cluster endpoint.
    pub(crate) fn endpoint(&mut self, name: &str, zone: &str, connected: bool) {
        self.scenario.endpoints.push(Endpoint {
            name: name.to_owned(),
            zone: zone.to_owned(),
            connected,
        });
    }
}

/// A downtime for [`Builder::downtime_with`].
#[derive(Clone, Debug)]
pub(crate) struct ScenarioDowntime {
    /// The host or service.
    pub(crate) object: ObjectKey,
    /// Who set it.
    pub(crate) author: &'static str,
    /// Why.
    pub(crate) comment: &'static str,
    /// The window.
    pub(crate) start: Timestamp,
    /// The window's end.
    pub(crate) end: Timestamp,
    /// Fixed, or flexible for `duration` seconds once a problem starts it.
    pub(crate) fixed: bool,
    /// A flexible downtime's length, in seconds.
    pub(crate) duration: f64,
    /// When a problem started a flexible downtime (`None`: not yet).
    pub(crate) trigger: Option<Timestamp>,
    /// When it was set (default: ten minutes before its start).
    pub(crate) entry: Option<Timestamp>,
    /// The host downtime it belongs to (`all_services`).
    pub(crate) parent: Option<String>,
    /// The `ScheduledDowntime` that made it (a config downtime).
    pub(crate) schedule: Option<&'static str>,
}

impl ScenarioDowntime {
    /// A fixed downtime on `object` for the window `start..end`.
    pub(crate) fn fixed(
        object: ObjectKey,
        author: &'static str,
        comment: &'static str,
        start: Timestamp,
        end: Timestamp,
    ) -> Self {
        Self {
            object,
            author,
            comment,
            start,
            end,
            fixed: true,
            duration: 0.0,
            trigger: None,
            entry: None,
            parent: None,
            schedule: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfdata_round_trips_through_the_parser() {
        for raw in [
            "replication_lag=412s;60;300",
            "wal_retained=1.8GiB;4;8",
            "active_connections=182;400;450",
            "'disk /var'=97%;80;90;0;100",
            "pl=100%;80;100;0",
            "load=U",
        ] {
            let parsed = parse_perfdata_entry(raw).unwrap();
            assert_eq!(format_perfdata(&parsed), raw);
        }
    }

    #[test]
    fn raw_check_results_split_output() {
        let result = raw_check_result(
            "CRIT - x\nline 2",
            &["a=1"],
            2,
            Timestamp::from_unix_seconds(100.0),
        );
        assert_eq!(result.output, "CRIT - x");
        assert_eq!(result.long_output, "line 2");
        assert_eq!(result.perfdata.len(), 1);
        assert_eq!(result.exit_status, 2);
    }
}
