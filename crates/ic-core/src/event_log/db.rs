//! The event log's `SQLite` database: schema, writes and queries. Every
//! function here blocks; the engine calls them only from the log's own
//! thread (`super::EventLog`).

use std::path::Path;
use std::time::Duration;

use ic_model::{
    CheckableState, HostState, ObjectKey, ServiceKey, ServiceState, StateType, Timestamp,
};
use ic_rules::{NotificationIntent, Tone};
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, params};

use crate::command::{LogEntry, LogKind, NotificationRecord};

/// The schema this version writes. A database with a newer version (from
/// a newer icygui) is left alone.
pub(super) const SCHEMA_VERSION: i64 = 1;

/// Queries return at most this many rows, whatever the caller asks for.
pub(super) const MAX_ROWS: usize = 100_000;

/// How long a statement waits for a lock another connection holds (a
/// second instance, a backup tool) before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const SCHEMA: &str = "
CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    at REAL NOT NULL,
    object TEXT NOT NULL,
    kind TEXT NOT NULL,
    state TEXT,
    state_type TEXT,
    text TEXT NOT NULL,
    author TEXT
);
CREATE INDEX events_at ON events (at);
CREATE INDEX events_object_at ON events (object, at);
CREATE TABLE notifications (
    id TEXT PRIMARY KEY,
    at REAL NOT NULL,
    object TEXT,
    title TEXT NOT NULL,
    subtitle TEXT NOT NULL,
    body TEXT NOT NULL,
    tone TEXT NOT NULL,
    sound INTEGER NOT NULL,
    silent INTEGER NOT NULL,
    read INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX notifications_at ON notifications (at);
";

/// Why the database can't be used.
#[derive(Debug, thiserror::Error)]
pub(super) enum DbError {
    /// `SQLite` failed.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Creating the directory or the file failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// A newer icygui wrote it.
    #[error("the event log has schema version {found}; this version reads {SCHEMA_VERSION}")]
    Newer {
        /// The database's version.
        found: i64,
    },
}

impl DbError {
    /// Whether the file isn't a usable `SQLite` database at all (corrupt, or
    /// something else), so it may be moved aside and started afresh.
    pub(super) fn is_corrupt(&self) -> bool {
        match self {
            Self::Sqlite(error) => matches!(
                error.sqlite_error_code(),
                Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
            ),
            Self::Io(_) | Self::Newer { .. } => false,
        }
    }
}

/// An open event log.
#[derive(Debug)]
pub(super) struct Database {
    conn: Connection,
}

impl Database {
    /// Opens (creating it if needed) the database at `path`: WAL journal,
    /// the current schema. The directory is created private (0700) and the
    /// file private (0600) on Unix.
    pub(super) fn open(path: &Path) -> Result<Self, DbError> {
        if let Some(dir) = path.parent() {
            create_private_dir(dir)?;
        }
        create_private_file(path)?;
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // `auto_vacuum` only takes effect on a new database (before the
        // first table): pruned pages go back to the file system.
        conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL; PRAGMA synchronous = NORMAL;")?;
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            tracing::warn!(%mode, "the event log can't use WAL mode");
        }
        migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Appends entries, in one transaction.
    pub(super) fn record(&mut self, entries: &[LogEntry]) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO events (at, object, kind, state, state_type, text, author)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for entry in entries {
                let (state, state_type) = match entry.kind {
                    LogKind::State { state, state_type } => {
                        (Some(state_name(state)), Some(state_type_name(state_type)))
                    }
                    _ => (None, None),
                };
                insert.execute(params![
                    entry.at.as_unix_seconds(),
                    entry.object.full_name(),
                    kind_name(entry.kind),
                    state,
                    state_type,
                    entry.text,
                    entry.author,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Records notifications, in one transaction; returns for each whether
    /// it is new (an id already in the log, from an earlier run, isn't).
    pub(super) fn log_notifications(
        &mut self,
        intents: &[NotificationIntent],
    ) -> Result<Vec<bool>, DbError> {
        let tx = self.conn.transaction()?;
        let mut new = Vec::with_capacity(intents.len());
        {
            let mut insert = tx.prepare_cached(
                "INSERT OR IGNORE INTO notifications
                 (id, at, object, title, subtitle, body, tone, sound, silent, read)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
            )?;
            for intent in intents {
                let inserted = insert.execute(params![
                    intent.id,
                    intent.at.as_unix_seconds(),
                    intent.object.as_ref().map(ObjectKey::full_name),
                    intent.title,
                    intent.subtitle,
                    intent.body,
                    tone_name(intent.tone),
                    intent.sound,
                    intent.silent,
                ])?;
                new.push(inserted > 0);
            }
        }
        tx.commit()?;
        Ok(new)
    }

    /// The newest entries first, at most `limit`: of one object (a host's
    /// include its services'), or of every object.
    pub(super) fn history(
        &self,
        object: Option<&ObjectKey>,
        limit: usize,
    ) -> Result<Vec<LogEntry>, DbError> {
        let limit = row_limit(limit);
        let columns = "SELECT at, object, kind, state, state_type, text, author FROM events";
        let rows = match object {
            None => {
                let mut query = self
                    .conn
                    .prepare_cached(&format!("{columns} ORDER BY at DESC, id DESC LIMIT ?1"))?;
                query
                    .query_map(params![limit], read_entry)?
                    .collect::<Result<Vec<_>, _>>()?
            }
            Some(ObjectKey::Host { name }) => {
                // The host and its services: `host!` up to (excluding)
                // `host"`, the next byte, in the binary collation.
                let mut query = self.conn.prepare_cached(&format!(
                    "{columns} WHERE object = ?1 OR (object >= ?2 AND object < ?3)
                     ORDER BY at DESC, id DESC LIMIT ?4"
                ))?;
                let name = name.as_str();
                query
                    .query_map(
                        params![name, format!("{name}!"), format!("{name}\""), limit],
                        read_entry,
                    )?
                    .collect::<Result<Vec<_>, _>>()?
            }
            Some(service) => {
                let mut query = self.conn.prepare_cached(&format!(
                    "{columns} WHERE object = ?1 ORDER BY at DESC, id DESC LIMIT ?2"
                ))?;
                query
                    .query_map(params![service.full_name(), limit], read_entry)?
                    .collect::<Result<Vec<_>, _>>()?
            }
        };
        Ok(rows.into_iter().flatten().collect())
    }

    /// The newest notifications first, at most `limit`.
    pub(super) fn notifications(&self, limit: usize) -> Result<Vec<NotificationRecord>, DbError> {
        let mut query = self.conn.prepare_cached(
            "SELECT id, at, object, title, subtitle, body, tone, sound, silent, read
             FROM notifications ORDER BY at DESC, rowid DESC LIMIT ?1",
        )?;
        let rows = query
            .query_map(params![row_limit(limit)], read_notification)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().flatten().collect())
    }

    /// Marks every notification read; returns how many were unread.
    pub(super) fn mark_read(&self) -> Result<usize, DbError> {
        Ok(self
            .conn
            .execute("UPDATE notifications SET read = 1 WHERE read = 0", [])?)
    }

    /// Deletes events and notifications from before `before`, and gives
    /// the freed pages back. Returns how many rows went.
    pub(super) fn prune(&self, before: Timestamp) -> Result<usize, DbError> {
        let before = before.as_unix_seconds();
        let events = self
            .conn
            .execute("DELETE FROM events WHERE at < ?1", params![before])?;
        let notifications = self
            .conn
            .execute("DELETE FROM notifications WHERE at < ?1", params![before])?;
        if events + notifications > 0 {
            self.conn.execute_batch("PRAGMA incremental_vacuum")?;
        }
        Ok(events + notifications)
    }

    /// The schema version recorded in the database.
    #[cfg(test)]
    pub(super) fn version(&self) -> Result<Option<i64>, DbError> {
        Ok(self
            .conn
            .query_row("SELECT max(version) FROM schema_version", [], |row| {
                row.get(0)
            })?)
    }

    /// The journal mode (`wal`) and the `auto_vacuum` mode (2:
    /// incremental).
    #[cfg(test)]
    pub(super) fn modes(&self) -> Result<(String, i64), DbError> {
        let journal = self
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        let vacuum = self
            .conn
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))?;
        Ok((journal, vacuum))
    }
}

/// Creates the schema of a new database, or checks an existing one's
/// version.
fn migrate(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL)")?;
    let version: Option<i64> = conn
        .query_row("SELECT max(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .optional()?
        .flatten();
    match version {
        None => {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(SCHEMA)?;
            tx.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                params![SCHEMA_VERSION],
            )?;
            tx.commit()?;
            Ok(())
        }
        Some(found) if found > SCHEMA_VERSION => Err(DbError::Newer { found }),
        // Version 1 is the first; later versions migrate here.
        Some(_) => Ok(()),
    }
}

fn row_limit(limit: usize) -> i64 {
    i64::try_from(limit.min(MAX_ROWS)).unwrap_or(0)
}

/// One `events` row; `None` for a row this version doesn't understand.
fn read_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<LogEntry>> {
    let at: f64 = row.get(0)?;
    let object: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let state: Option<String> = row.get(3)?;
    let state_type: Option<String> = row.get(4)?;
    let text: String = row.get(5)?;
    let author: Option<String> = row.get(6)?;
    let Some(object) = parse_object(&object) else {
        return Ok(None);
    };
    let kind = match kind.as_str() {
        "state" => {
            let Some(state) = state.as_deref().and_then(|s| parse_state(&object, s)) else {
                return Ok(None);
            };
            let Some(state_type) = state_type.as_deref().and_then(parse_state_type) else {
                return Ok(None);
            };
            LogKind::State { state, state_type }
        }
        "acknowledgement_set" => LogKind::AcknowledgementSet,
        "acknowledgement_cleared" => LogKind::AcknowledgementCleared,
        "comment_added" => LogKind::CommentAdded,
        "comment_removed" => LogKind::CommentRemoved,
        "downtime_started" => LogKind::DowntimeStarted,
        "downtime_ended" => LogKind::DowntimeEnded,
        "flapping_started" => LogKind::FlappingStarted,
        "flapping_stopped" => LogKind::FlappingStopped,
        _ => return Ok(None),
    };
    Ok(Some(LogEntry {
        at: Timestamp::from_unix_seconds(at),
        object,
        kind,
        text,
        author,
    }))
}

/// One `notifications` row; `None` for a row this version doesn't
/// understand.
fn read_notification(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<NotificationRecord>> {
    let object: Option<String> = row.get(2)?;
    let tone: String = row.get(6)?;
    let object = match object {
        Some(name) => match parse_object(&name) {
            Some(object) => Some(object),
            None => return Ok(None),
        },
        None => None,
    };
    let Some(tone) = parse_tone(&tone) else {
        return Ok(None);
    };
    Ok(Some(NotificationRecord {
        intent: NotificationIntent {
            id: row.get(0)?,
            object,
            title: row.get(3)?,
            subtitle: row.get(4)?,
            body: row.get(5)?,
            tone,
            sound: row.get(7)?,
            silent: row.get(8)?,
            at: Timestamp::from_unix_seconds(row.get(1)?),
        },
        read: row.get(9)?,
    }))
}

/// A host or service from its full name (`host` or `host!service`).
pub(super) fn parse_object(name: &str) -> Option<ObjectKey> {
    if name.contains('!') {
        ServiceKey::parse(name).map(ObjectKey::from)
    } else if name.is_empty() {
        None
    } else {
        Some(ObjectKey::host(name))
    }
}

fn kind_name(kind: LogKind) -> &'static str {
    match kind {
        LogKind::State { .. } => "state",
        LogKind::AcknowledgementSet => "acknowledgement_set",
        LogKind::AcknowledgementCleared => "acknowledgement_cleared",
        LogKind::CommentAdded => "comment_added",
        LogKind::CommentRemoved => "comment_removed",
        LogKind::DowntimeStarted => "downtime_started",
        LogKind::DowntimeEnded => "downtime_ended",
        LogKind::FlappingStarted => "flapping_started",
        LogKind::FlappingStopped => "flapping_stopped",
    }
}

fn state_name(state: CheckableState) -> &'static str {
    match state {
        CheckableState::Host(HostState::Up) => "up",
        CheckableState::Host(HostState::Down) => "down",
        CheckableState::Host(HostState::Unreachable) => "unreachable",
        CheckableState::Service(ServiceState::Ok) => "ok",
        CheckableState::Service(ServiceState::Warning) => "warning",
        CheckableState::Service(ServiceState::Critical) => "critical",
        CheckableState::Service(ServiceState::Unknown) => "unknown",
        CheckableState::Host(HostState::Pending)
        | CheckableState::Service(ServiceState::Pending) => "pending",
    }
}

/// A state name for `object` (host and service states share `pending`).
fn parse_state(object: &ObjectKey, name: &str) -> Option<CheckableState> {
    match object {
        ObjectKey::Host { .. } => Some(CheckableState::Host(match name {
            "up" => HostState::Up,
            "down" => HostState::Down,
            "unreachable" => HostState::Unreachable,
            "pending" => HostState::Pending,
            _ => return None,
        })),
        ObjectKey::Service { .. } => Some(CheckableState::Service(match name {
            "ok" => ServiceState::Ok,
            "warning" => ServiceState::Warning,
            "critical" => ServiceState::Critical,
            "unknown" => ServiceState::Unknown,
            "pending" => ServiceState::Pending,
            _ => return None,
        })),
    }
}

fn state_type_name(state_type: StateType) -> &'static str {
    match state_type {
        StateType::Soft => "soft",
        StateType::Hard => "hard",
    }
}

fn parse_state_type(name: &str) -> Option<StateType> {
    match name {
        "soft" => Some(StateType::Soft),
        "hard" => Some(StateType::Hard),
        _ => None,
    }
}

fn tone_name(tone: Tone) -> &'static str {
    match tone {
        Tone::Critical => "critical",
        Tone::Warning => "warning",
        Tone::Unknown => "unknown",
        Tone::Recovery => "recovery",
        Tone::Info => "info",
    }
}

fn parse_tone(name: &str) -> Option<Tone> {
    match name {
        "critical" => Some(Tone::Critical),
        "warning" => Some(Tone::Warning),
        "unknown" => Some(Tone::Unknown),
        "recovery" => Some(Tone::Recovery),
        "info" => Some(Tone::Info),
        _ => None,
    }
}

/// Creates `dir` (and its parents), private on Unix.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    if dir.as_os_str().is_empty() || dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Creates an empty `path` readable only by the user (Unix), unless it
/// exists: `SQLite` would create it with the default permissions, and its
/// journal files copy the database file's.
fn create_private_file(path: &Path) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    match options.open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}
