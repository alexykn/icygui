//! The app around its window (BG-01, BG-05, NOTE-01): the application
//! menus and their actions, a closed main window coming back with its
//! state, closing it asking first when unsaved work would be lost, and
//! desktop notifications from the real core (the demo's
//! problem storm) whose *Acknowledge* opens the acknowledge dialog for
//! their object, but never for an object of another environment.

use std::time::Duration;

use futures::FutureExt as _;
use gpui::{App, AppContext as _, Entity, OwnedMenuItem};
use ic_core::ports::Notifier as _;
use ic_model::Timestamp;
use ic_rules::{NotificationIntent, Tone};

use super::live::LOAD;
use super::{Body, network, production, replication, run, run_app, wait_for};
use crate::actions::{ActionRequest, ObjectAction, OpenSettings, ShowAbout};
use crate::app_state::AppState;
use crate::app_state::testing::Recorder;
use crate::background::{menus, window};
use crate::dashboard::DashboardEvent;
use crate::fixture::FixtureOptions;
use crate::live::demo::{self, DemoOptions};
use crate::live::desktop::{ACKNOWLEDGE_ACTION, Response, Urgency};
use crate::live::{self, Launch, Session};
use crate::operate::dialog::{ActionDialog, DialogKind};
use crate::operate::forms::FormField;
use crate::workspace::{self, Confirmed, ModalKind};

#[test]
fn the_app_menus_open_settings_and_about() {
    run(FixtureOptions::default(), |app, cx| {
        menus::install(cx);
        let menus = cx.get_menus().unwrap();
        let names: Vec<String> = menus.iter().map(|menu| menu.name.to_string()).collect();
        assert_eq!(names, ["icygui", "Edit", "View", "Window"]);
        let app_menu: Vec<String> = menus[0]
            .items
            .iter()
            .filter_map(|item| match item {
                OwnedMenuItem::Action { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(app_menu, ["About icygui", "Settings…", "Quit icygui"]);

        // The menu's actions reach the window (also when it doesn't have
        // the focus: the global handlers).
        cx.dispatch_action(&ShowAbout);
        app.draw(cx);
        assert_eq!(app.workspace.read(cx).modal(cx), Some(ModalKind::About));
        app.keys(cx, "escape");
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        cx.dispatch_action(&OpenSettings);
        app.draw(cx);
        assert_eq!(app.workspace.read(cx).modal(cx), Some(ModalKind::Settings));
    });
}

#[test]
fn a_closed_window_comes_back_with_its_state() {
    run(FixtureOptions::default(), |app, cx| {
        window::install(app.state.clone(), cx);
        app.state.update(cx, |state, cx| {
            state.select(network());
            cx.notify();
        });
        assert!(window::show(cx), "brings the open window forward");
        // Closing it keeps the app (QuitMode::Explicit); the state stays.
        app.in_window(cx, |window, _| window.remove_window());
        assert!(window::main_window(cx).is_none());
        assert_eq!(app.state.read(cx).selected(), Some(&network()));

        // A notification's click opens it again, showing the object.
        assert!(live::open_object(&replication(), cx));
        let reopened = window::main_window(cx).expect("a new window");
        let workspace = reopened
            .read(cx)
            .unwrap()
            .view()
            .clone()
            .downcast::<workspace::Workspace>()
            .unwrap();
        let shown = workspace.read(cx).dashboard().read(cx).pane_object(cx);
        assert_eq!(shown, Some(replication()));
    });
}

#[test]
fn closing_the_window_asks_before_unsaved_work_is_lost() {
    run(FixtureOptions::default(), |app, cx| {
        window::install(app.state.clone(), cx);
        let recorder = Recorder::default();
        app.state
            .update(cx, |state, _| state.set_core(Box::new(recorder.clone())));
        let close = |cx: &mut App| {
            app.in_window(cx, workspace::close_window);
        };
        let asks = |cx: &App, lost: &str| {
            matches!(
                app.workspace.read(cx).modal(cx),
                Some(ModalKind::Confirm(confirmation))
                    if confirmation.action == Confirmed::CloseWindow
                        && confirmation.detail.contains(lost)
            )
        };

        // A comment typed in the acknowledge dialog: the window asks, and
        // stays open with the dialog and its text when told so.
        app.state.update(cx, |state, cx| {
            let _ = state.request(ActionRequest {
                action: ObjectAction::Acknowledge,
                targets: vec![replication()],
                review: false,
            });
            cx.notify();
        });
        app.draw(cx);
        let dialog = app.workspace.read(cx).action_dialog().unwrap().clone();
        app.in_window(cx, |window, cx| {
            ActionDialog::type_into(&dialog, FormField::Comment, "looking into it", window, cx);
        });
        app.draw(cx);
        close(cx);
        app.draw(cx);
        assert!(window::main_window(cx).is_some(), "still open");
        assert!(asks(cx, "what you typed in “Acknowledge”"));
        app.keys(cx, "escape");
        assert_eq!(
            app.workspace.read(cx).modal(cx),
            Some(ModalKind::Action(DialogKind::Acknowledge)),
            "the dialog is back"
        );
        assert!(
            app.workspace
                .read(cx)
                .action_dialog()
                .unwrap()
                .read(cx)
                .is_dirty(),
            "with its text"
        );
        app.keys(cx, "escape");
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        assert!(recorder.actions().is_empty(), "nothing was sent");

        // The dashboard editor with changes: asks; confirming closes the
        // window (the app's state stays for the next one).
        app.dashboard(cx)
            .update(cx, |_, cx| cx.emit(DashboardEvent::Edit(production())));
        app.draw(cx);
        let name = app
            .workspace
            .read(cx)
            .editor()
            .unwrap()
            .read(cx)
            .name_input()
            .clone();
        app.in_window(cx, |window, cx| {
            name.update(cx, |input, cx| input.replace_all("prod", window, cx));
        });
        app.draw(cx);
        close(cx);
        app.draw(cx);
        assert!(asks(cx, "your changes to prod"));
        app.keys(cx, "escape");
        assert_eq!(app.workspace.read(cx).modal(cx), None, "kept editing");
        assert!(app.workspace.read(cx).editor().is_some());
        close(cx);
        app.in_window(cx, |window, cx| {
            app.workspace
                .update(cx, |workspace, cx| workspace.confirm(window, cx));
        });
        assert!(window::main_window(cx).is_none(), "closed");
    });
}

/// The demo through the real core, with a problem storm a few seconds in.
fn storming_demo(cx: &mut App) -> Entity<AppState> {
    demo_with_storms(Some(5), cx)
}

/// The demo through the real core, with a problem storm every
/// `storm_every` seconds.
fn demo_with_storms(storm_every: Option<u64>, cx: &mut App) -> Entity<AppState> {
    let state = cx.new(|_| AppState::demo(demo::config(), Timestamp::now()));
    let session = Session::install(
        state.clone(),
        Launch::Demo {
            options: DemoOptions {
                scenario: "prod-cluster".to_owned(),
                seed: 3,
                fault: None,
                storm_every,
            },
        },
        None,
        cx,
    );
    session.update(cx, Session::start);
    state
}

/// A critical problem of `prod-cluster` on the desktop: the demo has
/// several environments, so the title starts with its name (A1).
const PROD_CRITICAL: &str = "prod-cluster · CRITICAL · ";

#[test]
fn desktop_notifications_open_and_acknowledge_their_object() {
    run_app(
        crate::WINDOW_SIZE,
        storming_demo,
        Body::Async(Box::new(|app, cx| {
            async move {
                let session = cx.update(|cx| live::session(cx).unwrap());
                wait_for(
                    &app,
                    &cx,
                    "a desktop notification",
                    LOAD + Duration::from_secs(40),
                    |_, cx| {
                        session
                            .read(cx)
                            .shown_notifications()
                            .iter()
                            .any(|posted| posted.title.starts_with(PROD_CRITICAL))
                    },
                )
                .await;
                let posted = cx.update(|cx| {
                    session
                        .read(cx)
                        .shown_notifications()
                        .into_iter()
                        .find(|posted| posted.title.starts_with(PROD_CRITICAL))
                        .unwrap()
                });
                assert_eq!(posted.urgency, Urgency::Critical);
                assert_eq!(
                    posted.actions,
                    [(ACKNOWLEDGE_ACTION, "Acknowledge"), ("open", "Open")]
                );
                // It is in the notification centre too, unread.
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert!(
                        state
                            .notification_records()
                            .any(|record| record.intent.id == posted.tag && !record.read)
                    );
                });
                // Acknowledge on the notification: the object shows and its
                // acknowledge dialog opens; the notification counts as read.
                cx.update(|cx| {
                    session.update(cx, |session, cx| {
                        session.notification_clicked(
                            &Response {
                                tag: posted.tag.clone(),
                                action: Some(ACKNOWLEDGE_ACTION.to_owned()),
                            },
                            cx,
                        );
                    });
                });
                cx.update(|cx| app.draw(cx));
                wait_for(
                    &app,
                    &cx,
                    "the acknowledge dialog",
                    Duration::from_secs(5),
                    |app, cx| {
                        app.workspace.read(cx).modal(cx)
                            == Some(ModalKind::Action(DialogKind::Acknowledge))
                    },
                )
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert!(
                        state
                            .notification_records()
                            .any(|record| record.intent.id == posted.tag && record.read),
                        "marked read"
                    );
                    let object = state
                        .notification_records()
                        .find(|record| record.intent.id == posted.tag)
                        .and_then(|record| record.intent.object.clone())
                        .unwrap();
                    assert_eq!(
                        app.pane_object(cx).or_else(|| state.active_tab().cloned()),
                        Some(object),
                        "the object shows"
                    );
                });
            }
            .boxed_local()
        })),
    );
}

/// A1: a notification raised by `prod-cluster` and still queued when the
/// user switched to `staging` keeps its environment: its title names it,
/// and *Acknowledge* opens the dialog for prod-cluster's object without
/// switching back (the dialog sends to prod-cluster's engine).
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one story: queued, switched, posted, acknowledged where it came from"
)]
fn a_notification_keeps_its_environment_after_a_switch() {
    run_app(
        crate::WINDOW_SIZE,
        |cx| demo_with_storms(None, cx),
        Body::Async(Box::new(|app, cx| {
            async move {
                let session = cx.update(|cx| live::session(cx).unwrap());
                wait_for(&app, &cx, "prod-cluster", LOAD, |app, cx| {
                    let state = app.state.read(cx);
                    state.connection().is_connected()
                        && state
                            .snapshot_of(demo::ENVIRONMENT_ID)
                            .is_some_and(|snapshot| {
                                snapshot.services.contains_key(&ic_model::ServiceKey::new(
                                    "db-prod-03",
                                    "postgres-replication",
                                ))
                            })
                })
                .await;
                cx.update(|cx| {
                    session.update(cx, |session, cx| {
                        session
                            .engine_notifier(demo::ENVIRONMENT_ID)
                            .notify(&NotificationIntent {
                                id: "db-prod-03!postgres-replication:critical:1".to_owned(),
                                object: Some(replication()),
                                title: "CRITICAL · postgres-replication on db-prod-03".to_owned(),
                                subtitle: "overview / production".to_owned(),
                                body: "CRITICAL - standby lag 412s (> 300s)".to_owned(),
                                tone: Tone::Critical,
                                sound: true,
                                silent: false,
                                silenced: None,
                                at: Timestamp::now(),
                            });
                        assert!(session.switch_environment(demo::STAGING_ID, cx));
                    });
                });
                wait_for(
                    &app,
                    &cx,
                    "the queued notification",
                    Duration::from_secs(5),
                    |_, cx| {
                        session
                            .read(cx)
                            .shown_notifications()
                            .iter()
                            .any(|posted| posted.tag.ends_with(":critical:1"))
                    },
                )
                .await;
                let posted = cx.update(|cx| {
                    session
                        .read(cx)
                        .shown_notifications()
                        .into_iter()
                        .find(|posted| posted.tag.ends_with(":critical:1"))
                        .unwrap()
                });
                assert_eq!(
                    posted.title,
                    "prod-cluster · CRITICAL · postgres-replication on db-prod-03"
                );
                assert_eq!(
                    posted.actions,
                    [(ACKNOWLEDGE_ACTION, "Acknowledge"), ("open", "Open")],
                    "Acknowledge goes to its own environment"
                );
                cx.update(|cx| {
                    session.update(cx, |session, cx| {
                        session.notification_clicked(
                            &Response {
                                tag: posted.tag.clone(),
                                action: Some(ACKNOWLEDGE_ACTION.to_owned()),
                            },
                            cx,
                        );
                    });
                });
                cx.update(|cx| app.draw(cx));
                wait_for(
                    &app,
                    &cx,
                    "the acknowledge dialog",
                    Duration::from_secs(5),
                    |app, cx| {
                        app.workspace.read(cx).modal(cx)
                            == Some(ModalKind::Action(DialogKind::Acknowledge))
                    },
                )
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert_eq!(
                        state.active_environment_id(),
                        Some(demo::STAGING_ID),
                        "no switch"
                    );
                    assert_eq!(app.pane_object(cx), None, "nothing revealed in staging");
                });
            }
            .boxed_local()
        })),
    );
}
