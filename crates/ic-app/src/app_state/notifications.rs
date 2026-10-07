//! Notifications in the state (NOTE-02..07): the notification centre's
//! list (the local log's recent notifications, then every new one, silent
//! or not), read marks, pausing (app-wide: a pause carries over to the
//! next environment's engine), watching and muting objects, the
//! notification settings and the app-wide settings.
//!
//! Everything here is client-side (PLAN.md D5, D6): rules, mutes and
//! pauses never touch Icinga's own notification switches.

use futures::channel::oneshot;
use ic_config::General;
use ic_core::{Command, LogEntry, NotificationRecord};
use ic_model::{ObjectKey, Timestamp};
use ic_rules::{NotificationSettings, ObjectMode, ObjectOverride, ScopeSetting};

use super::{AppState, MAX_NOTIFICATIONS};

/// The notification settings of one environment as the settings dialog
/// edits them: the environment's own (rule, quiet hours, storm control,
/// watched and muted objects) and every group's and dashboard's setting.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NotificationPlan {
    /// The environment it was read from: it applies to that one only.
    pub(crate) environment_id: String,
    /// The environment's settings.
    pub(crate) settings: NotificationSettings,
    /// Each group's setting, in sidebar order.
    pub(crate) groups: Vec<GroupPlan>,
}

/// A group's notification setting and its dashboards'.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupPlan {
    /// The group's id.
    pub(crate) id: String,
    /// The group's name.
    pub(crate) name: String,
    /// Its setting.
    pub(crate) setting: ScopeSetting,
    /// Its dashboards: id, name, setting.
    pub(crate) dashboards: Vec<(String, String, ScopeSetting)>,
}

impl AppState {
    /// The active environment's recent notifications, newest first.
    #[cfg(test)]
    pub(crate) fn notification_records(&self) -> impl Iterator<Item = &NotificationRecord> {
        self.engine.notifications()
    }

    /// The active environment's notifications not seen yet, silent ones
    /// included.
    pub(crate) fn unread_notifications(&self) -> usize {
        self.engine.unread()
    }

    /// Asks environment `id`'s engine for its log's recent notifications
    /// (when it starts). `None` without its engine.
    pub(crate) fn request_notifications_of(
        &self,
        id: &str,
    ) -> Option<oneshot::Receiver<Vec<NotificationRecord>>> {
        let core = self.slot(id)?.core.as_ref()?;
        let (reply, receiver) = oneshot::channel();
        core.send(Command::LoadNotifications {
            limit: MAX_NOTIFICATIONS,
            reply,
        });
        Some(receiver)
    }

    /// Asks the active environment's engine for its log's recent
    /// notifications. `None` without a core.
    #[cfg(test)]
    pub(crate) fn request_notifications(
        &self,
    ) -> Option<oneshot::Receiver<Vec<NotificationRecord>>> {
        let id = self.active_environment_id()?.to_owned();
        self.request_notifications_of(&id)
    }

    /// Takes environment `id`'s log's recent notifications: merged with
    /// those that arrived meanwhile (by id; read if either says so),
    /// newest first.
    pub(crate) fn load_notifications_of(&mut self, id: &str, records: Vec<NotificationRecord>) {
        if let Some(slot) = self.slot_mut(id) {
            merge_notifications(&mut slot.notifications, records);
        }
    }

    /// [`AppState::load_notifications_of`] for the active environment.
    #[cfg(test)]
    pub(crate) fn load_notifications(&mut self, records: Vec<NotificationRecord>) {
        merge_notifications(&mut self.engine.notifications, records);
    }

    /// Marks one of the active environment's notifications read. Returns
    /// whether it was unread.
    #[cfg(test)]
    pub(crate) fn mark_notification_read(&mut self, id: &str) -> bool {
        self.engine.mark_read(id)
    }

    /// Marks notification `tag` of environment `environment` read (a
    /// desktop notification clicked). Returns whether it was unread.
    pub(crate) fn mark_notification_read_in(&mut self, environment: &str, tag: &str) -> bool {
        self.slot_mut(environment)
            .is_some_and(|slot| slot.mark_read(tag))
    }

    /// Environment `id`'s recent notifications, newest first (none for an
    /// environment whose engine hasn't reported).
    pub(crate) fn notification_records_of(&self, id: &str) -> Vec<&NotificationRecord> {
        self.slot(id)
            .map(|slot| slot.notifications().collect())
            .unwrap_or_default()
    }

    /// Environment `id`'s notifications not seen yet (A3: the switcher and
    /// the centre's scopes show the other environments').
    pub(crate) fn unread_in(&self, id: &str) -> usize {
        self.slot(id).map_or(0, super::engines::EngineSlot::unread)
    }

    /// Marks notifications of environment `environment` read, as the
    /// centre's *mark all read* does for what it shows: all of them, also
    /// in its log (`ids` `None`), or those listed. Returns whether any was
    /// unread.
    pub(crate) fn mark_read_in(&mut self, environment: &str, ids: Option<&[String]>) -> bool {
        let Some(slot) = self.slot_mut(environment) else {
            return false;
        };
        let mut changed = false;
        if let Some(ids) = ids {
            for id in ids {
                changed |= slot.mark_read(id);
            }
        } else {
            for record in &mut slot.notifications {
                changed |= !record.read;
                record.read = true;
            }
            if changed {
                slot.send(Command::MarkNotificationsRead);
            }
        }
        changed
    }

    /// Marks the active environment's notifications about `object` read:
    /// it was opened, so they have been seen (A2). Returns whether any was
    /// unread.
    pub(crate) fn mark_object_read(&mut self, object: &ObjectKey) -> bool {
        let ids: Vec<String> = self
            .engine
            .notifications()
            .filter(|record| !record.read && record.intent.object.as_ref() == Some(object))
            .map(|record| record.intent.id.clone())
            .collect();
        let mut changed = false;
        for id in &ids {
            changed |= self.engine.mark_read(id);
        }
        changed
    }

    /// Marks every notification of the active environment read. Returns
    /// whether any was unread.
    pub(crate) fn mark_all_notifications_read(&mut self) -> bool {
        let mut changed = false;
        for record in &mut self.engine.notifications {
            changed |= !record.read;
            record.read = true;
        }
        if changed {
            self.send(Command::MarkNotificationsRead);
        }
        changed
    }

    /// Asks the core for the local event log's newest `limit` entries of
    /// `object` (a host's include its services'). `None` without a core.
    pub(crate) fn load_history(
        &self,
        object: ObjectKey,
        limit: usize,
    ) -> Option<oneshot::Receiver<Vec<LogEntry>>> {
        let (reply, receiver) = oneshot::channel();
        #[cfg(test)]
        if let Some(history) = &self.fake_history {
            let entries = history
                .iter()
                .filter(|entry| match &object {
                    ObjectKey::Host { name } => entry.object.host_name() == name,
                    service @ ObjectKey::Service { .. } => entry.object == *service,
                })
                .take(limit)
                .cloned()
                .collect();
            let _ = reply.send(entries);
            return Some(receiver);
        }
        let core = self.engine.core.as_ref()?;
        core.send(Command::LoadHistory {
            object: Some(object),
            limit,
            reply,
        });
        Some(receiver)
    }

    /// Asks the core when the event log's oldest entry happened. `None`
    /// without a core.
    pub(crate) fn load_history_start(&self) -> Option<oneshot::Receiver<Option<Timestamp>>> {
        let (reply, receiver) = oneshot::channel();
        #[cfg(test)]
        if let Some(history) = &self.fake_history {
            let oldest = history
                .iter()
                .map(|entry| entry.at)
                .min_by(|a, b| a.as_unix_seconds().total_cmp(&b.as_unix_seconds()));
            let _ = reply.send(oldest);
            return Some(receiver);
        }
        let core = self.engine.core.as_ref()?;
        core.send(Command::LoadHistoryStart { reply });
        Some(receiver)
    }

    /// Until when notifications are paused in every environment (`None`:
    /// not paused).
    pub(crate) fn paused_until(&self) -> Option<Timestamp> {
        self.paused_until
    }

    /// Whether notifications are paused in every environment at `now`.
    #[cfg(test)]
    pub(crate) fn is_paused(&self, now: Timestamp) -> bool {
        self.paused_until.is_some_and(|until| until > now)
    }

    /// Until when environment `id`'s own pause runs, at `now` (`None`:
    /// none, or over).
    pub(crate) fn environment_paused_until(&self, id: &str, now: Timestamp) -> Option<Timestamp> {
        self.environment_pauses
            .get(id)
            .copied()
            .filter(|until| *until > now)
    }

    /// Until when environment `id`'s notifications are paused at `now`:
    /// the later of the pause of every environment and its own (`None`:
    /// they notify).
    pub(crate) fn effective_pause(&self, id: &str, now: Timestamp) -> Option<Timestamp> {
        let global = self.paused_until.filter(|until| *until > now);
        let own = self.environment_paused_until(id, now);
        match (global, own) {
            (Some(global), Some(own)) => {
                Some(if own.as_unix_seconds() > global.as_unix_seconds() {
                    own
                } else {
                    global
                })
            }
            (pause, None) | (None, pause) => pause,
        }
    }

    /// Until when the active environment's notifications are paused at
    /// `now` (by the pause of every environment, or its own).
    pub(crate) fn active_pause(&self, now: Timestamp) -> Option<Timestamp> {
        self.active_environment_id().map_or_else(
            || self.paused_until.filter(|until| *until > now),
            |id| self.effective_pause(id, now),
        )
    }

    /// Pauses notifications in every environment until `until`, or
    /// resumes them (`None`; an environment's own pause still holds). The
    /// pause shows at once and every engine, also those started later,
    /// gets it.
    pub(crate) fn pause_notifications(&mut self, until: Option<Timestamp>) {
        tracing::info!(
            ?until,
            "notifications paused by the user (every environment)"
        );
        self.paused_until = until;
        self.send_pauses();
    }

    /// Pauses environment `id`'s notifications until `until`, or resumes
    /// them (`None`; the pause of every environment still holds). Returns
    /// whether that changed anything (an unknown environment changes
    /// nothing).
    pub(crate) fn pause_environment(&mut self, id: &str, until: Option<Timestamp>) -> bool {
        if self.config.environment(id).is_none() {
            return false;
        }
        let before = self.environment_pauses.get(id).copied();
        match until {
            Some(until) => {
                self.environment_pauses.insert(id.to_owned(), until);
            }
            None => {
                self.environment_pauses.remove(id);
            }
        }
        if before == until {
            return false;
        }
        tracing::info!(environment = %id, ?until, "an environment's notifications paused by the user");
        let pause = self.effective_pause(id, Timestamp::now());
        if let Some(slot) = self.slot(id) {
            slot.send(Command::PauseNotifications(pause));
        }
        true
    }

    /// Gives every engine the pause in force for its environment.
    fn send_pauses(&self) {
        let now = Timestamp::now();
        for environment in &self.config.environments {
            if let Some(slot) = self.slot(&environment.id) {
                slot.send(Command::PauseNotifications(
                    self.effective_pause(&environment.id, now),
                ));
            }
        }
    }

    /// Gives a new engine of the active environment the pause still
    /// running for it.
    #[cfg(test)]
    pub(super) fn resend_pause(&self) {
        let pause = self
            .active_environment_id()
            .and_then(|id| self.effective_pause(id, Timestamp::now()));
        if let Some(until) = pause {
            self.send(Command::PauseNotifications(Some(until)));
        }
    }

    /// Forgets the pauses that are over at `now`.
    pub(super) fn expire_pauses(&mut self, now: Timestamp) {
        if self.paused_until.is_some_and(|until| until <= now) {
            self.paused_until = None;
        }
        self.environment_pauses.retain(|_, until| *until > now);
    }

    /// The watch or mute in force on `object` at `now` (an expired mute
    /// is none).
    pub(crate) fn object_override(
        &self,
        object: &ObjectKey,
        now: Timestamp,
    ) -> Option<&ObjectOverride> {
        self.environment()?
            .notifications
            .objects
            .iter()
            .find(|entry| entry.object == *object && is_current(entry, now))
    }

    /// Watches (`Watch`, no end) or mutes (`Mute`, until `until`, `None`:
    /// until unmuted) `objects`, replacing what they had; expired entries
    /// are dropped. Returns whether anything changed.
    pub(crate) fn set_object_override(
        &mut self,
        objects: &[ObjectKey],
        mode: ObjectMode,
        until: Option<Timestamp>,
        now: Timestamp,
    ) -> bool {
        if objects.is_empty() {
            return false;
        }
        let until = match mode {
            ObjectMode::Watch => None,
            ObjectMode::Mute => until,
        };
        self.change_environment(|environment| {
            let list = &mut environment.notifications.objects;
            let before = list.clone();
            list.retain(|entry| is_current(entry, now) && !objects.contains(&entry.object));
            list.extend(objects.iter().map(|object| ObjectOverride {
                object: object.clone(),
                mode,
                until,
            }));
            (*list != before).then_some(())
        })
        .is_some()
    }

    /// Removes the watch or mute of `objects` (and expired entries).
    /// Returns whether anything changed.
    pub(crate) fn remove_object_override(&mut self, objects: &[ObjectKey], now: Timestamp) -> bool {
        self.change_environment(|environment| {
            let list = &mut environment.notifications.objects;
            let before = list.len();
            list.retain(|entry| is_current(entry, now) && !objects.contains(&entry.object));
            (list.len() != before).then_some(())
        })
        .is_some()
    }

    /// The active environment's notification settings as the settings
    /// dialog edits them.
    pub(crate) fn notification_plan(&self) -> Option<NotificationPlan> {
        let environment = self.environment()?;
        Some(NotificationPlan {
            environment_id: environment.id.clone(),
            settings: environment.notifications.clone(),
            groups: environment
                .groups
                .iter()
                .map(|group| GroupPlan {
                    id: group.id.clone(),
                    name: group.name.clone(),
                    setting: group.notifications.clone(),
                    dashboards: group
                        .dashboards
                        .iter()
                        .map(|dashboard| {
                            (
                                dashboard.id.clone(),
                                dashboard.name.clone(),
                                dashboard.notifications.clone(),
                            )
                        })
                        .collect(),
                })
                .collect(),
        })
    }

    /// Saves the notification settings from the settings dialog (groups
    /// and dashboards deleted meanwhile are skipped). A plan read from
    /// another environment than the active one changes nothing: another
    /// environment's rules and mutes must never overwrite these. Returns
    /// whether anything changed.
    pub(crate) fn apply_notification_plan(&mut self, plan: NotificationPlan) -> bool {
        if self.active_environment_id() != Some(plan.environment_id.as_str()) {
            tracing::warn!(
                plan = %plan.environment_id,
                active = ?self.active_environment_id(),
                "notification settings of another environment were not applied"
            );
            return false;
        }
        self.change_environment(|environment| {
            let mut changed = environment.notifications != plan.settings;
            environment.notifications = plan.settings;
            for planned in plan.groups {
                let Some(group) = environment.group_mut(&planned.id) else {
                    continue;
                };
                changed |= group.notifications != planned.setting;
                group.notifications = planned.setting;
                for (id, _, setting) in planned.dashboards {
                    if let Some(dashboard) = group
                        .dashboards
                        .iter_mut()
                        .find(|dashboard| dashboard.id == id)
                    {
                        changed |= dashboard.notifications != setting;
                        dashboard.notifications = setting;
                    }
                }
            }
            changed.then_some(())
        })
        .is_some()
    }

    /// Takes new app-wide settings (keep running in the tray, launch at
    /// login, event log retention, reconcile interval, quiet mode): saved,
    /// and every environment's engine told (and, for quiet mode, whether
    /// it is quiet now). Returns whether they changed.
    pub(crate) fn set_general(&mut self, general: &General) -> bool {
        if self.config.general == *general {
            return false;
        }
        let quiet_changed = self.config.general.quiet_when_hidden != general.quiet_when_hidden;
        self.config.general = general.clone();
        self.save_config();
        self.send_to_every_engine(|| Command::UpdateGeneral(general.clone()));
        if quiet_changed {
            self.announce_quiet();
        }
        true
    }
}

#[cfg(all(test, target_os = "linux"))]
impl AppState {
    /// Answers history queries from `entries` (newest first) instead of a
    /// core's log.
    pub(crate) fn set_fake_history(&mut self, entries: Vec<LogEntry>) {
        self.fake_history = Some(entries);
    }
}

/// Merges `records` (a log's recent notifications) into `known`: by id
/// (read if either says so), newest first, at most `MAX_NOTIFICATIONS`.
fn merge_notifications(
    known: &mut std::collections::VecDeque<NotificationRecord>,
    records: Vec<NotificationRecord>,
) {
    for record in records {
        match known
            .iter_mut()
            .find(|existing| existing.intent.id == record.intent.id)
        {
            Some(existing) => existing.read |= record.read,
            None => known.push_back(record),
        }
    }
    known.make_contiguous().sort_by(|a, b| {
        b.intent
            .at
            .as_unix_seconds()
            .total_cmp(&a.intent.at.as_unix_seconds())
    });
    known.truncate(MAX_NOTIFICATIONS);
}

/// Whether an override is still in force at `now`.
fn is_current(entry: &ObjectOverride, now: Timestamp) -> bool {
    entry.until.is_none_or(|until| until > now)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use ic_rules::{NotificationIntent, Rule, Tone};

    use super::*;
    use crate::app_state::testing::Recorder;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_790_000_000.)
    }

    fn record(id: &str, at: f64, read: bool) -> NotificationRecord {
        NotificationRecord {
            intent: NotificationIntent {
                id: id.to_owned(),
                object: Some(ObjectKey::service("db-prod-03", "postgres-replication")),
                title: format!("CRITICAL · {id}"),
                subtitle: "databases / production".to_owned(),
                body: "replication lag 412s".to_owned(),
                tone: Tone::Critical,
                sound: true,
                silent: false,
                silenced: None,
                at: Timestamp::from_unix_seconds(at),
            },
            read,
        }
    }

    fn connected() -> (AppState, Recorder) {
        let mut state = AppState::fixture(now());
        let recorder = Recorder::default();
        state.set_core(Box::new(recorder.clone()));
        (state, recorder)
    }

    #[test]
    fn the_log_and_live_notifications_merge_newest_first() {
        let (mut state, recorder) = connected();
        // A live one arrives before the log answers, and is also in it.
        state.apply(ic_core::CoreEvent::Notification(record("live", 30., false)));
        assert!(state.request_notifications().is_some());
        assert_eq!(
            recorder.sent(),
            ["LoadNotifications { limit: 200, reply: Sender { complete: false } }"]
        );
        state.load_notifications(vec![
            record("live", 30., true),
            record("older", 10., false),
            record("old-read", 20., true),
        ]);
        let ids: Vec<&str> = state
            .notification_records()
            .map(|record| record.intent.id.as_str())
            .collect();
        assert_eq!(ids, ["live", "old-read", "older"]);
        assert_eq!(
            state.unread_notifications(),
            1,
            "the log knew `live` was read"
        );
    }

    #[test]
    fn read_marks_go_to_the_log() {
        let (mut state, recorder) = connected();
        state.load_notifications(vec![record("a", 10., false), record("b", 20., false)]);
        assert_eq!(state.unread_notifications(), 2);
        assert!(state.mark_notification_read("a"));
        assert!(!state.mark_notification_read("a"), "already read");
        assert!(!state.mark_notification_read("nope"));
        assert_eq!(state.unread_notifications(), 1);
        assert!(state.mark_all_notifications_read());
        assert!(!state.mark_all_notifications_read(), "nothing left");
        assert_eq!(state.unread_notifications(), 0);
        assert_eq!(
            recorder.sent(),
            ["MarkNotificationRead(\"a\")", "MarkNotificationsRead"]
        );
    }

    #[test]
    fn a_pause_shows_at_once_and_carries_over_to_the_next_engine() {
        let (mut state, recorder) = connected();
        let until = Timestamp::from_unix_seconds(Timestamp::now().as_unix_seconds() + 1800.);
        state.pause_notifications(Some(until));
        assert_eq!(state.paused_until(), Some(until));
        assert!(state.is_paused(Timestamp::now()));
        // Another environment's engine gets the pause when it starts.
        state.reset_connection();
        assert_eq!(state.paused_until(), Some(until), "the pause is app-wide");
        let next = Recorder::default();
        state.set_core(Box::new(next.clone()));
        assert_eq!(next.sent().len(), 1);
        assert!(
            next.sent()[0].starts_with("PauseNotifications(Some("),
            "{:?}",
            next.sent()
        );
        state.pause_notifications(None);
        assert!(!state.is_paused(Timestamp::now()));
        assert!(recorder.sent()[0].starts_with("PauseNotifications(Some("));
    }

    #[test]
    fn objects_are_watched_and_muted_with_expiry() {
        let (mut state, recorder) = connected();
        let replication = ObjectKey::service("db-prod-03", "postgres-replication");
        let host = ObjectKey::host("db-prod-03");
        let in_an_hour = Timestamp::from_unix_seconds(now().as_unix_seconds() + 3600.);
        assert!(state.set_object_override(
            std::slice::from_ref(&replication),
            ObjectMode::Mute,
            Some(in_an_hour),
            now()
        ));
        assert_eq!(
            state
                .object_override(&replication, now())
                .map(|entry| entry.mode),
            Some(ObjectMode::Mute)
        );
        // Expired: none in force; it's dropped with the next change.
        let later = Timestamp::from_unix_seconds(in_an_hour.as_unix_seconds() + 1.);
        assert!(state.object_override(&replication, later).is_none());
        assert!(state.set_object_override(
            std::slice::from_ref(&host),
            ObjectMode::Watch,
            Some(in_an_hour),
            later
        ));
        let objects = &state.environment().unwrap().notifications.objects;
        assert_eq!(objects.len(), 1, "{objects:?}");
        assert_eq!(objects[0].object, host);
        assert_eq!(objects[0].until, None, "a watch has no end");
        // Unchanged: nothing sent.
        recorder.clear();
        assert!(!state.set_object_override(
            std::slice::from_ref(&host),
            ObjectMode::Watch,
            None,
            later
        ));
        assert!(recorder.sent().is_empty());
        assert!(state.remove_object_override(std::slice::from_ref(&host), later));
        assert!(!state.remove_object_override(std::slice::from_ref(&host), later));
        assert!(
            state
                .environment()
                .unwrap()
                .notifications
                .objects
                .is_empty()
        );
        assert_eq!(recorder.sent(), ["UpdateEnvironment(prod-cluster)"]);
    }

    #[test]
    fn the_notification_plan_round_trips_and_skips_what_is_gone() {
        let (mut state, recorder) = connected();
        let mut plan = state.notification_plan().unwrap();
        assert_eq!(plan.groups.len(), state.groups().len());
        assert!(!state.apply_notification_plan(plan.clone()), "unchanged");
        plan.settings.quiet_hours.enabled = true;
        plan.settings.default_rule.min_duration_secs = 300;
        plan.groups[0].setting = ScopeSetting::Off;
        let custom = Rule {
            hard_only: false,
            ..Rule::default()
        };
        plan.groups[0].dashboards[0].2 = ScopeSetting::Custom(custom.clone());
        plan.groups.push(GroupPlan {
            id: "deleted-meanwhile".to_owned(),
            name: "gone".to_owned(),
            setting: ScopeSetting::On,
            dashboards: Vec::new(),
        });
        // Read from another environment: never applied here.
        let mut elsewhere = plan.clone();
        elsewhere.environment_id = "staging".to_owned();
        assert!(!state.apply_notification_plan(elsewhere));
        assert!(
            !state
                .environment()
                .unwrap()
                .notifications
                .quiet_hours
                .enabled
        );
        assert!(state.apply_notification_plan(plan));
        let environment = state.environment().unwrap();
        assert!(environment.notifications.quiet_hours.enabled);
        assert_eq!(
            environment.notifications.default_rule.min_duration_secs,
            300
        );
        assert_eq!(environment.groups[0].notifications, ScopeSetting::Off);
        assert_eq!(
            environment.groups[0].dashboards[0].notifications,
            ScopeSetting::Custom(custom)
        );
        assert_eq!(recorder.sent(), ["UpdateEnvironment(prod-cluster)"]);
    }

    #[test]
    fn general_settings_go_to_the_engine() {
        let (mut state, recorder) = connected();
        let general = General {
            event_log_retention_hours: 72,
            ..state.config().general.clone()
        };
        assert!(state.set_general(&general));
        assert!(!state.set_general(&general), "unchanged");
        assert_eq!(state.config().general.event_log_retention_hours, 72);
        let sent = recorder.sent();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].starts_with("UpdateGeneral("), "{sent:?}");
        let _ = Rc::new(RefCell::new(()));
    }

    #[test]
    fn history_comes_from_the_log_through_the_core() {
        let (state, recorder) = connected();
        assert!(
            state
                .load_history(ObjectKey::host("db-prod-03"), 50)
                .is_some()
        );
        assert!(state.load_history_start().is_some());
        let sent = recorder.sent();
        assert!(sent[0].starts_with("LoadHistory {"), "{sent:?}");
        assert!(sent[1].starts_with("LoadHistoryStart {"), "{sent:?}");
        let idle = AppState::empty();
        assert!(idle.load_history(ObjectKey::host("h"), 1).is_none());
        assert!(idle.load_history_start().is_none());
    }
}
