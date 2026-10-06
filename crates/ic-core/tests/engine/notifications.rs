//! Notifications end to end: changes on the mock become rule inputs, the
//! rule engine decides, every intent is logged and emitted, the audible
//! ones reach the (fake) notifier. Nothing from the initial load.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::time::Duration;

use ic_core::{Command, CoreEvent};
use ic_mock::{MockConfig, MockControl, scenarios};
use ic_model::{HostState, ObjectKey, ServiceState};
use ic_rules::Tone;

use crate::support::{Engine, Launch, mock, wait_until};

/// Waits until the engine is live and Icinga's notifications are loaded:
/// everything the connect brings is in.
async fn settled(engine: &mut Engine) {
    engine.connected().await;
    engine
        .snapshot(|snapshot| !snapshot.icinga_notifications.is_empty())
        .await;
}

fn critical(control: &MockControl, host: &str, service: &str) {
    control
        .set_service_state(
            host,
            service,
            ServiceState::Critical,
            &format!("CRITICAL - {service} is broken"),
            true,
        )
        .unwrap();
}

fn ok(control: &MockControl, host: &str, service: &str) {
    control
        .set_service_state(
            host,
            service,
            ServiceState::Ok,
            &format!("OK - {service}"),
            true,
        )
        .unwrap();
}

/// Waits for notifications until one titled `last` came; returns every
/// title since the call, in order (a fence: anything that shouldn't have
/// notified before it shows up in the list).
async fn titles_until(engine: &mut Engine, last: &str) -> Vec<String> {
    let mut titles = Vec::new();
    loop {
        let record = engine.notification().await;
        titles.push(record.intent.title.clone());
        if record.intent.title == last {
            return titles;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_initial_load_is_silent_and_live_problems_notify_once() {
    // staging has three warnings, prod_cluster critical problems and a
    // down host: none of them notifies on connect.
    for scenario in [scenarios::staging(), scenarios::prod_cluster()] {
        let server = mock(MockConfig::with_scenario(scenario)).await;
        let mut engine = Launch::new(&server).start();
        settled(&mut engine).await;
        // A change that notifies, as a fence for anything before it: an OK
        // service on a healthy host.
        let control = server.control();
        let snapshot = engine.latest().unwrap();
        let service = snapshot
            .services
            .values()
            .find(|service| {
                service.state == ServiceState::Ok
                    && service.check.downtime_depth == 0
                    && !service.check.flapping
                    && snapshot
                        .host_of(&service.key)
                        .is_some_and(|host| host.state == HostState::Up)
            })
            .unwrap();
        critical(&control, service.key.host.as_str(), &service.key.name);
        let title = format!(
            "CRITICAL · {} on {}",
            service.display_name,
            snapshot.host_of(&service.key).unwrap().display_name
        );
        assert_eq!(
            titles_until(&mut engine, &title).await,
            std::slice::from_ref(&title)
        );
        assert_eq!(engine.shown(), [title]);
        engine.shutdown();
    }

    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    settled(&mut engine).await;

    critical(&control, "stg-api-01", "http");
    let record = engine.notification().await;
    let intent = &record.intent;
    assert_eq!(intent.title, "CRITICAL · http on stg-api-01");
    // The default dashboards: "problems" in "overview" comes first.
    assert_eq!(intent.subtitle, "overview / problems");
    assert_eq!(intent.body, "CRITICAL - http is broken");
    assert_eq!(intent.tone, Tone::Critical);
    assert_eq!(
        intent.object,
        Some(ObjectKey::service("stg-api-01", "http"))
    );
    assert!(!intent.silent && intent.sound && !record.read);

    // A warning (not selected by the default rule) that recovers: nothing;
    // the critical one recovers: notified.
    ok(&control, "stg-db-01", "pg-connections");
    ok(&control, "stg-api-01", "http");
    assert_eq!(
        titles_until(&mut engine, "RECOVERED · http on stg-api-01").await,
        ["RECOVERED · http on stg-api-01"]
    );
    assert!(wait_until(|| engine.shown().len() == 2).await);

    // Every notification is in the log, newest first; mark them read.
    let stored = engine.stored_notifications().await;
    let titles: Vec<&str> = stored.iter().map(|r| r.intent.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "RECOVERED · http on stg-api-01",
            "CRITICAL · http on stg-api-01"
        ]
    );
    assert_eq!(stored[1].intent, *intent, "logged as emitted");
    assert!(stored.iter().all(|record| !record.read));
    // The entry the user opened, then all of them.
    engine.send(Command::MarkNotificationRead(stored[1].intent.id.clone()));
    let stored = engine.stored_notifications().await;
    let read: Vec<bool> = stored.iter().map(|record| record.read).collect();
    assert_eq!(read, [false, true]);
    engine.send(Command::MarkNotificationsRead);
    let stored = engine.stored_notifications().await;
    assert!(stored.iter().all(|record| record.read));
    // The history starts with the first change logged.
    let (reply, start) = futures::channel::oneshot::channel();
    engine.send(Command::LoadHistoryStart { reply });
    let start = start.await.unwrap().expect("changes were logged");
    assert!(start <= stored[1].intent.at, "{start:?}");
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_storm_is_silenced_and_summarized() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    settled(&mut engine).await;
    // Storm control: more than 5 audible notifications within 10 s.
    let hosts = [
        "stg-web-01",
        "stg-web-02",
        "stg-api-01",
        "stg-api-02",
        "stg-db-01",
        "stg-mq-01",
        "stg-cache-01",
        "stg-k8s-01",
    ];
    for host in hosts {
        critical(&control, host, "load");
    }
    let mut records = Vec::new();
    while records.len() < hosts.len() {
        records.push(engine.notification().await);
    }
    let silent: Vec<bool> = records.iter().map(|record| record.intent.silent).collect();
    assert_eq!(
        silent,
        [false, false, false, false, false, true, true, true],
        "the sixth and later are silent"
    );
    assert!(wait_until(|| engine.shown().len() == 5).await);

    // Once a whole window passes without more, the summary.
    engine.clock.advance(Duration::from_secs(11));
    // It covers the whole storm.
    let summary = engine.notification().await.intent;
    assert_eq!(summary.object, None);
    assert_eq!(summary.title, "8 new problems in test");
    assert_eq!(summary.body, "8 critical");
    assert_eq!(summary.tone, Tone::Info);
    assert!(!summary.silent);
    assert!(wait_until(|| engine.shown().len() == 6).await);
    assert_eq!(engine.shown()[5], "8 new problems in test");
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pause_silences_until_it_ends() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    settled(&mut engine).await;

    let until = engine
        .clock
        .now
        .lock()
        .unwrap()
        .plus(Duration::from_mins(30));
    engine.send(Command::PauseNotifications(Some(until)));
    let paused = engine
        .wait_for(|event| match event {
            CoreEvent::NotificationsPaused(paused) => Some(*paused),
            _ => None,
        })
        .await;
    assert_eq!(paused, Some(until));
    critical(&control, "stg-api-01", "http");
    let record = engine.notification().await;
    assert!(record.intent.silent, "recorded, not shown");
    // The pause ends on its own.
    engine.clock.advance(Duration::from_mins(31));
    let resumed = engine
        .wait_for(|event| match event {
            CoreEvent::NotificationsPaused(paused) => Some(*paused),
            _ => None,
        })
        .await;
    assert_eq!(resumed, None);
    critical(&control, "stg-api-02", "http");
    let record = engine.notification().await;
    assert_eq!(record.intent.title, "CRITICAL · http on stg-api-02");
    assert!(!record.intent.silent);
    assert_eq!(engine.shown(), ["CRITICAL · http on stg-api-02"]);

    // Paused and resumed by hand.
    engine.send(Command::PauseNotifications(Some(
        until.plus(Duration::from_hours(5)),
    )));
    engine.send(Command::PauseNotifications(None));
    let mut announced = Vec::new();
    while announced.len() < 2 {
        announced.push(
            engine
                .wait_for(|event| match event {
                    CoreEvent::NotificationsPaused(paused) => Some(*paused),
                    _ => None,
                })
                .await,
        );
    }
    assert_eq!(announced[1], None);
    critical(&control, "stg-web-01", "http");
    assert!(!engine.notification().await.intent.silent);
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn problems_under_a_failed_host_wait_for_a_fresh_check() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    settled(&mut engine).await;
    let http = ObjectKey::service("stg-api-01", "http");

    control
        .set_host_state("stg-api-01", HostState::Down, "CRITICAL - no route", true)
        .unwrap();
    assert_eq!(
        engine.notification().await.intent.title,
        "DOWN · stg-api-01"
    );
    // A service failing while its host is down is handled: silent.
    critical(&control, "stg-api-01", "http");
    // The host comes back and notifies its recovery; the service is still
    // critical, but its state may be left over from the outage.
    control
        .set_host_state("stg-api-01", HostState::Up, "PING OK", true)
        .unwrap();
    assert_eq!(
        titles_until(&mut engine, "RECOVERED · stg-api-01").await,
        ["RECOVERED · stg-api-01"]
    );
    // Its first check since (reachable again) shows the handling over and
    // confirms the state: it notifies.
    control
        .process_check_result(&http, 2, "CRITICAL - still broken", &[])
        .unwrap();
    let record = engine.notification().await;
    assert_eq!(record.intent.title, "CRITICAL · http on stg-api-01");
    assert_eq!(record.intent.body, "CRITICAL - still broken");

    // A problem acknowledged before it turned hard never notifies, and its
    // recovery doesn't either; one that did notify recovers loudly.
    let ssh = ObjectKey::service("stg-api-02", "ssh");
    control
        .set_service_state(
            "stg-api-02",
            "ssh",
            ServiceState::Critical,
            "refused",
            false,
        )
        .unwrap();
    control
        .acknowledge(&ssh, "m.keller", "known", true)
        .unwrap();
    critical(&control, "stg-api-02", "ssh");
    ok(&control, "stg-api-02", "ssh");
    ok(&control, "stg-api-01", "http");
    assert_eq!(
        titles_until(&mut engine, "RECOVERED · http on stg-api-01").await,
        ["RECOVERED · http on stg-api-01"]
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missed_changes_found_by_a_reconcile_notify() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let launch = Launch::new(&server);
    let mut environment = launch.environment.clone();
    let mut engine = launch.start();
    settled(&mut engine).await;

    // The stream drops; a service fails while the client is away.
    control.drop_event_streams();
    critical(&control, "stg-cache-01", "redis-memory");
    // Reconnected: live at once, then the jittered reload finds it.
    let record = engine.notification().await;
    assert_eq!(
        record.intent.title,
        "CRITICAL · redis-memory on stg-cache-01"
    );
    assert_eq!(
        record.intent.body, "CRITICAL - redis-memory is broken",
        "the output of the problem's details"
    );
    // A delayed rule: nothing until the problem lasted a minute (a
    // recovery, never delayed, is the fence). Commands are handled before
    // stream lines, so the rule applies to the change after it.
    environment.notifications.default_rule.min_duration_secs = 60;
    engine.send(Command::UpdateEnvironment(environment));
    critical(&control, "stg-k8s-02", "kubelet");
    ok(&control, "stg-cache-01", "redis-memory");
    assert_eq!(
        titles_until(&mut engine, "RECOVERED · redis-memory on stg-cache-01").await,
        ["RECOVERED · redis-memory on stg-cache-01"],
        "the delayed one is held back"
    );
    // Once the problem lasted the minute, the tick releases it.
    engine.clock.advance(Duration::from_secs(61));
    let record = engine.notification().await;
    assert_eq!(record.intent.title, "CRITICAL · kubelet on stg-k8s-02");
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rules_follow_the_environment() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    // Everything off but a watch on one service.
    launch.environment.notifications.enabled = false;
    launch
        .environment
        .notifications
        .objects
        .push(ic_rules::ObjectOverride {
            object: ObjectKey::service("stg-web-02", "http"),
            mode: ic_rules::ObjectMode::Watch,
            until: None,
        });
    let mut environment = launch.environment.clone();
    let mut engine = launch.start();
    settled(&mut engine).await;
    critical(&control, "stg-web-01", "http");
    critical(&control, "stg-web-02", "http");
    let record = engine.notification().await;
    assert_eq!(record.intent.title, "CRITICAL · http on stg-web-02");
    assert_eq!(record.intent.subtitle, "test", "a watch: the environment");

    // Turned on, with the problems dashboard off: the "all services"
    // dashboard names it.
    environment.notifications.enabled = true;
    environment.groups[0].dashboards[0].notifications = ic_rules::ScopeSetting::Off;
    engine.send(Command::UpdateEnvironment(environment));
    critical(&control, "stg-api-01", "http");
    let record = engine.notification().await;
    assert_eq!(record.intent.title, "CRITICAL · http on stg-api-01");
    assert_eq!(record.intent.subtitle, "overview / all services");
    engine.shutdown();
}
