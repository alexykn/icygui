//! The app around its window (BG-01, BG-05, NOTE-01): the application
//! menus and their actions, a closed main window coming back with its
//! state, and desktop notifications from the real core (the demo's
//! problem storm) whose *Acknowledge* opens the acknowledge dialog for
//! their object.

use std::time::Duration;

use futures::FutureExt as _;
use gpui::{App, AppContext as _, Entity, OwnedMenuItem};
use ic_model::Timestamp;

use super::live::LOAD;
use super::{Body, network, replication, run, run_app, wait_for};
use crate::actions::{OpenSettings, ShowAbout};
use crate::app_state::AppState;
use crate::background::{menus, window};
use crate::fixture::FixtureOptions;
use crate::live::demo::{self, DemoOptions};
use crate::live::desktop::{ACKNOWLEDGE_ACTION, Response, Urgency};
use crate::live::{self, Launch, Session};
use crate::operate::dialog::DialogKind;
use crate::workspace::ModalKind;

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
            .downcast::<crate::workspace::Workspace>()
            .unwrap();
        let shown = workspace.read(cx).dashboard().read(cx).pane_object(cx);
        assert_eq!(shown, Some(replication()));
    });
}

/// The demo through the real core, with a problem storm a few seconds in.
fn storming_demo(cx: &mut App) -> Entity<AppState> {
    let state = cx.new(|_| AppState::demo(demo::config(), Timestamp::now()));
    let session = Session::install(
        state.clone(),
        Launch::Demo {
            options: DemoOptions {
                scenario: "prod-cluster".to_owned(),
                seed: 3,
                fault: None,
                storm_every: Some(5),
            },
        },
        None,
        cx,
    );
    session.update(cx, Session::start);
    state
}

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
                            .any(|posted| posted.title.starts_with("CRITICAL · "))
                    },
                )
                .await;
                let posted = cx.update(|cx| {
                    session
                        .read(cx)
                        .shown_notifications()
                        .into_iter()
                        .find(|posted| posted.title.starts_with("CRITICAL · "))
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
