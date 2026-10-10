use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use futures::executor::block_on;
use ic_model::{CheckableState, HostState, ServiceState, StateType};
use ic_rules::{Silence, Tone};

use super::db::{Database, DbError, SCHEMA_VERSION};
use super::*;

fn state(object: &ObjectKey, at: f64, state: CheckableState, text: &str) -> LogEntry {
    LogEntry {
        at: Timestamp::from_unix_seconds(at),
        object: object.clone(),
        kind: LogKind::State {
            state,
            state_type: StateType::Hard,
        },
        text: text.to_owned(),
        author: None,
    }
}

fn intent(id: &str, at: f64, object: Option<ObjectKey>, silent: bool) -> NotificationIntent {
    NotificationIntent {
        id: id.to_owned(),
        object,
        title: format!("CRITICAL · {id}"),
        subtitle: "prod-cluster".to_owned(),
        body: "CRITICAL - lag 412s".to_owned(),
        tone: Tone::Critical,
        sound: true,
        silent,
        silenced: silent.then_some(Silence::QuietHours),
        at: Timestamp::from_unix_seconds(at),
    }
}

use crate::command::LogKind;

#[test]
fn a_new_log_has_the_schema_wal_and_private_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data").join("events-env.sqlite3");
    let database = Database::open(&path).unwrap();
    assert_eq!(database.version().unwrap(), Some(SCHEMA_VERSION));
    let (journal, vacuum) = database.modes().unwrap();
    assert_eq!(journal, "wal");
    assert_eq!(vacuum, 2, "incremental auto-vacuum");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir.path().join("data")), 0o700);
    }
    drop(database);
    // Opening it again keeps the data and the version.
    let database = Database::open(&path).unwrap();
    assert_eq!(database.version().unwrap(), Some(SCHEMA_VERSION));
}

#[test]
fn history_is_newest_first_per_object_or_host() {
    let dir = tempfile::tempdir().unwrap();
    let mut database = Database::open(&dir.path().join("log.sqlite3")).unwrap();
    let host = ObjectKey::host("db-prod-03");
    let service = ObjectKey::service("db-prod-03", "postgres-replication");
    let other = ObjectKey::host("db-prod-030");
    let other_service = ObjectKey::service("db-prod-030", "disk");
    let critical = CheckableState::Service(ServiceState::Critical);
    database
        .record(&[
            state(&service, 10.0, critical, "CRITICAL - lag 412s"),
            state(&host, 20.0, CheckableState::Host(HostState::Down), "down"),
            state(&other, 30.0, CheckableState::Host(HostState::Up), "up"),
            state(&other_service, 35.0, critical, "disk full"),
            LogEntry {
                at: Timestamp::from_unix_seconds(40.0),
                object: service.clone(),
                kind: LogKind::AcknowledgementSet,
                text: "looking".to_owned(),
                author: Some("m.keller".to_owned()),
            },
        ])
        .unwrap();

    let all = database.history(None, 10).unwrap();
    let times: Vec<f64> = all.iter().map(|e| e.at.as_unix_seconds()).collect();
    assert_eq!(times, [40.0, 35.0, 30.0, 20.0, 10.0]);
    assert_eq!(all[0].kind, LogKind::AcknowledgementSet);
    assert_eq!(all[0].author.as_deref(), Some("m.keller"));
    assert_eq!(
        all[4],
        state(&service, 10.0, critical, "CRITICAL - lag 412s"),
        "round trip"
    );

    let of_service = database.history(Some(&service), 10).unwrap();
    assert_eq!(of_service.len(), 2);
    assert!(of_service.iter().all(|entry| entry.object == service));

    // A host's history includes its services', not those of a host whose
    // name merely starts the same.
    let of_host = database.history(Some(&host), 10).unwrap();
    let objects: Vec<&ObjectKey> = of_host.iter().map(|entry| &entry.object).collect();
    assert_eq!(objects, [&service, &host, &service]);

    assert_eq!(database.history(None, 2).unwrap().len(), 2, "limit");
    assert!(database.history(None, 0).unwrap().is_empty());
}

#[test]
fn notifications_round_trip_dedupe_and_read_flags() {
    let dir = tempfile::tempdir().unwrap();
    let mut database = Database::open(&dir.path().join("log.sqlite3")).unwrap();
    let service = ObjectKey::service("db-prod-03", "postgres-replication");
    let first = intent("a", 10.0, Some(service.clone()), false);
    let summary = NotificationIntent {
        tone: Tone::Info,
        sound: false,
        silenced: Some(Silence::Paused),
        ..intent("storm:1", 20.0, None, true)
    };
    let absorbed = NotificationIntent {
        silenced: Some(Silence::Storm {
            summary: "storm:1".to_owned(),
        }),
        ..intent("c", 15.0, Some(service.clone()), true)
    };
    assert_eq!(
        database
            .log_notifications(&[first.clone(), summary.clone(), absorbed.clone()])
            .unwrap(),
        [true, true, true]
    );
    // The same id again (an earlier run's) isn't new.
    assert_eq!(
        database
            .log_notifications(&[first.clone(), intent("b", 30.0, None, false)])
            .unwrap(),
        [false, true]
    );

    let records = database.notifications(10).unwrap();
    let ids: Vec<&str> = records.iter().map(|r| r.intent.id.as_str()).collect();
    assert_eq!(ids, ["b", "storm:1", "c", "a"], "newest first");
    assert_eq!(records[1].intent, summary, "round trip, without object");
    assert_eq!(records[2].intent, absorbed, "round trip, with the storm");
    assert_eq!(records[3].intent, first, "round trip, with object");
    assert!(records.iter().all(|record| !record.read));
    assert_eq!(database.notifications(1).unwrap().len(), 1);

    // One by one (the entry the user opened), then all of them.
    assert!(database.mark_one_read("storm:1").unwrap());
    assert!(!database.mark_one_read("storm:1").unwrap(), "already read");
    assert!(!database.mark_one_read("no-such-id").unwrap());
    let read: Vec<bool> = database
        .notifications(10)
        .unwrap()
        .iter()
        .map(|record| record.read)
        .collect();
    assert_eq!(read, [false, true, false, false]);

    assert_eq!(database.mark_read().unwrap(), 3);
    assert!(database.notifications(10).unwrap().iter().all(|r| r.read));
    assert_eq!(database.mark_read().unwrap(), 0);

    // Which ids an earlier run notified, in the order asked.
    let asked = ["b", "x", "a"].map(str::to_owned);
    assert_eq!(database.known(&asked).unwrap(), ["b", "a"]);
    assert!(database.known(&[]).unwrap().is_empty());
}

#[test]
fn the_history_starts_with_its_oldest_entry() {
    let dir = tempfile::tempdir().unwrap();
    let database = Database::open(&dir.path().join("log.sqlite3")).unwrap();
    assert_eq!(database.history_start().unwrap(), None, "nothing recorded");
    let mut database = database;
    let host = ObjectKey::host("h");
    database
        .record(&[
            state(&host, 30.0, CheckableState::Host(HostState::Down), "down"),
            state(&host, 20.0, CheckableState::Host(HostState::Up), "up"),
        ])
        .unwrap();
    assert_eq!(
        database.history_start().unwrap(),
        Some(Timestamp::from_unix_seconds(20.0))
    );
    database.prune(Timestamp::from_unix_seconds(25.0)).unwrap();
    assert_eq!(
        database.history_start().unwrap(),
        Some(Timestamp::from_unix_seconds(30.0)),
        "pruning moves the start"
    );
}

#[test]
fn pruning_drops_what_is_older_than_the_cutoff() {
    let dir = tempfile::tempdir().unwrap();
    let mut database = Database::open(&dir.path().join("log.sqlite3")).unwrap();
    let host = ObjectKey::host("h");
    let up = CheckableState::Host(HostState::Up);
    database
        .record(&[
            state(&host, 100.0, up, "old"),
            state(&host, 300.0, up, "new"),
        ])
        .unwrap();
    database
        .log_notifications(&[
            intent("old", 100.0, None, false),
            intent("new", 300.0, None, false),
        ])
        .unwrap();
    assert_eq!(
        database.prune(Timestamp::from_unix_seconds(200.0)).unwrap(),
        2
    );
    let history = database.history(None, 10).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, "new");
    let records = database.notifications(10).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].intent.id, "new");
    assert_eq!(
        database.prune(Timestamp::from_unix_seconds(200.0)).unwrap(),
        0
    );
}

#[test]
fn a_log_from_before_the_silence_reasons_gains_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.sqlite3");
    {
        // Version 1 as the first release wrote it, with a silent row.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version (version) VALUES (1);
             CREATE TABLE events (id INTEGER PRIMARY KEY, at REAL NOT NULL,
                 object TEXT NOT NULL, kind TEXT NOT NULL, state TEXT,
                 state_type TEXT, text TEXT NOT NULL, author TEXT);
             CREATE TABLE notifications (id TEXT PRIMARY KEY, at REAL NOT NULL,
                 object TEXT, title TEXT NOT NULL, subtitle TEXT NOT NULL,
                 body TEXT NOT NULL, tone TEXT NOT NULL, sound INTEGER NOT NULL,
                 silent INTEGER NOT NULL, read INTEGER NOT NULL DEFAULT 0);
             INSERT INTO notifications
                 (id, at, object, title, subtitle, body, tone, sound, silent)
                 VALUES ('old', 5.0, 'h', 'DOWN · h', 'prod-cluster', '', 'critical', 1, 1);",
        )
        .unwrap();
    }
    let mut database = Database::open(&path).unwrap();
    assert_eq!(
        database.version().unwrap(),
        Some(SCHEMA_VERSION),
        "same version"
    );
    let old = &database.notifications(10).unwrap()[0];
    assert!(old.intent.silent);
    assert_eq!(old.intent.silenced, None, "the reason wasn't recorded");
    let new = intent("new", 6.0, None, true);
    database
        .log_notifications(std::slice::from_ref(&new))
        .unwrap();
    assert_eq!(database.notifications(1).unwrap()[0].intent, new);
    drop(database);
    // Opening it again finds the column there.
    let database = Database::open(&path).unwrap();
    assert_eq!(database.notifications(10).unwrap().len(), 2);
}

#[test]
fn a_newer_schema_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.sqlite3");
    drop(Database::open(&path).unwrap());
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("UPDATE schema_version SET version = 99", [])
            .unwrap();
    }
    let error = Database::open(&path).unwrap_err();
    assert!(matches!(error, DbError::Newer { found: 99 }), "{error}");
    assert!(!error.is_corrupt());

    // The engine then runs without a log: nothing written, queries empty,
    // notifications pass through.
    let log = EventLog::open(path.clone());
    log.record(vec![state(
        &ObjectKey::host("h"),
        1.0,
        CheckableState::Host(HostState::Up),
        "",
    )]);
    let (tx, rx) = oneshot::channel();
    log.history(None, 10, tx);
    assert!(block_on(rx).unwrap().is_empty());
    let (done, logged) = std_mpsc::channel();
    log.log_notifications(
        vec![intent("x", 1.0, None, false)],
        Box::new(move |fresh| done.send(fresh).unwrap()),
    );
    assert_eq!(
        logged.recv_timeout(Duration::from_secs(5)).unwrap().len(),
        1
    );
    drop(log);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 99, "untouched");
}

#[test]
fn a_corrupt_log_is_moved_aside_and_started_afresh() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events-env.sqlite3");
    std::fs::write(
        &path,
        b"this is not a database, just text that is long enough",
    )
    .unwrap();
    let mut log = EventLog::open(path.clone());
    log.record(vec![state(
        &ObjectKey::host("h"),
        1.0,
        CheckableState::Host(HostState::Up),
        "fresh",
    )]);
    let (tx, rx) = oneshot::channel();
    log.history(None, 10, tx);
    let history = block_on(rx).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, "fresh");
    log.close(Duration::from_secs(5));
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names
            .iter()
            .any(|name| name.starts_with("events-env.sqlite3.corrupt-")),
        "{names:?}"
    );
}

#[test]
fn the_thread_answers_in_order_and_flushes_on_close() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.sqlite3");
    let mut log = EventLog::open(path.clone());
    let host = ObjectKey::host("h");
    for index in 0..50 {
        log.record(vec![state(
            &host,
            f64::from(index),
            CheckableState::Host(HostState::Up),
            &index.to_string(),
        )]);
    }
    // A query after writes sees them.
    let (tx, rx) = oneshot::channel();
    log.history(Some(host.clone()), 100, tx);
    assert_eq!(block_on(rx).unwrap().len(), 50);

    let (done, logged) = std_mpsc::channel();
    let first = done.clone();
    log.log_notifications(
        vec![intent("a", 1.0, None, false), intent("b", 2.0, None, true)],
        Box::new(move |fresh| first.send(fresh).unwrap()),
    );
    log.log_notifications(
        vec![intent("a", 1.0, None, false)],
        Box::new(move |fresh| done.send(fresh).unwrap()),
    );
    let fresh = logged.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(fresh.len(), 2);
    assert!(
        logged
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_empty(),
        "a known id isn't new"
    );

    log.mark_one_read("b".to_owned());
    let (tx, rx) = oneshot::channel();
    log.notifications(10, tx);
    let read: Vec<(String, bool)> = block_on(rx)
        .unwrap()
        .into_iter()
        .map(|record| (record.intent.id, record.read))
        .collect();
    assert_eq!(read, [("b".to_owned(), true), ("a".to_owned(), false)]);
    let (done, known) = std_mpsc::channel();
    log.known(
        vec!["a".to_owned(), "c".to_owned()],
        Box::new(move |found| done.send(found).unwrap()),
    );
    assert_eq!(known.recv_timeout(Duration::from_secs(5)).unwrap(), ["a"]);
    log.mark_read();
    let (tx, rx) = oneshot::channel();
    log.notifications(10, tx);
    let records = block_on(rx).unwrap();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| record.read));
    let (tx, rx) = oneshot::channel();
    log.history_start(tx);
    assert_eq!(
        block_on(rx).unwrap(),
        Some(Timestamp::from_unix_seconds(0.0))
    );

    // Writes queued right before closing are on disk afterwards.
    log.record(vec![state(
        &host,
        99.0,
        CheckableState::Host(HostState::Down),
        "last",
    )]);
    log.close(Duration::from_secs(5));
    let database = Database::open(&path).unwrap();
    assert_eq!(database.history(None, 1).unwrap()[0].text, "last");
}

#[test]
fn paths_are_safe_for_any_id_and_deleting_removes_every_file() {
    let dir = Path::new("/data");
    assert_eq!(
        event_log_path(dir, "11111111-2222-3333-4444-555555555555"),
        Path::new("/data/events-11111111-2222-3333-4444-555555555555.sqlite3")
    );
    assert_eq!(
        event_log_path(dir, "../prod/eu"),
        Path::new("/data/events-_2e_2e_2fprod_2feu.sqlite3")
    );
    assert_ne!(event_log_path(dir, "a_2f"), event_log_path(dir, "a/"));
    assert_eq!(
        event_log_path(dir, "ü"),
        Path::new("/data/events-_c3_bc.sqlite3")
    );

    let dir = tempfile::tempdir().unwrap();
    let path = event_log_path(dir.path(), "env");
    let mut log = EventLog::open(path.clone());
    log.record(vec![state(
        &ObjectKey::host("h"),
        1.0,
        CheckableState::Host(HostState::Up),
        "",
    )]);
    log.close(Duration::from_secs(5));
    assert!(path.exists());
    let mut wal = path.clone().into_os_string();
    wal.push("-wal");
    std::fs::write(&wal, b"").unwrap();
    delete_event_log(dir.path(), "env").unwrap();
    assert!(!path.exists());
    assert!(!Path::new(&wal).exists());
    // Again, and for an environment that never ran: nothing to do.
    delete_event_log(dir.path(), "env").unwrap();
    delete_event_log(dir.path(), "never").unwrap();
    // Another environment's log stays.
    let other = event_log_path(dir.path(), "other");
    EventLog::open(other.clone()).close(Duration::from_secs(5));
    delete_event_log(dir.path(), "env").unwrap();
    assert!(other.exists());
}

#[test]
fn a_log_can_be_seeded_before_its_engine_opens_it() {
    let dir = tempfile::tempdir().unwrap();
    let service = ObjectKey::service("db-prod-03", "postgres-replication");
    let critical = CheckableState::Service(ServiceState::Critical);
    seed_event_log(
        dir.path(),
        "demo",
        &[state(&service, 10.0, critical, "CRITICAL - lag 412s")],
    )
    .unwrap();
    let database = Database::open(&event_log_path(dir.path(), "demo")).unwrap();
    assert_eq!(
        database.history(None, 10).unwrap(),
        [state(&service, 10.0, critical, "CRITICAL - lag 412s")]
    );
}
