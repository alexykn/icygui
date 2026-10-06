//! The live app's states on the fixture: every connection state renders
//! (banners over the list, the whole body before anything loaded), the
//! footer opens the connection details, refused actions stay refused, and
//! late checks and Icinga's notifications show in rows and panes.

use std::sync::Arc;
use std::time::Instant;

use gpui::{App, Modifiers, point, px};
use ic_core::snapshot::Snapshot;
use ic_core::{ApiInfo, CertificateInfo, ConnectionState, LoadPhase};
use ic_model::{Notification, ObjectKey, Timestamp};

use super::{Harness, replication, row_position, run};
use crate::actions::ObjectAction;
use crate::app_state::testing::Recorder;
use crate::app_state::{ConnectionStatus, Hydrated};
use crate::dashboard::rows;
use crate::fixture::FixtureOptions;
use crate::pane::HostTab;

/// Every connection state the core reports.
fn states(now: Timestamp) -> Vec<ConnectionState> {
    vec![
        ConnectionState::Connecting { attempt: 1 },
        ConnectionState::Connecting { attempt: 3 },
        ConnectionState::Loading {
            phase: LoadPhase::Hosts,
            done: 3,
            total: Some(8),
        },
        ConnectionState::Loading {
            phase: LoadPhase::Services,
            done: 0,
            total: None,
        },
        ConnectionState::Loading {
            phase: LoadPhase::Details,
            done: 120,
            total: Some(450),
        },
        ConnectionState::Reconnecting {
            error: "connect: connection refused (127.0.0.1:5665)".to_owned(),
            attempt: 4,
            retry_at: now.plus(std::time::Duration::from_secs(12)),
        },
        ConnectionState::AuthFailed {
            message: "401 Unauthorized".to_owned(),
        },
        ConnectionState::TlsFailed {
            message: "invalid peer certificate: UnknownIssuer".to_owned(),
            certificate: Some(CertificateInfo {
                sha256: [0x5a; 32],
                subject: "CN=master-01".to_owned(),
                issuer: "CN=Icinga CA".to_owned(),
                names: vec!["master-01".to_owned()],
                not_before: now,
                not_after: now,
            }),
        },
        ConnectionState::MissingSecret,
        ConnectionState::Misconfigured {
            message: "the CA file /etc/icinga2/ca.crt can't be read".to_owned(),
        },
        ConnectionState::Connected {
            endpoint: "master-01".to_owned(),
            version: "r2.15.6-1".to_owned(),
            since: now,
        },
    ]
}

fn set_state(app: &Harness, cx: &mut App, state: ConnectionState) {
    app.state.update(cx, |app_state, cx| {
        let mut status = ConnectionStatus::starting("master-01", Some("icygui".to_owned()));
        status.on_state(state);
        app_state.set_connection(status);
        cx.notify();
    });
    app.draw(cx);
}

#[test]
fn every_connection_state_renders_over_the_list_and_alone() {
    run(FixtureOptions::default(), |app, cx| {
        let now = Timestamp::now();
        // Over the list (objects known), with a pane open and with a tab.
        app.keys(cx, "j enter");
        for state in states(now) {
            set_state(app, cx, state.clone());
            let notice = app.state.read(cx).connection_notice(now);
            let expect_notice = matches!(
                state,
                ConnectionState::Reconnecting { .. }
                    | ConnectionState::AuthFailed { .. }
                    | ConnectionState::TlsFailed { .. }
                    | ConnectionState::MissingSecret
                    | ConnectionState::Misconfigured { .. }
            );
            assert_eq!(notice.is_some(), expect_notice, "{state:?}");
        }
        app.state.update(cx, |state, cx| {
            state.open_tab(replication());
            cx.notify();
        });
        app.draw(cx);
        for state in states(now) {
            set_state(app, cx, state);
        }
        app.keys(cx, "escape");

        // Before anything loaded: the state is the whole body.
        app.state.update(cx, |state, cx| {
            state.set_snapshot(Arc::new(Snapshot::default()));
            cx.notify();
        });
        for state in states(now) {
            set_state(app, cx, state);
            assert!(app.state.read(cx).has_no_objects());
        }
    });
}

#[test]
fn the_footer_status_opens_the_connection_details() {
    run(FixtureOptions::default(), |app, cx| {
        let sidebar = app.workspace.read(cx).sidebar().clone();
        // The footer is the window's last 39px; its status sits right of the
        // two icon buttons.
        let status = point(px(150.), px(880.));
        assert!(!sidebar.read(cx).details_open());
        app.click(cx, status, Modifiers::default());
        assert!(sidebar.read(cx).details_open());
        // Clicking the status again closes them rather than reopening.
        app.click(cx, status, Modifiers::default());
        assert!(!sidebar.read(cx).details_open());
        app.click(cx, status, Modifiers::default());
        assert!(sidebar.read(cx).details_open());
        // A click elsewhere closes them and still does its job.
        app.click(cx, row_position(2), Modifiers::default());
        assert!(!sidebar.read(cx).details_open());
        assert_eq!(
            app.dashboard(cx)
                .read(cx)
                .cursor_in(cx)
                .map(|(index, _)| index),
            Some(2)
        );
        // With permissions known, the details name the user.
        app.state.update(cx, |state, cx| {
            state.set_permissions(Some(ApiInfo {
                user: "icygui".to_owned(),
                permissions: vec!["*".to_owned()],
                version: "v2.15.6".to_owned(),
            }));
            cx.notify();
        });
        app.click(cx, status, Modifiers::default());
        assert!(sidebar.read(cx).details_open());
    });
}

#[test]
fn actions_the_api_user_may_not_run_are_refused() {
    run(FixtureOptions::default(), |app, cx| {
        app.state.update(cx, |state, cx| {
            state.set_permissions(Some(ApiInfo {
                user: "viewer".to_owned(),
                permissions: vec![
                    "objects/query/*".to_owned(),
                    "status/query".to_owned(),
                    "events/*".to_owned(),
                    "actions/reschedule-check".to_owned(),
                ],
                version: "v2.15.6".to_owned(),
            }));
            cx.notify();
        });
        app.keys(cx, "j j enter a");
        let state = app.state.read(cx);
        assert_eq!(state.last_request(), None, "acknowledge is refused");
        assert!(
            state
                .last_denial()
                .is_some_and(|denial| denial.contains("actions/acknowledge-problem")),
            "{:?}",
            state.last_denial()
        );
        assert!(state.action_denial(&ObjectAction::AddComment).is_some());
        // What it may do still works.
        app.keys(cx, "r");
        assert_eq!(
            app.state
                .read(cx)
                .last_request()
                .map(|request| &request.action),
            Some(&ObjectAction::CheckNow)
        );
        // The host pane's buttons render disabled too.
        let pane = app.dashboard(cx).read(cx).pane(cx).unwrap();
        pane.update(cx, |pane, cx| {
            pane.navigate(ObjectKey::host("db-prod-03"), cx);
        });
        app.draw(cx);
    });
}

#[test]
fn late_checks_and_notifications_show_in_rows_and_panes() {
    run(FixtureOptions::default(), |app, cx| {
        let now = Timestamp::now();
        let key = replication();
        app.state.update(cx, |state, cx| {
            let old = state.snapshot().clone();
            let notifications = [(
                key.clone(),
                Arc::from(vec![Notification {
                    name: "db-prod-03!postgres-replication!dba".to_owned(),
                    object: key.clone(),
                    last_notification: Some(now),
                    notified_problem_users: vec!["dba-oncall".to_owned()],
                }]),
            )]
            .into_iter()
            .collect();
            state.set_snapshot(Arc::new(Snapshot {
                revision: old.revision + 1,
                late: Arc::new(
                    [
                        (key.clone(), now.plus(std::time::Duration::ZERO)),
                        (ObjectKey::host("db-prod-03"), now),
                    ]
                    .into_iter()
                    .collect(),
                ),
                icinga_notifications: Arc::new(notifications),
                ..(*old).clone()
            }));
            cx.notify();
        });
        app.draw(cx);
        let snapshot = app.state.read(cx).snapshot().clone();
        assert!(
            rows::object_row(&snapshot, &key, now)
                .unwrap()
                .late
                .is_some()
        );
        assert_eq!(snapshot.notified(&key).users, ["dba-oncall"]);
        app.keys(cx, "j j enter");
        let pane = app.dashboard(cx).read(cx).pane(cx).unwrap();
        assert_eq!(pane.read(cx).object(), &key);
        pane.update(cx, |pane, cx| {
            pane.navigate(ObjectKey::host("db-prod-03"), cx);
        });
        pane.update(cx, |pane, cx| pane.select_host_tab(HostTab::Config, cx));
        app.draw(cx);
    });
}

#[test]
fn view_changes_and_tabs_reach_the_core() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = Recorder::default();
        app.state
            .update(cx, |state, _| state.set_core(Box::new(recorder.clone())));
        // The sort menu: `severity ↓`, then "host".
        app.click(cx, point(px(1352.), px(20.)), Modifiers::default());
        app.state.update(cx, |state, cx| {
            state.update_view(&super::production(), |view| view.hide_handled = true);
            cx.notify();
        });
        app.draw(cx);
        assert_eq!(recorder.sent(), ["UpdateEnvironment(prod-cluster)"]);
        // Hydration is debounced (a timer the sync harness never runs);
        // the state sends it when asked.
        let outcome = app.state.update(cx, |state, _| {
            state.hydrate(vec![replication()], Instant::now())
        });
        assert_eq!(outcome, Hydrated::Sent(vec![replication()]));
        assert_eq!(recorder.sent().len(), 2);
    });
}
