//! `POST /v1/events` (`EventsHandler`): newline-delimited JSON over a
//! long-lived chunked response.

use std::collections::HashSet;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::header::{CONTENT_TYPE, HeaderValue};
use http_body::Frame;
use http_body_util::BodyExt as _;
use serde_json::Value as Json;
use tokio::sync::mpsc;

use super::params::{Params, to_icinga_string};
use super::response::{Body, json_error, path_not_found};
use crate::auth::Principal;
use crate::config::NumberFormat;
use crate::events::{EventType, StreamItem};
use crate::filter::{self, FilterError};
use crate::model::World;

/// The body of an event stream. When the bus drops the stream (overflow,
/// `drop_event_streams`, shutdown) the body fails, which makes hyper abort
/// the connection: the client sees a broken connection, as with Icinga.
struct EventBody {
    rx: mpsc::Receiver<StreamItem>,
}

impl http_body::Body for EventBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(line)) => Poll::Ready(Some(Ok(Frame::data(line)))),
            Poll::Ready(None) => Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "event stream closed by the server",
            )))),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Handles an event stream subscription.
pub(crate) fn handle(
    world: &mut World,
    user: &Principal,
    params: &Params,
    segments: &[String],
    http10: bool,
    format: NumberFormat,
    enforce_filter_permission: bool,
) -> hyper::Response<Body> {
    if http10 {
        return json_error(
            400,
            "HTTP/1.0 not supported for event streams.",
            format,
            Some(params),
            None,
        );
    }
    let types = match params.get("types") {
        None | Some(Json::Null) => {
            return json_error(
                400,
                "'types' query parameter is required.",
                format,
                Some(params),
                None,
            );
        }
        Some(Json::Array(types)) => types.iter().map(to_icinga_string).collect::<Vec<_>>(),
        // A non-array `types` throws in Icinga, which turns into the
        // generic 404 of the HTTP handler.
        Some(_) => return path_not_found(segments, format, Some(params)),
    };
    // Missing `events/<type>` permissions throw a `MissingPermissionError`
    // the events handler doesn't catch: the HTTP handler answers with its
    // generic 404 (it never reveals specific permission errors there).
    for name in &types {
        if !user.has_permission(&format!("events/{name}")) {
            tracing::debug!(user = %user.name, r#type = %name, "missing event permission");
            return path_not_found(segments, format, Some(params));
        }
    }
    // Icinga 2.15 requires `queue` (newer versions ignore it), so a client
    // that works here works with both.
    if params.last_string("queue").is_empty() {
        return json_error(
            400,
            "'queue' query parameter is required.",
            format,
            Some(params),
            None,
        );
    }
    let event_types: HashSet<EventType> = types
        .iter()
        .filter_map(|name| EventType::from_name(name))
        .collect();
    let compiled = if params.contains("filter") {
        if enforce_filter_permission && !user.has_permission("filter-expression") {
            return json_error(
                403,
                "Missing permission: filter-expression",
                format,
                Some(params),
                None,
            );
        }
        match filter::compile(&params.last_string("filter"), None) {
            Ok(compiled) => Some(compiled),
            Err(FilterError::Syntax(_)) => return path_not_found(segments, format, Some(params)),
            Err(error @ FilterError::Unsupported(_)) => {
                return json_error(400, &error.to_string(), format, Some(params), None);
            }
        }
    } else {
        None
    };
    let (_, rx) = world.bus.subscribe(event_types, compiled, &user.name);
    let mut response = hyper::Response::new(EventBody { rx }.boxed());
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}
