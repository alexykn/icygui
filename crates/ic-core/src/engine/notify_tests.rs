//! What applied changes mean for the rule engine and the event log.

use std::sync::Arc;

use ic_api::Detail;
use ic_config::{Dashboard, DashboardGroup, Environment, View};
use ic_model::{AckKind, CheckResult, Comment, Downtime, Host, Service, StateAfter};
use ic_rules::{DashboardRef, ScopeSetting};

use super::*;

fn t(seconds: f64) -> Timestamp {
    Timestamp::from_unix_seconds(seconds)
}

fn svc(state: ServiceState) -> CheckableState {
    CheckableState::Service(state)
}

fn key(name: &str) -> ObjectKey {
    ObjectKey::service("h", name)
}

/// A store with host `h` (up) and services `h!a` (OK) and `h!b` (hard
/// critical, since 50), and the rule inputs it produces.
struct Harness {
    store: Store,
    notify: Notify,
    seq: u64,
    log: Vec<LogEntry>,
}

impl Harness {
    fn new() -> Self {
        let mut store = Store::default();
        let mut host = Host::new("h");
        host.display_name = "Host H".to_owned();
        host.state = HostState::Up;
        host.check.last_check = Some(t(100.0));
        store.replace_hosts(vec![host], 1);
        let service = |name: &str, state: ServiceState| {
            let mut service = Service::new("h", name);
            service.display_name = format!("Service {name}");
            service.state = state;
            service.check.state_type = StateType::Hard;
            service.check.last_state_change = t(50.0);
            service.check.max_attempts = 3;
            service.check.check_interval = 300.0;
            service.check.last_check = Some(t(100.0));
            service.check.result = Some(CheckResult {
                output: format!("{state:?} output"),
                execution_end: t(100.0),
                ..CheckResult::default()
            });
            service
        };
        store.replace_services(
            vec![
                service("a", ServiceState::Ok),
                service("b", ServiceState::Critical),
            ],
            Detail::Full,
            1,
        );
        store.take_changes();
        store.take_discovered();
        Self {
            store,
            notify: Notify::new(&Environment::default()),
            seq: 10,
            log: Vec::new(),
        }
    }

    /// Applies `event` like the engine and returns the inputs it queued.
    fn apply(&mut self, event: Event) -> Vec<RuleInput> {
        self.seq += 1;
        let downtime_was_in_effect = match &event {
            Event::DowntimeRemoved { downtime, .. } => self
                .store
                .downtime_of(&downtime.object, &downtime.name)
                .is_some_and(|stored| stored.in_effect),
            _ => false,
        };
        let previous_check = match &event {
            Event::CheckResult { object, .. } => {
                self.store.check(object).and_then(|check| check.last_check)
            }
            _ => None,
        };
        if let Applied::Changed { before, after } = self.store.apply(self.seq, &event) {
            let entry = AppliedEvent {
                seq: self.seq,
                event,
                before,
                after,
                downtime_was_in_effect,
                previous_check,
            };
            self.notify.applied(&self.store, &entry, &mut self.log);
        }
        mem::take(&mut self.notify.pending)
    }

    fn log(&mut self) -> Vec<LogEntry> {
        mem::take(&mut self.log)
    }
}

use crate::store::Applied;

fn check(object: &ObjectKey, state: CheckableState, state_type: StateType, at: f64) -> Event {
    check_reachable(object, state, state_type, at, true)
}

fn check_reachable(
    object: &ObjectKey,
    state: CheckableState,
    state_type: StateType,
    at: f64,
    reachable: bool,
) -> Event {
    Event::CheckResult {
        object: object.clone(),
        result: CheckResult {
            output: format!("{state:?} now"),
            long_output: "long output".to_owned(),
            execution_end: t(at),
            ..CheckResult::default()
        },
        downtime_depth: None,
        acknowledgement: None,
        after: Some(StateAfter {
            state,
            state_type,
            attempt: 1,
            reachable,
        }),
        at: t(at + 0.5),
    }
}

fn state_of(input: &RuleInput) -> (CheckableState, StateType, f64, bool) {
    match &input.change {
        Change::State {
            current,
            state_type,
            since,
            ..
        } => (
            *current,
            *state_type,
            since.as_unix_seconds(),
            input.handled,
        ),
        other => panic!("not a state: {other:?}"),
    }
}

fn downtime(name: &str, object: ObjectKey, in_effect: bool, triggered: bool) -> Downtime {
    Downtime {
        name: name.to_owned(),
        object,
        author: "j.berg".to_owned(),
        comment: "patching".to_owned(),
        start_time: t(100.0),
        end_time: t(10_000.0),
        fixed: true,
        duration: 0.0,
        entry_time: t(90.0),
        trigger_time: triggered.then(|| t(100.0)),
        triggered_by: None,
        parent: None,
        in_effect,
        config_owned: false,
    }
}

#[test]
fn state_changes_become_inputs_and_log_entries() {
    let mut h = Harness::new();
    let a = key("a");
    let inputs = h.apply(check(
        &a,
        svc(ServiceState::Critical),
        StateType::Soft,
        200.0,
    ));
    assert_eq!(inputs.len(), 1);
    let input = &inputs[0];
    assert_eq!(input.object, a);
    assert_eq!(input.host_display, "Host H");
    assert_eq!(input.service_display.as_deref(), Some("Service a"));
    assert_eq!(input.at, t(200.5));
    assert!(input.memberships.is_empty(), "filled in with the snapshot");
    let Change::State {
        previous, output, ..
    } = &input.change
    else {
        panic!();
    };
    assert_eq!(*previous, Some(svc(ServiceState::Ok)));
    assert_eq!(output, "Service(Critical) now");
    assert_eq!(
        state_of(input),
        (svc(ServiceState::Critical), StateType::Soft, 200.0, false)
    );
    let log = h.log();
    assert_eq!(log.len(), 1);
    assert_eq!(
        log[0].kind,
        LogKind::State {
            state: svc(ServiceState::Critical),
            state_type: StateType::Soft
        }
    );
    assert_eq!(log[0].at, t(200.0), "when the state began");
    assert_eq!(log[0].text, "Service(Critical) now");

    // The StateChange event of the same check changes nothing more.
    let inputs = h.apply(Event::StateChange {
        object: a.clone(),
        state: svc(ServiceState::Critical),
        state_type: StateType::Soft,
        result: CheckResult {
            execution_end: t(200.0),
            ..CheckResult::default()
        },
        downtime_depth: None,
        acknowledgement: None,
        at: t(200.6),
    });
    assert!(inputs.is_empty());
    // A plain check result in the same state: nothing.
    assert!(
        h.apply(check(
            &a,
            svc(ServiceState::Critical),
            StateType::Soft,
            260.0
        ))
        .is_empty()
    );
    assert!(h.log().is_empty(), "plain check results aren't logged");
    // Soft turns hard: same `since`, logged too.
    let inputs = h.apply(check(
        &a,
        svc(ServiceState::Critical),
        StateType::Hard,
        320.0,
    ));
    assert_eq!(
        state_of(&inputs[0]),
        (svc(ServiceState::Critical), StateType::Hard, 200.0, false)
    );
    assert_eq!(h.log().len(), 1);
}

#[test]
fn a_host_problem_repeats_its_services_with_the_new_handling() {
    let mut h = Harness::new();
    let host = ObjectKey::host("h");
    let b = key("b");
    // The host goes down: its own input, and a repeat of h!b (a problem)
    // as handled; h!a (OK) has nothing to repeat.
    let inputs = h.apply(check(
        &host,
        CheckableState::Host(HostState::Down),
        StateType::Hard,
        200.0,
    ));
    assert_eq!(inputs.len(), 2, "{inputs:#?}");
    assert_eq!(inputs[0].object, host);
    assert_eq!(inputs[0].service_display, None);
    assert_eq!(inputs[1].object, b);
    assert_eq!(
        state_of(&inputs[1]),
        (svc(ServiceState::Critical), StateType::Hard, 50.0, true),
        "the same state and since, now handled"
    );
    assert_eq!(inputs[1].at, t(200.5));

    // h!b checks while the host is down: still handled, nothing new.
    assert!(
        h.apply(check(
            &b,
            svc(ServiceState::Critical),
            StateType::Hard,
            250.0
        ))
        .is_empty()
    );

    // The host comes back: h!b's handling ended.
    let inputs = h.apply(check(
        &host,
        CheckableState::Host(HostState::Up),
        StateType::Hard,
        300.0,
    ));
    assert_eq!(inputs.len(), 2);
    assert_eq!(
        state_of(&inputs[1]),
        (svc(ServiceState::Critical), StateType::Hard, 50.0, false)
    );
    // Its next check result confirms the state, once, with its time.
    let inputs = h.apply(check(
        &b,
        svc(ServiceState::Critical),
        StateType::Hard,
        350.0,
    ));
    assert_eq!(inputs.len(), 1);
    assert_eq!(
        state_of(&inputs[0]),
        (svc(ServiceState::Critical), StateType::Hard, 50.0, false)
    );
    assert_eq!(inputs[0].at, t(350.5));
    assert!(
        h.apply(check(
            &b,
            svc(ServiceState::Critical),
            StateType::Hard,
            400.0
        ))
        .is_empty()
    );
}

#[test]
fn a_recovery_instead_of_the_confirmation_is_judged_as_such() {
    let mut h = Harness::new();
    let host = ObjectKey::host("h");
    let b = key("b");
    let down = CheckableState::Host(HostState::Down);
    let up = CheckableState::Host(HostState::Up);
    h.apply(check(&host, down, StateType::Hard, 200.0));
    h.apply(check(&host, up, StateType::Hard, 300.0));
    let inputs = h.apply(check(&b, svc(ServiceState::Ok), StateType::Hard, 350.0));
    assert_eq!(inputs.len(), 1);
    assert_eq!(state_of(&inputs[0]).0, svc(ServiceState::Ok));
    // No confirmation left over.
    h.apply(check(
        &b,
        svc(ServiceState::Critical),
        StateType::Hard,
        400.0,
    ));
    assert!(
        h.apply(check(
            &b,
            svc(ServiceState::Critical),
            StateType::Hard,
            450.0
        ))
        .is_empty()
    );
}

#[test]
fn unreachable_hosts_are_unreachable_and_handled() {
    let mut h = Harness::new();
    let host = ObjectKey::host("h");
    let inputs = h.apply(check_reachable(
        &host,
        CheckableState::Host(HostState::Unreachable),
        StateType::Hard,
        200.0,
        false,
    ));
    assert_eq!(
        state_of(&inputs[0]),
        (
            CheckableState::Host(HostState::Unreachable),
            StateType::Hard,
            200.0,
            true
        )
    );
    // Reachable again but still down: a new state, unhandled.
    let inputs = h.apply(check(
        &host,
        CheckableState::Host(HostState::Down),
        StateType::Hard,
        300.0,
    ));
    assert_eq!(
        state_of(&inputs[0]),
        (
            CheckableState::Host(HostState::Down),
            StateType::Hard,
            200.0,
            false
        )
    );
}

#[test]
fn acknowledgements_comments_and_flapping() {
    let mut h = Harness::new();
    let b = key("b");
    let inputs = h.apply(Event::AcknowledgementSet {
        object: b.clone(),
        author: "m.keller".to_owned(),
        comment: "looking".to_owned(),
        kind: AckKind::Sticky,
        expiry: None,
        at: t(200.0),
    });
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].handled);
    assert_eq!(
        inputs[0].change,
        Change::AcknowledgementSet {
            author: "m.keller".to_owned(),
            comment: "looking".to_owned()
        }
    );
    let log = h.log();
    assert_eq!(log[0].kind, LogKind::AcknowledgementSet);
    assert_eq!(log[0].author.as_deref(), Some("m.keller"));
    assert_eq!(log[0].text, "looking");

    let inputs = h.apply(Event::AcknowledgementCleared {
        object: b.clone(),
        at: t(300.0),
    });
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].change, Change::AcknowledgementCleared);
    assert!(!inputs[0].handled);
    assert_eq!(h.log()[0].kind, LogKind::AcknowledgementCleared);
    // Cleared again (nothing to clear): nothing.
    assert!(
        h.apply(Event::AcknowledgementCleared {
            object: b.clone(),
            at: t(301.0),
        })
        .is_empty()
    );
    // The handling ended: the next check confirms.
    let inputs = h.apply(check(
        &b,
        svc(ServiceState::Critical),
        StateType::Hard,
        350.0,
    ));
    assert!(!state_of(&inputs[0]).3);
    assert_eq!(inputs[0].at, t(350.5));

    // User comments are logged; other comments aren't; no inputs.
    let mut comment = Comment {
        name: "h!b!c1".to_owned(),
        object: b.clone(),
        author: "a.ivanova".to_owned(),
        text: "vendor ticket 4711".to_owned(),
        kind: CommentKind::User,
        entry_time: t(400.0),
        expire_time: None,
        persistent: false,
    };
    assert!(
        h.apply(Event::CommentAdded {
            comment: comment.clone(),
            at: t(400.0)
        })
        .is_empty()
    );
    assert!(
        h.apply(Event::CommentRemoved {
            comment: comment.clone(),
            at: t(410.0)
        })
        .is_empty()
    );
    let log = h.log();
    assert_eq!(
        log.iter().map(|entry| entry.kind).collect::<Vec<_>>(),
        [LogKind::CommentAdded, LogKind::CommentRemoved]
    );
    assert_eq!(log[0].text, "vendor ticket 4711");
    comment.kind = CommentKind::Acknowledgement;
    h.apply(Event::CommentAdded {
        comment,
        at: t(420.0),
    });
    assert!(h.log().is_empty());

    // Flapping, once per change of the flag.
    let flapping = |flapping: bool, at: f64| Event::Flapping {
        object: b.clone(),
        flapping,
        current: 42.5,
        at: t(at),
    };
    let inputs = h.apply(flapping(true, 500.0));
    assert_eq!(inputs[0].change, Change::FlappingStarted);
    assert!(h.apply(flapping(true, 510.0)).is_empty());
    assert_eq!(
        h.apply(flapping(false, 520.0))[0].change,
        Change::FlappingStopped
    );
    let log = h.log();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].kind, LogKind::FlappingStarted);
    assert_eq!(log[0].text, "42.5 % state change");
}

#[test]
fn downtimes_start_once_and_end_only_if_they_began() {
    let mut h = Harness::new();
    let b = key("b");
    h.apply(Event::DowntimeAdded {
        downtime: downtime("h!b!d1", b.clone(), false, false),
        at: t(95.0),
    });
    // Started and triggered (a fixed downtime): two inputs (the rule
    // engine counts them as one), one log entry.
    let started = h.apply(Event::DowntimeStarted {
        downtime: downtime("h!b!d1", b.clone(), true, false),
        at: t(100.0),
    });
    let triggered = h.apply(Event::DowntimeTriggered {
        downtime: downtime("h!b!d1", b.clone(), true, true),
        at: t(100.1),
    });
    assert_eq!(started.len(), 1);
    assert_eq!(
        started[0].change,
        Change::DowntimeStarted {
            author: "j.berg".to_owned(),
            comment: "patching".to_owned()
        }
    );
    assert!(started[0].handled);
    assert_eq!(triggered.len(), 1);
    let log = h.log();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].kind, LogKind::DowntimeStarted);
    assert_eq!(log[0].author.as_deref(), Some("j.berg"));

    // Removed while in effect: ended, handling over, confirmation next.
    let ended = h.apply(Event::DowntimeRemoved {
        downtime: downtime("h!b!d1", b.clone(), false, true),
        at: t(200.0),
    });
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].change, Change::DowntimeEnded);
    assert!(!ended[0].handled);
    assert_eq!(h.log()[0].kind, LogKind::DowntimeEnded);
    assert_eq!(
        h.apply(check(
            &b,
            svc(ServiceState::Critical),
            StateType::Hard,
            250.0
        ))
        .len(),
        1
    );

    // Scheduled for later and cancelled before it began: nothing.
    h.apply(Event::DowntimeAdded {
        downtime: downtime("h!b!d2", b.clone(), false, false),
        at: t(300.0),
    });
    let cancelled = h.apply(Event::DowntimeRemoved {
        downtime: downtime("h!b!d2", b.clone(), false, false),
        at: t(310.0),
    });
    assert!(cancelled.is_empty(), "{cancelled:#?}");
    assert!(h.log().is_empty());
}

#[test]
fn discovered_changes_wait_for_the_load_and_removals_forget() {
    let mut h = Harness::new();
    let a = key("a");
    // A reload finds h!a critical (missed), and h!b gone.
    let mut critical = Service::new("h", "a");
    critical.display_name = "Service a".to_owned();
    critical.state = ServiceState::Critical;
    critical.check.state_type = StateType::Hard;
    critical.check.last_state_change = t(150.0);
    critical.check.last_check = Some(t(150.0));
    critical.check.result = Some(CheckResult {
        output: "CRITICAL - found by the reload".to_owned(),
        execution_end: t(150.0),
        ..CheckResult::default()
    });
    h.store.replace_services(vec![critical], Detail::Full, 20);
    let found = h.store.take_discovered();
    assert_eq!(found.len(), 2, "{found:#?}");
    h.notify
        .discovered(&h.store, found, true, t(160.0), &mut h.log);
    assert!(
        h.notify.pending.is_empty(),
        "deferred until the load is over"
    );
    h.notify.load_finished(&h.store, &mut h.log);
    let inputs = mem::take(&mut h.notify.pending);
    assert_eq!(inputs.len(), 2, "{inputs:#?}");
    let missed = inputs.iter().find(|input| input.object == a).unwrap();
    assert_eq!(
        state_of(missed),
        (svc(ServiceState::Critical), StateType::Hard, 150.0, false)
    );
    let Change::State { output, .. } = &missed.change else {
        panic!();
    };
    assert_eq!(output, "CRITICAL - found by the reload");
    let gone = inputs
        .iter()
        .find(|input| input.object == key("b"))
        .unwrap();
    assert_eq!(state_of(gone).0, svc(ServiceState::Pending));
    let log = h.log();
    assert_eq!(log.len(), 1, "the missed state change is logged");
    assert_eq!(log[0].at, t(150.0));

    // Outside a load, at once.
    let mut ok = Service::new("h", "a");
    ok.state = ServiceState::Ok;
    ok.check.state_type = StateType::Hard;
    ok.check.last_state_change = t(170.0);
    ok.check.last_check = Some(t(170.0));
    h.store
        .apply_fetched(Vec::new(), vec![ok], Detail::Lean, &[], 30);
    let found = h.store.take_discovered();
    h.notify
        .discovered(&h.store, found, false, t(175.0), &mut h.log);
    let inputs = mem::take(&mut h.notify.pending);
    assert_eq!(inputs.len(), 1);
    let Change::State { output, .. } = &inputs[0].change else {
        panic!();
    };
    assert!(
        output.is_empty(),
        "the stored result is older than the state: no stale output"
    );
}

#[test]
fn a_finding_is_judged_before_newer_events_about_its_object() {
    let mut h = Harness::new();
    let a = key("a");
    // A reload's tier 2 finds h!a critical and acknowledged (missed during
    // a reconnect gap); it waits for the load to end.
    let mut acknowledged = Service::new("h", "a");
    acknowledged.state = ServiceState::Critical;
    acknowledged.check.state_type = StateType::Hard;
    acknowledged.check.last_state_change = t(150.0);
    acknowledged.check.last_check = Some(t(150.0));
    acknowledged.check.acknowledgement = AckKind::Normal;
    h.store
        .apply_fetched(Vec::new(), vec![acknowledged], Detail::Lean, &[], 20);
    let found = h.store.take_discovered();
    assert_eq!(found.len(), 1);
    h.notify
        .discovered(&h.store, found, true, t(160.0), &mut h.log);
    assert!(h.notify.pending.is_empty());

    // Before the load is over, a colleague removes the acknowledgement
    // (read after the answer's query went out).
    h.seq = 100;
    let inputs = h.apply(Event::AcknowledgementCleared {
        object: a.clone(),
        at: t(170.0),
    });
    assert_eq!(inputs.len(), 2, "{inputs:#?}");
    assert_eq!(
        state_of(&inputs[0]),
        (svc(ServiceState::Critical), StateType::Hard, 150.0, true),
        "the finding first, as the answer saw it"
    );
    assert_eq!(inputs[1].change, Change::AcknowledgementCleared);
    assert!(!inputs[1].handled);
    assert!(h.notify.confirm.contains(&a), "the next check confirms");

    // The load ends: nothing is judged twice, nothing stale undoes it.
    h.notify.load_finished(&h.store, &mut h.log);
    assert!(h.notify.pending.is_empty());
    assert!(h.notify.confirm.contains(&a));

    // The next check result confirms the unhandled problem.
    let inputs = h.apply(check(
        &a,
        svc(ServiceState::Critical),
        StateType::Hard,
        180.0,
    ));
    assert_eq!(inputs.len(), 1, "{inputs:#?}");
    assert_eq!(
        state_of(&inputs[0]),
        (svc(ServiceState::Critical), StateType::Hard, 150.0, false)
    );
}

#[test]
fn a_finding_overtaken_by_an_event_keeps_its_place() {
    let mut h = Harness::new();
    let a = key("a");
    // The reload finds h!a critical; before the load ends, a check result
    // shows it recovered.
    let mut critical = Service::new("h", "a");
    critical.state = ServiceState::Critical;
    critical.check.state_type = StateType::Hard;
    critical.check.last_state_change = t(150.0);
    critical.check.last_check = Some(t(150.0));
    h.store
        .apply_fetched(Vec::new(), vec![critical], Detail::Lean, &[], 20);
    let found = h.store.take_discovered();
    h.notify
        .discovered(&h.store, found, true, t(160.0), &mut h.log);
    h.seq = 100;
    let inputs = h.apply(check(&a, svc(ServiceState::Ok), StateType::Hard, 170.0));
    let states: Vec<_> = inputs.iter().map(|input| state_of(input).0).collect();
    assert_eq!(
        states,
        [svc(ServiceState::Critical), svc(ServiceState::Ok)],
        "the problem, then its recovery"
    );
    let Change::State { output, .. } = &inputs[0].change else {
        panic!();
    };
    assert!(
        output.is_empty(),
        "the stored output belongs to the recovery"
    );
    h.notify.load_finished(&h.store, &mut h.log);
    assert!(h.notify.pending.is_empty(), "judged once");
}

#[test]
fn memberships_come_from_the_dashboards_and_rules_from_the_environment() {
    let mut environment = Environment::default();
    "prod".clone_into(&mut environment.name);
    let mut group = DashboardGroup::new("databases");
    group.notifications = ScopeSetting::Off;
    let mut board = Dashboard::new(
        "replication",
        View {
            filter: "service.name == \"b\"".to_owned(),
            problems_only: true,
            ..View::default()
        },
    );
    board.notifications = ScopeSetting::On;
    group.dashboards.push(board);
    environment.groups.push(group);
    let rules = rule_set(&environment);
    assert_eq!(rules.environment_name, "prod");
    assert_eq!(rules.groups.len(), 1);
    assert_eq!(rules.groups[0].setting, ScopeSetting::Off);
    assert_eq!(rules.groups[0].dashboards[0].setting, ScopeSetting::On);

    let mut h = Harness::new();
    h.notify = Notify::new(&environment);
    let mut dashboards = Dashboards::default();
    dashboards.configure(&environment);
    let snapshot = h.store.snapshot(1, t(0.0), Arc::default(), Arc::default());
    let data = crate::dashboards::Data {
        hosts: snapshot.hosts.clone(),
        services: snapshot.services.clone(),
        host_groups: snapshot.host_groups.clone(),
        service_groups: snapshot.service_groups.clone(),
        now: t(0.0),
    };
    let changes = crate::store::Changes {
        all: true,
        ..Default::default()
    };
    dashboards.update(
        &data,
        &changes,
        false,
        &std::sync::atomic::AtomicBool::new(false),
    );
    // h!b recovers and fails again: the new problem is judged.
    h.apply(check(
        &key("b"),
        svc(ServiceState::Ok),
        StateType::Hard,
        200.0,
    ));
    h.notify.pending = h.apply(check(
        &key("b"),
        svc(ServiceState::Critical),
        StateType::Hard,
        300.0,
    ));
    let intents = h.notify.judge(
        &dashboards,
        false,
        t(301.0),
        LocalTime {
            weekday: 0,
            minute_of_day: 600,
        },
    );
    // The group is off, the dashboard on: it notifies, named after it.
    assert_eq!(intents.len(), 1, "{intents:#?}");
    assert_eq!(intents[0].subtitle, "databases / replication");
    assert_eq!(
        dashboards.memberships(&key("b")),
        [DashboardRef {
            group_id: environment.groups[0].id.clone(),
            dashboard_id: environment.groups[0].dashboards[0].id.clone(),
        }]
    );
    assert!(dashboards.memberships(&key("a")).is_empty());
}

#[test]
fn a_check_that_shows_the_handling_over_confirms_the_state() {
    let mut h = Harness::new();
    let b = key("b");
    // Unreachable (a failed dependency): handled.
    let inputs = h.apply(check_reachable(
        &b,
        svc(ServiceState::Critical),
        StateType::Hard,
        200.0,
        false,
    ));
    assert_eq!(inputs.len(), 1);
    assert!(state_of(&inputs[0]).3);
    assert_eq!(inputs[0].at, t(200.5));
    // Reachable again and still critical: the handling ended by the
    // previous check at the latest, and this check confirms the state.
    let inputs = h.apply(check(
        &b,
        svc(ServiceState::Critical),
        StateType::Hard,
        300.0,
    ));
    assert_eq!(inputs.len(), 2, "{inputs:#?}");
    assert!(inputs.iter().all(|input| !state_of(input).3));
    assert_eq!(inputs[0].at, t(200.0), "the previous check");
    assert_eq!(inputs[1].at, t(300.5), "this check");
    // Nothing more to confirm.
    assert!(
        h.apply(check(
            &b,
            svc(ServiceState::Critical),
            StateType::Hard,
            400.0
        ))
        .is_empty()
    );

    // End to end through the rule engine: the problem notifies at once.
    let mut h = Harness::new();
    let rules = |h: &mut Harness, inputs: Vec<RuleInput>| {
        h.notify.pending = inputs;
        h.notify.judge(
            &Dashboards::default(),
            false,
            t(1_000.0),
            LocalTime {
                weekday: 0,
                minute_of_day: 600,
            },
        )
    };
    let inputs = h.apply(check(&b, svc(ServiceState::Ok), StateType::Hard, 150.0));
    assert!(rules(&mut h, inputs).is_empty());
    let inputs = h.apply(check_reachable(
        &b,
        svc(ServiceState::Critical),
        StateType::Hard,
        200.0,
        false,
    ));
    assert!(rules(&mut h, inputs).is_empty(), "unreachable: handled");
    let inputs = h.apply(check(
        &b,
        svc(ServiceState::Critical),
        StateType::Hard,
        300.0,
    ));
    let intents = rules(&mut h, inputs);
    assert_eq!(intents.len(), 1, "{intents:#?}");
    assert_eq!(intents[0].title, "CRITICAL · Service b on Host H");
}

#[test]
fn a_problem_found_begun_again_is_a_new_problem() {
    let mut h = Harness::new();
    let b = key("b");
    // A reload finds h!b critical as before, but since 150 (it was 50): it
    // recovered and failed again while no event told.
    let mut again = Service::new("h", "b");
    again.display_name = "Service b".to_owned();
    again.state = ServiceState::Critical;
    again.check.state_type = StateType::Hard;
    again.check.last_state_change = t(150.0);
    again.check.last_check = Some(t(150.0));
    again.check.result = Some(CheckResult {
        output: "CRITICAL - again".to_owned(),
        execution_end: t(150.0),
        ..CheckResult::default()
    });
    h.store
        .apply_fetched(Vec::new(), vec![again], Detail::Full, &[], 20);
    let found = h.store.take_discovered();
    h.notify
        .discovered(&h.store, found, false, t(160.0), &mut h.log);
    let inputs = mem::take(&mut h.notify.pending);
    assert_eq!(inputs.len(), 1, "{inputs:#?}");
    let Change::State {
        previous,
        current,
        since,
        ..
    } = &inputs[0].change
    else {
        panic!();
    };
    assert_eq!(inputs[0].object, b);
    assert_eq!(
        *previous,
        Some(svc(ServiceState::Ok)),
        "the missed recovery"
    );
    assert_eq!((*current, *since), (svc(ServiceState::Critical), t(150.0)));
    let log = h.log();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].at, t(150.0), "logged when it began");

    // The same `since` within a millisecond is the same problem.
    let mut same = Service::new("h", "b");
    same.state = ServiceState::Critical;
    same.check.state_type = StateType::Hard;
    same.check.last_state_change = t(150.000_4);
    same.check.last_check = Some(t(200.0));
    h.store
        .apply_fetched(Vec::new(), vec![same], Detail::Lean, &[], 30);
    let found = h.store.take_discovered();
    h.notify
        .discovered(&h.store, found, false, t(210.0), &mut h.log);
    assert!(h.notify.pending.is_empty());
}

#[test]
fn the_first_load_seeds_problems_and_flapping_objects() {
    let mut h = Harness::new();
    // h!a flaps (OK now); h!b is critical.
    let mut flapping = Service::new("h", "a");
    flapping.state = ServiceState::Ok;
    flapping.check.state_type = StateType::Hard;
    flapping.check.flapping = true;
    flapping.check.last_state_change = t(90.0);
    h.store
        .apply_fetched(Vec::new(), vec![flapping], Detail::Lean, &[], 20);
    h.store.take_discovered();
    h.notify
        .seed(&h.store, &HashSet::new(), t(120.0), &mut h.log);
    assert!(h.notify.has_pending());
    assert!(h.log.is_empty(), "seeds aren't logged");
    let seeds = mem::take(&mut h.notify.seeds);
    let summary: Vec<(ObjectKey, CheckableState, bool, bool)> = seeds
        .iter()
        .map(|(input, flapping)| {
            let (state, _, _, handled) = state_of(input);
            (input.object.clone(), state, handled, *flapping)
        })
        .collect();
    assert_eq!(
        summary,
        [
            (key("a"), svc(ServiceState::Ok), false, true),
            (key("b"), svc(ServiceState::Critical), false, false),
        ]
    );
    assert!(seeds.iter().all(|(input, _)| input.at == t(120.0)));
    let Change::State { output, .. } = &seeds[1].0.change else {
        panic!();
    };
    assert_eq!(output, "Critical output");
}

/// A state change the first load's answers already show, but which came
/// after the stream subscribed (it waited during the load, or a background
/// start's delay), is judged and logged like any state change; the
/// problems from before stay seeds, and a flapping object is seeded all
/// the same.
#[test]
fn the_first_load_judges_what_began_after_the_stream_opened() {
    let mut h = Harness::new();
    let mut flapping = Service::new("h", "a");
    flapping.state = ServiceState::Critical;
    flapping.check.state_type = StateType::Hard;
    flapping.check.flapping = true;
    flapping.check.last_state_change = t(110.0);
    h.store
        .apply_fetched(Vec::new(), vec![flapping], Detail::Lean, &[], 20);
    h.store.take_discovered();
    // h!b (critical) began during the load; so did h!a's state.
    let began: HashSet<ObjectKey> = [key("a"), key("b")].into_iter().collect();
    h.notify.seed(&h.store, &began, t(120.0), &mut h.log);
    let seeds: Vec<ObjectKey> = h
        .notify
        .seeds
        .iter()
        .map(|(input, _)| input.object.clone())
        .collect();
    assert_eq!(seeds, [key("a")], "a flapping object stays a seed");
    assert_eq!(h.notify.pending.len(), 1);
    let input = &h.notify.pending[0];
    assert_eq!(input.object, key("b"));
    let Change::State {
        previous,
        current,
        output,
        ..
    } = &input.change
    else {
        panic!("{input:?}");
    };
    assert_eq!(*previous, None);
    assert_eq!(*current, svc(ServiceState::Critical));
    assert_eq!(output, "Critical output");
    assert_eq!(h.log.len(), 1);
    assert_eq!(h.log[0].object, key("b"));
    assert!(matches!(h.log[0].kind, LogKind::State { .. }));
}
