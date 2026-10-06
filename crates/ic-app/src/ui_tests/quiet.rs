//! Quiet mode from the app's side (PERF-09): the environments off screen
//! and the one on screen while the window is hidden are quiet (their
//! Icinga streams them no check results), a state change still notifies
//! at once, and the notification's click brings the window back with the
//! object shown and the environment awake; the object a pane opens is
//! asked for first (`Command::Focus`), once the cursor rests when it moves
//! through the list, and again after waking up; the pane says `updating`
//! after a moment in a fixed slot; and the setting's switch.

use std::time::Duration;

use futures::FutureExt as _;
use gpui::{App, AppContext as _, Modifiers, point, px};
use ic_config::{Config, UiState};
use ic_core::Command;
use ic_core::snapshot::Snapshot;
use ic_model::{ObjectKey, Timestamp};

use super::environments::{CONNECT, demo_app};
use super::every_environment::{
    EVERY, break_it, every_environment_connected, one_stream, posted_about, victim,
};
use super::live::LOAD;
use super::{Body, Harness, row_position, run_app, wait_for};
use crate::app_state::AppState;
use crate::app_state::hydration::row_needs_details;
use crate::app_state::testing::Recorder;
use crate::background::{presence, window};
use crate::fixture::FixtureOptions;
use crate::live::desktop::Response;
use crate::live::{self, demo};
use crate::pane::UPDATING_HINT_AFTER;
use crate::workspace::ModalKind;

/// The Focus commands `core` got, in order.
fn focused(core: &Recorder) -> Vec<String> {
    core.sent()
        .into_iter()
        .filter(|command| command.starts_with("Focus("))
        .collect()
}

fn focus(key: &ObjectKey) -> String {
    format!("{:?}", Command::Focus(key.clone()))
}

/// Whether the pane beside the list shows the `updating` hint.
fn pane_updating(app: &Harness, cx: &App) -> bool {
    app.dashboard(cx)
        .read(cx)
        .pane(cx)
        .is_some_and(|pane| pane.read(cx).shows_updating())
}

/// Publishes the snapshot again with `updating` as the objects being
/// fetched.
fn publish_updating(app: &Harness, cx: &mut App, updating: &[ObjectKey]) {
    app.state.update(cx, |state, cx| {
        let old = state.snapshot().clone();
        state.set_snapshot(std::sync::Arc::new(Snapshot {
            revision: old.revision + 1,
            updating: std::sync::Arc::new(updating.iter().cloned().collect()),
            ..(*old).clone()
        }));
        cx.notify();
    });
    app.draw(cx);
}

/// The fixture with a recording engine: a click opens a row's pane and
/// asks for its object at once; moving the cursor with the keys asks only
/// for the row it rests on; waking up asks again; fresher details that
/// take a moment show the `updating` hint, only after
/// [`UPDATING_HINT_AFTER`].
#[test]
fn the_opened_object_is_asked_for_first_and_says_when_it_updates() {
    run_app(
        crate::WINDOW_SIZE,
        |cx| cx.new(|_| AppState::fixture_with(Timestamp::now(), FixtureOptions::default())),
        Body::Async(Box::new(|app, cx| {
            async move {
                let core = Recorder::default();
                cx.update(|cx| {
                    app.state
                        .update(cx, |state, _| state.set_core(Box::new(core.clone())));
                    app.click(cx, row_position(0), Modifiers::default());
                });
                let first = cx.update(|cx| app.row_key(cx, 0));
                wait_for(&app, &cx, "the clicked row asked for", CONNECT, |_, _| {
                    focused(&core) == [focus(&first)]
                })
                .await;

                // Down three rows: only where the cursor rests.
                cx.update(|cx| app.keys(cx, "j j j"));
                let rested = cx.update(|cx| app.row_key(cx, 3));
                assert_eq!(focused(&core).len(), 1, "nothing while moving");
                wait_for(&app, &cx, "the row the cursor rests on", CONNECT, |_, _| {
                    focused(&core) == [focus(&first), focus(&rested)]
                })
                .await;
                cx.update(|cx| assert_eq!(app.pane_object(cx), Some(rested.clone())));

                // Waking up (the window back): asked again.
                core.clear();
                cx.update(|cx| {
                    app.state.update(cx, |state, _| {
                        assert!(state.set_window_hidden(true));
                        assert!(state.set_window_hidden(false));
                    });
                    app.draw(cx);
                });
                wait_for(&app, &cx, "asked again after waking up", CONNECT, |_, _| {
                    focused(&core) == [focus(&rested)]
                })
                .await;
                assert_eq!(
                    core.sent()[..2],
                    ["SetQuiet(true)".to_owned(), "SetQuiet(false)".to_owned()]
                );

                // The engine fetches it: the hint only after a moment.
                cx.update(|cx| {
                    publish_updating(&app, cx, std::slice::from_ref(&rested));
                    assert!(!pane_updating(&app, cx), "not at once");
                });
                cx.background_executor()
                    .timer(UPDATING_HINT_AFTER / 2)
                    .await;
                cx.update(|cx| {
                    app.draw(cx);
                    assert!(!pane_updating(&app, cx), "not after half the time");
                });
                wait_for(&app, &cx, "the updating hint", CONNECT, |app, cx| {
                    pane_updating(app, cx)
                })
                .await;
                cx.update(|cx| {
                    publish_updating(&app, cx, &[]);
                    assert!(!pane_updating(&app, cx), "gone with the answer");
                });
            }
            .boxed_local()
        })),
    );
}

/// PERF-09 against the demo's mock Icinga servers: prod-cluster on screen
/// streams live while staging and lab stream quiet; the window hidden,
/// prod-cluster turns quiet too and a service turning critical there
/// still notifies at once; the click on that notification brings the
/// window back with the object shown (its output from the state change
/// already there) and prod-cluster live again.
#[test]
fn quiet_mode_follows_the_window_and_notifications_wake_it_up() {
    run_app(
        crate::WINDOW_SIZE,
        |cx| {
            let state = demo_app(None)(cx);
            window::install(state.clone(), cx);
            presence::set_grace(Duration::ZERO, cx);
            state
        },
        Body::Async(Box::new(|app, cx| {
            async move {
                let session = cx.update(|cx| live::session(cx).unwrap());
                every_environment_connected(&app, &cx).await;
                let controls: Vec<_> = cx.update(|cx| {
                    EVERY
                        .iter()
                        .map(|id| session.read(cx).demo_control_of(id).unwrap())
                        .collect()
                });
                let streams = |prod_live: bool| {
                    EVERY.iter().zip(&controls).all(|(id, control)| {
                        one_stream(control, prod_live && *id == demo::ENVIRONMENT_ID)
                    })
                };
                wait_for(&app, &cx, "the others quiet", CONNECT, |_, _| streams(true)).await;

                // Hidden (the grace is zero here): prod-cluster turns quiet.
                cx.update(|cx| presence::hidden(&app.state, cx));
                wait_for(&app, &cx, "every stream quiet", CONNECT, |_, _| {
                    streams(false)
                })
                .await;
                cx.update(|cx| assert!(app.state.read(cx).window_hidden()));

                // A state change still notifies at once.
                let (object, prod) = victim(&cx, &app, &session, demo::ENVIRONMENT_ID);
                break_it(&prod, &object);
                wait_for(&app, &cx, "the notification while quiet", LOAD, |_, cx| {
                    posted_about(session.read(cx), &object).is_some()
                })
                .await;
                let tag = cx.update(|cx| posted_about(session.read(cx), &object).unwrap().tag);

                // Its click: the window back, the object shown with what
                // the state change brought, prod-cluster awake.
                cx.update(|cx| {
                    session.update(cx, |session, cx| {
                        session.notification_clicked(&Response { tag, action: None }, cx);
                    });
                    let state = app.state.read(cx);
                    assert!(!state.window_hidden(), "shown at once");
                });
                wait_for(&app, &cx, "the object shown", CONNECT, |app, cx| {
                    let state = app.state.read(cx);
                    app.pane_object(cx).or_else(|| state.active_tab().cloned())
                        == Some(object.clone())
                })
                .await;
                cx.update(|cx| {
                    let ObjectKey::Service { key } = &object else {
                        unreachable!("a service")
                    };
                    let state = app.state.read(cx);
                    let service = &state.snapshot().services[key];
                    let output = service.check.result.as_ref().map(|result| &result.output);
                    assert_eq!(
                        output.map(String::as_str),
                        Some("CRITICAL - disk full"),
                        "from the state change, no wait"
                    );
                });
                wait_for(&app, &cx, "prod-cluster live again", CONNECT, |_, _| {
                    streams(true)
                })
                .await;
            }
            .boxed_local()
        })),
    );
}

/// A pane opened on a service the engine holds lean (the demo's mock
/// answering slowly): the hint shows while the opened object is fetched
/// ahead of everything, and goes once its details are in.
#[test]
fn a_slow_answer_shows_the_updating_hint_until_the_details_are_in() {
    run_app(
        crate::WINDOW_SIZE,
        super::live::demo_app("prod-cluster", None),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "the first load", LOAD, |app, cx| {
                    app.state.read(cx).connection().is_connected()
                })
                .await;
                let (key, control) = cx.update(|cx| {
                    let state = app.state.read(cx);
                    let snapshot = state.snapshot();
                    let key = snapshot
                        .services
                        .values()
                        .map(|service| service.object_key())
                        .find(|key| row_needs_details(snapshot, key))
                        .expect("a lean service");
                    (key, super::live::control(cx))
                });
                control.set_latency(Duration::from_millis(1_500));
                cx.update(|cx| {
                    app.state.update(cx, |state, cx| {
                        assert!(state.open_tab(key.clone()));
                        cx.notify();
                    });
                    app.draw(cx);
                });
                let tab_updating = |app: &Harness, cx: &App| {
                    app.workspace
                        .read(cx)
                        .tab(&key)
                        .is_some_and(|pane| pane.read(cx).shows_updating())
                };
                wait_for(&app, &cx, "the updating hint", CONNECT, |app, cx| {
                    tab_updating(app, cx)
                })
                .await;
                wait_for(&app, &cx, "the details in", CONNECT, |app, cx| {
                    let state = app.state.read(cx);
                    !row_needs_details(state.snapshot(), &key)
                        && !state.snapshot().is_updating(&key)
                })
                .await;
                control.set_latency(Duration::ZERO);
                cx.update(|cx| {
                    app.draw(cx);
                    assert!(!tab_updating(&app, cx), "the hint goes with the answer");
                });
            }
            .boxed_local()
        })),
    );
}

/// The general settings' switch turns quiet mode off (every environment
/// live) and on again.
#[test]
fn the_quiet_mode_switch_is_saved() {
    let config: Config = AppState::fixture(Timestamp::now()).config().clone();
    run_app(
        crate::WINDOW_SIZE,
        move |cx| cx.new(|_| AppState::live(config, UiState::default(), Timestamp::now())),
        Body::Async(Box::new(|app, cx| {
            async move {
                cx.update(|cx| {
                    assert!(app.state.read(cx).config().general.quiet_when_hidden);
                    app.keys(cx, "ctrl-,");
                });
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
                    // Under *start at login* and its one-line hint; the
                    // tray hint above has two lines without a tray host.
                    let host = app
                        .workspace
                        .read(cx)
                        .settings()
                        .is_some_and(|dialog| dialog.read(cx).tray_host() == Some(true));
                    let quiet = if host { 254. } else { 270. };
                    app.click(cx, point(px(405.), px(quiet)), Modifiers::default());
                });
                cx.update(|cx| app.keys(cx, "ctrl-s"));
                cx.update(|cx| {
                    assert_eq!(app.workspace.read(cx).modal(cx), None, "saved and closed");
                    let general = &app.state.read(cx).config().general;
                    assert!(!general.quiet_when_hidden, "switched off");
                    assert!(general.close_to_tray, "the others as they were");
                    assert!(!general.launch_at_login);
                });
            }
            .boxed_local()
        })),
    );
}
