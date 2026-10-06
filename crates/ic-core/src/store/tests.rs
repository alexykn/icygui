use std::sync::Arc;

use ic_api::Detail;
use ic_model::{
    AckKind, CheckResult, CheckableState, Comment, CommentKind, Downtime, Event, Host, HostState,
    Links, ObjectKey, Service, ServiceKey, ServiceState, StateAfter, StateType, Timestamp,
};
use serde_json::json;

use super::{Applied, Overview, Store};

fn t(seconds: f64) -> Timestamp {
    Timestamp::from_unix_seconds(seconds)
}

fn host(name: &str, state: HostState) -> Host {
    let mut host = Host::new(name);
    host.state = state;
    host.check.last_check = Some(t(100.0));
    host
}

fn service(host: &str, name: &str, state: ServiceState) -> Service {
    let mut service = Service::new(host, name);
    service.state = state;
    service.check.max_attempts = 3;
    service.check.check_interval = 300.0;
    service.check.retry_interval = 60.0;
    service.check.last_check = Some(t(100.0));
    service
}

fn key(host: &str, name: &str) -> ObjectKey {
    ObjectKey::service(host, name)
}

/// A store with host `h` (up) and services `h!a` (OK), `h!b` (critical),
/// loaded at sequence number 10.
fn loaded() -> Store {
    let mut store = Store::default();
    store.replace_hosts(vec![host("h", HostState::Up)], 10);
    store.replace_services(
        vec![
            service("h", "a", ServiceState::Ok),
            service("h", "b", ServiceState::Critical),
        ],
        Detail::Lean,
        10,
    );
    store.take_changes();
    store
}

fn result(state: i32, output: &str, end: f64) -> CheckResult {
    CheckResult {
        output: output.to_owned(),
        exit_status: state,
        execution_start: t(end - 1.0),
        execution_end: t(end),
        schedule_start: t(end - 1.0),
        active: true,
        ..CheckResult::default()
    }
}

fn check_result(
    object: ObjectKey,
    state: CheckableState,
    state_type: StateType,
    attempt: u32,
    end: f64,
) -> Event {
    let code = match state {
        CheckableState::Service(ServiceState::Ok) | CheckableState::Host(HostState::Up) => 0,
        CheckableState::Service(ServiceState::Warning) => 1,
        CheckableState::Service(ServiceState::Unknown) => 3,
        _ => 2,
    };
    Event::CheckResult {
        object,
        result: result(code, "output", end),
        downtime_depth: Some(0),
        acknowledgement: None,
        after: Some(StateAfter {
            state,
            state_type,
            attempt,
            reachable: !matches!(state, CheckableState::Host(HostState::Unreachable)),
        }),
        at: t(end + 0.1),
    }
}

fn svc(state: ServiceState) -> CheckableState {
    CheckableState::Service(state)
}

fn stored(store: &Store, host: &str, name: &str) -> Service {
    (**store.services.get(&ServiceKey::new(host, name)).unwrap()).clone()
}

#[test]
fn a_check_result_updates_the_whole_object() {
    let mut store = loaded();
    let applied = store.apply(
        11,
        &check_result(
            key("h", "a"),
            svc(ServiceState::Warning),
            StateType::Soft,
            1,
            200.0,
        ),
    );
    let Applied::Changed { before, after } = applied else {
        panic!("{applied:?}");
    };
    assert_eq!(before.unwrap().state, svc(ServiceState::Ok));
    assert_eq!(after.unwrap().state, svc(ServiceState::Warning));
    let a = stored(&store, "h", "a");
    assert_eq!(a.state, ServiceState::Warning);
    assert_eq!(a.check.state_type, StateType::Soft);
    assert_eq!(a.check.attempt, 1);
    assert_eq!(a.check.last_state_change, t(200.0));
    assert_eq!(a.check.last_hard_state_change, Timestamp::EPOCH, "soft");
    assert_eq!(a.check.last_check, Some(t(200.0)));
    assert_eq!(
        a.check.next_check,
        Some(t(260.0)),
        "retry interval while soft"
    );
    assert_eq!(a.check.output(), "output");
    let changes = store.take_changes();
    assert!(changes.any);
    assert!(changes.objects.contains(&key("h", "a")));

    // The next attempt: same state, attempt 2; no state change.
    store.apply(
        12,
        &check_result(
            key("h", "a"),
            svc(ServiceState::Warning),
            StateType::Soft,
            2,
            260.0,
        ),
    );
    let a = stored(&store, "h", "a");
    assert_eq!(a.check.attempt, 2);
    assert_eq!(a.check.last_state_change, t(200.0));

    // Hard now.
    store.apply(
        13,
        &check_result(
            key("h", "a"),
            svc(ServiceState::Warning),
            StateType::Hard,
            1,
            320.0,
        ),
    );
    let a = stored(&store, "h", "a");
    assert_eq!(a.check.state_type, StateType::Hard);
    assert_eq!(a.check.last_hard_state_change, t(320.0));
    assert_eq!(a.check.last_state_change, t(200.0));
    assert_eq!(
        a.check.next_check,
        Some(t(620.0)),
        "check interval when hard"
    );

    // Recovery: a hard state change.
    store.apply(
        14,
        &check_result(
            key("h", "a"),
            svc(ServiceState::Ok),
            StateType::Hard,
            1,
            400.0,
        ),
    );
    let a = stored(&store, "h", "a");
    assert_eq!(a.state, ServiceState::Ok);
    assert_eq!(a.check.last_state_change, t(400.0));
    assert_eq!(a.check.last_hard_state_change, t(400.0));
}

#[test]
fn without_vars_after_the_result_state_decides() {
    let mut store = loaded();
    store.apply(
        11,
        &Event::CheckResult {
            object: key("h", "b"),
            result: result(0, "OK now", 200.0),
            downtime_depth: None,
            acknowledgement: None,
            after: None,
            at: t(200.0),
        },
    );
    let b = stored(&store, "h", "b");
    assert_eq!(b.state, ServiceState::Ok);
    assert_eq!(b.check.state_type, StateType::Hard);

    store.apply(
        12,
        &Event::CheckResult {
            object: ObjectKey::host("h"),
            result: result(1, "warning is up for hosts", 200.0),
            downtime_depth: None,
            acknowledgement: None,
            after: None,
            at: t(200.0),
        },
    );
    assert_eq!(store.hosts.get(&"h".into()).unwrap().state, HostState::Up);
}

#[test]
fn hosts_become_unreachable_without_a_state_change() {
    let mut store = loaded();
    let down = |store: &mut Store, seq, state, end| {
        store.apply(
            seq,
            &check_result(
                ObjectKey::host("h"),
                CheckableState::Host(state),
                StateType::Hard,
                1,
                end,
            ),
        );
        (**store.hosts.get(&"h".into()).unwrap()).clone()
    };
    let host = down(&mut store, 11, HostState::Down, 200.0);
    assert_eq!(host.state, HostState::Down);
    assert!(host.check.reachable);
    assert_eq!(host.check.last_state_change, t(200.0));
    let host = down(&mut store, 12, HostState::Unreachable, 300.0);
    assert_eq!(host.state, HostState::Unreachable);
    assert!(!host.check.reachable);
    assert_eq!(
        host.check.last_state_change,
        t(200.0),
        "down and unreachable are the same raw state"
    );
}

#[test]
fn acknowledgements_end_like_in_icinga() {
    let mut store = loaded();
    let set = |kind| Event::AcknowledgementSet {
        object: key("h", "b"),
        author: "a".to_owned(),
        comment: "c".to_owned(),
        kind,
        expiry: Some(t(9_999.0)),
        at: t(150.0),
    };
    store.apply(11, &set(AckKind::Normal));
    let b = stored(&store, "h", "b");
    assert_eq!(b.check.acknowledgement, AckKind::Normal);
    assert_eq!(b.check.acknowledgement_expiry, Some(t(9_999.0)));
    // A normal acknowledgement ends with any state change.
    store.apply(
        12,
        &check_result(
            key("h", "b"),
            svc(ServiceState::Warning),
            StateType::Hard,
            1,
            200.0,
        ),
    );
    assert_eq!(
        stored(&store, "h", "b").check.acknowledgement,
        AckKind::None
    );

    // A sticky one survives problem changes, ends with the recovery.
    store.apply(13, &set(AckKind::Sticky));
    store.apply(
        14,
        &check_result(
            key("h", "b"),
            svc(ServiceState::Critical),
            StateType::Hard,
            1,
            300.0,
        ),
    );
    assert_eq!(
        stored(&store, "h", "b").check.acknowledgement,
        AckKind::Sticky
    );
    // The event's boolean doesn't turn sticky into normal.
    let mut event = check_result(
        key("h", "b"),
        svc(ServiceState::Critical),
        StateType::Hard,
        1,
        350.0,
    );
    if let Event::CheckResult {
        acknowledgement, ..
    } = &mut event
    {
        *acknowledgement = Some(AckKind::Normal);
    }
    store.apply(15, &event);
    assert_eq!(
        stored(&store, "h", "b").check.acknowledgement,
        AckKind::Sticky
    );
    store.apply(
        16,
        &check_result(
            key("h", "b"),
            svc(ServiceState::Ok),
            StateType::Hard,
            1,
            400.0,
        ),
    );
    assert_eq!(
        stored(&store, "h", "b").check.acknowledgement,
        AckKind::None
    );

    store.apply(17, &set(AckKind::Normal));
    store.apply(
        18,
        &Event::AcknowledgementCleared {
            object: key("h", "b"),
            at: t(500.0),
        },
    );
    let b = stored(&store, "h", "b");
    assert_eq!(b.check.acknowledgement, AckKind::None);
    assert_eq!(b.check.acknowledgement_expiry, None);
}

#[test]
fn a_state_change_after_its_check_result_changes_nothing_more() {
    let mut store = loaded();
    let check = check_result(
        key("h", "a"),
        svc(ServiceState::Critical),
        StateType::Hard,
        1,
        200.0,
    );
    store.apply(11, &check);
    let after_check = stored(&store, "h", "a");
    let Event::CheckResult { result, .. } = check else {
        unreachable!()
    };
    store.apply(
        12,
        &Event::StateChange {
            object: key("h", "a"),
            state: svc(ServiceState::Critical),
            state_type: StateType::Hard,
            result,
            downtime_depth: Some(0),
            acknowledgement: Some(AckKind::None),
            at: t(200.1),
        },
    );
    assert_eq!(stored(&store, "h", "a"), after_check);
}

#[test]
fn flapping_and_unknown_objects() {
    let mut store = loaded();
    store.apply(
        11,
        &Event::Flapping {
            object: key("h", "a"),
            flapping: true,
            current: 42.0,
            at: t(1.0),
        },
    );
    let a = stored(&store, "h", "a");
    assert!(a.check.flapping);
    assert!((a.check.flapping_current - 42.0).abs() < f64::EPSILON);

    let unknown = key("h", "new");
    assert_eq!(
        store.apply(
            12,
            &check_result(
                unknown.clone(),
                svc(ServiceState::Ok),
                StateType::Hard,
                1,
                1.0
            )
        ),
        Applied::Unknown(unknown)
    );
    assert_eq!(
        store.apply(
            13,
            &Event::ObjectLifecycle {
                change: ic_model::ObjectChange::Created,
                object_type: "Host".to_owned(),
                name: "x".to_owned(),
                at: t(1.0),
            }
        ),
        Applied::Ignored
    );
}

#[test]
fn events_already_reflected_in_a_query_answer_are_skipped() {
    let mut store = loaded();
    // Loaded at 10: line 9 was read before the query went out.
    assert_eq!(
        store.apply(
            9,
            &check_result(
                key("h", "a"),
                svc(ServiceState::Critical),
                StateType::Hard,
                1,
                50.0
            )
        ),
        Applied::Stale
    );
    assert_eq!(stored(&store, "h", "a").state, ServiceState::Ok);
}

#[test]
fn answers_older_than_an_applied_event_keep_the_event_state() {
    let mut store = loaded();
    // Line 12 (critical) is applied; then an answer of a query sent at 11
    // arrives, still saying OK but with new vars.
    store.apply(
        12,
        &check_result(
            key("h", "a"),
            svc(ServiceState::Critical),
            StateType::Hard,
            1,
            200.0,
        ),
    );
    let mut old = service("h", "a", ServiceState::Ok);
    old.vars.insert("role".to_owned(), json!("db"));
    store.apply_fetched(Vec::new(), vec![old.clone()], Detail::Lean, &[], 11);
    let a = stored(&store, "h", "a");
    assert_eq!(a.state, ServiceState::Critical, "the event is newer");
    assert_eq!(a.check.output(), "output");
    assert_eq!(
        a.vars.get("role"),
        Some(&json!("db")),
        "config comes from the answer"
    );

    // An answer sent after the event wins.
    store.apply_fetched(Vec::new(), vec![old], Detail::Lean, &[], 12);
    assert_eq!(stored(&store, "h", "a").state, ServiceState::Ok);
    // And makes older lines stale.
    assert_eq!(
        store.apply(
            12,
            &check_result(
                key("h", "a"),
                svc(ServiceState::Critical),
                StateType::Hard,
                1,
                200.0
            )
        ),
        Applied::Stale
    );
}

#[test]
fn lean_answers_keep_results_and_links() {
    let mut store = loaded();
    let mut full = service("h", "b", ServiceState::Critical);
    full.check.result = Some(result(2, "CRITICAL - disk full", 90.0));
    full.links = Links {
        notes_url: "https://wiki/disk".to_owned(),
        ..Links::default()
    };
    store.apply_fetched(Vec::new(), vec![full], Detail::Full, &[], 11);
    assert!(store.is_full(&ServiceKey::new("h", "b")));

    let mut lean = service("h", "b", ServiceState::Critical);
    lean.check.last_check = Some(t(110.0));
    store.replace_services(
        vec![lean, service("h", "a", ServiceState::Ok)],
        Detail::Lean,
        12,
    );
    let b = stored(&store, "h", "b");
    assert_eq!(b.check.output(), "CRITICAL - disk full");
    assert_eq!(b.links.notes_url, "https://wiki/disk");
    assert_eq!(b.check.last_check, Some(t(110.0)));
}

#[test]
fn reloads_remove_what_is_gone_unless_an_event_saw_it() {
    let mut store = loaded();
    store.apply(
        20,
        &check_result(
            key("h", "b"),
            svc(ServiceState::Critical),
            StateType::Hard,
            1,
            200.0,
        ),
    );
    // A reload sent at 15 lacks both services: `a` is gone, `b` had an
    // event after the query went out, so it stays.
    store.replace_services(Vec::new(), Detail::Lean, 15);
    assert!(!store.contains(&key("h", "a")));
    assert!(store.contains(&key("h", "b")));
    let changes = store.take_changes();
    assert!(changes.all);
    assert!(changes.objects.contains(&key("h", "a")));

    // A host that is gone takes its services and their comments along.
    store.apply(
        21,
        &Event::CommentAdded {
            comment: comment("h!b!c1", key("h", "b"), 1.0),
            at: t(1.0),
        },
    );
    let removed = store.apply_fetched(
        Vec::new(),
        Vec::new(),
        Detail::Full,
        &[ObjectKey::host("h")],
        30,
    );
    assert_eq!(removed, [key("h", "b"), ObjectKey::host("h")]);
    assert!(store.services.is_empty());
    assert!(store.comments.is_empty());
}

fn comment(name: &str, object: ObjectKey, entry: f64) -> Comment {
    Comment {
        name: name.to_owned(),
        object,
        author: "me".to_owned(),
        text: "text".to_owned(),
        kind: CommentKind::User,
        entry_time: t(entry),
        expire_time: None,
        persistent: false,
    }
}

fn downtime(name: &str, object: ObjectKey, in_effect: bool) -> Downtime {
    Downtime {
        name: name.to_owned(),
        object,
        author: "me".to_owned(),
        comment: "maintenance".to_owned(),
        start_time: t(100.0),
        end_time: t(200.0),
        fixed: true,
        duration: 0.0,
        entry_time: t(90.0),
        trigger_time: None,
        triggered_by: None,
        parent: None,
        in_effect,
        config_owned: false,
    }
}

#[test]
fn comment_and_downtime_lists_keep_newer_events() {
    let mut store = loaded();
    let b = key("h", "b");
    store.apply_overview(
        Overview {
            comments: vec![
                comment("h!b!old", b.clone(), 1.0),
                comment("h!b!gone", b.clone(), 2.0),
            ],
            ..Overview::default()
        },
        10,
    );
    assert_eq!(store.comments.get(&b).map(Vec::len), Some(2));

    // A reload's comment query goes out at 20; meanwhile events at 21 and
    // 22 add one and remove one. The answer (without either change)
    // arrives after them.
    store.begin_annotation_query();
    store.apply(
        21,
        &Event::CommentAdded {
            comment: comment("h!b!new", b.clone(), 3.0),
            at: t(3.0),
        },
    );
    store.apply(
        22,
        &Event::CommentRemoved {
            comment: comment("h!b!gone", b.clone(), 2.0),
            at: t(4.0),
        },
    );
    store.apply_overview(
        Overview {
            comments: vec![
                comment("h!b!old", b.clone(), 1.0),
                comment("h!b!gone", b.clone(), 2.0),
            ],
            ..Overview::default()
        },
        20,
    );
    let names: Vec<&str> = store.comments[&b].iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["h!b!old", "h!b!new"]);

    // Lines read before the list's query are already in it.
    assert_eq!(
        store.apply(
            19,
            &Event::CommentRemoved {
                comment: comment("h!b!old", b.clone(), 1.0),
                at: t(4.0),
            },
        ),
        Applied::Stale
    );
}

#[test]
fn downtimes_keep_the_depth_in_step() {
    let mut store = loaded();
    let b = key("h", "b");
    let depth = |store: &Store| stored(store, "h", "b").check.downtime_depth;
    // Fixed downtime added inside its window: in effect at once.
    store.apply(
        11,
        &Event::DowntimeAdded {
            downtime: downtime("h!b!d1", b.clone(), true),
            at: t(1.0),
        },
    );
    assert_eq!(depth(&store), 1);
    // Started and triggered for the same downtime: still one.
    for (seq, event) in [
        (
            12,
            Event::DowntimeStarted {
                downtime: downtime("h!b!d1", b.clone(), true),
                at: t(1.0),
            },
        ),
        (
            13,
            Event::DowntimeTriggered {
                downtime: downtime("h!b!d1", b.clone(), true),
                at: t(1.0),
            },
        ),
    ] {
        store.apply(seq, &event);
    }
    assert_eq!(depth(&store), 1);
    // A flexible one waiting for a problem: not yet.
    store.apply(
        14,
        &Event::DowntimeAdded {
            downtime: downtime("h!b!d2", b.clone(), false),
            at: t(1.0),
        },
    );
    assert_eq!(depth(&store), 1);
    assert_eq!(store.downtimes[&b].len(), 2);
    store.apply(
        15,
        &Event::DowntimeRemoved {
            downtime: downtime("h!b!d1", b.clone(), true),
            at: t(1.0),
        },
    );
    assert_eq!(depth(&store), 0);
    assert_eq!(
        store.downtime("h!b!d2").map(|d| d.object.clone()),
        Some(b.clone())
    );
    assert!(store.downtime("h!b!d1").is_none());
}

#[test]
fn snapshots_share_and_stay_unchanged() {
    let mut store = loaded();
    let before = store.snapshot(1, t(1.0), Arc::default(), Arc::default());
    assert_eq!(before.overall.critical, 1);
    assert_eq!(before.overall.ok, 2, "one up host, one ok service");
    assert_eq!(before.overall.unhandled, 1);
    store.apply(
        11,
        &check_result(
            key("h", "b"),
            svc(ServiceState::Ok),
            StateType::Hard,
            1,
            200.0,
        ),
    );
    store.set_last_event_at(t(201.0));
    let after = store.snapshot(2, t(2.0), Arc::default(), Arc::default());
    let b = ServiceKey::new("h", "b");
    assert_eq!(
        before.services[&b].state,
        ServiceState::Critical,
        "copied on write"
    );
    assert_eq!(after.services[&b].state, ServiceState::Ok);
    assert!(
        Arc::ptr_eq(
            &before.services[&ServiceKey::new("h", "a")],
            &after.services[&ServiceKey::new("h", "a")]
        ),
        "unchanged objects are shared"
    );
    assert_eq!(after.overall.critical, 0);
    assert_eq!(after.last_event_at, Some(t(201.0)));
    assert_eq!(after.revision, 2);
}

#[test]
fn an_older_answer_applied_after_a_newer_one_changes_nothing() {
    // A reload's service list is sent at 20 and takes seconds; meanwhile
    // config changes are re-queried by name (sent at 30) and answered
    // first.
    let mut store = loaded();

    // Created: the by-name answer brings `c`; the older list lacks it.
    let created = service("h", "c", ServiceState::Ok);
    store.apply_fetched(Vec::new(), vec![created], Detail::Lean, &[], 30);
    // Modified: the by-name answer brings `a`'s new vars.
    let mut modified = service("h", "a", ServiceState::Ok);
    modified.vars.insert("role".to_owned(), json!("new"));
    store.apply_fetched(Vec::new(), vec![modified], Detail::Lean, &[], 30);
    // Deleted: the by-name answer finds `b` gone.
    let removed = store.apply_fetched(Vec::new(), Vec::new(), Detail::Lean, &[key("h", "b")], 30);
    assert_eq!(removed, [key("h", "b")]);
    store.take_discovered();

    // The older list: `a` with its old vars, `b` still there, no `c`.
    let mut old_a = service("h", "a", ServiceState::Ok);
    old_a.vars.insert("role".to_owned(), json!("old"));
    old_a.state = ServiceState::Critical;
    store.replace_services(
        vec![old_a, service("h", "b", ServiceState::Critical)],
        Detail::Lean,
        20,
    );
    assert!(store.contains(&key("h", "c")), "created since: stays");
    assert!(!store.contains(&key("h", "b")), "deleted since: stays gone");
    let a = stored(&store, "h", "a");
    assert_eq!(
        a.vars.get("role"),
        Some(&json!("new")),
        "newer config stays"
    );
    assert_eq!(a.state, ServiceState::Ok);
    assert!(
        store.take_discovered().is_empty(),
        "an older answer finds nothing"
    );

    // A newer list wins again, and may bring `b` back (created again).
    store.replace_services(
        vec![
            service("h", "a", ServiceState::Ok),
            service("h", "b", ServiceState::Warning),
        ],
        Detail::Lean,
        40,
    );
    assert!(store.contains(&key("h", "b")));
    assert!(!store.contains(&key("h", "c")), "gone as of 40");
    assert_eq!(stored(&store, "h", "a").vars.get("role"), None);
}

#[test]
fn a_deletion_event_keeps_older_answers_from_adding_the_object() {
    let mut store = loaded();
    // `d` is created and deleted while a list sent at 20 runs; the
    // deletion event is line 25. The list still has it.
    store.note_deleted(key("h", "d"), 25);
    store.apply_fetched(
        Vec::new(),
        vec![service("h", "d", ServiceState::Ok)],
        Detail::Lean,
        &[],
        20,
    );
    assert!(!store.contains(&key("h", "d")));
    // A known object announced deleted stays until an answer confirms it,
    // but older answers don't change it any more.
    store.note_deleted(key("h", "a"), 25);
    let mut old = service("h", "a", ServiceState::Critical);
    old.vars.insert("role".to_owned(), json!("old"));
    store.apply_fetched(Vec::new(), vec![old], Detail::Lean, &[], 20);
    assert_eq!(stored(&store, "h", "a").state, ServiceState::Ok);
    store.apply_fetched(Vec::new(), Vec::new(), Detail::Lean, &[key("h", "a")], 26);
    assert!(!store.contains(&key("h", "a")));
    // Created again: a newer answer brings it back.
    store.apply_fetched(
        Vec::new(),
        vec![service("h", "a", ServiceState::Ok)],
        Detail::Lean,
        &[],
        27,
    );
    assert!(store.contains(&key("h", "a")));
}

#[test]
fn removed_hosts_keep_their_services_out_too() {
    let mut store = loaded();
    // The host is found gone by name (sent at 30); an older list (20)
    // still has it and its services.
    store.apply_fetched(
        Vec::new(),
        Vec::new(),
        Detail::Full,
        &[ObjectKey::host("h")],
        30,
    );
    assert!(store.services.is_empty());
    store.replace_hosts(vec![host("h", HostState::Up)], 20);
    store.replace_services(
        vec![
            service("h", "a", ServiceState::Ok),
            service("h", "b", ServiceState::Ok),
        ],
        Detail::Lean,
        20,
    );
    assert!(store.hosts.is_empty());
    assert!(store.services.is_empty());
}

#[test]
fn tombstones_are_bounded() {
    let mut store = Store::default();
    for index in 0..super::MAX_TOMBSTONES as u64 + 10 {
        store.note_deleted(ObjectKey::host(&format!("h{index}")), index + 1);
    }
    assert!(store.removed.len() <= super::MAX_TOMBSTONES);
    assert!(
        store
            .removed
            .contains_key(&ObjectKey::host(&format!("h{}", super::MAX_TOMBSTONES))),
        "the newest stay"
    );
}

#[test]
fn query_answers_record_what_no_event_explained() {
    let mut store = loaded();
    assert!(
        store.take_discovered().is_empty(),
        "the first load finds nothing"
    );

    // An event moves `a`; a later reload shows `b` recovered and flapping
    // and `a` (as the event left it) unchanged: only `b` was missed.
    store.apply(
        20,
        &check_result(
            key("h", "a"),
            svc(ServiceState::Warning),
            StateType::Soft,
            1,
            200.0,
        ),
    );
    let mut b = service("h", "b", ServiceState::Ok);
    b.check.flapping = true;
    b.check.last_state_change = t(150.0);
    let mut a = service("h", "a", ServiceState::Ok);
    a.check.last_check = Some(t(90.0));
    store.replace_services(vec![a, b], Detail::Lean, 15);
    let found = store.take_discovered();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].object, key("h", "b"));
    assert_eq!(found[0].before.state, svc(ServiceState::Critical));
    let after = found[0].after.unwrap();
    assert_eq!(after.state, svc(ServiceState::Ok));
    assert!(after.flapping);
    assert_eq!(after.since, t(150.0));

    // The same answer again: nothing new.
    let mut b = service("h", "b", ServiceState::Ok);
    b.check.flapping = true;
    b.check.last_state_change = t(150.0);
    store.apply_fetched(Vec::new(), vec![b], Detail::Lean, &[], 30);
    assert!(store.take_discovered().is_empty());

    // Removals: the host and its services.
    store.apply_fetched(
        Vec::new(),
        Vec::new(),
        Detail::Full,
        &[ObjectKey::host("h")],
        40,
    );
    let found = store.take_discovered();
    let gone: Vec<ObjectKey> = found.iter().map(|change| change.object.clone()).collect();
    assert_eq!(gone, [key("h", "a"), key("h", "b"), ObjectKey::host("h")]);
    assert!(found.iter().all(|change| change.after.is_none()));
}

#[test]
fn a_result_older_than_the_last_check_is_stale() {
    let mut store = loaded();
    let b = ServiceKey::new("h", "b");
    assert!(!store.result_is_stale(&b), "no result at all");
    let mut full = service("h", "b", ServiceState::Critical);
    full.check.result = Some(result(2, "CRITICAL - disk full", 100.0));
    store.apply_fetched(Vec::new(), vec![full], Detail::Full, &[], 11);
    assert!(!store.result_is_stale(&b));
    // A lean answer from a later check keeps the result, which is now old.
    let mut lean = service("h", "b", ServiceState::Ok);
    lean.check.last_check = Some(t(400.0));
    store.apply_fetched(Vec::new(), vec![lean], Detail::Lean, &[], 12);
    assert_eq!(
        stored(&store, "h", "b").check.output(),
        "CRITICAL - disk full"
    );
    assert!(store.result_is_stale(&b));
}

#[test]
fn group_list_changes_are_flagged() {
    let mut store = loaded();
    store.set_host_groups(vec![ic_model::HostGroup {
        name: "db".to_owned(),
        display_name: "Databases".to_owned(),
    }]);
    let changes = store.take_changes();
    assert!(changes.groups && changes.any);
    store.set_host_groups(vec![ic_model::HostGroup {
        name: "db".to_owned(),
        display_name: "Databases".to_owned(),
    }]);
    assert!(!store.take_changes().groups, "unchanged");
    store.set_service_groups(Vec::new());
    assert!(!store.has_changes());
    assert_eq!(store.object_count(), 3);
    assert_eq!(store.latest_check(), Some(t(100.0)));
}

fn notification(name: &str, last: f64, users: &[&str]) -> ic_model::Notification {
    ic_model::Notification {
        name: name.to_owned(),
        object: super::notification_object(name).unwrap(),
        last_notification: t(last).non_zero(),
        notified_problem_users: users.iter().map(|user| (*user).to_owned()).collect(),
    }
}

#[test]
fn notification_names_belong_to_their_object() {
    use super::notification_object;
    assert_eq!(notification_object("h!a!mail"), Some(key("h", "a")));
    assert_eq!(notification_object("h!mail"), Some(ObjectKey::host("h")));
    assert_eq!(notification_object("mail"), None);
    assert_eq!(notification_object("h!"), None);
    assert_eq!(notification_object("!mail"), None);
}

#[test]
fn notification_lists_and_by_name_answers_converge() {
    let mut store = loaded();
    // The list, queried at 20.
    store.replace_notifications(
        vec![
            notification("h!b!mail", 50.0, &["oncall"]),
            notification("h!b!sms", 0.0, &[]),
            notification("h!mail", 0.0, &[]),
        ],
        20,
    );
    assert!(store.take_changes().any);
    assert_eq!(store.notifications_listed(), 20);
    assert_eq!(
        store.notification_names(&key("h", "b")),
        ["h!b!mail", "h!b!sms"]
    );
    let snapshot = store.snapshot(1, t(0.0), Arc::default(), Arc::default());
    let notified = snapshot.notified(&key("h", "b"));
    assert_eq!(notified.last_notification, Some(t(50.0)));
    assert_eq!(notified.users, ["oncall"]);
    assert!(snapshot.notified(&ObjectKey::host("h")).is_never());
    assert!(
        snapshot.notified(&key("h", "a")).is_never(),
        "no notifications"
    );

    // A by-name answer (sent at 30) after a new notification.
    store.apply_fetched_notifications(
        vec![notification("h!b!mail", 60.0, &["oncall", "m.keller"])],
        &["h!b!sms".to_owned()],
        30,
    );
    assert!(store.take_changes().any);
    assert_eq!(store.notification_names(&key("h", "b")), ["h!b!mail"]);
    // An older list (sent at 25, answered late) changes nothing it covers
    // newer, and isn't applied at all: the list at 20 is older still, but
    // a list older than the stored one is ignored too.
    store.replace_notifications(vec![notification("h!b!sms", 0.0, &[])], 25);
    let list = &store.icinga_notifications()[&key("h", "b")];
    assert_eq!(list.len(), 1, "the by-name answer at 30 is newer");
    assert_eq!(list[0].notified_problem_users, ["oncall", "m.keller"]);
    assert!(
        !store
            .icinga_notifications()
            .contains_key(&ObjectKey::host("h")),
        "gone from the list at 25"
    );
    store.replace_notifications(Vec::new(), 10);
    assert_eq!(store.notifications_listed(), 25, "an older list is ignored");

    // A by-name answer older than the list in the store is ignored.
    store.apply_fetched_notifications(vec![notification("h!mail", 5.0, &["x"])], &[], 24);
    assert!(
        !store
            .icinga_notifications()
            .contains_key(&ObjectKey::host("h"))
    );

    // A newer list replaces everything; the recovery cleared the users.
    store.replace_notifications(vec![notification("h!b!mail", 70.0, &[])], 40);
    let snapshot = store.snapshot(2, t(0.0), Arc::default(), Arc::default());
    let notified = snapshot.notified(&key("h", "b"));
    assert_eq!(notified.last_notification, Some(t(70.0)));
    assert!(notified.users.is_empty());
    assert!(!notified.is_never());

    // Unchanged answers change nothing.
    store.take_changes();
    store.replace_notifications(vec![notification("h!b!mail", 70.0, &[])], 41);
    store.apply_fetched_notifications(vec![notification("h!b!mail", 70.0, &[])], &[], 42);
    assert!(!store.take_changes().any);

    // An object's notifications go with it.
    store.replace_services(vec![service("h", "a", ServiceState::Ok)], Detail::Lean, 50);
    assert!(store.icinga_notifications().is_empty());
}

#[test]
fn a_partial_view_hides_what_it_leaves_out_until_a_full_one_brings_it_back() {
    let mut store = loaded();
    store.take_discovered();
    // A satellite's answers leave `h!b` out: hidden, not gone.
    store.set_hiding(true);
    store.replace_services(vec![service("h", "a", ServiceState::Ok)], Detail::Lean, 20);
    assert!(!store.contains(&key("h", "b")));
    assert!(store.take_discovered().is_empty(), "not reported gone");
    assert_eq!(store.hidden_count(), 1);

    // A master brings it back, recovered meanwhile: that is reported.
    store.set_hiding(false);
    store.track_appeared(true);
    store.replace_services(
        vec![
            service("h", "a", ServiceState::Ok),
            service("h", "b", ServiceState::Ok),
            service("h", "c", ServiceState::Critical),
        ],
        Detail::Lean,
        30,
    );
    let found = store.take_discovered();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].object, key("h", "b"));
    assert_eq!(
        found[0].before.state,
        CheckableState::Service(ServiceState::Critical)
    );
    assert_eq!(
        found[0].after.map(|after| after.state),
        Some(CheckableState::Service(ServiceState::Ok))
    );
    assert_eq!(store.hidden_count(), 0);
    // `h!c` was never seen: listed for the rule engine to learn.
    assert_eq!(store.take_appeared(), [key("h", "c")]);
    store.track_appeared(false);

    // Hidden again, and a complete load from a master doesn't bring it:
    // gone.
    store.set_hiding(true);
    store.replace_services(vec![service("h", "a", ServiceState::Ok)], Detail::Lean, 40);
    assert_eq!(store.hidden_count(), 2);
    store.set_hiding(false);
    store.release_hidden();
    let gone: Vec<(ObjectKey, bool)> = store
        .take_discovered()
        .into_iter()
        .map(|found| (found.object, found.after.is_none()))
        .collect();
    assert_eq!(gone, [(key("h", "b"), true), (key("h", "c"), true)]);
    assert_eq!(store.hidden_count(), 0);
}
