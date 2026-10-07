//! Settings-dialog helpers that need no running environment: "test
//! connection" and "read the server's certificate" (trust on first use).
//! They run on a small shared background runtime and answer through a
//! oneshot channel the UI can await.

use std::sync::OnceLock;

use futures::channel::oneshot;
use ic_api::{ApiError, ApiInfo, CertificateInfo, Url, fetch_server_certificate};
use ic_model::InstanceStatus;
use secrecy::SecretString;

use crate::connect::{self, Failure, Login, Password};
use crate::topology::ConnectedNode;

/// Every permission the client uses (PLAN.md §5), and nothing more: the
/// object types it queries, status, the event types it subscribes to, and
/// the runtime actions. (No `User`, `UserGroup` or `CheckCommand`: nothing
/// reads those objects, and they hold contact data and command lines.)
pub const REQUIRED_PERMISSIONS: &[&str] = &[
    "objects/query/Host",
    "objects/query/Service",
    "objects/query/HostGroup",
    "objects/query/ServiceGroup",
    "objects/query/Comment",
    "objects/query/Downtime",
    "objects/query/Notification",
    "objects/query/Dependency",
    "objects/query/Endpoint",
    "objects/query/Zone",
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
    "events/Notification",
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

/// What "test connection" found at one URL.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectionReport {
    /// The API user, its permissions and Icinga's version.
    pub info: ApiInfo,
    /// The instance status (defaults when the user may not read it).
    pub status: InstanceStatus,
    /// [`REQUIRED_PERMISSIONS`] the user lacks.
    pub missing_permissions: Vec<String>,
    /// The node that answered, its zone and how much of the cluster it
    /// sees (ENV-12); `passed_over` is empty.
    pub node: ConnectedNode,
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
            Failure::Misconfigured(message)
            | Failure::Transient(message)
            | Failure::TransientUntrusted { error: message, .. } => Self::Other(message),
            Failure::Auth(_) => Self::Unauthorized,
            Failure::Tls {
                mismatch: Some((expected, actual)),
                ..
            } => Self::CertificateMismatch { expected, actual },
            Failure::Tls {
                message,
                certificate,
                mismatch: None,
                ..
            } => Self::Tls {
                message,
                certificate: certificate.map(|certificate| *certificate),
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

/// Tests one URL of an environment's settings, as edited (not saved
/// yet): `environment.urls[url]`, with `password` (basic authentication;
/// ignored for client certificates). Logs in, finds out which node answers
/// and how much of the cluster it sees (`/v1/status/IcingaApplication`,
/// `/v1/objects/zones`; ENV-12), reads the instance status, and lists the
/// permissions the client would miss. Sends no other request.
#[expect(
    clippy::result_large_err,
    reason = "the contract's type; answered once per click, never on a hot path"
)]
pub fn test_connection(
    environment: ic_config::Environment,
    url: usize,
    password: Option<SecretString>,
) -> oneshot::Receiver<Result<ConnectionReport, ConnectionFailure>> {
    spawn(probe(environment, url, password), || {
        Err(ConnectionFailure::Other(
            "the background runtime isn't available".to_owned(),
        ))
    })
}

async fn probe(
    environment: ic_config::Environment,
    url: usize,
    password: Option<SecretString>,
) -> Result<ConnectionReport, ConnectionFailure> {
    let login = Login::read(&environment, Password::Given(password)).await?;
    let reached = match connect::reach(
        &login,
        &environment.urls,
        url,
        ic_api::DEFAULT_ACTION_TIMEOUT,
        None,
    )
    .await
    {
        Ok(reached) => reached,
        Err(Failure::Transient(message)) => return Err(ConnectionFailure::Unreachable(message)),
        Err(failure) => return Err(failure.into()),
    };
    let status = match reached.client.status().await {
        Ok(status) => status,
        Err(ApiError::Forbidden(_) | ApiError::NotFound(_)) => InstanceStatus::default(),
        Err(error) => {
            let base = reached.client.base_url().clone();
            let failure = Failure::from_api(error, &reached.node.url, &base, None).await;
            return Err(match failure {
                Failure::Transient(message) => ConnectionFailure::Unreachable(message),
                other => other.into(),
            });
        }
    };
    Ok(ConnectionReport {
        missing_permissions: missing_permissions(&reached.info),
        info: reached.info,
        status,
        node: reached.node,
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

    /// The user guide's ready-made `ApiUser` grants exactly what the client
    /// uses, except `actions/execute-command`: that one is opt-in, because
    /// with free-form macros it runs any command on the agents.
    #[test]
    fn the_user_guides_api_user_is_the_list_without_execute_command() {
        let guide = include_str!("../../../docs/user-guide.md");
        let snippet = guide
            .split("object ApiUser \"icygui\" {")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("the user guide has the ApiUser snippet");
        let mut granted: Vec<&str> = snippet
            .lines()
            .filter_map(|line| line.trim().strip_prefix('"')?.strip_suffix("\","))
            .collect();
        granted.sort_unstable();
        let mut expected: Vec<&str> = REQUIRED_PERMISSIONS
            .iter()
            .copied()
            .filter(|permission| *permission != "actions/execute-command")
            .collect();
        expected.sort_unstable();
        assert_eq!(granted, expected);
    }

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

    /// The event and action permissions are exactly the event types the
    /// client subscribes to and the actions it can send (the object
    /// queries are checked against a real load in `tests/engine/probe.rs`).
    #[test]
    fn event_and_action_permissions_are_exactly_what_the_client_uses() {
        use std::collections::BTreeSet;

        use ic_model::{
            Action, ChildOptions, CommandType, DowntimeMode, EventKind, Timestamp, Vars,
        };

        let listed = |prefix: &str| -> BTreeSet<String> {
            REQUIRED_PERMISSIONS
                .iter()
                .filter_map(|permission| permission.strip_prefix(prefix))
                .map(str::to_owned)
                .collect()
        };
        let kinds: BTreeSet<String> = EventKind::ALL
            .iter()
            .map(|kind| kind.api_name().to_owned())
            .collect();
        assert_eq!(listed("events/"), kinds);

        let every_action = [
            Action::CheckNow { force: true },
            Action::Acknowledge {
                comment: String::new(),
                sticky: false,
                persistent: false,
                expiry: None,
            },
            Action::RemoveAcknowledgement,
            Action::ScheduleDowntime {
                comment: String::new(),
                start: Timestamp::EPOCH,
                end: Timestamp::EPOCH,
                mode: DowntimeMode::Fixed,
                all_services: false,
                child_options: ChildOptions::None,
                trigger_name: None,
            },
            Action::RemoveAllDowntimes,
            Action::AddComment {
                text: String::new(),
                expiry: None,
            },
            Action::ProcessCheckResult {
                exit_status: 0,
                output: String::new(),
                perfdata: Vec::new(),
                ttl: None,
            },
            Action::ExecuteCommand {
                command_type: CommandType::EventCommand,
                command: None,
                endpoint: None,
                macros: Vars::new(),
                ttl: 60.0,
            },
        ];
        let mut sent: BTreeSet<String> = every_action
            .iter()
            .map(|action| action.api_name().to_owned())
            .collect();
        // A comment target (`ActionTarget::Comments`) removes the comments.
        sent.insert("remove-comment".to_owned());
        assert_eq!(listed("actions/"), sent);

        // Nothing else: object queries, status, events, actions.
        assert!(REQUIRED_PERMISSIONS.iter().all(|permission| {
            ["objects/query/", "events/", "actions/"]
                .iter()
                .any(|prefix| permission.starts_with(prefix))
                || *permission == "status/query"
        }));
    }

    #[test]
    fn failures_map_to_what_the_dialog_shows() {
        assert_eq!(
            ConnectionFailure::from(Failure::Auth("401".to_owned())),
            ConnectionFailure::Unauthorized
        );
        assert_eq!(
            ConnectionFailure::from(Failure::Tls {
                url: "https://m:5665".to_owned(),
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
