use std::path::Path;

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

/// Returns the SHA-256 fingerprint of a DER-encoded certificate as a
/// lowercase hex string (64 characters).
#[must_use]
pub fn fingerprint_sha256(cert_der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(cert_der);
    hex::encode(hash)
}

/// Saves a [`CertKeyPair`] to `dir` as two files: `cert.der` and `key.der`.
///
/// Creates `dir` (and parents) if it does not exist. Existing files are
/// overwritten so callers can regenerate the identity when needed.
///
/// # Errors
///
/// Returns an error if directory creation or file writing fails.
pub fn save_cert_pair(pair: &CertKeyPair, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create cert directory {}", dir.display()))?;

    let cert_path = dir.join("cert.der");
    let key_path = dir.join("key.der");

    std::fs::write(&cert_path, &pair.cert_der)
        .with_context(|| format!("failed to write {}", cert_path.display()))?;
    std::fs::write(&key_path, &pair.private_key_pkcs8_der)
        .with_context(|| format!("failed to write {}", key_path.display()))?;

    Ok(())
}

/// Loads a previously saved [`CertKeyPair`] from `dir`.
///
/// Returns `Ok(None)` if neither `cert.der` nor `key.der` exist. Returns
/// an error if only one of the two files exists or if reading fails.
///
/// # Errors
///
/// Returns an error if only one of the two files exists, or if reading
/// either file fails.
pub fn load_cert_pair(dir: &Path) -> Result<Option<CertKeyPair>> {
    let cert_path = dir.join("cert.der");
    let key_path = dir.join("key.der");

    let cert_exists = cert_path.exists();
    let key_exists = key_path.exists();

    if !cert_exists && !key_exists {
        return Ok(None);
    }

    if cert_exists != key_exists {
        anyhow::bail!(
            "incomplete cert pair in {}: cert.der exists={cert_exists}, key.der exists={key_exists}",
            dir.display()
        );
    }

    let cert_der = std::fs::read(&cert_path)
        .with_context(|| format!("failed to read {}", cert_path.display()))?;
    let private_key_pkcs8_der = std::fs::read(&key_path)
        .with_context(|| format!("failed to read {}", key_path.display()))?;

    Ok(Some(CertKeyPair {
        cert_der,
        private_key_pkcs8_der,
    }))
}

/// Loads an existing [`CertKeyPair`] from `dir`, or generates a new one
/// for `subjects`, saves it, and returns it.
///
/// # Errors
///
/// Returns an error if loading, generating, or saving the certificate fails.
pub fn load_or_generate_cert(dir: &Path, subjects: &[&str]) -> Result<CertKeyPair> {
    if let Some(pair) = load_cert_pair(dir)? {
        tracing::info!(dir = %dir.display(), "loaded existing TLS certificate");
        return Ok(pair);
    }

    tracing::info!(dir = %dir.display(), "generating new self-signed TLS certificate");
    let pair = generate_self_signed_cert(subjects)?;
    save_cert_pair(&pair, dir)?;
    Ok(pair)
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

    #[test]
    fn given_cert_der_when_fingerprinted_then_returns_64_hex_chars() {
        let pair = generate_self_signed_cert(&["localhost"]).unwrap();
        let fp = fingerprint_sha256(&pair.cert_der);
        assert_eq!(fp.len(), 64);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn given_same_cert_when_fingerprinted_twice_then_results_match() {
        let pair = generate_self_signed_cert(&["localhost"]).unwrap();
        let fp1 = fingerprint_sha256(&pair.cert_der);
        let fp2 = fingerprint_sha256(&pair.cert_der);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn given_different_certs_when_fingerprinted_then_results_differ() {
        let a = generate_self_signed_cert(&["localhost"]).unwrap();
        let b = generate_self_signed_cert(&["localhost"]).unwrap();
        assert_ne!(
            fingerprint_sha256(&a.cert_der),
            fingerprint_sha256(&b.cert_der)
        );
    }

    #[test]
    fn given_cert_pair_when_saved_and_loaded_then_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let original = generate_self_signed_cert(&["localhost"]).unwrap();
        save_cert_pair(&original, dir.path()).unwrap();

        let loaded = load_cert_pair(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.cert_der, original.cert_der);
        assert_eq!(loaded.private_key_pkcs8_der, original.private_key_pkcs8_der);
    }

    #[test]
    fn given_empty_dir_when_loaded_then_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_cert_pair(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn given_only_cert_file_when_loaded_then_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cert.der"), b"dummy").unwrap();
        let result = load_cert_pair(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn given_only_key_file_when_loaded_then_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("key.der"), b"dummy").unwrap();
        let result = load_cert_pair(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn given_empty_dir_when_load_or_generate_then_generates_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let pair = load_or_generate_cert(dir.path(), &["localhost"]).unwrap();
        assert!(!pair.cert_der.is_empty());
        assert!(dir.path().join("cert.der").exists());
        assert!(dir.path().join("key.der").exists());
    }

    #[test]
    fn given_existing_cert_when_load_or_generate_then_reuses_it() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_generate_cert(dir.path(), &["localhost"]).unwrap();
        let second = load_or_generate_cert(dir.path(), &["localhost"]).unwrap();
        assert_eq!(first.cert_der, second.cert_der);
        assert_eq!(first.private_key_pkcs8_der, second.private_key_pkcs8_der);
    }
}
