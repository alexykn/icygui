//! Changing the active environment's sidebar: groups (create, rename,
//! collapse, reorder, delete, notification setting; DASH-02), dashboards
//! (create, edit, rename, duplicate, move, reorder, delete, notification
//! setting; DASH-03) and sharing groups as files (DASH-06).
//!
//! Every change is saved and sent to the core (`UpdateEnvironment`, which
//! re-evaluates the dashboards and rebuilds the notification rules in
//! place), held back like view changes while the engine waits to
//! reconnect. Methods return what they changed, or `None`/`false` when
//! the group or dashboard doesn't exist (the sidebar may be a frame behind)
//! or the change is empty.

use ic_config::{Dashboard, DashboardGroup, Environment, View};
use ic_rules::{DashboardRef, ScopeSetting};

use super::AppState;

/// The name a new group gets until it's renamed.
pub(crate) const NEW_GROUP_NAME: &str = "new group";
/// `views`, each with an id (a fresh one where it had none).
fn with_ids(mut views: Vec<View>) -> Vec<View> {
    for view in &mut views {
        if view.id.trim().is_empty() {
            view.id = ic_config::new_id();
        }
    }
    views
}

/// The name a new dashboard gets until it's named.
pub(crate) const NEW_DASHBOARD_NAME: &str = "new dashboard";

/// A dashboard as the editor saves it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DashboardDraft {
    /// Display name (trimmed when saved; blank keeps the old name, or
    /// [`NEW_DASHBOARD_NAME`]).
    pub(crate) name: String,
    /// What it shows: its views, top to bottom (at least one).
    pub(crate) views: Vec<View>,
    /// Notification setting relative to its group.
    pub(crate) notifications: ScopeSetting,
    /// The group it goes into.
    pub(crate) group_id: String,
    /// What its sidebar row shows in the mark slot (topic 14, round 5).
    pub(crate) mark: ic_config::SidebarMark,
}

impl AppState {
    /// The icons picked most recently as sidebar marks (the icon
    /// picker's *recent* row), newest first.
    pub(crate) fn recent_icons(&self) -> &[String] {
        &self.ui.recent_icons
    }

    /// A saved dashboard's icon mark goes first in the recent icons.
    fn remember_mark(&mut self, mark: &ic_config::SidebarMark) {
        if let ic_config::SidebarMark::Icon(icon) = mark
            && self.ui.remember_icon(icon)
        {
            self.save_ui();
        }
    }

    /// The active environment's groups, in sidebar order.
    pub(crate) fn groups(&self) -> &[DashboardGroup] {
        self.environment()
            .map_or(&[][..], |environment| environment.groups.as_slice())
    }

    /// Runs `change` on the active environment; when it returns `Some`,
    /// the environment is saved and sent to the core.
    pub(super) fn change_environment<R>(
        &mut self,
        change: impl FnOnce(&mut Environment) -> Option<R>,
    ) -> Option<R> {
        let id = self.config.active_environment.clone()?;
        let environment = self.config.environment_mut(&id)?;
        let result = change(environment)?;
        self.repair_selection();
        #[cfg(test)]
        self.evaluate_fixture_all();
        self.environment_changed();
        Some(result)
    }

    /// [`AppState::change_environment`] for environment `id`, on screen or
    /// not: an environment off screen is saved and its own engine told.
    pub(super) fn change_environment_of<R>(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut Environment) -> Option<R>,
    ) -> Option<R> {
        if self.config.active_environment.as_deref() == Some(id) {
            return self.change_environment(change);
        }
        let environment = self.config.environment_mut(id)?;
        let result = change(environment)?;
        self.save_config();
        self.send_environment_to(id);
        Some(result)
    }

    /// After a change: a selected dashboard that's gone selects the first
    /// one (and the UI state remembers it).
    pub(super) fn repair_selection(&mut self) {
        let valid = self
            .selected
            .as_ref()
            .is_some_and(|reference| self.dashboard(reference).is_some());
        if !valid {
            self.selected = self.first_dashboard();
            self.remember_environment_ui();
        }
    }

    /// Adds a group named `name` (trimmed; blank is [`NEW_GROUP_NAME`])
    /// after the group `after`, or at the end. Returns its id.
    pub(crate) fn create_group(&mut self, name: &str, after: Option<&str>) -> Option<String> {
        let name = non_blank(name).unwrap_or(NEW_GROUP_NAME);
        self.change_environment(|environment| {
            let group = DashboardGroup::new(name);
            let id = group.id.clone();
            let index = after
                .and_then(|after| {
                    environment
                        .groups
                        .iter()
                        .position(|group| group.id == after)
                })
                .map_or(environment.groups.len(), |index| index + 1);
            environment.groups.insert(index, group);
            Some(id)
        })
    }

    /// Renames a group (`name` trimmed; blank names are refused). Returns
    /// whether it changed.
    pub(crate) fn rename_group(&mut self, group_id: &str, name: &str) -> bool {
        let Some(name) = non_blank(name) else {
            return false;
        };
        self.change_environment(|environment| {
            let group = environment.group_mut(group_id)?;
            (group.name != name).then(|| name.clone_into(&mut group.name))
        })
        .is_some()
    }

    /// Moves a group `delta` places up (negative) or down in the sidebar.
    /// Returns whether it moved.
    pub(crate) fn move_group(&mut self, group_id: &str, delta: isize) -> bool {
        self.change_environment(|environment| {
            let from = environment
                .groups
                .iter()
                .position(|group| group.id == group_id)?;
            let to = shifted(from, delta, environment.groups.len())?;
            let group = environment.groups.remove(from);
            environment.groups.insert(to, group);
            Some(())
        })
        .is_some()
    }

    /// Deletes a group and its dashboards (the sidebar asks first).
    /// Returns whether it existed.
    pub(crate) fn delete_group(&mut self, group_id: &str) -> bool {
        self.change_environment(|environment| {
            let index = environment
                .groups
                .iter()
                .position(|group| group.id == group_id)?;
            environment.groups.remove(index);
            Some(())
        })
        .is_some()
    }

    /// Sets a group's notification setting. Returns whether it changed.
    pub(crate) fn set_group_notifications(
        &mut self,
        group_id: &str,
        setting: ScopeSetting,
    ) -> bool {
        self.change_environment(|environment| {
            let group = environment.group_mut(group_id)?;
            (group.notifications != setting).then(|| group.notifications = setting)
        })
        .is_some()
    }

    /// Adds `draft` as a new dashboard at the end of its group and selects
    /// it. Returns its reference.
    pub(crate) fn add_dashboard(&mut self, draft: DashboardDraft) -> Option<DashboardRef> {
        let name = non_blank(&draft.name).unwrap_or(NEW_DASHBOARD_NAME);
        let mut dashboard = Dashboard::with_views(name, draft.views);
        dashboard.notifications = draft.notifications;
        self.remember_mark(&draft.mark);
        dashboard.mark = draft.mark;
        let reference = DashboardRef {
            group_id: draft.group_id.clone(),
            dashboard_id: dashboard.id.clone(),
        };
        self.change_environment(|environment| {
            environment
                .group_mut(&draft.group_id)?
                .dashboards
                .push(dashboard);
            Some(())
        })?;
        self.select(reference.clone());
        Some(reference)
    }

    /// Saves the editor's `draft` over the dashboard `reference`: name,
    /// view, notification setting, and its group (moved to the end of
    /// another one). Returns its reference afterwards.
    pub(crate) fn update_dashboard(
        &mut self,
        reference: &DashboardRef,
        draft: DashboardDraft,
    ) -> Option<DashboardRef> {
        let was_selected = self.selected.as_ref() == Some(reference);
        let moved = DashboardRef {
            group_id: draft.group_id.clone(),
            dashboard_id: reference.dashboard_id.clone(),
        };
        let changed = self.change_environment(|environment| {
            environment.group(&draft.group_id)?;
            let group = environment.group_mut(&reference.group_id)?;
            let index = group
                .dashboards
                .iter()
                .position(|dashboard| dashboard.id == reference.dashboard_id)?;
            let mut dashboard = group.dashboards[index].clone();
            if let Some(name) = non_blank(&draft.name) {
                name.clone_into(&mut dashboard.name);
            }
            dashboard.views = with_ids(draft.views);
            dashboard.notifications = draft.notifications;
            dashboard.mark = draft.mark.clone();
            let unchanged =
                group.dashboards[index] == dashboard && reference.group_id == draft.group_id;
            if unchanged {
                return None;
            }
            if reference.group_id == draft.group_id {
                group.dashboards[index] = dashboard;
            } else {
                group.dashboards.remove(index);
                environment
                    .group_mut(&draft.group_id)?
                    .dashboards
                    .push(dashboard);
            }
            Some(())
        });
        if changed.is_none() {
            return self.dashboard(reference).map(|_| reference.clone());
        }
        if let Some((_, dashboard)) = self.dashboard(&moved) {
            let mark = dashboard.mark.clone();
            self.remember_mark(&mark);
        }
        if was_selected {
            self.select(moved.clone());
        }
        Some(moved)
    }

    /// Renames a dashboard (`name` trimmed; blank names are refused).
    /// Returns whether it changed.
    pub(crate) fn rename_dashboard(&mut self, reference: &DashboardRef, name: &str) -> bool {
        let Some(name) = non_blank(name) else {
            return false;
        };
        self.change_environment(|environment| {
            let dashboard =
                environment.dashboard_mut(&reference.group_id, &reference.dashboard_id)?;
            (dashboard.name != name).then(|| name.clone_into(&mut dashboard.name))
        })
        .is_some()
    }

    /// Copies a dashboard (`<name> copy`, a fresh id) right after it and
    /// selects the copy. Returns its reference.
    pub(crate) fn duplicate_dashboard(&mut self, reference: &DashboardRef) -> Option<DashboardRef> {
        let copy = self.change_environment(|environment| {
            let group = environment.group_mut(&reference.group_id)?;
            let index = group
                .dashboards
                .iter()
                .position(|dashboard| dashboard.id == reference.dashboard_id)?;
            let original = &group.dashboards[index];
            let mut copy =
                Dashboard::with_views(&format!("{} copy", original.name), original.views.clone());
            copy.notifications = original.notifications.clone();
            let copy_reference = DashboardRef {
                group_id: group.id.clone(),
                dashboard_id: copy.id.clone(),
            };
            group.dashboards.insert(index + 1, copy);
            Some(copy_reference)
        })?;
        self.select(copy.clone());
        Some(copy)
    }

    /// Moves a dashboard to the end of another group. Returns its new
    /// reference (the same id, the other group).
    pub(crate) fn move_dashboard_to(
        &mut self,
        reference: &DashboardRef,
        group_id: &str,
    ) -> Option<DashboardRef> {
        if reference.group_id == group_id {
            return None;
        }
        let draft = {
            let (_, dashboard) = self.dashboard(reference)?;
            DashboardDraft {
                name: dashboard.name.clone(),
                views: dashboard.views.clone(),
                notifications: dashboard.notifications.clone(),
                group_id: group_id.to_owned(),
                mark: dashboard.mark.clone(),
            }
        };
        self.update_dashboard(reference, draft)
    }

    /// Moves a dashboard `delta` places up (negative) or down within its
    /// group. Returns whether it moved.
    pub(crate) fn move_dashboard(&mut self, reference: &DashboardRef, delta: isize) -> bool {
        self.change_environment(|environment| {
            let group = environment.group_mut(&reference.group_id)?;
            let from = group
                .dashboards
                .iter()
                .position(|dashboard| dashboard.id == reference.dashboard_id)?;
            let to = shifted(from, delta, group.dashboards.len())?;
            let dashboard = group.dashboards.remove(from);
            group.dashboards.insert(to, dashboard);
            Some(())
        })
        .is_some()
    }

    /// Deletes a dashboard (the sidebar asks first). Returns whether it
    /// existed.
    pub(crate) fn delete_dashboard(&mut self, reference: &DashboardRef) -> bool {
        self.change_environment(|environment| {
            let group = environment.group_mut(&reference.group_id)?;
            let index = group
                .dashboards
                .iter()
                .position(|dashboard| dashboard.id == reference.dashboard_id)?;
            group.dashboards.remove(index);
            Some(())
        })
        .is_some()
    }

    /// Sets a dashboard's notification setting. Returns whether it
    /// changed.
    pub(crate) fn set_dashboard_notifications(
        &mut self,
        reference: &DashboardRef,
        setting: ScopeSetting,
    ) -> bool {
        self.change_environment(|environment| {
            let dashboard =
                environment.dashboard_mut(&reference.group_id, &reference.dashboard_id)?;
            (dashboard.notifications != setting).then(|| dashboard.notifications = setting)
        })
        .is_some()
    }

    /// Adds imported groups (with the fresh ids `ic_config::import_groups`
    /// gave them) after the existing ones. Returns how many were added.
    pub(crate) fn import_groups(&mut self, groups: Vec<DashboardGroup>) -> usize {
        if groups.is_empty() {
            return 0;
        }
        let count = groups.len();
        let first_import = groups.first().and_then(|group| {
            group.dashboards.first().map(|dashboard| DashboardRef {
                group_id: group.id.clone(),
                dashboard_id: dashboard.id.clone(),
            })
        });
        let added = self.change_environment(|environment| {
            environment.groups.extend(groups);
            Some(())
        });
        if added.is_none() {
            return 0;
        }
        if let Some(reference) = first_import {
            self.select(reference);
        }
        count
    }

    /// The groups `group_ids` (all of them when empty) as an export file
    /// (`ic_config::export_groups`).
    ///
    /// # Errors
    ///
    /// No environment, or the groups can't be written as TOML.
    pub(crate) fn export_groups(&self, group_ids: &[String]) -> Result<String, String> {
        let environment = self
            .environment()
            .ok_or_else(|| "no environment is active".to_owned())?;
        let groups: Vec<DashboardGroup> = environment
            .groups
            .iter()
            .filter(|group| group_ids.is_empty() || group_ids.contains(&group.id))
            .cloned()
            .collect();
        if groups.is_empty() {
            return Err("there are no dashboards to export".to_owned());
        }
        ic_config::export_groups(&groups).map_err(|error| error.to_string())
    }

    /// Re-evaluates the fixture's dashboards (it has no core) and drops
    /// results of dashboards that are gone.
    #[cfg(test)]
    pub(super) fn evaluate_fixture_all(&mut self) {
        use std::sync::Arc;
        let Some(evaluator) = &self.evaluator else {
            return;
        };
        let dashboards = evaluator.evaluate_all(&self.engine.snapshot, &self.config);
        self.engine.snapshot = Arc::new(ic_core::snapshot::Snapshot {
            revision: self.engine.snapshot.revision + 1,
            dashboards: Arc::new(dashboards),
            ..(*self.engine.snapshot).clone()
        });
    }
}

/// `name` trimmed, unless that's empty.
fn non_blank(name: &str) -> Option<&str> {
    Some(name.trim()).filter(|name| !name.is_empty())
}

/// `index` moved by `delta` within `0..len`; `None` when that leaves the
/// range or doesn't move.
fn shifted(index: usize, delta: isize, len: usize) -> Option<usize> {
    let to = index.checked_add_signed(delta)?;
    (to < len && to != index).then_some(to)
}

#[cfg(test)]
mod tests {
    use ic_config::{ObjectKind, Paths};
    use ic_core::snapshot::DashboardResult;
    use ic_model::Timestamp;

    use super::*;
    use crate::app_state::testing::Recorder;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_790_000_000.)
    }

    fn fixture() -> (AppState, Recorder) {
        let mut state = AppState::fixture(now());
        let recorder = Recorder::default();
        state.set_core(Box::new(recorder.clone()));
        (state, recorder)
    }

    fn names(state: &AppState) -> Vec<(String, Vec<String>)> {
        state
            .groups()
            .iter()
            .map(|group| {
                (
                    group.name.clone(),
                    group
                        .dashboards
                        .iter()
                        .map(|dashboard| dashboard.name.clone())
                        .collect(),
                )
            })
            .collect()
    }

    fn group_id(state: &AppState, name: &str) -> String {
        state
            .groups()
            .iter()
            .find(|group| group.name == name)
            .unwrap()
            .id
            .clone()
    }

    fn reference(state: &AppState, group: &str, dashboard: &str) -> DashboardRef {
        let group = state.groups().iter().find(|g| g.name == group).unwrap();
        DashboardRef {
            group_id: group.id.clone(),
            dashboard_id: group
                .dashboards
                .iter()
                .find(|d| d.name == dashboard)
                .unwrap()
                .id
                .clone(),
        }
    }

    #[test]
    fn groups_are_created_renamed_reordered_and_deleted() {
        let (mut state, recorder) = fixture();
        let platform = group_id(&state, "platform");
        let created = state.create_group("  databases ", Some(&platform)).unwrap();
        assert_eq!(
            names(&state)
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            ["overview", "platform", "databases", "lab"]
        );
        assert_eq!(recorder.sent(), ["UpdateEnvironment(prod-cluster)"]);
        let unnamed = state.create_group("   ", None).unwrap();
        assert_eq!(state.groups().last().unwrap().name, NEW_GROUP_NAME);
        assert_eq!(state.groups().last().unwrap().id, unnamed);

        assert!(state.rename_group(&created, "db"));
        assert!(!state.rename_group(&created, "db"), "unchanged");
        assert!(
            !state.rename_group(&created, "  "),
            "blank names are refused"
        );
        assert!(!state.rename_group("missing", "x"));

        assert!(state.move_group(&created, -2));
        assert_eq!(state.groups()[0].name, "db");
        assert!(!state.move_group(&created, -1), "already first");
        assert!(state.move_group(&created, 1));
        assert_eq!(state.groups()[1].name, "db");
        assert!(!state.move_group(&unnamed, 1), "already last");

        assert!(state.delete_group(&created));
        assert!(!state.delete_group(&created));
        assert!(state.groups().iter().all(|group| group.name != "db"));
    }

    #[test]
    fn deleting_the_selected_dashboards_group_selects_the_first_left() {
        let (mut state, _) = fixture();
        let overview = group_id(&state, "overview");
        assert_eq!(state.selected().unwrap().group_id, overview);
        assert!(state.delete_group(&overview));
        let (group, dashboard) = state.selected_dashboard().unwrap();
        assert_eq!(
            (group.name.as_str(), dashboard.name.as_str()),
            ("platform", "network")
        );
        // The fixture has no core evaluating: the state does it here.
        assert!(state.result(state.selected().unwrap()).is_some());
    }

    #[test]
    fn dashboards_are_added_edited_and_moved() {
        let (mut state, recorder) = fixture();
        let lab = group_id(&state, "lab");
        let view = View {
            object_kind: ObjectKind::Hosts,
            filter: "host.vars.env == \"prod\"".to_owned(),
            ..View::default()
        };
        let added = state
            .add_dashboard(DashboardDraft {
                name: " prod hosts ".to_owned(),
                views: vec![view.clone()],
                notifications: ScopeSetting::Off,
                group_id: lab.clone(),
                mark: ic_config::SidebarMark::Auto,
            })
            .unwrap();
        assert_eq!(state.selected(), Some(&added), "a new dashboard is shown");
        let (_, dashboard) = state.dashboard(&added).unwrap();
        assert_eq!(dashboard.name, "prod hosts");
        assert_eq!(dashboard.notifications, ScopeSetting::Off);
        assert!(
            state
                .result(&added)
                .and_then(DashboardResult::first)
                .is_some_and(|result| result.error.is_none()),
            "evaluated"
        );
        assert!(
            recorder
                .sent()
                .contains(&"UpdateEnvironment(prod-cluster)".to_owned())
        );

        // Edit: name, view, and into another group.
        let platform = group_id(&state, "platform");
        let moved = state
            .update_dashboard(
                &added,
                DashboardDraft {
                    name: "prod".to_owned(),
                    views: vec![View {
                        problems_only: false,
                        ..view.clone()
                    }],
                    notifications: ScopeSetting::Inherit,
                    group_id: platform.clone(),
                    mark: ic_config::SidebarMark::Auto,
                },
            )
            .unwrap();
        assert_eq!(moved.group_id, platform);
        assert_eq!(moved.dashboard_id, added.dashboard_id, "the id stays");
        assert_eq!(state.selected(), Some(&moved), "still shown");
        assert!(state.dashboard(&added).is_none());
        assert_eq!(names(&state)[1].1.last().unwrap(), "prod");

        // Saving the same draft changes nothing.
        recorder.clear();
        let (_, current) = state.dashboard(&moved).unwrap();
        let same = DashboardDraft {
            name: current.name.clone(),
            views: current.views.clone(),
            notifications: current.notifications.clone(),
            group_id: platform.clone(),
            mark: ic_config::SidebarMark::Auto,
        };
        assert_eq!(state.update_dashboard(&moved, same), Some(moved.clone()));
        assert!(recorder.sent().is_empty());

        // An unknown target group changes nothing.
        let nowhere = DashboardDraft {
            name: "x".to_owned(),
            views: vec![view],
            notifications: ScopeSetting::Inherit,
            group_id: "missing".to_owned(),
            mark: ic_config::SidebarMark::Auto,
        };
        assert_eq!(state.update_dashboard(&moved, nowhere), Some(moved));
    }

    #[test]
    fn dashboards_are_renamed_duplicated_reordered_and_deleted() {
        let (mut state, _) = fixture();
        let network = reference(&state, "platform", "network");
        assert!(state.rename_dashboard(&network, "net"));
        assert!(!state.rename_dashboard(&network, " "));
        let copy = state.duplicate_dashboard(&network).unwrap();
        assert_eq!(state.selected(), Some(&copy));
        assert_eq!(
            names(&state)[1].1[..2],
            ["net".to_owned(), "net copy".to_owned()]
        );
        assert!(state.move_dashboard(&copy, -1));
        assert_eq!(names(&state)[1].1[0], "net copy");
        assert!(!state.move_dashboard(&copy, -1), "already first");

        let lab = group_id(&state, "lab");
        let moved = state.move_dashboard_to(&copy, &lab).unwrap();
        assert_eq!(names(&state)[2].1, ["sandbox", "net copy"]);
        assert_eq!(state.selected(), Some(&moved));
        assert_eq!(state.move_dashboard_to(&moved, &lab), None, "already there");

        assert!(state.set_dashboard_notifications(&moved, ScopeSetting::Off));
        assert!(!state.set_dashboard_notifications(&moved, ScopeSetting::Off));
        assert!(state.set_group_notifications(&lab, ScopeSetting::On));

        assert!(state.delete_dashboard(&moved));
        assert!(!state.delete_dashboard(&moved));
        let (group, _) = state.selected_dashboard().unwrap();
        assert_eq!(group.name, "overview", "the first dashboard is selected");
    }

    #[test]
    fn exports_import_as_fresh_copies() {
        let (mut state, _) = fixture();
        let platform = group_id(&state, "platform");
        let text = state
            .export_groups(std::slice::from_ref(&platform))
            .unwrap();
        assert!(text.contains("icygui-dashboards"));
        let all = state.export_groups(&[]).unwrap();
        assert!(all.contains("sandbox"));
        let imported = ic_config::import_groups(&text).unwrap();
        assert_eq!(state.import_groups(imported), 1);
        let names = names(&state);
        assert_eq!(names.len(), 4);
        assert_eq!(names[3].0, "platform");
        assert_ne!(state.groups()[3].id, platform, "fresh ids");
        assert_eq!(state.selected().unwrap().group_id, state.groups()[3].id);
        assert_eq!(state.import_groups(Vec::new()), 0);
        assert!(state.export_groups(&["missing".to_owned()]).is_err());
    }

    #[test]
    fn edits_wait_while_the_engine_waits_to_reconnect() {
        let (mut state, recorder) = fixture();
        state.apply(ic_core::CoreEvent::Connection(
            ic_core::ConnectionState::Reconnecting {
                error: "refused".to_owned(),
                attempt: 2,
                retry_at: now(),
                untrusted: None,
            },
        ));
        assert!(state.create_group("x", None).is_some());
        assert!(recorder.sent().is_empty(), "held back");
        state.apply(ic_core::CoreEvent::Connection(
            ic_core::ConnectionState::Connected {
                node: crate::app_state::connection::full_node("master-01"),
                version: "v2.15.6".to_owned(),
                since: now(),
            },
        ));
        assert_eq!(recorder.sent(), ["UpdateEnvironment(prod-cluster)"]);
    }

    #[test]
    fn edits_are_saved() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        let mut config = crate::fixture::build(now()).config;
        config.active_environment = None;
        let mut state = AppState::live(config, ic_config::UiState::default(), now());
        let persistence = crate::persist::Persistence::start(
            paths.config_store(),
            paths.state_store(),
            Box::new(|_| {}),
        )
        .unwrap();
        state.set_persistence(persistence);
        let id = state.create_group("saved", None).unwrap();
        assert!(state.flush_persistence(std::time::Duration::from_secs(5)));
        let saved = paths.config_store().load().unwrap();
        assert!(saved.environments[0].group(&id).is_some());
    }

    #[test]
    fn shifting_stays_in_range() {
        assert_eq!(shifted(0, 1, 3), Some(1));
        assert_eq!(shifted(2, 1, 3), None);
        assert_eq!(shifted(0, -1, 3), None);
        assert_eq!(shifted(1, 0, 3), None);
        assert_eq!(shifted(2, -2, 3), Some(0));
    }
}
