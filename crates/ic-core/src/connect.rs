//! Turning an environment into a connected [`Client`]: the password from
//! the secret store, certificate and CA files, the pin; and classifying what
//! goes wrong into what the user can do about it.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ic_api::{
    ApiError, ApiInfo, CertificateInfo, Client, ConnectionSettings, Credentials, EventLines,
    TlsSettings, Url, fetch_server_certificate,
};
use ic_config::{AuthConfig, Environment};
use ic_model::EventKind;
use secrecy::{SecretBox, SecretString};

use crate::ports::SecretStore;

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
        /// What went wrong.
        message: String,
        /// Both fingerprints, for a pin mismatch.
        mismatch: Option<(String, String)>,
        /// What the server presented.
        certificate: Option<CertificateInfo>,
    },
    /// Anything that may go away by itself: retry with backoff.
    Transient(String),
}

impl Failure {
    /// Classifies an API error; for TLS errors it reads the certificate the
    /// server presents (one more handshake), for trust on first use.
    pub(crate) async fn from_api(error: ApiError, url: &Url, server_name: Option<&str>) -> Self {
        match error {
            ApiError::Unauthorized => Self::Auth(error.to_string()),
            ApiError::Tls(_) | ApiError::CertificateMismatch { .. } => {
                let mismatch = match &error {
                    ApiError::CertificateMismatch { expected, actual } => {
                        Some((expected.clone(), actual.clone()))
                    }
                    _ => None,
                };
                let certificate = match fetch_server_certificate(url, server_name).await {
                    Ok(certificate) => Some(certificate),
                    Err(fetch) => {
                        tracing::debug!(error = %fetch, "couldn't read the server certificate");
                        None
                    }
                };
                Self::Tls {
                    message: error.to_string(),
                    mismatch,
                    certificate,
                }
            }
            ApiError::InvalidSettings(message) => Self::Misconfigured(message),
            other => Self::Transient(other.to_string()),
        }
    }
}

/// A client, the API user it logged in as, and the event stream if the
/// user may read events.
pub(crate) struct Connected {
    pub(crate) client: Client,
    pub(crate) info: ApiInfo,
    pub(crate) lines: Option<EventLines>,
}

/// Builds the connection settings of `environment`.
///
/// # Errors
///
/// [`Failure::MissingSecret`] without a password, [`Failure::Transient`]
/// if the secret store can't be read right now, [`Failure::Misconfigured`]
/// for an invalid URL or pin and unreadable files.
pub(crate) async fn settings(
    environment: &Environment,
    password: Password,
) -> Result<ConnectionSettings, Failure> {
    let url = environment
        .api_url()
        .map_err(|error| Failure::Misconfigured(error.to_string()))?;
    let ca_pem = match &environment.tls.ca_file {
        Some(path) => Some(read_file("CA file", path).await?),
        None => None,
    };
    let pinned_sha256 = environment
        .tls
        .pinned_fingerprint()
        .map_err(|error| Failure::Misconfigured(error.to_string()))?;
    let tls = TlsSettings {
        ca_pem,
        pinned_sha256,
        server_name: environment
            .tls
            .server_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        use_system_roots: environment.tls.use_system_roots,
    };
    let credentials = match &environment.auth {
        AuthConfig::Basic { username } => Credentials::Basic {
            username: username.trim().to_owned(),
            password: password_for(environment, password).await?,
        },
        AuthConfig::ClientCertificate {
            cert_path,
            key_path,
        } => Credentials::ClientCertificate {
            cert_pem: read_file("client certificate", cert_path).await?,
            key_pem: SecretBox::new(Box::new(read_file("client key", key_path).await?)),
        },
    };
    Ok(ConnectionSettings::new(url, credentials, tls))
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

/// Connects: builds the client (actions wait `action_timeout` for their
/// answer), checks the credentials (`GET /v1`) and opens the event stream
/// (queue `icygui-<uuid>`) for every event type the API user may read.
///
/// # Errors
///
/// As [`settings`]; API errors classified by [`Failure::from_api`].
pub(crate) async fn connect(
    environment: &Environment,
    secrets: Arc<dyn SecretStore>,
    action_timeout: Duration,
) -> Result<Connected, Failure> {
    let mut settings = settings(environment, Password::Store(secrets)).await?;
    settings.action_timeout = action_timeout;
    let url = settings.base_url.clone();
    let server_name = settings.tls.server_name.clone();
    let client = Client::new(settings).map_err(|error| match error {
        ApiError::InvalidSettings(message) => Failure::Misconfigured(message),
        other => Failure::Misconfigured(other.to_string()),
    })?;
    let info = match client.info().await {
        Ok(info) => info,
        Err(error) => return Err(Failure::from_api(error, &url, server_name.as_deref()).await),
    };
    let kinds: Vec<EventKind> = EventKind::ALL
        .into_iter()
        .filter(|kind| info.allows(&format!("events/{}", kind.api_name())))
        .collect();
    let lines = if kinds.is_empty() {
        tracing::warn!(
            user = %info.user,
            "the API user may not read any event type: no live updates"
        );
        None
    } else {
        if kinds.len() < EventKind::ALL.len() {
            tracing::warn!(
                user = %info.user,
                allowed = kinds.len(),
                "the API user may read only some event types"
            );
        }
        let queue = format!("icygui-{}", uuid::Uuid::new_v4());
        match client.events(&queue, &kinds).await {
            Ok(stream) => Some(stream.into_lines()),
            Err(ApiError::Forbidden(message)) => {
                tracing::warn!(%message, "the event stream was refused: no live updates");
                None
            }
            Err(error) => {
                return Err(Failure::from_api(error, &url, server_name.as_deref()).await);
            }
        }
    };
    Ok(Connected {
        client,
        info,
        lines,
    })
}
