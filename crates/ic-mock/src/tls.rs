//! Certificates: self-signed or signed by a mock "Icinga CA", rotation, and
//! the rustls server configuration.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::RootCertStore;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use time::{Duration as TimeDuration, OffsetDateTime};

use crate::error::MockError;

/// Certificates and keys in PEM form.
#[derive(Clone, PartialEq, Eq)]
pub struct TlsMaterial {
    /// The server certificate.
    pub cert_pem: String,
    /// The server's private key (PKCS#8).
    pub key_pem: String,
    /// The CA certificate, when the server certificate is CA-signed.
    pub ca_pem: Option<String>,
    /// The CA's private key: signs rotated server certificates and client
    /// certificates.
    pub ca_key_pem: Option<String>,
}

impl fmt::Debug for TlsMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsMaterial")
            .field("cert_pem", &self.cert_pem)
            .field("key_pem", &"<redacted>")
            .field("ca_pem", &self.ca_pem)
            .field(
                "ca_key_pem",
                &self.ca_key_pem.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

fn cert_error(error: impl fmt::Display) -> MockError {
    MockError::Certificate(error.to_string())
}

fn validity(days: i64) -> (OffsetDateTime, OffsetDateTime) {
    let now = OffsetDateTime::now_utc();
    (now - TimeDuration::days(1), now + TimeDuration::days(days))
}

/// Parameters of the CA ("Icinga CA", like `icinga2 pki new-ca`).
fn ca_params() -> CertificateParams {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Icinga CA");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let (not_before, not_after) = validity(15 * 365);
    params.not_before = not_before;
    params.not_after = not_after;
    params
}

/// Parameters of a server certificate for `node_name`.
fn leaf_params(node_name: &str) -> Result<CertificateParams, MockError> {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, node_name);
    params.distinguished_name = dn;
    let mut names = vec![node_name.to_owned()];
    if node_name != "localhost" {
        names.push("localhost".to_owned());
    }
    for name in names {
        params
            .subject_alt_names
            .push(SanType::DnsName(name.try_into().map_err(cert_error)?));
    }
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    params.use_authority_key_identifier_extension = true;
    let (not_before, not_after) = validity(397);
    params.not_before = not_before;
    params.not_after = not_after;
    Ok(params)
}

impl TlsMaterial {
    /// A self-signed certificate for `node_name`, `localhost`, `127.0.0.1`
    /// and `::1`.
    ///
    /// # Errors
    /// Key generation or signing failed.
    pub fn self_signed(node_name: &str) -> Result<Self, MockError> {
        let key = KeyPair::generate().map_err(cert_error)?;
        let mut params = leaf_params(node_name)?;
        params.use_authority_key_identifier_extension = false;
        let cert = params.self_signed(&key).map_err(cert_error)?;
        Ok(Self {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
            ca_pem: None,
            ca_key_pem: None,
        })
    }

    /// A new CA and a server certificate for `node_name` signed by it.
    ///
    /// # Errors
    /// Key generation or signing failed.
    pub fn ca_signed(node_name: &str) -> Result<Self, MockError> {
        let ca_key = KeyPair::generate().map_err(cert_error)?;
        let ca_cert = ca_params().self_signed(&ca_key).map_err(cert_error)?;
        Self::signed_by_ca(node_name, &ca_cert.pem(), &ca_key.serialize_pem())
    }

    /// A server certificate for `node_name` signed by an existing CA.
    ///
    /// # Errors
    /// The CA can't be parsed, or signing failed.
    pub fn signed_by_ca(
        node_name: &str,
        ca_pem: &str,
        ca_key_pem: &str,
    ) -> Result<Self, MockError> {
        let ca_key = KeyPair::from_pem(ca_key_pem).map_err(cert_error)?;
        let issuer = Issuer::from_ca_cert_pem(ca_pem, ca_key).map_err(cert_error)?;
        let key = KeyPair::generate().map_err(cert_error)?;
        let cert = leaf_params(node_name)?
            .signed_by(&key, &issuer)
            .map_err(cert_error)?;
        Ok(Self {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
            ca_pem: Some(ca_pem.to_owned()),
            ca_key_pem: Some(ca_key_pem.to_owned()),
        })
    }

    /// A new server certificate (new key, new fingerprint), signed by the
    /// same CA when there is one.
    ///
    /// # Errors
    /// Key generation or signing failed.
    pub fn rotated(&self, node_name: &str) -> Result<Self, MockError> {
        match (&self.ca_pem, &self.ca_key_pem) {
            (Some(ca), Some(key)) => Self::signed_by_ca(node_name, ca, key),
            _ => Self::self_signed(node_name),
        }
    }

    /// A client certificate with common name `cn`, signed by the CA (for
    /// users with a `client_cn`).
    ///
    /// # Errors
    /// There's no CA key, or signing failed.
    pub fn client_certificate(&self, cn: &str) -> Result<(String, String), MockError> {
        let (Some(ca_pem), Some(ca_key_pem)) = (&self.ca_pem, &self.ca_key_pem) else {
            return Err(MockError::Certificate(
                "client certificates need CA-signed TLS".to_owned(),
            ));
        };
        let ca_key = KeyPair::from_pem(ca_key_pem).map_err(cert_error)?;
        let issuer = Issuer::from_ca_cert_pem(ca_pem, ca_key).map_err(cert_error)?;
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, cn);
        params.distinguished_name = dn;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        params.use_authority_key_identifier_extension = true;
        let (not_before, not_after) = validity(397);
        params.not_before = not_before;
        params.not_after = not_after;
        let key = KeyPair::generate().map_err(cert_error)?;
        let cert = params.signed_by(&key, &issuer).map_err(cert_error)?;
        Ok((cert.pem(), key.serialize_pem()))
    }

    /// SHA-256 of the server certificate's DER encoding.
    ///
    /// # Errors
    /// The certificate PEM can't be parsed.
    pub fn sha256(&self) -> Result<[u8; 32], MockError> {
        let der = CertificateDer::from_pem_slice(self.cert_pem.as_bytes()).map_err(cert_error)?;
        Ok(sha256(&der))
    }
}

/// SHA-256 of DER bytes.
pub(crate) fn sha256(der: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, der);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// A fingerprint as colon-separated uppercase hex (`AB:CD:...`).
#[must_use]
pub fn format_fingerprint(fingerprint: &[u8; 32]) -> String {
    fingerprint
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// The rustls side of a running server.
pub(crate) struct TlsState {
    pub(crate) material: TlsMaterial,
    pub(crate) acceptor: tokio_rustls::TlsAcceptor,
    pub(crate) fingerprint: [u8; 32],
}

impl fmt::Debug for TlsState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsState")
            .field("fingerprint", &format_fingerprint(&self.fingerprint))
            .finish_non_exhaustive()
    }
}

/// Builds the server configuration: TLS 1.2+, no ALPN (HTTP/1.1 like
/// Icinga), optional client certificates when there's a CA.
pub(crate) fn server_state(material: TlsMaterial) -> Result<TlsState, MockError> {
    let chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(material.cert_pem.as_bytes())
            .collect::<Result<_, _>>()
            .map_err(cert_error)?;
    let Some(leaf) = chain.first() else {
        return Err(MockError::Certificate(
            "no certificate in cert_pem".to_owned(),
        ));
    };
    let fingerprint = sha256(leaf);
    let key = PrivateKeyDer::from_pem_slice(material.key_pem.as_bytes()).map_err(cert_error)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| MockError::Tls(e.to_string()))?;
    let builder = match &material.ca_pem {
        Some(ca_pem) => {
            let mut roots = RootCertStore::empty();
            for ca in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
                roots
                    .add(ca.map_err(cert_error)?)
                    .map_err(|e| MockError::Tls(e.to_string()))?;
            }
            let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                .allow_unauthenticated()
                .build()
                .map_err(|e| MockError::Tls(e.to_string()))?;
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    };
    let config = builder
        .with_single_cert(chain, key)
        .map_err(|e| MockError::Tls(e.to_string()))?;
    Ok(TlsState {
        material,
        acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(config)),
        fingerprint,
    })
}

/// The common name of a client certificate.
pub(crate) fn common_name(der: &[u8]) -> Option<String> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    cert.subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_signed_material_builds_a_server() {
        let material = TlsMaterial::self_signed("master-01").unwrap();
        let state = server_state(material.clone()).unwrap();
        assert_eq!(state.fingerprint, material.sha256().unwrap());
        assert!(material.ca_pem.is_none());
        assert!(!format!("{material:?}").contains("PRIVATE KEY"));
    }

    #[test]
    fn ca_signed_material_rotates_under_the_same_ca() {
        let material = TlsMaterial::ca_signed("master-01").unwrap();
        let rotated = material.rotated("master-01").unwrap();
        assert_eq!(rotated.ca_pem, material.ca_pem);
        assert_ne!(rotated.sha256().unwrap(), material.sha256().unwrap());
        server_state(rotated).unwrap();
    }

    #[test]
    fn client_certificates_carry_the_common_name() {
        let material = TlsMaterial::ca_signed("master-01").unwrap();
        let (cert, _) = material.client_certificate("icygui-client").unwrap();
        let der = CertificateDer::from_pem_slice(cert.as_bytes()).unwrap();
        assert_eq!(common_name(&der).as_deref(), Some("icygui-client"));
        assert!(
            TlsMaterial::self_signed("x")
                .unwrap()
                .client_certificate("y")
                .is_err()
        );
    }

    #[test]
    fn fingerprints_are_colon_hex() {
        let text = format_fingerprint(&[0xab; 32]);
        assert_eq!(text.len(), 32 * 3 - 1);
        assert!(text.starts_with("AB:AB:"));
    }
}
