//! The HTTP client: object queries, status, actions and the event stream.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use ic_model::{
    Action, ActionTarget, Comment, Dependency, Downtime, Endpoint, EventKind, Host, HostGroup,
    InstanceStatus, ObjectKey, Service, ServiceGroup, Timestamp,
};
use reqwest::header::{ACCEPT, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use url::Url;

use crate::actions::{self, Batch, TargetKind};
use crate::error::ApiError;
use crate::events::EventStream;
use crate::info::ApiInfo;
use crate::settings::{CONNECT_TIMEOUT, ConnectionSettings, Credentials};
use crate::tls;
use crate::wire::{
    self, ActionResultWire, CheckableAttrs, CommentAttrs, DependencyAttrs, DowntimeAttrs,
    EndpointAttrs, GroupAttrs, InfoResult, QueryResult, Results, StatusResult, ZoneAttrs,
};

/// How many names one targeted query or action request carries. Icinga
/// limits request bodies to 1 MiB for these permissions; 200 names stay far
/// below that even with long names.
pub const NAMES_PER_REQUEST: usize = 200;

/// Icinga's message for a name list that contains an unknown name.
const NO_OBJECTS_FOUND: &str = "No objects found.";

/// How long an idle pooled connection is kept. Icinga closes idle API
/// connections after about 10 seconds; reusing one it is about to close
/// would fail the request.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// TCP keepalive, so a connection that died without a reset (sleep and
/// resume, VPN or network change, lost NAT state) is noticed even when
/// nothing is sent: the event stream has no read timeout, and Icinga writes
/// nothing while nothing happens. After this much silence the OS starts
/// probing the peer…
const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
/// …every this often…
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// …and drops the connection after this many unanswered probes: a dead
/// stream ends within about a minute (30 s + 3 × 10 s).
const KEEPALIVE_RETRIES: u32 = 3;
/// On Linux, sent data (keepalive probes included) left unacknowledged
/// this long also drops the connection.
#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
const TCP_USER_TIMEOUT: Duration = Duration::from_secs(30);

/// A response body may take this many request timeouts in total, as long
/// as it keeps arriving (it may pause at most one request timeout between
/// reads). A full service list of a large installation over a slow VPN
/// takes longer than one request timeout; a server that trickles a byte
/// now and then still can't hold a request forever.
const BODY_TIME_FACTOR: u32 = 20;

/// How much of an error response is read (Icinga's are tiny; a proxy's
/// error page needn't be read whole).
const MAX_ERROR_BODY: usize = 64 * 1024;

/// The result of an action for one object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionResult {
    /// Icinga's per-object code: 200 (done), 202 (accepted, for
    /// `execute-command`), 4xx/5xx (failed for this object). For objects
    /// whose request failed as a whole after other requests of the same
    /// action had been answered (or that weren't sent because of that
    /// failure): the HTTP status of the failure, or 0 if it had none
    /// (connection lost, timeout).
    pub code: u16,
    /// Icinga's message (`Successfully acknowledged problem for object …`).
    pub status: String,
    /// The `name` Icinga returns: the created comment or downtime for
    /// `add-comment` and `schedule-downtime`.
    pub name: Option<String>,
    /// The full name of the object, downtime or comment this result is
    /// for (`host`, `host!service`, a downtime name), when known. Icinga
    /// answers name lists in request order, which is how it's matched.
    pub target: Option<String>,
}

impl ActionResult {
    /// Whether the action succeeded for this object (2xx).
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.code)
    }
}

/// A client for one Icinga 2 API endpoint. Cheap to clone; clones share
/// the connection pool.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    /// The base URL, path ending in `/`.
    base: Url,
    basic: Option<(String, SecretString)>,
    timeout: Duration,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.inner.base.as_str())
            .field(
                "user",
                &self.inner.basic.as_ref().map(|(user, _)| user.as_str()),
            )
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Builds a client. Nothing is sent until the first request.
    ///
    /// Proxy environment variables are ignored: Icinga APIs are internal,
    /// and a desktop app launched from the dock wouldn't see them anyway.
    /// Redirects are not followed.
    ///
    /// # Errors
    ///
    /// [`ApiError::InvalidSettings`] if the URL isn't `https` with a host,
    /// a PEM (CA, client certificate or key) can't be parsed, or the
    /// server name override isn't a valid name.
    pub fn new(settings: ConnectionSettings) -> Result<Self, ApiError> {
        let base = normalize_base(settings.base_url)?;
        let tls = tls::client_config(&settings.tls, &settings.credentials)?;
        let builder = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .http1_only()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .tcp_keepalive(KEEPALIVE_IDLE)
            .tcp_keepalive_interval(KEEPALIVE_INTERVAL)
            .tcp_keepalive_retries(KEEPALIVE_RETRIES)
            .tcp_nodelay(true)
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .user_agent(concat!("icygui/", env!("CARGO_PKG_VERSION")));
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        let builder = builder.tcp_user_timeout(TCP_USER_TIMEOUT);
        let http = builder
            .build()
            .map_err(|error| ApiError::InvalidSettings(format!("HTTP client: {error}")))?;
        let basic = match settings.credentials {
            Credentials::Basic { username, password } => Some((username, password)),
            Credentials::ClientCertificate { .. } => None,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                base,
                basic,
                timeout: settings.request_timeout,
            }),
        })
    }

    /// The API base URL (path ending in `/`).
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.inner.base
    }

    /// `GET /v1`: the API user, its permissions and the Icinga version.
    /// Also the cheapest way to check credentials.
    ///
    /// # Errors
    ///
    /// Transport, TLS and HTTP errors as [`ApiError`]; [`ApiError::Decode`]
    /// if the response isn't Icinga's JSON.
    pub async fn info(&self) -> Result<ApiInfo, ApiError> {
        let request = self.request(reqwest::Method::GET, "v1")?;
        let results: Results<InfoResult> = self.send_json(request).await?;
        let info = first(results, "/v1")?;
        Ok(ApiInfo {
            user: info.user.0,
            permissions: info.permissions.0,
            version: info.version.0,
        })
    }

    /// Instance status from `/v1/status/IcingaApplication` and
    /// `/v1/status/CIB`.
    ///
    /// # Errors
    ///
    /// Transport, TLS and HTTP errors as [`ApiError`] (`Forbidden` without
    /// `status/query`), from either request; [`ApiError::Decode`] for
    /// unexpected JSON.
    pub async fn status(&self) -> Result<InstanceStatus, ApiError> {
        let (application, cib) = futures::try_join!(
            self.status_entry("IcingaApplication"),
            self.status_entry("CIB")
        )?;
        Ok(wire::instance_status(&application, &cib))
    }

    async fn status_entry(&self, name: &str) -> Result<Value, ApiError> {
        let request = self.request(reqwest::Method::GET, &format!("v1/status/{name}"))?;
        let results: Results<StatusResult> = self.send_json(request).await?;
        let entry = results
            .results
            .into_iter()
            .find(|entry| entry.name.0.is_empty() || entry.name.0 == name)
            .ok_or_else(|| ApiError::Decode(format!("/v1/status/{name} returned no entry")))?;
        Ok(entry.status.0.unwrap_or(Value::Null))
    }

    /// All hosts.
    ///
    /// # Errors
    ///
    /// Transport, TLS and HTTP errors as [`ApiError`]; [`ApiError::Decode`]
    /// for unexpected JSON. Single unusable objects are skipped, not errors.
    pub async fn hosts(&self) -> Result<Vec<Host>, ApiError> {
        let results = self
            .query::<CheckableAttrs>("hosts", None, wire::HOST_ATTRS)
            .await?;
        Ok(map_results(results, "host", CheckableAttrs::into_host))
    }

    /// All services.
    ///
    /// # Errors
    ///
    /// As [`Client::hosts`].
    pub async fn services(&self) -> Result<Vec<Service>, ApiError> {
        let results = self
            .query::<CheckableAttrs>("services", None, wire::SERVICE_ATTRS)
            .await?;
        Ok(map_results(
            results,
            "service",
            CheckableAttrs::into_service,
        ))
    }

    /// All comments.
    ///
    /// # Errors
    ///
    /// As [`Client::hosts`].
    pub async fn comments(&self) -> Result<Vec<Comment>, ApiError> {
        let results = self
            .query::<CommentAttrs>("comments", None, wire::COMMENT_ATTRS)
            .await?;
        Ok(map_results(results, "comment", |attrs, name| {
            attrs.into_model(name)
        }))
    }

    /// All downtimes; `in_effect` is computed for the current time.
    ///
    /// # Errors
    ///
    /// As [`Client::hosts`].
    pub async fn downtimes(&self) -> Result<Vec<Downtime>, ApiError> {
        let results = self
            .query::<DowntimeAttrs>("downtimes", None, wire::DOWNTIME_ATTRS)
            .await?;
        let now = Timestamp::now();
        Ok(map_results(results, "downtime", |attrs, name| {
            attrs.into_model(name, now)
        }))
    }

    /// All host groups.
    ///
    /// # Errors
    ///
    /// As [`Client::hosts`].
    pub async fn host_groups(&self) -> Result<Vec<HostGroup>, ApiError> {
        let results = self
            .query::<GroupAttrs>("hostgroups", None, wire::GROUP_ATTRS)
            .await?;
        Ok(map_results(
            results,
            "host group",
            GroupAttrs::into_host_group,
        ))
    }

    /// All service groups.
    ///
    /// # Errors
    ///
    /// As [`Client::hosts`].
    pub async fn service_groups(&self) -> Result<Vec<ServiceGroup>, ApiError> {
        let results = self
            .query::<GroupAttrs>("servicegroups", None, wire::GROUP_ATTRS)
            .await?;
        Ok(map_results(
            results,
            "service group",
            GroupAttrs::into_service_group,
        ))
    }

    /// All dependencies.
    ///
    /// # Errors
    ///
    /// As [`Client::hosts`].
    pub async fn dependencies(&self) -> Result<Vec<Dependency>, ApiError> {
        let results = self
            .query::<DependencyAttrs>("dependencies", None, wire::DEPENDENCY_ATTRS)
            .await?;
        Ok(map_results(results, "dependency", |attrs, name| {
            attrs.into_model(name)
        }))
    }

    /// All endpoints, with the zone each belongs to (from the zones'
    /// `endpoints`; without permission to query zones, the endpoint's own
    /// `zone` attribute).
    ///
    /// Icinga reports its own endpoint, the one the client talks to, as not
    /// connected (it has no connection to itself). That endpoint, named
    /// like the node in `/v1/status/IcingaApplication`, counts as
    /// connected; without `status/query` permission Icinga's flag is kept.
    ///
    /// # Errors
    ///
    /// As [`Client::hosts`].
    pub async fn endpoints(&self) -> Result<Vec<Endpoint>, ApiError> {
        let (endpoints, zones, application) = futures::join!(
            self.query::<EndpointAttrs>("endpoints", None, wire::ENDPOINT_ATTRS),
            self.query::<ZoneAttrs>("zones", None, wire::ZONE_ATTRS),
            self.status_entry("IcingaApplication"),
        );
        let endpoints = endpoints?;
        let zones = match zones {
            Ok(zones) => zones,
            Err(error @ (ApiError::Forbidden(_) | ApiError::NotFound(_))) => {
                tracing::debug!(%error, "zones unavailable; using the endpoints' zone attribute");
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        let local = match application {
            Ok(status) => wire::node_name(&status),
            Err(error) => {
                tracing::debug!(%error, "node name unavailable; keeping Icinga's connected flags");
                None
            }
        };
        let members: Vec<(String, Vec<String>)> = zones
            .into_iter()
            .filter_map(|zone| Some((zone.name.0, zone.attrs?.endpoints.0)))
            .collect();
        let zone_of = |endpoint: &str| {
            members
                .iter()
                .find(|(_, endpoints)| endpoints.iter().any(|name| name == endpoint))
                .map(|(zone, _)| zone.clone())
        };
        Ok(map_results(endpoints, "endpoint", |attrs, name| {
            attrs.into_model(name, zone_of, local.as_deref())
        }))
    }

    /// Re-queries specific hosts and services by name, in batches of
    /// [`NAMES_PER_REQUEST`]. Objects that no longer exist are left out
    /// (a batch with an unknown name is split until the unknown names are
    /// isolated).
    ///
    /// # Errors
    ///
    /// As [`Client::hosts`].
    pub async fn objects(&self, keys: &[ObjectKey]) -> Result<(Vec<Host>, Vec<Service>), ApiError> {
        let (host_names, service_names) = actions::names_by_kind(keys);
        let (hosts, services) = futures::try_join!(
            self.query_names::<CheckableAttrs>("hosts", "hosts", host_names, wire::HOST_ATTRS),
            self.query_names::<CheckableAttrs>(
                "services",
                "services",
                service_names,
                wire::SERVICE_ATTRS
            ),
        )?;
        Ok((
            map_results(hosts, "host", CheckableAttrs::into_host),
            map_results(services, "service", CheckableAttrs::into_service),
        ))
    }

    /// Runs an action (`POST /v1/actions/<name>`) on its target. Hosts and
    /// services go in separate requests; names are batched. `author` is
    /// sent for acknowledgements, downtimes, comments and removals.
    ///
    /// A [`ActionTarget::Downtime`] removes that downtime and a
    /// [`ActionTarget::Comment`] removes that comment; both only with
    /// [`Action::RemoveAllDowntimes`] (the model has no separate "remove
    /// comment" action).
    ///
    /// Per-object failures (`code >= 400`) are returned as results, not as
    /// errors, whatever HTTP status Icinga derives from them (it answers a
    /// single object's 409 with HTTP 409, and several different failures
    /// with 500). Objects that no longer exist get a 404 result.
    /// `process-check-result` on hosts maps the plugin exit status to UP
    /// (0–1) or DOWN (2–3), the only values Icinga accepts for hosts.
    ///
    /// If a request fails as a whole after earlier requests of the action
    /// were answered (and may have been applied), the action still returns
    /// `Ok`: that request's objects, and those not sent yet, get failure
    /// results carrying the error (see [`ActionResult::code`]); nothing
    /// more is sent.
    ///
    /// # Errors
    ///
    /// When the first request fails as a whole: transport, TLS and HTTP
    /// errors as [`ApiError`] (`Forbidden` with Icinga's "Missing
    /// permission: …"). [`ApiError::InvalidSettings`] for a downtime or
    /// comment target with another action.
    pub async fn run_action(
        &self,
        action: &Action,
        target: &ActionTarget,
        author: &str,
    ) -> Result<Vec<ActionResult>, ApiError> {
        let requests: Vec<Batch> = actions::plan(action, target)?
            .into_iter()
            .flat_map(|batch| batch.chunks(NAMES_PER_REQUEST))
            .collect();
        let mut run = ActionRun::default();
        let mut requests = requests.into_iter();
        while let Some(batch) = requests.next() {
            let Err(failure) = self.action_batch(action, batch, author, &mut run).await else {
                continue;
            };
            if !run.answered {
                return Err(failure.error);
            }
            let error = failure.error;
            tracing::warn!(
                %error,
                action = action.api_name(),
                "an action request failed after earlier ones were answered"
            );
            let code = error.http_status().unwrap_or(0);
            run.fail(failure.names, code, &format!("request failed: {error}"));
            run.fail(
                requests.by_ref().flat_map(|batch| batch.names),
                code,
                &format!("not sent: an earlier request failed: {error}"),
            );
            break;
        }
        Ok(run.results)
    }

    /// Sends one request of an action. A 404 "No objects found." means a
    /// name in it no longer exists; Icinga resolves every target before it
    /// runs anything, so nothing was applied: the request is split until
    /// the vanished names are isolated, and they get a 404 result.
    async fn action_batch(
        &self,
        action: &Action,
        batch: Batch,
        author: &str,
        run: &mut ActionRun,
    ) -> Result<(), BatchFailure> {
        let mut pending = vec![batch.names];
        while let Some(names) = pending.pop() {
            let body = actions::body(action, batch.kind, &names, author);
            match self.send_action(batch.endpoint, &body).await {
                Ok(results) => run.answer(&names, results),
                Err(ApiError::NotFound(message)) if is_no_objects(&message) => {
                    if let [name] = names.as_slice() {
                        tracing::debug!(%name, action = batch.endpoint, "action target no longer exists");
                        run.fail([name.clone()], 404, &message);
                    } else if matches!(batch.kind, TargetKind::Host | TargetKind::Service) {
                        let (first, second) = names.split_at(names.len() / 2);
                        pending.push(second.to_vec());
                        pending.push(first.to_vec());
                    }
                }
                Err(error) => {
                    // This request's names, then the rest in sending order.
                    let names = names
                        .into_iter()
                        .chain(pending.into_iter().rev().flatten())
                        .collect();
                    return Err(BatchFailure { error, names });
                }
            }
        }
        Ok(())
    }

    /// `POST /v1/actions/<endpoint>`.
    ///
    /// Icinga sets the HTTP status from the per-object codes
    /// (`actionshandler.cpp`): the code itself when all objects share one,
    /// the single failure code when there is one (even next to successes),
    /// 500 for several different failures. So the body decides: per-object
    /// `results` come back whatever the status, and only Icinga's error
    /// document (`{"error": …, "status": …}`: unknown action, missing
    /// permission, "No objects found.", shutting down) or an unreadable
    /// body is an error.
    async fn send_action(
        &self,
        endpoint: &str,
        body: &Value,
    ) -> Result<Vec<ActionResultWire>, ApiError> {
        let request = self
            .request(reqwest::Method::POST, &format!("v1/actions/{endpoint}"))?
            .json(body);
        let response = self.send(request).await?;
        let status = response.status();
        // An action response is bounded by its request (200 names), so it
        // is read whole whatever the status: a failure can carry 200
        // results.
        let body = match read_body(response, self.inner.timeout, usize::MAX).await {
            Ok(body) => body,
            Err(error) if status.is_success() => return Err(error),
            Err(error) => {
                tracing::debug!(%error, "couldn't read an action's error response");
                Vec::new()
            }
        };
        let parsed = serde_json::from_slice::<Results<ActionResultWire>>(&body);
        if status.is_success() {
            return parsed
                .map(|results| results.results)
                .map_err(|error| ApiError::Decode(error.to_string()));
        }
        match parsed {
            Ok(Results { results }) if !results.is_empty() => {
                tracing::debug!(
                    status = status.as_u16(),
                    endpoint,
                    "action failed for some objects"
                );
                Ok(results)
            }
            _ => {
                let error = ApiError::from_status(status.as_u16(), &body);
                tracing::debug!(status = status.as_u16(), %error, "action request failed");
                Err(error)
            }
        }
    }

    /// Opens the event stream (`POST /v1/events`) for `kinds`. `queue`
    /// names the subscription: Icinga 2.15 refuses a request without one
    /// (newer versions ignore it), and older versions split events between
    /// connections sharing a name, so make it unique per connection.
    ///
    /// Subscribe only to kinds the API user may see
    /// (`ApiInfo::allows(&format!("events/{}", kind.api_name()))`): Icinga
    /// refuses the whole stream if one `events/<type>` permission is
    /// missing.
    ///
    /// The response must begin within the request timeout; after that the
    /// stream has no read timeout (TCP keepalive notices a dead connection)
    /// and ends when the connection closes.
    ///
    /// # Errors
    ///
    /// - [`ApiError::InvalidSettings`] if `kinds` is empty or `queue` blank;
    /// - [`ApiError::Forbidden`] if an `events/<type>` permission is
    ///   missing, naming the missing ones when `GET /v1` can tell (Icinga
    ///   itself answers with its generic 404, "The requested path
    ///   'v1/events' could not be found …", to reveal nothing);
    /// - [`ApiError::Timeout`] if the server doesn't answer within the
    ///   request timeout;
    /// - other transport, TLS and HTTP errors as [`ApiError`].
    pub async fn events(&self, queue: &str, kinds: &[EventKind]) -> Result<EventStream, ApiError> {
        if kinds.is_empty() {
            return Err(ApiError::InvalidSettings(
                "subscribe to at least one event type".to_owned(),
            ));
        }
        if queue.trim().is_empty() {
            return Err(ApiError::InvalidSettings(
                "the event stream needs a queue name (Icinga 2.15 requires one)".to_owned(),
            ));
        }
        let types: Vec<&str> = kinds.iter().map(|kind| kind.api_name()).collect();
        let body = json!({ "queue": queue, "types": types });
        let request = self
            .request(reqwest::Method::POST, "v1/events")?
            .json(&body);
        let response = self.send(request).await?;
        let response = match check_status(response, self.inner.timeout).await {
            Ok(response) => response,
            Err(error) => return Err(self.explain_events_error(error, kinds).await),
        };
        let chunks = futures::stream::unfold(Some(response), |response| async move {
            let mut response = response?;
            match response.chunk().await {
                Ok(Some(chunk)) => Some((Ok(chunk), Some(response))),
                Ok(None) => None,
                Err(error) => Some((Err(ApiError::from_reqwest(&error)), None)),
            }
        });
        Ok(EventStream::new(chunks.boxed()))
    }

    /// Icinga's events handler lets a missing `events/<type>` permission
    /// escape as an exception, and its HTTP layer answers every escaped
    /// exception with the generic 404 for unknown paths, on purpose, to
    /// reveal nothing. Turn that back into `Forbidden`, naming the missing
    /// permissions when the user's permissions can be read.
    async fn explain_events_error(&self, error: ApiError, kinds: &[EventKind]) -> ApiError {
        let ApiError::NotFound(message) = &error else {
            return error;
        };
        if !is_hidden_events_error(message) {
            return error;
        }
        let required: Vec<String> = kinds
            .iter()
            .map(|kind| format!("events/{}", kind.api_name()))
            .collect();
        match self.info().await {
            Ok(info) => {
                let missing: Vec<&str> = required
                    .iter()
                    .filter(|permission| !info.allows(permission))
                    .map(String::as_str)
                    .collect();
                if missing.is_empty() {
                    // Not a permission problem after all.
                    error
                } else {
                    ApiError::Forbidden(format!("Missing permission: {}", missing.join(", ")))
                }
            }
            Err(probe) => {
                tracing::debug!(error = %probe, "couldn't read the API user's permissions");
                ApiError::Forbidden(format!(
                    "Missing permission: one of {}",
                    required.join(", ")
                ))
            }
        }
    }

    /// A request with authentication and `Accept: application/json`
    /// (exactly; Icinga answers `GET /v1` with HTML otherwise). Timeouts
    /// are applied by [`Client::send`] and [`read_body`], not here, so the
    /// event stream has no read timeout.
    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, ApiError> {
        let url = self
            .inner
            .base
            .join(path)
            .map_err(|error| ApiError::InvalidSettings(format!("URL for {path}: {error}")))?;
        let mut request = self
            .inner
            .http
            .request(method, url)
            .header(ACCEPT, HeaderValue::from_static("application/json"));
        if let Some((username, password)) = &self.inner.basic {
            request = request.basic_auth(username, Some(password.expose_secret()));
        }
        Ok(request)
    }

    /// Sends a request; done when the response headers have arrived, which
    /// must happen within the request timeout (connecting included).
    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response, ApiError> {
        tokio::time::timeout(self.inner.timeout, request.send())
            .await
            .map_err(|_| ApiError::Timeout)?
            .map_err(|error| ApiError::from_reqwest(&error))
    }

    /// Sends a request and parses its JSON body; non-success statuses are
    /// errors.
    async fn send_json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, ApiError> {
        let response = self.send(request).await?;
        let response = check_status(response, self.inner.timeout).await?;
        let body = read_body(response, self.inner.timeout, usize::MAX).await?;
        serde_json::from_slice(&body).map_err(|error| ApiError::Decode(error.to_string()))
    }

    /// `POST /v1/objects/<plural>` with `X-HTTP-Method-Override: GET`.
    ///
    /// If entries come back as per-object errors (an attribute this Icinga
    /// version doesn't have), the query is repeated once without `attrs`
    /// (all attributes) so older versions still work.
    async fn query<A: DeserializeOwned>(
        &self,
        plural: &str,
        names: Option<(&str, &[String])>,
        attrs: &[&str],
    ) -> Result<Vec<QueryResult<A>>, ApiError> {
        let results = self.query_once::<A>(plural, names, Some(attrs)).await?;
        let failed = results.iter().filter(|entry| entry.attrs.is_none()).count();
        if failed == 0 || attrs.is_empty() {
            return Ok(results);
        }
        if let Some(entry) = results.iter().find(|entry| entry.attrs.is_none()) {
            tracing::warn!(
                plural,
                failed,
                code = ?entry.code.0,
                status = %entry.status.0,
                "object query entries failed; retrying with all attributes"
            );
        }
        let retried = self.query_once::<A>(plural, names, None).await?;
        let still_failed = retried.iter().filter(|entry| entry.attrs.is_none()).count();
        if still_failed > 0 {
            tracing::warn!(
                plural,
                still_failed,
                "skipping objects the server couldn't serialise"
            );
        }
        Ok(retried)
    }

    async fn query_once<A: DeserializeOwned>(
        &self,
        plural: &str,
        names: Option<(&str, &[String])>,
        attrs: Option<&[&str]>,
    ) -> Result<Vec<QueryResult<A>>, ApiError> {
        let mut body = serde_json::Map::new();
        if let Some(attrs) = attrs {
            body.insert("attrs".to_owned(), json!(attrs));
        }
        if let Some((key, names)) = names {
            body.insert(key.to_owned(), json!(names));
        }
        let request = self
            .request(reqwest::Method::POST, &format!("v1/objects/{plural}"))?
            .header("X-HTTP-Method-Override", HeaderValue::from_static("GET"))
            .json(&Value::Object(body));
        let results: Results<QueryResult<A>> = self.send_json(request).await?;
        Ok(results.results)
    }

    /// A targeted query in batches. A 404 ("No objects found.") means a
    /// name in the batch is unknown: the batch is split until the unknown
    /// names are isolated, and those are left out (deleted objects). An
    /// empty name list sends nothing (Icinga would return every object).
    async fn query_names<A: DeserializeOwned>(
        &self,
        plural: &str,
        key: &str,
        names: Vec<String>,
        attrs: &[&str],
    ) -> Result<Vec<QueryResult<A>>, ApiError> {
        let mut results = Vec::new();
        let mut pending: Vec<Vec<String>> = names
            .chunks(NAMES_PER_REQUEST)
            .rev()
            .map(<[String]>::to_vec)
            .collect();
        while let Some(batch) = pending.pop() {
            if batch.is_empty() {
                continue;
            }
            match self.query::<A>(plural, Some((key, &batch)), attrs).await {
                Ok(found) => results.extend(found),
                Err(ApiError::NotFound(message)) if is_no_objects(&message) => {
                    if let [name] = batch.as_slice() {
                        tracing::debug!(%name, plural, "object no longer exists");
                    } else {
                        let (first, second) = batch.split_at(batch.len() / 2);
                        pending.push(second.to_vec());
                        pending.push(first.to_vec());
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(results)
    }
}

/// The results of one [`Client::run_action`] so far.
#[derive(Default)]
struct ActionRun {
    results: Vec<ActionResult>,
    /// Whether Icinga answered a request with per-object results, so that
    /// something may have been applied.
    answered: bool,
}

impl ActionRun {
    /// Icinga's per-object results for `names` (matched by order when the
    /// counts agree).
    fn answer(&mut self, names: &[String], results: Vec<ActionResultWire>) {
        self.answered = true;
        let aligned = results.len() == names.len();
        self.results.extend(
            results
                .into_iter()
                .enumerate()
                .map(|(index, result)| ActionResult {
                    code: status_code(result.code.0),
                    status: result.status.0,
                    name: non_empty(result.name.0),
                    target: if aligned {
                        names.get(index).cloned()
                    } else {
                        None
                    },
                }),
        );
    }

    /// The same failure for each of `names`.
    fn fail(&mut self, names: impl IntoIterator<Item = String>, code: u16, status: &str) {
        self.results
            .extend(names.into_iter().map(|name| ActionResult {
                code,
                status: status.to_owned(),
                name: None,
                target: Some(name),
            }));
    }
}

/// An action request that failed as a whole, with the names it and the
/// rest of its batch carried.
struct BatchFailure {
    error: ApiError,
    names: Vec<String>,
}

/// Fails with the mapped error for a non-success status. Only the start of
/// the error body is read, within the request timeout: the status is what
/// matters, so a body that can't be read only costs the message.
async fn check_status(
    response: reqwest::Response,
    idle: Duration,
) -> Result<reqwest::Response, ApiError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = read_body(response, idle, MAX_ERROR_BODY)
        .await
        .unwrap_or_default();
    let error = ApiError::from_status(status.as_u16(), &body);
    tracing::debug!(status = status.as_u16(), %error, "request failed");
    Err(error)
}

/// Reads a response body, up to `limit` bytes: it may pause at most `idle`
/// between reads and take at most [`BODY_TIME_FACTOR`] × `idle` in total.
/// So a large object list over a slow link takes as long as it needs while
/// data keeps coming, and a stalled response fails after `idle`.
async fn read_body(
    mut response: reqwest::Response,
    idle: Duration,
    limit: usize,
) -> Result<Vec<u8>, ApiError> {
    let read = async move {
        let mut body = Vec::new();
        while body.len() < limit {
            let chunk = tokio::time::timeout(idle, response.chunk())
                .await
                .map_err(|_| ApiError::Timeout)?
                .map_err(|error| ApiError::from_reqwest(&error))?;
            let Some(chunk) = chunk else {
                break;
            };
            body.extend_from_slice(&chunk);
        }
        body.truncate(limit);
        Ok(body)
    };
    tokio::time::timeout(idle.saturating_mul(BODY_TIME_FACTOR), read)
        .await
        .unwrap_or(Err(ApiError::Timeout))
}

fn normalize_base(mut url: Url) -> Result<Url, ApiError> {
    if url.scheme() != "https" {
        return Err(ApiError::InvalidSettings(format!(
            "the API URL must use https, not {}",
            url.scheme()
        )));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(ApiError::InvalidSettings(
            "the API URL has no host".to_owned(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::InvalidSettings(
            "put credentials in the settings, not in the API URL".to_owned(),
        ));
    }
    url.set_query(None);
    url.set_fragment(None);
    let mut path = url.path().trim_end_matches('/').to_owned();
    // Accept a URL that already ends in /v1.
    if path.ends_with("/v1") {
        path.truncate(path.len() - 3);
    }
    path.push('/');
    url.set_path(&path);
    Ok(url)
}

fn first<T>(results: Results<T>, what: &str) -> Result<T, ApiError> {
    results
        .results
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::Decode(format!("{what} returned no results")))
}

/// Maps query entries, skipping failed or unidentifiable ones.
fn map_results<A, T>(
    results: Vec<QueryResult<A>>,
    what: &str,
    map: impl Fn(A, &str) -> Option<T>,
) -> Vec<T> {
    let mut skipped = 0_usize;
    let mapped = results
        .into_iter()
        .filter_map(|entry| {
            let name = entry.name.0;
            let mapped = entry.attrs.and_then(|attrs| map(attrs, &name));
            if mapped.is_none() {
                skipped += 1;
            }
            mapped
        })
        .collect();
    if skipped > 0 {
        tracing::warn!(what, skipped, "skipped objects that couldn't be read");
    }
    mapped
}

fn is_no_objects(message: &str) -> bool {
    message.trim().eq_ignore_ascii_case(NO_OBJECTS_FOUND)
}

/// Icinga's generic 404 for `/v1/events` ("The requested path 'v1/events'
/// could not be found or the request method is not valid for this
/// path."), which is how it reports a missing `events/<type>` permission.
/// A wrong path prefix names another path and stays a 404.
fn is_hidden_events_error(message: &str) -> bool {
    message.contains("path 'v1/events'") && message.contains("could not be found")
}

fn status_code(code: f64) -> u16 {
    let code = wire::clamp_u32(code);
    u16::try_from(code).unwrap_or(u16::MAX)
}

fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_are_normalised() {
        let url = |text: &str| normalize_base(Url::parse(text).unwrap());
        assert_eq!(
            url("https://icinga:5665").unwrap().as_str(),
            "https://icinga:5665/"
        );
        assert_eq!(
            url("https://icinga:5665/").unwrap().as_str(),
            "https://icinga:5665/"
        );
        assert_eq!(
            url("https://icinga:5665/v1").unwrap().as_str(),
            "https://icinga:5665/"
        );
        assert_eq!(
            url("https://proxy/icinga-api/v1/?x=1#y").unwrap().as_str(),
            "https://proxy/icinga-api/"
        );
        assert!(matches!(
            url("http://icinga:5665"),
            Err(ApiError::InvalidSettings(_))
        ));
        assert!(matches!(
            url("https://user:pw@icinga:5665"),
            Err(ApiError::InvalidSettings(_))
        ));
        let joined = url("https://proxy/icinga-api")
            .unwrap()
            .join("v1/objects/hosts")
            .unwrap();
        assert_eq!(joined.as_str(), "https://proxy/icinga-api/v1/objects/hosts");
    }

    #[test]
    fn no_objects_detection() {
        assert!(is_no_objects("No objects found."));
        assert!(!is_no_objects("Action 'execute-command' does not exist."));
    }

    #[test]
    fn hidden_event_permission_errors() {
        assert!(is_hidden_events_error(
            "The requested path 'v1/events' could not be found or the request method is not valid for this path."
        ));
        assert!(!is_hidden_events_error(
            "The requested path 'icinga/v1/events' could not be found or the request method is not valid for this path."
        ));
        assert!(!is_hidden_events_error("No objects found."));
    }

    #[test]
    fn status_codes_are_clamped() {
        assert_eq!(status_code(200.0), 200);
        assert_eq!(status_code(-5.0), 0);
        assert_eq!(status_code(1e9), u16::MAX);
    }

    #[test]
    fn action_runs_match_results_to_names_by_order() {
        let mut run = ActionRun::default();
        let wire = |code: f64| ActionResultWire {
            code: crate::lenient::L(code),
            ..ActionResultWire::default()
        };
        let names = ["a".to_owned(), "b".to_owned()];
        run.answer(&names, vec![wire(200.0), wire(409.0)]);
        assert!(run.answered);
        assert_eq!(run.results[1].target.as_deref(), Some("b"));
        assert_eq!(run.results[1].code, 409);
        // A count mismatch can't be matched.
        run.answer(&names, vec![wire(200.0)]);
        assert_eq!(run.results[2].target, None);
        run.fail(["c".to_owned()], 0, "request failed: request timed out");
        assert_eq!(run.results[3].target.as_deref(), Some("c"));
        assert!(!run.results[3].is_success());
    }
}
