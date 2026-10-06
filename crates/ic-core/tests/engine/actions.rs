//! Every action end to end: the request Icinga gets (author, parameters),
//! `ActionFinished` with per-object failures, the snapshot afterwards, and
//! the re-query of the targets.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::support::{
    ENV_ID, FakeSecrets, Launch, PASSWORD, USER, environment, mock, start, start_for, tuning,
    wait_until,
};
use ic_core::{ActionOutcome, Command, ConnectionState, CoreEvent, Tuning};
use ic_mock::{MockConfig, MockControl, MockUser, scenarios};
use ic_model::{
    AckKind, Action, ActionTarget, ChildOptions, CommandType, DowntimeMode, Endpoint, HostState,
    ObjectKey, ServiceKey, ServiceState, Timestamp, Vars,
};
use serde_json::{Value, json};

/// Runs an action and waits for its outcome.
async fn run(
    engine: &mut crate::support::Engine,
    id: u64,
    target: ActionTarget,
    action: Action,
) -> ActionOutcome {
    engine.send(Command::Action { id, target, action });
    engine
        .wait_for(|event| match event {
            CoreEvent::ActionFinished { id: done, outcome } if *done == id => Some(outcome.clone()),
            _ => None,
        })
        .await
}

/// The bodies of the action requests to `name`, oldest first.
fn bodies(control: &MockControl, name: &str) -> Vec<Value> {
    let path = format!("/v1/actions/{name}");
    control
        .requests()
        .into_iter()
        .filter(|request| request.path == path)
        .filter_map(|request| request.body)
        .collect()
}

fn ok(outcome: &ActionOutcome, count: usize) {
    assert_eq!(
        outcome,
        &ActionOutcome {
            ok: count,
            ..ActionOutcome::default()
        }
    );
}

fn requeried(control: &MockControl, name: &str) -> bool {
    control.requests().iter().any(|request| {
        request.body.as_ref().is_some_and(|body| {
            ["hosts", "services"].into_iter().any(|kind| {
                body.get(kind)
                    .and_then(Value::as_array)
                    .is_some_and(|names| names.iter().any(|n| n == name))
            })
        })
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "one end-to-end scenario")]
async fn every_action_end_to_end() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut engine = start_for(&server);
    engine.connected().await;
    let pg = ObjectKey::service("stg-db-01", "pg-connections");
    let pg_key = ServiceKey::new("stg-db-01", "pg-connections");
    let objects =
        |keys: &[&ObjectKey]| ActionTarget::Objects(keys.iter().map(|k| (*k).clone()).collect());

    // Acknowledge: the environment's author, never a notification.
    let outcome = run(
        &mut engine,
        1,
        objects(&[&pg]),
        Action::Acknowledge {
            comment: "on it".to_owned(),
            sticky: false,
            persistent: false,
            expiry: None,
        },
    )
    .await;
    ok(&outcome, 1);
    let body = &bodies(&control, "acknowledge-problem")[0];
    assert_eq!(body["author"], "icygui-test");
    assert_eq!(body["notify"], false);
    assert_eq!(body["services"], json!(["stg-db-01!pg-connections"]));
    engine
        .snapshot(|snapshot| snapshot.services[&pg_key].check.acknowledgement == AckKind::Normal)
        .await;
    // The `AcknowledgementSet` event shows it (no re-query needed; see
    // `a_forced_check_is_not_followed_by_a_query_of_its_targets`).

    // Remove it.
    let outcome = run(
        &mut engine,
        2,
        objects(&[&pg]),
        Action::RemoveAcknowledgement,
    )
    .await;
    ok(&outcome, 1);
    engine
        .snapshot(|snapshot| snapshot.services[&pg_key].check.acknowledgement == AckKind::None)
        .await;

    // Acknowledging an OK service fails for that object only.
    let ssh = ObjectKey::service("stg-db-01", "ssh");
    let outcome = run(
        &mut engine,
        3,
        objects(&[&ssh, &pg]),
        Action::Acknowledge {
            comment: "both".to_owned(),
            sticky: true,
            persistent: false,
            expiry: None,
        },
    )
    .await;
    assert_eq!(outcome.ok, 1);
    assert_eq!(outcome.error, None);
    assert_eq!(outcome.failed.len(), 1);
    assert_eq!(outcome.failed[0].0, "stg-db-01!ssh");
    assert!(outcome.failed[0].1.contains("OK"), "{:?}", outcome.failed);
    engine
        .snapshot(|snapshot| snapshot.services[&pg_key].check.acknowledgement == AckKind::Sticky)
        .await;

    // Comments: add, then remove by name.
    let outcome = run(
        &mut engine,
        4,
        objects(&[&pg]),
        Action::AddComment {
            text: "pgbouncer restarted".to_owned(),
            expiry: None,
        },
    )
    .await;
    ok(&outcome, 1);
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot.comments.get(&pg).is_some_and(|comments| {
                comments
                    .iter()
                    .any(|comment| comment.text == "pgbouncer restarted")
            })
        })
        .await;
    let comment = snapshot.comments[&pg]
        .iter()
        .find(|comment| comment.text == "pgbouncer restarted")
        .unwrap()
        .name
        .clone();
    assert_eq!(bodies(&control, "add-comment")[0]["author"], "icygui-test");
    let outcome = run(
        &mut engine,
        5,
        ActionTarget::Comment(comment.clone()),
        Action::RemoveAllDowntimes,
    )
    .await;
    ok(&outcome, 1);
    engine
        .snapshot(|snapshot| {
            snapshot
                .comments
                .get(&pg)
                .is_none_or(|comments| comments.iter().all(|c| c.name != comment))
        })
        .await;

    // A fixed downtime for a host and all its services, removed for the
    // host's objects at once.
    let host = ObjectKey::host("stg-db-01");
    let now = control.now().as_unix_seconds();
    let outcome = run(
        &mut engine,
        6,
        objects(&[&host]),
        Action::ScheduleDowntime {
            comment: "kernel update".to_owned(),
            start: Timestamp::from_unix_seconds(now - 60.0),
            end: Timestamp::from_unix_seconds(now + 3_600.0),
            mode: DowntimeMode::Fixed,
            all_services: true,
            child_options: ChildOptions::None,
            trigger_name: None,
        },
    )
    .await;
    ok(&outcome, 1);
    let body = &bodies(&control, "schedule-downtime")[0];
    assert_eq!(body["fixed"], true);
    assert_eq!(body["all_services"], true);
    assert_eq!(body["author"], "icygui-test");
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot.hosts[&"stg-db-01".into()].check.downtime_depth == 1
                && snapshot.services[&pg_key].check.downtime_depth == 1
        })
        .await;
    assert!(snapshot.downtimes.contains_key(&host));
    let mut everything = vec![host.clone()];
    everything.extend(
        snapshot
            .services_of(&"stg-db-01".into())
            .map(|service| service.object_key()),
    );
    let outcome = run(
        &mut engine,
        7,
        ActionTarget::Objects(everything),
        Action::RemoveAllDowntimes,
    )
    .await;
    assert_eq!(outcome.error, None);
    assert!(outcome.ok >= 1, "{outcome:?}");
    engine
        .snapshot(|snapshot| {
            snapshot.downtimes.is_empty()
                && snapshot.hosts[&"stg-db-01".into()].check.downtime_depth == 0
        })
        .await;

    // A flexible downtime removed by name.
    let outcome = run(
        &mut engine,
        8,
        objects(&[&pg]),
        Action::ScheduleDowntime {
            comment: "flexible".to_owned(),
            start: Timestamp::from_unix_seconds(now),
            end: Timestamp::from_unix_seconds(now + 7_200.0),
            mode: DowntimeMode::Flexible { duration: 600.0 },
            all_services: false,
            child_options: ChildOptions::None,
            trigger_name: None,
        },
    )
    .await;
    ok(&outcome, 1);
    let snapshot = engine
        .snapshot(|snapshot| snapshot.downtimes.contains_key(&pg))
        .await;
    let downtime = snapshot.downtimes[&pg][0].name.clone();
    assert_eq!(
        bodies(&control, "schedule-downtime")[1]["duration"].as_f64(),
        Some(600.0)
    );
    let outcome = run(
        &mut engine,
        9,
        ActionTarget::Downtime(downtime),
        Action::RemoveAllDowntimes,
    )
    .await;
    ok(&outcome, 1);
    engine
        .snapshot(|snapshot| !snapshot.downtimes.contains_key(&pg))
        .await;

    // A passive check result.
    let apt = ObjectKey::service("stg-web-01", "apt");
    let outcome = run(
        &mut engine,
        10,
        objects(&[&apt]),
        Action::ProcessCheckResult {
            exit_status: 2,
            output: "APT CRITICAL - 3 security updates".to_owned(),
            perfdata: vec!["updates=3;1;2".to_owned()],
            ttl: None,
        },
    )
    .await;
    ok(&outcome, 1);
    let snapshot = engine
        .snapshot(|snapshot| {
            snapshot.services[&ServiceKey::new("stg-web-01", "apt")].state == ServiceState::Critical
        })
        .await;
    let stored = &snapshot.services[&ServiceKey::new("stg-web-01", "apt")];
    assert_eq!(stored.check.output(), "APT CRITICAL - 3 security updates");
    assert_eq!(stored.check.result.as_ref().unwrap().perfdata.len(), 1);

    // Check now: forced, Icinga picks the time.
    let before = snapshot.services[&ServiceKey::new("stg-web-01", "apt")]
        .check
        .last_check;
    let outcome = run(
        &mut engine,
        11,
        objects(&[&ObjectKey::service("stg-web-01", "ssh")]),
        Action::CheckNow { force: true },
    )
    .await;
    ok(&outcome, 1);
    let body = &bodies(&control, "reschedule-check")[0];
    assert_eq!(body["force"], true);
    assert!(body.get("next_check").is_none());
    let ssh_key = ServiceKey::new("stg-web-01", "ssh");
    engine
        .snapshot(|snapshot| snapshot.services[&ssh_key].check.result.is_some())
        .await;
    assert!(before.is_some());

    // Host process-check-result: DOWN.
    let web = ObjectKey::host("stg-web-02");
    let outcome = run(
        &mut engine,
        12,
        objects(&[&web]),
        Action::ProcessCheckResult {
            exit_status: 2,
            output: "PING CRITICAL".to_owned(),
            perfdata: Vec::new(),
            ttl: Some(300.0),
        },
    )
    .await;
    ok(&outcome, 1);
    engine
        .snapshot(|snapshot| snapshot.hosts[&"stg-web-02".into()].state == HostState::Down)
        .await;
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_command_defaults_the_endpoint() {
    let mut scenario = scenarios::lab();
    for service in &mut scenario.services {
        if service.key.name.as_ref() == "ssh" && service.key.host.as_str() == "lab-01" {
            service.check.command_endpoint = Some("lab-agent".to_owned());
        }
    }
    scenario.endpoints.push(Endpoint {
        name: "lab-agent".to_owned(),
        zone: "master".to_owned(),
        connected: true,
    });
    let server = mock(MockConfig::with_scenario(scenario)).await;
    let control = server.control();
    let mut engine = start_for(&server);
    engine.connected().await;

    let outcome = run(
        &mut engine,
        1,
        ActionTarget::Objects(vec![
            ObjectKey::service("lab-01", "ssh"),
            ObjectKey::service("lab-01", "ping4"),
        ]),
        Action::ExecuteCommand {
            command_type: CommandType::CheckCommand,
            command: None,
            endpoint: None,
            macros: Vars::new(),
            ttl: 60.0,
        },
    )
    .await;
    ok(&outcome, 2);
    let mut requests: BTreeMap<String, Option<String>> = BTreeMap::new();
    for body in bodies(&control, "execute-command") {
        let names = body["services"].as_array().unwrap();
        for name in names {
            requests.insert(
                name.as_str().unwrap().to_owned(),
                body.get("endpoint")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            );
        }
    }
    assert_eq!(
        requests,
        BTreeMap::from([
            // Its own command endpoint, through Icinga's default.
            ("lab-01!ssh".to_owned(), None),
            // The instance itself otherwise.
            ("lab-01!ping4".to_owned(), Some("lab-icinga".to_owned())),
        ])
    );
    engine.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_actions_report_why() {
    let server = mock(MockConfig {
        users: vec![MockUser::root(), MockUser::read_only("viewer", "secret")],
        ..MockConfig::with_scenario(scenarios::staging())
    })
    .await;
    let mut environment = environment(&server);
    environment.auth = ic_config::AuthConfig::Basic {
        username: "viewer".to_owned(),
    };
    environment.author = None;
    let mut engine = start(environment, FakeSecrets::with(ENV_ID, "secret"), tuning());
    engine.connected().await;
    let outcome = run(
        &mut engine,
        1,
        ActionTarget::Objects(vec![ObjectKey::service("stg-db-01", "pg-connections")]),
        Action::RemoveAcknowledgement,
    )
    .await;
    assert_eq!(outcome.ok, 0);
    assert!(outcome.failed.is_empty());
    let error = outcome.error.unwrap();
    assert!(error.contains("remove-acknowledgement"), "{error}");
    engine.shutdown();

    // Not connected: answered at once.
    let secrets = Arc::new(FakeSecrets::default());
    let mut engine = start(crate::support::environment(&server), secrets, tuning());
    engine
        .wait_state(|state| *state == ConnectionState::MissingSecret)
        .await;
    let outcome = run(
        &mut engine,
        2,
        ActionTarget::Objects(vec![ObjectKey::host("stg-db-01")]),
        Action::CheckNow { force: true },
    )
    .await;
    assert_eq!(outcome.error.as_deref(), Some("not connected to Icinga"));
    engine.shutdown();
}

fn late_downtime(control: &MockControl, comment: &str) -> Action {
    let now = control.now().as_unix_seconds();
    Action::ScheduleDowntime {
        comment: comment.to_owned(),
        start: Timestamp::from_unix_seconds(now),
        end: Timestamp::from_unix_seconds(now + 3_600.0),
        mode: DowntimeMode::Fixed,
        all_services: false,
        child_options: ChildOptions::None,
        trigger_name: None,
    }
}

fn downtimes_with(control: &MockControl, comment: &str) -> usize {
    control
        .downtimes()
        .iter()
        .filter(|downtime| downtime.comment == comment)
        .count()
}

/// Icinga applies a downtime but answers too late: the outcome says it
/// may have been applied (not a plain failure), the object is re-queried,
/// and a retry doesn't schedule it twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unanswered_downtime_is_never_scheduled_twice() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.tuning.action_timeout = Duration::from_millis(300);
    let mut engine = launch.start();
    engine.connected().await;
    let api = ObjectKey::host("stg-api-01");
    let target = || ActionTarget::Objects(vec![api.clone()]);

    control.delay_action_answers(Duration::from_secs(3));
    let outcome = run(&mut engine, 1, target(), late_downtime(&control, "rack")).await;
    assert_eq!(outcome.ok, 0);
    assert_eq!(outcome.error, None, "not a failure as a whole");
    assert_eq!(outcome.failed.len(), 1);
    let (name, reason) = &outcome.failed[0];
    assert_eq!(name, "stg-api-01");
    assert!(
        reason.contains("no answer") && reason.contains("may have applied"),
        "{reason}"
    );
    assert_eq!(downtimes_with(&control, "rack"), 1, "Icinga applied it");
    // The stream brings it, and the host is re-queried.
    engine
        .snapshot(|snapshot| {
            snapshot
                .downtimes
                .get(&api)
                .is_some_and(|list| list.iter().any(|downtime| downtime.comment == "rack"))
        })
        .await;
    assert!(wait_until(|| requeried(&control, "stg-api-01")).await);

    // The same again: not sent, it's there.
    control.delay_action_answers(Duration::ZERO);
    let outcome = run(&mut engine, 2, target(), late_downtime(&control, "rack")).await;
    assert_eq!(outcome.ok, 0);
    assert_eq!(outcome.failed.len(), 1);
    assert!(
        outcome.failed[0].1.contains("not sent again"),
        "{}",
        outcome.failed[0].1
    );
    assert_eq!(downtimes_with(&control, "rack"), 1);
    assert_eq!(bodies(&control, "schedule-downtime").len(), 1);

    // Another downtime is a new decision, and actions that are safe to
    // repeat aren't held back.
    let outcome = run(&mut engine, 3, target(), late_downtime(&control, "other")).await;
    ok(&outcome, 1);
    let outcome = run(&mut engine, 4, target(), Action::CheckNow { force: true }).await;
    ok(&outcome, 1);
    engine.shutdown();
}

/// While nothing shows whether an unanswered comment was applied, adding
/// a comment to that object is held back; other objects aren't.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_comment_without_an_answer_holds_back_comments_on_its_object() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    let mut launch = Launch::new(&server);
    launch.tuning.action_timeout = Duration::from_millis(300);
    let mut engine = launch.start();
    engine.connected().await;
    let api = ObjectKey::host("stg-api-01");
    let db = ObjectKey::host("stg-db-01");
    let comment = |text: &str| Action::AddComment {
        text: text.to_owned(),
        expiry: None,
    };

    // The answer (and the request's handling) is delayed past the
    // timeout: the client gave up before anything happened, but it can't
    // know that.
    control.set_latency(Duration::from_secs(2));
    let outcome = run(
        &mut engine,
        1,
        ActionTarget::Objects(vec![api.clone()]),
        comment("first"),
    )
    .await;
    assert_eq!(outcome.ok, 0);
    assert!(outcome.failed[0].1.contains("no answer"), "{outcome:?}");
    control.set_latency(Duration::ZERO);

    let outcome = run(
        &mut engine,
        2,
        ActionTarget::Objects(vec![api.clone(), db.clone()]),
        comment("second"),
    )
    .await;
    assert_eq!(outcome.ok, 1, "{outcome:?}");
    assert_eq!(outcome.failed.len(), 1);
    let (name, reason) = &outcome.failed[0];
    assert_eq!(name, "stg-api-01");
    assert!(
        reason.contains("held back") && reason.contains("min"),
        "{reason}"
    );
    let second: Vec<Value> = bodies(&control, "add-comment")
        .into_iter()
        .filter(|body| body["comment"] == "second")
        .collect();
    assert_eq!(second.len(), 1);
    assert_eq!(
        second[0]["hosts"],
        json!(["stg-db-01"]),
        "not to the held host"
    );
    engine.shutdown();
}

/// A forced check of many services: the results come as `CheckResult`
/// events once Icinga ran the checks; querying the services right after
/// the action would only bring their state from before, at the moment
/// Icinga is busy running them. Without the events, the targets are
/// re-queried.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forced_check_is_not_followed_by_a_query_of_its_targets() {
    let server = mock(MockConfig::with_scenario(scenarios::staging())).await;
    let control = server.control();
    // The freshness watchdog looks once (the simulator is off, so some
    // objects are overdue), then not again within the test.
    let quiet_watchdog = || Tuning {
        watchdog_interval: Duration::from_hours(1),
        ..tuning()
    };
    let mut engine = start(
        environment(&server),
        FakeSecrets::with(ENV_ID, PASSWORD),
        quiet_watchdog(),
    );
    engine.connected().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let services: Vec<ObjectKey> = control
        .services()
        .iter()
        .map(|service| ObjectKey::from(service.key.clone()))
        .collect();
    assert!(services.len() > 20);
    let before = control
        .service("stg-web-01", "http")
        .and_then(|service| service.check.last_check)
        .unwrap();
    control.clear_requests();
    let outcome = run(
        &mut engine,
        1,
        ActionTarget::Objects(services.clone()),
        Action::CheckNow { force: true },
    )
    .await;
    ok(&outcome, services.len());
    // The checks run and their results arrive as events.
    engine
        .snapshot(|snapshot| {
            snapshot
                .services
                .get(&ServiceKey::new("stg-web-01", "http"))
                .and_then(|service| service.check.last_check)
                .is_some_and(|checked| checked > before)
        })
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let by_name = control
        .requests()
        .iter()
        .filter(|request| request.path == "/v1/objects/services")
        .count();
    assert_eq!(by_name, 0, "no re-query");
    engine.shutdown();

    // A user without `CheckResult` events: the targets are re-queried.
    let mut config = MockConfig::with_scenario(scenarios::staging());
    config.users = vec![MockUser::new(
        USER,
        PASSWORD,
        &[
            "objects/query/*",
            "status/query",
            "actions/*",
            "events/StateChange",
        ],
    )];
    let server = mock(config).await;
    let control = server.control();
    let mut engine = start(
        environment(&server),
        FakeSecrets::with(ENV_ID, PASSWORD),
        quiet_watchdog(),
    );
    engine.connected().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    control.clear_requests();
    let http = ObjectKey::service("stg-web-01", "http");
    let outcome = run(
        &mut engine,
        2,
        ActionTarget::Objects(vec![http]),
        Action::CheckNow { force: true },
    )
    .await;
    ok(&outcome, 1);
    assert!(wait_until(|| requeried(&control, "stg-web-01!http")).await);
    engine.shutdown();
}
