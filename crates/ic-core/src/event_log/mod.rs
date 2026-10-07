//! The local event log (PLAN.md D3): a `SQLite` database per environment at
//! `<data_dir>/events-<environment id>.sqlite3`, feeding the notification
//! centre, the "recent events" view and the panes' history tab.
//!
//! - Tables `events` (state changes hard and soft, acknowledgements, user
//!   comments, downtime start and end, flapping; never plain check
//!   results) and `notifications` (every intent of the rule engine, silent
//!   or not, with its read flag), plus `schema_version`. WAL journal.
//! - Entries older than `General::event_log_retention_hours` are pruned
//!   when the engine starts and every hour.
//! - All database work runs on the log's own thread, in the order the
//!   engine asked for it; the engine never waits for it except, briefly,
//!   when it stops (to flush).
//! - A log that can't be opened (permissions, a full disk) leaves the
//!   engine working without one: notifications still go out, history
//!   queries answer empty. A corrupt file is moved aside
//!   (`….corrupt-<unix seconds>`) and a new one started; one written by a
//!   newer icygui is left alone.

mod db;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use futures::channel::oneshot;
use ic_model::{ObjectKey, Timestamp};
use ic_rules::NotificationIntent;

use crate::command::{LogEntry, NotificationRecord};
use db::Database;

/// The event log file of an environment: `<data_dir>/events-<id>.sqlite3`.
///
/// The id is used as it is when it consists of ASCII letters, digits and
/// `-` (every id `ic-config` generates, UUIDs); any other byte is written
/// as `_` and two hex digits, so hand-written ids (`prod/eu`, `..`) can't
/// leave the directory or collide.
#[must_use]
pub fn event_log_path(data_dir: &Path, environment_id: &str) -> PathBuf {
    let mut name = String::from("events-");
    for byte in environment_id.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' {
            name.push(char::from(byte));
        } else {
            let _ = write!(name, "_{byte:02x}");
        }
    }
    name.push_str(".sqlite3");
    data_dir.join(name)
}

/// Deletes an environment's event log (requirement ENV-03: deleting an
/// environment deletes its event log): the database and its WAL and
/// shared-memory files. Missing files are fine, so deleting twice, or an
/// environment that never ran, succeeds.
///
/// Call it after the environment's engine has stopped
/// ([`CoreHandle::shutdown`](crate::CoreHandle::shutdown)): an engine still
/// running keeps writing to the deleted file until it stops, and the next
/// start begins an empty log.
///
/// # Errors
///
/// A file exists but can't be removed (permissions, a read-only file
/// system).
pub fn delete_event_log(data_dir: &Path, environment_id: &str) -> std::io::Result<()> {
    let path = event_log_path(data_dir, environment_id);
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut file = path.clone().into_os_string();
        file.push(suffix);
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Appends `entries` to environment `environment_id`'s event log in
/// `data_dir`, creating it if needed, before its engine opens it: the
/// demo's recent history (an engine already running keeps its own copy of
/// the latest entries, so seed first).
///
/// # Errors
///
/// The log can't be opened or written (the reason, as text).
pub fn seed_event_log(
    data_dir: &Path,
    environment_id: &str,
    entries: &[LogEntry],
) -> Result<(), String> {
    let path = event_log_path(data_dir, environment_id);
    let mut database = Database::open(&path).map_err(|error| error.to_string())?;
    database.record(entries).map_err(|error| error.to_string())
}

/// Called on the log's thread with the intents that were new (an id
/// already in the log, from an earlier run, is dropped), in order.
pub(crate) type Logged = Box<dyn FnOnce(Vec<NotificationIntent>) + Send>;

/// Called on the log's thread with the notification ids the log has, of
/// those asked for (none without a log).
pub(crate) type Known = Box<dyn FnOnce(Vec<String>) + Send>;

/// Work for the log's thread.
enum Job {
    Record(Vec<LogEntry>),
    Notifications(Vec<NotificationIntent>, Logged),
    Known(Vec<String>, Known),
    History {
        object: Option<ObjectKey>,
        limit: usize,
        reply: oneshot::Sender<Vec<LogEntry>>,
    },
    LoadNotifications {
        limit: usize,
        reply: oneshot::Sender<Vec<NotificationRecord>>,
    },
    MarkRead,
    MarkOneRead(String),
    HistoryStart(oneshot::Sender<Option<Timestamp>>),
    Prune(Timestamp),
    Stop(mpsc::Sender<()>),
}

/// The engine's handle on the log's thread. Every call returns at once;
/// the work happens on the thread, in call order.
pub(crate) struct EventLog {
    jobs: mpsc::Sender<Job>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for EventLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLog")
            .field("running", &self.thread.is_some())
            .finish_non_exhaustive()
    }
}

impl EventLog {
    /// Starts the log's thread, which opens (or creates) the database at
    /// `path`. Without a thread (the system refused one), every call is a
    /// no-op and notifications pass straight through.
    pub(crate) fn open(path: PathBuf) -> Self {
        let (jobs, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("icygui-event-log".to_owned())
            .spawn(move || run(&path, &receiver));
        match thread {
            Ok(thread) => Self {
                jobs,
                thread: Some(thread),
            },
            Err(error) => {
                tracing::warn!(%error, "couldn't start the event log's thread; running without a log");
                Self { jobs, thread: None }
            }
        }
    }

    fn send(&self, job: Job) -> Result<(), Job> {
        if self.thread.is_none() {
            return Err(job);
        }
        self.jobs.send(job).map_err(|error| error.0)
    }

    /// Appends entries.
    pub(crate) fn record(&self, entries: Vec<LogEntry>) {
        if !entries.is_empty() {
            let _ = self.send(Job::Record(entries));
        }
    }

    /// Logs intents; `logged` then gets the new ones, on the log's thread
    /// (or right away, without a log).
    pub(crate) fn log_notifications(&self, intents: Vec<NotificationIntent>, logged: Logged) {
        if let Err(Job::Notifications(intents, logged)) =
            self.send(Job::Notifications(intents, logged))
        {
            logged(intents);
        }
    }

    /// Tells `known` which of the notification `ids` the log has (from
    /// an earlier run: the rule engine restores what it notified), on the
    /// log's thread (or right away, without a log).
    pub(crate) fn known(&self, ids: Vec<String>, known: Known) {
        if let Err(Job::Known(_, known)) = self.send(Job::Known(ids, known)) {
            known(Vec::new());
        }
    }

    /// Answers `reply` with the newest entries (of `object`; a host's
    /// include its services').
    pub(crate) fn history(
        &self,
        object: Option<ObjectKey>,
        limit: usize,
        reply: oneshot::Sender<Vec<LogEntry>>,
    ) {
        if let Err(Job::History { reply, .. }) = self.send(Job::History {
            object,
            limit,
            reply,
        }) {
            let _ = reply.send(Vec::new());
        }
    }

    /// Answers `reply` with the newest notifications.
    pub(crate) fn notifications(
        &self,
        limit: usize,
        reply: oneshot::Sender<Vec<NotificationRecord>>,
    ) {
        if let Err(Job::LoadNotifications { reply, .. }) =
            self.send(Job::LoadNotifications { limit, reply })
        {
            let _ = reply.send(Vec::new());
        }
    }

    /// Marks every notification read.
    pub(crate) fn mark_read(&self) {
        let _ = self.send(Job::MarkRead);
    }

    /// Marks the notification with this intent id read.
    pub(crate) fn mark_one_read(&self, id: String) {
        let _ = self.send(Job::MarkOneRead(id));
    }

    /// Answers `reply` with the time of the oldest entry (`None`: no
    /// entries, or no log).
    pub(crate) fn history_start(&self, reply: oneshot::Sender<Option<Timestamp>>) {
        if let Err(Job::HistoryStart(reply)) = self.send(Job::HistoryStart(reply)) {
            let _ = reply.send(None);
        }
    }

    /// Deletes what happened before `before`.
    pub(crate) fn prune(&self, before: Timestamp) {
        let _ = self.send(Job::Prune(before));
    }

    /// Lets the thread finish what it was given, waiting at most `timeout`
    /// (it keeps going on its own after that).
    pub(crate) fn close(&mut self, timeout: Duration) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        let (done, flushed) = mpsc::channel();
        if self.jobs.send(Job::Stop(done)).is_err() {
            return;
        }
        if flushed.recv_timeout(timeout).is_ok() {
            if thread.join().is_err() {
                tracing::error!("the event log's thread panicked");
            }
        } else {
            tracing::warn!(?timeout, "the event log didn't finish in time");
        }
    }
}

/// The log's thread: opens the database, then works through the jobs until
/// told to stop or the engine is gone.
fn run(path: &Path, jobs: &mpsc::Receiver<Job>) {
    let mut database = open(path);
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Stop(done) => {
                drop(database.take());
                let _ = done.send(());
                return;
            }
            job => handle(database.as_mut(), job),
        }
    }
}

/// Opens the database; a corrupt file is moved aside and a new one
/// started. `None` if the log can't be used.
fn open(path: &Path) -> Option<Database> {
    match Database::open(path) {
        Ok(database) => return Some(database),
        Err(error) if error.is_corrupt() => {
            let aside = set_aside(path);
            tracing::warn!(%error, path = %path.display(), aside = ?aside, "the event log is corrupt; starting a new one");
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "can't open the event log; running without one");
            return None;
        }
    }
    match Database::open(path) {
        Ok(database) => Some(database),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "can't open the event log; running without one");
            None
        }
    }
}

/// Renames a corrupt database and its journal files to
/// `<name>.corrupt-<unix seconds>…`; returns the new name.
fn set_aside(path: &Path) -> Option<PathBuf> {
    let stamp = Timestamp::now().as_unix_seconds();
    let mut aside = path.as_os_str().to_owned();
    aside.push(format!(".corrupt-{stamp:.0}"));
    let aside = PathBuf::from(aside);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut from = path.as_os_str().to_owned();
        from.push(suffix);
        let mut to = aside.as_os_str().to_owned();
        to.push(suffix);
        let _ = std::fs::rename(from, to);
    }
    match std::fs::rename(path, &aside) {
        Ok(()) => Some(aside),
        Err(error) => {
            tracing::warn!(%error, "couldn't move the corrupt event log aside; removing it");
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

/// Reads `what` from the database: the default (empty) without a log or
/// when the read fails (logged).
fn read<T: Default>(
    database: Option<&mut Database>,
    what: &str,
    query: impl FnOnce(&Database) -> Result<T, db::DbError>,
) -> T {
    match database.map(|database| query(database)) {
        Some(Ok(value)) => value,
        Some(Err(error)) => {
            tracing::warn!(%error, "couldn't read {what}");
            T::default()
        }
        None => T::default(),
    }
}

/// Runs one job against the database (`None`: no log).
fn handle(database: Option<&mut Database>, job: Job) {
    match job {
        Job::Record(entries) => {
            if let Some(database) = database
                && let Err(error) = database.record(&entries)
            {
                tracing::warn!(%error, count = entries.len(), "couldn't write to the event log");
            }
        }
        Job::Notifications(intents, logged) => {
            let new = match database.map(|database| database.log_notifications(&intents)) {
                Some(Ok(new)) => new,
                Some(Err(error)) => {
                    // Shown anyway: a notification must not get lost
                    // because the disk is full.
                    tracing::warn!(%error, "couldn't log notifications");
                    vec![true; intents.len()]
                }
                None => vec![true; intents.len()],
            };
            let fresh = intents
                .into_iter()
                .zip(new)
                .filter_map(|(intent, new)| {
                    if !new {
                        tracing::debug!(id = %intent.id, "notification already in the log");
                    }
                    new.then_some(intent)
                })
                .collect();
            logged(fresh);
        }
        Job::Known(ids, known) => {
            known(read(database, "the notifications", |database| {
                database.known(&ids)
            }));
        }
        Job::History {
            object,
            limit,
            reply,
        } => {
            let entries = read(database, "the event log", |database| {
                database.history(object.as_ref(), limit)
            });
            let _ = reply.send(entries);
        }
        Job::LoadNotifications { limit, reply } => {
            let records = read(database, "the notifications", |database| {
                database.notifications(limit)
            });
            let _ = reply.send(records);
        }
        Job::MarkRead => {
            if let Some(database) = database
                && let Err(error) = database.mark_read()
            {
                tracing::warn!(%error, "couldn't mark the notifications read");
            }
        }
        Job::MarkOneRead(id) => {
            if let Some(database) = database
                && let Err(error) = database.mark_one_read(&id)
            {
                tracing::warn!(%error, "couldn't mark a notification read");
            }
        }
        Job::HistoryStart(reply) => {
            let _ = reply.send(read(database, "the event log", Database::history_start));
        }
        Job::Prune(before) => {
            if let Some(database) = database {
                match database.prune(before) {
                    Ok(0) => {}
                    Ok(count) => tracing::debug!(count, "pruned the event log"),
                    Err(error) => tracing::warn!(%error, "couldn't prune the event log"),
                }
            }
        }
        Job::Stop(_) => {}
    }
}
