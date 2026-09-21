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
//! Version 2 replaces `blob` and `signature` with a `blobs-v2` array. Every
//! entry carries a separately signed blob and can optionally carry the
//! publisher manifest that authorized its signature. The `effective` and
//! `expiration` fields remain inside each signed, decoded blob in Ripple time.
//!
//! In either version, a blob is base64-encoded JSON containing a `validators`
//! array of master public keys (and per-validator manifests), and its signature
//! covers the decoded JSON bytes rather than the Base64 transport encoding.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use rxrpl_ledger::header::RIPPLE_EPOCH_OFFSET;
use rxrpl_primitives::PublicKey;
use serde::Deserialize;
use tokio::sync::{RwLock, mpsc};

use crate::command::OverlayCommand;
use crate::manifest::ManifestStore;
use crate::peer_manager::ConsensusMessage;
use crate::validator_list::{self, BlobV2Wire, ValidatorListData, ValidatorListTracker};

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
    /// When set, each verified VL is forwarded to the consensus loop as a
    /// `ValidatorListVerified` message so the engine's UNL/quorum are updated
    /// from an HTTP-fetched list -- the same effect the P2P `TMValidatorList`
    /// path has. Without this, an HTTP-only dynamic VL populates the
    /// aggregator trust filter but leaves the consensus engine in solo mode.
    consensus_tx: Option<mpsc::Sender<ConsensusMessage>>,
    overlay_command_tx: Option<mpsc::UnboundedSender<OverlayCommand>>,
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
            consensus_tx: None,
            overlay_command_tx: None,
        })
    }

    /// Forward every verified VL to the consensus loop (as
    /// `ConsensusMessage::ValidatorListVerified`) so the engine UNL + quorum
    /// track an HTTP-fetched dynamic list, not just the aggregator trust filter.
    pub fn with_consensus_sender(mut self, tx: mpsc::Sender<ConsensusMessage>) -> Self {
        self.consensus_tx = Some(tx);
        self
    }

    /// Apply validator manifests from each verified list through the overlay
    /// owner of the manifest store. This binds validators' ephemeral signing
    /// keys to their UNL-trusted master keys before validations are counted.
    pub fn with_overlay_command_sender(
        mut self,
        tx: mpsc::UnboundedSender<OverlayCommand>,
    ) -> Self {
        self.overlay_command_tx = Some(tx);
        self
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

        let parsed = payload.verify(manifest_store, now_ripple())?;

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
        drop(guard);

        if !parsed.validator_manifests.is_empty() {
            if let Some(tx) = &self.overlay_command_tx {
                if tx
                    .send(OverlayCommand::ApplyValidatorListManifests {
                        manifests: parsed.validator_manifests.clone(),
                    })
                    .is_err()
                {
                    tracing::warn!("could not apply validator-list manifests to overlay");
                }
            }
        }

        // Feed the verified list into the consensus engine (UNL + quorum +
        // validations-trie), mirroring the P2P `ValidatorListVerified` path.
        if let Some(tx) = &self.consensus_tx {
            let msg = ConsensusMessage::ValidatorListVerified {
                validators: parsed.validators.clone(),
                sequence: parsed.sequence,
            };
            if let Err(e) = tx.try_send(msg) {
                tracing::warn!("could not forward verified VL to consensus: {e}");
            }
        }
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
    manifest: String,
    #[serde(default)]
    blob: Option<String>,
    #[serde(default)]
    signature: Option<String>,
    version: u8,
    #[serde(rename = "blobs-v2", default)]
    blobs_v2: Option<Vec<VlPayloadV2Blob>>,
}

#[derive(Debug, Deserialize)]
struct VlPayloadV2Blob {
    #[serde(default)]
    manifest: Option<String>,
    blob: String,
    signature: String,
}

impl VlPayload {
    fn verify(
        self,
        manifest_store: &mut ManifestStore,
        now_ripple: u64,
    ) -> Result<ValidatorListData, FetcherError> {
        let manifest_bytes = decode_manifest(&self.manifest)?;
        match self.version {
            1 => {
                if self.blobs_v2.is_some() {
                    return Err(FetcherError::MalformedPayload(
                        "v1 payload must not contain blobs-v2",
                    ));
                }
                let blob = self
                    .blob
                    .ok_or(FetcherError::MalformedPayload("v1 payload missing blob"))?;
                let signature = self.signature.ok_or(FetcherError::MalformedPayload(
                    "v1 payload missing signature",
                ))?;
                validator_list::verify_and_parse(
                    &manifest_bytes,
                    blob.as_bytes(),
                    signature.as_bytes(),
                    manifest_store,
                )
                .map_err(|e| FetcherError::Verify(e.to_string()))
            }
            2 => {
                if self.blob.is_some() || self.signature.is_some() {
                    return Err(FetcherError::MalformedPayload(
                        "v2 payload must not contain v1 blob or signature",
                    ));
                }
                let blobs_v2 = self.blobs_v2.ok_or(FetcherError::MalformedPayload(
                    "v2 payload missing blobs-v2",
                ))?;
                if blobs_v2.is_empty() {
                    return Err(FetcherError::MalformedPayload("v2 payload has no blobs"));
                }
                let blobs_v2 = blobs_v2
                    .into_iter()
                    .map(|entry| {
                        entry
                            .manifest
                            .map(|manifest| decode_manifest(&manifest))
                            .transpose()
                            .map(|manifest| BlobV2Wire {
                                manifest,
                                blob_base64: entry.blob.into_bytes(),
                                signature_hex: entry.signature.into_bytes(),
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let bundle = validator_list::verify_and_parse_v2(
                    &manifest_bytes,
                    &blobs_v2,
                    manifest_store,
                    now_ripple,
                )
                .map_err(|e| FetcherError::Verify(e.to_string()))?;
                bundle
                    .active
                    .into_iter()
                    .max_by_key(|list| list.base.sequence)
                    .map(|list| list.base)
                    .ok_or(FetcherError::NoActiveV2Blob)
            }
            version => Err(FetcherError::UnsupportedVersion(version)),
        }
    }
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
    #[error("unsupported validator list version ({0})")]
    UnsupportedVersion(u8),
    #[error("malformed validator list payload: {0}")]
    MalformedPayload(&'static str),
    #[error("no validator list v2 blob is currently effective")]
    NoActiveV2Blob,
}

fn decode_manifest(s: &str) -> Result<Vec<u8>, FetcherError> {
    if s.len() % 2 == 0 && s.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return hex::decode(s).map_err(|e| FetcherError::Decode(format!("manifest hex: {e}")));
    }
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| FetcherError::Decode(format!("manifest base64: {e}")))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_ripple() -> u64 {
    now_unix().saturating_sub(RIPPLE_EPOCH_OFFSET)
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

    fn test_pubkey(byte1: u8) -> PublicKey {
        let mut b = [0xED; 33];
        b[1] = byte1;
        PublicKey::from_slice(&b).unwrap()
    }

    fn signed_v2_payload(entries: &[(u64, u64, u64)]) -> String {
        use base64::Engine;

        let publisher = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("http_v2_publisher"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let signing = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("http_v2_signing"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let signing_data = crate::manifest::build_signing_data(
            1,
            publisher.public_key.as_bytes(),
            signing.public_key.as_bytes(),
            None,
        );
        let ephemeral_signature =
            rxrpl_crypto::ed25519::sign(&signing_data, &signing.private_key).unwrap();
        let master_signature =
            rxrpl_crypto::ed25519::sign(&signing_data, &publisher.private_key).unwrap();
        let manifest = crate::manifest::build_manifest_bytes(
            1,
            publisher.public_key.as_bytes(),
            signing.public_key.as_bytes(),
            ephemeral_signature.as_bytes(),
            master_signature.as_bytes(),
            None,
        );
        let blobs: Vec<serde_json::Value> = entries
            .iter()
            .map(|(effective, expiration, sequence)| {
                let blob = serde_json::json!({
                    "effective": effective,
                    "expiration": expiration,
                    "sequence": sequence,
                    "validators": [],
                });
                let blob = serde_json::to_vec(&blob).unwrap();
                let signature = rxrpl_crypto::ed25519::sign(&blob, &signing.private_key).unwrap();
                serde_json::json!({
                    "blob": base64::engine::general_purpose::STANDARD.encode(blob),
                    "signature": hex::encode(signature.as_bytes()),
                })
            })
            .collect();
        serde_json::json!({
            "version": 2,
            "manifest": base64::engine::general_purpose::STANDARD.encode(manifest),
            "blobs-v2": blobs,
        })
        .to_string()
    }

    #[test]
    fn v2_payload_selects_highest_active_sequence() {
        let payload: VlPayload = serde_json::from_str(&signed_v2_payload(&[
            (0, 200, 3),
            (0, 200, 7),
            (200, 400, 9),
        ]))
        .unwrap();
        let mut store = ManifestStore::new();

        let parsed = payload.verify(&mut store, 100).unwrap();
        assert_eq!(parsed.sequence, 7);
    }

    #[test]
    fn v2_payload_rejects_when_no_blob_is_active() {
        let payload: VlPayload =
            serde_json::from_str(&signed_v2_payload(&[(200, 400, 9)])).unwrap();
        let mut store = ManifestStore::new();

        assert!(matches!(
            payload.verify(&mut store, 100),
            Err(FetcherError::NoActiveV2Blob)
        ));
    }

    #[test]
    fn v2_payload_accepts_a_base64_entry_manifest() {
        let mut payload: serde_json::Value =
            serde_json::from_str(&signed_v2_payload(&[(0, 200, 7)])).unwrap();
        payload["blobs-v2"][0]["manifest"] = payload["manifest"].clone();
        let payload: VlPayload = serde_json::from_value(payload).unwrap();
        let mut store = ManifestStore::new();

        assert_eq!(payload.verify(&mut store, 100).unwrap().sequence, 7);
    }

    #[tokio::test]
    async fn forwards_verified_vl_to_consensus() {
        // A VlFetcher with a consensus sender must forward each verified VL to
        // the consensus loop as ValidatorListVerified, so an HTTP-fetched
        // dynamic list drives the engine UNL/quorum (not just the aggregator
        // trust filter -- otherwise consensus stays in solo mode).
        let (tx, mut rx) = mpsc::channel(8);
        let publisher = test_pubkey(0);
        let fetcher = VlFetcher::new(
            vec!["http://localhost/vl".into()],
            vec![publisher.clone()],
            new_trusted_keys(),
            Arc::new(RwLock::new(Vec::new())),
        )
        .unwrap()
        .with_consensus_sender(tx);

        let masters = vec![test_pubkey(1), test_pubkey(2), test_pubkey(3)];
        let parsed = ValidatorListData {
            sequence: 7,
            expiration: 999,
            validators: masters.clone(),
            validator_manifests: vec![],
            publisher_master_key: publisher,
        };
        fetcher.publish(&parsed).await;

        let msg = rx.try_recv().expect("verified VL forwarded to consensus");
        if let ConsensusMessage::ValidatorListVerified {
            validators,
            sequence,
        } = msg
        {
            assert_eq!(sequence, 7);
            assert_eq!(validators, masters);
        } else {
            panic!("expected ValidatorListVerified");
        }

        // With no consensus sender configured, publish is a no-op (does not
        // panic, emits nothing).
        let fetcher2 = VlFetcher::new(
            vec!["http://localhost/vl".into()],
            vec![test_pubkey(0)],
            new_trusted_keys(),
            Arc::new(RwLock::new(Vec::new())),
        )
        .unwrap();
        fetcher2.publish(&parsed).await;
    }

    #[tokio::test]
    async fn forwards_validator_manifests_to_overlay() {
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let publisher = test_pubkey(0);
        let fetcher = VlFetcher::new(
            vec!["http://localhost/vl".into()],
            vec![publisher.clone()],
            new_trusted_keys(),
            Arc::new(RwLock::new(Vec::new())),
        )
        .unwrap()
        .with_overlay_command_sender(command_tx);
        let manifests = vec![vec![1, 2, 3], vec![4, 5, 6]];
        let parsed = ValidatorListData {
            sequence: 7,
            expiration: 999,
            validators: vec![test_pubkey(1)],
            validator_manifests: manifests.clone(),
            publisher_master_key: publisher,
        };

        fetcher.publish(&parsed).await;

        assert!(matches!(
            command_rx.try_recv(),
            Ok(OverlayCommand::ApplyValidatorListManifests { manifests: received })
                if received == manifests
        ));
    }
}
