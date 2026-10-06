//! The tiered load (docs/performance.md): on connect, periodically, and on
//! reloads (a reconnect after a long gap, a restart, `Refresh`). It never
//! asks for more than the lean lists plus the problems' details, and keeps
//! few requests in flight: the hosts next to one small query at a time,
//! then the services, then the problems' details one batch of names at a
//! time. Which problems' details the engine decides once it applied the
//! services (a periodic reconcile and `Refresh` skip those it holds
//! current).

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ic_api::{ApiError, Client, Detail, NAMES_PER_REQUEST};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use super::{Internal, LoadStep};
use crate::command::LoadPhase;
use crate::connect::Failure;
use crate::store::Overview;

/// The tier-1 queries counted in [`LoadPhase::Hosts`] progress.
const OVERVIEW_QUERIES: usize = 8;

/// What a load task needs.
pub(super) struct LoadTask {
    pub(super) client: Client,
    /// The stream reader's line count (see the store's ordering notes).
    pub(super) seq: Arc<AtomicU64>,
    pub(super) tx: UnboundedSender<Internal>,
    pub(super) session: u64,
    pub(super) load: u64,
}

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
        if !self.send(LoadStep::Overview {
            started,
            overview: Box::new(overview),
            hosts,
        }) {
            return Ok(());
        }

        // Tier 2: every service, lean.
        let started = self.started();
        self.progress(LoadPhase::Services, 0, None);
        let services = permitted("services", self.client.services(Detail::Lean)).await?;
        let (details, problems) = oneshot::channel();
        if !self.send(LoadStep::Services {
            started,
            services,
            details,
        }) {
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
