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

/// TCP keepalive for all connections, so a dead event stream is noticed
/// even though it has no read timeout.
const TCP_KEEPALIVE: Duration = Duration::from_secs(30);

/// The result of an action for one object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionResult {
    /// Icinga's per-object code: 200 (done), 202 (accepted, for
    /// `execute-command`), 4xx/5xx (failed for this object).
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
    ///
    /// # Errors
    ///
    /// [`ApiError::InvalidSettings`] if the URL isn't `https` with a host,
    /// or a PEM (CA, client certificate or key) can't be parsed.
    pub fn new(settings: ConnectionSettings) -> Result<Self, ApiError> {
        let base = normalize_base(settings.base_url)?;
        let tls = tls::client_config(&settings.tls, &settings.credentials)?;
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .http1_only()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .tcp_keepalive(TCP_KEEPALIVE)
            .tcp_nodelay(true)
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .user_agent(concat!("icygui/", env!("CARGO_PKG_VERSION")))
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
    /// `status/query`); [`ApiError::Decode`] for unexpected JSON.
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
    /// # Errors
    ///
    /// As [`Client::hosts`].
    pub async fn endpoints(&self) -> Result<Vec<Endpoint>, ApiError> {
        let (endpoints, zones) = futures::join!(
            self.query::<EndpointAttrs>("endpoints", None, wire::ENDPOINT_ATTRS),
            self.query::<ZoneAttrs>("zones", None, wire::ZONE_ATTRS)
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
            attrs.into_model(name, zone_of)
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
        let mut host_names: Vec<String> = Vec::new();
        let mut service_names: Vec<String> = Vec::new();
        for key in keys {
            let (list, name) = match key {
                ObjectKey::Host { name } => (&mut host_names, name.to_string()),
                ObjectKey::Service { key } => (&mut service_names, key.full_name()),
            };
            if !list.contains(&name) {
                list.push(name);
            }
        }
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
    /// A [`ActionTarget::Downtime`] removes that downtime (only with
    /// [`Action::RemoveAllDowntimes`]); a [`ActionTarget::Comment`] removes
    /// that comment, whatever the action (the model has no separate
    /// "remove comment" action).
    ///
    /// Per-object failures (`code >= 400`) are returned as results, not as
    /// errors; objects that no longer exist get a 404 result.
    ///
    /// # Errors
    ///
    /// HTTP-level failures as [`ApiError`] (`Forbidden` with Icinga's
    /// "Missing permission: …"); [`ApiError::InvalidSettings`] for a
    /// downtime target with another action.
    pub async fn run_action(
        &self,
        action: &Action,
        target: &ActionTarget,
        author: &str,
    ) -> Result<Vec<ActionResult>, ApiError> {
        let mut results = Vec::new();
        for batch in actions::plan(action, target)? {
            for chunk in batch.names.chunks(NAMES_PER_REQUEST) {
                let part = Batch {
                    names: chunk.to_vec(),
                    ..batch.clone()
                };
                results.extend(self.action_batch(action, part, author).await?);
            }
        }
        Ok(results)
    }

    async fn action_batch(
        &self,
        action: &Action,
        batch: Batch,
        author: &str,
    ) -> Result<Vec<ActionResult>, ApiError> {
        let mut results = Vec::new();
        let mut pending = vec![batch.names];
        while let Some(names) = pending.pop() {
            let body = actions::body(action, batch.kind, &names, author);
            let request = self
                .request(
                    reqwest::Method::POST,
                    &format!("v1/actions/{}", batch.endpoint),
                )?
                .json(&body);
            match self.send_json::<Results<ActionResultWire>>(request).await {
                Ok(response) => {
                    let aligned = response.results.len() == names.len();
                    results.extend(response.results.into_iter().enumerate().map(
                        |(index, result)| ActionResult {
                            code: status_code(result.code.0),
                            status: result.status.0,
                            name: non_empty(result.name.0),
                            target: aligned.then(|| names[index].clone()),
                        },
                    ));
                }
                Err(ApiError::NotFound(message)) if is_no_objects(&message) => {
                    if let [name] = names.as_slice() {
                        tracing::debug!(%name, action = batch.endpoint, "action target no longer exists");
                        results.push(ActionResult {
                            code: 404,
                            status: message,
                            name: None,
                            target: Some(name.clone()),
                        });
                    } else if matches!(batch.kind, TargetKind::Host | TargetKind::Service) {
                        let (first, second) = names.split_at(names.len() / 2);
                        pending.push(second.to_vec());
                        pending.push(first.to_vec());
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(results)
    }

    /// Opens the event stream (`POST /v1/events`) for `kinds`. `queue`
    /// names the subscription; current Icinga versions ignore it, older
    /// ones split events between connections that share a queue name, so
    /// make it unique per connection.
    ///
    /// The stream has no read timeout and ends when the connection closes.
    ///
    /// # Errors
    ///
    /// Transport, TLS and HTTP errors as [`ApiError`] (`Forbidden` if an
    /// `events/<type>` permission is missing); [`ApiError::InvalidSettings`]
    /// if `kinds` is empty; [`ApiError::Timeout`] if the server doesn't
    /// answer within the request timeout.
    pub async fn events(&self, queue: &str, kinds: &[EventKind]) -> Result<EventStream, ApiError> {
        if kinds.is_empty() {
            return Err(ApiError::InvalidSettings(
                "subscribe to at least one event type".to_owned(),
            ));
        }
        let types: Vec<&str> = kinds.iter().map(|kind| kind.api_name()).collect();
        let body = json!({ "queue": queue, "types": types });
        let request = self
            .request(reqwest::Method::POST, "v1/events")?
            .json(&body);
        let response = tokio::time::timeout(self.inner.timeout, request.send())
            .await
            .map_err(|_| ApiError::Timeout)?
            .map_err(|error| ApiError::from_reqwest(&error))?;
        let response = check_status(response).await?;
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

    /// A request with authentication and `Accept: application/json`
    /// (exactly; Icinga answers `GET /v1` with HTML otherwise). The
    /// request timeout is applied by [`Client::send_json`], not here, so
    /// the event stream has none.
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

    async fn send_json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, ApiError> {
        let response = request
            .timeout(self.inner.timeout)
            .send()
            .await
            .map_err(|error| ApiError::from_reqwest(&error))?;
        let response = check_status(response).await?;
        let body = response
            .bytes()
            .await
            .map_err(|error| ApiError::from_reqwest(&error))?;
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

/// Fails with the mapped error for a non-success status.
async fn check_status(response: reqwest::Response) -> Result<reqwest::Response, ApiError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.bytes().await.unwrap_or_default();
    let error = ApiError::from_status(status.as_u16(), &body);
    tracing::debug!(status = status.as_u16(), %error, "request failed");
    Err(error)
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
    fn status_codes_are_clamped() {
        assert_eq!(status_code(200.0), 200);
        assert_eq!(status_code(-5.0), 0);
        assert_eq!(status_code(1e9), u16::MAX);
    }
}
