//! Turning an environment into a connected [`Client`]: the password from
//! the secret store, certificate and CA files, each URL's pin; walking the
//! environment's URLs in order of preference and finding out which node of
//! the cluster answers and how much of it that node sees (ENV-12,
//! [`crate::topology`]); and classifying what goes wrong into what the
//! user can do about it.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ic_api::{
    ApiError, ApiInfo, CertificateInfo, Client, ConnectionSettings, Credentials, EventLines,
    TlsSettings, Url, fetch_server_certificate,
};
use ic_config::{ApiUrl, AuthConfig, Environment};
use ic_model::EventKind;
use secrecy::{ExposeSecret as _, SecretBox, SecretString};

use crate::command::UntrustedUrl;
use crate::ports::SecretStore;
use crate::topology::{ClusterView, ConnectedNode, classify, walk_order};

/// Where the password for basic authentication comes from.
pub(crate) enum Password {
    /// The secret store, account = environment id (the running engine).
    Store(Arc<dyn SecretStore>),
    /// Given by the caller (the settings dialog's "test connection").
    Given(Option<SecretString>),
}

/// Why a connection can't be made, by what the user can do about it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Failure {
    /// No password in the secret store.
    MissingSecret,
    /// The settings can't work (URL, files, pin).
    Misconfigured(String),
    /// Icinga refused the credentials.
    Auth(String),
    /// The certificate isn't trusted or doesn't match the pin.
    Tls {
        /// The configured URL (as written in the environment) whose server
        /// presented it: its pin is the one to set.
        url: String,
        /// What went wrong.
        message: String,
        /// Both fingerprints, for a pin mismatch.
        mismatch: Option<(String, String)>,
        /// What the server presented.
        certificate: Option<Box<CertificateInfo>>,
    },
    /// Anything that may go away by itself: retry with backoff.
    Transient(String),
    /// Several URLs and none usable now: one may come back by itself
    /// (retry with backoff, like [`Failure::Transient`], whose message
    /// `error` is), and another presented a certificate that isn't
    /// trusted, which the user may trust to use that one meanwhile (a
    /// standby never trusted on first use).
    TransientUntrusted {
        /// Every URL's problem.
        error: String,
        /// The URL whose certificate isn't trusted, and the certificate.
        untrusted: Box<UntrustedUrl>,
    },
}

impl Failure {
    /// Classifies an API error of the server at `url` (the configured
    /// URL, `base` its parsed form); for TLS errors it reads the
    /// certificate the server presents (one more handshake), for trust on
    /// first use.
    pub(crate) async fn from_api(
        error: ApiError,
        url: &str,
        base: &Url,
        server_name: Option<&str>,
    ) -> Self {
        match error {
            ApiError::Unauthorized => Self::Auth(error.to_string()),
            ApiError::Tls(_) | ApiError::CertificateMismatch { .. } => {
                let mismatch = match &error {
                    ApiError::CertificateMismatch { expected, actual } => {
                        Some((expected.clone(), actual.clone()))
                    }
                    _ => None,
                };
                let certificate = match fetch_server_certificate(base, server_name).await {
                    Ok(certificate) => Some(Box::new(certificate)),
                    Err(fetch) => {
                        tracing::debug!(error = %fetch, "couldn't read the server certificate");
                        None
                    }
                };
                Self::Tls {
                    url: url.to_owned(),
                    message: error.to_string(),
                    mismatch,
                    certificate,
                }
            }
            ApiError::InvalidSettings(message) => Self::Misconfigured(message),
            other => Self::Transient(other.to_string()),
        }
    }

    /// Whether it may go away by itself.
    fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_) | Self::TransientUntrusted { .. })
    }

    /// Why a URL wasn't taken, in a few words: `connection refused`,
    /// `login refused`, `certificate not trusted`.
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::MissingSecret => "no password".to_owned(),
            Self::Misconfigured(message)
            | Self::Transient(message)
            | Self::TransientUntrusted { error: message, .. } => message.clone(),
            Self::Auth(_) => "login refused".to_owned(),
            Self::Tls {
                mismatch: Some(_), ..
            } => "the certificate doesn't match the pinned one".to_owned(),
            Self::Tls { .. } => "certificate not trusted".to_owned(),
        }
    }
}

/// What every URL of an environment shares: the login and the trusted CA,
/// read once per connect (the password from the secret store, the files
/// from disk) and kept for the session's probes of other URLs.
#[derive(Clone)]
pub(crate) struct Login {
    credentials: LoginCredentials,
    ca_pem: Option<Vec<u8>>,
    use_system_roots: bool,
}

#[derive(Clone)]
enum LoginCredentials {
    Basic {
        username: String,
        password: SecretString,
    },
    Certificate {
        cert_pem: Vec<u8>,
        key_pem: Arc<SecretBox<Vec<u8>>>,
    },
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login").finish_non_exhaustive()
    }
}

impl Login {
    /// Reads the login of `environment`: the password (basic
    /// authentication), the certificate and key files, the CA file.
    ///
    /// # Errors
    ///
    /// [`Failure::MissingSecret`] without a password, [`Failure::Transient`]
    /// if the secret store can't be read right now, [`Failure::Misconfigured`]
    /// for unreadable files.
    pub(crate) async fn read(
        environment: &Environment,
        password: Password,
    ) -> Result<Self, Failure> {
        let ca_pem = match &environment.tls.ca_file {
            Some(path) => Some(read_file("CA file", path).await?),
            None => None,
        };
        let credentials = match &environment.auth {
            AuthConfig::Basic { username } => LoginCredentials::Basic {
                username: username.trim().to_owned(),
                password: password_for(environment, password).await?,
            },
            AuthConfig::ClientCertificate {
                cert_path,
                key_path,
            } => LoginCredentials::Certificate {
                cert_pem: read_file("client certificate", cert_path).await?,
                key_pem: Arc::new(SecretBox::new(Box::new(
                    read_file("client key", key_path).await?,
                ))),
            },
        };
        Ok(Self {
            credentials,
            ca_pem,
            use_system_roots: environment.tls.use_system_roots,
        })
    }

    /// The connection settings for `url`, with its pin and server name.
    ///
    /// # Errors
    ///
    /// [`Failure::Misconfigured`] for an invalid URL or pin.
    pub(crate) fn settings(&self, url: &ApiUrl) -> Result<ConnectionSettings, Failure> {
        let base = url
            .api_url()
            .map_err(|error| Failure::Misconfigured(error.to_string()))?;
        let pinned_sha256 = url
            .pinned_fingerprint()
            .map_err(|error| Failure::Misconfigured(error.to_string()))?;
        let tls = TlsSettings {
            ca_pem: self.ca_pem.clone(),
            pinned_sha256,
            server_name: url.server_name().map(str::to_owned),
            use_system_roots: self.use_system_roots,
        };
        let credentials = match &self.credentials {
            LoginCredentials::Basic { username, password } => Credentials::Basic {
                username: username.clone(),
                password: password.clone(),
            },
            LoginCredentials::Certificate { cert_pem, key_pem } => Credentials::ClientCertificate {
                cert_pem: cert_pem.clone(),
                key_pem: SecretBox::new(Box::new(key_pem.expose_secret().clone())),
            },
        };
        Ok(ConnectionSettings::new(base, credentials, tls))
    }
}

async fn password_for(
    environment: &Environment,
    password: Password,
) -> Result<SecretString, Failure> {
    match password {
        Password::Given(Some(password)) => Ok(password),
        Password::Given(None) => Err(Failure::MissingSecret),
        Password::Store(secrets) => {
            let account = environment.id.clone();
            // Keychain and Secret Service calls block (D-Bus, prompts).
            let read = tokio::task::spawn_blocking(move || secrets.get(&account)).await;
            match read {
                Ok(Ok(Some(password))) => Ok(password),
                Ok(Ok(None)) => Err(Failure::MissingSecret),
                Ok(Err(error)) => Err(Failure::Transient(error.to_string())),
                Err(error) => Err(Failure::Transient(format!("secret store: {error}"))),
            }
        }
    }
}

async fn read_file(what: &str, path: &Path) -> Result<Vec<u8>, Failure> {
    tokio::fs::read(path)
        .await
        .map_err(|error| Failure::Misconfigured(format!("{what} {}: {error}", path.display())))
}

/// A node that answered: its client, the API user, and who it is.
pub(crate) struct Reached {
    pub(crate) client: Client,
    pub(crate) info: ApiInfo,
    pub(crate) node: ConnectedNode,
}

/// Logs in at `urls[index]` (`GET /v1`) and finds out which node answers
/// and how much of the cluster it sees ([`identify`]). `quick`: how long
/// these few small requests may take together while other URLs remain to
/// try (a node that accepts connections but doesn't answer, an Icinga busy
/// reloading, shouldn't hold up the others for the full request timeout);
/// `None` waits as long as any query.
///
/// # Errors
///
/// As [`Login::settings`]; API errors classified by [`Failure::from_api`];
/// [`Failure::Transient`] when `quick` ran out.
pub(crate) async fn reach(
    login: &Login,
    urls: &[ApiUrl],
    index: usize,
    action_timeout: Duration,
    quick: Option<Duration>,
) -> Result<Reached, Failure> {
    let answer = reach_unbounded(login, urls, index, action_timeout);
    match quick {
        None => answer.await,
        Some(limit) => tokio::time::timeout(limit, answer)
            .await
            .unwrap_or_else(|_| Err(Failure::Transient(format!("no answer within {limit:?}")))),
    }
}

async fn reach_unbounded(
    login: &Login,
    urls: &[ApiUrl],
    index: usize,
    action_timeout: Duration,
) -> Result<Reached, Failure> {
    let Some(url) = urls.get(index) else {
        return Err(Failure::Misconfigured(format!("no URL {}", index + 1)));
    };
    let mut settings = login.settings(url)?;
    settings.action_timeout = action_timeout;
    let base = settings.base_url.clone();
    let server_name = settings.tls.server_name.clone();
    let configured = url.url.trim().to_owned();
    let client = Client::new(settings).map_err(|error| match error {
        ApiError::InvalidSettings(message) => Failure::Misconfigured(message),
        other => Failure::Misconfigured(other.to_string()),
    })?;
    let classify_error = async |error: ApiError| {
        Failure::from_api(error, &configured, &base, server_name.as_deref()).await
    };
    let info = match client.info().await {
        Ok(info) => info,
        Err(error) => return Err(classify_error(error).await),
    };
    let (name, zone, view) = match identify(&client, &info, &base).await {
        Ok(identity) => identity,
        Err(error) => return Err(classify_error(error).await),
    };
    Ok(Reached {
        client,
        info,
        node: ConnectedNode {
            url: configured.clone(),
            url_index: index,
            name,
            zone,
            view,
            passed_over: Vec::new(),
        },
    })
}

/// Which node answers at `base` and how much of the cluster it sees: its
/// name from `/v1/status/IcingaApplication`, its zone from
/// `/v1/objects/zones` (the zone listing it among its endpoints), and the
/// view that follows ([`classify`]). Without the permissions (`status/query`,
/// `objects/query/Zone`) the view is not verified, and the name is the
/// URL's host when unknown.
///
/// # Errors
///
/// The API's error when Icinga can't answer (refusals of the permissions
/// aren't errors).
async fn identify(
    client: &Client,
    info: &ApiInfo,
    base: &Url,
) -> Result<(String, Option<String>, ClusterView), ApiError> {
    let host = base.host_str().unwrap_or_default().to_owned();
    let unverified = |name: String, reason: &str| {
        Ok((
            name,
            None,
            ClusterView::Unverified {
                reason: reason.to_owned(),
            },
        ))
    };
    if !info.allows("status/query") {
        return unverified(
            host,
            "the API user may not read the status (status/query), which names the node",
        );
    }
    let name = match client.node_name().await {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        Ok(_) => return unverified(host, "Icinga reported no node name"),
        Err(ApiError::Forbidden(_) | ApiError::NotFound(_)) => {
            return unverified(
                host,
                "the API user may not read the status (status/query), which names the node",
            );
        }
        Err(error) => return Err(error),
    };
    if !info.allows("objects/query/Zone") {
        return unverified(
            name,
            "the API user may not read the zones (objects/query/Zone)",
        );
    }
    match client.zones().await {
        Ok(zones) => {
            let (zone, view) = classify(&name, &zones);
            Ok((name, zone, view))
        }
        Err(ApiError::Forbidden(_) | ApiError::NotFound(_)) => unverified(
            name,
            "the API user may not read the zones (objects/query/Zone)",
        ),
        Err(error) => Err(error),
    }
}

/// A client, the API user it logged in as, the event stream if the user
/// may read events (and what it subscribed to), the node it reached, and
/// the login for probes of the other URLs.
pub(crate) struct Connected {
    pub(crate) client: Client,
    pub(crate) info: ApiInfo,
    pub(crate) lines: Option<EventLines>,
    /// The stream's event types.
    pub(crate) kinds: Vec<EventKind>,
    /// Its filter: a quiet stream's naming the heartbeats, whose check
    /// results it carries ([`StreamWish`]).
    pub(crate) filter: Option<String>,
    /// It was opened in quiet mode (no check results, PERF-09).
    pub(crate) quiet: bool,
    pub(crate) node: ConnectedNode,
    pub(crate) login: Login,
}

/// What the next event stream should carry, read whenever one opens (a
/// connect, a switch between quiet and live): quiet or live, and in quiet
/// mode the heartbeats' check results by Icinga's stream filter
/// ([`crate::heartbeat`]). Icinga refuses a filter without the
/// `filter-expression` permission where it enforces it (2.17 by default):
/// then [`StreamWish::refused`] is set for good, and quiet streams carry
/// no check results (the engine reads the heartbeats' last check once per
/// interval instead).
#[derive(Debug, Default)]
pub(crate) struct StreamWish {
    /// Quiet mode is wanted.
    pub(crate) quiet: AtomicBool,
    /// The quiet stream's filter naming the heartbeats (`None`: none).
    pub(crate) beats: std::sync::Mutex<Option<String>>,
    /// Icinga refused the filter.
    pub(crate) refused: AtomicBool,
}

impl StreamWish {
    /// The event types and filter a stream for the API user `info` opens
    /// with now.
    pub(crate) fn spec(&self, info: &ApiInfo) -> (Vec<EventKind>, Option<String>) {
        let quiet = self.quiet.load(Ordering::SeqCst);
        let mut kinds = stream_kinds(info, quiet);
        if !quiet || self.refused.load(Ordering::SeqCst) || kinds.is_empty() {
            return (kinds, None);
        }
        let check_results = info.allows(&format!("events/{}", EventKind::CheckResult.api_name()));
        let beats = self
            .beats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match beats {
            Some(filter) if check_results => {
                kinds.push(EventKind::CheckResult);
                (kinds, Some(filter))
            }
            _ => (kinds, None),
        }
    }
}

/// Opens a stream with `kinds` and `filter`; when Icinga refuses the
/// filter (no `filter-expression` permission), marks it refused in `wish`
/// and opens it without (and without check results). Returns the lines
/// and what the stream carries.
pub(crate) async fn open_wished(
    client: &Client,
    kinds: Vec<EventKind>,
    filter: Option<String>,
    wish: &StreamWish,
) -> Result<(EventLines, Vec<EventKind>, Option<String>), ApiError> {
    let queue = format!("icygui-{}", uuid::Uuid::new_v4());
    match client.events_filtered(&queue, &kinds, filter.as_deref()).await {
        Ok(stream) => Ok((stream.into_lines(), kinds, filter)),
        Err(ApiError::Forbidden(message))
            if filter.is_some() && message.contains("filter-expression") =>
        {
            tracing::info!(
                "the API user may not filter the event stream (filter-expression): \
                 heartbeats are read once per interval while quiet"
            );
            wish.refused.store(true, Ordering::SeqCst);
            let kinds: Vec<EventKind> = kinds
                .into_iter()
                .filter(|kind| *kind != EventKind::CheckResult)
                .collect();
            let lines = open_events(client, &kinds).await?;
            Ok((lines, kinds, None))
        }
        Err(error) => Err(error),
    }
}

/// The event types a stream subscribes to: every type the API user may
/// read, without `CheckResult` in quiet mode ([`EventKind::QUIET`]).
pub(crate) fn stream_kinds(info: &ApiInfo, quiet: bool) -> Vec<EventKind> {
    let kinds: &[EventKind] = if quiet {
        &EventKind::QUIET
    } else {
        &EventKind::ALL
    };
    kinds
        .iter()
        .copied()
        .filter(|kind| info.allows(&format!("events/{}", kind.api_name())))
        .collect()
}

/// Opens an event stream (a fresh queue `icygui-<uuid>`) for `kinds`.
///
/// # Errors
///
/// As [`Client::events`].
pub(crate) async fn open_events(
    client: &Client,
    kinds: &[EventKind],
) -> Result<EventLines, ApiError> {
    let queue = format!("icygui-{}", uuid::Uuid::new_v4());
    Ok(client.events(&queue, kinds).await?.into_lines())
}

/// Connects (ENV-12): reads the login, then walks the environment's URLs
/// in order of preference (`first` before the others: a node a probe
/// found better) and logs in at each until one answers whose node sees the
/// whole cluster, or whose view can't be verified (the user's order is all
/// there is to go by then). A node in a child zone (a partial view) is
/// taken only when the walk found nothing better: the first such one.
/// Then the event stream opens (queue `icygui-<uuid>`) for every event
/// type the API user may read, without check results while `quiet` is set
/// when it opens (quiet mode, PERF-09).
///
/// # Errors
///
/// As [`Login::read`]; when no URL can be used, what went wrong at each
/// ([`combine`]).
pub(crate) async fn connect(
    environment: &Environment,
    secrets: Arc<dyn SecretStore>,
    timeouts: (Duration, Duration),
    first: Option<usize>,
    wish: Arc<StreamWish>,
) -> Result<Connected, Failure> {
    let (action_timeout, identify_timeout) = timeouts;
    let login = Login::read(environment, Password::Store(secrets)).await?;
    let urls = &environment.urls;
    if urls.is_empty() {
        return Err(Failure::Misconfigured(
            "the environment lists no API URL".to_owned(),
        ));
    }
    let mut failures: Vec<(usize, Failure)> = Vec::new();
    let mut passed_over: Vec<(usize, String)> = Vec::new();
    let mut fallback: Option<Reached> = None;
    let order = walk_order(urls.len(), first);
    let last = order.last().copied();
    for index in order {
        // While another URL remains (or a satellite to fall back on), a
        // node that doesn't answer is passed over after `identify_timeout`.
        let quick = (Some(index) != last || fallback.is_some()).then_some(identify_timeout);
        let reached = match reach(&login, urls, index, action_timeout, quick).await {
            Ok(reached) => reached,
            Err(failure) => {
                tracing::info!(url = %urls[index].label(), reason = %failure.reason(), "API URL not usable");
                passed_over.push((index, failure.reason()));
                failures.push((index, failure));
                continue;
            }
        };
        if let ClusterView::Partial { zone } = &reached.node.view {
            tracing::info!(
                url = %urls[index].label(),
                node = %reached.node.name,
                %zone,
                "the node sees part of the cluster; looking for one that sees all of it"
            );
            // Passed over unless it is taken in the end (`finish` drops
            // the one taken).
            passed_over.push((index, reached.node.view.label()));
            if fallback.is_none() {
                fallback = Some(reached);
            }
            continue;
        }
        match open_stream(reached, &wish).await {
            Ok(opened) => {
                return Ok(finish(opened, login, urls, passed_over));
            }
            Err((reached_index, failure)) => {
                passed_over.push((reached_index, failure.reason()));
                failures.push((reached_index, failure));
            }
        }
    }
    if let Some(reached) = fallback {
        match open_stream(reached, &wish).await {
            Ok(opened) => {
                return Ok(finish(opened, login, urls, passed_over));
            }
            Err((index, failure)) => failures.push((index, failure)),
        }
    }
    Err(combine(failures, urls))
}

/// A reached node with its event stream open (if any).
struct Opened {
    client: Client,
    info: ApiInfo,
    lines: Option<EventLines>,
    kinds: Vec<EventKind>,
    filter: Option<String>,
    quiet: bool,
    node: ConnectedNode,
}

/// The connection with `passed_over` (by URL index, without the one taken)
/// in order of preference.
fn finish(
    opened: Opened,
    login: Login,
    urls: &[ApiUrl],
    mut passed_over: Vec<(usize, String)>,
) -> Connected {
    let Opened {
        client,
        info,
        lines,
        kinds,
        filter,
        quiet,
        mut node,
    } = opened;
    passed_over.retain(|(index, _)| *index != node.url_index);
    passed_over.sort_by_key(|(index, _)| *index);
    node.passed_over = passed_over
        .into_iter()
        .filter_map(|(index, reason)| Some((urls.get(index)?.label(), reason)))
        .collect();
    tracing::info!(
        node = %node.name,
        url = %node.url,
        view = %node.view.label(),
        "connected to Icinga"
    );
    Connected {
        client,
        info,
        lines,
        kinds,
        filter,
        quiet,
        node,
        login,
    }
}

/// Opens the event stream of a reached node for every event type the API
/// user may read, without check results when `quiet` (none: no stream).
async fn open_stream(reached: Reached, wish: &StreamWish) -> Result<Opened, (usize, Failure)> {
    let Reached { client, info, node } = reached;
    let quiet = wish.quiet.load(Ordering::SeqCst);
    let (kinds, filter) = wish.spec(&info);
    let opened = |lines: Option<(EventLines, Vec<EventKind>, Option<String>)>,
                  client: Client,
                  info: ApiInfo,
                  node: ConnectedNode| {
        let (lines, kinds, filter) = match lines {
            Some((lines, kinds, filter)) => (Some(lines), kinds, filter),
            None => (None, Vec::new(), None),
        };
        Opened {
            client,
            info,
            lines,
            kinds,
            filter,
            quiet,
            node,
        }
    };
    if kinds.is_empty() {
        tracing::warn!(
            user = %info.user,
            "the API user may not read any event type: no live updates"
        );
        return Ok(opened(None, client, info, node));
    }
    if stream_kinds(&info, false).len() < EventKind::ALL.len() {
        tracing::warn!(
            user = %info.user,
            allowed = kinds.len(),
            "the API user may read only some event types"
        );
    }
    match open_wished(&client, kinds, filter, wish).await {
        Ok(stream) => Ok(opened(Some(stream), client, info, node)),
        Err(ApiError::Forbidden(message)) => {
            tracing::warn!(%message, "the event stream was refused: no live updates");
            Ok(opened(None, client, info, node))
        }
        Err(error) => {
            let base = client.base_url().clone();
            let failure = Failure::from_api(error, &node.url, &base, None).await;
            Err((node.url_index, failure))
        }
    }
}

/// What to report when no URL could be used. One URL: its failure, as it
/// is. Several: retrying (with every URL's reason) as long as one of them
/// may come back by itself, so a node that is down for a while doesn't
/// stop the engine because another needs the user (offering to trust the
/// certificate of the first URL whose certificate isn't trusted, so a
/// standby never trusted on first use can take over); otherwise the
/// failure of the most preferred URL, which needs the user (a refused
/// login, an untrusted certificate, settings that can't work).
fn combine(mut failures: Vec<(usize, Failure)>, urls: &[ApiUrl]) -> Failure {
    failures.sort_by_key(|(index, _)| *index);
    if failures.len() <= 1 {
        return failures.pop().map_or_else(
            || Failure::Transient("no API URL answered".to_owned()),
            |(_, failure)| failure,
        );
    }
    let label = |index: usize| urls.get(index).map(ApiUrl::label).unwrap_or_default();
    if failures.iter().any(|(_, failure)| failure.is_transient()) {
        let reasons: Vec<String> = failures
            .iter()
            .map(|(index, failure)| format!("{}: {}", label(*index), failure.reason()))
            .collect();
        let error = reasons.join("; ");
        let untrusted = failures.into_iter().find_map(|(_, failure)| match failure {
            Failure::Tls {
                url,
                message,
                certificate: Some(certificate),
                ..
            } => Some(UntrustedUrl {
                url,
                message,
                certificate: *certificate,
            }),
            _ => None,
        });
        return match untrusted {
            Some(untrusted) => Failure::TransientUntrusted {
                error,
                untrusted: Box::new(untrusted),
            },
            None => Failure::Transient(error),
        };
    }
    let (index, failure) = failures.swap_remove(0);
    match failure {
        Failure::Auth(message) => Failure::Auth(format!("{}: {message}", label(index))),
        Failure::Misconfigured(message) => {
            Failure::Misconfigured(format!("{}: {message}", label(index)))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(count: usize) -> Vec<ApiUrl> {
        (1..=count)
            .map(|index| ApiUrl::new(&format!("https://master-{index:02}:5665")))
            .collect()
    }

    fn tls(url: &str) -> Failure {
        Failure::Tls {
            url: url.to_owned(),
            message: "unknown issuer".to_owned(),
            mismatch: None,
            certificate: None,
        }
    }

    #[test]
    fn one_url_reports_its_own_failure() {
        let failure = combine(vec![(0, tls("https://master-01:5665"))], &urls(1));
        assert_eq!(failure, tls("https://master-01:5665"));
        let failure = combine(
            vec![(0, Failure::Transient("connection refused".to_owned()))],
            &urls(1),
        );
        assert_eq!(
            failure,
            Failure::Transient("connection refused".to_owned()),
            "unchanged for a single master"
        );
    }

    #[test]
    fn several_urls_keep_retrying_while_one_may_come_back() {
        let failure = combine(
            vec![
                (1, Failure::Transient("connection refused".to_owned())),
                (0, tls("https://master-01:5665")),
            ],
            &urls(2),
        );
        assert_eq!(
            failure,
            Failure::Transient(
                "master-01:5665: certificate not trusted; master-02:5665: connection refused"
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_standby_with_a_certificate_not_trusted_is_offered_while_retrying() {
        let certificate = CertificateInfo {
            sha256: [0xab; 32],
            subject: "CN=master-02".to_owned(),
            issuer: "CN=Icinga CA".to_owned(),
            names: vec!["master-02".to_owned()],
            not_before: ic_model::Timestamp::from_unix_seconds(0.),
            not_after: ic_model::Timestamp::from_unix_seconds(1.),
        };
        let failure = combine(
            vec![
                (0, Failure::Transient("connection refused".to_owned())),
                (
                    1,
                    Failure::Tls {
                        url: "https://master-02:5665".to_owned(),
                        message: "unknown issuer".to_owned(),
                        mismatch: None,
                        certificate: Some(Box::new(certificate.clone())),
                    },
                ),
            ],
            &urls(2),
        );
        assert_eq!(
            failure,
            Failure::TransientUntrusted {
                error:
                    "master-01:5665: connection refused; master-02:5665: certificate not trusted"
                        .to_owned(),
                untrusted: Box::new(UntrustedUrl {
                    url: "https://master-02:5665".to_owned(),
                    message: "unknown issuer".to_owned(),
                    certificate,
                }),
            }
        );
        assert!(failure.is_transient());
    }

    #[test]
    fn otherwise_the_preferred_urls_failure_needs_the_user() {
        let failure = combine(
            vec![
                (1, tls("https://master-02:5665")),
                (0, Failure::Auth("401".to_owned())),
            ],
            &urls(2),
        );
        assert_eq!(failure, Failure::Auth("master-01:5665: 401".to_owned()));
        let failure = combine(
            vec![
                (1, Failure::Auth("401".to_owned())),
                (0, tls("https://master-01:5665")),
            ],
            &urls(2),
        );
        assert_eq!(failure, tls("https://master-01:5665"));
    }

    #[test]
    fn reasons_are_short() {
        assert_eq!(tls("u").reason(), "certificate not trusted");
        assert_eq!(
            Failure::Auth("401 Unauthorized".to_owned()).reason(),
            "login refused"
        );
        assert_eq!(
            Failure::Transient("connection refused".to_owned()).reason(),
            "connection refused"
        );
    }
}
