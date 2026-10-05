//! Response builders: JSON bodies and Icinga's error envelope.

use bytes::Bytes;
use http::header::{CONTENT_TYPE, HeaderValue};
use http::{Response, StatusCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt as _, Full};
use serde_json::{Map, Value as Json};

use super::params::Params;
use crate::config::NumberFormat;
use crate::json::{self, int};

/// Response body: a full buffer or an event stream.
pub(crate) type Body = BoxBody<Bytes, std::io::Error>;

pub(crate) fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed()
}

fn status(code: u16) -> StatusCode {
    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

/// A JSON response.
pub(crate) fn json(code: u16, value: Json, format: NumberFormat, pretty: bool) -> Response<Body> {
    json_bytes(code, json::encode(value, format, pretty))
}

/// A JSON response from encoded bytes.
pub(crate) fn json_bytes(code: u16, body: Vec<u8>) -> Response<Body> {
    let mut response = Response::new(full(body));
    *response.status_mut() = status(code);
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

/// Marks a response `HttpServerConnection` sends itself: it carries no
/// `Server` header.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bare;

/// The 500 of an exception that escapes the handlers (and their error
/// handling): Icinga's request parameters `pretty` and `verbose` that
/// can't be converted to booleans end up here.
pub(crate) fn unhandled_exception(format: NumberFormat) -> Response<Body> {
    let mut body = Map::new();
    body.insert("error".into(), int(500));
    body.insert("status".into(), Json::String("Unhandled exception".into()));
    let mut response = json(500, Json::Object(body), format, false);
    response.extensions_mut().insert(Bare);
    response
}

/// `HttpUtility::SendJsonError`: `{"error": code, "status": message}`, plus
/// `diagnostic_information` when the request asked for `verbose`. A
/// `verbose` or `pretty` Icinga can't read makes it fail itself: 500.
pub(crate) fn json_error(
    code: u16,
    message: &str,
    format: NumberFormat,
    params: Option<&Params>,
    diagnostic: Option<&str>,
) -> Response<Body> {
    if params
        .is_some_and(|params| params.flag("verbose").is_err() || params.flag("pretty").is_err())
    {
        return unhandled_exception(format);
    }
    let mut body = Map::new();
    body.insert("error".into(), int(code));
    if !message.is_empty() {
        body.insert("status".into(), Json::String(message.to_owned()));
    }
    if let (Some(diagnostic), Some(params)) = (diagnostic, params)
        && params.verbose()
    {
        body.insert(
            "diagnostic_information".into(),
            Json::String(diagnostic.to_owned()),
        );
    }
    json(
        code,
        Json::Object(body),
        format,
        params.is_some_and(Params::pretty),
    )
}

/// An HTML response (Icinga answers browsers in HTML).
pub(crate) fn html(code: u16, body: String) -> Response<Body> {
    let mut response = Response::new(full(body));
    *response.status_mut() = status(code);
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/html"));
    response
}

/// The 404 `HttpHandler` sends when no handler takes a request (also for
/// permission exceptions handlers don't catch).
pub(crate) fn path_not_found(
    segments: &[String],
    format: NumberFormat,
    params: Option<&Params>,
) -> Response<Body> {
    json_error(
        404,
        &format!(
            "The requested path '{}' could not be found or the request method is not valid for this path.",
            segments.join("/")
        ),
        format,
        params,
        None,
    )
}
