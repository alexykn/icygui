//! The tiered load (docs/performance.md): on connect, periodically, and on
//! reloads (a reconnect after a long gap, a restart, `Refresh`). It never
//! asks for more than the lean lists plus the problems' details, and keeps
//! few requests in flight: the hosts next to one small query at a time,
//! then the services, then the problems' details one batch of names at a
//! time. Which problems' details the engine decides once it applied the
//! services (a periodic reconcile and `Refresh` skip those it holds
//! current).
//!
//! A paced load ([`PaceLoad`], installations of `Tuning::pace_from`
//! services or more) asks for the services by name in batches of
//! [`NAMES_PER_REQUEST`] instead of one answer of every service (about
//! 20 MB at 33 000), paced by their own answer times ([`super::pace::Aimd`]):
//! the keys the engine knows (the store's, or the previous run's from the
//! event log), the problems first. When it knows none, or Icinga's count
//! of services (tier 1's status) differs from them, it first asks for every
//! service's name (a sixth of the bytes) and batches those; a batch that
//! finds fewer than Icinga counts asks for the names afterwards. Answers
//! are applied batch by batch; once the names were listed, services the
//! list doesn't have are gone.

use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use ic_api::{ApiError, Client, Detail, NAMES_PER_REQUEST};
use ic_model::{ObjectKey, ServiceKey};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::pace::{self, Aimd};
use super::{Internal, LoadStep};
use crate::command::LoadPhase;
use crate::connect::Failure;
use crate::store::Overview;

/// The tier-1 queries counted in [`LoadPhase::Hosts`] progress.
pub(super) const OVERVIEW_QUERIES: usize = 8;

/// What a load task needs.
pub(super) struct LoadTask {
    pub(super) client: Client,
    /// The stream reader's line count (see the store's ordering notes).
    pub(super) seq: Arc<AtomicU64>,
    pub(super) tx: UnboundedSender<Internal>,
    pub(super) session: u64,
    pub(super) load: u64,
    /// Load the services in paced batches when there are enough of them.
    pub(super) pace: Option<PaceLoad>,
    /// Tier 1 only (a reload of the hosts a count mismatch asked for).
    pub(super) hosts_only: bool,
}

/// How a paced load asks for the services (see the module notes).
pub(super) struct PaceLoad {
    /// The client for the batches: outside the request budget (they pace
    /// themselves), sharing the connection pool.
    pub(super) client: Client,
    /// The services' keys the engine knows, the problems first.
    pub(super) keys: Vec<ServiceKey>,
    /// How many of `keys` (from the front) were problems.
    pub(super) problems: usize,
    /// Batches only from this many services on (Icinga's count, or the
    /// keys'); one list below.
    pub(super) from: u32,
    /// Batches in flight at first, and at most.
    pub(super) window: usize,
    pub(super) max_window: usize,
    /// When a batch's answer is slow, and the fastest batch seen at this
    /// URL so far.
    pub(super) rules: pace::Rules,
    pub(super) baseline: Option<Duration>,
}

/// How a paced load went (logged when it is done).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PaceStats {
    /// Batches answered, and how many of them were slow (or failed).
    pub(crate) batches: u32,
    pub(crate) slow: u32,
    /// The widest window it reached.
    pub(crate) widest: usize,
    /// Paused between batches in all.
    pub(crate) paused: Duration,
    /// Whether it asked for every service's name.
    pub(crate) listed: bool,
}

/// How tier 2 brought the services.
#[derive(Debug)]
pub(crate) enum ServicesAnswer {
    /// Every service in one answer, sent when the reader had read the
    /// count given.
    All(u64, Vec<ic_model::Service>),
    /// In batches, applied already; `listed`: the names of every service,
    /// when they were asked for (and when): services not among them are
    /// gone.
    Batched {
        listed: Option<(u64, HashSet<ServiceKey>)>,
        stats: PaceStats,
    },
}

/// A failed batch is asked again this many times before the load fails.
const BATCH_TRIES: u32 = 3;

impl LoadTask {
    fn send(&self, step: LoadStep) -> bool {
        self.tx
            .send(Internal::Load {
                session: self.session,
                load: self.load,
                step,
            })
            .is_ok()
    }

    fn started(&self) -> u64 {
        self.seq.load(Ordering::SeqCst)
    }

    fn progress(&self, phase: LoadPhase, done: usize, total: Option<usize>) -> bool {
        self.send(LoadStep::Progress { phase, done, total })
    }

    /// Runs tiers 1–3, reporting each to the engine, then `Done` (or
    /// `Failed`).
    pub(super) async fn run(self) {
        match Box::pin(self.tiers()).await {
            Ok(()) => {
                self.send(LoadStep::Done);
            }
            Err(failure) => {
                self.send(LoadStep::Failed(failure));
            }
        }
    }

    async fn tiers(&self) -> Result<(), Failure> {
        // Tier 1: everything small, and the hosts in full.
        let started = self.started();
        self.progress(LoadPhase::Hosts, 0, Some(OVERVIEW_QUERIES));
        let done = AtomicUsize::new(0);
        let (hosts, overview) = futures::join!(
            self.counted(&done, permitted("hosts", self.client.hosts())),
            self.overview(&done),
        );
        let hosts = hosts?;
        let overview = overview?;
        let icinga_services = overview
            .status
            .as_ref()
            .map(|status| status.counts.services());
        if !self.send(LoadStep::Overview {
            started,
            overview: Box::new(overview),
            hosts,
        }) {
            return Ok(());
        }

        if self.hosts_only {
            return Ok(());
        }

        // Tier 2: every service, lean: in one answer, or paced in batches.
        let services = match &self.pace {
            Some(pace) if paced(pace, icinga_services) => {
                Box::pin(self.batched(pace, icinga_services)).await?
            }
            _ => {
                let started = self.started();
                self.progress(LoadPhase::Services, 0, None);
                let services = permitted("services", self.client.services(Detail::Lean)).await?;
                ServicesAnswer::All(started, services)
            }
        };
        let (details, problems) = oneshot::channel();
        if !self.send(LoadStep::Services { services, details }) {
            return Ok(());
        }
        // The engine applied them and names the problems to detail (none
        // if the load was cut off meanwhile).
        let Ok(problems) = problems.await else {
            return Ok(());
        };

        // Tier 3: the problems' check results and links, by name.
        let total = problems.len();
        self.progress(LoadPhase::Details, 0, Some(total));
        let mut done = 0;
        for batch in problems.chunks(NAMES_PER_REQUEST) {
            let started = self.started();
            match self.client.objects(batch, Detail::Full).await {
                Ok(fetched) => {
                    done += batch.len();
                    if !self.send(LoadStep::Details { started, fetched }) {
                        return Ok(());
                    }
                    self.progress(LoadPhase::Details, done, Some(total));
                }
                // The states are complete without the details; events and
                // hydration bring the rest. A broken connection also ends
                // the stream, which reconnects.
                Err(ApiError::Unauthorized) => {
                    return Err(Failure::Auth(ApiError::Unauthorized.to_string()));
                }
                Err(error) => {
                    tracing::warn!(%error, "couldn't load the problems' details");
                    break;
                }
            }
        }
        Ok(())
    }

    /// Tier 2 in paced batches (see the module notes).
    async fn batched(
        &self,
        pace: &PaceLoad,
        icinga: Option<u32>,
    ) -> Result<ServicesAnswer, Failure> {
        let mut stats = PaceStats::default();
        let mut aimd = Aimd::new(pace.window, pace.max_window);
        let mut baseline = pace.baseline;
        let mut listed: Option<(u64, HashSet<ServiceKey>)> = None;
        let count_matches = icinga.is_some_and(|count| count as usize == pace.keys.len());
        let mut keys = if pace.keys.is_empty() || !count_matches {
            let (started, names) = self.list_names(pace, &mut stats).await?;
            let ordered = problems_first(pace, &names);
            listed = Some((started, names));
            ordered
        } else {
            pace.keys.clone()
        };
        let mut found = 0_usize;
        let mut asked: HashSet<ServiceKey> = keys.iter().cloned().collect();
        loop {
            let total = asked.len();
            found += self
                .run_batches(pace, &keys, total, &mut aimd, &mut baseline, &mut stats)
                .await?;
            // Fewer than Icinga counts (services created since the keys
            // were taken): their names, once.
            let short = icinga.is_some_and(|count| found < count as usize);
            if listed.is_some() || !short {
                break;
            }
            let (started, names) = self.list_names(pace, &mut stats).await?;
            keys = names
                .iter()
                .filter(|key| !asked.contains(*key))
                .cloned()
                .collect();
            asked.extend(keys.iter().cloned());
            listed = Some((started, names));
            if keys.is_empty() {
                break;
            }
        }
        Ok(ServicesAnswer::Batched { listed, stats })
    }

    /// Every service's name, and the reader's count when they were asked
    /// for.
    async fn list_names(
        &self,
        pace: &PaceLoad,
        stats: &mut PaceStats,
    ) -> Result<(u64, HashSet<ServiceKey>), Failure> {
        stats.listed = true;
        let started = self.started();
        self.progress(LoadPhase::Services, 0, None);
        let names = permitted("services", pace.client.service_names()).await?;
        Ok((started, names.into_iter().collect()))
    }

    /// Asks for `keys` in batches, paced by `aimd`, applying each answer;
    /// returns how many services they found.
    async fn run_batches(
        &self,
        pace: &PaceLoad,
        keys: &[ServiceKey],
        total: usize,
        aimd: &mut Aimd,
        baseline: &mut Option<Duration>,
        stats: &mut PaceStats,
    ) -> Result<usize, Failure> {
        let mut queue: VecDeque<(Vec<ObjectKey>, u32)> = keys
            .chunks(NAMES_PER_REQUEST)
            .map(|chunk| (chunk.iter().cloned().map(ObjectKey::from).collect(), 0))
            .collect();
        let mut flying = FuturesUnordered::new();
        let mut found = 0;
        let mut done = total.saturating_sub(keys.len());
        let mut next_send = Instant::now();
        self.progress(LoadPhase::Services, done, Some(total));
        loop {
            while flying.len() < aimd.allowed() && Instant::now() >= next_send {
                let Some((batch, tries)) = queue.pop_front() else {
                    break;
                };
                stats.widest = stats.widest.max(aimd.allowed());
                let client = pace.client.clone();
                let started = self.started();
                flying.push(async move {
                    let sent = Instant::now();
                    let result = client.objects(&batch, Detail::Lean).await;
                    (batch, tries, started, sent.elapsed(), result)
                });
                let gap = aimd.gap();
                if !gap.is_zero() {
                    stats.paused += gap;
                    next_send = Instant::now() + gap;
                }
            }
            if flying.is_empty() && queue.is_empty() {
                return Ok(found);
            }
            let next = tokio::select! {
                biased;
                answer = flying.next(), if !flying.is_empty() => answer,
                () = tokio::time::sleep_until(next_send), if !queue.is_empty() && flying.len() < aimd.allowed() => None,
            };
            let Some((batch, tries, started, took, result)) = next else {
                continue;
            };
            match result {
                Ok(fetched) => {
                    let slow = pace::slow(pace.rules, *baseline, took);
                    *baseline = Some(baseline.map_or(took, |fastest| fastest.min(took)));
                    aimd.answered(slow, took);
                    stats.batches += 1;
                    stats.slow += u32::from(slow);
                    found += fetched.services.len();
                    done += batch.len();
                    if !self.send(LoadStep::Batch {
                        started,
                        took,
                        fetched,
                    }) {
                        return Ok(found);
                    }
                    self.progress(LoadPhase::Services, done, Some(total));
                }
                Err(ApiError::Unauthorized) => {
                    return Err(Failure::Auth(ApiError::Unauthorized.to_string()));
                }
                Err(error) if error.is_transient() && tries + 1 < BATCH_TRIES => {
                    tracing::debug!(%error, "a batch of the load failed; asking again");
                    aimd.answered(true, took);
                    stats.slow += 1;
                    queue.push_front((batch, tries + 1));
                }
                Err(error) => return Err(failure(error)),
            }
        }
    }

    /// Tier 1 apart from the hosts, one query after the other.
    async fn overview(&self, done: &AtomicUsize) -> Result<Overview, Failure> {
        let cluster = Box::pin(self.counted(done, optional("endpoints", self.client.cluster())))
            .await?
            .unwrap_or_default();
        Ok(Overview {
            status: self
                .counted(done, optional("status", self.client.status()))
                .await?,
            host_groups: self
                .counted(done, permitted("host groups", self.client.host_groups()))
                .await?,
            service_groups: self
                .counted(
                    done,
                    permitted("service groups", self.client.service_groups()),
                )
                .await?,
            dependencies: self
                .counted(done, permitted("dependencies", self.client.dependencies()))
                .await?,
            endpoints: cluster.endpoints,
            zones: cluster.zones,
            comments: self
                .counted(done, permitted("comments", self.client.comments()))
                .await?,
            downtimes: self
                .counted(done, permitted("downtimes", self.client.downtimes()))
                .await?,
        })
    }

    /// Awaits `query` and reports tier-1 progress.
    async fn counted<T>(&self, done: &AtomicUsize, query: impl Future<Output = T>) -> T {
        let answer = query.await;
        let count = done.fetch_add(1, Ordering::SeqCst) + 1;
        self.progress(LoadPhase::Hosts, count, Some(OVERVIEW_QUERIES));
        answer
    }
}

/// Whether a paced load batches its services: Icinga counts at least
/// `from` (or, without its count, the engine knows that many).
fn paced(pace: &PaceLoad, icinga: Option<u32>) -> bool {
    let size = icinga.unwrap_or_else(|| u32::try_from(pace.keys.len()).unwrap_or(u32::MAX));
    size >= pace.from && size > 0
}

/// `names` in the order to ask for them: those that were problems first
/// (the window needs them first), then the rest.
fn problems_first(pace: &PaceLoad, names: &HashSet<ServiceKey>) -> Vec<ServiceKey> {
    let problems = &pace.keys[..pace.problems.min(pace.keys.len())];
    let first: HashSet<&ServiceKey> = problems.iter().filter(|key| names.contains(*key)).collect();
    let mut ordered: Vec<ServiceKey> = problems
        .iter()
        .filter(|key| names.contains(*key))
        .cloned()
        .collect();
    let mut rest: Vec<ServiceKey> = names
        .iter()
        .filter(|key| !first.contains(key))
        .cloned()
        .collect();
    rest.sort();
    ordered.extend(rest);
    ordered
}

/// A list query the API user may not be allowed: `Forbidden` (and the
/// generic 404 of a hidden type) is an empty list, logged once per load.
async fn permitted<T>(
    what: &str,
    query: impl Future<Output = Result<Vec<T>, ApiError>>,
) -> Result<Vec<T>, Failure> {
    match query.await {
        Ok(list) => Ok(list),
        Err(ApiError::Forbidden(message) | ApiError::NotFound(message)) => {
            tracing::warn!(%message, "the API user may not read {what}; leaving them out");
            Ok(Vec::new())
        }
        Err(error) => Err(failure(error)),
    }
}

/// A single query the API user may not be allowed (`None` then).
async fn optional<T>(
    what: &str,
    query: impl Future<Output = Result<T, ApiError>>,
) -> Result<Option<T>, Failure> {
    match query.await {
        Ok(value) => Ok(Some(value)),
        Err(ApiError::Forbidden(message) | ApiError::NotFound(message)) => {
            tracing::warn!(%message, "the API user may not read the {what}");
            Ok(None)
        }
        Err(error) => Err(failure(error)),
    }
}

/// Classifies an error of an established connection: credentials that
/// stopped working need the user, and so does an answer this client can't
/// use (unreadable, or a request Icinga refuses as such: 400 and the like),
/// which no retry changes; a connection problem, a timeout or a server
/// error (5xx, 408, 429) may go away, and a TLS problem shows up with the
/// certificate when reconnecting.
pub(super) fn failure(error: ApiError) -> Failure {
    match error {
        ApiError::Unauthorized => Failure::Auth(error.to_string()),
        ApiError::Tls(_) | ApiError::CertificateMismatch { .. } => {
            Failure::Transient(error.to_string())
        }
        error if error.is_transient() => Failure::Transient(error.to_string()),
        error => Failure::Misconfigured(format!(
            "Icinga's answer to a query can't be used ({error}); retrying won't help"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_may_go_away_is_retried() {
        let transient = [
            ApiError::Connect("reset".to_owned()),
            ApiError::Timeout,
            ApiError::Http {
                status: 502,
                message: "Bad Gateway".to_owned(),
            },
            ApiError::Http {
                status: 429,
                message: String::new(),
            },
            ApiError::Tls("unknown issuer".to_owned()),
        ];
        for error in transient {
            assert!(
                matches!(failure(error.clone()), Failure::Transient(_)),
                "{error}"
            );
        }
        assert!(matches!(failure(ApiError::Unauthorized), Failure::Auth(_)));
        let permanent = [
            ApiError::Decode("missing field `name`".to_owned()),
            ApiError::Http {
                status: 400,
                message: "Invalid attribute".to_owned(),
            },
            ApiError::InvalidSettings("bad URL".to_owned()),
        ];
        for error in permanent {
            assert!(
                matches!(failure(error.clone()), Failure::Misconfigured(_)),
                "{error}"
            );
        }
    }
}
