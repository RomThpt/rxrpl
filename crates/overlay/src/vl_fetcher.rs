//! Validator List (UNL) HTTP fetcher.
//!
//! Periodically downloads the JSON-formatted UNL from configured publisher
//! sites (e.g. `https://vl.ripple.com/`), verifies the publisher manifest +
//! blob signature via [`crate::validator_list::verify_and_parse`], and
//! republishes the trusted master-key set into a shared [`TrustedKeys`]
//! handle that the [`crate::validation_aggregator::ValidationAggregator`]
//! consults to filter incoming validations.
//!
//! ### Wire format
//!
//! Each site is expected to return a JSON object of the form:
//!
//! ```json
//! {
//!   "public_key": "ED2677ABFFD1B33AC6FBC3062B71F1E8397A1505E1C42C64D11AD1B28FF73F4734",
//!   "manifest":   "<base64>",
//!   "blob":       "<base64>",
//!   "signature":  "<hex>",
//!   "version":    1
//! }
//! ```
//!
//! The `blob` field is a base64-encoded JSON document with a `validators`
//! array of master public keys (and per-validator manifests). The signature
//! covers the *base64-encoded* blob bytes, not the decoded JSON.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use rxrpl_primitives::PublicKey;
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::manifest::ManifestStore;
use crate::validator_list::{self, ValidatorListData, ValidatorListTracker};

/// Default polling interval between successive fetches (5 minutes,
/// matching rippled's `validator-list-fetch-interval`).
pub const DEFAULT_REFRESH: Duration = Duration::from_secs(300);

/// Default per-request HTTP timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum bytes accepted from a single VL response. The mainnet UNL
/// blob is ~30 KB; 1 MiB is generous and stops a malicious or compromised
/// publisher (or DNS hijack) from exhausting our memory by streaming an
/// arbitrarily large body. (Audit finding H1.)
pub const MAX_VL_BODY: u64 = 1024 * 1024;

/// Shared, mutable set of trusted validator master public keys.
///
/// Cloned (cheaply, via [`Arc`]) into the [`crate::validation_aggregator`]
/// so it can filter validations from non-trusted senders.
pub type TrustedKeys = Arc<RwLock<HashSet<PublicKey>>>;

/// Create a new empty [`TrustedKeys`] handle.
pub fn new_trusted_keys() -> TrustedKeys {
    Arc::new(RwLock::new(HashSet::new()))
}

/// Status of the most recent fetch attempt for a single site.
#[derive(Clone, Debug, Default)]
pub struct SiteStatus {
    pub site: String,
    pub last_fetch_unix: Option<u64>,
    pub last_sequence: Option<u64>,
    pub last_validator_count: Option<usize>,
    pub last_error: Option<String>,
}

/// Snapshot of fetcher state for the `validator_list_sites` RPC.
pub type StatusHandle = Arc<RwLock<Vec<SiteStatus>>>;

/// Periodic UNL fetcher.
pub struct VlFetcher {
    sites: Vec<String>,
    trusted_publisher_keys: Vec<PublicKey>,
    trusted_validators: TrustedKeys,
    status: StatusHandle,
    refresh: Duration,
    timeout: Duration,
    http: reqwest::Client,
}

impl VlFetcher {
    /// Create a new fetcher.
    ///
    /// `trusted_publisher_keys` is the set of publisher master keys that the
    /// node is willing to trust as VL signers (e.g. Ripple's
    /// `ED2677ABFFD1B33AC6FBC3062B71F1E8397A1505E1C42C64D11AD1B28FF73F4734`).
    /// VLs signed by any other publisher are rejected.
    pub fn new(
        sites: Vec<String>,
        trusted_publisher_keys: Vec<PublicKey>,
        trusted_validators: TrustedKeys,
        status: StatusHandle,
    ) -> Result<Self, FetcherError> {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .user_agent(concat!("rxrpl/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| FetcherError::Http(e.to_string()))?;
        Ok(Self {
            sites,
            trusted_publisher_keys,
            trusted_validators,
            status,
            refresh: DEFAULT_REFRESH,
            timeout: DEFAULT_TIMEOUT,
            http,
        })
    }

    /// Override the refresh interval (default 5 minutes).
    pub fn with_refresh(mut self, refresh: Duration) -> Self {
        self.refresh = refresh;
        self
    }

    /// Override the per-request timeout (default 10 seconds).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run the fetch loop forever. Designed to be `tokio::spawn`-ed.
    pub async fn run(self) {
        let mut tracker = ValidatorListTracker::new();
        let mut manifest_store = ManifestStore::new();
        for pk in &self.trusted_publisher_keys {
            tracker.add_trusted_publisher(pk);
        }
        // Seed status entries so RPC sees one row per configured site.
        {
            let mut guard = self.status.write().await;
            *guard = self
                .sites
                .iter()
                .cloned()
                .map(|site| SiteStatus {
                    site,
                    ..Default::default()
                })
                .collect();
        }

        // Initial fetch is immediate; subsequent ones are paced by `refresh`.
        let mut interval = tokio::time::interval(self.refresh);
        loop {
            interval.tick().await;
            for (idx, site) in self.sites.iter().enumerate() {
                match self
                    .fetch_one(site, &mut tracker, &mut manifest_store)
                    .await
                {
                    Ok(parsed) => {
                        self.publish(&parsed).await;
                        self.record_status(idx, Some(parsed), None).await;
                    }
                    Err(e) => {
                        tracing::warn!("VL fetch from {site} failed: {e}");
                        self.record_status(idx, None, Some(e.to_string())).await;
                    }
                }
            }
        }
    }

    /// Perform a single fetch + verify cycle for one site.
    async fn fetch_one(
        &self,
        site: &str,
        tracker: &mut ValidatorListTracker,
        manifest_store: &mut ManifestStore,
    ) -> Result<ValidatorListData, FetcherError> {
        let resp = self
            .http
            .get(site)
            .send()
            .await
            .map_err(|e| FetcherError::Http(e.to_string()))?
            .error_for_status()
            .map_err(|e| FetcherError::Http(e.to_string()))?;
        // Reject responses that advertise a body larger than our cap before
        // we touch the bytes, in case the publisher (or a MITM) tries to
        // exhaust memory with a giant Content-Length.
        if let Some(len) = resp.content_length() {
            if len > MAX_VL_BODY {
                return Err(FetcherError::Http(format!(
                    "VL body {len} bytes exceeds cap {MAX_VL_BODY}"
                )));
            }
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| FetcherError::Http(format!("read body: {e}")))?;
        if bytes.len() as u64 > MAX_VL_BODY {
            return Err(FetcherError::Http(format!(
                "VL body {} bytes exceeds cap {MAX_VL_BODY}",
                bytes.len()
            )));
        }
        let payload: VlPayload = serde_json::from_slice(&bytes)
            .map_err(|e| FetcherError::Http(format!("decode JSON: {e}")))?;

        let manifest_bytes = base64_decode(&payload.manifest)
            .map_err(|e| FetcherError::Decode(format!("manifest base64: {e}")))?;

        let parsed = validator_list::verify_and_parse(
            &manifest_bytes,
            payload.blob.as_bytes(),
            payload.signature.as_bytes(),
            manifest_store,
        )
        .map_err(|e| FetcherError::Verify(e.to_string()))?;

        if !tracker.is_trusted_publisher(&parsed.publisher_master_key) {
            return Err(FetcherError::UntrustedPublisher);
        }
        if !tracker.record_sequence(&parsed.publisher_master_key, parsed.sequence) {
            return Err(FetcherError::StaleSequence(parsed.sequence));
        }
        Ok(parsed)
    }

    async fn publish(&self, parsed: &ValidatorListData) {
        let mut guard = self.trusted_validators.write().await;
        guard.clear();
        for pk in &parsed.validators {
            guard.insert(pk.clone());
        }
        tracing::info!(
            "trusted validator set updated: publisher={} sequence={} validators={}",
            hex::encode(parsed.publisher_master_key.as_bytes()),
            parsed.sequence,
            parsed.validators.len(),
        );
    }

    async fn record_status(
        &self,
        idx: usize,
        parsed: Option<ValidatorListData>,
        err: Option<String>,
    ) {
        let mut guard = self.status.write().await;
        if let Some(slot) = guard.get_mut(idx) {
            slot.last_fetch_unix = Some(now_unix());
            if let Some(p) = parsed {
                slot.last_sequence = Some(p.sequence);
                slot.last_validator_count = Some(p.validators.len());
                slot.last_error = None;
            } else {
                slot.last_error = err;
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct VlPayload {
    blob: String,
    manifest: String,
    signature: String,
    #[serde(default)]
    #[allow(dead_code)]
    version: u32,
}

/// Errors produced by [`VlFetcher`].
#[derive(Debug, thiserror::Error)]
pub enum FetcherError {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("verification failed: {0}")]
    Verify(String),
    #[error("publisher key not in configured trust list")]
    UntrustedPublisher,
    #[error("stale VL sequence ({0})")]
    StaleSequence(u64),
}

fn base64_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s)
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

    #[test]
    fn site_status_default() {
        let s = SiteStatus {
            site: "https://vl.ripple.com/".into(),
            ..Default::default()
        };
        assert!(s.last_fetch_unix.is_none());
        assert!(s.last_error.is_none());
    }

    #[tokio::test]
    async fn trusted_keys_handle_is_shared() {
        let tk = new_trusted_keys();
        let tk_clone = Arc::clone(&tk);
        {
            let mut guard = tk.write().await;
            guard.insert(PublicKey::from_slice(&[0xED; 33]).unwrap());
        }
        assert_eq!(tk_clone.read().await.len(), 1);
    }
}
