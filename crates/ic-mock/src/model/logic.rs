//! Icinga's behaviour: the soft/hard state machine (`checkable-check.cpp`),
//! reachability (dependencies), acknowledgements, comments and downtimes
//! (`apiactions.cpp`, `comment.cpp`, `downtime.cpp`) and their timers.

use serde_json::{Map, Value as Json};

use super::types::{
    CheckResultData, Checkable, CommentData, DowntimeData, ObjMeta, SourceLocation, VarsState,
};
use super::{PendingExecution, World};
use crate::events::EventType;
use crate::json::{int, num};

/// `DependencyType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DepType {
    /// Reachability for state and severity.
    State,
    /// Whether checks may run / passive results are accepted.
    CheckExecution,
}

/// A check result to process.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CheckInput {
    /// Raw state 0..=3 (hosts: 0 up, 2 down).
    pub(crate) state: u8,
    pub(crate) exit_status: i64,
    /// Output including long output.
    pub(crate) output: String,
    pub(crate) performance_data: Option<Vec<String>>,
    pub(crate) active: bool,
    /// Empty: the node name, or the command endpoint.
    pub(crate) check_source: String,
    pub(crate) command: Json,
    pub(crate) ttl: f64,
    /// Zero: now.
    pub(crate) execution_start: f64,
    pub(crate) execution_end: f64,
    pub(crate) schedule_start: f64,
    pub(crate) schedule_end: f64,
}

/// `Checkable::ProcessingResult`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessOutcome {
    Processed,
    NewerCheckResultPresent,
}

/// Why a downtime is removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemovalReason {
    Expired,
    User,
}

/// Options of a new downtime.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DowntimeSpec {
    pub(crate) author: String,
    pub(crate) comment: String,
    pub(crate) start_time: f64,
    pub(crate) end_time: f64,
    pub(crate) fixed: bool,
    pub(crate) duration: f64,
    /// The downtime that triggers this one.
    pub(crate) trigger: Option<String>,
    pub(crate) scheduled_by: String,
    pub(crate) parent: String,
    pub(crate) config_owner: String,
}

const SOFT: u8 = 0;
const HARD: u8 = 1;

impl World {
    // --- derived attributes ---------------------------------------------

    /// `IsAcknowledged`, treating an expired acknowledgement as gone.
    pub(crate) fn is_acknowledged(&self, checkable: &Checkable) -> bool {
        checkable.acknowledgement != 0
            && !(checkable.acknowledgement_expiry != 0.0
                && checkable.acknowledgement_expiry < self.now())
    }

    /// `GetDowntimeDepth`: downtimes in effect right now.
    pub(crate) fn downtime_depth(&self, object: &str) -> u32 {
        let now = self.now();
        let count = self
            .downtimes_of(object)
            .iter()
            .filter_map(|name| self.downtimes.get(name))
            .filter(|d| d.is_in_effect(now))
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// `GetSeverity` (host.cpp / service.cpp).
    pub(crate) fn severity(&self, checkable: &Checkable) -> u32 {
        if !checkable.has_been_checked() {
            return 16;
        }
        let base = if checkable.is_service() {
            match checkable.state_raw {
                0 => return 0,
                1 => 32,
                3 => 64,
                2 => 128,
                _ => 256,
            }
        } else {
            if checkable.state() == 0 {
                return 0;
            }
            32
        };
        let full = checkable.full_name();
        base + if self.is_acknowledged(checkable) {
            512
        } else if self.downtime_depth(&full) > 0 {
            256
        } else if !self.is_reachable(&full, DepType::State) {
            1024
        } else {
            2048
        }
    }

    /// `GetHandled`: a problem in downtime or acknowledged; for services
    /// also any service whose host has a problem.
    pub(crate) fn handled(&self, checkable: &Checkable) -> bool {
        let own = checkable.problem()
            && (self.downtime_depth(&checkable.full_name()) > 0 || self.is_acknowledged(checkable));
        if own {
            return true;
        }
        checkable.is_service()
            && self
                .hosts
                .get(&checkable.host_name)
                .is_some_and(Checkable::problem)
    }

    /// `GetNextUpdate`.
    pub(crate) fn next_update(&self, checkable: &Checkable) -> f64 {
        let (interval, latency) = match &checkable.cr {
            Some(cr) => (
                if checkable.enable_active_checks
                    && checkable.problem()
                    && checkable.state_type == SOFT
                {
                    checkable.retry_interval
                } else {
                    checkable.check_interval
                },
                cr.execution_end - cr.schedule_start,
            ),
            None => (checkable.check_interval, 0.0),
        };
        let base = if checkable.enable_active_checks {
            checkable.next_check
        } else {
            checkable
                .cr
                .as_ref()
                .map_or(self.app.program_start, |cr| cr.execution_end)
                + interval
        };
        base + interval + 2.0 * latency
    }

    // --- reachability ----------------------------------------------------

    /// `Checkable::IsReachable` with dependency groups.
    pub(crate) fn is_reachable(&self, object: &str, dt: DepType) -> bool {
        self.reachable_inner(object, dt, 0)
    }

    fn reachable_inner(&self, object: &str, dt: DepType, depth: u32) -> bool {
        if depth > 256 {
            return false;
        }
        let Some(checkable) = self.checkable(object) else {
            return true;
        };
        if checkable.is_service()
            && dt != DepType::CheckExecution
            && let Some(host) = self.hosts.get(&checkable.host_name)
            && host.state() != 0
            && host.state_type == HARD
        {
            return false;
        }
        let dependencies = self.dependencies_of_child(object);
        if dependencies.is_empty() {
            return true;
        }
        // Group: redundancy groups share one group; other dependencies are
        // grouped by parent.
        let mut groups: std::collections::BTreeMap<(bool, String), Vec<&super::DependencyData>> =
            std::collections::BTreeMap::new();
        for dependency in dependencies {
            let key = if dependency.redundancy_group.is_empty() {
                (false, dependency.parent.full_name())
            } else {
                (true, dependency.redundancy_group.clone())
            };
            groups.entry(key).or_default().push(dependency);
        }
        for ((redundant, _), members) in groups {
            let mut reachable = 0;
            let mut available = 0;
            for dependency in &members {
                let parent = dependency.parent.full_name();
                if self.reachable_inner(&parent, dt, depth + 1) {
                    reachable += 1;
                    if self.dependency_available(dependency, dt) {
                        available += 1;
                    }
                }
            }
            let ok = if redundant {
                reachable > 0 && available > 0
            } else {
                reachable == members.len() && available == members.len()
            };
            if !ok {
                return false;
            }
        }
        true
    }

    /// `Dependency::IsAvailable`.
    fn dependency_available(&self, dependency: &super::DependencyData, dt: DepType) -> bool {
        if dependency.parent == dependency.child {
            return true;
        }
        let Some(parent) = self.checkable_by_key(&dependency.parent) else {
            return true;
        };
        if !parent.has_been_checked() {
            return true;
        }
        if dependency.ignore_soft_states && parent.state_type == SOFT {
            return true;
        }
        let bit = if parent.is_service() {
            1u32 << parent.state_raw.min(3)
        } else if parent.state() == 0 {
            16
        } else {
            32
        };
        if bit & dependency.state_filter() != 0 {
            return true;
        }
        match dt {
            DepType::CheckExecution => !dependency.disable_checks,
            DepType::State => false,
        }
    }

    // --- check results ---------------------------------------------------

    /// `Checkable::ProcessCheckResult`. `None` if the object doesn't exist.
    #[expect(
        clippy::too_many_lines,
        reason = "follows Icinga's ProcessCheckResult step by step"
    )]
    pub(crate) fn process_check_result(
        &mut self,
        object: &str,
        input: CheckInput,
    ) -> Option<ProcessOutcome> {
        let now = self.now();
        let node = self.app.node_name.clone();
        let reachable = self.is_reachable(object, DepType::State);
        // `GetAcknowledgement()` drops an expired acknowledgement first.
        self.expire_acknowledgement(object);
        let checkable = self.checkable(object)?;
        let or_now = |t: f64| if t == 0.0 { now } else { t };
        let mut cr = CheckResultData {
            schedule_start: or_now(input.schedule_start),
            schedule_end: or_now(input.schedule_end),
            execution_start: or_now(input.execution_start),
            execution_end: or_now(input.execution_end),
            command: input.command,
            exit_status: input.exit_status,
            state: input.state.min(3),
            previous_hard_state: 99,
            output: input.output,
            performance_data: input.performance_data,
            active: input.active,
            check_source: input.check_source,
            scheduling_source: node.clone(),
            ttl: input.ttl,
            vars_before: None,
            vars_after: None,
        };
        if cr.check_source.is_empty() {
            cr.check_source = if checkable.command_endpoint.is_empty() {
                node
            } else {
                checkable.command_endpoint.clone()
            };
        }
        if let Some(old) = &checkable.cr
            && old.execution_start <= now
            && cr.execution_start < old.execution_start
        {
            return Some(ProcessOutcome::NewerCheckResultPresent);
        }
        let execution_end = cr.execution_end;

        let checkable = self.checkable_mut(object)?;
        let old_state = checkable.state_raw;
        let old_type = checkable.state_type;
        let old_attempt = checkable.check_attempt;
        let old_vars = checkable.cr.as_ref().and_then(|old| old.vars_after);
        checkable.last_state_raw = old_state;
        checkable.last_state_type = old_type;
        checkable.last_reachable = reachable;

        let new_state = cr.state;
        let mut attempt = 1;
        let mut recovery = false;
        if checkable.is_state_ok(new_state) {
            checkable.state_type = HARD;
            recovery = !checkable.is_state_ok(old_state);
            save_last_state(checkable, 0, execution_end);
        } else {
            if checkable.is_state_ok(old_state) {
                checkable.state_type = SOFT;
                attempt = 1;
            }
            if old_type == SOFT && !checkable.is_state_ok(old_state) {
                checkable.state_type = SOFT;
                attempt = old_attempt + 1;
            }
            if attempt >= checkable.max_check_attempts {
                checkable.state_type = HARD;
                attempt = 1;
            }
            save_last_state(checkable, new_state, execution_end);
        }
        if !reachable {
            checkable.last_state_unreachable = execution_end;
        }
        checkable.check_attempt = attempt;
        checkable.state_raw = new_state;
        let state_change = if checkable.is_service() {
            old_state != new_state
        } else {
            super::types::host_state(old_state) != super::types::host_state(new_state)
        };
        checkable.previous_state_change = checkable.last_state_change;
        if state_change {
            checkable.last_state_change = execution_end;
        }
        let clear_ack = state_change
            && (checkable.acknowledgement == 1
                || (checkable.acknowledgement == 2 && checkable.is_state_ok(new_state)));
        let new_type = checkable.state_type;
        let mut hard_change = new_type == HARD && old_type == SOFT;
        if state_change && old_type == HARD && new_type == HARD {
            hard_change = true;
        }
        let volatile = checkable.volatile;
        if hard_change || volatile {
            checkable.last_hard_state_raw = new_state;
            checkable.last_hard_state_change = execution_end;
            checkable.last_hard_states_raw =
                checkable.last_hard_states_raw / 100 + u16::from(new_state) * 100;
        }
        cr.previous_hard_state = u8::try_from(checkable.last_hard_states_raw % 100).unwrap_or(99);
        let old_ok = checkable.is_state_ok(old_state);
        let new_ok = checkable.is_state_ok(new_state);
        let is_service = checkable.is_service();

        if clear_ack {
            self.clear_acknowledgement(object, "");
        }
        let ack_none = self
            .checkable(object)
            .is_some_and(|c| !self.is_acknowledged(c));
        if !new_ok {
            self.trigger_downtimes(object, execution_end);
        }
        self.stats.record(execution_end, cr.active, !is_service);
        if ack_none {
            self.remove_ack_comments(object, "", execution_end);
        }

        let was_flapping = self.checkable(object).is_some_and(|c| self.is_flapping(c));
        let enable_flapping_global = self.app.enable_flapping;
        let checkable = self.checkable_mut(object)?;
        cr.vars_before = old_vars;
        cr.vars_after = Some(VarsState {
            attempt: checkable.check_attempt,
            reachable,
            state: new_state,
            state_type: checkable.state_type,
        });
        let active = cr.active;
        let ttl = cr.ttl;
        checkable.cr = Some(cr);
        update_flapping(checkable, new_state, now);
        // Next check: the regular schedule for active results, the TTL (or
        // check interval) for passive ones.
        checkable.next_check = if active {
            now + if checkable.state_type == SOFT {
                checkable.retry_interval
            } else {
                checkable.check_interval
            }
        } else if ttl > 0.0 {
            now + ttl
        } else {
            now + checkable.check_interval
        };
        let is_flapping = checkable.enable_flapping && enable_flapping_global && checkable.flapping;
        let new_state_type = checkable.state_type;

        // `OnFlappingChanged` fires inside `UpdateFlappingStatus`, before
        // `OnNewCheckResult`.
        if was_flapping != is_flapping {
            self.emit(EventType::Flapping, |world| {
                world.flapping_event(object).unwrap_or_default()
            });
        }
        self.emit(EventType::CheckResult, |world| {
            world.checkable_event(object, false).unwrap_or_default()
        });
        let state_change_event = hard_change
            || (volatile && !(old_ok && new_ok))
            || state_change
            || new_state_type == SOFT;
        if state_change_event {
            self.emit(EventType::StateChange, |world| {
                world.checkable_event(object, true).unwrap_or_default()
            });
        }
        // Notifications (`ProcessCheckResult`): on a hard state change (not
        // soft OK to hard OK), or every hard result of a volatile object;
        // held back while flapping, in downtime, acknowledged or
        // unreachable. (Icinga then stashes them as suppressed; the mock
        // drops them.)
        let mut send_notification =
            (hard_change && !(old_type == SOFT && new_ok)) || (volatile && new_state_type == HARD);
        if (old_ok && old_type == SOFT) || (volatile && old_ok && new_ok) {
            send_notification = false;
        }
        let suppressed = !reachable
            || self.downtime_depth(object) > 0
            || self
                .checkable(object)
                .is_some_and(|c| self.is_acknowledged(c));
        if send_notification && !is_flapping && !suppressed {
            self.send_notifications(object, if new_ok { "RECOVERY" } else { "PROBLEM" });
        }
        if recovery {
            // Icinga re-checks problem children of a recovered parent soon.
            let children = self.children_of(object);
            for child in children {
                if let Some(c) = self.checkable_mut(&child)
                    && c.problem()
                    && c.enable_active_checks
                {
                    c.next_check = c.next_check.min(now + 30.0);
                }
            }
        }
        Some(ProcessOutcome::Processed)
    }

    /// `Checkable::SendNotifications` → `Notification::BeginExecuteNotification`
    /// for a `PROBLEM` or `RECOVERY` (Icinga's compat type names): every
    /// notification of the object notifies its users (and the members of
    /// its user groups); a recovery only those told about the problem, and
    /// it clears that list. Each sends a `Notification` event. User
    /// filters, time periods and `times` are not modelled.
    pub(crate) fn send_notifications(&mut self, object: &str, kind: &'static str) {
        let Some(checkable) = self.checkable(object) else {
            return;
        };
        if !self.app.enable_notifications || !checkable.enable_notifications {
            return;
        }
        let recovery = kind == "RECOVERY";
        let now = self.now();
        for name in self.notifications_of(object) {
            let members: Vec<String> = match self.notifications.get(&name) {
                Some(notification) => self
                    .users
                    .values()
                    .filter(|user| {
                        notification.users.contains(&user.name)
                            || user
                                .groups
                                .iter()
                                .any(|group| notification.user_groups.contains(group))
                    })
                    .map(|user| user.name.clone())
                    .collect(),
                None => continue,
            };
            let Some(notification) = self.notifications.get_mut(&name) else {
                continue;
            };
            let users: Vec<String> = if recovery {
                members
                    .into_iter()
                    .filter(|user| notification.notified_problem_users.contains(user))
                    .collect()
            } else {
                members
            };
            notification.last_notification = now;
            if recovery {
                notification.notification_number = 0;
                notification.notified_problem_users.clear();
            } else {
                notification.notification_number += 1;
                notification.last_problem_notification = now;
                notification.next_notification = now + notification.interval;
                for user in &users {
                    if !notification.notified_problem_users.contains(user) {
                        notification.notified_problem_users.push(user.clone());
                    }
                }
            }
            let command = notification.command.clone();
            self.emit(EventType::Notification, |world| {
                let mut event = Map::new();
                if let Some(checkable) = world.checkable(object) {
                    Self::event_object_fields(&mut event, checkable);
                    event.insert(
                        "check_result".into(),
                        checkable
                            .cr
                            .as_ref()
                            .map_or(Json::Null, CheckResultData::to_json),
                    );
                }
                event.insert("command".into(), Json::String(command));
                event.insert(
                    "users".into(),
                    Json::Array(users.into_iter().map(Json::String).collect()),
                );
                event.insert("notification_type".into(), Json::String(kind.to_owned()));
                event.insert("author".into(), Json::String(String::new()));
                event.insert("text".into(), Json::String(String::new()));
                event
            });
        }
    }

    /// `IsFlapping`: the flag, but only with flap detection enabled.
    pub(crate) fn is_flapping(&self, checkable: &Checkable) -> bool {
        checkable.enable_flapping && self.app.enable_flapping && checkable.flapping
    }

    /// The payload of `CheckResult` / `StateChange` events.
    fn checkable_event(&self, object: &str, state_change: bool) -> Option<Map<String, Json>> {
        let checkable = self.checkable(object)?;
        let mut event = Map::new();
        Self::event_object_fields(&mut event, checkable);
        if state_change {
            event.insert("state".into(), int(checkable.state()));
            event.insert("state_type".into(), int(checkable.state_type));
        }
        event.insert(
            "check_result".into(),
            checkable
                .cr
                .as_ref()
                .map_or(Json::Null, CheckResultData::to_json),
        );
        event.insert("downtime_depth".into(), int(self.downtime_depth(object)));
        event.insert(
            "acknowledgement".into(),
            Json::Bool(self.is_acknowledged(checkable)),
        );
        Some(event)
    }

    fn flapping_event(&self, object: &str) -> Option<Map<String, Json>> {
        let checkable = self.checkable(object)?;
        let mut event = Map::new();
        Self::event_object_fields(&mut event, checkable);
        event.insert("state".into(), int(checkable.state()));
        event.insert("state_type".into(), int(checkable.state_type));
        event.insert(
            "is_flapping".into(),
            Json::Bool(self.is_flapping(checkable)),
        );
        event.insert("flapping_current".into(), num(checkable.flapping_current));
        event.insert(
            "threshold_low".into(),
            num(checkable.flapping_threshold_low),
        );
        event.insert(
            "threshold_high".into(),
            num(checkable.flapping_threshold_high),
        );
        Some(event)
    }

    /// Runs a check now and processes its result: the current state again
    /// with fresh timestamps (what a scheduled check does in the mock).
    pub(crate) fn run_check(&mut self, object: &str) -> Option<ProcessOutcome> {
        if self.checks_stopped {
            return None;
        }
        let checkable = self.checkable(object)?;
        let input = self.recheck_input(checkable, None);
        self.process_check_result(object, input)
    }

    /// A check result that repeats the current state, or reports `state`
    /// with `output`.
    pub(crate) fn recheck_input(
        &self,
        checkable: &Checkable,
        replace: Option<(u8, String, Option<Vec<String>>)>,
    ) -> CheckInput {
        let now = self.now();
        let (state, output, performance_data) = match replace {
            Some(values) => values,
            None => match &checkable.cr {
                Some(cr) => (cr.state, cr.output.clone(), cr.performance_data.clone()),
                None => {
                    if checkable.is_service() {
                        (0, "OK".to_owned(), Some(Vec::new()))
                    } else {
                        (0, "PING OK - Packet loss = 0%".to_owned(), Some(Vec::new()))
                    }
                }
            },
        };
        let mut command = checkable.cr.as_ref().map_or_else(
            || plugin_command(&checkable.check_command),
            |cr| {
                if cr.command.is_null() {
                    plugin_command(&checkable.check_command)
                } else {
                    cr.command.clone()
                }
            },
        );
        let address = self
            .hosts
            .get(&checkable.host_name)
            .map_or("", |host| host.address.as_str());
        super::load::resolve_address(&mut command, address);
        // The check ends now. It started its execution time earlier, but
        // not before the previous check ended: Icinga never runs two checks
        // of one object at once, and discards a result that started before
        // the current one.
        let previous_end = checkable
            .cr
            .as_ref()
            .map_or(f64::MIN, |cr| cr.execution_end.min(now));
        let execution_start = (now - execution_time(&checkable.check_command)).max(previous_end);
        CheckInput {
            state,
            exit_status: i64::from(state),
            output,
            performance_data: performance_data.or_else(|| Some(Vec::new())),
            active: true,
            check_source: String::new(),
            command,
            ttl: 0.0,
            schedule_start: execution_start - 0.001,
            schedule_end: now,
            execution_start,
            execution_end: now,
        }
    }

    // --- acknowledgements ------------------------------------------------

    /// `acknowledge-problem` after validation: adds the acknowledgement
    /// comment, then sets the acknowledgement.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the acknowledge-problem parameters"
    )]
    pub(crate) fn acknowledge(
        &mut self,
        object: &str,
        author: &str,
        comment: &str,
        sticky: bool,
        notify: bool,
        persistent: bool,
        expiry: f64,
    ) {
        self.add_comment(object, 4, author, comment, persistent, expiry, sticky);
        let now = self.now();
        let Some(checkable) = self.checkable_mut(object) else {
            return;
        };
        checkable.acknowledgement = if sticky { 2 } else { 1 };
        checkable.acknowledgement_expiry = expiry;
        self.emit(EventType::AcknowledgementSet, |world| {
            let mut event = Map::new();
            if let Some(checkable) = world.checkable(object) {
                Self::event_object_fields(&mut event, checkable);
                event.insert("state".into(), int(checkable.state()));
                event.insert("state_type".into(), int(checkable.state_type));
                event.insert(
                    "acknowledgement_type".into(),
                    int(checkable.acknowledgement),
                );
            }
            event.insert("author".into(), Json::String(author.to_owned()));
            event.insert("comment".into(), Json::String(comment.to_owned()));
            event.insert("notify".into(), Json::Bool(notify));
            event.insert("persistent".into(), Json::Bool(persistent));
            event.insert("expiry".into(), num(expiry));
            event
        });
        if let Some(checkable) = self.checkable_mut(object) {
            checkable.acknowledgement_last_change = now;
        }
    }

    /// `ClearAcknowledgement`: emits `AcknowledgementCleared` if it was set
    /// (the last-change time moves either way, as in Icinga).
    pub(crate) fn clear_acknowledgement(&mut self, object: &str, _removed_by: &str) {
        let now = self.now();
        let Some(checkable) = self.checkable_mut(object) else {
            return;
        };
        let was_acked = checkable.acknowledgement != 0;
        checkable.acknowledgement = 0;
        checkable.acknowledgement_expiry = 0.0;
        checkable.acknowledgement_last_change = now;
        if was_acked {
            self.emit(EventType::AcknowledgementCleared, |world| {
                let mut event = Map::new();
                if let Some(checkable) = world.checkable(object) {
                    Self::event_object_fields(&mut event, checkable);
                    event.insert("state".into(), int(checkable.state()));
                    event.insert("state_type".into(), int(checkable.state_type));
                }
                event.insert("acknowledgement_type".into(), int(0));
                event
            });
        }
    }

    /// Clears an acknowledgement whose expiry passed (what Icinga's
    /// `GetAcknowledgement()` does whenever it is asked).
    pub(crate) fn expire_acknowledgement(&mut self, object: &str) {
        let now = self.now();
        let expired = self.checkable(object).is_some_and(|c| {
            c.acknowledgement != 0
                && c.acknowledgement_expiry != 0.0
                && c.acknowledgement_expiry < now
        });
        if expired {
            self.clear_acknowledgement(object, "");
        }
    }

    /// `RemoveAckComments`: non-persistent acknowledgement comments created
    /// before `created_before`.
    pub(crate) fn remove_ack_comments(
        &mut self,
        object: &str,
        removed_by: &str,
        created_before: f64,
    ) {
        let names: Vec<String> = self
            .comments_of(object)
            .into_iter()
            .filter(|name| {
                self.comments.get(name).is_some_and(|c| {
                    c.entry_type == 4 && !c.persistent && c.entry_time <= created_before
                })
            })
            .collect();
        for name in names {
            self.remove_comment(&name, removed_by);
        }
    }

    // --- comments --------------------------------------------------------

    /// `Comment::AddComment`; returns the name and legacy id.
    #[expect(clippy::too_many_arguments, reason = "mirrors Comment::AddComment")]
    pub(crate) fn add_comment(
        &mut self,
        object: &str,
        entry_type: u8,
        author: &str,
        text: &str,
        persistent: bool,
        expire_time: f64,
        sticky: bool,
    ) -> Option<(String, u64)> {
        let now = self.now();
        let checkable = self.checkable(object)?;
        let zone = checkable.meta.zone.clone();
        let (host, service) = (checkable.host_name.clone(), checkable.service_name.clone());
        let short = self.names.uuid();
        let name = format!("{object}!{short}");
        let legacy_id = self.next_comment_legacy_id();
        let comment = CommentData {
            name: name.clone(),
            short_name: short.clone(),
            host_name: host,
            service_name: service,
            author: author.to_owned(),
            text: text.to_owned(),
            entry_time: now,
            entry_type,
            expire_time,
            persistent,
            sticky,
            legacy_id,
            meta: runtime_meta("Comment", &short, &zone, &name, now),
        };
        self.index_comment(&comment);
        self.comments.insert(name.clone(), comment);
        self.emit(EventType::CommentAdded, |world| {
            let mut event = Map::new();
            if let Some(comment) = world.comments.get(&name) {
                event.insert("comment".into(), comment.to_event_json());
            }
            event
        });
        self.emit_object_change(EventType::ObjectCreated, "Comment", &name);
        Some((name, legacy_id))
    }

    /// `Comment::RemoveComment` (only runtime-created comments).
    pub(crate) fn remove_comment(&mut self, name: &str, _removed_by: &str) -> bool {
        let Some(comment) = self.comments.get(name) else {
            return false;
        };
        if comment.meta.package != "_api" {
            return false;
        }
        let Some(comment) = self.comments.remove(name) else {
            return false;
        };
        self.unindex_comment(&comment);
        self.emit(EventType::CommentRemoved, |_| {
            let mut event = Map::new();
            event.insert("comment".into(), comment.to_event_json());
            event
        });
        self.emit_object_change(EventType::ObjectDeleted, "Comment", name);
        true
    }

    // --- downtimes -------------------------------------------------------

    /// `Downtime::AddDowntime` and `Downtime::Start`.
    pub(crate) fn add_downtime(
        &mut self,
        object: &str,
        spec: DowntimeSpec,
    ) -> Option<(String, u64)> {
        let now = self.now();
        let checkable = self.checkable(object)?;
        let zone = checkable.meta.zone.clone();
        let (host, service) = (checkable.host_name.clone(), checkable.service_name.clone());
        let state_ok = checkable.is_state_ok(checkable.state_raw);
        let last_state_change = checkable.last_state_change;
        let short = self.names.uuid();
        let name = format!("{object}!{short}");
        let legacy_id = self.next_downtime_legacy_id();
        let triggered_by = spec.trigger.clone().unwrap_or_default();
        let downtime = DowntimeData {
            name: name.clone(),
            short_name: short.clone(),
            host_name: host,
            service_name: service,
            author: spec.author,
            comment: spec.comment,
            start_time: spec.start_time,
            end_time: spec.end_time,
            entry_time: now,
            trigger_time: 0.0,
            fixed: spec.fixed,
            duration: spec.duration,
            triggered_by,
            scheduled_by: spec.scheduled_by,
            parent: spec.parent,
            triggers: Vec::new(),
            legacy_id,
            remove_time: 0.0,
            authoritative_zone: if spec.config_owner.is_empty() {
                String::new()
            } else {
                zone.clone()
            },
            config_owner_hash: if spec.config_owner.is_empty() {
                String::new()
            } else {
                format!("{:016x}", self.names.next_u64())
            },
            config_owner: spec.config_owner,
            meta: runtime_meta("Downtime", &short, &zone, &name, now),
        };
        if let Some(trigger) = &spec.trigger
            && let Some(parent) = self.downtimes.get_mut(trigger)
            && !parent.triggers.contains(&name)
        {
            parent.triggers.push(name.clone());
        }
        self.index_downtime(&downtime);
        self.downtimes.insert(name.clone(), downtime);
        self.emit(EventType::DowntimeAdded, |world| {
            downtime_event(world, &name)
        });
        let (fixed, start_time, entry_time) = {
            let d = self.downtimes.get(&name)?;
            (d.fixed, d.start_time, d.entry_time)
        };
        if !fixed && !state_ok {
            self.trigger_downtime(&name, start_time.max(entry_time).max(last_state_change), 0);
        }
        if fixed
            && self
                .downtimes
                .get(&name)
                .is_some_and(|d| d.can_be_triggered(now))
        {
            self.emit(EventType::DowntimeStarted, |world| {
                downtime_event(world, &name)
            });
            self.trigger_downtime(&name, start_time.max(entry_time), 0);
        }
        self.emit_object_change(EventType::ObjectCreated, "Downtime", &name);
        Some((name, legacy_id))
    }

    /// `Downtime::TriggerDowntime`, including the downtimes it triggers.
    pub(crate) fn trigger_downtime(&mut self, name: &str, trigger_time: f64, depth: u32) {
        let now = self.now();
        if depth > 32 {
            return;
        }
        let Some(downtime) = self.downtimes.get_mut(name) else {
            return;
        };
        if !downtime.can_be_triggered(now) {
            return;
        }
        if downtime.trigger_time == 0.0 {
            downtime.trigger_time = trigger_time;
        }
        let triggers = downtime.triggers.clone();
        for child in triggers {
            self.trigger_downtime(&child, trigger_time, depth + 1);
        }
        self.emit(EventType::DowntimeTriggered, |world| {
            downtime_event(world, name)
        });
    }

    /// `Checkable::TriggerDowntimes`.
    pub(crate) fn trigger_downtimes(&mut self, object: &str, trigger_time: f64) {
        for name in self.downtimes_of(object) {
            self.trigger_downtime(&name, trigger_time, 0);
        }
    }

    /// `Downtime::RemoveDowntime`.
    ///
    /// # Errors
    /// Users can't remove downtimes owned by a `ScheduledDowntime`.
    pub(crate) fn remove_downtime(
        &mut self,
        name: &str,
        include_children: bool,
        reason: RemovalReason,
        _removed_by: &str,
    ) -> Result<bool, String> {
        let Some(downtime) = self.downtimes.get(name) else {
            return Ok(false);
        };
        if downtime.meta.package != "_api" {
            return Ok(false);
        }
        if !downtime.config_owner.is_empty() && reason == RemovalReason::User {
            return Err(format!(
                "Cannot remove downtime '{}'. It is owned by scheduled downtime object '{}'",
                downtime.name, downtime.config_owner
            ));
        }
        if include_children {
            for child in self.downtime_children(name) {
                self.remove_downtime(&child, true, reason, "")?;
            }
        }
        let now = self.now();
        let Some(mut downtime) = self.downtimes.remove(name) else {
            return Ok(false);
        };
        if reason != RemovalReason::Expired {
            downtime.remove_time = now;
        }
        self.unindex_downtime(&downtime);
        let payload = downtime.to_event_json();
        self.emit(EventType::DowntimeRemoved, |_| {
            let mut event = Map::new();
            event.insert("downtime".into(), payload);
            event
        });
        self.emit_object_change(EventType::ObjectDeleted, "Downtime", name);
        Ok(true)
    }

    // --- timers ------------------------------------------------------------

    /// Icinga's timers: acknowledgement expiry, fixed downtimes starting,
    /// expired downtimes and comments, rescheduled checks that are due (to
    /// the checker's queue) and command executions.
    pub(crate) fn housekeeping(&mut self) {
        let now = self.now();
        // Expired acknowledgements.
        let expired_acks: Vec<String> = self
            .all_checkables()
            .filter(|c| {
                c.acknowledgement != 0
                    && c.acknowledgement_expiry != 0.0
                    && c.acknowledgement_expiry < now
            })
            .map(Checkable::full_name)
            .collect();
        for object in expired_acks {
            self.clear_acknowledgement(&object, "");
        }
        // Fixed downtimes whose window started.
        let starting: Vec<(String, f64)> = self
            .downtimes
            .values()
            .filter(|d| d.fixed && d.can_be_triggered(now))
            .map(|d| (d.name.clone(), d.start_time.max(d.entry_time)))
            .collect();
        for (name, trigger_time) in starting {
            if self
                .downtimes
                .get(&name)
                .is_some_and(|d| d.can_be_triggered(now))
            {
                self.emit(EventType::DowntimeStarted, |world| {
                    downtime_event(world, &name)
                });
                self.trigger_downtime(&name, trigger_time, 0);
            }
        }
        // Expired downtimes.
        let expired: Vec<String> = self
            .downtimes
            .values()
            .filter(|d| d.is_expired(now))
            .map(|d| d.name.clone())
            .collect();
        for name in expired {
            let _ = self.remove_downtime(&name, false, RemovalReason::Expired, "");
        }
        // Expired comments (persistent acknowledgement comments stay).
        let expired: Vec<String> = self
            .comments
            .values()
            .filter(|c| c.expire_time != 0.0 && c.expire_time < now)
            .filter(|c| !(c.entry_type == 4 && c.persistent))
            .map(|c| c.name.clone())
            .collect();
        for name in expired {
            self.remove_comment(&name, "");
        }
        // Rescheduled checks that are due go to the checker, in order.
        let mut due: Vec<(f64, String)> = self
            .scheduled_checks
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(object, at)| (*at, object.clone()))
            .collect();
        due.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        for (_, object) in due {
            self.scheduled_checks.remove(&object);
            self.checks.push(&object);
        }
        self.run_realtime_checks(now);
        // Command executions.
        let (due, later): (Vec<PendingExecution>, Vec<PendingExecution>) =
            std::mem::take(&mut self.pending_executions)
                .into_iter()
                .partition(|execution| execution.due <= now);
        self.pending_executions = later;
        for execution in due {
            self.finish_execution(&execution, now);
        }
    }

    /// Runs the real-time checks that are due (heartbeats): OK, or UNKNOWN
    /// with Icinga's output while the endpoint the check is pinned to
    /// isn't connected. Nothing runs while checks are stopped.
    pub(crate) fn run_realtime_checks(&mut self, now: f64) {
        if self.checks_stopped {
            return;
        }
        let due: Vec<String> = self
            .realtime
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(object, _)| object.clone())
            .collect();
        for object in due {
            let Some(checkable) = self.checkable(&object) else {
                self.realtime.remove(&object);
                continue;
            };
            let interval = checkable.check_interval.max(1.0);
            let node = self.app.node_name.clone();
            let pinned = checkable.command_endpoint.clone();
            let disconnected = !pinned.is_empty()
                && pinned != node
                && self
                    .endpoints
                    .get(&pinned)
                    .is_none_or(|endpoint| !endpoint.connected);
            let (state, output, source) = if disconnected {
                (
                    3,
                    format!("Remote Icinga instance '{pinned}' is not connected to '{node}'"),
                    node.clone(),
                )
            } else {
                let source = if pinned.is_empty() { node.clone() } else { pinned.clone() };
                (0, format!("icygui heartbeat {now:.0}"), source)
            };
            let base = self.recheck_input(checkable, Some((state, output, Some(Vec::new()))));
            let input = CheckInput {
                check_source: source,
                ..base
            };
            self.process_check_result(&object, input);
            self.realtime.insert(object, now + interval);
        }
    }

    /// Schedules a check (from `reschedule-check`) for `at`; it reaches
    /// the checker's queue `reschedule_delay` after that (the timers move
    /// it). A later call for the same object replaces the time.
    pub(crate) fn schedule_check(&mut self, object: &str, at: f64) {
        let due = at.max(self.now()) + self.reschedule_delay;
        self.scheduled_checks.insert(object.to_owned(), due);
    }

    fn finish_execution(&mut self, execution: &PendingExecution, now: f64) {
        let Some(checkable) = self.checkable_mut(&execution.object) else {
            return;
        };
        let output = if execution.command_type == "CheckCommand" {
            checkable
                .cr
                .as_ref()
                .map_or_else(|| "OK".to_owned(), |cr| cr.output.clone())
        } else {
            format!(
                "Executed {} '{}'",
                execution.command_type, execution.command
            )
        };
        let exit = if execution.command_type == "CheckCommand" {
            checkable.cr.as_ref().map_or(0, |cr| i64::from(cr.state))
        } else {
            0
        };
        if let Some(entry) = checkable
            .executions
            .as_mut()
            .and_then(|executions| executions.get_mut(&execution.id))
            .and_then(Json::as_object_mut)
        {
            entry.remove("pending");
            entry.insert("exit".into(), int(exit));
            entry.insert("output".into(), Json::String(output));
            entry.insert("start".into(), num(now - 0.05));
            entry.insert("end".into(), num(now));
        }
    }
}

/// The payload of downtime events.
fn downtime_event(world: &World, name: &str) -> Map<String, Json> {
    let mut event = Map::new();
    if let Some(downtime) = world.downtimes.get(name) {
        event.insert("downtime".into(), downtime.to_event_json());
    }
    event
}

/// `SaveLastState`.
fn save_last_state(checkable: &mut Checkable, state: u8, at: f64) {
    if checkable.is_service() {
        if let Some(slot) = checkable.last_state_at.get_mut(usize::from(state.min(3))) {
            *slot = at;
        }
    } else {
        match state {
            0 | 1 => checkable.last_state_at[0] = at,
            2 => checkable.last_state_at[1] = at,
            _ => {}
        }
    }
}

/// `UpdateFlappingStatus`: a weighted count of state changes over the last
/// 20 results.
fn update_flapping(checkable: &mut Checkable, new_state: u8, now: f64) {
    let state_change = new_state != checkable.flapping_last_state;
    checkable.flapping_last_state = new_state;
    let index = checkable.flapping_index % 20;
    if state_change {
        checkable.flapping_buffer |= 1 << index;
    } else {
        checkable.flapping_buffer &= !(1 << index);
    }
    let oldest = (index + 1) % 20;
    let mut changes = 0.0;
    for i in 0..20 {
        if checkable.flapping_buffer & (1 << ((oldest + i) % 20)) != 0 {
            changes += 0.8 + 0.02 * f64::from(i);
        }
    }
    let value = 100.0 * changes / 20.0;
    let flapping = if checkable.flapping {
        value > checkable.flapping_threshold_low
    } else {
        value > checkable.flapping_threshold_high
    };
    checkable.flapping_index = oldest;
    checkable.flapping_current = value;
    if flapping != checkable.flapping {
        checkable.flapping = flapping;
        checkable.flapping_last_change = now;
    }
}

/// `ConfigObjectUtility::EscapeName`: `<>:"/\|?*` and `%` become `%XX`.
pub(crate) fn escape_file_name(name: &str) -> String {
    use std::fmt::Write as _;
    let mut escaped = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(
            c,
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '%'
        ) {
            let _ = write!(escaped, "%{:02X}", u32::from(c));
        } else {
            escaped.push(c);
        }
    }
    escaped
}

/// `ConfigObject` attributes of a comment or downtime created at runtime
/// (`ConfigObjectUtility::CreateObject`): a file named after the escaped
/// full name in the `_api` package. Its one generated line starts with
/// `object <Type> "<short name>" ignore_on_error`, which is what the
/// source location spans (columns counted from 0, as recorded from Icinga
/// 2.15).
pub(crate) fn runtime_meta(
    type_name: &str,
    short: &str,
    zone: &str,
    full_name: &str,
    now: f64,
) -> ObjMeta {
    let directory = format!("{}s", type_name.to_lowercase());
    let header = format!("object {type_name} \"{short}\" ignore_on_error");
    let last_column = u32::try_from(header.chars().count().saturating_sub(1)).unwrap_or(u32::MAX);
    ObjMeta {
        templates: vec![short.to_owned()],
        package: "_api".to_owned(),
        zone: zone.to_owned(),
        source: SourceLocation {
            path: format!(
                "/var/lib/icinga2/api/packages/_api/9e5e1e6f-2c43-4c09-8c53-7a1ee8d9d2b1/conf.d/{directory}/{}.conf",
                escape_file_name(full_name)
            ),
            first_line: 1,
            first_column: 0,
            last_line: 1,
            last_column,
        },
        version: now,
    }
}

/// The task function of the check commands Icinga runs internally instead
/// of starting a plugin (`command-icinga.conf` in the ITL).
pub(crate) fn internal_check_function(check_command: &str) -> Option<&'static str> {
    Some(match check_command {
        "dummy" | "passive" => "Internal#DummyCheck",
        "random" => "Internal#RandomCheck",
        "icinga" => "Internal#IcingaCheck",
        "cluster" => "Internal#ClusterCheck",
        "cluster-zone" => "Internal#ClusterZoneCheck",
        "exception" => "Internal#ExceptionCheck",
        "null" => "Internal#NullCheck",
        "sleep" => "Internal#SleepCheck",
        "ido" => "Internal#IdoCheck",
        "ifw-api" => "Internal#IfwApiCheck",
        _ => return None,
    })
}

/// The `command` of an active check result: the command line a plugin
/// check ran (with macros still unresolved), or the command's name for
/// internal checks (`"dummy"`), as Icinga 2.15 records them.
pub(crate) fn plugin_command(check_command: &str) -> Json {
    if internal_check_function(check_command).is_some() {
        return Json::String(check_command.to_owned());
    }
    let plugin = match check_command {
        "hostalive" | "ping4" | "ping" => "check_ping",
        other => {
            let plugin = other.replace('-', "_");
            let plugin = if plugin.starts_with("check_") {
                plugin
            } else {
                format!("check_{plugin}")
            };
            return Json::Array(vec![Json::String(format!(
                "/usr/lib/nagios/plugins/{plugin}"
            ))]);
        }
    };
    Json::Array(
        [
            format!("/usr/lib/nagios/plugins/{plugin}"),
            "-H".to_owned(),
            "$address$".to_owned(),
            "-c".to_owned(),
            "5000,100%".to_owned(),
            "-w".to_owned(),
            "3000,80%".to_owned(),
        ]
        .into_iter()
        .map(Json::String)
        .collect(),
    )
}

/// A stable, plausible execution time per check command (seconds).
pub(crate) fn execution_time(check_command: &str) -> f64 {
    let hash = check_command
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |acc, b| {
            (acc ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
    #[expect(
        clippy::cast_precision_loss,
        reason = "a value below 2000 converts exactly"
    )]
    let millis = (hash % 1_900) as f64;
    0.02 + millis / 1000.0
}
