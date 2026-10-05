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

use crate::support::{
    ENV_ID, FakeSecrets, environment, mock, start, start_for, tuning, wait_until,
};
use ic_core::{ActionOutcome, Command, ConnectionState, CoreEvent};
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
    // The target is re-queried right after.
    assert!(wait_until(|| requeried(&control, "stg-db-01!pg-connections")).await);

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
