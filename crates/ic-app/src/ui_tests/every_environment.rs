//! Every environment runs its own engine (PLAN.md D2, NOTE-08), through
//! the real cores against the demo's mock Icinga servers (`prod-cluster`'s
//! master and satellite, `staging`, `lab`: four `ic_mock::MockServer`s in
//! one app): an environment off screen notifies with its name in the title
//! and is acknowledged there without switching; a click on its
//! notification switches and shows the object; switching is instant and
//! keeps every stream; a muted environment stays silent while the others
//! notify, and the pause of every environment silences them all; editing
//! an environment off screen restarts its engine alone; deleting an
//! environment stops its engine and deletes its event log.

use std::time::Duration;

use futures::FutureExt as _;
use gpui::{App, AsyncApp};
use ic_core::snapshot::Snapshot;
use ic_mock::MockControl;
use ic_model::{HostState, ObjectKey, ServiceState, Timestamp};

use super::environments::{CONNECT, demo_app};
use super::live::LOAD;
use super::{Body, Harness, run_app, wait_for};
use crate::live::demo;
use crate::live::desktop::{ACKNOWLEDGE_ACTION, Posted, Response};
use crate::live::{self, Session};
use crate::operate::dialog::{ActionDialog, DialogKind};
use crate::operate::forms::FormField;
use crate::workspace::ModalKind;

/// The demo's environments.
pub(super) const EVERY: [&str; 3] = [demo::ENVIRONMENT_ID, demo::STAGING_ID, demo::LAB_ID];

/// Waits until every demo environment's engine is connected with objects.
pub(super) async fn every_environment_connected(app: &Harness, cx: &AsyncApp) {
    wait_for(
        app,
        cx,
        "every environment connected",
        CONNECT,
        |app, cx| {
            let state = app.state.read(cx);
            EVERY.iter().all(|id| {
                state.slot(id).is_some_and(|slot| {
                    slot.connection().is_connected() && !slot.snapshot().services.is_empty()
                })
            })
        },
    )
    .await;
}

/// An OK service on an UP host, neither acknowledged nor in a downtime:
/// one whose turning critical notifies.
fn quiet_service(snapshot: &Snapshot) -> ObjectKey {
    snapshot
        .services
        .values()
        .find(|service| {
            service.state == ServiceState::Ok
                && service.check.downtime_depth == 0
                && snapshot
                    .hosts
                    .get(&service.key.host)
                    .is_some_and(|host| host.state == HostState::Up)
        })
        .map(|service| service.object_key())
        .expect("an OK service on an UP host")
}

/// A quiet service of environment `id`, and its server's control.
pub(super) fn victim(
    cx: &AsyncApp,
    app: &Harness,
    session: &gpui::Entity<Session>,
    id: &str,
) -> (ObjectKey, MockControl) {
    cx.update(|cx| {
        let object = quiet_service(app.state.read(cx).snapshot_of(id).unwrap());
        let control = session.read(cx).demo_control_of(id).unwrap();
        (object, control)
    })
}

/// Makes `object` a hard CRITICAL in Icinga.
pub(super) fn break_it(control: &MockControl, object: &ObjectKey) {
    let ObjectKey::Service { key } = object else {
        unreachable!("a service")
    };
    control
        .set_service_state(
            key.host.as_str(),
            &key.name,
            ServiceState::Critical,
            "CRITICAL - disk full",
            true,
        )
        .unwrap();
}

/// The desktop notification about `object` turning critical, if one was
/// shown.
pub(super) fn posted_about(session: &Session, object: &ObjectKey) -> Option<Posted> {
    let prefix = format!("{object}:critical:");
    session
        .shown_notifications()
        .into_iter()
        .find(|posted| posted.tag.starts_with(&prefix))
}

/// Whether environment `id` recorded a notification about `object` turning
/// critical: `Some(silent)`.
fn recorded_about(app: &Harness, cx: &App, id: &str, object: &ObjectKey) -> Option<bool> {
    let prefix = format!("{object}:critical:");
    app.state
        .read(cx)
        .slot(id)?
        .notifications()
        .find(|record| record.intent.id.starts_with(&prefix))
        .map(|record| record.intent.silent)
}

fn succeeded(control: &MockControl, action: &str) -> usize {
    control
        .requests()
        .iter()
        .filter(|request| {
            request.path == format!("/v1/actions/{action}") && (200..300).contains(&request.status)
        })
        .count()
}

/// A1: `staging` notifies while `prod-cluster` is on screen, its name in
/// front of the title; *Acknowledge* opens the dialog for its object and
/// the acknowledgement goes to staging's Icinga, without switching; a
/// click on the notification then switches to staging and shows it.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one story: notified off screen, acknowledged there, then shown"
)]
fn an_environment_off_screen_notifies_and_is_acknowledged_there() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(None),
        Body::Async(Box::new(|app, cx| {
            async move {
                let session = cx.update(|cx| live::session(cx).unwrap());
                every_environment_connected(&app, &cx).await;
                let (object, staging) = victim(&cx, &app, &session, demo::STAGING_ID);
                let prod = cx.update(|cx| session.read(cx).demo_control().unwrap());
                break_it(&staging, &object);
                wait_for(&app, &cx, "staging's notification", LOAD, |_, cx| {
                    posted_about(session.read(cx), &object).is_some()
                })
                .await;
                let posted = cx.update(|cx| posted_about(session.read(cx), &object).unwrap());
                assert!(
                    posted.title.starts_with("staging · CRITICAL · "),
                    "{}",
                    posted.title
                );
                assert_eq!(
                    posted.actions,
                    [(ACKNOWLEDGE_ACTION, "Acknowledge"), ("open", "Open")]
                );
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert_eq!(state.active_environment_id(), Some(demo::ENVIRONMENT_ID));
                    assert_eq!(
                        recorded_about(&app, cx, demo::STAGING_ID, &object),
                        Some(false),
                        "in staging's list"
                    );
                    assert!(
                        state
                            .notification_records()
                            .all(|record| record.intent.id != posted.tag),
                        "not in prod-cluster's"
                    );
                });

                // Acknowledge: the dialog for staging's object, prod-cluster
                // stays on screen.
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
                    assert_eq!(state.active_environment_id(), Some(demo::ENVIRONMENT_ID));
                    let read = state
                        .slot(demo::STAGING_ID)
                        .unwrap()
                        .notifications()
                        .any(|record| record.intent.id == posted.tag && record.read);
                    assert!(read, "the notification counts as read");
                    let dialog = app.workspace.read(cx).action_dialog().cloned().unwrap();
                    app.in_window(cx, |window, cx| {
                        ActionDialog::type_into(&dialog, FormField::Comment, "on it", window, cx);
                    });
                    app.draw(cx);
                });
                cx.update(|cx| app.keys(cx, "enter"));
                wait_for(&app, &cx, "the acknowledgement in staging", LOAD, |_, _| {
                    succeeded(&staging, "acknowledge-problem") == 1
                })
                .await;
                let ObjectKey::Service { key } = &object else {
                    unreachable!()
                };
                assert!(
                    staging
                        .service(key.host.as_str(), &key.name)
                        .unwrap()
                        .check
                        .acknowledgement
                        .is_acknowledged()
                );
                assert_eq!(
                    succeeded(&prod, "acknowledge-problem"),
                    0,
                    "nothing went to prod-cluster"
                );
                wait_for(&app, &cx, "the toast", LOAD, |app, cx| {
                    app.state.read(cx).toasts().any(|toast| {
                        toast.title.starts_with("Acknowledged ")
                            && toast.title.ends_with(" in staging")
                    })
                })
                .await;

                // A click on the notification itself switches and shows it.
                cx.update(|cx| {
                    session.update(cx, |session, cx| {
                        session.notification_clicked(
                            &Response {
                                tag: posted.tag.clone(),
                                action: None,
                            },
                            cx,
                        );
                    });
                });
                wait_for(
                    &app,
                    &cx,
                    "staging's object",
                    Duration::from_secs(5),
                    |app, cx| {
                        let state = app.state.read(cx);
                        state.active_environment_id() == Some(demo::STAGING_ID)
                            && app.pane_object(cx).or_else(|| state.active_tab().cloned())
                                == Some(object.clone())
                    },
                )
                .await;
            }
            .boxed_local()
        })),
    );
}

/// Whether `control`'s Icinga has exactly one event stream, live (with
/// check results) or quiet (without; PERF-09).
pub(super) fn one_stream(control: &MockControl, live: bool) -> bool {
    let streams = control.event_stream_stats();
    streams.len() == 1 && streams[0].types.contains(&"CheckResult") == live
}

/// ENV-01, D2: switching shows what the other environment's engine has at
/// once (it ran all along) and reloads nothing; every environment keeps
/// one stream: the one on screen live, the others quiet (PERF-09).
#[test]
fn switching_is_instant_and_keeps_every_stream() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(None),
        Body::Async(Box::new(|app, cx| {
            async move {
                let session = cx.update(|cx| live::session(cx).unwrap());
                every_environment_connected(&app, &cx).await;
                let controls: Vec<MockControl> = cx.update(|cx| {
                    EVERY
                        .iter()
                        .map(|id| session.read(cx).demo_control_of(id).unwrap())
                        .collect()
                });
                let loads = |control: &MockControl| {
                    control
                        .requests()
                        .iter()
                        .filter(|request| request.path == "/v1/objects/hosts")
                        .count()
                };
                let before: Vec<usize> = controls.iter().map(loads).collect();
                for id in [demo::STAGING_ID, demo::LAB_ID, demo::ENVIRONMENT_ID] {
                    cx.update(|cx| {
                        session.update(cx, |session, cx| {
                            assert!(session.switch_environment(id, cx));
                        });
                        app.draw(cx);
                        let state = app.state.read(cx);
                        assert_eq!(state.active_environment_id(), Some(id));
                        assert!(state.connection().is_connected(), "{id}: connected at once");
                        assert!(!state.has_no_objects(), "{id}: its objects at once");
                    });
                }
                // The streams follow (the old one closes once the new
                // one overlaps it).
                wait_for(&app, &cx, "one stream each", CONNECT, |_, _| {
                    EVERY
                        .iter()
                        .zip(&controls)
                        .all(|(id, control)| one_stream(control, *id == demo::ENVIRONMENT_ID))
                })
                .await;
                for (control, before) in controls.iter().zip(before) {
                    assert_eq!(loads(control), before, "switching reloads nothing");
                }
            }
            .boxed_local()
        })),
    );
}

/// A5: muting `staging` silences it alone (recorded, silent, not on the
/// desktop) while `prod-cluster` still notifies; the pause of every
/// environment then silences `lab` too.
#[test]
fn a_muted_environment_stays_silent_while_the_others_notify() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(None),
        Body::Async(Box::new(|app, cx| {
            async move {
                let session = cx.update(|cx| live::session(cx).unwrap());
                every_environment_connected(&app, &cx).await;
                let hour = Timestamp::from_unix_seconds(Timestamp::now().as_unix_seconds() + 3600.);
                cx.update(|cx| {
                    app.state.update(cx, |state, cx| {
                        assert!(state.pause_environment(demo::STAGING_ID, Some(hour)));
                        cx.notify();
                    });
                });
                let (staged, staging) = victim(&cx, &app, &session, demo::STAGING_ID);
                let (produced, prod) = victim(&cx, &app, &session, demo::ENVIRONMENT_ID);
                break_it(&staging, &staged);
                break_it(&prod, &produced);
                wait_for(&app, &cx, "both recorded", LOAD, |app, cx| {
                    recorded_about(app, cx, demo::STAGING_ID, &staged).is_some()
                        && posted_about(session.read(cx), &produced).is_some()
                })
                .await;
                cx.update(|cx| {
                    assert_eq!(
                        recorded_about(&app, cx, demo::STAGING_ID, &staged),
                        Some(true),
                        "staging's is silent"
                    );
                    assert!(posted_about(session.read(cx), &staged).is_none());
                    assert_eq!(
                        recorded_about(&app, cx, demo::ENVIRONMENT_ID, &produced),
                        Some(false)
                    );
                });

                // The pause of every environment silences lab as well.
                cx.update(|cx| {
                    app.state.update(cx, |state, cx| {
                        state.pause_notifications(Some(hour));
                        cx.notify();
                    });
                });
                let (lab_object, lab) = victim(&cx, &app, &session, demo::LAB_ID);
                break_it(&lab, &lab_object);
                wait_for(&app, &cx, "lab's, recorded", LOAD, |app, cx| {
                    recorded_about(app, cx, demo::LAB_ID, &lab_object).is_some()
                })
                .await;
                cx.update(|cx| {
                    assert_eq!(
                        recorded_about(&app, cx, demo::LAB_ID, &lab_object),
                        Some(true)
                    );
                    assert!(posted_about(session.read(cx), &lab_object).is_none());
                });
            }
            .boxed_local()
        })),
    );
}

/// ENV-03: deleting an environment that isn't on screen stops its engine
/// (its stream closes), then deletes its event log; the others keep
/// running.
#[test]
fn deleting_an_environment_stops_its_engine_and_deletes_its_log() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(None),
        Body::Async(Box::new(|app, cx| {
            async move {
                let session = cx.update(|cx| live::session(cx).unwrap());
                every_environment_connected(&app, &cx).await;
                let (log, staging) = cx.update(|cx| {
                    let session = session.read(cx);
                    (
                        session.event_log_of(demo::STAGING_ID).unwrap(),
                        session.demo_control_of(demo::STAGING_ID).unwrap(),
                    )
                });
                assert!(log.exists(), "staging's engine keeps its own log");
                assert_eq!(staging.event_streams(), 1);
                cx.update(|cx| {
                    session.update(cx, |session, cx| {
                        assert!(session.delete_environment(demo::STAGING_ID, cx));
                    });
                });
                wait_for(
                    &app,
                    &cx,
                    "staging's engine stopped and its log gone",
                    CONNECT,
                    |_, cx| !session.read(cx).has_engine(demo::STAGING_ID) && !log.exists(),
                )
                .await;
                assert_eq!(staging.event_streams(), 0, "its stream closed");
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert_eq!(state.environments().len(), 2);
                    assert!(state.slot(demo::STAGING_ID).is_none());
                    assert_eq!(state.active_environment_id(), Some(demo::ENVIRONMENT_ID));
                    for id in [demo::ENVIRONMENT_ID, demo::LAB_ID] {
                        assert!(state.slot(id).unwrap().connection().is_connected(), "{id}");
                    }
                    let tray = crate::background::tray::tray_view(state, Timestamp::now());
                    assert!(!tray.tooltip.contains("staging"), "{}", tray.tooltip);
                });
            }
            .boxed_local()
        })),
    );
}

/// ENV-03: quitting right after deleting an environment still removes its
/// password (and its event log) once its engine stopped.
#[test]
fn quitting_right_after_deleting_an_environment_still_cleans_up() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(None),
        Body::Async(Box::new(|app, cx| {
            async move {
                let session = cx.update(|cx| live::session(cx).unwrap());
                every_environment_connected(&app, &cx).await;
                let log = cx.update(|cx| {
                    let session = session.read(cx);
                    assert!(session.has_password(demo::STAGING_ID));
                    session.event_log_of(demo::STAGING_ID).unwrap()
                });
                assert!(log.exists());
                cx.update(|cx| {
                    session.update(cx, |session, cx| {
                        assert!(session.delete_environment(demo::STAGING_ID, cx));
                        // Quit before the engine had a chance to stop.
                        session.quit_now(cx);
                        assert!(!session.has_password(demo::STAGING_ID), "removed");
                    });
                });
                assert!(!log.exists());
            }
            .boxed_local()
        })),
    );
}

/// NOTE-08: changing the connection settings of an environment off screen
/// restarts its engine alone (a new stream to its Icinga, connected again
/// in the background); the environment on screen keeps its stream.
#[test]
fn editing_an_environment_off_screen_restarts_only_its_engine() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(None),
        Body::Async(Box::new(|app, cx| {
            async move {
                let session = cx.update(|cx| live::session(cx).unwrap());
                every_environment_connected(&app, &cx).await;
                let (prod, staging) = cx.update(|cx| {
                    let session = session.read(cx);
                    (
                        session.demo_control().unwrap(),
                        session.demo_control_of(demo::STAGING_ID).unwrap(),
                    )
                });
                let streams = |control: &MockControl| {
                    control
                        .requests()
                        .iter()
                        .filter(|request| request.path == "/v1/events")
                        .count()
                };
                let (prod_before, staging_before) = (streams(&prod), streams(&staging));
                // A TLS setting: the pin still holds, so it connects again.
                let task = cx.update(|cx| {
                    let mut edited = app
                        .state
                        .read(cx)
                        .environment_by_id(demo::STAGING_ID)
                        .unwrap()
                        .clone();
                    edited.tls.use_system_roots = !edited.tls.use_system_roots;
                    session.update(cx, |session, cx| session.save_environment(edited, None, cx))
                });
                assert_eq!(
                    task.await,
                    Ok(crate::app_state::environments::EnvironmentSaved::Reconnect)
                );
                wait_for(&app, &cx, "staging's new engine", CONNECT, |app, cx| {
                    streams(&staging) > staging_before
                        && app
                            .state
                            .read(cx)
                            .slot(demo::STAGING_ID)
                            .is_some_and(|slot| slot.connection().is_connected())
                })
                .await;
                assert_eq!(streams(&prod), prod_before, "prod-cluster's stream stays");
                assert_eq!(staging.event_streams(), 1, "the old stream closed");
                cx.update(|cx| {
                    assert_eq!(
                        app.state.read(cx).active_environment_id(),
                        Some(demo::ENVIRONMENT_ID)
                    );
                });
            }
            .boxed_local()
        })),
    );
}
