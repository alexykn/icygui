//! Quiet mode (PERF-09) and the instant wake-up, plus the load pacing that
//! scales with the installation.
//!
//! **Modes.** An engine is *live* (the event stream carries every event
//! type the API user may read) or *quiet* ([`Command::SetQuiet`]: the
//! stream carries everything but `CheckResult`, about 95–99 % of the
//! traffic; state changes, acknowledgements, comments, downtimes,
//! flapping, Icinga's notifications and configuration changes still
//! arrive at once, so notifications are never delayed). Quiet mode also
//! polls the status every `Tuning::quiet_status_interval`, reconciles at
//! least every `Tuning::quiet_reconcile_interval`, runs no freshness
//! watchdog and no hydration, and evaluates only the dashboards that take
//! part in notification decisions.
//!
//! **Switching without losing or repeating an event.** Icinga can't
//! change a stream's event types, so a switch opens a second stream with
//! the new types while the old one keeps being applied:
//!
//! 1. Icinga subscribes a stream before it answers the request
//!    (`EventsHandler`: the subscriber exists before the response headers
//!    are flushed), so once the new stream is open it receives every event
//!    sent from then on.
//! 2. Both streams are read side by side; the new one's lines wait. Every
//!    old-stream line read since the new stream was asked for is
//!    remembered by a hash of its bytes (Icinga encodes an event once per
//!    stream from the same dictionary, so the same event is the same
//!    line on both).
//! 3. As soon as a line arrived on both streams (the old stream has then
//!    delivered everything sent before the new one subscribed), or after
//!    `Tuning::stream_handover` at the latest, the old stream is closed
//!    and what its reader had read is applied. The new stream's lines are
//!    then applied in order, skipping those the old stream already
//!    brought (by hash, once each); the new stream goes on from there.
//! 4. Waking up, the new stream's check results from the overlap can be
//!    older than a state change the old stream already applied: while
//!    duplicates are being skipped, a check result older than the stored
//!    one is skipped too (Icinga itself drops results older than the
//!    current one).
//!
//! Lines are numbered by one counter over every stream (the reader numbers
//! a line when it reads it), so the store's ordering against query answers
//! holds across the switch. A switch whose new stream fails to open (or
//! dies during the overlap) keeps the old stream and tries again later
//! (backoff); a session that ends meanwhile reconnects in the wanted mode.
//!
//! **Waking up.** When the live stream is back: the freshness watchdog
//! gives every object one interval before it looks at it again (deadlines
//! that passed while no check results came say nothing), and the problems
//! whose check result a check may have replaced meanwhile are fetched in
//! full by name in the background lane (at most [`WAKE_REFRESH_MAX`], most
//! severe and most recent first) — after the object the user opens
//! ([`Command::Focus`], its own request ahead of every queue and of the
//! request budget) and the rows on screen ([`Command::Hydrate`]).
//!
//! **Prefetch on notification.** An object whose notification is shown is
//! fetched in full right away (one small request), so its pane is
//! complete when the notification is clicked; silent notifications (storm,
//! pause, quiet hours) prefetch nothing, and at most [`PREFETCH_MAX`] go
//! out per [`PREFETCH_WINDOW`].
//!
//! **Background starts** ([`crate::Start::Background`]): the first load
//! waits a random delay of up to `Tuning::start_delay_per_thousand` per
//! 1 000 services (the count from `/v1/status/CIB`, two small requests;
//! without `status/query`, [`SERVICES_PER_HOST`] per host from a list of
//! the hosts' names), at most `Tuning::start_delay_max`;
//! [`Command::StartNow`] ends it. The stream is open meanwhile: a state
//! that changes during the wait is judged as a change once the load is in
//! (it notifies), not seeded as already known.
//!
//! **A quiet stream that stalls** can be silent for good reasons (no state
//! changed). Each quiet status poll compares Icinga's service counts by
//! state with the previous poll's (same node): they only move with a
//! state change, which comes with a `StateChange` event. Counts that moved
//! while no line arrived mean a stalled stream (confirmed
//! [`STALL_CONFIRM`] later, if still no line came: reconnect), or, when
//! the session reconnected meanwhile, changes during the gap (reload).
//!
//! [`Command::SetQuiet`]: crate::Command::SetQuiet
//! [`Command::Focus`]: crate::Command::Focus
//! [`Command::Hydrate`]: crate::Command::Hydrate
//! [`Command::StartNow`]: crate::Command::StartNow

use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ic_api::{ApiError, Detail, EventLines};
use ic_model::{CheckInfo, CheckableState, Event, EventKind, InstanceStatus, ObjectKey, StateType};
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::task::AbortHandle;
use tokio::time::Instant;

use super::stream::{self, ReaderMsg};
use super::{Engine, Internal, Phase, sync::ReloadCause};
use crate::command::{ConnectionState, LoadPhase};
use crate::connect::{self, Failure};
use crate::spec::Start;

/// After waking up, at most this many problems are fetched in full in the
/// background (five requests); the others' results come with their next
/// check.
pub(super) const WAKE_REFRESH_MAX: usize = 1_000;

/// At most this many notified objects are prefetched per
/// [`PREFETCH_WINDOW`].
pub(super) const PREFETCH_MAX: usize = 10;
/// See [`PREFETCH_MAX`].
pub(super) const PREFETCH_WINDOW: Duration = Duration::from_mins(1);

/// Without `status/query`, a background start estimates the number of
/// services from the number of hosts at this many per host (the large
/// installations icygui is measured against have 15).
pub(super) const SERVICES_PER_HOST: u32 = 15;

/// After a handover, lines of the new stream are checked against the old
/// stream's for at most this long.
const DEDUPE_FOR: Duration = Duration::from_mins(1);

/// A quiet stream suspected of stalling (Icinga's counts moved, no line
/// came) is reconnected if still no line came this long after (or
/// `Tuning::stall_after`, if shorter).
pub(super) const STALL_CONFIRM: Duration = Duration::from_secs(10);

/// How long a failed switch waits before it is tried again, at first and
/// at most.
pub(super) const SWITCH_RETRY: (Duration, Duration) =
    (Duration::from_secs(5), Duration::from_mins(5));

/// A switch of the event stream's subscription in progress.
#[derive(Debug)]
pub(super) struct Switch {
    /// Its number (answers of an abandoned switch are dropped).
    id: u64,
    /// The mode it switches to.
    quiet: bool,
    /// The new stream's event types.
    kinds: Vec<EventKind>,
    /// The line count when the new stream was asked for: old-stream lines
    /// read after it may come on the new stream too.
    since: u64,
    /// Hashes of those old-stream lines (how many of each).
    old: HashMap<u64, u32>,
    stage: Stage,
}

#[derive(Debug)]
enum Stage {
    /// The new stream is being opened.
    Opening,
    /// Both streams are read; the new one's lines wait in `buffered`.
    Overlap {
        /// The new stream's lines (taken out of here while the engine
        /// waits on them).
        lines: Option<UnboundedReceiver<ReaderMsg>>,
        reader: AbortHandle,
        buffered: Vec<ReaderMsg>,
        /// Hashes of the new stream's lines so far.
        seen: HashSet<u64>,
        /// The old stream is closed by then at the latest.
        until: Instant,
    },
}

/// After a handover: the old stream's lines the new one may bring again.
#[derive(Debug)]
pub(super) struct Dedupe {
    hashes: HashMap<u64, u32>,
    until: Instant,
    /// Waking up: check results older than the stored one are skipped.
    older_checks: bool,
}

/// Icinga's service counts by state minus the store's, as a status poll
/// answered by `node` at `at` found them: constant while the store follows
/// every state change, whatever the API user may not see or a satellite
/// leaves out.
#[derive(Clone, Debug)]
pub(super) struct CountOffset {
    node: String,
    offset: [i64; 4],
    at: Instant,
}

impl CountOffset {
    fn of(node: &str, icinga: [u32; 4], store: [u32; 4], at: Instant) -> Self {
        let mut offset = [0_i64; 4];
        for ((offset, icinga), store) in offset.iter_mut().zip(icinga).zip(store) {
            *offset = i64::from(icinga) - i64::from(store);
        }
        Self {
            node: node.to_owned(),
            offset,
            at,
        }
    }

    /// The same offset as `other`, from the same node.
    fn same(&self, other: &Self) -> bool {
        self.node == other.node && self.offset == other.offset
    }
}

/// How many status polls' offsets are kept: the references of a switch's
/// check (those answered while the old stream still delivered lines; more
/// than one, so a poll that raced a state change whose event came moments
/// after the answer is outvoted).
const OFFSETS_KEPT: usize = 4;

/// A check that can't tell after this many polls ends: with a mismatch
/// seen, as one that found something missing.
const VERIFY_TRIES: u32 = 4;

/// A switch whose old stream may have withheld events, being checked.
#[derive(Debug)]
pub(super) struct Verify {
    /// Polls sent before this don't count.
    since: Instant,
    /// The offsets the store's counts should still have.
    references: Vec<CountOffset>,
    /// A mismatch the previous poll found (a second one confirms it).
    suspect: Option<CountOffset>,
    tries: u32,
}

/// A reload a switch's check asked for: once it is in, the offset should
/// match the references again; if it doesn't, Icinga's counts include
/// objects the API user doesn't see, and switches aren't checked against
/// them any more.
#[derive(Debug)]
pub(super) struct Recheck {
    references: Vec<CountOffset>,
    asked: Instant,
}

/// What a poll says about a switch being checked.
enum Verdict {
    Fine,
    Missed,
    Unsure,
}

/// The object the user is opening ([`crate::Command::Focus`]).
#[derive(Debug, Default)]
pub(super) struct Focus {
    /// Being fetched.
    pub(super) flying: Option<ObjectKey>,
    /// Asked for while another was being fetched (the latest wins).
    pub(super) next: Option<ObjectKey>,
}

/// A line's hash, to recognise the same event on two streams.
fn line_hash(line: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    line.hash(&mut hasher);
    hasher.finish()
}

/// How long a background start waits before its first load: random, up to
/// `per_thousand` per 1 000 services (`max` when the size couldn't be
/// read at all), at most `max`.
pub(super) fn start_delay(
    services: Option<u32>,
    per_thousand: Duration,
    max: Duration,
) -> Duration {
    let ceiling = services.map_or(max, |services| {
        per_thousand.mul_f64(f64::from(services) / 1_000.0).min(max)
    });
    ceiling.mul_f64(fastrand::f64())
}

/// The installation's size for a background start's delay: Icinga's
/// service count (`/v1/status/CIB`, with `status/query`), else an estimate
/// from the number of hosts (a list of their names) at
/// [`SERVICES_PER_HOST`]; `None` if neither could be read.
async fn installation_size(client: &ic_api::Client, sized: bool) -> Option<u32> {
    if sized {
        match client.status().await {
            Ok(status) => return Some(status.counts.services()),
            Err(error) => tracing::debug!(%error, "couldn't read the installation's size"),
        }
    }
    match client.host_count().await {
        Ok(hosts) => Some(
            u32::try_from(hosts)
                .unwrap_or(u32::MAX)
                .saturating_mul(SERVICES_PER_HOST),
        ),
        Err(error) => {
            tracing::debug!(%error, "couldn't count the hosts");
            None
        }
    }
}

/// When a check would have replaced `check`'s result (Unix seconds on
/// Icinga's clock): the result's end plus the interval Icinga checks at
/// (the retry interval while a problem with active checks is soft).
/// `None` without a result.
fn result_due(state: CheckableState, check: &CheckInfo) -> Option<f64> {
    let result = check.result.as_ref()?;
    let end = result
        .execution_end
        .non_zero()
        .or(check.last_check)?
        .as_unix_seconds();
    let soft_problem =
        state.is_problem() && check.state_type == StateType::Soft && check.features.active_checks;
    let interval = if soft_problem {
        check.retry_interval
    } else {
        check.check_interval
    };
    let interval = if interval.is_finite() && interval > 0.0 {
        interval
    } else {
        // Nothing expected of it (passive, no interval): due never.
        f64::INFINITY
    };
    Some(end + interval)
}

impl Engine {
    /// Whether quiet mode is wanted ([`crate::Command::SetQuiet`]).
    pub(super) fn quiet(&self) -> bool {
        self.quiet_wanted.load(Ordering::SeqCst)
    }

    /// Whether the session's stream carries no check results (quiet mode;
    /// no session: what the last stream did).
    pub(super) fn stream_quiet(&self) -> bool {
        self.conn
            .as_ref()
            .map_or(self.stream_quiet, |conn| conn.quiet)
    }

    /// `Command::SetQuiet`.
    pub(super) fn set_quiet(&mut self, quiet: bool) {
        if self.quiet_wanted.swap(quiet, Ordering::SeqCst) == quiet {
            return;
        }
        tracing::info!(environment = %self.spec.environment.name, quiet, "quiet mode");
        let now = Instant::now();
        if quiet {
            // The reconcile moves out to the quiet interval; the status
            // poll after the next one follows the quiet pace by itself.
            if self.reconcile_at.is_some() {
                self.schedule_reconcile();
            }
        } else {
            let interval = self.tuning.status_interval;
            if let Some(conn) = &mut self.conn
                && let Some(at) = &mut conn.next_status
            {
                *at = (*at).min(now + interval);
            }
            // Never a reconcile at once for waking up: as planned, but at
            // the latest one live interval from now.
            if let Some(planned) = self.reconcile_at {
                self.reconcile_at = Some(planned.min(now + self.reconcile_interval()));
            }
            // Every dashboard again (those left out in full).
            self.dashboards_resume = true;
            if self.active {
                self.nodes_on_screen();
            }
        }
        self.want_mode();
    }

    // --- switching the stream ----------------------------------------------------

    /// Starts switching the stream to the wanted mode, if it differs and
    /// nothing else is in the way (connected and live with a stream, no
    /// switch running or waiting to be tried again).
    pub(super) fn want_mode(&mut self) {
        let quiet = self.quiet();
        if self.phase != Phase::Live || self.switch_retry_at.is_some() {
            return;
        }
        if let Some(switch) = &self.switch {
            if switch.quiet != quiet {
                // Changed its mind during a switch: let that one finish,
                // then switch back (its completion asks again).
                tracing::debug!("quiet mode changed during a switch; switching back after it");
            }
            return;
        }
        let Some(conn) = &mut self.conn else {
            return;
        };
        if conn.quiet == quiet {
            return;
        }
        if self.lines.is_none() {
            // No stream: only the schedules change.
            conn.quiet = quiet;
            self.stream_quiet = quiet;
            self.mode_changed = true;
            return;
        }
        let kinds = connect::stream_kinds(&conn.info, quiet);
        if kinds == conn.kinds {
            // The API user may not read check results anyway.
            conn.quiet = quiet;
            self.stream_quiet = quiet;
            self.mode_changed = true;
            return;
        }
        self.switches += 1;
        let id = self.switches;
        let client = conn.client.clone();
        let tx = self.internal_tx.clone();
        let session = self.session;
        tracing::debug!(quiet, "switching the event stream");
        self.switch = Some(Switch {
            id,
            quiet,
            kinds: kinds.clone(),
            since: self.seq.load(Ordering::SeqCst),
            old: HashMap::new(),
            stage: Stage::Opening,
        });
        self.tasks.spawn(async move {
            let result = connect::open_events(&client, &kinds).await;
            let _ = tx.send(Internal::StreamOpened {
                session,
                switch: id,
                result,
            });
        });
    }

    /// The new stream of switch `id` is open (or couldn't be opened).
    pub(super) fn on_stream_opened(&mut self, id: u64, result: Result<EventLines, ApiError>) {
        if self.switch.as_ref().is_none_or(|switch| switch.id != id) {
            return;
        }
        match result {
            Ok(lines) => {
                let (tx, rx) = mpsc::unbounded_channel();
                let reader = self
                    .tasks
                    .spawn(stream::read(lines, Arc::clone(&self.seq), tx));
                let until = Instant::now() + self.tuning.stream_handover;
                if let Some(switch) = &mut self.switch {
                    switch.stage = Stage::Overlap {
                        lines: Some(rx),
                        reader,
                        buffered: Vec::new(),
                        seen: HashSet::new(),
                        until,
                    };
                }
            }
            Err(ApiError::Unauthorized) => {
                self.switch = None;
                self.fail(Failure::Auth(ApiError::Unauthorized.to_string()));
            }
            Err(error) => self.abandon_switch(&format!("the new event stream: {error}")),
        }
    }

    /// The switch failed: the old stream stays, the switch is tried again
    /// later.
    fn abandon_switch(&mut self, why: &str) {
        if let Some(Switch {
            stage: Stage::Overlap { reader, .. },
            ..
        }) = self.switch.take()
        {
            reader.abort();
        }
        let wait = self.switch_backoff.fail();
        tracing::warn!(%why, ?wait, "couldn't switch the event stream; trying again later");
        self.switch_retry_at = Some(Instant::now() + wait);
    }

    /// The new stream's receiver while both streams are read (the engine
    /// waits on it outside `self`).
    pub(super) fn take_overlap(&mut self) -> Option<UnboundedReceiver<ReaderMsg>> {
        match &mut self.switch {
            Some(Switch {
                stage: Stage::Overlap { lines, .. },
                ..
            }) => lines.take(),
            _ => None,
        }
    }

    /// Puts it back.
    pub(super) fn restore_overlap(&mut self, receiver: Option<UnboundedReceiver<ReaderMsg>>) {
        if let (
            Some(receiver),
            Some(Switch {
                stage: Stage::Overlap { lines, .. },
                ..
            }),
        ) = (receiver, &mut self.switch)
        {
            *lines = Some(receiver);
        }
    }

    /// When the overlap ends at the latest.
    pub(super) fn handover_at(&self) -> Option<Instant> {
        match &self.switch {
            Some(Switch {
                stage: Stage::Overlap { until, .. },
                ..
            }) => Some(*until),
            _ => None,
        }
    }

    /// Whether both streams are being read.
    pub(super) fn overlapping(&self) -> bool {
        self.handover_at().is_some()
    }

    /// Lines of the new stream during the overlap: they wait; a line the
    /// old stream brought too ends the overlap. Its end abandons the
    /// switch.
    pub(super) fn on_new_lines(&mut self, batch: Vec<ReaderMsg>) {
        let mut common = false;
        let mut ended = None;
        let Some(Switch {
            old,
            stage: Stage::Overlap { buffered, seen, .. },
            ..
        }) = &mut self.switch
        else {
            return;
        };
        for message in batch {
            match message {
                ReaderMsg::Line(seq, line) => {
                    let hash = line_hash(&line);
                    common |= old.contains_key(&hash);
                    seen.insert(hash);
                    buffered.push(ReaderMsg::Line(seq, line));
                }
                ReaderMsg::End(error) => {
                    ended = Some(error);
                    break;
                }
            }
        }
        if let Some(error) = ended {
            let why = error.map_or_else(
                || "the new event stream was closed".to_owned(),
                |error| format!("the new event stream broke: {error}"),
            );
            self.abandon_switch(&why);
        } else if common {
            self.complete_handover(true);
        }
    }

    /// The new stream's reader is gone without saying why.
    pub(super) fn new_reader_gone(&mut self) {
        tracing::error!("the new event stream's reader ended unexpectedly");
        self.abandon_switch("the new event stream ended unexpectedly");
    }

    /// Remembers old-stream lines that the new stream may bring too;
    /// whether one of them already came on it (the overlap can end).
    pub(super) fn note_old_lines(&mut self, lines: &[(u64, Vec<u8>)]) -> bool {
        let Some(switch) = &mut self.switch else {
            return false;
        };
        let mut common = false;
        for (seq, line) in lines {
            if *seq <= switch.since {
                continue;
            }
            let hash = line_hash(line);
            *switch.old.entry(hash).or_default() += 1;
            if let Stage::Overlap { seen, .. } = &switch.stage {
                common |= seen.contains(&hash);
            }
        }
        common
    }

    /// Ends the overlap: closes the old stream, applies what its reader
    /// had read, then the new stream's lines without those the old one
    /// brought; the new stream goes on. `proven`: a line came on both
    /// streams; otherwise the next status polls verify that nothing went
    /// missing ([`Engine::check_counts`]).
    pub(super) fn complete_handover(&mut self, proven: bool) {
        let Some(switch) = self.switch.take() else {
            return;
        };
        let Switch {
            quiet,
            kinds,
            mut old,
            since,
            stage,
            ..
        } = switch;
        let Stage::Overlap {
            lines: new_lines,
            reader: new_reader,
            mut buffered,
            ..
        } = stage
        else {
            return;
        };
        let Some(mut new_lines) = new_lines else {
            // Never happens: the receiver is only taken while waiting.
            new_reader.abort();
            return;
        };
        // The old stream: stop it, apply what was read.
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
        let mut rest = Vec::new();
        if let Some(receiver) = &mut self.lines {
            // Its end (the abort) says nothing: the new stream goes on.
            while let Ok(message) = receiver.try_recv() {
                if let ReaderMsg::Line(seq, line) = message {
                    rest.push((seq, line));
                }
            }
        }
        for (seq, line) in &rest {
            if *seq > since {
                *old.entry(line_hash(line)).or_default() += 1;
            }
        }
        self.apply_lines(rest);
        // The new stream takes over.
        while let Ok(message) = new_lines.try_recv() {
            buffered.push(message);
        }
        self.lines = Some(new_lines);
        self.reader = Some(new_reader);
        let was_quiet = self.stream_quiet;
        let old_last_line = self
            .conn
            .as_ref()
            .map_or_else(Instant::now, |conn| conn.last_line);
        if let Some(conn) = &mut self.conn {
            conn.check_events = kinds.contains(&EventKind::CheckResult);
            conn.kinds = kinds;
            conn.quiet = quiet;
            // The stall watch counts from the new stream.
            conn.last_line = Instant::now();
        }
        self.stream_quiet = quiet;
        self.mode_changed = true;
        self.switch_backoff.reset();
        self.dedupe = Some(Dedupe {
            hashes: old,
            until: Instant::now() + DEDUPE_FOR,
            older_checks: !quiet,
        });
        tracing::debug!(
            quiet,
            proven,
            waiting = buffered.len(),
            "the new event stream took over"
        );
        if was_quiet && !quiet {
            self.woke();
        }
        self.on_lines(buffered);
        if !proven {
            self.verify_switch(old_last_line);
        }
        // Asked for the other mode meanwhile.
        self.want_mode();
    }

    /// Drops lines the old stream already brought (after a handover).
    pub(super) fn drop_duplicates(&mut self, lines: &mut Vec<(u64, Vec<u8>)>) {
        let Some(dedupe) = &mut self.dedupe else {
            return;
        };
        if dedupe.hashes.is_empty() || Instant::now() >= dedupe.until {
            self.dedupe = None;
            return;
        }
        lines.retain(|(_, line)| {
            let hash = line_hash(line);
            match dedupe.hashes.get_mut(&hash) {
                Some(count) => {
                    *count -= 1;
                    if *count == 0 {
                        dedupe.hashes.remove(&hash);
                    }
                    false
                }
                None => true,
            }
        });
    }

    /// While waking up: a check result older than the object's stored one
    /// (the new stream's from the overlap, after a state change the old
    /// stream brought).
    pub(super) fn older_check(&self, event: &Event) -> bool {
        if !self
            .dedupe
            .as_ref()
            .is_some_and(|dedupe| dedupe.older_checks)
        {
            return false;
        }
        let Event::CheckResult {
            object, result, at, ..
        } = event
        else {
            return false;
        };
        // Strictly older: a check's result and its state change carry the
        // same end, and the attempts of one problem can come within a
        // millisecond (passive results, a burst).
        let end = result.execution_end.non_zero().unwrap_or(*at);
        self.store
            .check(object)
            .and_then(|check| check.last_check)
            .is_some_and(|last| end.as_unix_seconds() < last.as_unix_seconds())
    }

    // --- waking up -----------------------------------------------------------------

    /// The live stream is back after quiet mode: deadlines start over, and
    /// the problems whose results may have been replaced meanwhile are
    /// fetched in the background.
    pub(super) fn woke(&mut self) {
        let now = self.icinga_seconds();
        self.woke_at = Some(now);
        self.watchdog.rebase();
        if self.reload_at.is_some() && self.reload_full {
            // A reload follows, with every problem's details.
            return;
        }
        let mut stale: Vec<(u32, f64, ObjectKey)> = self
            .store
            .hosts()
            .values()
            .filter(|host| host.is_problem())
            .filter(|host| !self.result_current(&host.key()))
            .map(|host| {
                (
                    host.severity(),
                    host.check.last_state_change.as_unix_seconds(),
                    host.key(),
                )
            })
            .chain(
                self.store
                    .services()
                    .values()
                    .filter(|service| service.is_problem())
                    .filter(|service| !self.result_current(&service.object_key()))
                    .map(|service| {
                        (
                            service.severity(),
                            service.check.last_state_change.as_unix_seconds(),
                            service.object_key(),
                        )
                    }),
            )
            .collect();
        stale.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.total_cmp(&a.1)));
        let total = stale.len();
        let added = self.fetch.mark_background(
            stale
                .into_iter()
                .take(WAKE_REFRESH_MAX)
                .map(|(_, _, key)| key),
            Instant::now(),
        );
        self.note_updating();
        tracing::debug!(total, added, "awake: refreshing the problems' details");
    }

    /// Icinga's time now (Unix seconds), as far as it is known.
    pub(super) fn icinga_seconds(&self) -> f64 {
        self.watchdog
            .icinga_now()
            .unwrap_or_else(|| self.ports.clock.now().as_unix_seconds())
    }

    /// Whether the store holds `key` in full with a result no check can
    /// have replaced unseen: the stream has carried check results since
    /// before the next check was due (or always has).
    pub(super) fn result_current(&self, key: &ObjectKey) -> bool {
        if let ObjectKey::Service { key: service } = key
            && (!self.store.is_full(service) || self.store.result_is_stale(service))
        {
            return false;
        }
        let Some((state, check)) = self.store.checkable(key) else {
            return false;
        };
        let Some(due) = result_due(state, check) else {
            return false;
        };
        // Since when a check could have gone unseen.
        let cutoff = if self.stream_quiet() {
            self.icinga_seconds()
        } else {
            match self.woke_at {
                Some(woke) => woke,
                None => return true,
            }
        };
        due > cutoff
    }

    // --- the object the user opens ---------------------------------------------

    /// `Command::Focus`.
    pub(super) fn focus(&mut self, key: ObjectKey) {
        if !self.store.contains(&key) {
            tracing::debug!(object = %key, "focus: unknown object");
            return;
        }
        if self.focus.flying.as_ref() == Some(&key) || self.result_current(&key) {
            return;
        }
        if self.phase != Phase::Live || self.conn.is_none() {
            // Not connected: the pane shows what the store has.
            return;
        }
        self.fetch.forget(&key);
        if self.focus.flying.is_some() {
            self.focus.next = Some(key);
        } else {
            self.start_focus(key);
        }
        self.note_updating();
    }

    fn start_focus(&mut self, key: ObjectKey) {
        let Some(conn) = &self.conn else {
            return;
        };
        let client = conn.client.priority();
        let seq = Arc::clone(&self.seq);
        let tx = self.internal_tx.clone();
        let session = self.session;
        self.focus.flying = Some(key.clone());
        self.tasks.spawn(async move {
            let started = seq.load(Ordering::SeqCst);
            let result = client
                .objects(std::slice::from_ref(&key), Detail::Full)
                .await;
            let _ = tx.send(Internal::Focused {
                session,
                key,
                started,
                result,
            });
        });
    }

    /// The opened object's answer: applied and published at once.
    pub(super) fn on_focused(
        &mut self,
        key: &ObjectKey,
        started: u64,
        result: Result<ic_api::Fetched, ApiError>,
    ) {
        self.focus.flying = None;
        self.note_updating();
        match result {
            Ok(fetched) => {
                self.store.apply_fetched(
                    fetched.hosts,
                    fetched.services,
                    Detail::Full,
                    &fetched.missing,
                    started,
                );
                self.watchdog
                    .answered(&self.store, std::slice::from_ref(key));
                self.record_discovered(false);
            }
            Err(ApiError::Unauthorized) => {
                self.fail(Failure::Auth(ApiError::Unauthorized.to_string()));
                return;
            }
            Err(ApiError::Forbidden(message)) => {
                let hosts = matches!(key, ObjectKey::Host { .. });
                tracing::warn!(%message, "the API user may not query this kind by name; not asking again");
                self.fetch.refuse(hosts, !hosts);
            }
            Err(error) => tracing::warn!(%error, object = %key, "couldn't fetch the opened object"),
        }
        self.publish();
        if let Some(next) = self.focus.next.take() {
            self.focus(next);
        }
    }

    // --- prefetch on notification ------------------------------------------------

    /// A notification about `key` is shown: its details are fetched now,
    /// so its pane is complete when it is clicked.
    pub(super) fn prefetch(&mut self, key: &ObjectKey) {
        if self.phase != Phase::Live || !self.store.contains(key) || self.result_current(key) {
            return;
        }
        let now = Instant::now();
        while self
            .prefetched
            .front()
            .is_some_and(|at| now.duration_since(*at) >= PREFETCH_WINDOW)
        {
            self.prefetched.pop_front();
        }
        if self.prefetched.len() >= PREFETCH_MAX {
            tracing::debug!(object = %key, "prefetch: too many notifications; skipped");
            return;
        }
        if self.fetch.mark_full([key.clone()], now) > 0 {
            self.prefetched.push_back(now);
            self.note_updating();
        }
    }

    /// Brings `Snapshot::updating` up to date (the objects waiting for or
    /// in a full fetch); a change is a reason to publish.
    pub(super) fn note_updating(&mut self) {
        let updating: BTreeSet<ObjectKey> = self
            .fetch
            .updating()
            .chain(&self.focus.flying)
            .chain(&self.focus.next)
            .cloned()
            .collect();
        if *self.updating != updating {
            self.updating = Arc::new(updating);
            self.updating_changed = true;
        }
    }

    // --- background starts ---------------------------------------------------------

    /// The session's first load: at once, or after a background start's
    /// delay (sized from Icinga's status first).
    pub(super) fn begin_first_load(&mut self) {
        if self.start == Start::User {
            self.start_load(super::LoadKind::First);
            return;
        }
        let Some((client, sized)) = self
            .conn
            .as_ref()
            .map(|conn| (conn.client.clone(), conn.info.allows("status/query")))
        else {
            return;
        };
        self.set_state(ConnectionState::Loading {
            phase: LoadPhase::Hosts,
            done: 0,
            total: Some(super::load::OVERVIEW_QUERIES),
        });
        let tx = self.internal_tx.clone();
        let session = self.session;
        self.tasks.spawn(async move {
            let services = installation_size(&client, sized).await;
            let _ = tx.send(Internal::StartSize { session, services });
        });
    }

    /// A background start's first load waits for a delay that fits
    /// `services` (unknown: the longest).
    pub(super) fn start_after(&mut self, services: Option<u32>) {
        if self.phase != Phase::Loading || self.load.is_some() {
            return;
        }
        if self.start == Start::User {
            self.start_load(super::LoadKind::First);
            return;
        }
        let delay = start_delay(
            services,
            self.tuning.start_delay_per_thousand,
            self.tuning.start_delay_max,
        );
        tracing::info!(
            ?services,
            ?delay,
            "started in the background; the first load waits"
        );
        self.start_at = Some(Instant::now() + delay);
    }

    /// `Command::StartNow`: the user is here.
    pub(super) fn start_now(&mut self) {
        self.start = Start::User;
        if self.start_at.is_some() {
            self.start_at = Some(Instant::now());
        }
    }

    // --- a switch without proof ----------------------------------------------------

    /// A switch ended without a line on both streams (the overlap timed
    /// out, or the old stream ended), so nothing proves the old stream
    /// delivered everything sent before the new one subscribed: a status
    /// poll now, and the store's service counts by state compared with
    /// Icinga's against the offsets of the polls answered while the old
    /// stream still delivered lines (`old_last_line`; a stall that began
    /// later may already show in the others), the last of which is kept
    /// however long ago it was. If there is none, the latest is the
    /// reference: the stream's stall watch (a live stream silent while
    /// checks run, a quiet stream whose counts moved without a line) would
    /// have caught a stall there. Without
    /// `status/query` (or a poll before the switch) there is nothing to
    /// compare: as without a switch, a stall isn't noticed then.
    pub(super) fn verify_switch(&mut self, old_last_line: Instant) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        if conn.next_status.is_none() || self.counts_untrusted {
            return;
        }
        let mut references: Vec<CountOffset> = self
            .count_offsets
            .iter()
            .chain(&self.count_heard)
            .filter(|offset| offset.at <= old_last_line)
            .cloned()
            .collect();
        if references.is_empty() {
            references.extend(self.count_offsets.back().cloned());
        }
        if references.is_empty() {
            tracing::debug!("the switch can't be checked (no status poll before it)");
            return;
        }
        let now = Instant::now();
        self.verify = Some(Verify {
            since: now,
            references,
            suspect: None,
            tries: 0,
        });
        if !conn.status_in_flight {
            conn.next_status = Some(now);
        }
    }

    /// A status poll's answer (the poll was sent at `sent`): judges a
    /// switch being checked ([`Engine::verify_switch`]), else keeps the
    /// offset of Icinga's service counts against the store's. The offsets
    /// only differ if the store missed a state change (or one was on its
    /// way when the poll was answered: a mismatch counts once the next
    /// poll, at most [`STALL_CONFIRM`] later, finds the same); then the old
    /// stream withheld events, and a reload (jittered, as after a
    /// reconnect) brings them.
    pub(super) fn check_counts(&mut self, status: &InstanceStatus, sent: Instant) {
        // Lines still waiting: the store lags behind what Icinga counted.
        let idle =
            self.lines.as_ref().is_none_or(UnboundedReceiver::is_empty) && self.load.is_none();
        let now = Instant::now();
        let current = CountOffset::of(
            &status.node_name,
            status.counts.service_states(),
            self.store.service_states(),
            now,
        );
        if idle {
            self.recheck_counts(&current);
        }
        let confirm = STALL_CONFIRM.min(self.tuning.status_interval);
        let Some(verify) = &mut self.verify else {
            // A poll whose counts the quiet watch found moving without a
            // line isn't a reference.
            if idle && self.stall_suspect.is_none() && !self.counts_untrusted {
                // The latest poll a line followed stays a reference
                // however long the stream is silent since.
                if let Some(last_line) = self.conn.as_ref().map(|conn| conn.last_line)
                    && let Some(heard) = self
                        .count_offsets
                        .iter()
                        .rev()
                        .find(|offset| offset.at <= last_line)
                {
                    self.count_heard = Some(heard.clone());
                }
                self.count_offsets.push_back(current);
                while self.count_offsets.len() > OFFSETS_KEPT {
                    self.count_offsets.pop_front();
                }
            }
            return;
        };
        let next = if sent < verify.since {
            // Sent before the switch ended: ask again at once.
            Some(now)
        } else {
            let verdict = if verify
                .references
                .iter()
                .any(|reference| reference.same(&current))
            {
                Verdict::Fine
            } else if !idle
                || !verify
                    .references
                    .iter()
                    .any(|reference| reference.node == current.node)
            {
                Verdict::Unsure
            } else if verify
                .suspect
                .as_ref()
                .is_some_and(|suspect| suspect.same(&current))
            {
                Verdict::Missed
            } else {
                verify.suspect = Some(current);
                Verdict::Unsure
            };
            match verdict {
                Verdict::Fine => {
                    tracing::debug!("the switch missed nothing");
                    self.verify = None;
                    None
                }
                Verdict::Missed => {
                    let references = std::mem::take(&mut verify.references);
                    self.verify = None;
                    self.missed_in_switch(references);
                    None
                }
                Verdict::Unsure => {
                    verify.tries += 1;
                    if verify.tries < VERIFY_TRIES {
                        Some(now + confirm)
                    } else {
                        let suspected = verify.suspect.is_some();
                        let references = std::mem::take(&mut verify.references);
                        self.verify = None;
                        if suspected {
                            self.missed_in_switch(references);
                        } else {
                            tracing::debug!(
                                "the switch couldn't be checked (another node answers)"
                            );
                        }
                        None
                    }
                }
            }
        };
        if let Some(at) = next
            && let Some(conn) = &mut self.conn
            && let Some(planned) = &mut conn.next_status
        {
            *planned = (*planned).min(at);
        }
    }

    /// The old stream of a switch withheld state changes: they come with a
    /// reload, and the stream counts as interrupted (the reconcile's
    /// stretch starts over).
    fn missed_in_switch(&mut self, references: Vec<CountOffset>) {
        tracing::info!(
            "Icinga's state counts moved beyond what the streams brought during a switch; reloading"
        );
        let now = Instant::now();
        self.continuous_since = Some(now);
        self.reconcile_streak = 0;
        self.recheck = Some(Recheck {
            references,
            asked: now,
        });
        self.request_reload(ReloadCause::Reconnect);
    }

    /// Once the reload a switch's check asked for is in: the counts match
    /// the references again, or they can't be trusted (objects the API
    /// user doesn't see change Icinga's counts too).
    fn recheck_counts(&mut self, current: &CountOffset) {
        let Some(recheck) = &self.recheck else {
            return;
        };
        if self.last_load_end.is_none_or(|end| end <= recheck.asked) {
            return;
        }
        if recheck
            .references
            .iter()
            .any(|reference| reference.same(current))
        {
            self.recheck = None;
        } else if recheck
            .references
            .iter()
            .any(|reference| reference.node == current.node)
        {
            tracing::info!(
                "Icinga's state counts include objects this API user doesn't see; switches of the stream aren't checked against them"
            );
            self.recheck = None;
            self.counts_untrusted = true;
            self.count_offsets.clear();
            self.count_heard = None;
        }
    }

    // --- a quiet stream that stalls ----------------------------------------------

    /// Compares a quiet status poll's counts with the previous poll's (see
    /// the module notes): a stall is suspected, or a reload asked for.
    pub(super) fn watch_quiet_stream(&mut self, status: &InstanceStatus) {
        let now = Instant::now();
        let Some(conn) = &self.conn else {
            return;
        };
        if !conn.quiet || self.lines.is_none() {
            self.quiet_counts = None;
            return;
        }
        let counts = status.counts.service_states();
        let previous = self.quiet_counts.replace(super::QuietCounts {
            node: status.node_name.clone(),
            counts,
            at: now,
        });
        let Some(previous) = previous.filter(|previous| previous.node == status.node_name) else {
            return;
        };
        if previous.counts == counts || conn.last_line > previous.at {
            // Nothing changed, or events came: the stream was heard.
            self.last_heard = Some((now, self.ports.clock.now()));
            return;
        }
        if conn.since > previous.at {
            // Changed while the session was gone: the gap hid it.
            tracing::info!("Icinga's state counts changed while reconnecting; reloading");
            self.request_reload(ReloadCause::Reconnect);
        } else {
            tracing::debug!("Icinga's state counts changed but no event came; watching the stream");
            let confirm = STALL_CONFIRM.min(self.tuning.stall_after);
            self.stall_suspect = Some((now + confirm, previous.at));
        }
    }

    /// A suspected stall is due: still no line since the counts moved?
    pub(super) fn check_stall(&mut self) {
        let Some((_, since)) = self.stall_suspect.take() else {
            return;
        };
        let silent = self
            .conn
            .as_ref()
            .is_some_and(|conn| conn.last_line <= since)
            && self.lines.as_ref().is_some_and(UnboundedReceiver::is_empty);
        if silent {
            // Whatever it missed comes with a reload (the quiet polls kept
            // the stream heard, so the gap alone wouldn't reload).
            self.request_reload(ReloadCause::Reconnect);
            self.fail(Failure::Transient(
                "the event stream stalled: Icinga's state counts changed, but no event arrived"
                    .to_owned(),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use ic_model::Timestamp;

    use super::*;

    #[test]
    fn start_delays_scale_with_the_installation() {
        let per = Duration::from_secs(3);
        let max = Duration::from_secs(90);
        for _ in 0..1_000 {
            assert!(start_delay(Some(200), per, max) < Duration::from_millis(600));
            assert!(start_delay(Some(3_000), per, max) < Duration::from_secs(9));
            assert!(start_delay(Some(30_000), per, max) < Duration::from_secs(90));
            assert!(start_delay(Some(300_000), per, max) < Duration::from_secs(90));
            assert!(start_delay(None, per, max) < Duration::from_secs(90));
        }
        assert_eq!(start_delay(Some(0), per, max), Duration::ZERO);
        // Spread over the whole range.
        let samples: Vec<Duration> = (0..1_000)
            .map(|_| start_delay(Some(30_000), per, max))
            .collect();
        assert!(samples.iter().any(|delay| *delay > Duration::from_secs(80)));
        assert!(samples.iter().any(|delay| *delay < Duration::from_secs(10)));
    }

    #[test]
    fn a_result_is_due_one_interval_after_it() {
        let mut check = CheckInfo {
            check_interval: 300.0,
            retry_interval: 60.0,
            ..CheckInfo::default()
        };
        check.features.active_checks = true;
        let state = CheckableState::Service(ic_model::ServiceState::Critical);
        assert_eq!(result_due(state, &check), None, "no result");
        check.result = Some(ic_model::CheckResult {
            execution_end: Timestamp::from_unix_seconds(1_000.0),
            ..ic_model::CheckResult::default()
        });
        check.state_type = StateType::Hard;
        assert_eq!(result_due(state, &check), Some(1_300.0));
        check.state_type = StateType::Soft;
        assert_eq!(result_due(state, &check), Some(1_060.0), "retrying");
        check.check_interval = 0.0;
        check.state_type = StateType::Hard;
        assert_eq!(result_due(state, &check), Some(f64::INFINITY));
    }

    #[test]
    fn the_same_line_has_the_same_hash() {
        assert_eq!(line_hash(b"{\"a\":1}"), line_hash(b"{\"a\":1}"));
        assert_ne!(line_hash(b"{\"a\":1}"), line_hash(b"{\"a\":2}"));
    }
}
