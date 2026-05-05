/// Validator list (VL) signature verification and parsing.
///
/// A ValidatorList message carries:
///   - `manifest`: the list publisher's manifest (serialized STObject)
///   - `blob`: base64-encoded JSON containing the validator entries
///   - `signature`: hex-encoded signature over the raw blob bytes
///   - `version`: protocol version
///
/// The signature is computed by the publisher's *ephemeral* key over the
/// raw blob bytes (before base64-decoding).  To verify:
///   1. Parse the publisher's manifest to extract the ephemeral key
///   2. Verify the blob signature against that ephemeral key
///   3. Base64-decode and parse the blob JSON for validator entries
///
/// Each validator entry in the blob has:
///   - `validation_public_key`: hex master public key
///   - `manifest`: base64-encoded individual validator manifest
use rxrpl_primitives::PublicKey;

use crate::manifest::{self, MANIFEST_REVOKED_SEQ, ManifestStore};

/// A parsed and verified validator list.
#[derive(Clone, Debug)]
pub struct ValidatorListData {
    /// Sequence number of this list (higher = newer).
    pub sequence: u64,
    /// Expiration time (seconds since ripple epoch).
    pub expiration: u64,
    /// Validator master public keys from the blob.
    pub validators: Vec<PublicKey>,
    /// Raw validator manifests from the blob (base64-decoded).
    pub validator_manifests: Vec<Vec<u8>>,
    /// The publisher's master public key (from their manifest).
    pub publisher_master_key: PublicKey,
}

/// Errors from VL verification.
#[derive(Debug, thiserror::Error)]
pub enum ValidatorListError {
    #[error("publisher manifest invalid: {0}")]
    PublisherManifest(#[from] manifest::ManifestError),
    #[error("publisher key revoked")]
    PublisherRevoked,
    #[error("blob signature invalid")]
    BlobSignatureInvalid,
    #[error("blob decode failed: {0}")]
    BlobDecode(String),
    #[error("missing blob or signature")]
    MissingData,
    #[error("stale validator list (seq {got} <= {have})")]
    StaleSequence { got: u64, have: u64 },
    #[error("publisher master key not registered")]
    UnknownPublisher,
    #[error("rotation master signature invalid")]
    RotationSignatureInvalid,
    #[error("unsupported VL version: {0}")]
    UnsupportedVersion(u64),
    #[error("no v2 blob is currently within its effective window")]
    NoEffectiveBlob,
    #[error("cascade depth limit exceeded (limit={0})")]
    CascadeDepthExceeded(usize),
    #[error("cyclic delegate detected")]
    CyclicDelegate,
    #[error("delegate fetch failed: {0}")]
    DelegateFetchFailed(String),
    #[error("delegate publisher revoked")]
    DelegateRevoked,
    #[error("cascade signature invalid")]
    CascadeSignatureInvalid,
}

/// A v2 VL data record: a v1-style ValidatorListData enriched with the
/// effective time window (`effective_start..effective_expiration`) under
/// which this blob is considered authoritative, and any optional delegate
/// publishers referenced by this blob (cascade trust, see B3).
#[derive(Clone, Debug)]
pub struct ValidatorListDataV2 {
    pub base: ValidatorListData,
    /// Window start (unix seconds) inclusive.
    pub effective_start: u64,
    /// Window end (unix seconds) exclusive.
    pub effective_expiration: u64,
    /// Optional delegate publisher master keys referenced by this blob.
    /// Used by the cascade resolver in B3.
    pub delegates: Vec<PublicKey>,
}

/// Result of a v2 parse: the (already filtered) blobs whose window
/// contains `now`, and the list of blobs that were rejected because their
/// window does not contain `now` (kept for diagnostics / "stale-UNL"
/// fallback decisions in the consensus layer).
#[derive(Clone, Debug, Default)]
pub struct ValidatorListV2Bundle {
    pub active: Vec<ValidatorListDataV2>,
    pub inactive: Vec<ValidatorListDataV2>,
}

/// Verify and parse a ValidatorList message.
///
/// Arguments:
///   - `publisher_manifest_bytes`: raw STObject manifest of the VL publisher
///   - `blob_base64`: the base64-encoded blob (as raw bytes, not yet decoded)
///   - `signature_hex`: hex-encoded signature over the blob bytes
///   - `manifest_store`: used to check publisher status and extract ephemeral key
///
/// Returns the parsed validator list on success.
pub fn verify_and_parse(
    publisher_manifest_bytes: &[u8],
    blob_base64: &[u8],
    signature_hex: &[u8],
    manifest_store: &mut ManifestStore,
) -> Result<ValidatorListData, ValidatorListError> {
    // Parse and verify the publisher's manifest
    let publisher_manifest = manifest::parse_and_verify(publisher_manifest_bytes)?;

    // Check if publisher is revoked
    if manifest_store.is_revoked(&publisher_manifest.master_public_key) {
        return Err(ValidatorListError::PublisherRevoked);
    }

    // Apply the publisher manifest to the store
    manifest_store.apply(publisher_manifest.clone());

    // Get the publisher's current ephemeral key
    let ephemeral_pk = publisher_manifest
        .ephemeral_public_key
        .as_ref()
        .ok_or(ValidatorListError::MissingData)?;

    // Verify the signature over the blob bytes
    let sig_bytes = hex::decode(signature_hex)
        .map_err(|e| ValidatorListError::BlobDecode(format!("signature hex: {}", e)))?;

    let verified = verify_blob_signature(blob_base64, ephemeral_pk.as_bytes(), &sig_bytes);
    if !verified {
        return Err(ValidatorListError::BlobSignatureInvalid);
    }

    // Base64-decode the blob
    use base64::Engine;
    let blob_json = base64::engine::general_purpose::STANDARD
        .decode(blob_base64)
        .map_err(|e| ValidatorListError::BlobDecode(format!("base64: {}", e)))?;

    // Parse the blob JSON
    let blob: serde_json::Value = serde_json::from_slice(&blob_json)
        .map_err(|e| ValidatorListError::BlobDecode(format!("json: {}", e)))?;

    let sequence = blob.get("sequence").and_then(|v| v.as_u64()).unwrap_or(0);

    let expiration = blob.get("expiration").and_then(|v| v.as_u64()).unwrap_or(0);

    let validators_arr = blob
        .get("validators")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ValidatorListError::BlobDecode("missing validators array".into()))?;

    let mut validators = Vec::with_capacity(validators_arr.len());
    let mut validator_manifests = Vec::new();

    for entry in validators_arr {
        // Extract master public key
        if let Some(pk_hex) = entry.get("validation_public_key").and_then(|v| v.as_str()) {
            if let Ok(pk_bytes) = hex::decode(pk_hex) {
                if let Ok(pk) = PublicKey::from_slice(&pk_bytes) {
                    validators.push(pk);
                }
            }
        }

        // Extract individual validator manifest
        if let Some(manifest_b64) = entry.get("manifest").and_then(|v| v.as_str()) {
            if let Ok(manifest_bytes) =
                base64::engine::general_purpose::STANDARD.decode(manifest_b64)
            {
                validator_manifests.push(manifest_bytes);
            }
        }
    }

    Ok(ValidatorListData {
        sequence,
        expiration,
        validators,
        validator_manifests,
        publisher_master_key: publisher_manifest.master_public_key,
    })
}

/// Verify and parse a v2 ValidatorList payload.
///
/// The v2 wire format is:
///
/// ```json
/// {
///   "version": 2,
///   "manifest": "<base64>",
///   "blobs_v2": [
///     { "effective_start": u64, "effective_expiration": u64,
///       "blob": "<base64>", "signature": "<hex>" },
///     ...
///   ]
/// }
/// ```
///
/// Each blob signature is verified against the publisher's ephemeral key
/// (extracted from the manifest, exactly as in v1). Blobs whose window
/// does not contain `now_unix` are returned in `inactive`; in-window
/// blobs are returned in `active`.
///
/// `now_unix` is taken as a parameter (not pulled from the system clock)
/// so consensus and tests can drive deterministic time.
pub fn verify_and_parse_v2(
    publisher_manifest_bytes: &[u8],
    blobs_v2: &[BlobV2Wire],
    manifest_store: &mut ManifestStore,
    now_unix: u64,
) -> Result<ValidatorListV2Bundle, ValidatorListError> {
    let publisher_manifest = manifest::parse_and_verify(publisher_manifest_bytes)?;
    if manifest_store.is_revoked(&publisher_manifest.master_public_key) {
        return Err(ValidatorListError::PublisherRevoked);
    }
    manifest_store.apply(publisher_manifest.clone());

    let ephemeral_pk = publisher_manifest
        .ephemeral_public_key
        .as_ref()
        .ok_or(ValidatorListError::MissingData)?;

    let mut bundle = ValidatorListV2Bundle::default();

    for entry in blobs_v2 {
        let sig_bytes = hex::decode(&entry.signature_hex)
            .map_err(|e| ValidatorListError::BlobDecode(format!("signature hex: {}", e)))?;
        if !verify_blob_signature(&entry.blob_base64, ephemeral_pk.as_bytes(), &sig_bytes) {
            return Err(ValidatorListError::BlobSignatureInvalid);
        }

        use base64::Engine;
        let blob_json = base64::engine::general_purpose::STANDARD
            .decode(&entry.blob_base64)
            .map_err(|e| ValidatorListError::BlobDecode(format!("base64: {}", e)))?;
        let parsed = parse_blob_json(&blob_json, &publisher_manifest.master_public_key)?;

        let v2 = ValidatorListDataV2 {
            base: parsed.0,
            effective_start: entry.effective_start,
            effective_expiration: entry.effective_expiration,
            delegates: parsed.1,
        };

        if now_unix >= v2.effective_start && now_unix < v2.effective_expiration {
            bundle.active.push(v2);
        } else {
            bundle.inactive.push(v2);
        }
    }

    Ok(bundle)
}

/// Wire-level v2 blob entry (one element of `blobs_v2`).
#[derive(Clone, Debug)]
pub struct BlobV2Wire {
    pub effective_start: u64,
    pub effective_expiration: u64,
    /// Base64-encoded blob bytes (NOT decoded; signature is over these bytes).
    pub blob_base64: Vec<u8>,
    /// Hex-encoded ephemeral signature over `blob_base64`.
    pub signature_hex: Vec<u8>,
}

/// Parse the inner blob JSON for both v1 and v2. Returns the
/// ValidatorListData and (for v2) optional delegate publisher master keys.
fn parse_blob_json(
    blob_json: &[u8],
    publisher_master_key: &PublicKey,
) -> Result<(ValidatorListData, Vec<PublicKey>), ValidatorListError> {
    let blob: serde_json::Value = serde_json::from_slice(blob_json)
        .map_err(|e| ValidatorListError::BlobDecode(format!("json: {}", e)))?;

    let sequence = blob.get("sequence").and_then(|v| v.as_u64()).unwrap_or(0);
    let expiration = blob.get("expiration").and_then(|v| v.as_u64()).unwrap_or(0);

    let validators_arr = blob
        .get("validators")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ValidatorListError::BlobDecode("missing validators array".into()))?;

    let mut validators = Vec::with_capacity(validators_arr.len());
    let mut validator_manifests = Vec::new();
    for entry in validators_arr {
        if let Some(pk_hex) = entry.get("validation_public_key").and_then(|v| v.as_str()) {
            if let Ok(pk_bytes) = hex::decode(pk_hex) {
                if let Ok(pk) = PublicKey::from_slice(&pk_bytes) {
                    validators.push(pk);
                }
            }
        }
        if let Some(manifest_b64) = entry.get("manifest").and_then(|v| v.as_str()) {
            use base64::Engine;
            if let Ok(manifest_bytes) =
                base64::engine::general_purpose::STANDARD.decode(manifest_b64)
            {
                validator_manifests.push(manifest_bytes);
            }
        }
    }

    let mut delegates = Vec::new();
    if let Some(arr) = blob.get("delegates").and_then(|v| v.as_array()) {
        for d in arr {
            if let Some(pk_hex) = d.as_str() {
                if let Ok(pk_bytes) = hex::decode(pk_hex) {
                    if let Ok(pk) = PublicKey::from_slice(&pk_bytes) {
                        delegates.push(pk);
                    }
                }
            }
        }
    }

    Ok((
        ValidatorListData {
            sequence,
            expiration,
            validators,
            validator_manifests,
            publisher_master_key: publisher_master_key.clone(),
        },
        delegates,
    ))
}

/// Maximum cascade depth. Chosen as 3 because (a) deeper trust chains
/// are operationally hard to reason about, (b) it matches the plan's
/// default, and (c) it bounds the HTTP fetch fan-out.
pub const CASCADE_DEPTH_DEFAULT: usize = 3;

/// Trait used by [`resolve_cascade`] to fetch a delegate publisher's v2
/// payload. Production code wires this to the HTTP VL fetcher; tests
/// implement it in-memory.
///
/// Returning `Ok(None)` indicates the delegate is unknown / not
/// reachable; the caller may treat this as
/// [`ValidatorListError::DelegateFetchFailed`].
pub trait DelegateResolver {
    /// Fetch (publisher_manifest_bytes, blobs_v2) for `delegate_pk`.
    fn resolve(
        &mut self,
        delegate_pk: &PublicKey,
    ) -> Result<Option<(Vec<u8>, Vec<BlobV2Wire>)>, ValidatorListError>;
}

/// Resolve a cascade chain rooted at `primary_blobs`.
///
/// For every active blob in `primary_blobs`, recursively follow each
/// `delegates` reference, fetch and verify the delegate's v2 payload,
/// and append its active blobs to the output.
///
/// Constraints:
///   - Maximum recursion depth `depth_limit` (depth 0 = primary only).
///   - Cycles are rejected via `visited` set (publisher master keys).
///   - Delegates flagged revoked in `manifest_store` are rejected.
///
/// Returns the union of primary + cascaded active blobs in DFS order.
pub fn resolve_cascade<R: DelegateResolver>(
    primary_blobs: Vec<ValidatorListDataV2>,
    resolver: &mut R,
    manifest_store: &mut ManifestStore,
    now_unix: u64,
    depth_limit: usize,
) -> Result<Vec<ValidatorListDataV2>, ValidatorListError> {
    let mut visited: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    // Seed visited with primary publishers so they cannot be re-pulled
    // as delegates of themselves.
    for b in &primary_blobs {
        visited.insert(b.base.publisher_master_key.as_bytes().to_vec());
    }
    let mut out: Vec<ValidatorListDataV2> = Vec::new();
    // Walk delegates from each primary blob at depth=1.
    let primary_clone = primary_blobs.clone();
    out.extend(primary_blobs);
    for blob in &primary_clone {
        for delegate_pk in &blob.delegates {
            walk_delegate(
                delegate_pk,
                1,
                depth_limit,
                resolver,
                manifest_store,
                now_unix,
                &mut visited,
                &mut out,
            )?;
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn walk_delegate<R: DelegateResolver>(
    delegate_pk: &PublicKey,
    depth: usize,
    depth_limit: usize,
    resolver: &mut R,
    manifest_store: &mut ManifestStore,
    now_unix: u64,
    visited: &mut std::collections::HashSet<Vec<u8>>,
    out: &mut Vec<ValidatorListDataV2>,
) -> Result<(), ValidatorListError> {
    if depth > depth_limit {
        return Err(ValidatorListError::CascadeDepthExceeded(depth_limit));
    }
    let key_bytes = delegate_pk.as_bytes().to_vec();
    if !visited.insert(key_bytes) {
        return Err(ValidatorListError::CyclicDelegate);
    }
    if manifest_store.is_revoked(delegate_pk) {
        return Err(ValidatorListError::DelegateRevoked);
    }

    let (manifest_bytes, blobs_v2) = resolver.resolve(delegate_pk)?.ok_or_else(|| {
        ValidatorListError::DelegateFetchFailed(hex::encode(delegate_pk.as_bytes()))
    })?;

    let bundle = verify_and_parse_v2(&manifest_bytes, &blobs_v2, manifest_store, now_unix)
        .map_err(|e| match e {
            ValidatorListError::BlobSignatureInvalid => ValidatorListError::CascadeSignatureInvalid,
            other => other,
        })?;

    // Snapshot delegates of each active blob before we move it into out.
    let next_delegates: Vec<Vec<PublicKey>> =
        bundle.active.iter().map(|b| b.delegates.clone()).collect();
    out.extend(bundle.active);

    for delegates in next_delegates {
        for next_pk in delegates {
            walk_delegate(
                &next_pk,
                depth + 1,
                depth_limit,
                resolver,
                manifest_store,
                now_unix,
                visited,
                out,
            )?;
        }
    }
    Ok(())
}

/// Verify the blob signature using the publisher's ephemeral key.
///
/// The signature is over the raw blob bytes (the base64-encoded string,
/// NOT the decoded content).
fn verify_blob_signature(blob_bytes: &[u8], public_key: &[u8], signature: &[u8]) -> bool {
    if public_key.first() == Some(&0xED) {
        rxrpl_crypto::ed25519::verify(blob_bytes, public_key, signature)
    } else {
        rxrpl_crypto::secp256k1::verify(blob_bytes, public_key, signature)
    }
}

/// Cached state for a single VL publisher.
///
/// Holds the publisher's currently-valid signing public key (used to verify
/// VL blob signatures) plus every VL accepted under this publisher. When the
/// master key is revoked all cached VLs are dropped.
#[derive(Clone, Debug)]
pub struct PublisherState {
    /// The publisher's master (long-lived) public key.
    pub master_pk: PublicKey,
    /// The currently-valid signing key used to verify VL signatures.
    pub signing_pk: PublicKey,
    /// VLs accepted under this publisher.
    pub vls: Vec<ValidatorListData>,
    /// True once a revocation manifest has been observed for this master.
    pub revoked: bool,
}

/// Track trusted VL publishers, their latest sequence numbers, and the
/// signing key currently authorized for VL signature verification.
#[derive(Debug, Default)]
pub struct ValidatorListTracker {
    /// Publisher master key hex -> latest sequence seen.
    latest_sequences: std::collections::HashMap<String, u64>,
    /// Set of trusted publisher master key hex strings.
    trusted_publishers: std::collections::HashSet<String>,
    /// Per-publisher state: cached signing key + accepted VLs.
    publishers: std::collections::HashMap<String, PublisherState>,
}

impl ValidatorListTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a trusted publisher by master public key.
    pub fn add_trusted_publisher(&mut self, master_pk: &PublicKey) {
        self.trusted_publishers
            .insert(hex::encode(master_pk.as_bytes()));
    }

    /// Check if a publisher is trusted.
    pub fn is_trusted_publisher(&self, master_pk: &PublicKey) -> bool {
        self.trusted_publishers
            .contains(&hex::encode(master_pk.as_bytes()))
    }

    /// Record a VL sequence for a publisher, returning true if this is newer.
    pub fn record_sequence(&mut self, master_pk: &PublicKey, sequence: u64) -> bool {
        let key = hex::encode(master_pk.as_bytes());
        let entry = self.latest_sequences.entry(key).or_insert(0);
        if sequence > *entry {
            *entry = sequence;
            true
        } else {
            false
        }
    }

    /// Register (or replace) a publisher with its initial signing key.
    ///
    /// Required before [`Self::rotate_publisher_signing_key`] or
    /// [`Self::cache_validator_list`] can be called for that master key.
    pub fn register_publisher(&mut self, master_pk: PublicKey, signing_pk: PublicKey) {
        let hex_key = hex::encode(master_pk.as_bytes());
        self.publishers.insert(
            hex_key,
            PublisherState {
                master_pk,
                signing_pk,
                vls: Vec::new(),
                revoked: false,
            },
        );
    }

    /// Cache a verified VL under its publisher master key.
    pub fn cache_validator_list(
        &mut self,
        vl: ValidatorListData,
    ) -> Result<(), ValidatorListError> {
        let hex_key = hex::encode(vl.publisher_master_key.as_bytes());
        let state = self
            .publishers
            .get_mut(&hex_key)
            .ok_or(ValidatorListError::UnknownPublisher)?;
        if state.revoked {
            return Err(ValidatorListError::PublisherRevoked);
        }
        state.vls.push(vl);
        Ok(())
    }

    /// Borrow the cached state for a publisher, if any.
    pub fn publisher_state(&self, master_pk: &PublicKey) -> Option<&PublisherState> {
        self.publishers.get(&hex::encode(master_pk.as_bytes()))
    }

    /// Return the currently-valid signing key for `master_pk`, if known.
    pub fn signing_key_for(&self, master_pk: &PublicKey) -> Option<&PublicKey> {
        self.publisher_state(master_pk).map(|s| &s.signing_pk)
    }

    /// Atomically rotate a publisher's signing key.
    ///
    /// Verifies that `master_sig` is a valid signature from the currently
    /// registered master key over the rotation payload
    /// `HashPrefix::MANIFEST || master_pk || new_signing_pk`, then swaps
    /// the cached signing key. Existing cached VLs are kept (they were
    /// already verified under the previous signing key); only future VL
    /// fetches will be checked against `new_signing_pk`.
    ///
    // NIGHT-SHIFT-REVIEW: rotation payload format is
    // `HashPrefix::MANIFEST || master_pk || new_signing_pk` rather than the
    // full rippled STObject manifest body (sequence + sfPublicKey +
    // sfSigningPubKey + optional sfDomain). Chosen because the spec does
    // not mandate STObject framing for the rotation message and a minimal
    // domain-separated payload keeps the API surface narrow. If wire
    // compatibility with rippled-emitted rotation manifests is required,
    // swap this for a `manifest::parse_and_verify`-based implementation
    // that consumes rippled-format manifest bytes directly.
    ///
    /// Returns `Err(UnknownPublisher)` if no publisher is registered for
    /// `master_pk`, `Err(PublisherRevoked)` if the master key has been
    /// revoked, or `Err(RotationSignatureInvalid)` if the master signature
    /// fails to verify.
    pub fn rotate_publisher_signing_key(
        &mut self,
        master_pk: &PublicKey,
        new_signing_pk: PublicKey,
        master_sig: &[u8],
    ) -> Result<(), ValidatorListError> {
        let hex_key = hex::encode(master_pk.as_bytes());
        let state = self
            .publishers
            .get_mut(&hex_key)
            .ok_or(ValidatorListError::UnknownPublisher)?;
        if state.revoked {
            return Err(ValidatorListError::PublisherRevoked);
        }

        let payload = rotation_signing_payload(master_pk, &new_signing_pk);
        if !verify_blob_signature(&payload, master_pk.as_bytes(), master_sig) {
            return Err(ValidatorListError::RotationSignatureInvalid);
        }

        state.signing_pk = new_signing_pk;
        Ok(())
    }

    /// Apply a publisher manifest, dropping cached VLs on revocation.
    ///
    /// When `manifest.sequence == MANIFEST_REVOKED_SEQ`, the publisher is
    /// marked revoked and ALL cached VLs under that master key are
    /// invalidated. A `tracing::warn!` is emitted with the master-key
    /// fingerprint.
    ///
    /// Returns `true` if a revocation was applied (cached VLs dropped),
    /// `false` otherwise (non-revocation manifest or unknown publisher).
    pub fn apply_publisher_manifest(&mut self, manifest: &manifest::Manifest) -> bool {
        if manifest.sequence != MANIFEST_REVOKED_SEQ {
            return false;
        }
        let hex_key = hex::encode(manifest.master_public_key.as_bytes());
        let Some(state) = self.publishers.get_mut(&hex_key) else {
            return false;
        };
        let dropped = state.vls.len();
        state.vls.clear();
        state.revoked = true;
        tracing::warn!(
            "publisher master key revoked: {} (dropped {} cached VLs)",
            hex::encode(manifest.master_public_key.as_bytes()),
            dropped,
        );
        true
    }
}

/// Build the bytes a publisher master key signs to authorize a new signing
/// key. Uses the standard XRPL manifest hash prefix as a domain separator
/// so a stray signature cannot be replayed against unrelated payloads.
fn rotation_signing_payload(master_pk: &PublicKey, new_signing_pk: &PublicKey) -> Vec<u8> {
    let prefix = rxrpl_crypto::hash_prefix::HashPrefix::MANIFEST.to_bytes();
    let mut buf = Vec::with_capacity(
        prefix.len() + master_pk.as_bytes().len() + new_signing_pk.as_bytes().len(),
    );
    buf.extend_from_slice(&prefix);
    buf.extend_from_slice(master_pk.as_bytes());
    buf.extend_from_slice(new_signing_pk.as_bytes());
    buf
}

/// Sign the rotation payload for `(master_pk, new_signing_pk)` with the
/// caller-provided master keypair. Used by tests and node operators that
/// need to produce a valid `master_sig` for [`ValidatorListTracker::rotate_publisher_signing_key`].
pub fn sign_rotation_payload(
    master_keypair: &rxrpl_crypto::KeyPair,
    new_signing_pk: &PublicKey,
) -> Result<Vec<u8>, ValidatorListError> {
    let payload = rotation_signing_payload(&master_keypair.public_key, new_signing_pk);
    let sig = match master_keypair.key_type {
        rxrpl_crypto::KeyType::Ed25519 => {
            rxrpl_crypto::ed25519::sign(&payload, &master_keypair.private_key)
        }
        rxrpl_crypto::KeyType::Secp256k1 => {
            rxrpl_crypto::secp256k1::sign(&payload, &master_keypair.private_key)
        }
    }
    .map_err(|_| ValidatorListError::RotationSignatureInvalid)?;
    Ok(sig.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest;

    /// Helper: build a signed VL blob and publisher manifest for testing.
    fn make_test_vl(
        publisher_seed: &str,
        eph_seed: &str,
        validators: &[&str],
        sequence: u64,
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        use base64::Engine;

        let pub_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase(publisher_seed),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let eph_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase(eph_seed),
            rxrpl_crypto::KeyType::Ed25519,
        );

        // Build publisher manifest -- both sigs sign the same data
        let signing_data = manifest::build_signing_data(
            1, // manifest sequence
            pub_kp.public_key.as_bytes(),
            eph_kp.public_key.as_bytes(),
            None,
        );
        let eph_sig = rxrpl_crypto::ed25519::sign(&signing_data, &eph_kp.private_key).unwrap();
        let master_sig = rxrpl_crypto::ed25519::sign(&signing_data, &pub_kp.private_key).unwrap();

        let publisher_manifest = manifest::build_manifest_bytes(
            1,
            pub_kp.public_key.as_bytes(),
            eph_kp.public_key.as_bytes(),
            eph_sig.as_bytes(),
            master_sig.as_bytes(),
            None,
        );

        // Build blob JSON
        let validator_entries: Vec<serde_json::Value> = validators
            .iter()
            .map(|v| {
                let kp = rxrpl_crypto::KeyPair::from_seed(
                    &rxrpl_crypto::Seed::from_passphrase(v),
                    rxrpl_crypto::KeyType::Ed25519,
                );
                serde_json::json!({
                    "validation_public_key": hex::encode_upper(kp.public_key.as_bytes()),
                    "manifest": base64::engine::general_purpose::STANDARD.encode(b"placeholder"),
                })
            })
            .collect();

        let blob_json = serde_json::json!({
            "sequence": sequence,
            "expiration": 999999999u64,
            "validators": validator_entries,
        });

        let blob_b64 = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&blob_json).unwrap());

        // Sign the blob with ephemeral key
        let blob_sig =
            rxrpl_crypto::ed25519::sign(blob_b64.as_bytes(), &eph_kp.private_key).unwrap();
        let sig_hex = hex::encode(blob_sig.as_bytes());

        (
            publisher_manifest,
            blob_b64.into_bytes(),
            sig_hex.into_bytes(),
        )
    }

    #[test]
    fn verify_valid_validator_list() {
        let (manifest, blob, sig) =
            make_test_vl("vl_pub1", "vl_eph1", &["val1", "val2", "val3"], 1);
        let mut store = ManifestStore::new();

        let result = verify_and_parse(&manifest, &blob, &sig, &mut store);
        assert!(result.is_ok(), "expected Ok, got {:?}", result);

        let vl = result.unwrap();
        assert_eq!(vl.validators.len(), 3);
        assert_eq!(vl.sequence, 1);
        assert_eq!(vl.expiration, 999999999);
    }

    #[test]
    fn reject_tampered_blob_signature() {
        let (manifest, blob, mut sig) = make_test_vl("vl_pub2", "vl_eph2", &["val1"], 1);

        // Tamper signature
        if let Some(b) = sig.get_mut(0) {
            *b = b'0';
        }

        let mut store = ManifestStore::new();
        let result = verify_and_parse(&manifest, &blob, &sig, &mut store);
        assert!(result.is_err());
    }

    #[test]
    fn reject_tampered_blob_content() {
        let (manifest, mut blob, sig) = make_test_vl("vl_pub3", "vl_eph3", &["val1"], 1);

        // Tamper blob
        if let Some(b) = blob.get_mut(5) {
            *b ^= 0xFF;
        }

        let mut store = ManifestStore::new();
        let result = verify_and_parse(&manifest, &blob, &sig, &mut store);
        assert!(result.is_err());
    }

    #[test]
    fn validator_list_tracker_sequence() {
        let mut tracker = ValidatorListTracker::new();
        let pk = PublicKey(vec![0xED; 33]);

        assert!(tracker.record_sequence(&pk, 1));
        assert!(tracker.record_sequence(&pk, 5));
        assert!(!tracker.record_sequence(&pk, 5)); // same
        assert!(!tracker.record_sequence(&pk, 3)); // older
        assert!(tracker.record_sequence(&pk, 10)); // newer
    }

    #[test]
    fn validator_list_tracker_trusted() {
        let mut tracker = ValidatorListTracker::new();
        let pk = PublicKey(vec![0xED; 33]);

        assert!(!tracker.is_trusted_publisher(&pk));
        tracker.add_trusted_publisher(&pk);
        assert!(tracker.is_trusted_publisher(&pk));
    }

    /// T39: rotating the publisher signing key with a valid master signature
    /// succeeds and subsequent VL fetches will be checked against the new key.
    #[test]
    fn rotate_signing_key_accepts_valid_chain() {
        let master_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("t39_rotate_master"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let initial_signing_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("t39_rotate_signing_a"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let new_signing_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("t39_rotate_signing_b"),
            rxrpl_crypto::KeyType::Ed25519,
        );

        let mut tracker = ValidatorListTracker::new();
        tracker.register_publisher(
            master_kp.public_key.clone(),
            initial_signing_kp.public_key.clone(),
        );
        assert_eq!(
            tracker.signing_key_for(&master_kp.public_key).unwrap(),
            &initial_signing_kp.public_key
        );

        let sig = sign_rotation_payload(&master_kp, &new_signing_kp.public_key)
            .expect("rotation signing must succeed");

        tracker
            .rotate_publisher_signing_key(
                &master_kp.public_key,
                new_signing_kp.public_key.clone(),
                &sig,
            )
            .expect("rotation must verify and apply");

        assert_eq!(
            tracker.signing_key_for(&master_kp.public_key).unwrap(),
            &new_signing_kp.public_key,
            "subsequent VL fetches must use the rotated signing key"
        );
    }

    /// T39: rotation request signed by something other than the registered
    /// master key is rejected and the cached signing key is left unchanged.
    #[test]
    fn rotate_signing_key_rejects_unsigned() {
        let master_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("t39_reject_master"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let initial_signing_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("t39_reject_signing_a"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let new_signing_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("t39_reject_signing_b"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let attacker_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("t39_attacker"),
            rxrpl_crypto::KeyType::Ed25519,
        );

        let mut tracker = ValidatorListTracker::new();
        tracker.register_publisher(
            master_kp.public_key.clone(),
            initial_signing_kp.public_key.clone(),
        );

        // Forge a rotation signed by the attacker, not the real master.
        let bad_sig = sign_rotation_payload(&attacker_kp, &new_signing_kp.public_key)
            .expect("attacker can produce a sig over the payload");

        let err = tracker
            .rotate_publisher_signing_key(
                &master_kp.public_key,
                new_signing_kp.public_key.clone(),
                &bad_sig,
            )
            .expect_err("rotation must fail when not signed by the registered master");
        assert!(matches!(err, ValidatorListError::RotationSignatureInvalid));

        assert_eq!(
            tracker.signing_key_for(&master_kp.public_key).unwrap(),
            &initial_signing_kp.public_key,
            "cached signing key must be unchanged after a rejected rotation"
        );
    }

    /// Helper: build a v2 payload (publisher manifest + blobs_v2 entries).
    /// Each blob entry takes (effective_start, effective_expiration, sequence,
    /// validators, delegates).
    #[allow(clippy::type_complexity)]
    fn make_test_vl_v2(
        publisher_seed: &str,
        eph_seed: &str,
        blobs: &[(u64, u64, u64, &[&str], &[PublicKey])],
    ) -> (Vec<u8>, Vec<BlobV2Wire>) {
        use base64::Engine;

        let pub_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase(publisher_seed),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let eph_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase(eph_seed),
            rxrpl_crypto::KeyType::Ed25519,
        );

        let signing_data = manifest::build_signing_data(
            1,
            pub_kp.public_key.as_bytes(),
            eph_kp.public_key.as_bytes(),
            None,
        );
        let eph_sig = rxrpl_crypto::ed25519::sign(&signing_data, &eph_kp.private_key).unwrap();
        let master_sig = rxrpl_crypto::ed25519::sign(&signing_data, &pub_kp.private_key).unwrap();

        let publisher_manifest = manifest::build_manifest_bytes(
            1,
            pub_kp.public_key.as_bytes(),
            eph_kp.public_key.as_bytes(),
            eph_sig.as_bytes(),
            master_sig.as_bytes(),
            None,
        );

        let mut wire = Vec::new();
        for (start, end, seq, validators, delegates) in blobs {
            let validator_entries: Vec<serde_json::Value> = validators
                .iter()
                .map(|v| {
                    let kp = rxrpl_crypto::KeyPair::from_seed(
                        &rxrpl_crypto::Seed::from_passphrase(v),
                        rxrpl_crypto::KeyType::Ed25519,
                    );
                    serde_json::json!({
                        "validation_public_key": hex::encode_upper(kp.public_key.as_bytes()),
                        "manifest": base64::engine::general_purpose::STANDARD.encode(b"placeholder"),
                    })
                })
                .collect();

            let delegate_entries: Vec<String> = delegates
                .iter()
                .map(|pk| hex::encode_upper(pk.as_bytes()))
                .collect();

            let blob_json = serde_json::json!({
                "sequence": *seq,
                "expiration": *end,
                "validators": validator_entries,
                "delegates": delegate_entries,
            });
            let blob_b64 = base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(&blob_json).unwrap());
            let blob_sig =
                rxrpl_crypto::ed25519::sign(blob_b64.as_bytes(), &eph_kp.private_key).unwrap();
            wire.push(BlobV2Wire {
                effective_start: *start,
                effective_expiration: *end,
                blob_base64: blob_b64.into_bytes(),
                signature_hex: hex::encode(blob_sig.as_bytes()).into_bytes(),
            });
        }

        (publisher_manifest, wire)
    }

    /// B1: parse a v2 payload with three blobs and time=100.
    /// blob1: 50..150 (active), blob2: 0..50 (expired), blob3: 150..200 (future).
    #[test]
    fn parse_vl_v2_multiple_blobs_filters_by_time() {
        let v_a = ["v2_val_a"];
        let v_b = ["v2_val_b"];
        let v_c = ["v2_val_c"];
        let no_delegates: Vec<PublicKey> = vec![];
        let (manifest, wire) = make_test_vl_v2(
            "v2_pub_b1",
            "v2_eph_b1",
            &[
                (50, 150, 1, &v_a, &no_delegates),
                (0, 50, 2, &v_b, &no_delegates),
                (150, 200, 3, &v_c, &no_delegates),
            ],
        );

        let mut store = ManifestStore::new();
        let bundle =
            verify_and_parse_v2(&manifest, &wire, &mut store, 100).expect("v2 parse must succeed");

        assert_eq!(bundle.active.len(), 1, "exactly one blob is in window");
        assert_eq!(bundle.inactive.len(), 2, "two blobs are out of window");
        assert_eq!(bundle.active[0].base.sequence, 1);
        assert_eq!(bundle.active[0].effective_start, 50);
        assert_eq!(bundle.active[0].effective_expiration, 150);
        assert_eq!(bundle.active[0].base.validators.len(), 1);
    }

    /// In-memory DelegateResolver for tests. Maps publisher master key
    /// (raw bytes) -> (manifest, blobs_v2).
    struct MockResolver {
        entries: std::collections::HashMap<Vec<u8>, (Vec<u8>, Vec<BlobV2Wire>)>,
    }

    impl MockResolver {
        fn new() -> Self {
            Self {
                entries: std::collections::HashMap::new(),
            }
        }
        fn insert(&mut self, pk: &PublicKey, manifest: Vec<u8>, wire: Vec<BlobV2Wire>) {
            self.entries
                .insert(pk.as_bytes().to_vec(), (manifest, wire));
        }
    }

    impl DelegateResolver for MockResolver {
        fn resolve(
            &mut self,
            delegate_pk: &PublicKey,
        ) -> Result<Option<(Vec<u8>, Vec<BlobV2Wire>)>, ValidatorListError> {
            Ok(self.entries.get(&delegate_pk.as_bytes().to_vec()).cloned())
        }
    }

    /// Helper: extract a publisher's master key from a publisher seed.
    fn pub_master_pk(seed: &str) -> PublicKey {
        rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase(seed),
            rxrpl_crypto::KeyType::Ed25519,
        )
        .public_key
        .clone()
    }

    /// B3: cascade with one delegate level merges validators from primary
    /// and delegate publishers.
    #[test]
    fn cascade_resolves_one_level() {
        let delegate_pk = pub_master_pk("b3_delegate_pub");
        let v_a = ["b3_val_primary"];
        let v_b = ["b3_val_delegate"];
        let no_delegates: Vec<PublicKey> = vec![];

        // Primary references delegate.
        let (primary_manifest, primary_wire) = make_test_vl_v2(
            "b3_primary_pub",
            "b3_primary_eph",
            &[(0, 1000, 1, &v_a, &[delegate_pk.clone()][..])],
        );
        // Delegate's own VL (no further delegates).
        let (delegate_manifest, delegate_wire) = make_test_vl_v2(
            "b3_delegate_pub",
            "b3_delegate_eph",
            &[(0, 1000, 1, &v_b, &no_delegates)],
        );

        let mut store = ManifestStore::new();
        let bundle = verify_and_parse_v2(&primary_manifest, &primary_wire, &mut store, 100)
            .expect("primary parse ok");
        assert_eq!(bundle.active.len(), 1);

        let mut resolver = MockResolver::new();
        resolver.insert(&delegate_pk, delegate_manifest, delegate_wire);

        let merged = resolve_cascade(
            bundle.active,
            &mut resolver,
            &mut store,
            100,
            CASCADE_DEPTH_DEFAULT,
        )
        .expect("cascade resolve ok");

        // Expect 2 blobs total: primary + delegate.
        assert_eq!(merged.len(), 2);
        let total_validators: usize = merged.iter().map(|b| b.base.validators.len()).sum();
        assert_eq!(total_validators, 2);
    }

    /// B3: cascade depth limit is enforced.
    #[test]
    fn cascade_depth_exceeded() {
        // Build chain: primary -> d1 -> d2 -> d3 -> d4. depth_limit=2 should reject.
        let d1_pk = pub_master_pk("b3_chain_d1");
        let d2_pk = pub_master_pk("b3_chain_d2");
        let d3_pk = pub_master_pk("b3_chain_d3");
        let d4_pk = pub_master_pk("b3_chain_d4");
        let no_v: [&str; 0] = [];
        let no_delegates: Vec<PublicKey> = vec![];

        let (m_primary, w_primary) = make_test_vl_v2(
            "b3_chain_primary",
            "b3_chain_primary_eph",
            &[(0, 1000, 1, &no_v, &[d1_pk.clone()][..])],
        );
        let (m1, w1) = make_test_vl_v2(
            "b3_chain_d1",
            "b3_chain_d1_eph",
            &[(0, 1000, 1, &no_v, &[d2_pk.clone()][..])],
        );
        let (m2, w2) = make_test_vl_v2(
            "b3_chain_d2",
            "b3_chain_d2_eph",
            &[(0, 1000, 1, &no_v, &[d3_pk.clone()][..])],
        );
        let (m3, w3) = make_test_vl_v2(
            "b3_chain_d3",
            "b3_chain_d3_eph",
            &[(0, 1000, 1, &no_v, &[d4_pk.clone()][..])],
        );
        let (m4, w4) = make_test_vl_v2(
            "b3_chain_d4",
            "b3_chain_d4_eph",
            &[(0, 1000, 1, &no_v, &no_delegates)],
        );

        let mut store = ManifestStore::new();
        let bundle = verify_and_parse_v2(&m_primary, &w_primary, &mut store, 100).unwrap();
        let mut resolver = MockResolver::new();
        resolver.insert(&d1_pk, m1, w1);
        resolver.insert(&d2_pk, m2, w2);
        resolver.insert(&d3_pk, m3, w3);
        resolver.insert(&d4_pk, m4, w4);

        let res = resolve_cascade(bundle.active, &mut resolver, &mut store, 100, 2);
        assert!(matches!(
            res,
            Err(ValidatorListError::CascadeDepthExceeded(2))
        ));
    }

    /// B3: cycle (A -> B -> A) is rejected.
    #[test]
    fn cascade_rejects_cycle() {
        let primary_pk = pub_master_pk("b3_cycle_primary");
        let d_pk = pub_master_pk("b3_cycle_d");
        let no_v: [&str; 0] = [];

        let (m_primary, w_primary) = make_test_vl_v2(
            "b3_cycle_primary",
            "b3_cycle_primary_eph",
            &[(0, 1000, 1, &no_v, &[d_pk.clone()][..])],
        );
        // Delegate cycles back to primary.
        let (m_d, w_d) = make_test_vl_v2(
            "b3_cycle_d",
            "b3_cycle_d_eph",
            &[(0, 1000, 1, &no_v, &[primary_pk.clone()][..])],
        );

        let mut store = ManifestStore::new();
        let bundle = verify_and_parse_v2(&m_primary, &w_primary, &mut store, 100).unwrap();
        let mut resolver = MockResolver::new();
        resolver.insert(&d_pk, m_d, w_d);

        let res = resolve_cascade(bundle.active, &mut resolver, &mut store, 100, 3);
        assert!(matches!(res, Err(ValidatorListError::CyclicDelegate)));
    }

    /// B4: tampered delegate signature must surface as CascadeSignatureInvalid.
    #[test]
    fn cascade_rejects_tampered_delegate_signature() {
        let delegate_pk = pub_master_pk("b4_delegate_pub");
        let no_v: [&str; 0] = [];
        let v_b = ["b4_val_delegate"];
        let no_delegates: Vec<PublicKey> = vec![];

        let (m_primary, w_primary) = make_test_vl_v2(
            "b4_primary_pub",
            "b4_primary_eph",
            &[(0, 1000, 1, &no_v, &[delegate_pk.clone()][..])],
        );
        let (m_d, mut w_d) = make_test_vl_v2(
            "b4_delegate_pub",
            "b4_delegate_eph",
            &[(0, 1000, 1, &v_b, &no_delegates)],
        );
        // Tamper the delegate's signature byte 0.
        if let Some(b) = w_d[0].signature_hex.get_mut(0) {
            *b = if *b == b'0' { b'1' } else { b'0' };
        }

        let mut store = ManifestStore::new();
        let bundle = verify_and_parse_v2(&m_primary, &w_primary, &mut store, 100).unwrap();
        let mut resolver = MockResolver::new();
        resolver.insert(&delegate_pk, m_d, w_d);

        let res = resolve_cascade(
            bundle.active,
            &mut resolver,
            &mut store,
            100,
            CASCADE_DEPTH_DEFAULT,
        );
        assert!(
            matches!(res, Err(ValidatorListError::CascadeSignatureInvalid)),
            "expected CascadeSignatureInvalid, got {:?}",
            res
        );
    }

    /// B4: revoked delegate publisher is rejected before any cascade fetch.
    #[test]
    fn cascade_rejects_revoked_delegate() {
        let delegate_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("b4_revoke_delegate_master"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let delegate_pk = delegate_kp.public_key.clone();
        let no_v: [&str; 0] = [];
        let v_b = ["b4_revoke_val_delegate"];
        let no_delegates: Vec<PublicKey> = vec![];

        let (m_primary, w_primary) = make_test_vl_v2(
            "b4_revoke_primary",
            "b4_revoke_primary_eph",
            &[(0, 1000, 1, &no_v, &[delegate_pk.clone()][..])],
        );
        // We won't actually need delegate's wire because revocation
        // intercepts before fetch. But create them for completeness.
        let (m_d, w_d) = make_test_vl_v2(
            "b4_revoke_delegate_master",
            "b4_revoke_delegate_eph",
            &[(0, 1000, 1, &v_b, &no_delegates)],
        );

        let mut store = ManifestStore::new();
        let bundle = verify_and_parse_v2(&m_primary, &w_primary, &mut store, 100).unwrap();

        // Mark the delegate as revoked by applying a revocation manifest.
        let revoke_manifest = manifest::Manifest {
            sequence: MANIFEST_REVOKED_SEQ,
            master_public_key: delegate_pk.clone(),
            ephemeral_public_key: None,
            domain: None,
            raw: vec![],
        };
        store.apply(revoke_manifest);
        assert!(store.is_revoked(&delegate_pk));

        let mut resolver = MockResolver::new();
        resolver.insert(&delegate_pk, m_d, w_d);

        let res = resolve_cascade(
            bundle.active,
            &mut resolver,
            &mut store,
            100,
            CASCADE_DEPTH_DEFAULT,
        );
        assert!(matches!(res, Err(ValidatorListError::DelegateRevoked)));
    }

    /// B1: tampered v2 signature is rejected.
    #[test]
    fn v2_rejects_tampered_signature() {
        let v_a = ["v2_val_t"];
        let no_delegates: Vec<PublicKey> = vec![];
        let (manifest, mut wire) = make_test_vl_v2(
            "v2_pub_tamper",
            "v2_eph_tamper",
            &[(0, 1000, 1, &v_a, &no_delegates)],
        );
        if let Some(b) = wire[0].signature_hex.get_mut(0) {
            *b = if *b == b'0' { b'1' } else { b'0' };
        }
        let mut store = ManifestStore::new();
        let res = verify_and_parse_v2(&manifest, &wire, &mut store, 100);
        assert!(matches!(res, Err(ValidatorListError::BlobSignatureInvalid)));
    }

    /// T39: a revocation manifest (sequence == MANIFEST_REVOKED_SEQ) for a
    /// registered publisher must invalidate every cached VL under that
    /// master key.
    #[test]
    fn revocation_drops_all_cached_vls() {
        let master_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("t39_revoke_master"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let signing_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("t39_revoke_signing"),
            rxrpl_crypto::KeyType::Ed25519,
        );

        let mut tracker = ValidatorListTracker::new();
        tracker.register_publisher(master_kp.public_key.clone(), signing_kp.public_key.clone());

        let vl_a = ValidatorListData {
            sequence: 1,
            expiration: 999_999_999,
            validators: vec![PublicKey(vec![0xED; 33])],
            validator_manifests: vec![],
            publisher_master_key: master_kp.public_key.clone(),
        };
        let vl_b = ValidatorListData {
            sequence: 2,
            expiration: 999_999_999,
            validators: vec![PublicKey(vec![0xED; 33]), PublicKey(vec![0x02; 33])],
            validator_manifests: vec![],
            publisher_master_key: master_kp.public_key.clone(),
        };
        tracker.cache_validator_list(vl_a).expect("cache vl_a");
        tracker.cache_validator_list(vl_b).expect("cache vl_b");
        assert_eq!(
            tracker
                .publisher_state(&master_kp.public_key)
                .unwrap()
                .vls
                .len(),
            2
        );

        // Build the revocation manifest the way rippled does: sequence == u32::MAX.
        let revoke_manifest = manifest::Manifest {
            sequence: MANIFEST_REVOKED_SEQ,
            master_public_key: master_kp.public_key.clone(),
            ephemeral_public_key: None,
            domain: None,
            raw: vec![],
        };

        let dropped = tracker.apply_publisher_manifest(&revoke_manifest);
        assert!(dropped, "revocation must report it dropped state");

        let state = tracker
            .publisher_state(&master_kp.public_key)
            .expect("publisher entry remains so revocation is observable");
        assert!(state.revoked, "publisher must be flagged revoked");
        assert!(state.vls.is_empty(), "all cached VLs must be dropped");

        // Subsequent attempts to cache a VL under the revoked publisher fail.
        let vl_c = ValidatorListData {
            sequence: 3,
            expiration: 999_999_999,
            validators: vec![],
            validator_manifests: vec![],
            publisher_master_key: master_kp.public_key.clone(),
        };
        let err = tracker
            .cache_validator_list(vl_c)
            .expect_err("post-revocation caching must be refused");
        assert!(matches!(err, ValidatorListError::PublisherRevoked));
    }
}
