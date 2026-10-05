//! Raw HTTPS requests next to the client, for what it doesn't expose:
//! response sizes, and the exact answers to hand-made queries.

use std::net::{SocketAddr, ToSocketAddrs};

use ic_api::Url;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use serde_json::Value;

/// A plain HTTP client for one Icinga API, trusting `ca_pem`.
pub(crate) struct Raw {
    http: reqwest::Client,
    base: Url,
    user: String,
    password: String,
}

/// An answer: the status and the body's bytes.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

impl Answer {
    pub(crate) fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    /// The bytes one attribute's values take in a query answer: each
    /// result's `attrs[attr]`, serialised compactly as Icinga and `ic-mock`
    /// do (`null` where it's missing).
    pub(crate) fn attr_bytes(&self, attr: &str) -> usize {
        self.json()["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| serde_json::to_vec(&entry["attrs"][attr]).unwrap().len())
            .sum()
    }
}

impl Raw {
    /// Verifies the server certificate against `ca_pem` and the URL's host,
    /// or `server_name` (then connecting to the URL's address under that
    /// name).
    pub(crate) fn new(
        base: &Url,
        server_name: Option<&str>,
        ca_pem: &[u8],
        user: &str,
        password: &str,
    ) -> Self {
        let mut roots = rustls::RootCertStore::empty();
        for certificate in CertificateDer::pem_slice_iter(ca_pem) {
            roots.add(certificate.unwrap()).unwrap();
        }
        let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let mut builder = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .no_proxy()
            .http1_only();
        let mut base = base.clone();
        if let Some(name) = server_name {
            let addr: SocketAddr = (
                base.host_str().unwrap(),
                base.port_or_known_default().unwrap(),
            )
                .to_socket_addrs()
                .unwrap()
                .next()
                .unwrap();
            builder = builder.resolve(name, addr);
            base.set_host(Some(name)).unwrap();
        }
        Self {
            http: builder.build().unwrap(),
            base,
            user: user.to_owned(),
            password: password.to_owned(),
        }
    }

    /// `POST /v1/objects/<plural>` with `X-HTTP-Method-Override: GET`.
    pub(crate) async fn query(&self, plural: &str, body: &Value) -> Answer {
        let url = self.base.join(&format!("v1/objects/{plural}")).unwrap();
        let response = self
            .http
            .post(url)
            .basic_auth(&self.user, Some(&self.password))
            .header("Accept", "application/json")
            .header("X-HTTP-Method-Override", "GET")
            .json(body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body = response.bytes().await.unwrap().to_vec();
        Answer { status, body }
    }
}
