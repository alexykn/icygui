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

/// What an event about a host or service changes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Writes {
    /// The check state: a check result, a state change, flapping.
    Check,
    /// Only how it is handled: an acknowledgement.
    Handling,
}

/// What a check event does to the acknowledgement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AckFrom {
    /// Icinga's value when it built the event (`IsAcknowledged`).
    Carried(AckKind),
    /// The event doesn't carry it: a state change ends a normal
    /// acknowledgement, a recovery a sticky one (`ProcessCheckResult`).
    Inferred,
    /// An acknowledgement event newer than the check event (by Icinga's
    /// time) was applied: the stored value stays.
    Older,
}

/// Which downtime event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DowntimeChange {
    Added,
    Started,
    Triggered,
    Removed,
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
            } if !self.overtaken(object, seq, result_time(result, *at)) => {
                let depth = self.line_depth(object, *downtime_depth, *at);
                let ack = self.line_ack(object, *acknowledgement, *at);
                let applied = self.on_object(object, seq, Writes::Check, |target| {
                    check_result(target, result, *after, depth, ack, *at);
                });
                self.depth_from_line(object, seq, depth, *at, &applied);
                applied
            }
            Event::StateChange {
                object,
                state,
                state_type,
                result,
                downtime_depth,
                acknowledgement,
                at,
            } if !self.overtaken(object, seq, result_time(result, *at)) => {
                let depth = self.line_depth(object, *downtime_depth, *at);
                let ack = self.line_ack(object, *acknowledgement, *at);
                let applied = self.on_object(object, seq, Writes::Check, |target| {
                    state_change(target, *state, *state_type, result, depth, ack, *at);
                });
                self.depth_from_line(object, seq, depth, *at, &applied);
                applied
            }
            Event::AcknowledgementSet {
                object,
                kind,
                expiry,
                at,
                ..
            } => {
                let applied = self.on_object(object, seq, Writes::Handling, |target| {
                    let check = target.check();
                    check.acknowledgement = if kind.is_acknowledged() {
                        *kind
                    } else {
                        AckKind::Normal
                    };
                    check.acknowledgement_expiry = *expiry;
                });
                self.ack_line(object, *at, &applied);
                applied
            }
            Event::AcknowledgementCleared { object, at } => {
                let applied = self.on_object(object, seq, Writes::Handling, |target| {
                    let check = target.check();
                    check.acknowledgement = AckKind::None;
                    check.acknowledgement_expiry = None;
                });
                self.ack_line(object, *at, &applied);
                applied
            }
            Event::Flapping {
                object,
                flapping,
                current,
                at,
            } if !self.overtaken(object, seq, *at) => {
                self.on_object(object, seq, Writes::Check, |target| {
                    let check = target.check();
                    check.flapping = *flapping;
                    check.flapping_current = *current;
                })
            }
            Event::CommentAdded { comment, .. } => self.on_comment(seq, comment, true),
            Event::CommentRemoved { comment, .. } => self.on_comment(seq, comment, false),
            Event::DowntimeAdded { downtime, at } => {
                self.on_downtime(seq, DowntimeChange::Added, downtime, *at)
            }
            Event::DowntimeStarted { downtime, at } => {
                self.on_downtime(seq, DowntimeChange::Started, downtime, *at)
            }
            Event::DowntimeTriggered { downtime, at } => {
                self.on_downtime(seq, DowntimeChange::Triggered, downtime, *at)
            }
            Event::DowntimeRemoved { downtime, at } => {
                self.on_downtime(seq, DowntimeChange::Removed, downtime, *at)
            }
            Event::ObjectLifecycle { .. } | Event::Notification { .. } => Applied::Ignored,
            // Overtaken by a query answer.
            Event::CheckResult { .. } | Event::StateChange { .. } | Event::Flapping { .. } => {
                Applied::Stale
            }
        }
    }

    /// Whether an event about `key` from a check that ended at `end` (or,
    /// for flapping, was processed then), read as line `seq`, is older
    /// than the query answer the store's check state of `key` came from.
    ///
    /// A query sent before the line was read can be answered before it is
    /// read: the stream is slow, or holds its lines for Icinga's next
    /// batch, while the answer comes on a connection of its own. Icinga
    /// composed the answer after it processed the event then, so the
    /// answer shows a later check (`last_check` after `end`; Icinga
    /// processes the results of a check in the order they started and
    /// drops older ones, and checks of one object don't overlap). Applied
    /// on top, the event would take the object back to an older state (a
    /// hard problem soft again) and the next events forward again: two
    /// state changes in the log, and rule inputs, for one. Such an event
    /// is [`Applied::Stale`], judged by its own check's time, until a
    /// check event that is not older is applied: from then on events
    /// wrote the check state last and are applied in order again.
    ///
    /// Only an event that writes the check state ends this (`checked`):
    /// an acknowledgement or a downtime that came in between says nothing
    /// about the order of check results. Icinga sends the
    /// `AcknowledgementCleared` of a state change just before the change's
    /// check result, so it would otherwise let the older attempts of that
    /// very change through.
    fn overtaken(&self, key: &ObjectKey, seq: u64, end: Timestamp) -> bool {
        let Some(seqs) = self.seqs.get(key) else {
            return false;
        };
        // A check event wrote the check state last (events come in
        // Icinga's order), or the line is stale anyway.
        if seqs.checked > seqs.fetched || seq <= seqs.fetched {
            return false;
        }
        self.older_result(key, end)
    }

    /// Whether a check result of `key` that ended at `end` is older than
    /// the one the store holds (`last_check` is the end of the latest).
    /// Strictly older: a check's `CheckResult` and `StateChange` carry the
    /// same end, and the attempts of one problem can come within a
    /// millisecond (passive results, a burst).
    pub(crate) fn older_result(&self, key: &ObjectKey, end: Timestamp) -> bool {
        self.check(key)
            .and_then(|check| check.last_check)
            .is_some_and(|last| end.as_unix_seconds() < last.as_unix_seconds())
    }

    /// Changes a known host or service with `change`, unless the event is
    /// already reflected in the object's last query answer. `writes`: what
    /// the event changes.
    fn on_object(
        &mut self,
        key: &ObjectKey,
        seq: u64,
        writes: Writes,
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
        match writes {
            Writes::Check => seqs.checked = seqs.checked.max(seq),
            Writes::Handling => seqs.handled = seqs.handled.max(seq),
        }
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
    pub(crate) fn view_of(&self, key: &ObjectKey) -> Option<ObjectView> {
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

    /// The downtime depth a check event about `key` at Icinga time `at`
    /// carries, if the store takes it: not when a downtime event that
    /// changed the count is newer (by Icinga's time: in the overlap of a
    /// stream switch the new stream's check results can be older than the
    /// old stream's lines already applied; in one stream they never are).
    fn line_depth(&self, key: &ObjectKey, depth: Option<u32>, at: Timestamp) -> Option<u32> {
        depth.filter(|_| {
            self.seqs
                .get(key)
                .and_then(|seqs| seqs.downtime_at)
                .is_none_or(|changed| at.as_unix_seconds() >= changed.as_unix_seconds())
        })
    }

    /// What a check event about `key` at Icinga time `at` does to the
    /// acknowledgement (`carried`: the event's value). Like the depth: not
    /// when an acknowledgement event applied before is newer by Icinga's
    /// time. In the overlap of a stream switch the new stream's check
    /// result can be older than the old stream's acknowledgement line
    /// already applied (a quiet stream has no check results), and in one
    /// stream a check event built just before an acknowledgement changed
    /// can be written just after its event; either way the event's value
    /// is older than the stored one.
    fn line_ack(&self, key: &ObjectKey, carried: Option<AckKind>, at: Timestamp) -> AckFrom {
        let older = self
            .seqs
            .get(key)
            .and_then(|seqs| seqs.ack_at)
            .is_some_and(|changed| at.as_unix_seconds() < changed.as_unix_seconds());
        match carried {
            _ if older => AckFrom::Older,
            Some(kind) => AckFrom::Carried(kind),
            None => AckFrom::Inferred,
        }
    }

    /// An acknowledgement event about `key` at Icinga time `at` was read:
    /// check events older than it no longer write the acknowledgement
    /// ([`Store::line_ack`]). Also when the object's last answer already
    /// had the line (stale): the store's value is then the answer's, newer
    /// than the line, and a check event older than the line older still.
    fn ack_line(&mut self, key: &ObjectKey, at: Timestamp, applied: &Applied) {
        if matches!(applied, Applied::Unknown(_)) {
            return;
        }
        if let Some(seqs) = self.seqs.get_mut(key)
            && seqs
                .ack_at
                .is_none_or(|latest| at.as_unix_seconds() > latest.as_unix_seconds())
        {
            seqs.ack_at = Some(at);
        }
    }

    /// A downtime event at Icinga time `at` changed (or, for a line the
    /// answer already has, may have changed) `key`'s count: check events
    /// older than it no longer bring theirs ([`Store::line_depth`]).
    fn downtime_line(&mut self, key: &ObjectKey, at: Timestamp) {
        let seqs = self.seqs.entry(key.clone()).or_default();
        if seqs
            .downtime_at
            .is_none_or(|latest| at.as_unix_seconds() > latest.as_unix_seconds())
        {
            seqs.downtime_at = Some(at);
        }
    }

    /// A check event (line `seq`, Icinga time `at`) that brought Icinga's
    /// count (`depth`) was applied: the depth is counted from it now, and
    /// the downtime events after it change it by what they change in
    /// Icinga's count (see the module notes on downtime depths).
    fn depth_from_line(
        &mut self,
        key: &ObjectKey,
        seq: u64,
        depth: Option<u32>,
        at: Timestamp,
        applied: &Applied,
    ) {
        if !matches!(applied, Applied::Changed { .. }) {
            return;
        }
        if depth.is_none() {
            // A state change that doesn't bring the count: a provisional
            // depth of a new problem is asked for.
            self.recheck_unsettled(key);
            return;
        }
        // Applied: `on_object` made the entry.
        if let Some(seqs) = self.seqs.get_mut(key) {
            seqs.depth = seqs.depth.max(seq);
            seqs.depth_line = Some(at);
        }
        // (Most check events find both empty.)
        if !self.counted.is_empty() {
            self.counted.remove(key);
        }
        self.settle(key);
    }

    /// Adds, updates or removes a downtime, and keeps the object's
    /// `downtime_depth` in step with Icinga's count (unless the object's
    /// last answer was sent after the line was read, and so has it).
    fn on_downtime(
        &mut self,
        seq: u64,
        change: DowntimeChange,
        downtime: &Downtime,
        at: Timestamp,
    ) -> Applied {
        let object = &downtime.object;
        // The list's query and the object's may have been sent at
        // different times: each part is skipped only if its own answer
        // has the line.
        let listed = seq > self.annotations_fetched;
        let known = self.contains(object);
        let reflected = known && self.reflects(object, seq);
        let counts = known && !reflected;
        let stored = self.downtime_of(object, &downtime.name).cloned();
        if reflected && (downtime.in_effect || stored.as_ref().is_some_and(|d| d.in_effect)) {
            // The answer has the line, but its time still dates the count.
            self.downtime_line(object, at);
        }
        if !listed && !counts {
            return Applied::Stale;
        }
        if listed {
            let present = change != DowntimeChange::Removed;
            self.log_annotation(
                seq,
                &downtime.name,
                Annotation::Downtime(downtime.clone(), present),
            );
            let map = Arc::make_mut(&mut self.downtimes);
            if present {
                upsert_downtime(map, downtime.clone());
                if let Some(list) = map.get_mut(object) {
                    sort_downtimes(list);
                }
            } else {
                remove_named(map, object, &downtime.name, |d| &d.name);
            }
            self.changes.any = true;
            self.changes.objects.insert(object.clone());
        }
        let before = self.view_of(object);
        if counts {
            self.count_downtime(seq, change, downtime, stored.as_ref(), at);
        }
        Applied::Changed {
            before,
            after: self.view_of(object),
        }
    }

    /// Changes the depth of `downtime`'s object by what the event (line
    /// `seq`, Icinga time `at`) changes in Icinga's count. `stored`: the
    /// downtime as the store had it before the event.
    ///
    /// Per downtime, by name: whether the stored depth counts it now, and
    /// whether Icinga counts it after the event (in effect, and not
    /// removed). The first event about a downtime since the depth was taken
    /// from a check event judges what that count included by Icinga's rule
    /// at the check event's time ([`counted_at`]): a fixed downtime whose
    /// start passed before it was counted then, even though Icinga's start
    /// timer reports it up to 5 s later. Since an answer, that can't be
    /// known (it may already count a downtime whose event was read after
    /// the request): the depth changes by what the store had, and the
    /// object is asked for again by name.
    fn count_downtime(
        &mut self,
        seq: u64,
        change: DowntimeChange,
        downtime: &Downtime,
        stored: Option<&Downtime>,
        at: Timestamp,
    ) {
        let object = &downtime.object;
        let mut line = self.seqs.get(object).and_then(|seqs| seqs.depth_line);
        if change == DowntimeChange::Added
            && line
                .is_some_and(|base| base.as_unix_seconds() >= downtime.entry_time.as_unix_seconds())
        {
            // Icinga registers a new downtime with its object before it
            // sends `DowntimeAdded` (`Downtime::Start`): a check event built
            // in between, on another thread, counts it and can be written
            // first. A check event from after the downtime's entry may have
            // it, so the count can't be placed (rarely: the two fall in the
            // same moment): as from an answer.
            if let Some(seqs) = self.seqs.get_mut(object) {
                seqs.depth_line = None;
            }
            line = None;
        }
        let now = change != DowntimeChange::Removed && downtime.in_effect;
        let counted = self.counted.entry(object.clone()).or_default();
        let was = counted
            .get(&downtime.name)
            .copied()
            .unwrap_or_else(|| match line {
                Some(base) => counted_at(change, downtime, stored, base),
                None => stored.map_or(
                    change == DowntimeChange::Removed && downtime.in_effect,
                    |stored| stored.in_effect,
                ),
            });
        counted.insert(downtime.name.clone(), now);
        if !(was || now || downtime.in_effect) {
            // Never in effect: neither count changes.
            return;
        }
        let seqs = self.seqs.entry(object.clone()).or_default();
        seqs.evented = seqs.evented.max(seq);
        seqs.depth = seqs.depth.max(seq);
        self.downtime_line(object, at);
        if line.is_none() {
            self.unsettle(object);
        }
        if was == now {
            return;
        }
        if let Some(mut target) = self.target(object) {
            let check = target.check();
            check.downtime_depth = if now {
                check.downtime_depth.saturating_add(1)
            } else {
                check.downtime_depth.saturating_sub(1)
            };
        }
        self.changes.any = true;
        self.changes.objects.insert(object.clone());
    }
}

/// Whether Icinga's count at Icinga time `at` (when a check event carried
/// it) included the downtime of a `change` event, the first about that
/// downtime since then (`stored`: as the store had it before the event).
/// Icinga's `Downtime::IsInEffect`: a fixed downtime during its window, a
/// flexible one for `duration` after its trigger. A downtime the event
/// adds didn't exist then, and a trigger the event reports came after
/// (Icinga sends a check result's downtime triggers before the result).
fn counted_at(
    change: DowntimeChange,
    downtime: &Downtime,
    stored: Option<&Downtime>,
    at: Timestamp,
) -> bool {
    if change == DowntimeChange::Added {
        return false;
    }
    let downtime = stored.unwrap_or(downtime);
    let at = at.as_unix_seconds();
    if downtime.fixed {
        downtime.start_time.as_unix_seconds() <= at && at < downtime.end_time.as_unix_seconds()
    } else {
        change != DowntimeChange::Triggered
            && downtime
                .trigger_time
                .is_some_and(|trigger| at < trigger.as_unix_seconds() + downtime.duration)
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
/// from the event win when present (and the store takes them).
fn transition(
    target: &mut Target<'_>,
    new_state: CheckableState,
    new_type: StateType,
    end: Timestamp,
    acknowledgement: AckFrom,
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
        AckFrom::Older => {}
        AckFrom::Carried(AckKind::None) => {
            check.acknowledgement = AckKind::None;
            check.acknowledgement_expiry = None;
        }
        // Events only say "acknowledged": keep sticky.
        AckFrom::Carried(_) => {
            if !check.acknowledgement.is_acknowledged() {
                check.acknowledgement = AckKind::Normal;
            }
        }
        AckFrom::Inferred => {
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
    acknowledgement: AckFrom,
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
    acknowledgement: AckFrom,
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
