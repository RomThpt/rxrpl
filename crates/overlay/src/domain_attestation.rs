//! Validator domain attestation via `xrp-ledger.toml`.
//!
//! A validator may publish a domain claim in its manifest (sfDomain).
//! To prove ownership we fetch
//! `https://<domain>/.well-known/xrp-ledger.toml`, parse the TOML, and
//! check that the validator's master public key appears in the
//! `[[VALIDATORS]]` array. Reference: rippled `ValidatorList::domainVerified`.
//!
//! The fetcher uses HTTPS with strict TLS validation (reqwest default).
//! Body size is capped at 64 KiB. A configurable timeout (default 10 s)
//! prevents the background task from hanging.

use std::time::Duration;

use rxrpl_primitives::PublicKey;
use serde::Deserialize;

/// Default per-request HTTP timeout for `xrp-ledger.toml` fetches.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum bytes accepted from a single TOML response.
pub const MAX_BODY: u64 = 64 * 1024;

/// Errors produced while fetching or verifying a domain attestation.
#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("body exceeds {MAX_BODY} byte cap")]
    BodyTooLarge,
    #[error("TOML parse error: {0}")]
    Parse(String),
    #[error("invalid hex public_key in TOML: {0}")]
    InvalidKey(String),
    #[error("invalid domain: {0}")]
    InvalidDomain(String),
}

/// Schema for the relevant portion of `xrp-ledger.toml`.
#[derive(Debug, Deserialize, Default)]
struct XrpLedgerToml {
    #[serde(rename = "VALIDATORS", default)]
    validators: Vec<TomlValidator>,
}

#[derive(Debug, Deserialize)]
struct TomlValidator {
    public_key: String,
    #[serde(default)]
    #[allow(dead_code)]
    network: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    attestation: Option<String>,
}

/// Fetcher that resolves and validates `xrp-ledger.toml` attestations.
pub struct DomainAttestationFetcher {
    http: reqwest::Client,
    /// Optional override for the base URL (test injection). When set, the
    /// fetcher targets `<base_url>/.well-known/xrp-ledger.toml` instead of
    /// `https://<domain>/...`.
    base_url_override: Option<String>,
}

impl DomainAttestationFetcher {
    /// Build a new fetcher with strict TLS and the given timeout.
    pub fn new(timeout: Duration) -> Result<Self, AttestationError> {
        // reqwest validates TLS certificates by default; we never call
        // `danger_accept_invalid_certs`. Audit: any future override here
        // must be rejected.
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .https_only(true)
            .user_agent(concat!("rxrpl/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| AttestationError::Http(e.to_string()))?;
        Ok(Self {
            http,
            base_url_override: None,
        })
    }

    /// Test helper: target the given base URL (e.g. an httpmock server)
    /// instead of `https://<domain>/`. Strict TLS still applies but
    /// `https_only` is relaxed so plain-HTTP mocks can be exercised.
    #[doc(hidden)]
    pub fn with_base_url_for_tests(base_url: impl Into<String>) -> Result<Self, AttestationError> {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .user_agent(concat!("rxrpl/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| AttestationError::Http(e.to_string()))?;
        Ok(Self {
            http,
            base_url_override: Some(base_url.into()),
        })
    }

    /// Fetch `https://<domain>/.well-known/xrp-ledger.toml` and check
    /// whether `master_key` appears in the `[[VALIDATORS]]` array.
    ///
    /// Returns `Ok(true)` on a successful match, `Ok(false)` if the TOML
    /// parses but the key is absent, and `Err` on transport/parse errors.
    pub async fn fetch_and_verify(
        &self,
        domain: &str,
        master_key: &PublicKey,
    ) -> Result<bool, AttestationError> {
        validate_domain(domain)?;
        let url = match &self.base_url_override {
            Some(base) => format!(
                "{}/.well-known/xrp-ledger.toml",
                base.trim_end_matches('/')
            ),
            None => format!("https://{domain}/.well-known/xrp-ledger.toml"),
        };

        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AttestationError::Http(e.to_string()))?
            .error_for_status()
            .map_err(|e| AttestationError::Http(e.to_string()))?;

        if let Some(len) = resp.content_length() {
            if len > MAX_BODY {
                return Err(AttestationError::BodyTooLarge);
            }
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AttestationError::Http(format!("read body: {e}")))?;
        if bytes.len() as u64 > MAX_BODY {
            return Err(AttestationError::BodyTooLarge);
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|e| AttestationError::Parse(format!("non-UTF-8: {e}")))?;

        verify_toml(text, master_key)
    }
}

/// Validate the manifest-supplied domain string.
///
/// rippled accepts any printable host; we reject empty strings, embedded
/// whitespace, and slashes (which would let an attacker smuggle a path).
fn validate_domain(domain: &str) -> Result<(), AttestationError> {
    if domain.is_empty() {
        return Err(AttestationError::InvalidDomain("empty".into()));
    }
    if domain
        .chars()
        .any(|c| c.is_whitespace() || c == '/' || c == '\\' || c.is_control())
    {
        return Err(AttestationError::InvalidDomain(
            "contains illegal characters".into(),
        ));
    }
    Ok(())
}

/// Parse a TOML body and check membership of `master_key`.
fn verify_toml(text: &str, master_key: &PublicKey) -> Result<bool, AttestationError> {
    let parsed: XrpLedgerToml =
        toml::from_str(text).map_err(|e| AttestationError::Parse(e.to_string()))?;
    let target = hex::encode_upper(master_key.as_bytes());
    for entry in &parsed.validators {
        let candidate_hex = entry.public_key.trim().to_ascii_uppercase();
        let normalized = candidate_hex.strip_prefix("0X").unwrap_or(&candidate_hex);
        if normalized == target {
            return Ok(true);
        }
        // Validate that `public_key` is at least valid hex; if not, surface
        // an error so a malformed TOML is loud rather than silently absent.
        if hex::decode(normalized).is_err() {
            return Err(AttestationError::InvalidKey(entry.public_key.clone()));
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed_key(byte: u8) -> PublicKey {
        let mut buf = vec![0xED; 33];
        buf[1] = byte;
        PublicKey::from_slice(&buf).unwrap()
    }

    #[test]
    fn verify_toml_finds_matching_key() {
        let key = ed_key(0x01);
        let hex = hex::encode_upper(key.as_bytes());
        let toml = format!(
            "[[VALIDATORS]]\npublic_key = \"{hex}\"\nnetwork = \"main\"\n"
        );
        assert_eq!(verify_toml(&toml, &key).unwrap(), true);
    }

    #[test]
    fn verify_toml_returns_false_when_absent() {
        let key = ed_key(0x01);
        let other = ed_key(0x02);
        let hex = hex::encode_upper(other.as_bytes());
        let toml = format!("[[VALIDATORS]]\npublic_key = \"{hex}\"\n");
        assert_eq!(verify_toml(&toml, &key).unwrap(), false);
    }

    #[test]
    fn verify_toml_rejects_invalid_hex() {
        let key = ed_key(0x01);
        let toml = "[[VALIDATORS]]\npublic_key = \"NOT_HEX_AT_ALL\"\n";
        assert!(matches!(
            verify_toml(toml, &key),
            Err(AttestationError::InvalidKey(_))
        ));
    }

    #[test]
    fn verify_toml_accepts_lowercase_hex() {
        let key = ed_key(0x01);
        let hex = hex::encode(key.as_bytes());
        let toml = format!("[[VALIDATORS]]\npublic_key = \"{hex}\"\n");
        assert_eq!(verify_toml(&toml, &key).unwrap(), true);
    }

    #[test]
    fn validate_domain_rejects_empty_and_paths() {
        assert!(validate_domain("").is_err());
        assert!(validate_domain("foo bar").is_err());
        assert!(validate_domain("foo/bar").is_err());
        assert!(validate_domain("example.com").is_ok());
    }
}
