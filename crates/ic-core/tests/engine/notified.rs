//! Who Icinga notified, and when (PANE-06): Icinga's own `Notification`
//! objects load in the background after the problem lists, follow its
//! `Notification` events by name, and stay out of periodic reconciles
//! while the stream carries those events.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::time::Duration;

use ic_core::snapshot::Snapshot;
use ic_mock::{MockConfig, MockControl, MockUser, scenarios};
use ic_model::{Notification, ObjectKey, ServiceState};
use serde_json::json;

use crate::support::{Launch, PASSWORD, USER, mock, wait_until};

fn stored(snapshot: &Snapshot) -> Vec<Notification> {
    let mut list: Vec<Notification> = snapshot
        .icinga_notifications
        .values()
        .flat_map(|list| list.iter().cloned())
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

fn mock_notifications(control: &MockControl) -> Vec<Notification> {
    let mut list = control.notifications();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

/// Whether two lists hold the same notifications (times to the
/// millisecond: the wire's doubles may differ in the last digits).
fn same(a: &[Notification], b: &[Notification]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            let near = match (a.last_notification, b.last_notification) {
                (Some(x), Some(y)) => (x.as_unix_seconds() - y.as_unix_seconds()).abs() < 1e-3,
                (x, y) => x == y,
            };
            near && a.name == b.name
                && a.object == b.object
                && a.notified_problem_users == b.notified_problem_users
        })
}

/// Requests for `/v1/objects/notifications`: their name lists (`None`
/// for the whole list).
fn notification_queries(control: &MockControl) -> Vec<Option<Vec<String>>> {
    control
        .requests()
        .into_iter()
        .filter(|request| request.path == "/v1/objects/notifications")
        .map(|request| {
            let body = request.body.unwrap_or_default();
            body.get("notifications").map(|names| {
                names
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|name| name.as_str().unwrap().to_owned())
                    .collect()
            })
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn who_icinga_notified_loads_and_follows_its_notifications() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    engine.connected().await;
    let snapshot = engine
        .snapshot(|snapshot| !snapshot.icinga_notifications.is_empty())
        .await;
    assert!(same(&stored(&snapshot), &mock_notifications(&control)));
    // The scenario's hard warning was notified when it began.
    let warning = ObjectKey::service("stg-db-01", "pg-connections");
    let notified = snapshot.notified(&warning);
    assert_eq!(notified.users, ["qa-oncall"]);
    assert!(notified.last_notification.is_some());
    let http = ObjectKey::service("stg-api-01", "http");
    assert!(snapshot.notified(&http).is_never());
    // One query for the whole list, with only the four attributes.
    let queries = notification_queries(&control);
    assert_eq!(queries, [None]);
    let request = control
        .requests()
        .into_iter()
        .find(|request| request.path == "/v1/objects/notifications")
        .unwrap();
    assert_eq!(
        request.body.unwrap()["attrs"],
        json!([
            "host_name",
            "service_name",
            "last_notification",
            "notified_problem_users"
        ])
    );

    // Icinga notifies about a new problem: the object's notifications are
    // read again by name.
    control
        .set_service_state(
            "stg-api-01",
            "http",
            ServiceState::Critical,
            "CRITICAL - 503",
            true,
        )
        .unwrap();
    let snapshot = engine
        .snapshot(|snapshot| !snapshot.notified(&http).users.is_empty())
        .await;
    let notified = snapshot.notified(&http);
    assert_eq!(notified.users, ["qa-oncall"]);
    let sent = notified.last_notification.unwrap();
    assert!(sent.as_unix_seconds() > 0.0);
    assert!(
        notification_queries(&control).contains(&Some(vec!["stg-api-01!http!mail-qa".to_owned()])),
        "{:?}",
        notification_queries(&control)
    );

    // The recovery: users cleared, the time moves on.
    control.advance_clock(Duration::from_secs(30));
    control
        .set_service_state("stg-api-01", "http", ServiceState::Ok, "HTTP OK", true)
        .unwrap();
    let snapshot = engine
        .snapshot(|snapshot| {
            let notified = snapshot.notified(&http);
            notified.users.is_empty() && notified.last_notification > Some(sent)
        })
        .await;
    assert!(same(&stored(&snapshot), &mock_notifications(&control)));

    // A changed `Notification` object (a config change by API) is read by
    // name too.
    control.clear_requests();
    control.emit_raw(json!({
        "type": "ObjectModified",
        "timestamp": control.now().as_unix_seconds(),
        "object_type": "Notification",
        "object_name": "stg-web-01!http!mail-qa",
    }));
    assert!(
        wait_until(
            || notification_queries(&control) == [Some(vec!["stg-web-01!http!mail-qa".to_owned()])]
        )
        .await,
        "{:?}",
        notification_queries(&control)
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn periodic_reconciles_leave_them_to_the_events_but_reconnects_reload() {
    let server = mock(MockConfig::with_scenario(scenarios::lab())).await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.tuning.reconcile_interval = Some(Duration::from_millis(150));
    let mut engine = launch.start();
    engine.connected().await;
    engine
        .snapshot(|snapshot| !snapshot.icinga_notifications.is_empty())
        .await;
    let hosts_loads = || {
        control
            .requests()
            .iter()
            .filter(|request| {
                request.path == "/v1/objects/hosts"
                    && request
                        .body
                        .as_ref()
                        .is_some_and(|body| body.get("hosts").is_none())
            })
            .count()
    };
    assert!(wait_until(|| hosts_loads() >= 3).await, "reconciles ran");
    assert_eq!(notification_queries(&control), [None], "loaded once");

    // A reconnect may have missed Notification events: reloaded.
    control.drop_event_streams();
    assert!(wait_until(|| notification_queries(&control).len() == 2).await);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_permission_there_are_none_and_nothing_is_asked() {
    // Allowed everything but Icinga's notifications.
    let mut config = MockConfig::with_scenario(scenarios::staging());
    config.users = vec![MockUser::new(
        USER,
        PASSWORD,
        &[
            "objects/query/Host",
            "objects/query/Service",
            "objects/query/HostGroup",
            "objects/query/ServiceGroup",
            "objects/query/Comment",
            "objects/query/Downtime",
            "objects/query/Dependency",
            "objects/query/Endpoint",
            "objects/query/Zone",
            "status/query",
            "events/*",
            "actions/*",
        ],
    )];
    let server = mock(config).await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    engine.connected().await;
    control
        .set_service_state(
            "stg-api-01",
            "http",
            ServiceState::Critical,
            "CRITICAL - 503",
            true,
        )
        .unwrap();
    // The client's own notification still comes.
    let record = engine.notification().await;
    assert_eq!(record.intent.title, "CRITICAL · http on stg-api-01");
    let snapshot = engine.latest().unwrap();
    assert!(snapshot.icinga_notifications.is_empty());
    assert!(
        snapshot
            .notified(&ObjectKey::service("stg-api-01", "http"))
            .is_never()
    );
    assert!(notification_queries(&control).is_empty(), "never asked");
    engine.shutdown();
}
