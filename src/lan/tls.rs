//! Self-signed TLS 1.3 identity generated on the fly (`rcgen`) plus a
//! fingerprint-pinning client verifier.
//!
//! Model: the receiver announces `sha256(DER certificate)` via mDNS TXT; the
//! sender pins that fingerprint when connecting. This gives confidentiality
//! *and* authenticity of the peer without any CA. Identities are persisted so
//! a device keeps the same fingerprint across restarts.

use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::ring as provider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};
use std::path::Path;
use std::sync::Arc;

pub struct Identity {
    pub cert_der: CertificateDer<'static>,
    pub key_der: PrivatePkcs8KeyDer<'static>,
    /// Lower-case hex SHA-256 of the DER certificate.
    pub fingerprint: String,
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Self { cert_der: self.cert_der.clone(), key_der: self.key_der.clone_key(), fingerprint: self.fingerprint.clone() }
    }
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity").field("fingerprint", &self.fingerprint).finish()
    }
}

pub fn fingerprint_of(cert: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, cert);
    hex::encode(digest.as_ref())
}

/// Short, human-friendly form: first 16 hex chars grouped by 4.
pub fn short_fingerprint(fp: &str) -> String {
    fp.chars()
        .take(16)
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
        .to_uppercase()
}

impl Identity {
    pub fn generate(device_name: &str) -> Result<Self> {
        let sans = vec!["localhost".to_string(), format!("{device_name}.local")];
        let key = rcgen::KeyPair::generate().context("generating key pair")?;
        let mut params = rcgen::CertificateParams::new(sans).context("cert params")?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, format!("uni-share {device_name}"));
        params.not_before = rcgen::date_time_ymd(2024, 1, 1);
        params.not_after = rcgen::date_time_ymd(2124, 1, 1);
        let cert = params.self_signed(&key).context("self-signing cert")?;
        let cert_der = CertificateDer::from(cert.der().to_vec());
        let key_der = PrivatePkcs8KeyDer::from(key.serialize_der());
        let fingerprint = fingerprint_of(&cert_der);
        Ok(Self { cert_der, key_der, fingerprint })
    }

    /// Load from `dir/identity.{crt,key}` or generate + persist.
    pub fn load_or_generate(dir: &Path, device_name: &str) -> Result<Self> {
        let crt = dir.join("identity.crt");
        let key = dir.join("identity.key");
        if crt.exists() && key.exists() {
            let cert_der = CertificateDer::from(std::fs::read(&crt)?);
            let fingerprint = fingerprint_of(&cert_der);
            return Ok(Self {
                cert_der,
                key_der: PrivatePkcs8KeyDer::from(std::fs::read(&key)?),
                fingerprint,
            });
        }
        let id = Self::generate(device_name)?;
        std::fs::create_dir_all(dir)?;
        std::fs::write(&crt, id.cert_der.as_ref())?;
        std::fs::write(&key, id.key_der.secret_pkcs8_der())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600));
        }
        Ok(id)
    }

    pub fn server_config(&self) -> Result<Arc<ServerConfig>> {
        let key = PrivateKeyDer::Pkcs8(self.key_der.clone_key());
        let mut cfg = ServerConfig::builder_with_provider(Arc::new(provider::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .context("tls versions")?
            .with_no_client_auth()
            .with_single_cert(vec![self.cert_der.clone()], key)
            .context("server tls config")?;
        cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Arc::new(cfg))
    }
}

/// Verifier that accepts exactly one certificate fingerprint (or any when
/// `None`, e.g. after the user confirmed the fingerprint manually).
#[derive(Debug)]
pub struct PinnedVerifier {
    expected: Option<String>,
    supported: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl PinnedVerifier {
    pub fn new(expected_fingerprint: Option<&str>) -> Self {
        Self {
            expected: expected_fingerprint.map(|s| s.to_ascii_lowercase()),
            supported: provider::default_provider().signature_verification_algorithms,
        }
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint_of(end_entity);
        match &self.expected {
            Some(exp) if exp != &fp => Err(rustls::Error::General(format!(
                "certificate fingerprint mismatch (expected {}, got {})",
                short_fingerprint(exp),
                short_fingerprint(&fp)
            ))),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.supported)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.supported)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported.supported_schemes()
    }
}

pub fn pinned_client_config(fingerprint: Option<&str>) -> Result<ClientConfig> {
    let mut cfg = ClientConfig::builder_with_provider(Arc::new(provider::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("tls versions")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier::new(fingerprint)))
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}

/// reqwest client that trusts only the pinned peer certificate.
pub fn pinned_reqwest_client(fingerprint: Option<&str>) -> Result<reqwest::Client> {
    let tls = pinned_client_config(fingerprint)?;
    reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .user_agent(crate::user_agent())
        .tcp_nodelay(true)
        .http2_keep_alive_interval(std::time::Duration::from_secs(10))
        .build()
        .context("building pinned http client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_generation_and_fingerprint() {
        let id = Identity::generate("TestBox").unwrap();
        assert_eq!(id.fingerprint.len(), 64);
        assert_eq!(id.fingerprint, fingerprint_of(&id.cert_der));
        assert!(id.server_config().is_ok());
        assert_eq!(short_fingerprint(&id.fingerprint).len(), 19);
    }

    #[test]
    fn identity_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let a = Identity::load_or_generate(dir.path(), "X").unwrap();
        let b = Identity::load_or_generate(dir.path(), "X").unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
    }

    #[test]
    fn pinned_verifier_logic() {
        let id = Identity::generate("A").unwrap();
        let other = Identity::generate("B").unwrap();
        let v = PinnedVerifier::new(Some(&id.fingerprint));
        let name = ServerName::try_from("localhost").unwrap();
        assert!(v.verify_server_cert(&id.cert_der, &[], &name, &[], UnixTime::now()).is_ok());
        assert!(v.verify_server_cert(&other.cert_der, &[], &name, &[], UnixTime::now()).is_err());
        let any = PinnedVerifier::new(None);
        assert!(any.verify_server_cert(&other.cert_der, &[], &name, &[], UnixTime::now()).is_ok());
        assert!(pinned_client_config(Some(&id.fingerprint)).is_ok());
    }
}
