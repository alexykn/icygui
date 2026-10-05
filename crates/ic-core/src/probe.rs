//! Settings-dialog helpers that need no running environment: "test
//! connection" and "read the server's certificate" (trust on first use).
//! They run on a small shared background runtime and answer through a
//! oneshot channel the UI can await.

use std::sync::OnceLock;

use futures::channel::oneshot;
use ic_api::{ApiError, ApiInfo, CertificateInfo, Client, Url, fetch_server_certificate};
use ic_model::InstanceStatus;
use secrecy::SecretString;

use crate::connect::{self, Failure, Password};

/// Every permission the client uses (PLAN.md §5): object queries, status,
/// the event types it subscribes to, and the runtime actions.
pub const REQUIRED_PERMISSIONS: &[&str] = &[
    "objects/query/Host",
    "objects/query/Service",
    "objects/query/HostGroup",
    "objects/query/ServiceGroup",
    "objects/query/Comment",
    "objects/query/Downtime",
    "objects/query/User",
    "objects/query/UserGroup",
    "objects/query/Notification",
    "objects/query/Dependency",
    "objects/query/Endpoint",
    "objects/query/Zone",
    "objects/query/CheckCommand",
    "status/query",
    "events/CheckResult",
    "events/StateChange",
    "events/Flapping",
    "events/AcknowledgementSet",
    "events/AcknowledgementCleared",
    "events/CommentAdded",
    "events/CommentRemoved",
    "events/DowntimeAdded",
    "events/DowntimeRemoved",
    "events/DowntimeStarted",
    "events/DowntimeTriggered",
    "events/ObjectCreated",
    "events/ObjectModified",
    "events/ObjectDeleted",
    "actions/reschedule-check",
    "actions/acknowledge-problem",
    "actions/remove-acknowledgement",
    "actions/schedule-downtime",
    "actions/remove-downtime",
    "actions/add-comment",
    "actions/remove-comment",
    "actions/process-check-result",
    "actions/execute-command",
];

/// The [`REQUIRED_PERMISSIONS`] the API user lacks, in that order.
#[must_use]
pub fn missing_permissions(info: &ApiInfo) -> Vec<String> {
    REQUIRED_PERMISSIONS
        .iter()
        .filter(|permission| !info.allows(permission))
        .map(|permission| (*permission).to_owned())
        .collect()
}

/// What "test connection" found.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectionReport {
    /// The API user, its permissions and Icinga's version.
    pub info: ApiInfo,
    /// The instance status (defaults when the user may not read it).
    pub status: InstanceStatus,
    /// [`REQUIRED_PERMISSIONS`] the user lacks.
    pub missing_permissions: Vec<String>,
}

/// Why "test connection" failed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ConnectionFailure {
    /// Icinga refused the credentials (401).
    #[error("Icinga refused the credentials")]
    Unauthorized,
    /// The server's certificate isn't trusted; `certificate` is what it
    /// presented, for trust on first use.
    #[error("{message}")]
    Tls {
        /// The TLS error.
        message: String,
        /// The presented certificate, when it could be read.
        certificate: Option<CertificateInfo>,
    },
    /// The server presented another certificate than the pinned one.
    #[error("the server certificate {actual} does not match the pinned {expected}")]
    CertificateMismatch {
        /// The pinned fingerprint.
        expected: String,
        /// The presented fingerprint.
        actual: String,
    },
    /// The server can't be reached (connection refused, timeout, DNS).
    #[error("unreachable: {0}")]
    Unreachable(String),
    /// Anything else (invalid settings, unreadable files, HTTP errors).
    #[error("{0}")]
    Other(String),
}

impl From<Failure> for ConnectionFailure {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::MissingSecret => {
                Self::Other("no password: enter the API user's password".to_owned())
            }
            Failure::Misconfigured(message) | Failure::Transient(message) => Self::Other(message),
            Failure::Auth(_) => Self::Unauthorized,
            Failure::Tls {
                mismatch: Some((expected, actual)),
                ..
            } => Self::CertificateMismatch { expected, actual },
            Failure::Tls {
                message,
                certificate,
                mismatch: None,
            } => Self::Tls {
                message,
                certificate,
            },
        }
    }
}

/// The shared runtime of the helpers, created on first use. `None` if it
/// can't be built (the helpers then answer with an error).
fn runtime() -> Option<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("icygui-probe")
                .enable_all()
                .build()
                .inspect_err(|error| {
                    tracing::error!(%error, "couldn't build the background runtime");
                })
                .ok()
        })
        .as_ref()
}

/// Runs `task` on the shared runtime and returns its answer's receiver.
fn spawn<T: Send + 'static>(
    task: impl Future<Output = T> + Send + 'static,
    unavailable: impl FnOnce() -> T,
) -> oneshot::Receiver<T> {
    let (reply, receiver) = oneshot::channel();
    match runtime() {
        Some(runtime) => {
            runtime.spawn(async move {
                // The dialog may have closed: nobody to answer.
                let _ = reply.send(task.await);
            });
        }
        None => {
            let _ = reply.send(unavailable());
        }
    }
    receiver
}

/// Tests an environment's settings, as edited (not saved yet): connects
/// with `password` (basic authentication; ignored for client
/// certificates), reads the API user's permissions and the instance status,
/// and lists the permissions the client would miss. Sends no other
/// request.
#[expect(
    clippy::result_large_err,
    reason = "the contract's type; answered once per click, never on a hot path"
)]
pub fn test_connection(
    environment: ic_config::Environment,
    password: Option<SecretString>,
) -> oneshot::Receiver<Result<ConnectionReport, ConnectionFailure>> {
    spawn(probe(environment, password), || {
        Err(ConnectionFailure::Other(
            "the background runtime isn't available".to_owned(),
        ))
    })
}

async fn probe(
    environment: ic_config::Environment,
    password: Option<SecretString>,
) -> Result<ConnectionReport, ConnectionFailure> {
    let settings = connect::settings(&environment, Password::Given(password)).await?;
    let url = settings.base_url.clone();
    let server_name = settings.tls.server_name.clone();
    let client =
        Client::new(settings).map_err(|error| ConnectionFailure::Other(error.to_string()))?;
    let classify = |error: ApiError| {
        let url = url.clone();
        let server_name = server_name.clone();
        async move {
            match error {
                ApiError::Connect(message) => ConnectionFailure::Unreachable(message),
                ApiError::Timeout => ConnectionFailure::Unreachable(error.to_string()),
                other => Failure::from_api(other, &url, server_name.as_deref())
                    .await
                    .into(),
            }
        }
    };
    let info = match client.info().await {
        Ok(info) => info,
        Err(error) => return Err(classify(error).await),
    };
    let status = match client.status().await {
        Ok(status) => status,
        Err(ApiError::Forbidden(_) | ApiError::NotFound(_)) => InstanceStatus::default(),
        Err(error) => return Err(classify(error).await),
    };
    Ok(ConnectionReport {
        missing_permissions: missing_permissions(&info),
        info,
        status,
    })
}

/// Reads the certificate the server at `url` presents, whatever it is
/// (trust on first use: show it, then pin it). `server_name` is checked
/// like the client checks it.
pub fn fetch_certificate(
    url: String,
    server_name: Option<String>,
) -> oneshot::Receiver<Result<CertificateInfo, String>> {
    spawn(
        async move {
            let url = Url::parse(url.trim()).map_err(|error| format!("invalid URL: {error}"))?;
            let server_name = server_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty());
            fetch_server_certificate(&url, server_name)
                .await
                .map_err(|error| error.to_string())
        },
        || Err("the background runtime isn't available".to_owned()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_permissions_follow_icinga_wildcards() {
        let all = ApiInfo {
            permissions: vec!["*".to_owned()],
            ..ApiInfo::default()
        };
        assert!(missing_permissions(&all).is_empty());

        let read_only = ApiInfo {
            permissions: vec![
                "objects/query/*".to_owned(),
                "status/query".to_owned(),
                "events/*".to_owned(),
                "actions/acknowledge-problem (filtered)".to_owned(),
            ],
            ..ApiInfo::default()
        };
        let missing = missing_permissions(&read_only);
        assert_eq!(
            missing,
            [
                "actions/reschedule-check",
                "actions/remove-acknowledgement",
                "actions/schedule-downtime",
                "actions/remove-downtime",
                "actions/add-comment",
                "actions/remove-comment",
                "actions/process-check-result",
                "actions/execute-command",
            ]
        );
        assert_eq!(
            missing_permissions(&ApiInfo::default()).len(),
            REQUIRED_PERMISSIONS.len()
        );
    }

    #[test]
    fn failures_map_to_what_the_dialog_shows() {
        assert_eq!(
            ConnectionFailure::from(Failure::Auth("401".to_owned())),
            ConnectionFailure::Unauthorized
        );
        assert_eq!(
            ConnectionFailure::from(Failure::Tls {
                message: "pin".to_owned(),
                mismatch: Some(("AA".to_owned(), "BB".to_owned())),
                certificate: None,
            }),
            ConnectionFailure::CertificateMismatch {
                expected: "AA".to_owned(),
                actual: "BB".to_owned()
            }
        );
        assert!(matches!(
            ConnectionFailure::from(Failure::MissingSecret),
            ConnectionFailure::Other(_)
        ));
    }
}
