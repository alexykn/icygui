//! TLS: the rustls client configuration (ring provider), our certificate
//! verifiers (pinning, CA trust with a server-name override) and reading a
//! server's certificate for trust on first use.

use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ic_model::Timestamp;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{verify_server_cert_signed_by_trust_anchor, verify_server_name};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, OtherError, RootCertStore,
    SignatureScheme,
};
use secrecy::ExposeSecret;
use url::Url;
use x509_parser::prelude::{FromDer, GeneralName, X509Certificate};

use crate::error::ApiError;
use crate::settings::{CONNECT_TIMEOUT, Credentials, TlsSettings};

/// The crypto provider every connection uses (ring; no OpenSSL, no aws-lc).
pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Formats a SHA-256 fingerprint as colon-separated uppercase hex
/// (`AB:CD:…`), the form the UI shows and the errors report.
#[must_use]
pub fn format_fingerprint(sha256: &[u8; 32]) -> String {
    sha256
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// SHA-256 of a DER-encoded certificate.
pub(crate) fn sha256(der: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, der);
    let mut out = [0_u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// The server presented a certificate other than the pinned one. Carried
/// inside a `rustls::Error` and recovered by [`ApiError::from_rustls`].
#[derive(Debug)]
pub(crate) struct PinMismatch {
    pub(crate) expected: [u8; 32],
    pub(crate) actual: [u8; 32],
}

impl fmt::Display for PinMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "certificate {} does not match the pinned certificate {}",
            format_fingerprint(&self.actual),
            format_fingerprint(&self.expected)
        )
    }
}

impl std::error::Error for PinMismatch {}

impl PinMismatch {
    pub(crate) fn into_rustls_error(self) -> rustls::Error {
        rustls::Error::InvalidCertificate(CertificateError::Other(OtherError(Arc::new(self))))
    }
}

/// Builds the rustls configuration for [`crate::Client`].
pub(crate) fn client_config(
    tls: &TlsSettings,
    credentials: &Credentials,
) -> Result<ClientConfig, ApiError> {
    let provider = provider();
    let algorithms = provider.signature_verification_algorithms;
    let verifier: Arc<dyn ServerCertVerifier> = match tls.pinned_sha256 {
        Some(expected) => Arc::new(PinnedVerifier {
            expected,
            algorithms,
        }),
        None => Arc::new(ChainVerifier {
            roots: Arc::new(root_store(tls)?),
            server_name: server_name_override(tls.server_name.as_deref())?,
            algorithms,
        }),
    };
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| ApiError::InvalidSettings(format!("TLS setup: {error}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    match credentials {
        Credentials::Basic { .. } => Ok(builder.with_no_client_auth()),
        Credentials::ClientCertificate { cert_pem, key_pem } => {
            let chain = parse_certificates(cert_pem, "client certificate")?;
            let key = PrivateKeyDer::from_pem_slice(key_pem.expose_secret()).map_err(|error| {
                ApiError::InvalidSettings(format!("client certificate key: {error}"))
            })?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|error| ApiError::InvalidSettings(format!("client certificate: {error}")))
        }
    }
}

/// The trusted roots: the configured CA certificates plus, optionally, the
/// system's. Unreadable system certificates are skipped with a warning.
fn root_store(tls: &TlsSettings) -> Result<RootCertStore, ApiError> {
    let mut roots = RootCertStore::empty();
    if let Some(pem) = &tls.ca_pem {
        for certificate in parse_certificates(pem, "CA certificate")? {
            roots
                .add(certificate)
                .map_err(|error| ApiError::InvalidSettings(format!("CA certificate: {error}")))?;
        }
    }
    if tls.use_system_roots {
        let loaded = rustls_native_certs::load_native_certs();
        for error in &loaded.errors {
            tracing::warn!(%error, "could not read some system root certificates");
        }
        let (added, ignored) = roots.add_parsable_certificates(loaded.certs);
        tracing::debug!(added, ignored, "loaded system root certificates");
    }
    Ok(roots)
}

/// Parses every certificate in a PEM file; at least one is required.
fn parse_certificates(pem: &[u8], what: &str) -> Result<Vec<CertificateDer<'static>>, ApiError> {
    let certificates = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ApiError::InvalidSettings(format!("{what}: {error}")))?;
    if certificates.is_empty() {
        return Err(ApiError::InvalidSettings(format!(
            "{what}: no PEM certificate found"
        )));
    }
    Ok(certificates)
}

fn parse_server_name(name: &str) -> Result<ServerName<'static>, ApiError> {
    ServerName::try_from(name.trim().to_owned())
        .map_err(|_| ApiError::InvalidSettings(format!("invalid server name {name:?}")))
}

/// The [`TlsSettings::server_name`] override as a TLS name. Blank counts as
/// unset (a cleared settings field), the same everywhere.
fn server_name_override(name: Option<&str>) -> Result<Option<ServerName<'static>>, ApiError> {
    name.map(str::trim)
        .filter(|name| !name.is_empty())
        .map(parse_server_name)
        .transpose()
}

/// Accepts exactly one leaf certificate, identified by its SHA-256. The
/// chain, the name and the validity dates are not checked (that is what
/// pinning means: this very certificate, which the user compared and
/// accepted), but the handshake signature still is, so only the holder of
/// the key can pass.
#[derive(Debug)]
struct PinnedVerifier {
    expected: [u8; 32],
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual = sha256(end_entity);
        if actual == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(PinMismatch {
                expected: self.expected,
                actual,
            }
            .into_rustls_error())
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// webpki chain verification against our roots, with the name checked
/// against the override if one is set.
///
/// Icinga's CA and older Icinga node certificates are minimal: some node
/// certificates have no subjectAltName at all, only the node name as CN.
/// Such a certificate is accepted when its CN equals the expected name and
/// the chain is valid, as OpenSSL-based clients (curl, Icinga itself) do.
/// Certificates *with* a subjectAltName are checked strictly.
#[derive(Debug)]
struct ChainVerifier {
    roots: Arc<RootCertStore>,
    server_name: Option<ServerName<'static>>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for ChainVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if self.roots.is_empty() {
            // Nothing is trusted: report it like any untrusted certificate,
            // so the caller can offer trust on first use.
            return Err(rustls::Error::InvalidCertificate(
                CertificateError::UnknownIssuer,
            ));
        }
        let certificate = ParsedCertificate::try_from(end_entity)?;
        verify_server_cert_signed_by_trust_anchor(
            &certificate,
            &self.roots,
            intermediates,
            now,
            self.algorithms.all,
        )?;
        let name = self.server_name.as_ref().unwrap_or(server_name);
        match verify_server_name(&certificate, name) {
            Ok(()) => Ok(ServerCertVerified::assertion()),
            Err(
                error @ rustls::Error::InvalidCertificate(
                    CertificateError::NotValidForName
                    | CertificateError::NotValidForNameContext { .. },
                ),
            ) => {
                if common_name_matches_without_san(end_entity, name) {
                    tracing::debug!("accepting certificate by CN: it has no subjectAltName");
                    Ok(ServerCertVerified::assertion())
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Whether the certificate has no subjectAltName extension and a subject
/// CN equal to `name` (DNS names compare case-insensitively; no wildcards).
fn common_name_matches_without_san(der: &[u8], name: &ServerName<'_>) -> bool {
    let Ok((_, certificate)) = X509Certificate::from_der(der) else {
        return false;
    };
    if !matches!(certificate.subject_alternative_name(), Ok(None)) {
        return false;
    }
    let expected = match name {
        ServerName::DnsName(dns) => dns.as_ref().trim_end_matches('.').to_ascii_lowercase(),
        ServerName::IpAddress(ip) => IpAddr::from(*ip).to_string(),
        _ => return false,
    };
    certificate
        .subject()
        .iter_common_name()
        .filter_map(|cn| cn.as_str().ok())
        .any(|cn| cn.trim_end_matches('.').eq_ignore_ascii_case(&expected))
}

/// What a server's certificate says about itself, for trust on first use.
#[derive(Clone, Debug, PartialEq)]
pub struct CertificateInfo {
    /// SHA-256 of the DER certificate (what [`TlsSettings::pinned_sha256`]
    /// expects).
    pub sha256: [u8; 32],
    /// Subject distinguished name (`CN=icinga-master`).
    pub subject: String,
    /// Issuer distinguished name (`CN=Icinga CA`).
    pub issuer: String,
    /// DNS names and IP addresses from the subjectAltName extension.
    pub names: Vec<String>,
    /// Start of the validity period.
    pub not_before: Timestamp,
    /// End of the validity period.
    pub not_after: Timestamp,
}

impl CertificateInfo {
    /// The fingerprint as colon-separated uppercase hex.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        format_fingerprint(&self.sha256)
    }

    /// Reads a DER certificate.
    ///
    /// # Errors
    ///
    /// [`ApiError::Decode`] if it isn't a parsable X.509 certificate.
    pub fn from_der(der: &[u8]) -> Result<Self, ApiError> {
        let (_, certificate) = X509Certificate::from_der(der)
            .map_err(|error| ApiError::Decode(format!("server certificate: {error}")))?;
        let names = match certificate.subject_alternative_name() {
            Ok(Some(san)) => san
                .value
                .general_names
                .iter()
                .filter_map(|name| match name {
                    GeneralName::DNSName(dns) => Some((*dns).to_owned()),
                    GeneralName::IPAddress(bytes) => ip_from_bytes(bytes),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let validity = certificate.validity();
        #[expect(
            clippy::cast_precision_loss,
            reason = "certificate dates are far inside f64's exact integer range"
        )]
        let timestamp = |time: i64| Timestamp::from_unix_seconds(time as f64);
        Ok(Self {
            sha256: sha256(der),
            subject: certificate.subject().to_string(),
            issuer: certificate.issuer().to_string(),
            names,
            not_before: timestamp(validity.not_before.timestamp()),
            not_after: timestamp(validity.not_after.timestamp()),
        })
    }
}

fn ip_from_bytes(bytes: &[u8]) -> Option<String> {
    match bytes.len() {
        4 => <[u8; 4]>::try_from(bytes)
            .ok()
            .map(|octets| IpAddr::from(octets).to_string()),
        16 => <[u8; 16]>::try_from(bytes)
            .ok()
            .map(|octets| IpAddr::from(octets).to_string()),
        _ => None,
    }
}

/// Accepts any certificate and remembers the leaf; used only to read a
/// certificate, never to send requests.
#[derive(Debug)]
struct CapturingVerifier {
    leaf: Mutex<Option<CertificateDer<'static>>>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for CapturingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        *self.leaf.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(end_entity.clone().into_owned());
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Connects to the server and reads its certificate without trusting it,
/// so the user can compare the fingerprint and pin it (trust on first use).
/// No request is sent and no credentials are used.
///
/// The handshake sends the SNI [`crate::Client`] sends: the URL's host,
/// none for an IP address. So a proxy that picks the certificate by SNI
/// (or routes TLS by it) shows the certificate the client will get, and a
/// pin taken from it matches. `server_name` (the
/// [`TlsSettings::server_name`] override) only changes which name the
/// client *verifies*, so it doesn't change what is fetched; it is checked
/// like [`crate::Client::new`] checks it (blank means unset), so settings
/// the client would refuse fail here too.
///
/// # Errors
///
/// - [`ApiError::InvalidSettings`] for a URL without a host or not
///   `https`, or an invalid `server_name`;
/// - [`ApiError::Connect`] / [`ApiError::Timeout`] if the server can't be
///   reached, or doesn't finish the handshake, within the connect timeout;
/// - [`ApiError::Tls`] if the handshake failed before a certificate arrived;
/// - [`ApiError::Decode`] if the certificate can't be parsed.
pub async fn fetch_server_certificate(
    base_url: &Url,
    server_name: Option<&str>,
) -> Result<CertificateInfo, ApiError> {
    read_certificate(base_url, server_name, CONNECT_TIMEOUT).await
}

/// [`fetch_server_certificate`] with the time the TCP connection and the
/// handshake may take.
async fn read_certificate(
    base_url: &Url,
    server_name: Option<&str>,
    timeout: Duration,
) -> Result<CertificateInfo, ApiError> {
    if base_url.scheme() != "https" {
        return Err(ApiError::InvalidSettings(format!(
            "the API URL must use https, not {}",
            base_url.scheme()
        )));
    }
    let host = base_url
        .host_str()
        .ok_or_else(|| ApiError::InvalidSettings("the API URL has no host".to_owned()))?;
    let port = base_url.port_or_known_default().unwrap_or(5665);
    // Checked like the client checks it; it isn't sent.
    server_name_override(server_name)?;
    let sni = host_server_name(host)?;

    let provider = provider();
    let verifier = Arc::new(CapturingVerifier {
        leaf: Mutex::new(None),
        algorithms: provider.signature_verification_algorithms,
    });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| ApiError::InvalidSettings(format!("TLS setup: {error}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

    let address = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let handshake = async {
        let tcp = tokio::net::TcpStream::connect((address.as_str(), port))
            .await
            .map_err(|error| ApiError::Connect(format!("{host}:{port}: {error}")))?;
        connector
            .connect(sni, tcp)
            .await
            .map(drop)
            .map_err(|error| ApiError::from_io(&error))
    };
    let outcome = tokio::time::timeout(timeout, handshake)
        .await
        .unwrap_or(Err(ApiError::Timeout));

    // The handshake may still fail after the certificate arrived (say the
    // server insists on a client certificate); the certificate is enough.
    let leaf = verifier
        .leaf
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    match (leaf, outcome) {
        (Some(leaf), _) => CertificateInfo::from_der(&leaf),
        (None, Err(error)) => Err(error),
        (None, Ok(())) => Err(ApiError::Tls("the server sent no certificate".to_owned())),
    }
}

/// The URL host as a TLS server name (IP literals become IP addresses, for
/// which rustls sends no SNI), as reqwest makes it for the client.
fn host_server_name(host: &str) -> Result<ServerName<'static>, ApiError> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return Ok(ServerName::IpAddress(ip.into()));
    }
    parse_server_name(bare)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_are_colon_hex() {
        let mut bytes = [0_u8; 32];
        bytes[0] = 0xAB;
        bytes[1] = 0x01;
        bytes[31] = 0xFF;
        let text = format_fingerprint(&bytes);
        assert!(text.starts_with("AB:01:00:"));
        assert!(text.ends_with(":FF"));
        assert_eq!(text.len(), 32 * 3 - 1);
    }

    #[test]
    fn ip_addresses_from_san_bytes() {
        assert_eq!(ip_from_bytes(&[10, 0, 0, 1]).as_deref(), Some("10.0.0.1"));
        let mut v6 = [0_u8; 16];
        v6[15] = 1;
        assert_eq!(ip_from_bytes(&v6).as_deref(), Some("::1"));
        assert_eq!(ip_from_bytes(&[1, 2, 3]), None);
    }

    #[test]
    fn rejects_garbage_pem() {
        let error = parse_certificates(b"not a certificate", "CA certificate").unwrap_err();
        assert!(
            matches!(error, ApiError::InvalidSettings(message) if message.contains("CA certificate"))
        );
    }

    #[test]
    fn blank_server_name_overrides_count_as_unset() {
        assert_eq!(server_name_override(None).unwrap(), None);
        assert_eq!(server_name_override(Some("")).unwrap(), None);
        assert_eq!(server_name_override(Some("  ")).unwrap(), None);
        assert!(matches!(
            server_name_override(Some(" icinga-master ")),
            Ok(Some(ServerName::DnsName(name))) if name.as_ref() == "icinga-master"
        ));
        assert!(matches!(
            server_name_override(Some("not a name!")),
            Err(ApiError::InvalidSettings(_))
        ));
        // The client accepts what the certificate fetch accepts.
        let tls = TlsSettings {
            pinned_sha256: None,
            server_name: Some("   ".to_owned()),
            ..TlsSettings::default()
        };
        let credentials = Credentials::Basic {
            username: "u".to_owned(),
            password: secrecy::SecretString::from("p".to_owned()),
        };
        assert!(client_config(&tls, &credentials).is_ok());
    }

    #[tokio::test]
    async fn reading_a_certificate_from_a_silent_server_times_out() {
        // Accepts the TCP connection and never says anything.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("https://{}", listener.local_addr().unwrap())).unwrap();
        let silent = tokio::spawn(async move {
            let mut open = Vec::new();
            while let Ok((tcp, _)) = listener.accept().await {
                open.push(tcp);
            }
        });
        let started = std::time::Instant::now();
        let error = read_certificate(&url, None, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert_eq!(error, ApiError::Timeout);
        assert!(started.elapsed() < Duration::from_secs(5));
        silent.abort();
    }

    #[tokio::test]
    async fn invalid_server_names_fail_before_connecting() {
        let url = Url::parse("https://127.0.0.1:1").unwrap();
        assert!(matches!(
            read_certificate(&url, Some("not a name!"), Duration::from_secs(1)).await,
            Err(ApiError::InvalidSettings(_))
        ));
    }

    #[test]
    fn host_names_and_ip_literals() {
        assert!(matches!(
            host_server_name("10.1.2.3"),
            Ok(ServerName::IpAddress(_))
        ));
        assert!(matches!(
            host_server_name("[::1]"),
            Ok(ServerName::IpAddress(_))
        ));
        assert!(matches!(
            host_server_name("icinga-master"),
            Ok(ServerName::DnsName(_))
        ));
    }
}
