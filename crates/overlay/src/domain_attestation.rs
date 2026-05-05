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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rxrpl_primitives::PublicKey;
use serde::Deserialize;
use tokio::sync::RwLock;

/// Default per-request HTTP timeout for `xrp-ledger.toml` fetches.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum bytes accepted from a single TOML response.
pub const MAX_BODY: u64 = 64 * 1024;

/// Default cache TTL for a successful attestation (24 hours).
pub const DEFAULT_TTL: Duration = Duration::from_secs(24 * 3600);

/// Default refresh interval for the background loop (5 minutes).
pub const DEFAULT_REFRESH: Duration = Duration::from_secs(300);

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

// ---------------------------------------------------------------------
// B2: cache + TTL + retry backoff
// ---------------------------------------------------------------------

/// Status of a single validator's domain attestation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttestationStatus {
    /// No attempt has been made yet.
    NotAttempted,
    /// A fetch is in flight or scheduled but no result yet.
    Pending,
    /// Successfully verified at the given Unix timestamp.
    Verified { at: u64 },
    /// Most recent attempt failed with the recorded error message.
    Failed { last_error: String, fail_count: u32 },
    /// Previously verified but TTL has lapsed.
    Expired,
}

impl AttestationStatus {
    /// Short string for RPC exposure.
    pub fn label(&self) -> &'static str {
        match self {
            AttestationStatus::NotAttempted => "not_attempted",
            AttestationStatus::Pending => "pending",
            AttestationStatus::Verified { .. } => "verified",
            AttestationStatus::Failed { .. } => "failed",
            AttestationStatus::Expired => "expired",
        }
    }
}

/// Cache entry for a single validator.
#[derive(Clone, Debug)]
pub struct AttestationEntry {
    pub domain: String,
    pub status: AttestationStatus,
    pub last_checked_unix: u64,
}

/// Per-validator domain attestation cache.
#[derive(Debug, Default)]
pub struct AttestationCache {
    entries: HashMap<PublicKey, AttestationEntry>,
    ttl: Duration,
}

impl AttestationCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            ttl: DEFAULT_TTL,
        }
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
        }
    }

    /// Return the live status, applying TTL expiry to past `Verified`
    /// entries. Returns `Pending` if no entry exists yet for this key /
    /// domain pair (caller is expected to schedule a refresh).
    pub fn get_or_refresh(
        &self,
        key: &PublicKey,
        domain: &str,
        now: u64,
    ) -> AttestationStatus {
        match self.entries.get(key) {
            Some(entry) if entry.domain == domain => {
                if let AttestationStatus::Verified { at } = entry.status {
                    if now.saturating_sub(at) > self.ttl.as_secs() {
                        return AttestationStatus::Expired;
                    }
                }
                entry.status.clone()
            }
            _ => AttestationStatus::Pending,
        }
    }

    /// Record a fresh verification result.
    pub fn record_result(
        &mut self,
        key: &PublicKey,
        domain: &str,
        verified: bool,
        now: u64,
    ) {
        let prior_fail_count = match self.entries.get(key).map(|e| &e.status) {
            Some(AttestationStatus::Failed { fail_count, .. }) => *fail_count,
            _ => 0,
        };
        let status = if verified {
            AttestationStatus::Verified { at: now }
        } else {
            AttestationStatus::Failed {
                last_error: "validator key not listed in xrp-ledger.toml".into(),
                fail_count: prior_fail_count.saturating_add(1),
            }
        };
        self.entries.insert(
            key.clone(),
            AttestationEntry {
                domain: domain.to_string(),
                status,
                last_checked_unix: now,
            },
        );
    }

    /// Record an error result (HTTP, parse, timeout, ...).
    pub fn record_error(&mut self, key: &PublicKey, domain: &str, err: &str, now: u64) {
        let prior_fail_count = match self.entries.get(key).map(|e| &e.status) {
            Some(AttestationStatus::Failed { fail_count, .. }) => *fail_count,
            _ => 0,
        };
        self.entries.insert(
            key.clone(),
            AttestationEntry {
                domain: domain.to_string(),
                status: AttestationStatus::Failed {
                    last_error: err.to_string(),
                    fail_count: prior_fail_count.saturating_add(1),
                },
                last_checked_unix: now,
            },
        );
    }

    /// Return how many seconds the caller should wait before retrying
    /// `key` after a failure: 60 s after the first, 300 s after the
    /// second, 3600 s after the third+ (capped). For non-failure entries
    /// returns 0 (caller should respect the loop refresh interval).
    pub fn retry_backoff_secs(&self, key: &PublicKey) -> u64 {
        match self.entries.get(key).map(|e| &e.status) {
            Some(AttestationStatus::Failed { fail_count, .. }) => match fail_count {
                0 | 1 => 60,
                2 => 300,
                _ => 3600,
            },
            _ => 0,
        }
    }

    /// Last-checked timestamp recorded for this key, if any.
    pub fn last_checked(&self, key: &PublicKey) -> Option<u64> {
        self.entries.get(key).map(|e| e.last_checked_unix)
    }

    /// Return a snapshot of all known entries (for RPC).
    pub fn snapshot(&self, now: u64) -> Vec<(PublicKey, AttestationEntry)> {
        self.entries
            .iter()
            .map(|(k, v)| {
                let mut entry = v.clone();
                if let AttestationStatus::Verified { at } = entry.status {
                    if now.saturating_sub(at) > self.ttl.as_secs() {
                        entry.status = AttestationStatus::Expired;
                    }
                }
                (k.clone(), entry)
            })
            .collect()
    }
}

/// Shared handle to the attestation cache, cloned cheaply into RPC
/// handlers and the background service.
pub type CacheHandle = Arc<RwLock<AttestationCache>>;

/// Build a fresh shared cache handle.
pub fn new_cache() -> CacheHandle {
    Arc::new(RwLock::new(AttestationCache::new()))
}

// ---------------------------------------------------------------------
// B3: background attestation service
// ---------------------------------------------------------------------

/// One validator the background service should attest periodically.
#[derive(Clone, Debug)]
pub struct AttestationTarget {
    pub master_key: PublicKey,
    pub domain: String,
}

/// Periodic attestation service. Designed to run in its own
/// `tokio::spawn` task so HTTP latency cannot stall consensus.
pub struct DomainAttestationService {
    targets: Vec<AttestationTarget>,
    cache: CacheHandle,
    fetcher: Arc<DomainAttestationFetcher>,
    refresh: Duration,
}

impl DomainAttestationService {
    pub fn new(
        targets: Vec<AttestationTarget>,
        cache: CacheHandle,
        fetcher: DomainAttestationFetcher,
    ) -> Self {
        Self {
            targets,
            cache,
            fetcher: Arc::new(fetcher),
            refresh: DEFAULT_REFRESH,
        }
    }

    pub fn with_refresh(mut self, refresh: Duration) -> Self {
        self.refresh = refresh;
        self
    }

    /// Drive a single refresh cycle. Honors per-target retry backoff.
    pub async fn refresh_once(&self) {
        let now = now_unix();
        for target in &self.targets {
            let (last_checked, backoff) = {
                let guard = self.cache.read().await;
                (
                    guard.last_checked(&target.master_key).unwrap_or(0),
                    guard.retry_backoff_secs(&target.master_key),
                )
            };
            if backoff > 0 && now.saturating_sub(last_checked) < backoff {
                continue;
            }

            match self
                .fetcher
                .fetch_and_verify(&target.domain, &target.master_key)
                .await
            {
                Ok(verified) => {
                    let mut guard = self.cache.write().await;
                    guard.record_result(&target.master_key, &target.domain, verified, now);
                    if verified {
                        tracing::info!(
                            "Domain attestation verified for {}",
                            target.domain
                        );
                    } else {
                        tracing::warn!(
                            "Domain attestation failed for {}: key not listed",
                            target.domain
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Domain attestation failed for {}: {e}",
                        target.domain
                    );
                    let mut guard = self.cache.write().await;
                    guard.record_error(
                        &target.master_key,
                        &target.domain,
                        &e.to_string(),
                        now,
                    );
                }
            }
        }
    }

    /// Spawn the periodic loop. Call this once at boot.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.refresh);
            loop {
                interval.tick().await;
                self.refresh_once().await;
            }
        })
    }
}

/// Render the cache + optional local-validator entry into the JSON shape
/// consumed by `rpc-server` (`server_info` / `validators`).
///
/// `local` is the public key of the running validator (when in
/// validator mode), so the corresponding cache entry is duplicated under
/// `"local"` for `server_info`. All other entries land under
/// `"validators"`.
pub fn render_status_json(
    cache: &AttestationCache,
    local: Option<&PublicKey>,
    now: u64,
) -> serde_json::Value {
    let mut local_obj = serde_json::json!({});
    let mut validators = Vec::new();
    for (key, entry) in cache.snapshot(now) {
        let pk_hex = hex::encode_upper(key.as_bytes());
        let status_label = entry.status.label();
        let verified = matches!(entry.status, AttestationStatus::Verified { .. });
        let last_verified = match entry.status {
            AttestationStatus::Verified { at } => at,
            _ => entry.last_checked_unix,
        };
        let v = serde_json::json!({
            "public_key": pk_hex,
            "domain": entry.domain,
            "verification_status": status_label,
            "last_verified": last_verified,
        });
        if local == Some(&key) {
            local_obj = serde_json::json!({
                "verified": verified,
                "domain": entry.domain,
                "status": status_label,
                "last_check": entry.last_checked_unix,
            });
        } else {
            validators.push(v);
        }
    }
    serde_json::json!({ "local": local_obj, "validators": validators })
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

    #[test]
    fn cache_returns_pending_for_unknown_key() {
        let cache = AttestationCache::new();
        let key = ed_key(0x01);
        assert_eq!(
            cache.get_or_refresh(&key, "example.com", 1000),
            AttestationStatus::Pending
        );
    }

    #[test]
    fn cache_records_verification_then_expires() {
        let mut cache = AttestationCache::new();
        let key = ed_key(0x01);
        cache.record_result(&key, "example.com", true, 1000);
        assert!(matches!(
            cache.get_or_refresh(&key, "example.com", 1000),
            AttestationStatus::Verified { at: 1000 }
        ));
        assert!(matches!(
            cache.get_or_refresh(&key, "example.com", 1000 + 24 * 3600),
            AttestationStatus::Verified { .. }
        ));
        assert_eq!(
            cache.get_or_refresh(&key, "example.com", 1000 + 24 * 3600 + 1),
            AttestationStatus::Expired
        );
    }

    #[test]
    fn cache_failure_increments_backoff() {
        let mut cache = AttestationCache::new();
        let key = ed_key(0x02);
        cache.record_error(&key, "bad.example", "404", 100);
        assert_eq!(cache.retry_backoff_secs(&key), 60);
        cache.record_error(&key, "bad.example", "404", 200);
        assert_eq!(cache.retry_backoff_secs(&key), 300);
        cache.record_error(&key, "bad.example", "404", 700);
        assert_eq!(cache.retry_backoff_secs(&key), 3600);
    }

    #[test]
    fn cache_domain_change_treated_as_pending() {
        let mut cache = AttestationCache::new();
        let key = ed_key(0x03);
        cache.record_result(&key, "old.example", true, 100);
        assert_eq!(
            cache.get_or_refresh(&key, "new.example", 200),
            AttestationStatus::Pending
        );
    }

    #[test]
    fn status_labels_are_stable() {
        assert_eq!(AttestationStatus::NotAttempted.label(), "not_attempted");
        assert_eq!(AttestationStatus::Pending.label(), "pending");
        assert_eq!(AttestationStatus::Expired.label(), "expired");
        assert_eq!(AttestationStatus::Verified { at: 1 }.label(), "verified");
        assert_eq!(
            AttestationStatus::Failed {
                last_error: "x".into(),
                fail_count: 1
            }
            .label(),
            "failed"
        );
    }

    #[tokio::test]
    async fn cache_handle_is_shareable() {
        let h = new_cache();
        let h2 = h.clone();
        {
            let mut g = h.write().await;
            g.record_result(&ed_key(0x05), "example.com", true, 1);
        }
        assert_eq!(h2.read().await.snapshot(1).len(), 1);
    }

    #[test]
    fn render_status_json_splits_local_and_others() {
        let mut cache = AttestationCache::new();
        let local = ed_key(0x01);
        let other = ed_key(0x02);
        cache.record_result(&local, "self.example", true, 1000);
        cache.record_result(&other, "peer.example", false, 1000);
        let v = render_status_json(&cache, Some(&local), 1500);
        assert_eq!(v["local"]["status"], "verified");
        assert_eq!(v["local"]["domain"], "self.example");
        let arr = v["validators"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["domain"], "peer.example");
        assert_eq!(arr[0]["verification_status"], "failed");
    }

    #[test]
    fn render_status_json_no_local() {
        let cache = AttestationCache::new();
        let v = render_status_json(&cache, None, 0);
        assert_eq!(v["validators"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn service_refresh_once_writes_cache() {
        let key = ed_key(0x10);
        let server = httpmock::MockServer::start_async().await;
        let hex = hex::encode_upper(key.as_bytes());
        let _m = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/.well-known/xrp-ledger.toml");
                then.status(200).body(format!(
                    "[[VALIDATORS]]\npublic_key = \"{hex}\"\n"
                ));
            })
            .await;

        let cache = new_cache();
        let fetcher =
            DomainAttestationFetcher::with_base_url_for_tests(server.base_url()).unwrap();
        let svc = DomainAttestationService::new(
            vec![AttestationTarget {
                master_key: key.clone(),
                domain: "example.com".to_string(),
            }],
            cache.clone(),
            fetcher,
        );
        svc.refresh_once().await;
        let snap = cache.read().await.snapshot(now_unix());
        assert_eq!(snap.len(), 1);
        assert!(matches!(snap[0].1.status, AttestationStatus::Verified { .. }));
    }
}
