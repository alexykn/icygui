//! Applying events from the stream to the store, following what Icinga
//! itself does when it processes a check result
//! (`Checkable::ProcessCheckResult`), so no event needs a re-query.

use std::sync::Arc;
use std::time::Duration;

use ic_model::{
    AckKind, CheckInfo, CheckResult, CheckableState, Comment, Downtime, Event, Host, HostState,
    ObjectKey, Service, ServiceState, StateAfter, StateType, Timestamp,
};

use super::{
    Annotation, Store, remove_named, sort_comments, sort_downtimes, upsert_comment, upsert_downtime,
};

/// The parts of a host's or service's state the rule engine and the event
/// log care about, before or after an event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ObjectView {
    /// The state.
    pub(crate) state: CheckableState,
    /// Soft or hard.
    pub(crate) state_type: StateType,
    /// When the state began (`last_state_change`).
    pub(crate) since: Timestamp,
    /// Acknowledged.
    pub(crate) acknowledged: bool,
    /// Active downtimes.
    pub(crate) downtime_depth: u32,
    /// Flapping.
    pub(crate) flapping: bool,
    /// Reachable through its dependencies.
    pub(crate) reachable: bool,
}

impl ObjectView {
    /// The view of an object in `state` with `check`.
    pub(crate) fn of(state: CheckableState, check: &CheckInfo) -> Self {
        Self {
            state,
            state_type: check.state_type,
            since: check.last_state_change,
            acknowledged: check.acknowledgement.is_acknowledged(),
            downtime_depth: check.downtime_depth,
            flapping: check.flapping,
            reachable: check.reachable,
        }
    }
}

/// What applying one event did.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Applied {
    /// Applied. For events about a known host or service, its view before
    /// and after.
    Changed {
        /// Before the event.
        before: Option<ObjectView>,
        /// After it.
        after: Option<ObjectView>,
    },
    /// Already reflected by a query answered after the event was read:
    /// skipped.
    Stale,
    /// About a host or service the store doesn't know: re-query it.
    Unknown(ObjectKey),
    /// Not for the store: config object lifecycle, which the engine turns
    /// into re-queries.
    Ignored,
}

/// A host or service to change in place.
enum Target<'a> {
    Host(&'a mut Host),
    Service(&'a mut Service),
}

impl Target<'_> {
    fn state(&self) -> CheckableState {
        match self {
            Self::Host(host) => CheckableState::Host(host.state),
            Self::Service(service) => CheckableState::Service(service.state),
        }
    }

    fn check(&mut self) -> &mut CheckInfo {
        match self {
            Self::Host(host) => &mut host.check,
            Self::Service(service) => &mut service.check,
        }
    }

    fn view(&mut self) -> ObjectView {
        let state = self.state();
        ObjectView::of(state, self.check())
    }

    /// Sets the state; a state of the other kind (a host state for a
    /// service) is ignored.
    fn set_state(&mut self, state: CheckableState) {
        match (self, state) {
            (Self::Host(host), CheckableState::Host(state)) => host.state = state,
            (Self::Service(service), CheckableState::Service(state)) => service.state = state,
            _ => {}
        }
    }
}

impl Store {
    /// Applies one event read as line number `seq`. The caller records
    /// `last_event_at` per batch and handles `ObjectLifecycle` itself.
    pub(crate) fn apply(&mut self, seq: u64, event: &Event) -> Applied {
        match event {
            Event::CheckResult {
                object,
                result,
                downtime_depth,
                acknowledgement,
                after,
                at,
            } => self.on_object(object, seq, |target| {
                check_result(
                    target,
                    result,
                    *after,
                    *downtime_depth,
                    *acknowledgement,
                    *at,
                );
            }),
            Event::StateChange {
                object,
                state,
                state_type,
                result,
                downtime_depth,
                acknowledgement,
                at,
            } => self.on_object(object, seq, |target| {
                state_change(
                    target,
                    *state,
                    *state_type,
                    result,
                    *downtime_depth,
                    *acknowledgement,
                    *at,
                );
            }),
            Event::AcknowledgementSet {
                object,
                kind,
                expiry,
                ..
            } => self.on_object(object, seq, |target| {
                let check = target.check();
                check.acknowledgement = if kind.is_acknowledged() {
                    *kind
                } else {
                    AckKind::Normal
                };
                check.acknowledgement_expiry = *expiry;
            }),
            Event::AcknowledgementCleared { object, .. } => self.on_object(object, seq, |target| {
                let check = target.check();
                check.acknowledgement = AckKind::None;
                check.acknowledgement_expiry = None;
            }),
            Event::Flapping {
                object,
                flapping,
                current,
                ..
            } => self.on_object(object, seq, |target| {
                let check = target.check();
                check.flapping = *flapping;
                check.flapping_current = *current;
            }),
            Event::CommentAdded { comment, .. } => self.on_comment(seq, comment, true),
            Event::CommentRemoved { comment, .. } => self.on_comment(seq, comment, false),
            Event::DowntimeAdded { downtime, .. }
            | Event::DowntimeStarted { downtime, .. }
            | Event::DowntimeTriggered { downtime, .. } => self.on_downtime(seq, downtime, true),
            Event::DowntimeRemoved { downtime, .. } => self.on_downtime(seq, downtime, false),
            Event::ObjectLifecycle { .. } => Applied::Ignored,
        }
    }

    /// Changes a known host or service with `change`, unless the event is
    /// already reflected in the object's last query answer.
    fn on_object(
        &mut self,
        key: &ObjectKey,
        seq: u64,
        change: impl FnOnce(&mut Target<'_>),
    ) -> Applied {
        if !self.contains(key) {
            return Applied::Unknown(key.clone());
        }
        let seqs = self.seqs.entry(key.clone()).or_default();
        if seq <= seqs.fetched {
            return Applied::Stale;
        }
        seqs.evented = seqs.evented.max(seq);
        let Some(mut target) = self.target(key) else {
            return Applied::Unknown(key.clone());
        };
        let before = target.view();
        change(&mut target);
        let after = target.view();
        self.changes.any = true;
        self.changes.objects.insert(key.clone());
        Applied::Changed {
            before: Some(before),
            after: Some(after),
        }
    }

    /// The object for changing it, copying the map and the object if a
    /// snapshot still shares them.
    fn target(&mut self, key: &ObjectKey) -> Option<Target<'_>> {
        match key {
            ObjectKey::Host { name } => Arc::make_mut(&mut self.hosts)
                .get_mut(name)
                .map(|host| Target::Host(Arc::make_mut(host))),
            ObjectKey::Service { key } => Arc::make_mut(&mut self.services)
                .get_mut(key)
                .map(|service| Target::Service(Arc::make_mut(service))),
        }
    }

    /// The view of a known object.
    fn view_of(&self, key: &ObjectKey) -> Option<ObjectView> {
        match key {
            ObjectKey::Host { name } => self
                .hosts
                .get(name)
                .map(|host| ObjectView::of(CheckableState::Host(host.state), &host.check)),
            ObjectKey::Service { key } => self.services.get(key).map(|service| {
                ObjectView::of(CheckableState::Service(service.state), &service.check)
            }),
        }
    }

    fn log_annotation(&mut self, seq: u64, name: &str, annotation: Annotation) {
        if let Some(log) = &mut self.annotation_log {
            log.insert(name.to_owned(), (seq, annotation));
        }
    }

    fn on_comment(&mut self, seq: u64, comment: &Comment, present: bool) -> Applied {
        if seq <= self.annotations_fetched {
            return Applied::Stale;
        }
        self.log_annotation(
            seq,
            &comment.name,
            Annotation::Comment(comment.clone(), present),
        );
        let map = Arc::make_mut(&mut self.comments);
        if present {
            upsert_comment(map, comment.clone());
            if let Some(list) = map.get_mut(&comment.object) {
                sort_comments(list);
            }
        } else {
            remove_named(map, &comment.object, &comment.name, |c| &c.name);
        }
        self.changes.any = true;
        self.changes.objects.insert(comment.object.clone());
        let view = self.view_of(&comment.object);
        Applied::Changed {
            before: view,
            after: view,
        }
    }

    /// Adds, updates or removes a downtime, and keeps the object's
    /// `downtime_depth` (the number of downtimes in effect) in step: +1
    /// when one takes effect, −1 when one in effect is removed. Check
    /// results bring Icinga's own count again.
    fn on_downtime(&mut self, seq: u64, downtime: &Downtime, present: bool) -> Applied {
        if seq <= self.annotations_fetched {
            return Applied::Stale;
        }
        self.log_annotation(
            seq,
            &downtime.name,
            Annotation::Downtime(downtime.clone(), present),
        );
        let was_in_effect = self
            .downtimes
            .get(&downtime.object)
            .and_then(|list| list.iter().find(|stored| stored.name == downtime.name))
            .map(|stored| stored.in_effect);
        let map = Arc::make_mut(&mut self.downtimes);
        if present {
            upsert_downtime(map, downtime.clone());
            if let Some(list) = map.get_mut(&downtime.object) {
                sort_downtimes(list);
            }
        } else {
            remove_named(map, &downtime.object, &downtime.name, |d| &d.name);
        }
        self.changes.any = true;
        self.changes.objects.insert(downtime.object.clone());

        let delta: i8 = if present {
            match (was_in_effect.unwrap_or(false), downtime.in_effect) {
                (false, true) => 1,
                (true, false) => -1,
                _ => 0,
            }
        } else if was_in_effect.unwrap_or(false) || downtime.in_effect {
            -1
        } else {
            0
        };
        let before = self.view_of(&downtime.object);
        let fetched_after = self
            .seqs
            .get(&downtime.object)
            .is_none_or(|seqs| seq > seqs.fetched);
        if delta != 0 && before.is_some() && fetched_after {
            let seqs = self.seqs.entry(downtime.object.clone()).or_default();
            seqs.evented = seqs.evented.max(seq);
            if let Some(mut target) = self.target(&downtime.object) {
                let check = target.check();
                check.downtime_depth = if delta > 0 {
                    check.downtime_depth.saturating_add(1)
                } else {
                    check.downtime_depth.saturating_sub(1)
                };
            }
        }
        Applied::Changed {
            before,
            after: self.view_of(&downtime.object),
        }
    }
}

/// Whether Icinga counts the change from `old` to `new` as a state change
/// (it sets `last_state_change` then). For hosts only up vs. not up counts:
/// down and unreachable are the same raw state.
fn is_state_change(old: CheckableState, new: CheckableState) -> bool {
    let host_raw = |state: HostState| match state {
        HostState::Up => 0,
        HostState::Down | HostState::Unreachable => 1,
        HostState::Pending => 2,
    };
    match (old, new) {
        (CheckableState::Service(old), CheckableState::Service(new)) => old != new,
        (CheckableState::Host(old), CheckableState::Host(new)) => host_raw(old) != host_raw(new),
        _ => true,
    }
}

fn is_same_kind(a: CheckableState, b: CheckableState) -> bool {
    matches!(
        (a, b),
        (CheckableState::Host(_), CheckableState::Host(_))
            | (CheckableState::Service(_), CheckableState::Service(_))
    )
}

/// The state a result reports when the event has no `vars_after`: its
/// `state` (kept in `exit_status`); for hosts, 0 and 1 are up.
fn state_from_result(old: CheckableState, result: &CheckResult, reachable: bool) -> CheckableState {
    let code = u8::try_from(result.exit_status.clamp(0, 3)).unwrap_or(3);
    match old {
        CheckableState::Service(_) => {
            CheckableState::Service(ServiceState::from_code(code).unwrap_or(ServiceState::Unknown))
        }
        CheckableState::Host(_) => CheckableState::Host(match code {
            0 | 1 => HostState::Up,
            _ if reachable => HostState::Down,
            _ => HostState::Unreachable,
        }),
    }
}

/// When the result was produced: its execution end, else the event time.
fn result_time(result: &CheckResult, at: Timestamp) -> Timestamp {
    result.execution_end.non_zero().unwrap_or(at)
}

/// Moves `target` to `new_state`/`new_type` like `ProcessCheckResult`:
/// `last_state_change` on a state change, `last_hard_state_change` on a
/// hard change, a normal acknowledgement ends with any state change and a
/// sticky one with the recovery. `acknowledgement` and `downtime_depth`
/// from the event win when present.
fn transition(
    target: &mut Target<'_>,
    new_state: CheckableState,
    new_type: StateType,
    end: Timestamp,
    acknowledgement: Option<AckKind>,
    downtime_depth: Option<u32>,
) {
    let old_state = target.state();
    let check = target.check();
    let old_type = check.state_type;
    let changed = is_state_change(old_state, new_state);
    if changed {
        check.last_state_change = end;
    }
    let hard_change = (new_type == StateType::Hard && old_type == StateType::Soft)
        || (changed && old_type == StateType::Hard && new_type == StateType::Hard);
    if hard_change {
        check.last_hard_state_change = end;
    }
    check.state_type = new_type;
    match acknowledgement {
        Some(AckKind::None) => {
            check.acknowledgement = AckKind::None;
            check.acknowledgement_expiry = None;
        }
        // Events only say "acknowledged": keep sticky.
        Some(_) => {
            if !check.acknowledgement.is_acknowledged() {
                check.acknowledgement = AckKind::Normal;
            }
        }
        None => {
            let ends = match check.acknowledgement {
                AckKind::Normal => changed,
                AckKind::Sticky => changed && !new_state.is_problem(),
                AckKind::None => false,
            };
            if ends {
                check.acknowledgement = AckKind::None;
                check.acknowledgement_expiry = None;
            }
        }
    }
    if let Some(depth) = downtime_depth {
        check.downtime_depth = depth;
    }
    target.set_state(new_state);
}

/// Records `result` as the latest one and estimates the next check like
/// `Checkable::UpdateNextCheck`: the retry interval while soft, else the
/// check interval, counted from the result's end.
fn record_result(check: &mut CheckInfo, result: &CheckResult, end: Timestamp) {
    check.last_check = Some(end);
    check.result = Some(result.clone());
    let interval = if result.active && check.state_type == StateType::Soft {
        check.retry_interval
    } else {
        check.check_interval
    };
    check.next_check = Duration::try_from_secs_f64(interval)
        .ok()
        .map(|interval| end.plus(interval));
}

/// A `CheckResult` event: everything about the object's check state, from
/// the result and its `vars_after`.
fn check_result(
    target: &mut Target<'_>,
    result: &CheckResult,
    after: Option<StateAfter>,
    downtime_depth: Option<u32>,
    acknowledgement: Option<AckKind>,
    at: Timestamp,
) {
    let old_state = target.state();
    let reachable = after.map_or(target.check().reachable, |after| after.reachable);
    let after = after.filter(|after| is_same_kind(after.state, old_state));
    let new_state = after.map_or_else(
        || state_from_result(old_state, result, reachable),
        |after| after.state,
    );
    let new_type = after.map_or_else(
        || {
            if new_state.is_problem() {
                target.check().state_type
            } else {
                StateType::Hard
            }
        },
        |after| after.state_type,
    );
    let end = result_time(result, at);
    transition(
        target,
        new_state,
        new_type,
        end,
        acknowledgement,
        downtime_depth,
    );
    let check = target.check();
    if let Some(after) = after {
        check.attempt = after.attempt;
    }
    check.reachable = reachable;
    record_result(check, result, end);
}

/// A `StateChange` event (it follows the `CheckResult` of the same check,
/// which already brought most of it).
fn state_change(
    target: &mut Target<'_>,
    state: CheckableState,
    state_type: StateType,
    result: &CheckResult,
    downtime_depth: Option<u32>,
    acknowledgement: Option<AckKind>,
    at: Timestamp,
) {
    if !is_same_kind(state, target.state()) {
        return;
    }
    let end = result_time(result, at);
    transition(
        target,
        state,
        state_type,
        end,
        acknowledgement,
        downtime_depth,
    );
    let check = target.check();
    match state {
        CheckableState::Host(HostState::Down) => check.reachable = true,
        CheckableState::Host(HostState::Unreachable) => check.reachable = false,
        _ => {}
    }
    record_result(check, result, end);
}
