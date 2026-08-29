//! TLS material shared by the TCP and QUIC adapters: SPKI pinning and a
//! stream wrapper that carries either a plain or a TLS-protected connection.
//!
//! Pinning exists because these deployments run on bare IPs with self-signed
//! certificates — no CA can vouch for them, so chain validation would only ever
//! produce `UnknownIssuer`. Pinning the server's public key gives back the one
//! property that matters: this is the server we shipped, not whoever answered.

use std::sync::Arc;

use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};

/// SHA-256 of a certificate's SubjectPublicKeyInfo, base64-encoded.
///
/// This is the value a client pins. It is derived from the public key rather
/// than the whole certificate so the certificate can be re-issued for the same
/// key without invalidating every deployed pin.
pub fn spki_sha256_base64(cert_der: &[u8]) -> Result<String, String> {
    use x509_parser::prelude::FromDer;
    let (_, cert) = x509_parser::certificate::X509Certificate::from_der(cert_der)
        .map_err(|e| format!("failed to parse certificate: {e}"))?;
    Ok(base64_encode(&sha256(cert.public_key().raw)))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// Minimal standard-alphabet base64 with padding. Avoids pulling in a crate for
/// one 32-byte encode.
fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// A `ServerCertVerifier` that accepts a server only when its SPKI matches one
/// of the configured pins.
///
/// Multiple pins are supported so a key can be rotated without downtime: ship a
/// client that accepts both the current and the next pin, switch the server,
/// then drop the old pin in a later release. A single-pin design means a lost
/// or rotated server key locks out every client permanently.
///
/// Unlike a skip-verification verifier this still checks the handshake
/// signatures. Comparing the SPKI alone would only prove the peer presented a
/// certificate containing that key — anyone can copy a public certificate. The
/// signature check is what proves possession of the matching private key.
#[derive(Debug)]
pub struct PinnedSpkiVerification {
    pins: Vec<String>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl PinnedSpkiVerification {
    /// Build a verifier from base64 SHA-256 SPKI pins. Rejects an empty pin set:
    /// a verifier that trusts nothing is a configuration error, and silently
    /// accepting everything would be worse.
    pub fn new(pins: Vec<String>) -> Result<Self, String> {
        if pins.is_empty() {
            return Err("at least one SPKI pin is required".to_string());
        }
        if let Some(bad) = pins.iter().find(|p| p.trim().is_empty()) {
            return Err(format!("empty SPKI pin in list: {bad:?}"));
        }
        Ok(Self {
            pins,
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        })
    }

    pub fn pins(&self) -> &[String] {
        &self.pins
    }
}

impl ServerCertVerifier for PinnedSpkiVerification {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let presented = spki_sha256_base64(end_entity.as_ref()).map_err(|e| {
            rustls::Error::General(format!("cannot derive SPKI from server certificate: {e}"))
        })?;

        if self.pins.iter().any(|p| p == &presented) {
            return Ok(ServerCertVerified::assertion());
        }

        // The presented pin is deliberately included: an operator staring at a
        // pin mismatch needs to know what the server actually offered, and a
        // public key is not a secret.
        Err(rustls::Error::General(format!(
            "server SPKI pin mismatch: presented {presented}, expected one of {:?}",
            self.pins
        )))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
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
        rustls::crypto::verify_tls13_signature(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cert_der(cn: &str) -> (Vec<u8>, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec![cn.to_string()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let der = cert.der().to_vec();
        let pin = spki_sha256_base64(&der).unwrap();
        (der, pin)
    }

    #[test]
    fn spki_is_stable_for_the_same_key() {
        let (der, pin) = cert_der("localhost");
        assert_eq!(spki_sha256_base64(&der).unwrap(), pin);
        assert_eq!(pin.len(), 44, "base64 of a 32-byte digest");
        assert!(pin.ends_with('='));
    }

    #[test]
    fn different_keys_produce_different_pins() {
        let (_, a) = cert_der("localhost");
        let (_, b) = cert_der("localhost");
        assert_ne!(a, b);
    }

    #[test]
    fn matching_pin_is_accepted() {
        let (der, pin) = cert_der("localhost");
        let v = PinnedSpkiVerification::new(vec![pin]).unwrap();
        let cert = CertificateDer::from(der);
        assert!(v
            .verify_server_cert(
                &cert,
                &[],
                &ServerName::try_from("localhost").unwrap(),
                &[],
                UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000)),
            )
            .is_ok());
    }

    #[test]
    fn foreign_certificate_is_rejected() {
        let (_, pin) = cert_der("localhost");
        let (other_der, _) = cert_der("localhost");
        let v = PinnedSpkiVerification::new(vec![pin]).unwrap();
        let cert = CertificateDer::from(other_der);
        let err = v
            .verify_server_cert(
                &cert,
                &[],
                &ServerName::try_from("localhost").unwrap(),
                &[],
                UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000)),
            )
            .unwrap_err();
        assert!(format!("{err}").contains("pin mismatch"));
    }

    /// Rotation: a client shipped with both pins keeps working across the
    /// server-side key switch.
    #[test]
    fn either_of_two_pins_is_accepted() {
        let (current_der, current) = cert_der("localhost");
        let (next_der, next) = cert_der("localhost");
        let v = PinnedSpkiVerification::new(vec![current, next]).unwrap();
        let now = UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000));
        let name = ServerName::try_from("localhost").unwrap();
        for der in [current_der, next_der] {
            let cert = CertificateDer::from(der);
            assert!(v.verify_server_cert(&cert, &[], &name, &[], now).is_ok());
        }
    }

    #[test]
    fn empty_pin_set_is_refused() {
        assert!(PinnedSpkiVerification::new(vec![]).is_err());
        assert!(PinnedSpkiVerification::new(vec!["  ".to_string()]).is_err());
    }
}
