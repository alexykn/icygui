//! An in-process HTTPS server that plays Icinga for the client tests:
//! certificates from a throwaway CA (rcgen), every request and every
//! handshake's SNI recorded, and replies chosen by a handler per test.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::HeaderMap;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use ic_api::{ConnectionSettings, Credentials, TlsSettings, Url};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair, KeyUsagePurpose,
    date_time_ymd,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use secrecy::{SecretBox, SecretString};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

pub(crate) const USER: &str = "icygui";
pub(crate) const PASSWORD: &str = "icygui-test";
/// The name in the server certificate (the server listens on 127.0.0.1).
pub(crate) const SERVER_NAME: &str = "icinga-master";

/// A test CA that issues server and client certificates.
pub(crate) struct Pki {
    ca: CertifiedIssuer<'static, KeyPair>,
}

/// A certificate with its key, PEM-encoded.
pub(crate) struct Issued {
    pub(crate) cert_pem: String,
    pub(crate) key_pem: String,
    pub(crate) der: Vec<u8>,
}

impl Pki {
    pub(crate) fn new() -> Self {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .distinguished_name
            .push(DnType::CommonName, "Icinga CA");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca = CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap();
        Self { ca }
    }

    pub(crate) fn ca_pem(&self) -> String {
        self.ca.pem()
    }

    /// A certificate for `common_name`, with `sans` as subjectAltNames
    /// (IP addresses become IP entries; none at all if empty, like old
    /// Icinga node certificates).
    pub(crate) fn issue(&self, common_name: &str, sans: &[&str]) -> Issued {
        self.issue_with(common_name, sans, |_| {})
    }

    /// A certificate valid only from `from` to `until` (year, month, day).
    pub(crate) fn issue_valid(
        &self,
        common_name: &str,
        sans: &[&str],
        from: (i32, u8, u8),
        until: (i32, u8, u8),
    ) -> Issued {
        self.issue_with(common_name, sans, |params| {
            params.not_before = date_time_ymd(from.0, from.1, from.2);
            params.not_after = date_time_ymd(until.0, until.1, until.2);
        })
    }

    fn issue_with(
        &self,
        common_name: &str,
        sans: &[&str],
        adjust: impl FnOnce(&mut CertificateParams),
    ) -> Issued {
        let sans: Vec<String> = sans.iter().map(|san| (*san).to_owned()).collect();
        let mut params = CertificateParams::new(sans).unwrap();
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        adjust(&mut params);
        let key = KeyPair::generate().unwrap();
        let cert = params.signed_by(&key, &self.ca).unwrap();
        Issued {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
            der: cert.der().to_vec(),
        }
    }
}

/// One request as the server saw it.
#[derive(Clone, Debug)]
pub(crate) struct Recorded {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Vec<u8>,
    pub(crate) client_certificate: bool,
}

impl Recorded {
    pub(crate) fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

/// What the server answers.
pub(crate) enum Reply {
    /// A status with a JSON (or any text) body.
    Json(u16, String),
    /// A streamed 200 response; each item is one chunk, an `Err` aborts
    /// the connection mid-stream, the end of the channel ends the body.
    Stream(mpsc::UnboundedReceiver<Result<Bytes, std::io::Error>>),
    /// Waits, then answers.
    Slow(Duration, Box<Reply>),
    /// A status and the start of a body, then nothing: the response never
    /// ends.
    Stalled(u16, String),
    /// `302 Found` to this location.
    Redirect(String),
}

pub(crate) fn ok_json(value: &Value) -> Reply {
    Reply::Json(200, value.to_string())
}

pub(crate) fn error_json(status: u16, message: &str) -> Reply {
    Reply::Json(
        status,
        serde_json::json!({ "error": status, "status": message }).to_string(),
    )
}

/// Answers an action like Icinga's `ActionsHandler` (`actionshandler.cpp`):
/// the per-object results, with the HTTP status computed from their codes.
/// One distinct success code (and no failure) is the status; one distinct
/// failure code is the status even next to successes; several successes
/// and no failure are 200; anything else is 500.
pub(crate) fn icinga_action_reply(results: Vec<Value>) -> Reply {
    let codes: Vec<u16> = results
        .iter()
        .map(|result| u16::try_from(result["code"].as_u64().unwrap()).unwrap())
        .collect();
    let ok: BTreeSet<u16> = codes
        .iter()
        .copied()
        .filter(|code| (200..300).contains(code))
        .collect();
    let failed: BTreeSet<u16> = codes
        .iter()
        .copied()
        .filter(|code| !(200..300).contains(code))
        .collect();
    let status = match (ok.len(), failed.len()) {
        (1, 0) => *ok.first().unwrap(),
        (_, 1) => *failed.first().unwrap(),
        (n, 0) if n >= 2 => 200,
        _ => 500,
    };
    Reply::Json(
        status,
        serde_json::json!({ "results": Value::Array(results) }).to_string(),
    )
}

type Handler = Arc<dyn Fn(&Recorded) -> Reply + Send + Sync>;

pub(crate) struct ServerOptions {
    pub(crate) leaf: Issued,
    pub(crate) client_ca: Option<String>,
    pub(crate) handler: Handler,
    /// Speak TLS 1.2 only (older Icinga / OpenSSL setups).
    pub(crate) tls12_only: bool,
}

impl ServerOptions {
    /// A server with `leaf`, no client certificates, TLS 1.2 and 1.3.
    pub(crate) fn new(
        leaf: Issued,
        handler: impl Fn(&Recorded) -> Reply + Send + Sync + 'static,
    ) -> Self {
        Self {
            leaf,
            client_ca: None,
            handler: Arc::new(handler),
            tls12_only: false,
        }
    }
}

pub(crate) struct TestServer {
    pub(crate) addr: SocketAddr,
    pub(crate) leaf_der: Vec<u8>,
    pub(crate) requests: Arc<Mutex<Vec<Recorded>>>,
    /// The SNI of every completed handshake (`None`: no SNI sent).
    pub(crate) handshakes: Arc<Mutex<Vec<Option<String>>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestServer {
    pub(crate) async fn start(options: ServerOptions) -> Self {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = rustls::ServerConfig::builder_with_provider(provider.clone());
        let builder = if options.tls12_only {
            builder
                .with_protocol_versions(&[&rustls::version::TLS12])
                .unwrap()
        } else {
            builder.with_safe_default_protocol_versions().unwrap()
        };
        let builder = match &options.client_ca {
            Some(ca_pem) => {
                let mut roots = rustls::RootCertStore::empty();
                for cert in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
                    roots.add(cert.unwrap()).unwrap();
                }
                let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
                    Arc::new(roots),
                    provider,
                )
                .build()
                .unwrap();
                builder.with_client_cert_verifier(verifier)
            }
            None => builder.with_no_client_auth(),
        };
        let chain = vec![CertificateDer::from(options.leaf.der.clone())];
        let key = PrivateKeyDer::from_pem_slice(options.leaf.key_pem.as_bytes()).unwrap();
        let config = builder.with_single_cert(chain, key).unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let handshakes = Arc::new(Mutex::new(Vec::new()));
        let shaken = handshakes.clone();
        let handler = options.handler;
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                let handler = handler.clone();
                let recorded = recorded.clone();
                let shaken = shaken.clone();
                tokio::spawn(async move {
                    // Handshake failures are what some tests want.
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let connection = tls.get_ref().1;
                    shaken
                        .lock()
                        .unwrap()
                        .push(connection.server_name().map(str::to_owned));
                    let client_certificate = connection.peer_certificates().is_some();
                    let service = service_fn(move |request: Request<Incoming>| {
                        let handler = handler.clone();
                        let recorded = recorded.clone();
                        async move {
                            let (parts, body) = request.into_parts();
                            let body = body
                                .collect()
                                .await
                                .map(http_body_util::Collected::to_bytes)
                                .unwrap_or_default();
                            let request = Recorded {
                                method: parts.method.to_string(),
                                path: parts.uri.path().to_owned(),
                                headers: parts.headers,
                                body: body.to_vec(),
                                client_certificate,
                            };
                            recorded.lock().unwrap().push(request.clone());
                            let reply = handler(&request);
                            Ok::<_, Infallible>(respond(reply).await)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tls), service)
                        .await;
                });
            }
        });
        Self {
            addr,
            leaf_der: options.leaf.der,
            requests,
            handshakes,
            task,
        }
    }

    pub(crate) fn url(&self) -> Url {
        Url::parse(&format!("https://{}", self.addr)).unwrap()
    }

    pub(crate) fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    pub(crate) fn handshakes(&self) -> Vec<Option<String>> {
        self.handshakes.lock().unwrap().clone()
    }

    pub(crate) fn sha256(&self) -> [u8; 32] {
        let digest = ring::digest::digest(&ring::digest::SHA256, &self.leaf_der);
        digest.as_ref().try_into().unwrap()
    }
}

type Body = UnsyncBoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

async fn respond(reply: Reply) -> Response<Body> {
    match reply {
        Reply::Json(status, body) => Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(
                Full::new(Bytes::from(body))
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap(),
        Reply::Stream(receiver) => {
            let frames = futures::stream::unfold(receiver, |mut receiver| async move {
                let item = receiver.recv().await?;
                let frame = item
                    .map(Frame::data)
                    .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>);
                Some((frame, receiver))
            });
            Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(BodyExt::boxed_unsync(StreamBody::new(frames)))
                .unwrap()
        }
        Reply::Slow(delay, reply) => {
            tokio::time::sleep(delay).await;
            Box::pin(respond(*reply)).await
        }
        Reply::Stalled(status, start) => {
            let frames = futures::stream::once(async move {
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Frame::data(Bytes::from(start)))
            })
            .chain(futures::stream::pending());
            Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(BodyExt::boxed_unsync(StreamBody::new(frames)))
                .unwrap()
        }
        Reply::Redirect(location) => Response::builder()
            .status(302)
            .header("location", location)
            .body(
                Full::new(Bytes::new())
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
            .unwrap(),
    }
}

/// Settings for basic auth with the test credentials.
pub(crate) fn basic_settings(url: Url, tls: TlsSettings) -> ConnectionSettings {
    ConnectionSettings::new(
        url,
        Credentials::Basic {
            username: USER.to_owned(),
            password: SecretString::from(PASSWORD.to_owned()),
        },
        tls,
    )
}

/// Settings for client-certificate auth.
pub(crate) fn certificate_settings(
    url: Url,
    tls: TlsSettings,
    client: &Issued,
) -> ConnectionSettings {
    ConnectionSettings::new(
        url,
        Credentials::ClientCertificate {
            cert_pem: client.cert_pem.clone().into_bytes(),
            key_pem: SecretBox::new(Box::new(client.key_pem.clone().into_bytes())),
        },
        tls,
    )
}

/// Trust the test CA and verify against [`SERVER_NAME`].
pub(crate) fn ca_trust(pki: &Pki) -> TlsSettings {
    TlsSettings {
        ca_pem: Some(pki.ca_pem().into_bytes()),
        server_name: Some(SERVER_NAME.to_owned()),
        ..TlsSettings::default()
    }
}
