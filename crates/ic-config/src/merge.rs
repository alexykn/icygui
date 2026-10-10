//! Merging an edit of the settings file by hand with the changes made in
//! the app meanwhile (*edit in settings file* in the settings panel).
//!
//! Three versions take part: `base`, the settings as icygui last read or
//! wrote the file; `ours`, the app's settings now; and `theirs`, the file
//! as edited. Every setting changed on only one side keeps that change, so
//! an edit by hand never undoes a change made in the window and the other
//! way round. A setting changed on both sides to different values takes
//! the app's (the change made in the window is applied on top of the
//! edited file).
//!
//! Tables merge key by key. Lists of entries with an `id` (environments,
//! dashboard groups, dashboards) merge entry by entry, so editing one
//! environment's rules by hand and collapsing a group of another in the
//! window keep both; an entry removed on one side and left alone on the
//! other is removed. Other lists (an environment's URLs) are one value.

use std::collections::HashSet;

use toml::Value;

use crate::model::Config;

/// The settings with both sides' changes since `base`: `theirs` (the file
/// edited by hand) with the changes `ours` (the app) made since `base`
/// applied on top. When a version can't be turned into TOML, which saving
/// it would refuse as well (a path that isn't UTF-8), the file's version
/// is the result.
pub fn merge_edit(base: &Config, ours: &Config, theirs: &Config) -> Config {
    let values = (
        Value::try_from(base),
        Value::try_from(ours),
        Value::try_from(theirs),
    );
    let (Ok(base_value), Ok(ours_value), Ok(theirs_value)) = values else {
        tracing::warn!("the settings couldn't be merged; the file's version is taken over");
        return theirs.clone();
    };
    match merge(Some(&base_value), Some(&ours_value), Some(&theirs_value)) {
        Some(merged) => merged.try_into().unwrap_or_else(|error| {
            tracing::warn!(%error, "the merged settings don't read; the file's version is taken over");
            theirs.clone()
        }),
        None => theirs.clone(),
    }
}

/// One value, merged: `None` for a key or entry that ends up removed.
fn merge(base: Option<&Value>, ours: Option<&Value>, theirs: Option<&Value>) -> Option<Value> {
    if ours == theirs || ours == base {
        return theirs.cloned();
    }
    if theirs == base {
        return ours.cloned();
    }
    // Changed on both sides, differently.
    match (ours, theirs) {
        (Some(Value::Table(ours)), Some(Value::Table(theirs))) => {
            let base = match base {
                Some(Value::Table(base)) => Some(base),
                _ => None,
            };
            let mut merged = toml::Table::new();
            let keys = ours
                .keys()
                .chain(theirs.keys().filter(|key| !ours.contains_key(*key)));
            for key in keys {
                let value = merge(
                    base.and_then(|base| base.get(key)),
                    ours.get(key),
                    theirs.get(key),
                );
                if let Some(value) = value {
                    merged.insert(key.clone(), value);
                }
            }
            Some(Value::Table(merged))
        }
        (Some(Value::Array(ours)), Some(Value::Array(theirs))) => {
            let base = match base {
                Some(Value::Array(base)) => base.as_slice(),
                _ => &[],
            };
            match merge_entries(base, ours, theirs) {
                Some(merged) => Some(Value::Array(merged)),
                None => Some(Value::Array(ours.clone())),
            }
        }
        // A conflict: the change made in the window wins.
        _ => ours.cloned(),
    }
}

/// Lists of entries with unique ids, merged entry by entry (`None` when a
/// list isn't one: it is then one value). The order is the file's unless
/// the app reordered, added or removed entries; entries only one side
/// added come last.
fn merge_entries(base: &[Value], ours: &[Value], theirs: &[Value]) -> Option<Vec<Value>> {
    let base_ids = ids(base)?;
    let our_ids = ids(ours)?;
    let their_ids = ids(theirs)?;
    let first = if our_ids == base_ids {
        &their_ids
    } else {
        &our_ids
    };
    let mut order: Vec<&str> = Vec::new();
    for id in first.iter().chain(&our_ids).chain(&their_ids) {
        if !order.contains(id) {
            order.push(id);
        }
    }
    Some(
        order
            .into_iter()
            .filter_map(|id| merge(find(base, id), find(ours, id), find(theirs, id)))
            .collect(),
    )
}

/// The entry with `id`.
fn find<'a>(entries: &'a [Value], id: &str) -> Option<&'a Value> {
    entries.iter().find(|entry| entry_id(entry) == Some(id))
}

/// The entries' ids in order, if every entry is a table with a unique
/// string `id`.
fn ids(entries: &[Value]) -> Option<Vec<&str>> {
    let ids: Vec<&str> = entries.iter().map(entry_id).collect::<Option<_>>()?;
    let unique: HashSet<&&str> = ids.iter().collect();
    (unique.len() == ids.len()).then_some(ids)
}

fn entry_id(entry: &Value) -> Option<&str> {
    entry.as_table()?.get("id")?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AuthConfig, Dashboard, DashboardGroup, Environment, InterfaceSize, LogLevel, RowDensity,
    };

    fn environment(id: &str) -> Environment {
        let mut environment = Environment::new(
            id,
            &format!("https://{id}:5665"),
            AuthConfig::Basic {
                username: "icygui".to_owned(),
            },
        );
        environment.id = id.to_owned();
        environment.groups = vec![DashboardGroup {
            id: format!("{id}-group"),
            name: "services".to_owned(),
            dashboards: vec![Dashboard {
                id: format!("{id}-dashboard"),
                name: "overview".to_owned(),
                ..Dashboard::default()
            }],
            ..DashboardGroup::default()
        }];
        environment
    }

    fn base() -> Config {
        Config {
            environments: vec![environment("prod"), environment("staging")],
            active_environment: Some("prod".to_owned()),
            ..Config::default()
        }
    }

    #[test]
    fn changes_on_either_side_are_both_kept() {
        let base = base();
        let mut ours = base.clone();
        ours.appearance.row_density = RowDensity::Compact;
        ours.active_environment = Some("staging".to_owned());
        ours.environments[1].groups[0].collapsed = true;
        let mut theirs = base.clone();
        theirs.general.event_log_retention_hours = 100;
        theirs.environments[0].notifications.storm.threshold = 12;
        let merged = merge_edit(&base, &ours, &theirs);
        assert_eq!(merged.appearance.row_density, RowDensity::Compact);
        assert_eq!(merged.active_environment.as_deref(), Some("staging"));
        assert!(merged.environments[1].groups[0].collapsed);
        assert_eq!(merged.general.event_log_retention_hours, 100);
        assert_eq!(merged.environments[0].notifications.storm.threshold, 12);
    }

    #[test]
    fn one_sided_versions_are_taken_whole() {
        let base = base();
        let mut theirs = base.clone();
        theirs.general.log_level = LogLevel::Debug;
        assert_eq!(merge_edit(&base, &base, &theirs), theirs);
        let mut ours = base.clone();
        ours.appearance.interface_size = InterfaceSize::Large;
        assert_eq!(merge_edit(&base, &ours, &base), ours);
    }

    #[test]
    fn the_same_setting_changed_on_both_sides_takes_the_apps_change() {
        let base = base();
        let mut ours = base.clone();
        ours.general.event_log_retention_hours = 96;
        let mut theirs = base.clone();
        theirs.general.event_log_retention_hours = 100;
        theirs.general.close_to_tray = !base.general.close_to_tray;
        let merged = merge_edit(&base, &ours, &theirs);
        assert_eq!(merged.general.event_log_retention_hours, 96);
        assert_eq!(merged.general.close_to_tray, theirs.general.close_to_tray);
    }

    #[test]
    fn entries_merge_by_id() {
        let base = base();
        // The window adds a dashboard to prod; the file removes staging and
        // adds an environment.
        let mut ours = base.clone();
        ours.environments[0].groups[0].dashboards.push(Dashboard {
            id: "prod-new".to_owned(),
            name: "databases".to_owned(),
            ..Dashboard::default()
        });
        let mut theirs = base.clone();
        theirs.environments.remove(1);
        theirs.environments.push(environment("lab"));
        let merged = merge_edit(&base, &ours, &theirs);
        let ids: Vec<&str> = merged
            .environments
            .iter()
            .map(|environment| environment.id.as_str())
            .collect();
        assert_eq!(ids, ["prod", "lab"]);
        let dashboards: Vec<&str> = merged.environments[0].groups[0]
            .dashboards
            .iter()
            .map(|dashboard| dashboard.name.as_str())
            .collect();
        assert_eq!(dashboards, ["overview", "databases"]);
    }

    #[test]
    fn an_entry_removed_on_one_side_but_changed_on_the_other_stays() {
        let base = base();
        let mut ours = base.clone();
        ours.environments[1].groups[0].collapsed = true;
        let mut theirs = base.clone();
        theirs.environments.remove(1);
        let merged = merge_edit(&base, &ours, &theirs);
        assert_eq!(merged.environments.len(), 2, "the window's change wins");
        // Removed in the window, left alone in the file: removed.
        let mut ours = base.clone();
        ours.environments.remove(0);
        let mut theirs = base.clone();
        theirs.general.event_log_retention_hours = 100;
        let merged = merge_edit(&base, &ours, &theirs);
        assert_eq!(merged.environments.len(), 1);
        assert_eq!(merged.environments[0].id, "staging");
        assert_eq!(merged.general.event_log_retention_hours, 100);
    }

    #[test]
    fn the_files_order_holds_unless_the_app_reordered() {
        let base = base();
        let mut theirs = base.clone();
        theirs.environments.reverse();
        let mut ours = base.clone();
        ours.general.event_log_retention_hours = 96;
        let merged = merge_edit(&base, &ours, &theirs);
        assert_eq!(merged.environments[0].id, "staging");
        let mut ours = base.clone();
        ours.environments.reverse();
        ours.environments[0].groups[0].collapsed = true;
        let mut theirs = base.clone();
        theirs.environments[0].notifications.storm.threshold = 12;
        let merged = merge_edit(&base, &ours, &theirs);
        assert_eq!(merged.environments[0].id, "staging");
        assert!(merged.environments[0].groups[0].collapsed);
        assert_eq!(merged.environments[1].notifications.storm.threshold, 12);
    }

    #[test]
    fn lists_without_ids_are_one_value() {
        let base = base();
        let mut ours = base.clone();
        ours.environments[0].urls[0].url = "https://master-02:5665".to_owned();
        let mut theirs = base.clone();
        theirs.environments[0]
            .urls
            .push(crate::ApiUrl::new("https://master-03:5665"));
        let merged = merge_edit(&base, &ours, &theirs);
        assert_eq!(merged.environments[0].urls, ours.environments[0].urls);
    }
}
