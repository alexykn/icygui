//! Notifications in the window (NOTE-02..06, PANE-04) on the fixture: the
//! notification centre from the footer's clock (unread badge, opening an
//! entry, marking everything read), pausing from the palette, the
//! settings dialog (every rule field, a custom scope, quiet hours, storm
//! control with a bad value, the general tab and launch at login),
//! watching and muting from the pane's menu and the palette, and the
//! history from the local log.

use std::time::Duration;

use futures::FutureExt as _;
use gpui::{App, AppContext as _, Entity, Modifiers, point, px};
use ic_config::{Config, UiState};
use ic_core::{CoreEvent, LogEntry, LogKind, NotificationRecord};
use ic_model::{CheckableState, HostState, ObjectKey, ServiceState, StateType, Timestamp};
use ic_rules::{NotificationIntent, ObjectMode, ScopeSetting, Tone};

use super::{Body, Harness, replication, run, run_app, wait_for};
use crate::app_state::AppState;
use crate::fixture::FixtureOptions;
use crate::pane::HostTab;
use crate::settings::{FieldId, RuleFlag, ScopeKey, SettingsDialog, SettingsTab};
use crate::workspace::ModalKind;

/// The footer's clock (the notification centre).
const CLOCK: gpui::Point<gpui::Pixels> = gpui::Point {
    x: px(50.),
    y: px(880.),
};

fn record(
    id: &str,
    title: &str,
    object: Option<ObjectKey>,
    tone: Tone,
    silent: bool,
    ago: f64,
) -> NotificationRecord {
    NotificationRecord {
        intent: NotificationIntent {
            id: id.to_owned(),
            object,
            title: title.to_owned(),
            subtitle: "overview / production".to_owned(),
            body: "CRITICAL - standby lag 412s (> 300s)".to_owned(),
            tone,
            sound: true,
            silent,
            silenced: silent.then_some(ic_rules::Silence::QuietHours),
            at: Timestamp::from_unix_seconds(Timestamp::now().as_unix_seconds() - ago),
        },
        read: false,
    }
}

fn settings(app: &Harness, cx: &App) -> Entity<SettingsDialog> {
    app.workspace.read(cx).settings().unwrap().clone()
}

/// Replaces a settings field's text, as typing would.
fn type_into(app: &Harness, cx: &mut App, id: &FieldId, text: &str) {
    let input = settings(app, cx).read(cx).input(id).unwrap().clone();
    app.in_window(cx, |window, cx| {
        input.update(cx, |input, cx| input.replace_all(text, window, cx));
    });
    app.draw(cx);
}

#[test]
fn the_notification_centre_lists_opens_and_marks_read() {
    run(FixtureOptions::default(), |app, cx| {
        // Newest first: a storm summary (silent, no object), then the
        // design's problem (the second entry).
        app.state.update(cx, |state, cx| {
            state.apply(CoreEvent::Notification(record(
                "replication",
                "CRITICAL · postgres-replication on db-prod-03",
                Some(replication()),
                Tone::Critical,
                false,
                120.,
            )));
            state.apply(CoreEvent::Notification(record(
                "storm",
                "14 new problems in prod-cluster",
                None,
                Tone::Info,
                true,
                10.,
            )));
            cx.notify();
        });
        app.draw(cx);
        assert_eq!(app.state.read(cx).unread_notifications(), 2);

        // The clock opens the centre.
        app.click(cx, CLOCK, Modifiers::default());
        let sidebar = app.workspace.read(cx).sidebar().clone();
        assert!(sidebar.read(cx).notifications_open());

        // The second entry (the problem), under the section's title and
        // the first entry at the list's top: a click opens its object and
        // marks it read.
        let list = sidebar.read(cx).centre_list_bounds();
        app.click(
            cx,
            point(px(200.), list.origin.y + px(115.)),
            Modifiers::default(),
        );
        assert_eq!(app.pane_object(cx), Some(replication()));
        assert!(!sidebar.read(cx).notifications_open(), "closed");
        let state = app.state.read(cx);
        assert_eq!(state.unread_notifications(), 1);
        let read: Vec<(String, bool)> = state
            .notification_records()
            .map(|record| (record.intent.id.clone(), record.read))
            .collect();
        assert_eq!(
            read,
            [
                ("storm".to_owned(), false),
                ("replication".to_owned(), true)
            ]
        );

        // The palette marks the rest read.
        app.keys(cx, "ctrl-k");
        let palette = app.workspace.read(cx).palette().unwrap().clone();
        let input = palette.read(cx).input().clone();
        app.in_window(cx, |window, cx| {
            input.update(cx, |input, cx| input.replace_all("mark all", window, cx));
        });
        app.draw(cx);
        app.keys(cx, "enter");
        assert_eq!(app.state.read(cx).unread_notifications(), 0);
    });
}

/// NOTE-05: while the pointer is over the centre's list, a notification
/// arriving waits (the heading counts it at once), so the entry under the
/// pointer stays the one a click opens; leaving the list shows it.
#[test]
fn arrivals_wait_while_the_pointer_is_over_the_centres_list() {
    run(FixtureOptions::default(), |app, cx| {
        let push = |cx: &mut App, id: &str, ago: f64| {
            app.state.update(cx, |state, cx| {
                state.apply(CoreEvent::Notification(record(
                    id,
                    "CRITICAL · postgres-replication on db-prod-03",
                    Some(replication()),
                    Tone::Critical,
                    false,
                    ago,
                )));
                cx.notify();
            });
            app.draw(cx);
        };
        push(cx, "first", 60.);
        app.click(cx, CLOCK, Modifiers::default());
        let sidebar = app.workspace.read(cx).sidebar().clone();
        let listed = |cx: &App| -> Vec<String> {
            sidebar
                .read(cx)
                .centre_view(cx)
                .entries()
                .map(|entry| entry.id.clone())
                .collect()
        };
        let list = sidebar.read(cx).centre_list_bounds();
        app.hover(cx, point(px(200.), list.origin.y + px(60.)));
        assert!(sidebar.read(cx).centre_holds());

        push(cx, "second", 1.);
        assert_eq!(listed(cx), ["first"], "waits while the pointer is there");
        assert_eq!(sidebar.read(cx).centre_view(cx).unread, 2, "counted");

        // Away from the list (over the heading): it shows, newest first.
        app.hover(cx, point(px(200.), list.origin.y - px(60.)));
        assert!(!sidebar.read(cx).centre_holds());
        assert_eq!(listed(cx), ["second", "first"]);
    });
}

#[test]
fn notifications_pause_and_resume_from_the_palette() {
    run(FixtureOptions::default(), |app, cx| {
        let run_command = |cx: &mut App, query: &str| {
            app.keys(cx, "ctrl-k");
            let palette = app.workspace.read(cx).palette().unwrap().clone();
            let input = palette.read(cx).input().clone();
            app.in_window(cx, |window, cx| {
                input.update(cx, |input, cx| input.replace_all(query, window, cx));
            });
            app.draw(cx);
            app.keys(cx, "enter");
        };
        run_command(cx, "pause notifications for 30");
        let until = app.state.read(cx).paused_until().unwrap();
        let ahead = until.as_unix_seconds() - Timestamp::now().as_unix_seconds();
        assert!((1790. ..=1800.).contains(&ahead), "{ahead}");
        assert!(app.state.read(cx).is_paused(Timestamp::now()));
        run_command(cx, "resume notif");
        assert_eq!(app.state.read(cx).paused_until(), None);
    });
}

#[test]
fn the_settings_edit_every_rule_and_refuse_bad_values() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "ctrl-,");
        assert_eq!(app.workspace.read(cx).modal(cx), Some(ModalKind::Settings));
        let dialog = settings(app, cx);
        assert_eq!(dialog.read(cx).tab(), SettingsTab::General);
        dialog.update(cx, |dialog, cx| {
            dialog.show_tab(SettingsTab::Notifications, cx);
        });
        app.draw(cx);

        // The default rule: warnings too, after five minutes; the
        // overview group with a rule of its own, soft states too.
        let overview = ScopeKey::Group("demo-overview".to_owned());
        dialog.update(cx, |dialog, cx| {
            dialog.set_flag(&ScopeKey::Environment, RuleFlag::Warning, true, cx);
        });
        app.in_window(cx, |window, cx| {
            dialog.update(cx, |dialog, cx| {
                dialog.choose_scope(&overview, 3, window, cx);
            });
        });
        dialog.update(cx, |dialog, cx| {
            dialog.set_flag(&overview, RuleFlag::HardOnly, false, cx);
        });
        app.draw(cx);
        type_into(app, cx, &FieldId::MinDuration(ScopeKey::Environment), "5m");
        type_into(app, cx, &FieldId::MinDuration(overview.clone()), "90s");
        // A storm threshold of 0 isn't saved: the problem shows.
        type_into(app, cx, &FieldId::StormThreshold, "0");
        app.keys(cx, "ctrl-s");
        assert_eq!(app.workspace.read(cx).modal(cx), Some(ModalKind::Settings));
        assert!(
            settings(app, cx)
                .read(cx)
                .errors()
                .contains_key(&FieldId::StormThreshold)
        );
        type_into(app, cx, &FieldId::StormThreshold, "8");
        assert!(
            settings(app, cx).read(cx).errors().is_empty(),
            "fixed as typed"
        );
        app.keys(cx, "ctrl-s");
        assert_eq!(app.workspace.read(cx).modal(cx), None, "saved and closed");

        let state = app.state.read(cx);
        let notifications = &state.environment().unwrap().notifications;
        assert!(notifications.default_rule.states.warning);
        assert_eq!(notifications.default_rule.min_duration_secs, 300);
        assert_eq!(notifications.storm.threshold, 8);
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
fn quiet_hours_and_the_custom_rule_menu_item() {
    run(FixtureOptions::default(), |app, cx| {
        // The sidebar's "custom rule" opens the settings with that
        // dashboard's own rule.
        let production = super::production();
        let key = ScopeKey::Dashboard(production.group_id.clone(), production.dashboard_id.clone());
        app.in_window(cx, |window, cx| {
            app.workspace.update(cx, |workspace, cx| {
                workspace.open_settings(SettingsTab::Notifications, Some(&key), window, cx);
            });
        });
        app.draw(cx);
        let dialog = settings(app, cx);
        assert_eq!(dialog.read(cx).tab(), SettingsTab::Notifications);
        assert!(matches!(
            dialog.read(cx).plan().unwrap().setting(&key),
            Some(ScopeSetting::Custom(_))
        ));
        // Quiet hours from 23:30 to 06:00, critical stays audible.
        dialog.update(cx, |dialog, cx| dialog.set_quiet_hours(true, cx));
        app.draw(cx);
        type_into(app, cx, &FieldId::QuietStart, "23:30");
        type_into(app, cx, &FieldId::QuietEnd, "6");
        app.keys(cx, "ctrl-s");
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        let state = app.state.read(cx);
        let environment = state.environment().unwrap();
        let quiet = environment.notifications.quiet_hours;
        assert!(quiet.enabled && quiet.allow_critical);
        assert_eq!(
            (quiet.start_minute, quiet.end_minute),
            (23 * 60 + 30, 6 * 60)
        );
        let dashboard = state.dashboard(&production).unwrap().1;
        assert!(matches!(dashboard.notifications, ScopeSetting::Custom(_)));
    });
}

#[test]
fn the_general_settings_switch_the_tray_and_launch_at_login() {
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
                // Each step that a subscriber reacts to is its own update
                // (effects flush at the end of the outermost one).
                cx.update(|cx| app.keys(cx, "ctrl-,"));
                // The hint under the tray switch says whether this desktop
                // shows tray icons (none here: two lines) once it's known.
                wait_for(
                    &app,
                    &cx,
                    "the tray check",
                    Duration::from_secs(5),
                    |app, cx| {
                        app.workspace
                            .read(cx)
                            .settings()
                            .is_some_and(|dialog| dialog.read(cx).tray_host().is_some())
                    },
                )
                .await;
                cx.update(|cx| {
                    assert_eq!(app.workspace.read(cx).modal(cx), Some(ModalKind::Settings));
                    // The switches where the dialog draws them (1440 × 900);
                    // the tray hint has two lines without a tray host, one
                    // with (on a desktop running the tests).
                    let host = settings(&app, cx).read(cx).tray_host() == Some(true);
                    let login = if host { 200. } else { 216. };
                    app.click(cx, point(px(405.), px(146.)), Modifiers::default());
                    app.click(cx, point(px(405.), px(login)), Modifiers::default());
                    type_into(&app, cx, &FieldId::Retention, "72");
                });
                cx.update(|cx| app.keys(cx, "ctrl-s"));
                cx.update(|cx| {
                    assert_eq!(app.workspace.read(cx).modal(cx), None, "saved and closed");
                    let general = &app.state.read(cx).config().general;
                    assert!(!general.close_to_tray, "switched off");
                    assert!(general.launch_at_login, "switched on");
                    assert_eq!(general.event_log_retention_hours, 72);
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

#[test]
fn objects_are_watched_and_muted_from_the_pane_and_the_palette() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "j j enter");
        assert_eq!(app.pane_object(cx), Some(replication()));
        let palette_run = |cx: &mut App, query: &str| {
            app.keys(cx, "ctrl-k");
            let palette = app.workspace.read(cx).palette().unwrap().clone();
            let input = palette.read(cx).input().clone();
            app.in_window(cx, |window, cx| {
                input.update(cx, |input, cx| input.replace_all(query, window, cx));
            });
            app.draw(cx);
            app.keys(cx, "enter");
        };
        palette_run(cx, "mute for 1 hour");
        let now = Timestamp::now();
        let entry = app
            .state
            .read(cx)
            .object_override(&replication(), now)
            .cloned()
            .unwrap();
        assert_eq!(entry.mode, ObjectMode::Mute);
        let ahead = entry.until.unwrap().as_unix_seconds() - now.as_unix_seconds();
        assert!((3500. ..=3600.).contains(&ahead), "{ahead}");
        assert!(
            app.state
                .read(cx)
                .toasts()
                .any(|toast| toast.title.starts_with("Muted postgres-replication")),
            "a toast says so"
        );

        // The pane's menu offers watching, other mutes and unmuting.
        let pane = app.dashboard(cx).read(cx).pane(cx).unwrap();
        let labels = pane.update(cx, |pane, cx| pane.more_menu_labels(None, cx));
        assert!(
            labels.contains(&"watch: always notify".to_owned()),
            "{labels:?}"
        );
        assert!(labels.contains(&"unmute".to_owned()), "{labels:?}");

        palette_run(cx, "watch");
        assert_eq!(
            app.state
                .read(cx)
                .object_override(&replication(), Timestamp::now())
                .map(|entry| entry.mode),
            Some(ObjectMode::Watch)
        );
        palette_run(cx, "stop watching");
        assert!(
            app.state
                .read(cx)
                .object_override(&replication(), Timestamp::now())
                .is_none()
        );
    });
}

#[test]
fn the_history_shows_what_the_local_log_recorded() {
    let host = ObjectKey::host("db-prod-03");
    let entries = vec![
        LogEntry {
            at: Timestamp::now(),
            object: replication(),
            kind: LogKind::AcknowledgementSet,
            text: "on it".to_owned(),
            author: Some("m.keller".to_owned()),
        },
        LogEntry {
            at: Timestamp::from_unix_seconds(Timestamp::now().as_unix_seconds() - 600.),
            object: replication(),
            kind: LogKind::State {
                state: CheckableState::Service(ServiceState::Critical),
                state_type: StateType::Hard,
            },
            text: "CRITICAL - standby lag 412s".to_owned(),
            author: None,
        },
        LogEntry {
            at: Timestamp::from_unix_seconds(Timestamp::now().as_unix_seconds() - 900.),
            object: ObjectKey::host("web-prod-01"),
            kind: LogKind::State {
                state: CheckableState::Host(HostState::Down),
                state_type: StateType::Hard,
            },
            text: String::new(),
            author: None,
        },
    ];
    run_app(
        crate::WINDOW_SIZE,
        move |cx| {
            cx.new(|_| {
                let mut state = AppState::fixture(Timestamp::now());
                state.set_fake_history(entries);
                state
            })
        },
        Body::Async(Box::new(move |app, cx| {
            async move {
                // The service pane's section.
                cx.update(|cx| app.keys(cx, "j j enter"));
                wait_for(
                    &app,
                    &cx,
                    "the service's history",
                    Duration::from_secs(5),
                    |app, cx| {
                        let pane = app.dashboard(cx).read(cx).pane(cx).unwrap();
                        pane.read(cx)
                            .history_entries()
                            .is_some_and(|entries| entries.len() == 2)
                    },
                )
                .await;
                // The host's tab: its services' entries too, not other hosts'.
                cx.update(|cx| {
                    let pane = app.dashboard(cx).read(cx).pane(cx).unwrap();
                    pane.update(cx, |pane, cx| {
                        pane.navigate(host.clone(), cx);
                        pane.select_host_tab(HostTab::History, cx);
                    });
                    app.draw(cx);
                });
                wait_for(
                    &app,
                    &cx,
                    "the host's history",
                    Duration::from_secs(5),
                    |app, cx| {
                        let pane = app.dashboard(cx).read(cx).pane(cx).unwrap();
                        pane.read(cx).history_entries().is_some_and(|entries| {
                            entries.len() == 2
                                && entries
                                    .iter()
                                    .all(|entry| entry.object.host_name().as_str() == "db-prod-03")
                        })
                    },
                )
                .await;
            }
            .boxed_local()
        })),
    );
}
