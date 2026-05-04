//! rippled-style `validator-token` decoder.
//!
//! A validator token (as produced by rippled's `validator-keys` tool) is a
//! base64-encoded JSON object of the form:
//!
//! ```json
//! {
//!   "manifest": "<base64-encoded manifest STObject>",
//!   "validation_secret_key": "<hex-encoded ephemeral secret key, 32 bytes>"
//! }
//! ```
//!
//! This module decodes that bundle into raw bytes; interpretation
//! (rebuilding a [`KeyPair`] from the secret + the manifest's embedded
//! ephemeral pub-key) is the caller's responsibility.

use base64::Engine;
use serde::Deserialize;

/// Parsed validator token (raw bytes, ready for crypto consumption).
#[derive(Clone, Debug)]
pub struct ValidatorToken {
    /// Manifest bytes (signed STObject) — feed to `manifest::parse_and_verify`.
    pub manifest: Vec<u8>,
    /// Ephemeral validation secret key (raw private-key bytes).
    pub validation_secret_key: Vec<u8>,
}

/// Errors raised when parsing a rippled-style validator token.
#[derive(Debug, thiserror::Error)]
pub enum ValidatorTokenError {
    #[error("token is not valid base64: {0}")]
    OuterBase64(#[source] base64::DecodeError),
    #[error("token does not decode to valid UTF-8 JSON: {0}")]
    Utf8(#[source] std::string::FromUtf8Error),
    #[error("token JSON is malformed: {0}")]
    Json(#[source] serde_json::Error),
    #[error("`manifest` field is not valid base64: {0}")]
    ManifestBase64(#[source] base64::DecodeError),
    #[error("`validation_secret_key` field is not valid hex: {0}")]
    SecretKeyHex(#[source] hex::FromHexError),
}

#[derive(Deserialize)]
struct RawToken {
    manifest: String,
    validation_secret_key: String,
}

/// Decode a rippled-style validator token (outer base64 → JSON → parts).
///
/// Whitespace inside `token` is tolerated (rippled formats tokens with line
/// breaks every 72 chars).
pub fn parse_validator_token(token: &str) -> Result<ValidatorToken, ValidatorTokenError> {
    let stripped: String = token.chars().filter(|c| !c.is_whitespace()).collect();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(stripped.as_bytes())
        .map_err(ValidatorTokenError::OuterBase64)?;
    let json_str = String::from_utf8(decoded).map_err(ValidatorTokenError::Utf8)?;
    let raw: RawToken = serde_json::from_str(&json_str).map_err(ValidatorTokenError::Json)?;
    let manifest = base64::engine::general_purpose::STANDARD
        .decode(raw.manifest.as_bytes())
        .map_err(ValidatorTokenError::ManifestBase64)?;
    let validation_secret_key = hex::decode(raw.validation_secret_key.as_bytes())
        .map_err(ValidatorTokenError::SecretKeyHex)?;
    Ok(ValidatorToken {
        manifest,
        validation_secret_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a token from raw parts (test helper, mirrors rippled's
    /// `validator-keys` output format).
    fn make_token(manifest: &[u8], secret_key: &[u8]) -> String {
        use base64::Engine;
        let manifest_b64 = base64::engine::general_purpose::STANDARD.encode(manifest);
        let secret_hex = hex::encode(secret_key);
        let inner = format!(
            r#"{{"manifest":"{manifest_b64}","validation_secret_key":"{secret_hex}"}}"#
        );
        base64::engine::general_purpose::STANDARD.encode(inner.as_bytes())
    }

    #[test]
    fn parses_well_formed_token() {
        let manifest = vec![0x01, 0x02, 0x03, 0x04];
        let secret_key = vec![0xaau8; 32];
        let token = make_token(&manifest, &secret_key);

        let parts = parse_validator_token(&token).expect("parse");

        assert_eq!(parts.manifest, manifest);
        assert_eq!(parts.validation_secret_key, secret_key);
    }

    #[test]
    fn tolerates_whitespace_and_line_breaks() {
        let manifest = vec![0xde, 0xad, 0xbe, 0xef];
        let secret_key = vec![0x55u8; 32];
        let token = make_token(&manifest, &secret_key);
        let wrapped = token
            .as_bytes()
            .chunks(20)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("\n  ");

        let parts = parse_validator_token(&wrapped).expect("parse");

        assert_eq!(parts.manifest, manifest);
        assert_eq!(parts.validation_secret_key, secret_key);
    }

    #[test]
    fn rejects_invalid_outer_base64() {
        let err = parse_validator_token("not-base64-!!!").expect_err("must reject");
        assert!(matches!(err, ValidatorTokenError::OuterBase64(_)));
    }

    #[test]
    fn rejects_missing_field() {
        use base64::Engine;
        let inner = r#"{"manifest":"AAAA"}"#;
        let token = base64::engine::general_purpose::STANDARD.encode(inner.as_bytes());

        let err = parse_validator_token(&token).expect_err("must reject");
        assert!(matches!(err, ValidatorTokenError::Json(_)));
    }

    #[test]
    fn rejects_invalid_secret_hex() {
        use base64::Engine;
        let inner =
            r#"{"manifest":"AAAA","validation_secret_key":"zzznothex"}"#;
        let token = base64::engine::general_purpose::STANDARD.encode(inner.as_bytes());

        let err = parse_validator_token(&token).expect_err("must reject");
        assert!(matches!(err, ValidatorTokenError::SecretKeyHex(_)));
    }
}
