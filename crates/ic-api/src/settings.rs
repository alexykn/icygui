//! How to reach and authenticate against one Icinga 2 API endpoint.

use std::fmt;
use std::time::Duration;

use secrecy::{SecretBox, SecretString};
use url::Url;

/// The default request timeout (see [`ConnectionSettings::request_timeout`]).
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long establishing a TCP connection (plus TLS) may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The default action timeout (see [`ConnectionSettings::action_timeout`]).
pub const DEFAULT_ACTION_TIMEOUT: Duration = Duration::from_mins(5);

/// Everything needed to talk to one Icinga 2 API endpoint.
#[derive(Debug)]
pub struct ConnectionSettings {
    /// `https://host:5665`; the client appends `/v1/…`. A path prefix (for a
    /// reverse proxy) is kept.
    pub base_url: Url,
    /// How to authenticate.
    pub credentials: Credentials,
    /// How to trust the server.
    pub tls: TlsSettings,
    /// How long a request may wait for the response to begin (its
    /// headers), and how long a response body may pause between reads. A
    /// body that keeps arriving may take longer in total (a full object
    /// list over a slow VPN), up to 20 times this. The event stream only
    /// uses it for the response to begin; after that it has no read
    /// timeout. Actions wait [`ConnectionSettings::action_timeout`] instead.
    pub request_timeout: Duration,
    /// How long an action request may wait for its answer to begin
    /// (default 5 minutes). Icinga runs the action for every object of the
    /// request before it answers, and creating downtimes and comments
    /// writes a configuration object each, so a busy master can take far
    /// longer than a query. Giving up early doesn't stop Icinga: it would
    /// only turn an answer into an unknown outcome
    /// ([`crate::ActionResult::unknown`]). Icinga itself never closes a
    /// connection while it works on a request, and TCP keepalive notices a
    /// dead one.
    pub action_timeout: Duration,
}

impl ConnectionSettings {
    /// Settings with the default timeouts.
    #[must_use]
    pub fn new(base_url: Url, credentials: Credentials, tls: TlsSettings) -> Self {
        Self {
            base_url,
            credentials,
            tls,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            action_timeout: DEFAULT_ACTION_TIMEOUT,
        }
    }
}

/// How the client authenticates.
pub enum Credentials {
    /// HTTP basic auth with an `ApiUser`'s name and password.
    Basic {
        /// The API user's name.
        username: String,
        /// The password.
        password: SecretString,
    },
    /// A TLS client certificate for an `ApiUser` with `client_cn`.
    ClientCertificate {
        /// PEM certificate chain (leaf first).
        cert_pem: Vec<u8>,
        /// PEM private key (PKCS#8, PKCS#1 or SEC1; unencrypted).
        key_pem: SecretBox<Vec<u8>>,
    },
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
            Self::ClientCertificate { cert_pem, .. } => f
                .debug_struct("ClientCertificate")
                .field("cert_pem", &format_args!("{} bytes", cert_pem.len()))
                .field("key_pem", &"[REDACTED]")
                .finish(),
        }
    }
}

/// How the client decides whether to trust the server's certificate.
///
/// With a pin, only the pinned certificate is accepted (CA, name and
/// validity checks are skipped). Otherwise the chain must lead to `ca_pem` or (with
/// `use_system_roots`) a system root, and the certificate must be valid for
/// `server_name` or the URL's host. With neither a pin nor any trusted root,
/// every certificate is rejected as untrusted, which lets the caller offer
/// trust on first use.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsSettings {
    /// Extra trusted CA certificates, PEM (Icinga's `ca.crt`).
    pub ca_pem: Option<Vec<u8>>,
    /// Accept exactly the leaf certificate with this SHA-256 fingerprint.
    pub pinned_sha256: Option<[u8; 32]>,
    /// Verify the certificate against this name instead of the URL's host
    /// (blank counts as unset). Only verification uses it: the SNI sent is
    /// always the URL's host.
    pub server_name: Option<String>,
    /// Also trust the operating system's root certificates.
    pub use_system_roots: bool,
}
