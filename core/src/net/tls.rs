//! QUIC/TLS setup.
//!
//! Devices use self-signed certificates, so the TLS layer only proves that
//! each side holds the private key for the certificate it presents. Whether
//! that certificate is trusted is decided right after the handshake, before
//! any data is exchanged: a normal connection must present the exact
//! certificate pinned at pairing time, and a pairing connection must prove
//! knowledge of the pairing code (see `pairing.rs`).

use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

use crate::identity::{fingerprint, Identity};
use crate::protocol::{ALPN, ALPN_PAIR};

#[derive(Debug)]
struct ProofOfKey(Arc<CryptoProvider>);

impl ServerCertVerifier for ProofOfKey {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

impl ClientCertVerifier for ProofOfKey {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn transport() -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    // Notice a vanished peer within a few seconds so the cursor comes home.
    t.keep_alive_interval(Some(Duration::from_millis(1500)));
    t.max_idle_timeout(Some(Duration::from_secs(6).try_into().unwrap()));
    // Plenty of room for parallel file streams on a LAN.
    t.max_concurrent_bidi_streams(256u32.into());
    t.stream_receive_window((8u32 * 1024 * 1024).into());
    t.receive_window((32u32 * 1024 * 1024).into());
    t.send_window(32 * 1024 * 1024);
    Arc::new(t)
}

pub fn server_config(id: &Identity) -> anyhow::Result<quinn::ServerConfig> {
    let provider = provider();
    let mut tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(ProofOfKey(provider)))
        .with_single_cert(vec![id.cert.clone()], id.key())?;
    tls.alpn_protocols = vec![ALPN.to_vec(), ALPN_PAIR.to_vec()];
    tls.max_early_data_size = 0;
    let qsc = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(qsc));
    cfg.transport_config(transport());
    Ok(cfg)
}

pub fn client_config(id: &Identity, alpn: &[u8]) -> anyhow::Result<quinn::ClientConfig> {
    let provider = provider();
    let mut tls = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(ProofOfKey(provider)))
        .with_client_auth_cert(vec![id.cert.clone()], id.key())?;
    tls.alpn_protocols = vec![alpn.to_vec()];
    tls.enable_early_data = false;
    let qcc = quinn::crypto::rustls::QuicClientConfig::try_from(tls)?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(qcc));
    cfg.transport_config(transport());
    Ok(cfg)
}

/// Fingerprint of the certificate the other side presented.
pub fn peer_fingerprint(conn: &quinn::Connection) -> Option<String> {
    let ident = conn.peer_identity()?;
    let certs = ident.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    certs.first().map(|c| fingerprint(c.as_ref()))
}

pub fn negotiated_alpn(conn: &quinn::Connection) -> Option<Vec<u8>> {
    let data = conn.handshake_data()?;
    let data = data.downcast::<quinn::crypto::rustls::HandshakeData>().ok()?;
    data.protocol
}
