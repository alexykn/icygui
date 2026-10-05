//! Errors returned by the client, and their mapping from transport, TLS and
//! HTTP failures.

use std::error::Error as StdError;

use crate::tls::{PinMismatch, format_fingerprint};

/// Everything that can go wrong talking to Icinga.
///
/// Per-object failures of an action (`code >= 400` inside `results`) are not
/// errors: they come back as [`crate::ActionResult`]s, whatever HTTP status
/// Icinga derived from them.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// The TCP connection failed or broke (refused, reset, DNS, closed
    /// mid-response).
    #[error("connection failed: {0}")]
    Connect(String),
    /// The TLS handshake failed: untrusted certificate, wrong name, protocol
    /// error, or the server refused the client certificate.
    #[error("TLS error: {0}")]
    Tls(String),
    /// The server's certificate doesn't match the pinned fingerprint. Both
    /// fingerprints are SHA-256, colon-separated uppercase hex.
    #[error("server certificate {actual} does not match the pinned certificate {expected}")]
    CertificateMismatch {
        /// The pinned fingerprint.
        expected: String,
        /// The fingerprint of the certificate the server presented.
        actual: String,
    },
    /// HTTP 401: wrong username/password, or no API user for the client
    /// certificate.
    #[error("unauthorized: check the API user's credentials")]
    Unauthorized,
    /// HTTP 403: the API user lacks a permission. Carries Icinga's message
    /// (`Missing permission: actions/acknowledge-problem`). Also used for a
    /// missing `events/<type>` permission, which Icinga hides behind a 404
    /// (see [`crate::Client::events`]).
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// HTTP 404: the endpoint or the named objects don't exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// Any other non-success HTTP status.
    #[error("HTTP {status}: {message}")]
    Http {
        /// The status code.
        status: u16,
        /// Icinga's `status` text, or the start of the body.
        message: String,
    },
    /// The response wasn't the JSON we expected.
    #[error("unexpected response: {0}")]
    Decode(String),
    /// The request timed out.
    #[error("request timed out")]
    Timeout,
    /// The connection settings are unusable (bad URL, unreadable PEM, …), or
    /// the request is invalid (an action that can't apply to its target).
    #[error("invalid settings: {0}")]
    InvalidSettings(String),
}

impl ApiError {
    /// Whether retrying later may help: connection problems, timeouts and
    /// server-side errors (5xx, 408, 429). Authentication, permission, TLS
    /// and decoding errors need a human.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Connect(_) | Self::Timeout => true,
            Self::Http { status, .. } => *status >= 500 || *status == 408 || *status == 429,
            Self::Tls(_)
            | Self::CertificateMismatch { .. }
            | Self::Unauthorized
            | Self::Forbidden(_)
            | Self::NotFound(_)
            | Self::Decode(_)
            | Self::InvalidSettings(_) => false,
        }
    }

    /// The HTTP status the error came from, if any.
    pub(crate) fn http_status(&self) -> Option<u16> {
        match self {
            Self::Unauthorized => Some(401),
            Self::Forbidden(_) => Some(403),
            Self::NotFound(_) => Some(404),
            Self::Http { status, .. } => Some(*status),
            Self::Connect(_)
            | Self::Tls(_)
            | Self::CertificateMismatch { .. }
            | Self::Decode(_)
            | Self::Timeout
            | Self::InvalidSettings(_) => None,
        }
    }

    /// Maps an HTTP error status and its body to an error. Icinga's error
    /// bodies are `{"error": 403, "status": "Missing permission: …"}`.
    pub(crate) fn from_status(status: u16, body: &[u8]) -> Self {
        let message = error_message(body);
        match status {
            401 => Self::Unauthorized,
            403 => Self::Forbidden(message),
            404 => Self::NotFound(message),
            _ => Self::Http { status, message },
        }
    }

    /// Classifies a transport error, looking through its source chain for
    /// TLS failures (which reqwest reports as connect errors).
    pub(crate) fn from_reqwest(error: &reqwest::Error) -> Self {
        if let Some(tls) = find_tls_error(error) {
            return tls;
        }
        if error.is_timeout() || has_timed_out_io(error) {
            return Self::Timeout;
        }
        if error.is_builder() {
            return Self::InvalidSettings(chain_message(error));
        }
        // Everything else is the transport: refused, reset, DNS, or the
        // connection dropping mid-body (reqwest calls a failed body read a
        // "decode" error; we parse JSON ourselves, so it never is one).
        Self::Connect(chain_message(error))
    }

    /// Maps a rustls error, recognising a pin mismatch from our verifier.
    pub(crate) fn from_rustls(error: &rustls::Error) -> Self {
        if let rustls::Error::InvalidCertificate(rustls::CertificateError::Other(other)) = error
            && let Some(mismatch) = other.0.downcast_ref::<PinMismatch>()
        {
            return Self::CertificateMismatch {
                expected: format_fingerprint(&mismatch.expected),
                actual: format_fingerprint(&mismatch.actual),
            };
        }
        Self::Tls(error.to_string())
    }

    /// Maps an I/O error from a raw TLS connection (used by
    /// [`crate::fetch_server_certificate`]).
    pub(crate) fn from_io(error: &std::io::Error) -> Self {
        if let Some(rustls_error) = error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        {
            return Self::from_rustls(rustls_error);
        }
        if error.kind() == std::io::ErrorKind::TimedOut {
            return Self::Timeout;
        }
        Self::Connect(error.to_string())
    }
}

/// Extracts Icinga's `status` text from an error body, or the start of the
/// body if it isn't Icinga's JSON.
fn error_message(body: &[u8]) -> String {
    #[derive(serde::Deserialize)]
    struct ErrorBody {
        status: Option<String>,
    }
    if let Ok(ErrorBody {
        status: Some(status),
    }) = serde_json::from_slice::<ErrorBody>(body)
    {
        return status;
    }
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    match text.char_indices().nth(200) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

/// Visits an error and everything it wraps until `f` returns `true`.
///
/// `source()` alone isn't enough: `std::io::Error::source` returns its
/// *inner* error's source, skipping the inner error itself, and the TLS
/// stack reports handshake failures as (possibly nested) I/O errors that
/// wrap the `rustls::Error`. So wrapped I/O errors are visited explicitly.
fn visit_chain<'a>(
    error: &'a (dyn StdError + 'static),
    f: &mut dyn FnMut(&'a (dyn StdError + 'static)) -> bool,
    depth: usize,
) -> bool {
    if depth > 32 {
        return false;
    }
    if f(error) {
        return true;
    }
    let wrapped: Option<&'a (dyn StdError + 'static)> = error
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::get_ref)
        .map(|inner| inner as &(dyn StdError + 'static));
    if let Some(inner) = wrapped
        && visit_chain(inner, f, depth + 1)
    {
        return true;
    }
    error
        .source()
        .is_some_and(|source| visit_chain(source, f, depth + 1))
}

fn find_tls_error(error: &reqwest::Error) -> Option<ApiError> {
    let mut found = None;
    visit_chain(
        error,
        &mut |err| {
            if let Some(rustls_error) = err.downcast_ref::<rustls::Error>() {
                found = Some(ApiError::from_rustls(rustls_error));
                return true;
            }
            false
        },
        0,
    );
    found
}

fn has_timed_out_io(error: &reqwest::Error) -> bool {
    visit_chain(
        error,
        &mut |err| {
            err.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::TimedOut)
        },
        0,
    )
}

/// The error's message followed by its sources' messages, without repeats
/// (reqwest's top-level message is often just "error sending request").
fn chain_message(error: &reqwest::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(err) = current {
        let text = err.to_string();
        if !parts.iter().any(|part| part.contains(&text)) {
            parts.push(text);
        }
        current = err.source();
    }
    // reqwest prints the URL; keep it, it helps users spot a wrong port.
    parts.join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_icinga_error_bodies() {
        assert_eq!(
            ApiError::from_status(
                401,
                br#"{"error":401,"status":"Unauthorized. Please check your user credentials."}"#
            ),
            ApiError::Unauthorized
        );
        assert_eq!(
            ApiError::from_status(
                403,
                br#"{"error":403,"status":"Missing permission: actions/acknowledge-problem"}"#
            ),
            ApiError::Forbidden("Missing permission: actions/acknowledge-problem".to_owned())
        );
        assert_eq!(
            ApiError::from_status(404, br#"{"error":404,"status":"No objects found."}"#),
            ApiError::NotFound("No objects found.".to_owned())
        );
        assert_eq!(
            ApiError::from_status(500, b"<html>boom</html>"),
            ApiError::Http {
                status: 500,
                message: "<html>boom</html>".to_owned()
            }
        );
    }

    #[test]
    fn http_statuses_of_errors() {
        assert_eq!(ApiError::Unauthorized.http_status(), Some(401));
        assert_eq!(ApiError::Forbidden(String::new()).http_status(), Some(403));
        assert_eq!(ApiError::NotFound(String::new()).http_status(), Some(404));
        assert_eq!(
            ApiError::Http {
                status: 503,
                message: String::new()
            }
            .http_status(),
            Some(503)
        );
        assert_eq!(ApiError::Timeout.http_status(), None);
        assert_eq!(ApiError::Connect(String::new()).http_status(), None);
    }

    #[test]
    fn long_non_json_bodies_are_cut() {
        let body = "x".repeat(1000);
        let ApiError::Http { message, .. } = ApiError::from_status(502, body.as_bytes()) else {
            panic!("expected an HTTP error");
        };
        assert_eq!(message.chars().count(), 201);
    }

    #[test]
    fn transient_errors() {
        assert!(ApiError::Connect("refused".to_owned()).is_transient());
        assert!(ApiError::Timeout.is_transient());
        assert!(
            ApiError::Http {
                status: 503,
                message: "Icinga is reloading.".to_owned()
            }
            .is_transient()
        );
        assert!(
            ApiError::Http {
                status: 429,
                message: String::new()
            }
            .is_transient()
        );
        assert!(
            !ApiError::Http {
                status: 400,
                message: String::new()
            }
            .is_transient()
        );
        assert!(!ApiError::Unauthorized.is_transient());
        assert!(!ApiError::Forbidden(String::new()).is_transient());
        assert!(!ApiError::Tls(String::new()).is_transient());
        assert!(
            !ApiError::CertificateMismatch {
                expected: String::new(),
                actual: String::new()
            }
            .is_transient()
        );
    }

    #[test]
    fn recognises_pin_mismatches_inside_rustls_errors() {
        let error = PinMismatch {
            expected: [0xAB; 32],
            actual: [0x01; 32],
        }
        .into_rustls_error();
        let ApiError::CertificateMismatch { expected, actual } = ApiError::from_rustls(&error)
        else {
            panic!("expected a mismatch");
        };
        assert!(expected.starts_with("AB:AB:"));
        assert!(actual.starts_with("01:01:"));
        let io = std::io::Error::new(std::io::ErrorKind::InvalidData, error);
        assert!(matches!(
            ApiError::from_io(&io),
            ApiError::CertificateMismatch { .. }
        ));
    }
}
