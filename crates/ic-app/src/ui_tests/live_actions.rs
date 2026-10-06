//! Every operator action end to end (ACT-01..07): from a key, a button's
//! request or the palette through the dialog, `AppState::submit`, the real
//! `ic-core` and `ic-api` to the demo's mock Icinga and back as a toast
//! and a changed snapshot. Afterwards every request the client sent is
//! checked against the endpoints it may use (ACT-08: runtime operations
//! only; never `objects/modify`, config packages, object creation or
//! deletion, the console or a process restart).

use futures::FutureExt as _;
use gpui::{App, AsyncApp};
use ic_mock::{MockControl, RecordedRequest};
use ic_model::{ObjectKey, ServiceState};

use super::live::{LOAD, control, demo_app};
use super::{Body, Harness, replication, run_app, wait_for};
use crate::actions::{ActionRequest, ObjectAction};
use crate::live;
use crate::operate::dialog::{ActionDialog, DialogKind, Form};
use crate::operate::forms::FormField;
use crate::operate::tracker::ToastTone;
use crate::workspace::ModalKind;

use crate::operate::ALLOWED_ENDPOINTS;

/// Whether a request is one the client may send (ACT-08): `GET /v1` and
/// `/v1/status/*`, object queries (`POST` with `X-HTTP-Method-Override:
/// GET` on `/v1/objects/<type>`), the event stream, and the allowed
/// actions. Anything else (`PUT`/`DELETE` objects, `POST` to an object,
/// `/v1/config`, `/v1/console`, `restart-process`, notification actions)
/// is not.
pub(crate) fn is_allowed(request: &RecordedRequest) -> bool {
    let path = request.path.as_str();
    match (request.method.as_str(), request.effective_method()) {
        ("GET", "GET") => path == "/v1" || path.starts_with("/v1/status/"),
        ("POST", "GET") => path
            .strip_prefix("/v1/objects/")
            .is_some_and(|kind| !kind.is_empty() && !kind.contains('/')),
        ("POST", "POST") => {
            path == "/v1/events"
                || path
                    .strip_prefix("/v1/actions/")
                    .is_some_and(|action| ALLOWED_ENDPOINTS.contains(&action))
        }
        _ => false,
    }
}

/// The action requests the server answered with success, by action name.
fn succeeded(control: &MockControl, action: &str) -> usize {
    control
        .requests()
        .iter()
        .filter(|request| {
            request.path == format!("/v1/actions/{action}") && (200..300).contains(&request.status)
        })
        .count()
}

fn modal(app: &Harness, cx: &App) -> Option<ModalKind> {
    app.workspace.read(cx).modal(cx)
}

fn type_into(app: &Harness, cx: &mut App, field: FormField, text: &str) {
    let dialog = app
        .workspace
        .read(cx)
        .action_dialog()
        .cloned()
        .expect("a dialog");
    app.in_window(cx, |window, cx| {
        ActionDialog::type_into(&dialog, field, text, window, cx);
    });
    app.draw(cx);
}

fn request(app: &Harness, cx: &mut App, action: ObjectAction, targets: Vec<ObjectKey>) {
    app.state.update(cx, |state, cx| {
        let _ = state.request(ActionRequest { action, targets });
        cx.notify();
    });
    app.draw(cx);
}

/// The newest toast's tone and title.
fn last_toast(app: &Harness, cx: &App) -> Option<(ToastTone, String)> {
    app.state
        .read(cx)
        .toasts()
        .last()
        .map(|toast| (toast.tone, toast.title.clone()))
}

/// Waits until the newest toast reports success.
async fn succeeded_toast(app: &Harness, cx: &AsyncApp, what: &str) {
    wait_for(app, cx, what, LOAD, |app, cx| {
        last_toast(app, cx).is_some_and(|(tone, _)| tone == ToastTone::Success)
    })
    .await;
}

fn mock_service(control: &MockControl, key: &ObjectKey) -> ic_model::Service {
    let ObjectKey::Service { key } = key else {
        panic!("a service");
    };
    control
        .service(key.host.as_str(), &key.name)
        .expect("the mock has it")
}

/// ACT-02: acknowledge from the pane by keyboard, then remove it.
async fn acknowledge(app: &Harness, cx: &AsyncApp, control: &MockControl) {
    let object = replication();
    // ACT-02: acknowledge from the pane, by keyboard.
    // Each step that a subscriber must react to is an update of
    // its own (effects flush at the end of the outermost one).
    cx.update(|cx| assert!(live::open_object(&object, cx)));
    cx.update(|cx| app.keys(cx, "a"));
    cx.update(|cx| {
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Action(DialogKind::Acknowledge))
        );
        type_into(app, cx, FormField::Comment, "failover drill");
    });
    cx.update(|cx| app.keys(cx, "enter"));
    cx.update(|cx| {
        assert_eq!(modal(app, cx), None);
        assert_eq!(
            app.state.read(cx).pending_label(&object),
            Some("ack pending…")
        );
    });
    wait_for(
        app,
        cx,
        "the acknowledgement in Icinga and the window",
        LOAD,
        |app, cx| {
            let shown = app
                .state
                .read(cx)
                .snapshot()
                .services
                .values()
                .any(|service| {
                    service.object_key() == replication()
                        && service.check.acknowledgement.is_acknowledged()
                });
            shown && app.state.read(cx).pending_label(&replication()).is_none()
        },
    )
    .await;
    assert!(
        mock_service(control, &object)
            .check
            .acknowledgement
            .is_acknowledged()
    );
    succeeded_toast(app, cx, "the acknowledgement's toast").await;

    // ... and remove it again (the pane's "remove ack").
    cx.update(|cx| {
        request(
            app,
            cx,
            ObjectAction::RemoveAcknowledgement,
            vec![object.clone()],
        );
    });
    wait_for(app, cx, "the acknowledgement removed", LOAD, |_, _| {
        !mock_service(control, &replication())
            .check
            .acknowledgement
            .is_acknowledged()
    })
    .await;
    succeeded_toast(app, cx, "the removal's toast").await;
}

/// ACT-04: a comment, then removing it by name (the pane's ×).
async fn comment(app: &Harness, cx: &AsyncApp, control: &MockControl) {
    let object = replication();
    // ACT-04: a comment, then removing it by name (the pane's ×).
    cx.update(|cx| app.keys(cx, "c"));
    cx.update(|cx| type_into(app, cx, FormField::Comment, "replica rebuilt by hand"));
    cx.update(|cx| app.keys(cx, "enter"));
    let comment = {
        let mut name = None;
        wait_for(app, cx, "the comment in the window", LOAD, |app, cx| {
            name = app
                .state
                .read(cx)
                .snapshot()
                .comments
                .get(&replication())
                .and_then(|comments| {
                    comments
                        .iter()
                        .find(|comment| comment.text == "replica rebuilt by hand")
                        .map(|comment| comment.name.clone())
                });
            name.is_some()
        })
        .await;
        name.unwrap_or_default()
    };
    assert!(control.comments().iter().any(|c| c.name == comment));
    cx.update(|cx| {
        request(
            app,
            cx,
            ObjectAction::RemoveComment(comment.clone()),
            vec![object.clone()],
        );
    });
    wait_for(app, cx, "the comment removed", LOAD, |_, _| {
        control.comments().iter().all(|c| c.name != comment)
    })
    .await;
}

/// ACT-03: downtimes, removed by name and with all of the object's.
async fn downtimes(app: &Harness, cx: &AsyncApp, control: &MockControl) {
    let object = replication();
    // ACT-03: a downtime (fixed, the default hour), removed by
    // name; then another, removed with all of the object's.
    for round in 0..2 {
        cx.update(|cx| app.keys(cx, "d"));
        cx.update(|cx| {
            assert_eq!(
                modal(app, cx),
                Some(ModalKind::Action(DialogKind::Downtime))
            );
            type_into(app, cx, FormField::Comment, "maintenance");
        });
        cx.update(|cx| app.keys(cx, "enter"));
        cx.update(|cx| assert_eq!(modal(app, cx), None));
        let downtime = {
            let mut name = None;
            wait_for(app, cx, "the downtime in the window", LOAD, |app, cx| {
                name = app
                    .state
                    .read(cx)
                    .snapshot()
                    .downtimes
                    .get(&replication())
                    .and_then(|list| list.first())
                    .map(|downtime| downtime.name.clone());
                name.is_some()
            })
            .await;
            name.unwrap_or_default()
        };
        if round == 0 {
            cx.update(|cx| {
                request(
                    app,
                    cx,
                    ObjectAction::RemoveDowntime(downtime.clone()),
                    vec![object.clone()],
                );
            });
        } else {
            cx.update(|cx| {
                request(app, cx, ObjectAction::RemoveDowntimes, vec![object.clone()]);
            });
            cx.update(|cx| {
                assert!(matches!(modal(app, cx), Some(ModalKind::Confirm(_))));
            });
            cx.update(|cx| app.keys(cx, "enter"));
        }
        wait_for(app, cx, "the downtime removed", LOAD, |app, cx| {
            control.downtimes().iter().all(|d| d.name != downtime)
                && app
                    .state
                    .read(cx)
                    .snapshot()
                    .downtimes
                    .get(&replication())
                    .is_none_or(Vec::is_empty)
        })
        .await;
    }
}

/// ACT-01 and ACT-05: check now, then a passive result.
async fn check_and_result(app: &Harness, cx: &AsyncApp, control: &MockControl) {
    let object = replication();
    // ACT-01: check now (forced), at once.
    let checks = succeeded(control, "reschedule-check");
    cx.update(|cx| app.keys(cx, "r"));
    cx.update(|cx| assert_eq!(modal(app, cx), None));
    wait_for(app, cx, "the check rescheduled", LOAD, |_, _| {
        succeeded(control, "reschedule-check") > checks
    })
    .await;
    succeeded_toast(app, cx, "the check's toast").await;

    // ACT-05: a passive result turns the service OK.
    cx.update(|cx| {
        request(
            app,
            cx,
            ObjectAction::SubmitCheckResult,
            vec![object.clone()],
        );
    });
    cx.update(|cx| {
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Action(DialogKind::CheckResult))
        );
        type_into(app, cx, FormField::Output, "OK - caught up by hand");
        type_into(app, cx, FormField::Perfdata, "replication_lag=2s;60;300");
        let dialog = app.workspace.read(cx).action_dialog().cloned().unwrap();
        dialog.update(cx, |dialog, cx| {
            dialog.edit_form(cx, |form| {
                if let Form::Result(form) = form {
                    form.exit_status = 0;
                }
            });
        });
    });
    cx.update(|cx| app.keys(cx, "enter"));
    cx.update(|cx| assert_eq!(modal(app, cx), None));
    wait_for(
        app,
        cx,
        "the passive result in the window",
        LOAD,
        |app, cx| {
            app.state
                .read(cx)
                .snapshot()
                .services
                .values()
                .any(|service| {
                    service.object_key() == replication()
                        && service.state == ServiceState::Ok
                        && service.check.output() == "OK - caught up by hand"
                })
        },
    )
    .await;
    assert_eq!(mock_service(control, &object).state, ServiceState::Ok);
}

/// ACT-06: run a command behind a confirmation.
async fn run_command(app: &Harness, cx: &AsyncApp, control: &MockControl) {
    let object = replication();
    // ACT-06: run a command behind a confirmation. Its event
    // command first: it has none, and Icinga's reason shows.
    cx.update(|cx| request(app, cx, ObjectAction::RunCommand, vec![object.clone()]));
    cx.update(|cx| app.keys(cx, "enter"));
    cx.update(|cx| app.keys(cx, "ctrl-enter"));
    wait_for(app, cx, "Icinga's refusal", LOAD, |app, cx| {
        let state = app.state.read(cx);
        state.action_failure(&replication()).is_some_and(|failure| {
            failure.action == ObjectAction::RunCommand && failure.reason.contains("EventCommand")
        })
    })
    .await;
    // Then its check command.
    cx.update(|cx| request(app, cx, ObjectAction::RunCommand, vec![object.clone()]));
    cx.update(|cx| {
        let dialog = app.workspace.read(cx).action_dialog().cloned().unwrap();
        dialog.update(cx, |dialog, cx| {
            dialog.edit_form(cx, |form| {
                if let Form::Command(form) = form {
                    form.command_type = ic_model::CommandType::CheckCommand;
                }
            });
        });
    });
    cx.update(|cx| app.keys(cx, "enter"));
    cx.update(|cx| {
        let dialog = app.workspace.read(cx).action_dialog().cloned().unwrap();
        assert!(dialog.read(cx).is_confirming());
    });
    cx.update(|cx| app.keys(cx, "ctrl-enter"));
    cx.update(|cx| assert_eq!(modal(app, cx), None));
    wait_for(app, cx, "the command started", LOAD, |_, _| {
        succeeded(control, "execute-command") == 1
    })
    .await;
    succeeded_toast(app, cx, "the command's toast").await;
}

/// ACT-08: everything the client sent is a query, the event stream or an
/// allowed action.
fn assert_only_allowed_requests(control: &MockControl) {
    let requests = control.requests();
    let refused: Vec<String> = requests
        .iter()
        .filter(|request| !is_allowed(request))
        .map(|request| format!("{} {}", request.effective_method(), request.path))
        .collect();
    assert!(
        refused.is_empty(),
        "requests outside the allowed set: {refused:?}"
    );
    // And each of them ran, successfully, at least once.
    for action in ALLOWED_ENDPOINTS {
        assert!(succeeded(control, action) >= 1, "{action} didn't run");
    }
}

#[test]
fn every_action_reaches_icinga_and_its_change_shows() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app("prod-cluster", None),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "the first load", LOAD, |app, cx| {
                    let state = app.state.read(cx);
                    state.connection().is_connected()
                        && state
                            .snapshot()
                            .services
                            .contains_key(match &replication() {
                                ObjectKey::Service { key } => key,
                                ObjectKey::Host { .. } => unreachable!(),
                            })
                })
                .await;
                let control = cx.update(control);
                // Nothing changes but what the actions change.
                control.pause_simulation();

                acknowledge(&app, &cx, &control).await;
                comment(&app, &cx, &control).await;
                downtimes(&app, &cx, &control).await;
                check_and_result(&app, &cx, &control).await;
                run_command(&app, &cx, &control).await;
                assert_only_allowed_requests(&control);
            }
            .boxed_local()
        })),
    );
}

#[test]
fn a_bulk_action_reports_each_object_icinga_refused() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app("prod-cluster", None),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "the first load", LOAD, |app, cx| {
                    let state = app.state.read(cx);
                    state.connection().is_connected() && !state.snapshot().services.is_empty()
                })
                .await;
                let control = cx.update(control);
                control.pause_simulation();
                // Two unacknowledged problems.
                let targets: Vec<ObjectKey> = cx.update(|cx| {
                    app.state
                        .read(cx)
                        .snapshot()
                        .services
                        .values()
                        .filter(|service| {
                            service.is_problem() && !service.check.acknowledgement.is_acknowledged()
                        })
                        .take(2)
                        .map(|service| service.object_key())
                        .collect()
                });
                assert_eq!(targets.len(), 2);
                cx.update(|cx| request(&app, cx, ObjectAction::Acknowledge, targets.clone()));
                cx.update(|cx| type_into(&app, cx, FormField::Comment, "storage incident"));
                // A colleague acknowledges the second one meanwhile.
                control
                    .acknowledge(&targets[1], "colleague", "on it", false)
                    .unwrap();
                cx.update(|cx| app.keys(cx, "enter"));
                wait_for(&app, &cx, "the partial result", LOAD, |app, cx| {
                    last_toast(app, cx).is_some_and(|(tone, _)| tone == ToastTone::Partial)
                })
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    let toast = state.toasts().last().unwrap();
                    assert_eq!(toast.title, "Acknowledged 1 of 2 services");
                    assert_eq!(toast.lines.len(), 1);
                    assert!(
                        toast.lines[0].contains("already acknowledged"),
                        "Icinga's reason: {}",
                        toast.lines[0]
                    );
                    let failure = state.action_failure(&targets[1]).unwrap();
                    assert_eq!(failure.action, ObjectAction::Acknowledge);
                    assert!(state.action_failure(&targets[0]).is_none());
                });
                assert!(
                    mock_service(&control, &targets[0])
                        .check
                        .acknowledgement
                        .is_acknowledged()
                );
                assert!(control.requests().iter().all(is_allowed));
            }
            .boxed_local()
        })),
    );
}

#[test]
fn the_allowed_set_refuses_configuration_and_process_endpoints() {
    let request = |method: &str, method_override: Option<&str>, path: &str| RecordedRequest {
        method: method.to_owned(),
        method_override: method_override.map(str::to_owned),
        path: path.to_owned(),
        query: Vec::new(),
        body: None,
        user: None,
        status: 200,
        at: ic_model::Timestamp::now(),
    };
    for allowed in [
        request("GET", None, "/v1"),
        request("GET", None, "/v1/status/IcingaApplication"),
        request("POST", Some("GET"), "/v1/objects/services"),
        request("POST", None, "/v1/events"),
        request("POST", None, "/v1/actions/acknowledge-problem"),
        request("POST", None, "/v1/actions/execute-command"),
    ] {
        assert!(is_allowed(&allowed), "{allowed:?}");
    }
    for refused in [
        request("POST", None, "/v1/objects/hosts/db-01"),
        request("POST", None, "/v1/objects/services"),
        request("PUT", None, "/v1/objects/hosts/new-host"),
        request("DELETE", None, "/v1/objects/hosts/db-01"),
        request("POST", Some("DELETE"), "/v1/objects/hosts/db-01"),
        request("GET", None, "/v1/objects/hosts"),
        request("POST", None, "/v1/config/packages/icygui"),
        request("POST", None, "/v1/console/execute-script"),
        request("POST", None, "/v1/actions/restart-process"),
        request("POST", None, "/v1/actions/shutdown-process"),
        request("POST", None, "/v1/actions/send-custom-notification"),
        request("POST", None, "/v1/actions/delay-notification"),
        request("POST", None, "/v1/actions/generate-ticket"),
    ] {
        assert!(!is_allowed(&refused), "{refused:?}");
    }
    // Every action the client has is in the set.
    for name in crate::operate::allowed_action_names() {
        assert!(
            ALLOWED_ENDPOINTS.contains(&name),
            "{name} isn't an allowed action"
        );
    }
}
