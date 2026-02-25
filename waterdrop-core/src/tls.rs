use anyhow::{Context, Result};

/// Raw DER-encoded certificate and private key material.
///
/// Produced by [`generate_self_signed_cert`] and consumed by transport
/// implementations (QUIC, TLS-over-TCP, …) to build their TLS configs.
pub struct CertKeyPair {
    pub cert_der: Vec<u8>,
    pub private_key_pkcs8_der: Vec<u8>,
}

/// Generates a self-signed certificate for the given `subjects`.
///
/// The returned DER bytes are transport-agnostic — callers wrap them in
/// whatever TLS library their transport requires (e.g. `rustls` for QUIC).
///
/// # Errors
///
/// Returns an error if certificate generation fails.
pub fn generate_self_signed_cert(subjects: &[&str]) -> Result<CertKeyPair> {
    let subjects: Vec<String> = subjects.iter().map(|&s| s.to_string()).collect();

    let certified_key = rcgen::generate_simple_self_signed(subjects)
        .context("failed to generate self-signed certificate")?;

    Ok(CertKeyPair {
        cert_der: certified_key.cert.der().to_vec(),
        private_key_pkcs8_der: certified_key.key_pair.serialize_der(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_valid_subjects_when_generating_cert_then_returns_non_empty_der() {
        let pair = generate_self_signed_cert(&["localhost"]).unwrap();
        assert!(!pair.cert_der.is_empty());
        assert!(!pair.private_key_pkcs8_der.is_empty());
    }

    #[test]
    fn given_multiple_subjects_when_generating_cert_then_succeeds() {
        let pair = generate_self_signed_cert(&["localhost", "waterdrop.local"]).unwrap();
        assert!(!pair.cert_der.is_empty());
    }

    #[test]
    fn given_two_calls_when_generating_certs_then_keys_differ() {
        let a = generate_self_signed_cert(&["localhost"]).unwrap();
        let b = generate_self_signed_cert(&["localhost"]).unwrap();
        assert_ne!(a.private_key_pkcs8_der, b.private_key_pkcs8_der);
    }
}
