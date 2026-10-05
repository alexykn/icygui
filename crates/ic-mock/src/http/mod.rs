//! The request pipeline of `HttpServerConnection` and `HttpHandler`:
//! method override, `Accept` check, authentication, body limits, parameter
//! parsing and routing to the handlers.

pub(crate) mod actions;
mod events;
mod info;
mod objects;
pub(crate) mod params;
pub(crate) mod response;
mod targets;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http::header::{
    ACCEPT, AUTHORIZATION, CONNECTION, HeaderValue, LOCATION, SERVER, WWW_AUTHENTICATE,
};
use http::{Method, Request, Response, StatusCode, Version};
use http_body_util::{BodyExt as _, Limited};
use hyper::body::Incoming;
use serde_json::Value as Json;

use crate::auth::Principal;
use crate::control::RecordedRequest;
use crate::server::Shared;
use params::{Params, parse_query, unescape};
use response::{Body, full, html, json_error, path_not_found};

/// Per-connection facts.
#[derive(Debug)]
pub(crate) struct ConnInfo {
    /// The user of a client certificate (`ApiUser.client_cn`).
    pub(crate) cert_user: Option<Principal>,
    pub(crate) peer: SocketAddr,
}

/// Request body limit (`EnsureValidBody`): 1 MiB, 512 MiB with
/// `config/modify`.
fn body_limit(user: &Principal) -> usize {
    if user.has_permission("config/modify") {
        512 * 1024 * 1024
    } else {
        1024 * 1024
    }
}

/// Verbs `boost::beast::http::string_to_verb` knows (case-sensitive).
fn parse_verb(text: &str) -> Option<Method> {
    const VERBS: &[&str] = &[
        "DELETE",
        "GET",
        "HEAD",
        "POST",
        "PUT",
        "CONNECT",
        "OPTIONS",
        "TRACE",
        "COPY",
        "LOCK",
        "MKCOL",
        "MOVE",
        "PROPFIND",
        "PROPPATCH",
        "SEARCH",
        "UNLOCK",
        "BIND",
        "REBIND",
        "UNBIND",
        "ACL",
        "REPORT",
        "MKACTIVITY",
        "CHECKOUT",
        "MERGE",
        "MSEARCH",
        "NOTIFY",
        "SUBSCRIBE",
        "UNSUBSCRIBE",
        "PATCH",
        "PURGE",
        "MKCALENDAR",
        "LINK",
        "UNLINK",
    ];
    VERBS
        .contains(&text)
        .then(|| Method::from_bytes(text.as_bytes()).ok())
        .flatten()
}

fn close(mut response: Response<Body>) -> Response<Body> {
    response
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("close"));
    response
}

/// Serves one request and records it.
///
/// # Errors
/// Never: every failure becomes an HTTP response.
pub(crate) async fn serve(
    request: Request<Incoming>,
    shared: Arc<Shared>,
    conn: Arc<ConnInfo>,
) -> Result<Response<Body>, Infallible> {
    let mut record = RecordedRequest {
        method: request.method().to_string(),
        method_override: request
            .headers()
            .get("x-http-method-override")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        path: request.uri().path().to_owned(),
        query: request
            .uri()
            .query()
            .and_then(|q| parse_query(q).ok())
            .unwrap_or_default(),
        body: None,
        user: None,
        status: 0,
        at: ic_model::Timestamp::now(),
    };
    let mut response = pipeline(request, &shared, &conn, &mut record).await;
    record.status = response.status().as_u16();
    tracing::debug!(
        method = %record.method,
        path = %record.path,
        user = record.user.as_deref().unwrap_or("<unauthenticated>"),
        status = record.status,
        peer = %conn.peer,
        "request"
    );
    shared.record(record);
    if let Ok(value) = HeaderValue::from_str(&shared.server_header) {
        response.headers_mut().insert(SERVER, value);
    }
    Ok(response)
}

#[expect(
    clippy::too_many_lines,
    reason = "the steps of HttpServerConnection::ProcessMessages in order"
)]
async fn pipeline(
    request: Request<Incoming>,
    shared: &Shared,
    conn: &ConnInfo,
    record: &mut RecordedRequest,
) -> Response<Body> {
    let format = shared.number_format;
    let (latency, failure) = shared.take_faults();
    if !latency.is_zero() {
        tokio::time::sleep(latency).await;
    }
    if let Some(code) = failure {
        let reason = StatusCode::from_u16(code)
            .ok()
            .and_then(|s| s.canonical_reason())
            .unwrap_or("Error");
        return json_error(
            code,
            &format!("{reason} (failure injected by ic-mock)"),
            format,
            None,
            None,
        );
    }

    let accept_json = request
        .headers()
        .get(ACCEPT)
        .is_some_and(|v| v.as_bytes() == b"application/json");
    let http10 = request.version() == Version::HTTP_10;
    let query = match request.uri().query().map(parse_query).transpose() {
        Ok(query) => query.unwrap_or_default(),
        Err(message) => {
            return close(json_error(
                400,
                &format!("Bad Request: {message}"),
                format,
                None,
                None,
            ));
        }
    };
    let segments: Vec<String> = request
        .uri()
        .path()
        .split('/')
        .filter(|s| !s.is_empty())
        .map(unescape)
        .collect();

    // X-HTTP-Method-Override replaces the method before anything else.
    let method = request
        .headers()
        .get("x-http-method-override")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_verb)
        .unwrap_or_else(|| request.method().clone());

    // EnsureAcceptHeader: everything but GET needs `Accept: application/json`.
    if method != Method::GET && !accept_json {
        return close(html(
            400,
            "<h1>Accept header is missing or not set to 'application/json'.</h1>".to_owned(),
        ));
    }

    // EnsureAuthenticatedUser.
    let user = conn.cert_user.clone().or_else(|| {
        shared.users.authenticate(
            request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
        )
    });
    let Some(user) = user else {
        let mut response = if accept_json {
            json_error(
                401,
                "Unauthorized. Please check your user credentials.",
                format,
                None,
                None,
            )
        } else {
            html(
                401,
                "<h1>Unauthorized. Please check your user credentials.</h1>".to_owned(),
            )
        };
        response.headers_mut().insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"Icinga 2\""),
        );
        return close(response);
    };
    record.user = Some(user.name.clone());

    // EnsureValidBody.
    let limit = body_limit(&user);
    let body: Bytes = match Limited::new(request.into_body(), limit).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) => {
            let message = if error.is::<http_body_util::LengthLimitError>() {
                "body limit exceeded".to_owned()
            } else {
                error.to_string()
            };
            return close(if accept_json {
                json_error(400, &format!("Bad Request: {message}"), format, None, None)
            } else {
                html(
                    400,
                    format!("<h1>Bad Request</h1><p><pre>{message}</pre></p>"),
                )
            });
        }
    };
    record.body = serde_json::from_slice::<Json>(&body).ok();

    // HttpHandler::ProcessRequest: parameters, then routing.
    let params = match Params::parse(&body, &query) {
        Ok(params) => params,
        Err(message) => {
            return json_error(
                400,
                &format!("Invalid request body: {message}"),
                format,
                None,
                None,
            );
        }
    };
    let enforce = shared.enforce_filter_permission;
    let path: Vec<&str> = segments.iter().map(String::as_str).collect();
    match (path.as_slice(), &method) {
        ([], &Method::GET) => {
            let mut response = Response::new(full(Bytes::new()));
            *response.status_mut() = StatusCode::FOUND;
            response
                .headers_mut()
                .insert(LOCATION, HeaderValue::from_static("/v1"));
            response
        }
        (["v1"], &Method::GET) => {
            let world = shared.world();
            info::info(&world, &user, &params, accept_json, format)
        }
        (["v1", "status"], &Method::GET) => {
            let world = shared.world();
            info::status(&world, &user, params, None, format, enforce)
        }
        (["v1", "status", name], &Method::GET) => {
            let world = shared.world();
            info::status(&world, &user, params.clone(), Some(name), format, enforce)
        }
        (["v1", "objects", _] | ["v1", "objects", _, _], &Method::GET) => {
            let world = shared.world();
            objects::handle(&world, &user, params, &segments, format, enforce)
        }
        (["v1", "actions", action], &Method::POST) => {
            let mut world = shared.world();
            actions::handle(&mut world, &user, &params, action, format, enforce)
        }
        (["v1", "events"], &Method::POST) => {
            let mut world = shared.world();
            events::handle(
                &mut world, &user, &params, &segments, http10, format, enforce,
            )
        }
        _ => path_not_found(&segments, format, Some(&params)),
    }
}
