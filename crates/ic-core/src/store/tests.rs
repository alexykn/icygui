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
            // After the loaded check (an older one would be overtaken).
            at: t(101.0),
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

/// A query answer can overtake events still on their way (a slow stream):
/// Icinga composed it after it processed them, so it shows a later check.
/// Those events change nothing (no hard problem soft again, no change
/// twice); the first that isn't older applies, and from then on events
/// wrote the object last and apply in their order.
#[test]
fn events_an_answer_overtook_change_nothing_until_the_stream_catches_up() {
    let mut store = loaded();
    // Sent at 11, answered with the hard problem of the check at 300.
    let mut answer = service("h", "a", ServiceState::Critical);
    answer.check.state_type = StateType::Hard;
    answer.check.last_state_change = t(200.0);
    answer.check.last_hard_state_change = t(300.0);
    answer.check.last_check = Some(t(300.0));
    store.apply_fetched(Vec::new(), vec![answer], Detail::Lean, &[], 11);
    assert_eq!(store.take_discovered().len(), 1);
    let hard = stored(&store, "h", "a");
    // The soft attempts at 200 and 250 come after it: older.
    for (seq, end) in [(12, 200.0), (13, 250.0)] {
        for event in [
            check_result(
                key("h", "a"),
                svc(ServiceState::Critical),
                StateType::Soft,
                1,
                end,
            ),
            Event::StateChange {
                object: key("h", "a"),
                state: svc(ServiceState::Critical),
                state_type: StateType::Soft,
                result: result(2, "soft", end),
                downtime_depth: Some(0),
                acknowledgement: None,
                at: t(end + 0.1),
            },
            Event::Flapping {
                object: key("h", "a"),
                flapping: true,
                current: 40.0,
                at: t(end + 0.1),
            },
        ] {
            assert_eq!(store.apply(seq, &event), Applied::Stale, "{event:?}");
        }
        assert_eq!(stored(&store, "h", "a"), hard);
    }
    // The check the answer showed: applied (its output), the same state.
    let Applied::Changed { before, after } = store.apply(
        14,
        &check_result(
            key("h", "a"),
            svc(ServiceState::Critical),
            StateType::Hard,
            1,
            300.0,
        ),
    ) else {
        panic!("the answer's own check applies");
    };
    assert_eq!(before, after);
    // Events wrote it last now: they apply in Icinga's order.
    assert!(matches!(
        store.apply(
            15,
            &check_result(
                key("h", "a"),
                svc(ServiceState::Ok),
                StateType::Hard,
                1,
                290.0
            )
        ),
        Applied::Changed { .. }
    ));
    assert_eq!(stored(&store, "h", "a").state, ServiceState::Ok);
    // Lines read before an answer's request went out stay stale as ever;
    // a later answer starts over.
    let mut answer = service("h", "a", ServiceState::Warning);
    answer.check.last_check = Some(t(500.0));
    store.apply_fetched(Vec::new(), vec![answer], Detail::Lean, &[], 16);
    assert_eq!(
        store.apply(
            17,
            &check_result(
                key("h", "a"),
                svc(ServiceState::Ok),
                StateType::Hard,
                1,
                450.0
            )
        ),
        Applied::Stale
    );
    assert!(matches!(
        store.apply(
            18,
            &check_result(
                key("h", "a"),
                svc(ServiceState::Ok),
                StateType::Hard,
                1,
                510.0
            )
        ),
        Applied::Changed { .. }
    ));
    assert_eq!(stored(&store, "h", "a").state, ServiceState::Ok);
}

/// Icinga sends the `AcknowledgementCleared` of a state change just
/// before the change's check result, and a downtime can take effect
/// between two attempts. Neither writes the check state, so neither shows
/// that events wrote the object last: the older attempts an answer
/// overtook stay stale behind them (before, the cleared acknowledgement
/// let them through: the hard problem went soft and hard again, two state
/// changes logged twice).
#[test]
fn an_acknowledgement_or_downtime_between_overtaken_events_lets_none_through() {
    for between in ["acknowledgement cleared", "downtime started"] {
        let mut store = loaded();
        // Sent at 11, answered with the hard problem of the check at 300.
        let mut answer = service("h", "a", ServiceState::Critical);
        answer.check.state_type = StateType::Hard;
        answer.check.last_state_change = t(200.0);
        answer.check.last_hard_state_change = t(300.0);
        answer.check.last_check = Some(t(300.0));
        store.apply_fetched(Vec::new(), vec![answer], Detail::Lean, &[], 11);
        store.take_discovered();
        let hard = stored(&store, "h", "a");
        let event = if between == "downtime started" {
            Event::DowntimeStarted {
                downtime: downtime("h!a!d", key("h", "a"), true),
                at: t(150.0),
            }
        } else {
            Event::AcknowledgementCleared {
                object: key("h", "a"),
                at: t(199.9),
            }
        };
        assert!(
            matches!(store.apply(12, &event), Applied::Changed { .. }),
            "{between}"
        );
        // The attempts at 200 (the state change) and 250: older.
        for (seq, end) in [(13, 200.0), (14, 250.0)] {
            for event in [
                check_result(
                    key("h", "a"),
                    svc(ServiceState::Critical),
                    StateType::Soft,
                    1,
                    end,
                ),
                Event::StateChange {
                    object: key("h", "a"),
                    state: svc(ServiceState::Critical),
                    state_type: StateType::Soft,
                    result: result(2, "soft", end),
                    downtime_depth: Some(0),
                    acknowledgement: None,
                    at: t(end + 0.1),
                },
            ] {
                assert_eq!(
                    store.apply(seq, &event),
                    Applied::Stale,
                    "{between}: {event:?}"
                );
            }
            let a = stored(&store, "h", "a");
            assert_eq!(
                (a.state, a.check.state_type, a.check.last_check),
                (hard.state, hard.check.state_type, hard.check.last_check),
                "{between}"
            );
        }
        // The check the answer showed applies; from then on events apply
        // in Icinga's order.
        assert!(matches!(
            store.apply(
                15,
                &check_result(
                    key("h", "a"),
                    svc(ServiceState::Critical),
                    StateType::Hard,
                    1,
                    300.0,
                ),
            ),
            Applied::Changed { .. }
        ));
        assert!(matches!(
            store.apply(
                16,
                &check_result(
                    key("h", "a"),
                    svc(ServiceState::Ok),
                    StateType::Hard,
                    1,
                    290.0
                )
            ),
            Applied::Changed { .. }
        ));
        assert_eq!(
            stored(&store, "h", "a").state,
            ServiceState::Ok,
            "{between}"
        );
    }
}

/// An attempt an answer overtook is skipped by its own time, not by its
/// line number: a line with a lower number that comes later (the new
/// stream's, applied after the old one's at a handover) and is newer than
/// the answer still applies.
#[test]
fn skipping_an_overtaken_line_skips_no_newer_one_with_a_lower_number() {
    let mut store = loaded();
    let mut answer = service("h", "a", ServiceState::Critical);
    answer.check.state_type = StateType::Hard;
    answer.check.last_check = Some(t(300.0));
    store.apply_fetched(Vec::new(), vec![answer], Detail::Lean, &[], 11);
    store.take_discovered();
    // The old stream's line 20: older than the answer.
    assert_eq!(
        store.apply(
            20,
            &check_result(
                key("h", "a"),
                svc(ServiceState::Critical),
                StateType::Soft,
                2,
                250.0
            )
        ),
        Applied::Stale
    );
    // The new stream's line 15 (read earlier, applied after): the
    // recovery at 310, after the answer.
    assert!(matches!(
        store.apply(
            15,
            &check_result(
                key("h", "a"),
                svc(ServiceState::Ok),
                StateType::Hard,
                1,
                310.0
            )
        ),
        Applied::Changed { .. }
    ));
    assert_eq!(stored(&store, "h", "a").state, ServiceState::Ok);
    // An answer sent at 16, after the first one, is not superseded.
    let mut answer = service("h", "a", ServiceState::Warning);
    answer.check.last_check = Some(t(320.0));
    store.apply_fetched(Vec::new(), vec![answer], Detail::Lean, &[], 16);
    assert_eq!(stored(&store, "h", "a").state, ServiceState::Warning);
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
        schedule: None,
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

/// A fixed downtime on `object` from `start` to `end`.
fn fixed(name: &str, object: ObjectKey, start: f64, end: f64, in_effect: bool) -> Downtime {
    Downtime {
        start_time: t(start),
        end_time: t(end),
        entry_time: t(start.min(50.0)),
        ..downtime(name, object, in_effect)
    }
}

/// A check event (OK, hard) of `object` whose check ended at `end` and
/// that carries Icinga's downtime count `depth` (the event's time is
/// `end + 0.1`).
fn counted(object: ObjectKey, depth: u32, end: f64) -> Event {
    let state = if object == key("h", "b") {
        svc(ServiceState::Critical)
    } else {
        svc(ServiceState::Ok)
    };
    match check_result(object, state, StateType::Hard, 1, end) {
        Event::CheckResult {
            object,
            result,
            acknowledgement,
            after,
            at,
            ..
        } => Event::CheckResult {
            object,
            result,
            downtime_depth: Some(depth),
            acknowledgement,
            after,
            at,
        },
        other => other,
    }
}

/// `h!b` as Icinga answers a query: critical, `depth` downtimes in effect.
fn answered_b(depth: u32) -> Service {
    let mut answer = service("h", "b", ServiceState::Critical);
    answer.check.downtime_depth = depth;
    answer
}

fn depth_of(store: &Store, name: &str) -> u32 {
    stored(store, "h", name).check.downtime_depth
}

/// Ledger row (small fix 2 fixer, false green): an answer to a query sent
/// before a downtime's `DowntimeAdded` line was read can already count
/// the downtime. The store can't tell from the line, so the depth changes
/// for the moment and the object is asked for again by name; the answer to
/// that (sent after the line) is Icinga's count. Before, the line added 1
/// to the answer's count for good: 2 for one downtime, still 1 after its
/// removal, and in quiet mode (no check result brings Icinga's count) the
/// problem stayed hollow, handled and hidden while Icinga said it wasn't
/// in downtime (the engine test
/// `a_downtime_an_answer_already_counts_is_counted_once` fails without the
/// fix).
#[test]
fn a_downtime_an_answer_may_already_count_is_asked_for_again() {
    let mut store = loaded();
    let b = key("h", "b");
    // The answer to a query sent at 11 already counts the downtime.
    store.apply_fetched(Vec::new(), vec![answered_b(1)], Detail::Lean, &[], 11);
    assert!(store.take_unsettled().is_empty());
    store.apply(
        12,
        &Event::DowntimeAdded {
            downtime: fixed("h!b!d1", b.clone(), 100.0, 200.0, true),
            at: t(150.0),
        },
    );
    assert_eq!(
        store.take_unsettled(),
        std::slice::from_ref(&b),
        "Icinga is asked again"
    );
    // Its answer (sent after line 12 was read) is Icinga's count.
    store.apply_fetched(Vec::new(), vec![answered_b(1)], Detail::Lean, &[], 12);
    assert_eq!(depth_of(&store, "b"), 1);
    // Removed while no check result comes (quiet mode): none left.
    store.apply(
        13,
        &Event::DowntimeRemoved {
            downtime: fixed("h!b!d1", b.clone(), 100.0, 200.0, true),
            at: t(160.0),
        },
    );
    assert_eq!(depth_of(&store, "b"), 0, "Icinga's count");
    assert_eq!(
        store.take_unsettled(),
        std::slice::from_ref(&b),
        "and asked again"
    );
    store.apply_fetched(Vec::new(), vec![answered_b(0)], Detail::Lean, &[], 13);
    assert_eq!(depth_of(&store, "b"), 0);
    assert!(store.take_unsettled().is_empty());
}

/// Icinga counts a fixed downtime from its start (`Downtime::IsInEffect`
/// asks the clock), but its start timer reports it (`DowntimeStarted`)
/// only up to 5 s later; a check result in between already carries the
/// count with it. The started line then changes nothing; before, it added
/// 1 again: 2 for one downtime, 1 after its removal, the object in
/// downtime for good in quiet mode.
#[test]
fn a_fixed_downtime_counts_from_its_start_before_icinga_reports_it() {
    let mut store = loaded();
    let a = key("h", "a");
    store.apply(
        11,
        &Event::DowntimeAdded {
            downtime: fixed("h!a!d1", a.clone(), 100.0, 200.0, false),
            at: t(50.0),
        },
    );
    store.take_unsettled();
    assert_eq!(depth_of(&store, "a"), 0, "not yet");
    // A result at 102: Icinga's count has the downtime.
    store.apply(12, &counted(a.clone(), 1, 101.9));
    assert_eq!(depth_of(&store, "a"), 1);
    for (seq, event) in [
        (
            13,
            Event::DowntimeStarted {
                downtime: fixed("h!a!d1", a.clone(), 100.0, 200.0, true),
                at: t(104.0),
            },
        ),
        (
            14,
            Event::DowntimeTriggered {
                downtime: fixed("h!a!d1", a.clone(), 100.0, 200.0, true),
                at: t(104.0),
            },
        ),
    ] {
        store.apply(seq, &event);
        assert_eq!(depth_of(&store, "a"), 1, "line {seq}: counted once");
    }
    assert!(
        store.take_unsettled().is_empty(),
        "counted from a check event: exact, nothing to ask"
    );
    store.apply(
        15,
        &Event::DowntimeRemoved {
            downtime: fixed("h!a!d1", a.clone(), 100.0, 200.0, true),
            at: t(150.0),
        },
    );
    assert_eq!(depth_of(&store, "a"), 0, "cancelled: none left");
}

/// The other end: Icinga stops counting a fixed downtime at its end and
/// removes it a moment later (its cleanup timer). A check result in
/// between already left it out, so its removal takes nothing off; before,
/// it took off the other downtime still in effect, and the problem showed
/// as not in downtime while Icinga said it was.
#[test]
fn a_downtime_that_ran_out_before_a_check_result_is_not_taken_off_twice() {
    let mut store = loaded();
    let b = key("h", "b");
    store.apply_overview(
        Overview {
            downtimes: vec![
                fixed("h!b!d1", b.clone(), 100.0, 200.0, true),
                fixed("h!b!d2", b.clone(), 100.0, 400.0, true),
            ],
            ..Overview::default()
        },
        10,
    );
    store.apply(11, &counted(b.clone(), 2, 150.0));
    // d1 ran out at 200; a result at 200.05 counts only d2.
    store.apply(12, &counted(b.clone(), 1, 199.95));
    store.apply(
        13,
        &Event::DowntimeRemoved {
            downtime: fixed("h!b!d1", b.clone(), 100.0, 200.0, true),
            at: t(200.1),
        },
    );
    assert_eq!(depth_of(&store, "b"), 1, "d2 is still in effect");
    assert!(store.take_unsettled().is_empty());
}

/// Downtimes the API user may not read (a filtered `objects/query/Downtime`
/// permission) are in Icinga's count but not in the store's list; their
/// events still come (`events/*` can't be filtered). Counted from Icinga's
/// numbers, judged per downtime by name, they come out right too.
#[test]
fn downtimes_the_user_cannot_read_are_counted_by_icingas_numbers() {
    let mut store = loaded();
    let b = key("h", "b");
    let hidden = fixed("h!b!hidden", b.clone(), 100.0, 400.0, true);
    // From a check event: one downtime the list doesn't have.
    store.apply(11, &counted(b.clone(), 1, 150.0));
    store.apply(
        12,
        &Event::DowntimeRemoved {
            downtime: hidden.clone(),
            at: t(160.0),
        },
    );
    assert_eq!(depth_of(&store, "b"), 0);
    assert!(store.take_unsettled().is_empty());

    // From an answer that also counts a downtime whose line is on its way.
    let mut store = loaded();
    store.apply_fetched(Vec::new(), vec![answered_b(2)], Detail::Lean, &[], 11);
    store.apply(
        12,
        &Event::DowntimeAdded {
            downtime: fixed("h!b!d1", b.clone(), 100.0, 200.0, true),
            at: t(150.0),
        },
    );
    assert_eq!(store.take_unsettled(), std::slice::from_ref(&b));
    store.apply_fetched(Vec::new(), vec![answered_b(2)], Detail::Lean, &[], 12);
    store.apply(
        13,
        &Event::DowntimeRemoved {
            downtime: hidden,
            at: t(160.0),
        },
    );
    assert_eq!(depth_of(&store, "b"), 1, "d1 is left");
    assert_eq!(store.take_unsettled(), std::slice::from_ref(&b));
    store.apply_fetched(Vec::new(), vec![answered_b(1)], Detail::Lean, &[], 13);
    assert_eq!(depth_of(&store, "b"), 1);
}

/// The answer that settles a depth takes it although another event about
/// the object (an acknowledgement) came after the request went out: that
/// event wrote the acknowledgement, not the depth. Before, any newer event
/// kept every event field, so the depth counted twice stayed.
#[test]
fn an_answer_takes_the_fields_no_newer_event_wrote() {
    let mut store = loaded();
    let b = key("h", "b");
    store.apply_fetched(Vec::new(), vec![answered_b(1)], Detail::Lean, &[], 11);
    store.apply(
        12,
        &Event::DowntimeAdded {
            downtime: fixed("h!b!d1", b.clone(), 100.0, 200.0, true),
            at: t(150.0),
        },
    );
    store.take_unsettled();
    // Asked again at 12; line 13 acknowledges the problem before the
    // answer (without the acknowledgement) is in.
    store.apply(
        13,
        &Event::AcknowledgementSet {
            object: b.clone(),
            author: "carol".to_owned(),
            comment: "on it".to_owned(),
            kind: AckKind::Normal,
            expiry: None,
            at: t(151.0),
        },
    );
    store.apply_fetched(Vec::new(), vec![answered_b(1)], Detail::Lean, &[], 12);
    let b_now = stored(&store, "h", "b");
    assert_eq!(b_now.check.downtime_depth, 1, "the answer's count");
    assert!(
        b_now.check.acknowledgement.is_acknowledged(),
        "the newer event's acknowledgement"
    );
}

/// In the overlap of a stream switch the new stream's check results can
/// be older than a downtime line the old stream already brought; such a
/// result doesn't take the count back to before the downtime.
#[test]
fn a_check_result_older_than_a_downtime_line_keeps_the_count() {
    let mut store = loaded();
    let a = key("h", "a");
    store.apply(11, &counted(a.clone(), 0, 150.0));
    store.apply(
        12,
        &Event::DowntimeAdded {
            downtime: fixed("h!a!d1", a.clone(), 100.0, 400.0, true),
            at: t(300.0),
        },
    );
    assert_eq!(depth_of(&store, "a"), 1);
    store.apply(13, &counted(a.clone(), 0, 250.0));
    assert_eq!(depth_of(&store, "a"), 1, "the result is older");
    store.apply(14, &counted(a.clone(), 1, 310.0));
    assert_eq!(depth_of(&store, "a"), 1);

    // Review fixer (small fix 3): the same for a downtime line an answer
    // already had. The removal (line 12, at 160) is read, an answer sent
    // after it (13, none in effect) is applied first; the new stream's
    // result from 150 (read later, 14) still counted the downtime. Before,
    // only lines the store applied dated the count, so the result brought
    // 1 back: the problem in downtime while Icinga had none.
    let mut store = loaded();
    let b = key("h", "b");
    let downtime = fixed("h!b!d1", b.clone(), 100.0, 400.0, true);
    store.apply_overview(
        Overview {
            downtimes: vec![downtime.clone()],
            ..Overview::default()
        },
        10,
    );
    store.apply_fetched(Vec::new(), vec![answered_b(1)], Detail::Lean, &[], 11);
    store.apply_fetched(Vec::new(), vec![answered_b(0)], Detail::Lean, &[], 13);
    store.apply(
        12,
        &Event::DowntimeRemoved {
            downtime,
            at: t(160.0),
        },
    );
    assert_eq!(depth_of(&store, "b"), 0, "the answer's");
    store.apply(14, &counted(b.clone(), 1, 150.0));
    assert_eq!(
        depth_of(&store, "b"),
        0,
        "the result is older than the removal"
    );
}

/// `event` (a check event) as Icinga sends it: with the acknowledgement as
/// it was when Icinga built the event.
fn with_ack(event: Event, kind: AckKind) -> Event {
    match event {
        Event::CheckResult {
            object,
            result,
            downtime_depth,
            after,
            at,
            ..
        } => Event::CheckResult {
            object,
            result,
            downtime_depth,
            acknowledgement: Some(kind),
            after,
            at,
        },
        other => other,
    }
}

fn acknowledged(store: &Store, name: &str) -> bool {
    stored(store, "h", name)
        .check
        .acknowledgement
        .is_acknowledged()
}

/// Review finding (small fix 3 fixer, false green): Icinga's check events
/// carry the acknowledgement as it was when the event was built. In the
/// overlap of a stream switch the new stream's check result can be older
/// than an acknowledgement line the old stream already brought (a quiet
/// stream has no check results, so a result from the overlap waits on the
/// new one); applied after it, it took the acknowledgement back: removed
/// in Icinga, still acknowledged here (handled, hollow, hidden by *hide
/// handled*) until the next check result, and acknowledged in Icinga but
/// not here in the mirror order. Such a result keeps the acknowledgement,
/// like an older one keeps the downtime count.
#[test]
fn a_check_event_older_than_an_acknowledgement_line_keeps_the_acknowledgement() {
    let b = key("h", "b");
    let cleared = Event::AcknowledgementCleared {
        object: b.clone(),
        at: t(160.0),
    };
    let set = Event::AcknowledgementSet {
        object: b.clone(),
        author: "carol".to_owned(),
        comment: "on it".to_owned(),
        kind: AckKind::Normal,
        expiry: None,
        at: t(160.0),
    };

    // Acknowledged, then removed (the old stream's line 13); the new
    // stream's result (line 12, from 150) still had it.
    let mut store = loaded();
    let mut answer = answered_b(0);
    answer.check.acknowledgement = AckKind::Normal;
    store.apply_fetched(Vec::new(), vec![answer], Detail::Lean, &[], 11);
    store.apply(13, &cleared);
    store.apply(12, &with_ack(counted(b.clone(), 0, 150.0), AckKind::Normal));
    assert!(
        !acknowledged(&store, "b"),
        "removed at 160, after the result"
    );
    assert_eq!(stored(&store, "h", "b").check.last_check, Some(t(150.0)));
    // A newer check event brings Icinga's value again.
    store.apply(14, &with_ack(counted(b.clone(), 0, 170.0), AckKind::Normal));
    assert!(acknowledged(&store, "b"), "acknowledged again at 170");

    // The mirror: acknowledged at 160 (line 13); the result from 150 had
    // none yet.
    let mut store = loaded();
    store.apply(13, &set);
    store.apply(12, &with_ack(counted(b.clone(), 0, 150.0), AckKind::None));
    assert!(acknowledged(&store, "b"), "acknowledged at 160");
    // A state change from before the acknowledgement that carries none
    // (an event without the field) doesn't end it either.
    store.apply(
        14,
        &check_result(
            b.clone(),
            svc(ServiceState::Warning),
            StateType::Hard,
            1,
            155.0,
        ),
    );
    assert_eq!(stored(&store, "h", "b").state, ServiceState::Warning);
    assert!(acknowledged(&store, "b"), "the change came before it");
    // A newer state change does, as in Icinga.
    store.apply(
        15,
        &check_result(
            b.clone(),
            svc(ServiceState::Critical),
            StateType::Hard,
            1,
            170.0,
        ),
    );
    assert!(
        !acknowledged(&store, "b"),
        "a normal one ends with a change"
    );

    // The removal's line (12) is read, and an answer sent after it (13,
    // without the acknowledgement) is applied first: the line is stale,
    // but its time still dates the value; the new stream's result from
    // 150, read later (14), is older.
    let mut store = loaded();
    let mut answer = answered_b(0);
    answer.check.acknowledgement = AckKind::Normal;
    store.apply_fetched(Vec::new(), vec![answer], Detail::Lean, &[], 11);
    store.apply_fetched(Vec::new(), vec![answered_b(0)], Detail::Lean, &[], 13);
    assert_eq!(store.apply(12, &cleared), Applied::Stale);
    store.apply(14, &with_ack(counted(b.clone(), 0, 150.0), AckKind::Normal));
    assert!(!acknowledged(&store, "b"), "the answer's, newer than 150");
}

/// Whether `key`'s downtime depth is provisional (from an answer, a
/// downtime event since).
fn provisional(store: &Store, key: &ObjectKey) -> bool {
    store.unsettled.contains(key)
}

/// Where the count comes from: an answer becomes its source (the next
/// downtime event makes it provisional) until a check event brings
/// Icinga's count, whose downtime events are then exact; after a gap in
/// the stream (a new session, a switch without a line on both streams) a
/// count from a check event is no longer exact.
#[test]
fn the_count_keeps_its_exact_source_until_the_stream_has_a_gap() {
    let mut store = loaded();
    let a = key("h", "a");
    // Added at 300 (Icinga sends the event as it creates the downtime).
    let added = |name: &str| Event::DowntimeAdded {
        downtime: Downtime {
            entry_time: t(300.0),
            ..fixed(name, key("h", "a"), 100.0, 400.0, true)
        },
        at: t(300.0),
    };
    store.apply(11, &counted(a.clone(), 0, 150.0));
    store.apply(12, &added("h!a!d1"));
    assert_eq!(depth_of(&store, "a"), 1);
    assert!(!provisional(&store, &a), "from a check event: exact");

    let mut answer = service("h", "a", ServiceState::Ok);
    answer.check.last_check = Some(t(150.0));
    answer.check.downtime_depth = 1;
    store.apply_fetched(Vec::new(), vec![answer], Detail::Lean, &[], 13);
    store.apply(14, &added("h!a!d2"));
    assert_eq!(depth_of(&store, "a"), 2);
    assert!(
        provisional(&store, &a),
        "from an answer (even an agreeing one): provisional"
    );
    store.apply(15, &counted(a.clone(), 2, 310.0));
    store.apply(
        16,
        &Event::DowntimeRemoved {
            downtime: fixed("h!a!d2", a.clone(), 100.0, 400.0, true),
            at: t(320.0),
        },
    );
    assert_eq!(depth_of(&store, "a"), 1);
    assert!(!provisional(&store, &a), "a check event again: exact");

    let mut store = loaded();
    store.apply(11, &counted(a.clone(), 0, 150.0));
    store.stream_gap();
    store.apply(12, &added("h!a!d1"));
    assert_eq!(depth_of(&store, "a"), 1);
    assert!(provisional(&store, &a), "after a gap: provisional");
}

/// Review finding (small fix 3 fixer, request economy): every answer is a
/// count's source (a reconcile writes every object), so a schedule
/// starting or ending downtimes on thousands of objects made every client
/// ask for each by name at the same moment, OK objects too. Only problems
/// are asked for: a provisional count of an OK object can't hide anything,
/// and its next state change brings Icinga's count (or, when it can't,
/// hands it out then). Each is handed out once, again after a downtime
/// line newer than the request, after a failed request, and after a new
/// session.
#[test]
fn only_problems_with_a_provisional_count_are_asked_for_once_each() {
    let mut store = loaded();
    let (a, b) = (key("h", "a"), key("h", "b"));
    store.apply_fetched(
        Vec::new(),
        vec![service("h", "a", ServiceState::Ok), answered_b(0)],
        Detail::Lean,
        &[],
        11,
    );
    let added = |name: &str, object: &ObjectKey, seq: u64| {
        (
            seq,
            Event::DowntimeAdded {
                downtime: fixed(name, object.clone(), 100.0, 400.0, true),
                at: t(150.0 + f64::from(u32::try_from(seq).unwrap())),
            },
        )
    };
    for (seq, event) in [added("h!a!d1", &a, 12), added("h!b!d1", &b, 13)] {
        store.apply(seq, &event);
    }
    assert!(provisional(&store, &a) && provisional(&store, &b));
    assert_eq!(
        store.take_unsettled(),
        std::slice::from_ref(&b),
        "the problem only"
    );
    assert!(store.take_unsettled().is_empty(), "once");

    // A downtime line newer than the request in flight: asked again.
    let (seq, event) = added("h!b!d2", &b, 14);
    store.apply(seq, &event);
    assert_eq!(
        store.take_unsettled(),
        std::slice::from_ref(&b),
        "after a newer line"
    );
    // That request failed: still provisional, asked again when its retry
    // is due.
    assert_eq!(
        store.reask_failed(&[a.clone(), b.clone()]),
        std::slice::from_ref(&b)
    );
    assert!(store.take_unsettled().is_empty(), "not before the retry");
    assert!(store.keep_asking(&b));

    // `a` turns into a problem by a check event older than the downtime
    // lines (the overlap of a switch): its count stays provisional, so it
    // is asked for now.
    store.apply(
        15,
        &check_result(
            a.clone(),
            svc(ServiceState::Critical),
            StateType::Hard,
            1,
            140.0,
        ),
    );
    assert_eq!(stored(&store, "h", "a").state, ServiceState::Critical);
    assert_eq!(
        depth_of(&store, "a"),
        1,
        "the older event's count isn't taken"
    );
    assert_eq!(store.take_unsettled(), std::slice::from_ref(&a));
    // A new session: the queue is gone, both handed out again.
    store.new_session();
    assert_eq!(store.take_unsettled(), [a.clone(), b.clone()]);

    // Icinga's answers settle both; nothing is asked any more.
    let mut answer_a = service("h", "a", ServiceState::Critical);
    answer_a.check.downtime_depth = 1;
    store.apply_fetched(
        Vec::new(),
        vec![answer_a, answered_b(2)],
        Detail::Lean,
        &[],
        16,
    );
    assert!(!provisional(&store, &a) && !provisional(&store, &b));
    assert!(!store.keep_asking(&b), "settled: a retry asks nothing");
    assert!(store.take_unsettled().is_empty());
}

/// Review finding (small fix 3 fixer, Icinga 2.15 `downtime.cpp`): Icinga
/// registers a new downtime with its object before it sends
/// `DowntimeAdded`, so a check event built in between counts it and can be
/// written first. A count from a check event at or after the downtime's
/// entry can't tell: the event makes it provisional (asked for, a problem)
/// instead of adding to an exact count. Before, the store added 1 (2 for
/// one downtime), and 1 stayed after the removal: hidden in quiet mode.
#[test]
fn a_check_event_from_the_moment_a_downtime_is_added_may_count_it() {
    let mut store = loaded();
    let b = key("h", "b");
    let downtime = Downtime {
        entry_time: t(150.0),
        ..fixed("h!b!d1", b.clone(), 100.0, 400.0, true)
    };
    // Built at 150.0004 (its check ended at 150.0003), with the downtime.
    store.apply(11, &counted(b.clone(), 1, 149.9004));
    store.apply(
        12,
        &Event::DowntimeAdded {
            downtime: downtime.clone(),
            at: t(150.0005),
        },
    );
    assert!(provisional(&store, &b), "can't be placed");
    assert_eq!(store.take_unsettled(), std::slice::from_ref(&b));
    store.apply_fetched(Vec::new(), vec![answered_b(1)], Detail::Lean, &[], 12);
    store.apply(
        13,
        &Event::DowntimeRemoved {
            downtime,
            at: t(160.0),
        },
    );
    store.take_unsettled();
    store.apply_fetched(Vec::new(), vec![answered_b(0)], Detail::Lean, &[], 13);
    assert_eq!(depth_of(&store, "b"), 0, "none left");

    // A check event from before the entry didn't count it: exact.
    let mut store = loaded();
    store.apply(11, &counted(b.clone(), 0, 140.0));
    store.apply(
        12,
        &Event::DowntimeAdded {
            downtime: Downtime {
                entry_time: t(150.0),
                ..fixed("h!b!d1", b.clone(), 100.0, 400.0, true)
            },
            at: t(150.0005),
        },
    );
    assert_eq!(depth_of(&store, "b"), 1);
    assert!(!provisional(&store, &b));
}

/// Acknowledgements and comments are values, not counts: an answer that
/// already has what lines still on their way bring ends up the same once
/// they are in, whatever it overtook.
#[test]
fn acknowledgements_and_comments_converge_whatever_an_answer_overtook() {
    let mut store = loaded();
    let b = key("h", "b");
    // Answered at 11 acknowledged, with its comment: lines 12 and 13 (the
    // acknowledgement and its comment) are on their way.
    let mut answer = answered_b(0);
    answer.check.acknowledgement = AckKind::Normal;
    store.apply_fetched(Vec::new(), vec![answer], Detail::Lean, &[], 11);
    store.begin_annotation_query();
    store.apply_overview(
        Overview {
            comments: vec![comment("h!b!ack", b.clone(), 140.0)],
            ..Overview::default()
        },
        11,
    );
    let set = Event::AcknowledgementSet {
        object: b.clone(),
        author: "carol".to_owned(),
        comment: "on it".to_owned(),
        kind: AckKind::Normal,
        expiry: None,
        at: t(140.0),
    };
    store.apply(12, &set);
    store.apply(
        13,
        &Event::CommentAdded {
            comment: comment("h!b!ack", b.clone(), 140.0),
            at: t(140.0),
        },
    );
    assert!(
        stored(&store, "h", "b")
            .check
            .acknowledgement
            .is_acknowledged()
    );
    assert_eq!(store.comments_of(&b).len(), 1, "once");
    // Cleared and removed; an answer composed after both arrives first.
    let mut answer = answered_b(0);
    answer.check.acknowledgement = AckKind::None;
    store.apply_fetched(Vec::new(), vec![answer], Detail::Lean, &[], 13);
    store.apply(
        14,
        &Event::AcknowledgementCleared {
            object: b.clone(),
            at: t(150.0),
        },
    );
    store.apply(
        15,
        &Event::CommentRemoved {
            comment: comment("h!b!ack", b.clone(), 140.0),
            at: t(150.0),
        },
    );
    assert!(
        !stored(&store, "h", "b")
            .check
            .acknowledgement
            .is_acknowledged()
    );
    assert!(store.comments_of(&b).is_empty());
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

#[test]
fn service_counts_follow_icinga_with_pending_services_as_unknown() {
    let mut store = loaded();
    let mut pending = service("h", "new", ServiceState::Pending);
    pending.check.last_check = None;
    store.replace_services(
        vec![
            service("h", "a", ServiceState::Ok),
            service("h", "b", ServiceState::Critical),
            pending,
        ],
        Detail::Lean,
        11,
    );
    // Icinga's raw state of a service never checked is 3 (unknown), and
    // `/v1/status/CIB` counts it there.
    assert_eq!(store.service_states(), [1, 0, 1, 1]);

    // Its first result, OK: Icinga moves it from unknown to ok, so does
    // the store (the offset between them stays).
    let mut checked = service("h", "new", ServiceState::Ok);
    checked.check.last_check = Some(t(200.0));
    store.replace_services(
        vec![
            service("h", "a", ServiceState::Ok),
            service("h", "b", ServiceState::Critical),
            checked,
        ],
        Detail::Lean,
        12,
    );
    assert_eq!(store.service_states(), [2, 0, 1, 0]);
}
