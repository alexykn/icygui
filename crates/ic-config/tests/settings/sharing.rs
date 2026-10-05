//! Exporting and importing dashboard groups.

use std::collections::HashSet;

use ic_config::{CONFIG_VERSION, ConfigError, DashboardGroup, export_groups, import_groups};
use ic_rules::ScopeSetting;

use crate::fixtures::full_config;
use crate::logs::{capture, warnings};

/// Groups with their ids cleared, for comparing content.
fn without_ids(groups: &[DashboardGroup]) -> Vec<DashboardGroup> {
    let mut groups = groups.to_vec();
    for group in &mut groups {
        group.id.clear();
        for dashboard in &mut group.dashboards {
            dashboard.id.clear();
        }
    }
    groups
}

fn all_ids(groups: &[DashboardGroup]) -> Vec<&str> {
    groups
        .iter()
        .flat_map(|group| {
            std::iter::once(group.id.as_str()).chain(
                group
                    .dashboards
                    .iter()
                    .map(|dashboard| dashboard.id.as_str()),
            )
        })
        .collect()
}

#[test]
fn exports_import_with_the_same_content_and_fresh_ids() {
    let groups = full_config().environments[0].groups.clone();
    let text = export_groups(&groups).unwrap();
    let imported = import_groups(&text).unwrap();
    assert_eq!(without_ids(&imported), without_ids(&groups));

    let original: HashSet<&str> = all_ids(&groups).into_iter().collect();
    let new: Vec<&str> = all_ids(&imported);
    assert!(
        new.iter()
            .all(|id| !id.is_empty() && !original.contains(id))
    );
    assert_eq!(
        new.iter().collect::<HashSet<_>>().len(),
        new.len(),
        "unique"
    );

    // Importing the same file again gives yet other ids.
    let again = import_groups(&text).unwrap();
    assert!(all_ids(&again).iter().all(|id| !new.contains(id)));
}

#[test]
fn imported_groups_keep_their_notification_settings() {
    let groups = full_config().environments[0].groups.clone();
    let imported = import_groups(&export_groups(&groups).unwrap()).unwrap();
    assert!(matches!(imported[1].notifications, ScopeSetting::Custom(_)));
    assert_eq!(imported[1].dashboards[0].notifications, ScopeSetting::On);
    assert_eq!(imported[1].dashboards[1].notifications, ScopeSetting::Off);
    assert!(imported[1].collapsed);
}

#[test]
fn exports_are_marked() {
    let text = export_groups(&full_config().environments[0].groups).unwrap();
    assert!(
        text.contains("\nformat = \"icygui-dashboards\"\n"),
        "{text}"
    );
    assert!(
        text.contains(&format!("\nversion = {CONFIG_VERSION}\n")),
        "{text}"
    );
    assert!(text.contains("\n[[groups]]\n"), "{text}");
}

#[test]
fn hand_written_exports_are_accepted() {
    let imported = import_groups(
        r#"
[[groups]]
name = "web"

[[groups.dashboards]]
name = "frontends"
view = { object_kind = "hosts", filter = 'host.vars.role == "web"', look = "compact" }
"#,
    )
    .unwrap();
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].name, "web");
    assert!(!imported[0].id.is_empty());
    let view = &imported[0].dashboards[0].view;
    assert_eq!(view.object_kind, ic_config::ObjectKind::Hosts);
    assert_eq!(view.filter, r#"host.vars.role == "web""#);
    assert!(view.problems_only, "defaults fill in the rest");
}

#[test]
fn newer_exports_are_rejected() {
    let text = format!(
        "format = \"icygui-dashboards\"\nversion = {}\ngroups = []\n",
        CONFIG_VERSION + 1
    );
    assert!(matches!(
        import_groups(&text),
        Err(ConfigError::UnsupportedVersion { .. })
    ));
}

#[test]
fn broken_exports_are_reported_with_a_line() {
    let text = "format = \"icygui-dashboards\"\nversion = 1\n\n[[groups]]\nname = \"web\"\ncollapsed = \"no\"\n";
    match import_groups(text) {
        Err(ConfigError::Parse { message }) => {
            assert!(message.contains("line 6"), "{message}");
        }
        other => panic!("expected a parse error, got {other:?}"),
    }
    assert!(matches!(
        import_groups("[[groups]\nname = 1"),
        Err(ConfigError::Parse { .. })
    ));
}

#[test]
fn nameless_groups_and_dashboards_are_rejected() {
    let text = "[[groups]]\nname = \" \"\n\n[[groups.dashboards]]\n\n[[groups]]\nname = \"ok\"\n";
    match import_groups(text) {
        Err(ConfigError::Invalid(issues)) => {
            let issues: Vec<String> = issues.iter().map(ToString::to_string).collect();
            assert_eq!(
                issues,
                [
                    "groups[0].name: must not be empty",
                    "groups[0].dashboards[0].name: must not be empty",
                ]
            );
        }
        other => panic!("expected invalid content, got {other:?}"),
    }
}

#[test]
fn settings_files_are_not_exports() {
    let settings = toml::to_string(&full_config()).unwrap();
    assert!(matches!(
        import_groups(&settings),
        Err(ConfigError::NotAnExport(_))
    ));
}

#[test]
fn importing_an_export_logs_nothing() {
    let text = export_groups(&full_config().environments[0].groups).unwrap();
    let (imported, events) = capture(|| import_groups(&text).unwrap());
    assert_eq!(imported.len(), 2);
    assert_eq!(warnings(&events), Vec::<&str>::new());
}

#[test]
fn unknown_keys_in_imports_are_logged() {
    let text = "format = \"icygui-dashboards\"\nversion = 1\n\n[[groups]]\nname = \"web\"\ncolapsed = true\n";
    let (imported, events) = capture(|| import_groups(text).unwrap());
    assert!(!imported[0].collapsed);
    assert_eq!(
        warnings(&events),
        ["ignoring unknown key in the dashboard export key=groups.0.colapsed"]
    );
}
