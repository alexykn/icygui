//! The cluster health page's requests (topic 06, [`Command::WatchHealth`]):
//! what they cost, and when.
//!
//! The page reads the status poll the engine already makes and the
//! endpoints' numbers that come with the cluster nodes' states. Two things
//! need requests of their own: the node's `ApiListener` status (its queues
//! and connections, one request) and the node's features (three small
//! object queries). While the page shows this environment and it isn't
//! quiet, the listener comes with each status poll, and the features when
//! the page opens, every [`FEATURES_INTERVAL`] and after a restart.
//! Opening the page asks for both at once unless a poll brought the
//! listener less than a poll interval ago, so toggling the page never
//! sends more than the polls would. Otherwise the trouble alerts still
//! need them (a feature turned off, a growing relay queue): both come with
//! a status poll every [`TROUBLE_INTERVAL`] (PLAN.md §4.2 E2). Each
//! request takes a token from the request budget. A refused one
//! (`Forbidden`) isn't asked for again in the session.
//!
//! [`Command::WatchHealth`]: crate::Command::WatchHealth

use std::time::Duration;

use ic_api::{ApiError, Client};
use ic_model::{ListenerStatus, NodeFeatures};
use tokio::time::Instant;

use super::{Engine, Internal, Phase};

/// How often the page asks for the node's features while it is open.
const FEATURES_INTERVAL: Duration = Duration::from_mins(5);

/// How often the listener status and the features are asked for while no
/// page shows the environment: the trouble alerts need them (PLAN.md
/// §4.2 E2).
const TROUBLE_INTERVAL: Duration = Duration::from_mins(5);

/// What one round of the page's requests asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Ask {
    /// The `ApiListener` status.
    listener: bool,
    /// The node's features.
    features: bool,
}

/// Their answers.
#[derive(Debug)]
pub(crate) struct Answers {
    listener: Option<Result<ListenerStatus, ApiError>>,
    features: Option<Result<NodeFeatures, ApiError>>,
}

/// The page's requests in one session.
#[derive(Debug, Default)]
pub(super) struct Asked {
    /// When the listener status was last asked for.
    listener_at: Option<Instant>,
    /// When the features were last asked for (`None`: due).
    pub(super) features_at: Option<Instant>,
    /// Icinga refused them: not asked for again in the session.
    listener_refused: bool,
    features_refused: bool,
    /// The round sent when the page opened is out.
    pub(super) in_flight: bool,
}

/// Sends `ask`'s requests one after the other (each takes a token from the
/// client's request budget).
pub(super) async fn fetch(client: &Client, ask: Ask) -> Answers {
    let listener = if ask.listener {
        Some(client.listener_status().await)
    } else {
        None
    };
    let features = if ask.features {
        Some(client.node_features().await)
    } else {
        None
    };
    Answers { listener, features }
}

impl Engine {
    /// `Command::WatchHealth`.
    pub(super) fn watch_health(&mut self, watch: bool) {
        if self.health_watch == watch {
            return;
        }
        tracing::debug!(watch, "cluster health page");
        self.health_watch = watch;
        if !watch {
            return;
        }
        // The page opened: what it needs at once, unless a poll brought it
        // a moment ago or one is on its way.
        let interval = self.tuning.status_interval;
        let Some(conn) = &self.conn else {
            return;
        };
        if conn.status_in_flight
            || conn.health.in_flight
            || conn
                .health
                .listener_at
                .is_some_and(|at| at.elapsed() < interval)
        {
            return;
        }
        let Some(ask) = self.health_ask(Instant::now()) else {
            return;
        };
        let Some(conn) = &mut self.conn else {
            return;
        };
        conn.health.in_flight = true;
        let client = conn.client.clone();
        let tx = self.internal_tx.clone();
        let session = self.session;
        self.tasks.spawn(async move {
            let answers = Box::new(fetch(&client, ask).await);
            let _ = tx.send(Internal::Health { session, answers });
        });
    }

    /// What a round of the page's requests sent now asks for (marking it
    /// asked), or `None` when nothing is due or allowed, or the engine
    /// isn't live. While the page shows this environment (and it isn't
    /// quiet) the listener comes with every poll; else both every
    /// [`TROUBLE_INTERVAL`], for the trouble alerts.
    pub(super) fn health_ask(&mut self, now: Instant) -> Option<Ask> {
        if self.phase != Phase::Live {
            return None;
        }
        let watched = self.health_watch && !self.quiet();
        let conn = self.conn.as_mut()?;
        conn.next_status?;
        let listener = !conn.health.listener_refused
            && (watched
                || conn
                    .health
                    .listener_at
                    .is_none_or(|at| now.saturating_duration_since(at) >= TROUBLE_INTERVAL));
        let features = !conn.health.features_refused
            && conn
                .health
                .features_at
                .is_none_or(|at| now.saturating_duration_since(at) >= FEATURES_INTERVAL);
        if !listener && !features {
            return None;
        }
        if listener {
            conn.health.listener_at = Some(now);
        }
        if features {
            conn.health.features_at = Some(now);
        }
        Some(Ask { listener, features })
    }

    /// The page's answers are in: the store keeps them. Returns the
    /// listener status, for the trend of the poll it came with.
    pub(super) fn on_health(&mut self, answers: Answers) -> Option<ListenerStatus> {
        let now = self.ports.clock.now();
        let mut listener = None;
        match answers.listener {
            Some(Ok(status)) => {
                listener = Some(status.clone());
                self.store.update_health(|health| {
                    health.listener = Some(status);
                    health.listener_at = Some(now);
                });
            }
            Some(Err(ApiError::Forbidden(message))) => {
                tracing::warn!(%message, "the API user may not read the listener status; not asking again");
                if let Some(conn) = &mut self.conn {
                    conn.health.listener_refused = true;
                }
            }
            Some(Err(error)) => tracing::debug!(%error, "the listener status couldn't be read"),
            None => {}
        }
        match answers.features {
            Some(Ok(features)) => self
                .store
                .update_health(|health| health.features = Some(features)),
            Some(Err(ApiError::Forbidden(message))) => {
                tracing::warn!(%message, "the API user may not read the features; not asking again");
                if let Some(conn) = &mut self.conn {
                    conn.health.features_refused = true;
                }
            }
            Some(Err(error)) => {
                tracing::debug!(%error, "the features couldn't be read");
                // Asked for again with the next poll.
                if let Some(conn) = &mut self.conn {
                    conn.health.features_at = None;
                }
            }
            None => {}
        }
        listener
    }
}
