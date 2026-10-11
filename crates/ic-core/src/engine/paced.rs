//! Adaptive pacing in the engine (PLAN.md §4.2 1b; the rules are in
//! `pace.rs`): the gate before a paced load, the probe it sends, the
//! batches' parameters, what is kept across runs, the team's size, the
//! staggered reopening of the event stream after a lost session, and the
//! reconcile as a safety net (the status poll's object counts compared
//! with the store's).
//!
//! **Paced** (installations of `Tuning::pace_from` services or more): a
//! background start's first load (after its delay), the reload after a
//! restart, a long gap or a switch to another view, the periodic safety
//! pass, and a reload a count mismatch asks for. Each waits at its gate,
//! then loads its services in batches. **Never paced:** what the user
//! does: opening an object, actions, `Refresh` (and the reload it starts
//! after a failure), the hydration of the rows on screen; and the window's
//! first load (`Start::User`), which loads in batches when it has a key
//! list (the problems first, four in flight to begin with) but never
//! waits at a gate. There is no "master busy" state: a paced load waits
//! behind the same states as before (the background start's *loading*, or
//! nothing at all for a reload of a store that is complete already).
//!
//! **The reconcile as a safety net:** the engine learns new and deleted
//! objects from `ObjectCreated`/`ObjectDeleted` events, reloads after a
//! restart and a long gap, and every status poll now compares Icinga's
//! host and service counts (`/v1/status/CIB`) with the store's. A
//! difference that the next poll (10 s later) confirms while nothing is
//! on its way reloads that type, paced (hosts: tier 1; services: every
//! tier), and is logged and counted. The periodic reconcile becomes a
//! rare safety pass (`Tuning::safety_interval`, 3 hours; the random phase
//! and ±20 % of every interval). A short gap (a reconnect that didn't
//! reload) still plans a reconcile at the adaptive interval, as before.
//! Where the counts can't be used (no `status/query`; counts that still
//! differ after the reload they asked for: objects the API user doesn't
//! see) or the user set an interval, the adaptive interval stays.

use std::sync::Arc;
use std::time::Duration;

use ic_api::{ApiError, Client};
use ic_model::{InstanceStatus, ServiceKey};
use tokio::time::Instant;

use super::load::PaceLoad;
use super::pace::{self, Baselines, Gate, Signal, Verdict};
use super::{Engine, Internal, LoadKind, Phase};
use crate::event_log::RememberedPacing;
use crate::spec::Start;

/// What a load reloads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Everything (tiers 1–3).
    #[default]
    All,
    /// Tier 1 only: a count mismatch of the hosts alone.
    Hosts,
}

/// A count mismatch is acted on when the next poll, this soon, finds the
/// same.
const COUNT_CONFIRM: Duration = Duration::from_secs(10);

/// A status poll this recent that was slow lets a gate wait at once,
/// without a probe of its own.
const STATUS_FRESH: Duration = Duration::from_secs(15);

/// The status poll's object counts against the store's (see the module
/// notes).
#[derive(Debug)]
pub(crate) struct CountCheck {
    /// Icinga's hosts and services count, and the store's, at the first
    /// poll that found them different.
    suspect: Option<[usize; 4]>,
    /// A reload a mismatch asked for (what it reloads, when): if the next
    /// poll after it still finds a difference, the counts can't be used.
    recheck: Option<(Scope, Instant)>,
    /// The counts can be used for hosts and for services.
    trusted: (bool, bool),
    /// Mismatches acted on this run (instrumentation: PLAN.md §4.2,
    /// every catch logged and counted).
    pub(crate) catches: u64,
}

impl Default for CountCheck {
    fn default() -> Self {
        Self {
            suspect: None,
            recheck: None,
            trusted: (true, true),
            catches: 0,
        }
    }
}

/// A gate before a paced load, and the load it lets through.
#[derive(Debug)]
pub(crate) struct GateRun {
    pub(crate) gate: Gate,
    pub(crate) kind: LoadKind,
    pub(crate) scope: Scope,
    /// When it was opened (for the log).
    pub(crate) since: Instant,
}

/// What a probe found.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ProbeAnswer {
    /// It took this long.
    Took(Duration),
    /// The target can't be probed (gone, or not allowed): another one next
    /// time; says nothing about the master.
    Refused,
    /// It failed (a timeout, a server error): the master is busy, or worse.
    Failed(Duration),
    /// Nothing may be probed (no object, no status): no signal, so the
    /// load goes.
    Blind,
}

/// Adaptive pacing's state in the engine.
#[derive(Debug, Default)]
pub(crate) struct Pacing {
    pub(crate) baselines: Baselines,
    pub(crate) gate: Option<GateRun>,
    /// Icinga's `api_num_http_clients`, last seen (kept across runs), and
    /// the value last kept in the event log.
    pub(crate) team: Option<u32>,
    pub(crate) team_kept: Option<u32>,
    /// The services' keys of the previous run's last complete load, with
    /// whether each was a problem.
    pub(crate) remembered: Arc<Vec<(ServiceKey, bool)>>,
    /// Counts the reads of what the event log kept.
    pub(crate) generation: u64,
    /// Icinga's service count read by a background start before its first
    /// load (the store is empty then).
    pub(crate) size_hint: Option<u32>,
    /// When the last status poll was slow.
    pub(crate) status_slow_at: Option<Instant>,
    /// The next load a user asked for: never paced.
    pub(crate) unpaced_next: bool,
    /// The scope of the next reconcile (a count mismatch's).
    pub(crate) next_scope: Scope,
    /// Probe targets refused this session: the endpoint, a host.
    pub(crate) refused: (bool, bool),
    pub(crate) count: CountCheck,
    /// Paced loads, and how long their gates waited in all (instrumentation).
    pub(crate) gated: u64,
    pub(crate) gate_waited: Duration,
}

/// Where the probe goes.
#[derive(Clone, Debug)]
enum Target {
    /// An object by name: `endpoints` (the connected node) or `hosts`.
    Object(&'static str, String),
    /// The status (when no object may be asked for).
    Status,
}

impl Engine {
    /// When an answer is slow (`Tuning`).
    pub(super) fn pace_rules(&self) -> pace::Rules {
        pace::Rules {
            factor_percent: self.tuning.pace_factor_percent,
            floor: self.tuning.pace_floor,
            unknown: self.tuning.pace_unknown,
        }
    }

    /// The installation's size as far as known: the store's services,
    /// Icinga's count (the last status, or a background start's), the
    /// previous run's keys.
    pub(super) fn known_services(&self) -> usize {
        let status = self
            .store
            .status()
            .map_or(0, |status| status.counts.services() as usize);
        self.store
            .services()
            .len()
            .max(status)
            .max(self.pacing.size_hint.unwrap_or(0) as usize)
            .max(self.pacing.remembered.len())
    }

    /// Whether this installation is paced (`Tuning::pace_from`).
    pub(super) fn paces(&self) -> bool {
        self.tuning.pace_from == 0 || self.known_services() >= self.tuning.pace_from as usize
    }

    /// The URL the connection talks to (the baselines are per URL).
    fn pace_url(&self) -> Option<String> {
        self.conn
            .as_ref()
            .map(|conn| conn.client.base_url().to_string())
    }

    /// Starts a load of `kind`, or opens the gate before it when it is a
    /// paced one (see the module notes).
    pub(super) fn begin_load(&mut self, kind: LoadKind) {
        if self.load.is_some() || self.pacing.gate.is_some() {
            return;
        }
        let user = self.pacing.unpaced_next || self.reload_by_user;
        let gated = match kind {
            LoadKind::Reload | LoadKind::Reconcile => !user,
            LoadKind::First => self.start == Start::Background,
            LoadKind::Refresh => false,
        };
        let scope = if kind == LoadKind::Reconcile {
            std::mem::take(&mut self.pacing.next_scope)
        } else {
            Scope::All
        };
        if !gated || !self.paces() {
            self.load_scope = scope;
            self.start_load(kind);
            return;
        }
        let now = Instant::now();
        // A background start waited its delay already.
        let first_probe = if kind == LoadKind::First {
            now
        } else {
            now + self.jitter.up_to(self.tuning.pace_probe_jitter)
        };
        let mut gate = Gate::new(first_probe);
        gate.busy_from_status = self
            .pacing
            .status_slow_at
            .is_some_and(|at| now.saturating_duration_since(at) < STATUS_FRESH);
        tracing::debug!(?kind, ?scope, "a paced load waits at its gate");
        self.pacing.gate = Some(GateRun {
            gate,
            kind,
            scope,
            since: now,
        });
    }

    /// When the gate probes next.
    pub(super) fn gate_due(&self) -> Option<Instant> {
        if !matches!(self.phase, Phase::Live | Phase::Loading) {
            return None;
        }
        let run = self.pacing.gate.as_ref()?;
        (!run.gate.in_flight).then_some(run.gate.probe_at)
    }

    /// The gate's turn: a verdict from a slow status poll, or a probe.
    pub(super) fn run_gate(&mut self) {
        let Some(run) = &mut self.pacing.gate else {
            return;
        };
        if std::mem::take(&mut run.gate.busy_from_status) {
            tracing::debug!("the status poll was slow: the paced load waits");
            self.gate_verdict(true);
            return;
        }
        let Some(conn) = &self.conn else {
            return;
        };
        let target = if !self.pacing.refused.0 && conn.info.allows("objects/query/Endpoint") {
            Target::Object("endpoints", conn.node.name.clone())
        } else if !self.pacing.refused.1
            && conn.info.allows("objects/query/Host")
            && let Some(host) = self.store.hosts().keys().next()
        {
            Target::Object("hosts", host.to_string())
        } else {
            Target::Status
        };
        run.gate.in_flight = true;
        let client = conn.client.without_budget();
        let tx = self.internal_tx.clone();
        let session = self.session;
        self.tasks.spawn(async move {
            let answer = probe(&client, &target).await;
            let refused = matches!(answer, ProbeAnswer::Refused).then_some(target);
            let _ = tx.send(Internal::LoadProbe {
                session,
                answer,
                endpoint: refused
                    .as_ref()
                    .is_some_and(|target| matches!(target, Target::Object("endpoints", _))),
            });
        });
    }

    /// A probe of the gate is back.
    pub(super) fn on_load_probe(&mut self, answer: ProbeAnswer, endpoint_refused: bool) {
        let url = self.pace_url();
        let rules = self.pace_rules();
        let Some(run) = &mut self.pacing.gate else {
            return;
        };
        run.gate.in_flight = false;
        let slow = match answer {
            ProbeAnswer::Took(took) => {
                let baseline = url
                    .as_deref()
                    .and_then(|url| self.pacing.baselines.get(url, Signal::Probe));
                run.gate.probed(took);
                if let Some(url) = &url {
                    self.pacing.baselines.observe(url, Signal::Probe, took);
                }
                let slow = pace::slow(rules, baseline, took);
                tracing::debug!(
                    ?took,
                    ?baseline,
                    slow,
                    "probed the master before a paced load"
                );
                slow
            }
            ProbeAnswer::Refused => {
                if endpoint_refused {
                    self.pacing.refused.0 = true;
                } else {
                    self.pacing.refused.1 = true;
                }
                // Probe again at once with the next target.
                run.gate.probe_at = Instant::now();
                return;
            }
            ProbeAnswer::Failed(took) => {
                run.gate.probes += 1;
                tracing::debug!(?took, "the probe before a paced load failed");
                true
            }
            ProbeAnswer::Blind => false,
        };
        self.gate_verdict(slow);
    }

    /// Goes or waits after a probe found the master `slow` (or not).
    fn gate_verdict(&mut self, slow: bool) {
        let unit = self.jitter.unit();
        let (initial, cap) = (self.tuning.pace_wait_initial, self.tuning.pace_wait_max);
        let url = self.pace_url();
        let Some(run) = &mut self.pacing.gate else {
            return;
        };
        match run.gate.decide(slow, unit, initial, cap) {
            Verdict::Wait(wait) => {
                tracing::debug!(?wait, waited = ?run.gate.waited(), "the master answers slowly: the paced load waits");
                run.gate.probe_at = Instant::now() + wait;
            }
            Verdict::Go => {
                let Some(run) = self.pacing.gate.take() else {
                    return;
                };
                let waited = run.gate.waited();
                if run.gate.capped(cap)
                    && let (Some(fastest), Some(url)) = (run.gate.fastest, &url)
                {
                    // The master answered this slowly for the whole cap:
                    // its normal now (a slower path, or a master that stays
                    // this busy).
                    self.pacing.baselines.settle(url, Signal::Probe, fastest);
                }
                self.pacing.gated += 1;
                self.pacing.gate_waited += waited;
                tracing::info!(
                    environment = %self.spec.environment.name,
                    kind = ?run.kind,
                    scope = ?run.scope,
                    waited_ms = waited.as_millis(),
                    open_ms = run.since.elapsed().as_millis(),
                    probes = run.gate.probes,
                    capped = run.gate.capped(cap),
                    "a paced load goes"
                );
                self.load_scope = run.scope;
                self.start_load(run.kind);
            }
        }
    }

    /// How a load of `kind` asks for its services: in paced batches (with
    /// the keys known, the problems first), or `None` for one list
    /// (`Refresh`, a reload the user asked for; small installations decide
    /// in the load, by Icinga's count).
    pub(super) fn pace_load(&self, kind: LoadKind, user: bool) -> Option<PaceLoad> {
        let conn = self.conn.as_ref()?;
        if kind == LoadKind::Refresh || (user && kind != LoadKind::First) {
            return None;
        }
        let services = self.store.services();
        let (mut keys, problems): (Vec<ServiceKey>, usize) = if services.is_empty() {
            let mut problems: Vec<ServiceKey> = Vec::new();
            let mut rest = Vec::new();
            for (key, problem) in self.pacing.remembered.iter() {
                if *problem {
                    problems.push(key.clone());
                } else {
                    rest.push(key.clone());
                }
            }
            let count = problems.len();
            problems.extend(rest);
            (problems, count)
        } else {
            let mut problems: Vec<ServiceKey> = services
                .iter()
                .filter(|(_, service)| service.is_problem())
                .map(|(key, _)| key.clone())
                .collect();
            let count = problems.len();
            problems.extend(
                services
                    .iter()
                    .filter(|(_, service)| !service.is_problem())
                    .map(|(key, _)| key.clone()),
            );
            (problems, count)
        };
        keys.dedup();
        let window = if kind == LoadKind::First && self.start == Start::User {
            self.tuning.batch_window_user
        } else {
            1
        };
        let url = conn.client.base_url().to_string();
        Some(PaceLoad {
            client: conn.client.without_budget(),
            keys,
            problems,
            from: self.tuning.pace_from,
            window,
            max_window: self.tuning.batch_window_max,
            rules: self.pace_rules(),
            baseline: self.pacing.baselines.get(&url, Signal::Batch),
        })
    }

    /// A paced batch answered after `took`.
    pub(super) fn batch_answered(&mut self, took: Duration) {
        if let Some(url) = self.pace_url() {
            self.pacing.baselines.observe(&url, Signal::Batch, took);
        }
    }

    /// A status poll took `took`: its own signal, for free.
    pub(super) fn status_answered(&mut self, took: Duration) {
        let Some(url) = self.pace_url() else {
            return;
        };
        let baseline = self.pacing.baselines.get(&url, Signal::Status);
        if pace::slow(self.pace_rules(), baseline, took) {
            tracing::debug!(?took, ?baseline, "the status poll was slow");
            self.pacing.status_slow_at = Some(Instant::now());
        }
        self.pacing.baselines.observe(&url, Signal::Status, took);
    }

    // --- kept across runs ---------------------------------------------------------

    /// Reads what the event log kept.
    pub(super) fn read_remembered_pacing(&mut self) {
        self.pacing.generation += 1;
        let generation = self.pacing.generation;
        let tx = self.internal_tx.clone();
        self.event_log.pacing(Box::new(move |pacing| {
            let _ = tx.send(Internal::RememberedPacing {
                generation,
                pacing: Box::new(pacing),
            });
        }));
    }

    /// The event log answered.
    pub(super) fn on_remembered_pacing(&mut self, generation: u64, pacing: RememberedPacing) {
        if generation != self.pacing.generation {
            return;
        }
        let RememberedPacing {
            keys,
            baselines,
            team,
        } = pacing;
        tracing::debug!(
            keys = keys.len(),
            baselines = baselines.len(),
            ?team,
            "adaptive pacing's state from the last run"
        );
        self.pacing.remembered = Arc::new(keys);
        self.pacing.baselines.remember(
            baselines
                .into_iter()
                .filter_map(|(url, signal, took)| Some((url, Signal::from_name(&signal)?, took))),
        );
        // A count seen this run is newer.
        if self.pacing.team.is_none() {
            self.pacing.team = team;
        }
        if self.pacing.team_kept.is_none() {
            self.pacing.team_kept = team;
        }
    }

    /// A complete load: its keys (for the next run's first load) and the
    /// baselines go to the event log.
    pub(super) fn remember_pacing(&mut self) {
        let services = self.store.services();
        if !services.is_empty()
            && (self.tuning.pace_from == 0 || services.len() >= self.tuning.pace_from as usize)
        {
            let keys: Vec<(ServiceKey, bool)> = services
                .iter()
                .map(|(key, service)| (key.clone(), service.is_problem()))
                .collect();
            self.event_log.remember_keys(keys);
        }
        if let Some(baselines) = self.pacing.baselines.take_dirty() {
            self.event_log.remember_baselines(
                baselines
                    .into_iter()
                    .map(|(url, signal, took)| (url, signal.name().to_owned(), took))
                    .collect(),
            );
        }
    }

    /// Icinga reported `clients` open API connections (kept in the event
    /// log when it moved by more than a tenth: the count changes with every
    /// poll's connections).
    pub(super) fn note_team(&mut self, clients: u32) {
        self.pacing.team = Some(clients);
        let moved = self
            .pacing
            .team_kept
            .is_none_or(|kept| kept.abs_diff(clients).saturating_mul(10) > kept);
        if moved {
            self.pacing.team_kept = Some(clients);
            self.event_log.remember_team(clients);
        }
    }

    /// How long a background start's window may be, for the team's size.
    pub(super) fn team_factor(&self) -> f64 {
        let base = self.tuning.start_team_base.max(1);
        (f64::from(self.pacing.team.unwrap_or(0)) / f64::from(base)).max(1.0)
    }

    /// The delay before the next event stream opens after a live session
    /// was lost (ledger 154: every client loses it at once when the master
    /// restarts, and fifty streams opening together keep it busy): random,
    /// up to a spread that grows with the team; none for installations
    /// that aren't paced.
    pub(super) fn reopen_delay(&mut self) -> Duration {
        if !self.paces() {
            return Duration::ZERO;
        }
        let spread = pace::team_spread(
            self.pacing.team,
            self.tuning.reopen_per_client,
            self.tuning.reopen_spread_min,
            self.tuning.reopen_spread_max,
        );
        self.jitter.up_to(spread)
    }

    // --- the reconcile as a safety net ----------------------------------------------

    /// Whether the count check stands in for the periodic reconcile (see
    /// the module notes).
    pub(super) fn safety_net(&self) -> bool {
        self.tuning.reconcile_interval.is_none()
            && self.spec.general.reconcile_interval_secs == 0
            && self.pacing.count.trusted == (true, true)
            && self
                .conn
                .as_ref()
                .is_none_or(|conn| conn.next_status.is_some())
    }

    /// Compares a status poll's object counts with the store's (see the
    /// module notes).
    pub(super) fn check_object_counts(&mut self, status: &InstanceStatus) {
        let idle = self
            .lines
            .as_ref()
            .is_none_or(tokio::sync::mpsc::UnboundedReceiver::is_empty)
            && self.load.is_none()
            && self.pacing.gate.is_none()
            && self.fetch.idle()
            && self.store.hidden_count() == 0;
        if !idle {
            return;
        }
        let counts = [
            status.counts.hosts() as usize,
            self.store.hosts().len(),
            status.counts.services() as usize,
            self.store.services().len(),
        ];
        let hosts_differ = counts[0] != counts[1];
        let services_differ = counts[2] != counts[3];
        let now = Instant::now();
        let check = &mut self.pacing.count;
        if let Some((scope, asked)) = check.recheck
            && self.last_load_end.is_some_and(|end| end > asked)
        {
            check.recheck = None;
            let still = match scope {
                Scope::Hosts => hosts_differ,
                Scope::All => hosts_differ || services_differ,
            };
            if still {
                tracing::info!(
                    icinga_hosts = counts[0],
                    store_hosts = counts[1],
                    icinga_services = counts[2],
                    store_services = counts[3],
                    "Icinga's object counts include objects this API user doesn't see; the reconcile keeps its interval"
                );
                check.trusted = (
                    check.trusted.0 && !hosts_differ,
                    check.trusted.1 && !services_differ,
                );
                check.suspect = None;
                self.replan_reconcile();
                return;
            }
        }
        if check.trusted != (true, true) || !(hosts_differ || services_differ) {
            check.suspect = None;
            return;
        }
        if check.suspect != Some(counts) {
            // Something may still be on its way: the next poll decides.
            check.suspect = Some(counts);
            if let Some(conn) = &mut self.conn
                && let Some(next) = &mut conn.next_status
            {
                *next = (*next).min(now + COUNT_CONFIRM);
            }
            return;
        }
        check.suspect = None;
        check.catches += 1;
        let scope = if services_differ {
            Scope::All
        } else {
            Scope::Hosts
        };
        check.recheck = Some((scope, now));
        tracing::info!(
            environment = %self.spec.environment.name,
            icinga_hosts = counts[0],
            store_hosts = counts[1],
            icinga_services = counts[2],
            store_services = counts[3],
            ?scope,
            catches = check.catches,
            "Icinga's object counts differ from the store's with nothing on its way; reloading"
        );
        self.pacing.next_scope = scope;
        self.reconcile_at = Some(now);
    }
}

/// Times one probe of `target`.
async fn probe(client: &Client, target: &Target) -> ProbeAnswer {
    let sent = Instant::now();
    let result = match target {
        Target::Object(plural, name) => client.probe(plural, name).await,
        Target::Status => client.status().await.map(|_| ()),
    };
    let took = sent.elapsed();
    match (result, target) {
        (Ok(()), _) => ProbeAnswer::Took(took),
        (Err(ApiError::NotFound(_) | ApiError::Forbidden(_)), Target::Object(..)) => {
            ProbeAnswer::Refused
        }
        (Err(ApiError::NotFound(_) | ApiError::Forbidden(_)), Target::Status) => ProbeAnswer::Blind,
        (Err(_), _) => ProbeAnswer::Failed(took),
    }
}
