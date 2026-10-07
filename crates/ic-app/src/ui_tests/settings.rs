//! The settings panel (PLAN.md §4.2, mock-up 02) on the fixture: it opens
//! over the window from the shortcut, the palette and the app menu's
//! action, and Escape or a press outside closes it; the search dims the
//! categories without a match, counts the others and its rows work in
//! place; every change applies at once, and a bad value shows its problem
//! under its row and isn't applied; the notification rules, the custom
//! rule of a group or dashboard, quiet hours and storm control; the
//! keymap page and the general switches with launch at login.
//!
//! Positions are where the panel draws its rows in the 1440 × 900 window
//! (the panel 1080 × 760, centred); the screenshots in the review show the
//! same layout.

use std::time::Duration;

use futures::FutureExt as _;
use gpui::{App, AppContext as _, Entity, Focusable as _, Modifiers, point, px};
use ic_config::{Config, RowDensity, UiState};
use ic_model::Timestamp;
use ic_rules::ScopeSetting;

use super::{Body, Harness, run, run_app, wait_for};
use crate::app_state::AppState;
use crate::fixture::FixtureOptions;
use crate::settings::{
    FieldId, Locations, OPENED, RuleFlag, ScopeKey, SettingsPage, SettingsPanel,
};

/// The switch of a page's first row, and the rows under it (one-line
/// descriptions): `x` at the panel's right edge.
const SWITCH_X: f32 = 1213.;
/// The first row's control, on a page.
const FIRST_ROW_Y: f32 = 195.;
/// One row further down (a one-line description).
const ROW_STEP: f32 = 62.;

fn panel(app: &Harness, cx: &App) -> Entity<SettingsPanel> {
    app.workspace.read(cx).settings().unwrap().clone()
}

/// Replaces a field's text, as typing would.
fn type_into(app: &Harness, cx: &mut App, id: &FieldId, text: &str) {
    let input = panel(app, cx).read(cx).input(id).unwrap().clone();
    app.in_window(cx, |window, cx| {
        input.focus_handle(cx).focus(window, cx);
        input.update(cx, |input, cx| input.replace_all(text, window, cx));
    });
    app.draw(cx);
}

/// Types `text` into a field and presses Enter.
fn enter_into(app: &Harness, cx: &mut App, id: &FieldId, text: &str) {
    type_into(app, cx, id, text);
    app.keys(cx, "enter");
}

fn show(app: &Harness, cx: &mut App, page: SettingsPage) {
    let panel = panel(app, cx);
    app.in_window(cx, |window, cx| {
        panel.update(cx, |panel, cx| panel.show_page(page, window, cx));
    });
    app.draw(cx);
}

#[test]
fn the_panel_opens_over_the_window_and_closes() {
    run(FixtureOptions::default(), |app, cx| {
        // The shortcut: over the window, on general, the search field
        // with the keyboard.
        app.keys(cx, "ctrl-,");
        assert_eq!(app.workspace.read(cx).modal(cx), None, "not a dialog");
        let settings = panel(app, cx);
        assert_eq!(settings.read(cx).page(), SettingsPage::General);
        app.keys(cx, "a p p");
        assert_eq!(settings.read(cx).query(), "app");
        // Escape clears the search, then closes the panel.
        app.keys(cx, "escape");
        assert_eq!(settings.read(cx).query(), "");
        assert!(app.workspace.read(cx).settings().is_some());
        app.keys(cx, "escape");
        assert!(app.workspace.read(cx).settings().is_none());
        // The keys are the list's again.
        app.keys(cx, "j");
        assert!(app.cursor(cx).is_some());

        // The palette's "notification settings" opens that page.
        app.keys(cx, "ctrl-k");
        let palette = app.workspace.read(cx).palette().unwrap().clone();
        let input = palette.read(cx).input().clone();
        app.in_window(cx, |window, cx| {
            input.update(cx, |input, cx| {
                input.replace_all("notification settings", window, cx);
            });
        });
        app.draw(cx);
        app.keys(cx, "enter");
        assert_eq!(app.workspace.read(cx).modal(cx), None, "the palette closed");
        assert_eq!(panel(app, cx).read(cx).page(), SettingsPage::Notifications);

        // The app menu's action; then a press outside closes it.
        app.in_window(cx, |window, cx| {
            window.dispatch_action(Box::new(crate::actions::OpenSettings), cx);
        });
        app.draw(cx);
        assert_eq!(
            panel(app, cx).read(cx).page(),
            SettingsPage::Notifications,
            "open already: the page stays"
        );
        app.click(cx, point(px(100.), px(860.)), Modifiers::default());
        assert!(app.workspace.read(cx).settings().is_none());

        // The close button.
        app.keys(cx, "ctrl-,");
        app.click(cx, point(px(1236.), px(90.)), Modifiers::default());
        assert!(app.workspace.read(cx).settings().is_none());
    });
}

#[test]
fn the_navigation_and_its_keys_open_every_page() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "ctrl-,");
        // A click on a category (general's one section listed above).
        app.click(cx, point(px(250.), px(248.)), Modifiers::default());
        assert_eq!(panel(app, cx).read(cx).page(), SettingsPage::Icinga);
        // focus navbar, then the arrows.
        app.keys(cx, "ctrl-shift-e down down");
        assert_eq!(panel(app, cx).read(cx).page(), SettingsPage::Advanced);
        app.keys(cx, "up up up up up up");
        assert_eq!(panel(app, cx).read(cx).page(), SettingsPage::General);
        // Every page draws.
        for page in SettingsPage::ALL {
            show(app, cx, page);
            assert_eq!(panel(app, cx).read(cx).page(), page);
        }
    });
}

#[test]
fn the_search_counts_matches_and_its_rows_work_in_place() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "ctrl-,");
        app.keys(cx, "q u i e t");
        let settings = panel(app, cx);
        let counts = settings.read(cx).search_counts(cx);
        let count = |page: SettingsPage| {
            counts
                .iter()
                .find(|(each, _)| *each == page)
                .map_or(0, |(_, count)| *count)
        };
        assert_eq!(count(SettingsPage::General), 1, "{counts:?}");
        assert_eq!(count(SettingsPage::Notifications), 1, "{counts:?}");
        for page in [
            SettingsPage::Appearance,
            SettingsPage::Icinga,
            SettingsPage::Keymap,
            SettingsPage::Advanced,
        ] {
            assert_eq!(count(page), 0, "{page:?} dims");
        }
        // The first match is quiet mode's switch, under `general`: it works
        // in place.
        assert!(app.state.read(cx).config().general.quiet_when_hidden);
        app.click(cx, point(px(SWITCH_X), px(193.)), Modifiers::default());
        assert!(!app.state.read(cx).config().general.quiet_when_hidden);
        assert_eq!(settings.read(cx).query(), "quiet", "the search stays");

        // A section's name brings its rows: quiet hours on brings its
        // three rows too.
        app.state.update(cx, |state, cx| {
            let id = state.active_environment_id().unwrap().to_owned();
            state.change_notifications(&id, |plan| plan.settings.quiet_hours.enabled = true);
            cx.notify();
        });
        app.draw(cx);
        let counts = settings.read(cx).search_counts(cx);
        assert!(
            counts.contains(&(SettingsPage::Notifications, 4)),
            "{counts:?}"
        );
        // The keymap's shortcuts are found by what they do.
        app.keys(cx, "escape");
        app.keys(cx, "p a l e t t e");
        let counts = settings.read(cx).search_counts(cx);
        assert!(
            counts
                .iter()
                .any(|(page, count)| *page == SettingsPage::Keymap && *count >= 2),
            "{counts:?}"
        );
    });
}

#[test]
fn changes_apply_at_once_and_bad_values_are_refused_under_their_row() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "ctrl-,");
        // A switch: applied as it is clicked, no save.
        assert!(app.state.read(cx).config().general.close_to_tray);
        app.click(
            cx,
            point(px(SWITCH_X), px(FIRST_ROW_Y)),
            Modifiers::default(),
        );
        assert!(!app.state.read(cx).config().general.close_to_tray);

        // A choice.
        show(app, cx, SettingsPage::Appearance);
        app.click(cx, point(px(1186.), px(364.)), Modifiers::default());
        assert_eq!(
            app.state.read(cx).appearance().row_density,
            RowDensity::Compact
        );

        // A text field applies on Enter; a bad value shows its problem
        // and changes nothing.
        show(app, cx, SettingsPage::Icinga);
        enter_into(app, cx, &FieldId::Retention, "lots");
        let settings = panel(app, cx);
        assert!(
            settings
                .read(cx)
                .errors()
                .get(&FieldId::Retention)
                .is_some_and(|error| error.contains("number of hours")),
            "{:?}",
            settings.read(cx).errors()
        );
        assert_eq!(
            app.state
                .read(cx)
                .config()
                .general
                .event_log_retention_hours,
            48
        );
        // Fixing it clears the problem as it is typed; Enter applies it.
        type_into(app, cx, &FieldId::Retention, "72");
        assert!(settings.read(cx).errors().is_empty(), "fixed as typed");
        app.keys(cx, "enter");
        assert_eq!(
            app.state
                .read(cx)
                .config()
                .general
                .event_log_retention_hours,
            72
        );
        // Leaving the field applies it too.
        type_into(app, cx, &FieldId::Retention, "96h");
        app.keys(cx, "tab");
        assert_eq!(
            app.state
                .read(cx)
                .config()
                .general
                .event_log_retention_hours,
            96
        );
        assert_eq!(
            settings
                .read(cx)
                .input(&FieldId::Retention)
                .unwrap()
                .read(cx)
                .value(),
            "96",
            "the field shows the value as stored"
        );

        // A fixed reconcile interval: its field appears with 600 seconds,
        // and refuses less than a minute.
        app.click(cx, point(px(1160.), px(195.)), Modifiers::default());
        assert_eq!(
            app.state.read(cx).config().general.reconcile_interval_secs,
            600
        );
        enter_into(app, cx, &FieldId::Reconcile, "30");
        assert!(settings.read(cx).errors().contains_key(&FieldId::Reconcile));
        assert_eq!(
            app.state.read(cx).config().general.reconcile_interval_secs,
            600
        );
        enter_into(app, cx, &FieldId::Reconcile, "15m");
        assert_eq!(
            app.state.read(cx).config().general.reconcile_interval_secs,
            900
        );

        // Closing while a field is being typed in applies it.
        type_into(app, cx, &FieldId::Retention, "120");
        app.keys(cx, "escape");
        assert!(app.workspace.read(cx).settings().is_none());
        assert_eq!(
            app.state
                .read(cx)
                .config()
                .general
                .event_log_retention_hours,
            120
        );
    });
}

#[test]
fn notification_rules_change_at_once() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "ctrl-,");
        show(app, cx, SettingsPage::Notifications);
        let settings = panel(app, cx);
        let active = app
            .state
            .read(cx)
            .active_environment_id()
            .map(str::to_owned);
        assert_eq!(settings.read(cx).environment(), active.as_deref());

        // The default rule: warnings too, after five minutes; the
        // overview group with a rule of its own, soft states too.
        let overview = ScopeKey::Group("demo-overview".to_owned());
        settings.update(cx, |settings, cx| {
            settings.set_flag(&ScopeKey::Environment, RuleFlag::Warning, true, cx);
        });
        app.in_window(cx, |window, cx| {
            settings.update(cx, |settings, cx| {
                settings.choose_scope(&overview, 3, window, cx);
            });
        });
        settings.update(cx, |settings, cx| {
            settings.set_flag(&overview, RuleFlag::HardOnly, false, cx);
        });
        app.draw(cx);
        enter_into(app, cx, &FieldId::MinDuration(ScopeKey::Environment), "5m");
        enter_into(app, cx, &FieldId::MinDuration(overview.clone()), "90s");
        // A storm threshold of 0 isn't applied: the problem shows.
        enter_into(app, cx, &FieldId::StormThreshold, "0");
        assert!(
            settings
                .read(cx)
                .errors()
                .contains_key(&FieldId::StormThreshold)
        );
        assert_eq!(
            app.state
                .read(cx)
                .environment()
                .unwrap()
                .notifications
                .storm
                .threshold,
            5
        );
        enter_into(app, cx, &FieldId::StormThreshold, "8");
        assert!(settings.read(cx).errors().is_empty());

        // Quiet hours from 23:30 to 06:00, critical stays audible.
        settings.update(cx, |settings, cx| settings.set_quiet_hours(true, cx));
        app.draw(cx);
        enter_into(app, cx, &FieldId::QuietStart, "23:30");
        enter_into(app, cx, &FieldId::QuietEnd, "6");
        assert_eq!(
            settings
                .read(cx)
                .input(&FieldId::QuietEnd)
                .unwrap()
                .read(cx)
                .value(),
            "06:00"
        );

        let state = app.state.read(cx);
        let notifications = &state.environment().unwrap().notifications;
        assert!(notifications.default_rule.states.warning);
        assert_eq!(notifications.default_rule.min_duration_secs, 300);
        assert_eq!(notifications.storm.threshold, 8);
        let quiet = notifications.quiet_hours;
        assert!(quiet.enabled && quiet.allow_critical);
        assert_eq!(
            (quiet.start_minute, quiet.end_minute),
            (23 * 60 + 30, 6 * 60)
        );
        let group = state
            .groups()
            .iter()
            .find(|group| group.id == "demo-overview")
            .unwrap();
        let ScopeSetting::Custom(rule) = &group.notifications else {
            panic!("{:?}", group.notifications);
        };
        assert!(!rule.hard_only);
        assert!(rule.states.warning, "started from the environment's rule");
        assert_eq!(rule.min_duration_secs, 90);
    });
}

#[test]
fn the_custom_rule_menu_item_opens_the_rule_and_the_plugin_output_switch() {
    run(FixtureOptions::default(), |app, cx| {
        // The sidebar's "custom rule" opens the notifications page with
        // that dashboard's own rule.
        let production = super::production();
        let key = ScopeKey::Dashboard(production.group_id.clone(), production.dashboard_id.clone());
        app.in_window(cx, |window, cx| {
            app.workspace.update(cx, |workspace, cx| {
                workspace.open_settings(SettingsPage::Notifications, Some(&key), window, cx);
            });
        });
        app.draw(cx);
        assert_eq!(panel(app, cx).read(cx).page(), SettingsPage::Notifications);
        let state = app.state.read(cx);
        let dashboard = state.dashboard(&production).unwrap().1;
        assert!(matches!(dashboard.notifications, ScopeSetting::Custom(_)));

        // Show plugin output, the fourth row of the page.
        assert!(app.state.read(cx).config().general.show_plugin_output);
        app.click(
            cx,
            point(px(SWITCH_X), px(FIRST_ROW_Y + 3. * ROW_STEP)),
            Modifiers::default(),
        );
        assert!(!app.state.read(cx).config().general.show_plugin_output);
    });
}

#[test]
fn the_keymap_page_lists_the_shortcuts_and_filters_them() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "ctrl-,");
        show(app, cx, SettingsPage::Keymap);
        let settings = panel(app, cx);
        let all = settings.read(cx).shortcut_labels(cx);
        assert!(
            all.iter().any(|label| label == "command palette"),
            "{all:?}"
        );
        assert!(all.iter().any(|label| label == "select dashboard 1 to 9"));
        let filter = settings.read(cx).keymap_filter().clone();
        app.in_window(cx, |window, cx| {
            filter.update(cx, |filter, cx| filter.replace_all("ctrl-k", window, cx));
        });
        app.draw(cx);
        assert_eq!(
            settings.read(cx).shortcut_labels(cx),
            ["command palette"],
            "by key"
        );
    });
}

#[test]
fn the_files_and_folders_open_in_their_applications() {
    run(FixtureOptions::default(), |app, cx| {
        let dir = tempfile::tempdir().unwrap();
        let locations = Locations {
            settings_file: Some(dir.path().join("config.toml")),
            keymap_file: Some(dir.path().join("keymap.toml")),
            config_dir: Some(dir.path().to_path_buf()),
            log_dir: Some(dir.path().join("logs")),
        };
        app.keys(cx, "ctrl-,");
        panel(app, cx).update(cx, |panel, _| panel.set_locations(locations.clone()));
        app.draw(cx);
        OPENED.with(|opened| opened.borrow_mut().clear());
        // *edit in settings file* writes the settings first if there is no
        // file yet.
        app.click(cx, point(px(1110.), px(90.)), Modifiers::default());
        assert!(dir.path().join("config.toml").exists());
        // *edit keymap file* creates the commented template.
        show(app, cx, SettingsPage::Keymap);
        app.click(cx, point(px(1130.), px(90.)), Modifiers::default());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("keymap.toml")).unwrap(),
            ic_config::KEYMAP_TEMPLATE
        );
        // The advanced page's folders.
        show(app, cx, SettingsPage::Advanced);
        app.click(cx, point(px(1165.), px(257.)), Modifiers::default());
        app.click(cx, point(px(1165.), px(364.)), Modifiers::default());
        let opened = OPENED.with(|opened| opened.borrow().clone());
        assert_eq!(
            opened,
            [
                dir.path().join("config.toml"),
                dir.path().join("keymap.toml"),
                dir.path().join("logs"),
                dir.path().to_path_buf(),
            ]
        );
    });
}

#[test]
fn the_general_switches_reach_the_tray_and_the_login_entry() {
    let config: Config = AppState::fixture(Timestamp::now()).config().clone();
    run_app(
        crate::WINDOW_SIZE,
        move |cx| cx.new(|_| AppState::live(config, UiState::default(), Timestamp::now())),
        Body::Async(Box::new(|app, cx| {
            async move {
                crate::background::autostart::tests::REQUESTS
                    .lock()
                    .unwrap()
                    .clear();
                cx.update(|cx| app.keys(cx, "ctrl-,"));
                // Whether this desktop shows tray icons is asked off the UI
                // thread; the tray row says so once it's known.
                wait_for(
                    &app,
                    &cx,
                    "the tray check",
                    Duration::from_secs(5),
                    |app, cx| {
                        app.workspace
                            .read(cx)
                            .settings()
                            .is_some_and(|settings| settings.read(cx).tray_host().is_some())
                    },
                )
                .await;
                cx.update(|cx| {
                    app.click(
                        cx,
                        point(px(SWITCH_X), px(FIRST_ROW_Y + ROW_STEP)),
                        Modifiers::default(),
                    );
                    app.click(
                        cx,
                        point(px(SWITCH_X), px(FIRST_ROW_Y + 2. * ROW_STEP)),
                        Modifiers::default(),
                    );
                });
                cx.update(|cx| {
                    let general = &app.state.read(cx).config().general;
                    assert!(general.launch_at_login, "switched on at once");
                    assert!(!general.quiet_when_hidden, "switched off at once");
                    assert!(general.close_to_tray, "the others as they were");
                });
                // The login entry is written off the UI thread.
                wait_for(
                    &app,
                    &cx,
                    "the login entry",
                    Duration::from_secs(5),
                    |_, _| {
                        *crate::background::autostart::tests::REQUESTS
                            .lock()
                            .unwrap()
                            == [true]
                    },
                )
                .await;
            }
            .boxed_local()
        })),
    );
}
