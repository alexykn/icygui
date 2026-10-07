//! Shared helpers for the integration tests: HTTPS clients that pin the
//! mock's certificate (or trust its CA), JSON requests and event streams.

#![allow(dead_code, reason = "each test binary uses a different subset")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::match_wild_err_arm,
    reason = "test helpers fail the test loudly"
)]

use std::sync::{Arc, Once};
use std::time::Duration;

use futures::StreamExt;
use ic_mock::{MockConfig, MockServer};
use reqwest::{Client, Method, Response, StatusCode};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use serde_json::Value;

pub(crate) const USER: &str = "root";
pub(crate) const PASSWORD: &str = "icinga";

fn install_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Accepts exactly one certificate, identified by its SHA-256 fingerprint
/// (how a client trusts a self-signed Icinga node).
#[derive(Debug)]
pub(crate) struct Pinned {
    fingerprint: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let digest = ring::digest::digest(&ring::digest::SHA256, end_entity.as_ref());
        if digest.as_ref() == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("fingerprint mismatch".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A rustls config that pins `fingerprint`.
pub(crate) fn pinned_config(fingerprint: [u8; 32]) -> ClientConfig {
    install_provider();
    let provider = provider();
    ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            fingerprint,
            provider,
        }))
        .with_no_client_auth()
}

/// A rustls config that verifies the server against `ca_pem` (full
/// `WebPKI` verification including the host name).
pub(crate) fn ca_config(ca_pem: &str, client: Option<(&str, &str)>) -> ClientConfig {
    install_provider();
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let builder = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots);
    match client {
        None => builder.with_no_client_auth(),
        Some((cert_pem, key_pem)) => {
            let chain: Vec<CertificateDer<'static>> =
                CertificateDer::pem_slice_iter(cert_pem.as_bytes())
                    .map(Result::unwrap)
                    .collect();
            let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).unwrap();
            builder.with_client_auth_cert(chain, key).unwrap()
        }
    }
}

/// A reqwest client from a rustls config.
pub(crate) fn client_from(config: ClientConfig) -> Client {
    Client::builder()
        .tls_backend_preconfigured(config)
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap()
}

/// A client that pins the server's current certificate.
pub(crate) fn client(server: &MockServer) -> Client {
    client_from(pinned_config(server.cert_sha256()))
}

/// Starts a server with `config` and returns it with a pinned client.
pub(crate) async fn start(config: MockConfig) -> (MockServer, Client) {
    let server = MockServer::start(config).await.unwrap();
    let client = client(&server);
    (server, client)
}

/// Starts the `prod-cluster` scenario.
pub(crate) async fn prod() -> (MockServer, Client) {
    start(MockConfig::with_scenario(ic_mock::scenarios::prod_cluster())).await
}

/// Starts the `lab` scenario.
pub(crate) async fn lab() -> (MockServer, Client) {
    start(MockConfig::default()).await
}

/// A request as `root` with `Accept: application/json`.
pub(crate) fn request(
    client: &Client,
    server: &MockServer,
    method: Method,
    path: &str,
) -> reqwest::RequestBuilder {
    client
        .request(method, format!("{}{path}", server.url()))
        .basic_auth(USER, Some(PASSWORD))
        .header("Accept", "application/json")
}

/// Status and JSON body of a response.
pub(crate) async fn json(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let text = response.text().await.unwrap();
    let value =
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("not JSON ({error}): {text}"));
    (status, value)
}

/// `GET path` as root.
pub(crate) async fn get(client: &Client, server: &MockServer, path: &str) -> (StatusCode, Value) {
    json(
        request(client, server, Method::GET, path)
            .send()
            .await
            .unwrap(),
    )
    .await
}

/// `POST path` with a JSON body as root.
pub(crate) async fn post(
    client: &Client,
    server: &MockServer,
    path: &str,
    body: &Value,
) -> (StatusCode, Value) {
    json(
        request(client, server, Method::POST, path)
            .json(body)
            .send()
            .await
            .unwrap(),
    )
    .await
}

/// The `results` array of a response.
pub(crate) fn results(body: &Value) -> &Vec<Value> {
    body["results"]
        .as_array()
        .unwrap_or_else(|| panic!("no results: {body}"))
}

/// An open event stream that yields parsed events.
pub(crate) struct EventStream {
    stream: futures::stream::BoxStream<'static, reqwest::Result<bytes::Bytes>>,
    buffer: Vec<u8>,
}

impl EventStream {
    /// Opens `/v1/events` with the given body and waits until the server
    /// has registered the stream.
    pub(crate) async fn open(client: &Client, server: &MockServer, body: &Value) -> Self {
        let before = server.control().event_streams();
        let response = request(client, server, Method::POST, "/v1/events")
            .json(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "event stream refused");
        assert!(
            server
                .control()
                .wait_for_event_streams(before + 1, Duration::from_secs(5))
                .await
        );
        Self::from_response(response)
    }

    /// Reads the events of an accepted `/v1/events` response.
    pub(crate) fn from_response(response: Response) -> Self {
        Self {
            stream: response.bytes_stream().boxed(),
            buffer: Vec::new(),
        }
    }

    /// The next raw line (without the newline), or `None` at the end.
    pub(crate) async fn next_line(&mut self) -> Option<String> {
        loop {
            if let Some(position) = self.buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=position).collect();
                return Some(String::from_utf8(line[..line.len() - 1].to_vec()).unwrap());
            }
            match tokio::time::timeout(Duration::from_secs(5), self.stream.next()).await {
                Ok(Some(Ok(chunk))) => self.buffer.extend_from_slice(&chunk),
                Ok(Some(Err(_)) | None) => return None,
                Err(_) => panic!("no event within 5s"),
            }
        }
    }

    /// The next event.
    pub(crate) async fn next(&mut self) -> Value {
        let line = self.next_line().await.expect("stream ended");
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("bad event ({error}): {line}"))
    }

    /// Events until one of `type_name` arrives (returned last).
    pub(crate) async fn until(&mut self, type_name: &str) -> Vec<Value> {
        let mut events = Vec::new();
        loop {
            let event = self.next().await;
            let done = event["type"] == type_name;
            events.push(event);
            if done {
                return events;
            }
        }
    }

    /// Whether the stream ends within a short time.
    pub(crate) async fn ends(&mut self) -> bool {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), self.stream.next()).await {
                Ok(Some(Ok(chunk))) => self.buffer.extend_from_slice(&chunk),
                Ok(Some(Err(_)) | None) => return true,
                Err(_) => return false,
            }
        }
    }
}
