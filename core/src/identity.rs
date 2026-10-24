//! This device's certificate. Generated once; the private key is encrypted
//! with DPAPI so only this Windows account can read it.

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

use crate::config::data_dir;
use crate::protocol::DeviceId;

#[derive(Clone)]
pub struct Identity {
    pub cert: CertificateDer<'static>,
    key: Vec<u8>,
    pub fingerprint: String,
}

impl Identity {
    pub fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()))
    }

    pub fn load_or_create(device: &DeviceId) -> anyhow::Result<Self> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir)?;
        let cert_path = dir.join("device.crt");
        let key_path = dir.join("device.key");

        if let (Ok(cert), Ok(sealed)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
            match crate::win::dpapi_unprotect(&sealed) {
                Ok(key) => return Ok(Self::from_parts(cert, key)),
                Err(e) => tracing::warn!("device key unreadable ({e}), making a new identity; pair again"),
            }
        }

        let ck = rcgen::generate_simple_self_signed(vec![format!("seam-{device}")])?;
        let cert = ck.cert.der().to_vec();
        let key = ck.key_pair.serialize_der();
        std::fs::write(&cert_path, &cert)?;
        std::fs::write(&key_path, crate::win::dpapi_protect(&key)?)?;
        Ok(Self::from_parts(cert, key))
    }

    /// A throwaway identity that never touches disk (tests, previews).
    pub fn ephemeral(device: &DeviceId) -> anyhow::Result<Self> {
        let ck = rcgen::generate_simple_self_signed(vec![format!("seam-{device}")])?;
        Ok(Self::from_parts(ck.cert.der().to_vec(), ck.key_pair.serialize_der()))
    }

    fn from_parts(cert: Vec<u8>, key: Vec<u8>) -> Self {
        let fingerprint = fingerprint(&cert);
        Self { cert: CertificateDer::from(cert), key, fingerprint }
    }
}

pub fn fingerprint(der: &[u8]) -> String {
    let hash = Sha256::digest(der);
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// Short form shown in the UI so people can compare devices by eye.
pub fn short_fingerprint(fp: &str) -> String {
    fp.chars()
        .take(16)
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase()
}
