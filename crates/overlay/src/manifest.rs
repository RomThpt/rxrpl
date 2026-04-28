/// Manifest parsing, verification, and storage for XRPL validator identity.
///
/// A manifest binds a validator's permanent master key to a rotating
/// ephemeral key.  Each manifest carries a monotonically increasing
/// sequence number; a sequence of `u32::MAX` revokes the master key.
///
/// Wire format (rippled STObject binary):
///   - sfSequence    (UINT32, field 1)  -- manifest sequence
///   - sfPublicKey   (VL,     field 1)  -- master public key (33 bytes)
///   - sfSigningPubKey (VL,   field 3)  -- ephemeral public key (33 bytes)
///   - sfSignature   (VL,     field 6)  -- ephemeral signature over inner data
///   - sfMasterSignature (VL, field 4, extended type 16) -- master signature
///   - Optional: sfDomain (VL, field 7) -- domain string
///
/// Verification:
///   The manifest body (everything except sfMasterSignature) prefixed with
///   `HashPrefix::MANIFEST` is signed by both the ephemeral key (sfSignature)
///   and the master key (sfMasterSignature).

use std::collections::HashMap;

use rxrpl_primitives::PublicKey;

use crate::stobject;

/// Revocation sequence: master key permanently revoked.
pub const MANIFEST_REVOKED_SEQ: u32 = u32::MAX;

// SField identifiers used in manifest STObjects (for reference):
//   sfSequence:         STI_UINT32(2), field 1
//   sfPublicKey:        STI_VL(7),     field 1  (master)
//   sfSigningPubKey:    STI_VL(7),     field 3  (ephemeral)
//   sfMasterSignature:  STI_VL(7),     field 4
//   sfSignature:        STI_VL(7),     field 6  (ephemeral sig)
//   sfDomain:           STI_VL(7),     field 7

/// A parsed and verified manifest.
#[derive(Clone, Debug)]
pub struct Manifest {
    /// Monotonically increasing sequence number.
    pub sequence: u32,
    /// The validator's permanent master public key (33 bytes).
    pub master_public_key: PublicKey,
    /// The current ephemeral signing key (33 bytes).
    /// Empty/absent when sequence == MANIFEST_REVOKED_SEQ (revocation).
    pub ephemeral_public_key: Option<PublicKey>,
    /// Optional domain claim.
    pub domain: Option<String>,
    /// The raw serialized manifest bytes (for relay).
    pub raw: Vec<u8>,
}

impl Manifest {
    /// Returns true if this manifest revokes the master key.
    pub fn is_revoked(&self) -> bool {
        self.sequence == MANIFEST_REVOKED_SEQ
    }
}

/// Errors during manifest parsing or verification.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("truncated manifest data")]
    Truncated,
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("invalid public key length")]
    InvalidKeyLength,
    #[error("ephemeral signature verification failed")]
    EphemeralSigInvalid,
    #[error("master signature verification failed")]
    MasterSigInvalid,
    #[error("manifest revoked")]
    Revoked,
    #[error("sfDomain field is not valid UTF-8")]
    InvalidDomain,
}

/// Intermediate parse result before verification.
struct RawManifest {
    sequence: u32,
    master_public_key: Vec<u8>,
    ephemeral_public_key: Vec<u8>,
    signature: Vec<u8>,         // ephemeral sig
    master_signature: Vec<u8>,  // master sig
    domain: Option<String>,
    /// The data covered by both signatures: everything except sfMasterSignature.
    signing_data: Vec<u8>,
}

/// Parse raw manifest bytes into fields without verifying signatures.
fn parse_raw(data: &[u8]) -> Result<RawManifest, ManifestError> {
    let mut pos = 0;
    let mut sequence: Option<u32> = None;
    let mut master_pk: Option<Vec<u8>> = None;
    let mut ephemeral_pk: Option<Vec<u8>> = None;
    let mut signature: Option<Vec<u8>> = None;
    let mut master_signature: Option<Vec<u8>> = None;
    let mut domain: Option<String> = None;

    // We need to track which byte ranges belong to non-master-signature fields
    // to reconstruct signing_data.  The signing_data is the manifest bytes with
    // the sfMasterSignature field stripped out.
    let mut signing_ranges: Vec<(usize, usize)> = Vec::new();
    let mut master_sig_range: Option<(usize, usize)> = None;

    while pos < data.len() {
        let field_start = pos;
        let Some((type_id, field_id, consumed)) = stobject::decode_field_id(&data[pos..]) else {
            break;
        };
        pos += consumed;

        match (type_id, field_id) {
            // sfSequence: UINT32, field 1
            (2, 1) => {
                if pos + 4 > data.len() {
                    return Err(ManifestError::Truncated);
                }
                sequence = Some(u32::from_be_bytes([
                    data[pos],
                    data[pos + 1],
                    data[pos + 2],
                    data[pos + 3],
                ]));
                pos += 4;
                signing_ranges.push((field_start, pos));
            }
            // VL fields (type 7)
            (7, fid) => {
                let Some((vl_len, vl_consumed)) = stobject::decode_vl_length(&data[pos..]) else {
                    return Err(ManifestError::Truncated);
                };
                pos += vl_consumed;
                if pos + vl_len > data.len() {
                    return Err(ManifestError::Truncated);
                }
                let value = data[pos..pos + vl_len].to_vec();
                pos += vl_len;

                match fid {
                    1 => {
                        // sfPublicKey (master)
                        master_pk = Some(value);
                        signing_ranges.push((field_start, pos));
                    }
                    3 => {
                        // sfSigningPubKey (ephemeral)
                        ephemeral_pk = Some(value);
                        signing_ranges.push((field_start, pos));
                    }
                    4 => {
                        // sfMasterSignature: excluded from signing data
                        master_signature = Some(value);
                        master_sig_range = Some((field_start, pos));
                    }
                    6 => {
                        // sfSignature (ephemeral sig): excluded from signing data
                        signature = Some(value);
                    }
                    7 => {
                        // sfDomain: strict UTF-8 (rippled rejects non-UTF-8 to prevent
                        // U+FFFD impersonation via lossy substitution).
                        domain = Some(
                            String::from_utf8(value.to_vec())
                                .map_err(|_| ManifestError::InvalidDomain)?,
                        );
                        signing_ranges.push((field_start, pos));
                    }
                    _ => {
                        // Unknown VL field, include in signing data
                        signing_ranges.push((field_start, pos));
                    }
                }
            }
            // Unknown UINT32 field
            (2, _) => {
                if pos + 4 > data.len() {
                    return Err(ManifestError::Truncated);
                }
                pos += 4;
                signing_ranges.push((field_start, pos));
            }
            // Unknown UINT64 field (type 3)
            (3, _) => {
                if pos + 8 > data.len() {
                    return Err(ManifestError::Truncated);
                }
                pos += 8;
                signing_ranges.push((field_start, pos));
            }
            // Unknown UINT256 field (type 5)
            (5, _) => {
                if pos + 32 > data.len() {
                    return Err(ManifestError::Truncated);
                }
                pos += 32;
                signing_ranges.push((field_start, pos));
            }
            // Other VL-like extended types
            _ => {
                // If this is a VL type (type >= 16 mapped through extended), try VL decode
                // For safety, break on unknown types to avoid infinite loops
                break;
            }
        }
    }

    // Build signing_data: HashPrefix::MANIFEST + all fields except master signature
    let prefix = rxrpl_crypto::hash_prefix::HashPrefix::MANIFEST.to_bytes();
    let mut signing_data = Vec::with_capacity(data.len() + 4);
    signing_data.extend_from_slice(&prefix);
    for (start, end) in &signing_ranges {
        signing_data.extend_from_slice(&data[*start..*end]);
    }

    // If there's no explicit master_sig_range but we have a master_signature,
    // it was already excluded from signing_ranges.
    let _ = master_sig_range;

    Ok(RawManifest {
        sequence: sequence.ok_or(ManifestError::MissingField("sfSequence"))?,
        master_public_key: master_pk.ok_or(ManifestError::MissingField("sfPublicKey"))?,
        ephemeral_public_key: ephemeral_pk.unwrap_or_default(),
        signature: signature.unwrap_or_default(),
        master_signature: master_signature
            .ok_or(ManifestError::MissingField("sfMasterSignature"))?,
        domain,
        signing_data,
    })
}

/// Verify a signature with auto-detection of key type (ed25519 vs secp256k1).
fn verify_signature(message: &[u8], public_key: &[u8], signature: &[u8]) -> bool {
    if public_key.first() == Some(&0xED) {
        rxrpl_crypto::ed25519::verify(message, public_key, signature)
    } else {
        rxrpl_crypto::secp256k1::verify(message, public_key, signature)
    }
}

/// Parse and verify a manifest from raw binary (STObject) bytes.
///
/// Verifies both the ephemeral signature and the master signature.
/// Returns the verified `Manifest` on success.
pub fn parse_and_verify(data: &[u8]) -> Result<Manifest, ManifestError> {
    let raw = parse_raw(data)?;

    // Verify master signature over signing_data
    if !verify_signature(&raw.signing_data, &raw.master_public_key, &raw.master_signature) {
        return Err(ManifestError::MasterSigInvalid);
    }

    // For non-revocation manifests, verify ephemeral signature too
    let ephemeral_pk = if raw.sequence != MANIFEST_REVOKED_SEQ {
        if raw.ephemeral_public_key.is_empty() {
            return Err(ManifestError::MissingField("sfSigningPubKey"));
        }
        if raw.signature.is_empty() {
            return Err(ManifestError::MissingField("sfSignature"));
        }
        if !verify_signature(&raw.signing_data, &raw.ephemeral_public_key, &raw.signature) {
            return Err(ManifestError::EphemeralSigInvalid);
        }
        let pk = PublicKey::from_slice(&raw.ephemeral_public_key)
            .map_err(|_| ManifestError::InvalidKeyLength)?;
        Some(pk)
    } else {
        None
    };

    let master_pk = PublicKey::from_slice(&raw.master_public_key)
        .map_err(|_| ManifestError::InvalidKeyLength)?;

    Ok(Manifest {
        sequence: raw.sequence,
        master_public_key: master_pk,
        ephemeral_public_key: ephemeral_pk,
        domain: raw.domain,
        raw: data.to_vec(),
    })
}

/// Build a manifest STObject from parts (for testing).
///
/// Produces the binary format that `parse_and_verify` expects.
pub fn build_manifest_bytes(
    sequence: u32,
    master_pk: &[u8],
    ephemeral_pk: &[u8],
    ephemeral_sig: &[u8],
    master_sig: &[u8],
    domain: Option<&str>,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(256);

    // sfSequence (UINT32 type=2, field=1)
    stobject::put_uint32(&mut buf, 1, sequence);

    // sfPublicKey (VL type=7, field=1)
    stobject::put_vl(&mut buf, 1, master_pk);

    // sfSigningPubKey (VL type=7, field=3)
    stobject::put_vl(&mut buf, 3, ephemeral_pk);

    // sfDomain (VL type=7, field=7) -- optional
    if let Some(d) = domain {
        stobject::put_vl(&mut buf, 7, d.as_bytes());
    }

    // sfSignature (VL type=7, field=6)
    stobject::put_vl(&mut buf, 6, ephemeral_sig);

    // sfMasterSignature (VL type=7, field=4)
    stobject::put_vl(&mut buf, 4, master_sig);

    buf
}

/// Build the signing data for a manifest (for computing signatures).
///
/// This is HashPrefix::MANIFEST + non-signature fields only.
/// Both sfSignature and sfMasterSignature are excluded.
pub fn build_signing_data(
    sequence: u32,
    master_pk: &[u8],
    ephemeral_pk: &[u8],
    domain: Option<&str>,
) -> Vec<u8> {
    let prefix = rxrpl_crypto::hash_prefix::HashPrefix::MANIFEST.to_bytes();
    let mut buf = Vec::with_capacity(256);
    buf.extend_from_slice(&prefix);

    stobject::put_uint32(&mut buf, 1, sequence);
    stobject::put_vl(&mut buf, 1, master_pk);
    stobject::put_vl(&mut buf, 3, ephemeral_pk);
    if let Some(d) = domain {
        stobject::put_vl(&mut buf, 7, d.as_bytes());
    }

    buf
}

/// Sign a message with a keypair, dispatching on key type.
///
/// Ed25519 signs the raw message; secp256k1 signs SHA-512/256 of the
/// message (rippled's `sign()` convention) and DER-encodes the result.
fn sign_with_keypair(
    message: &[u8],
    key_pair: &rxrpl_crypto::KeyPair,
) -> Result<Vec<u8>, ManifestError> {
    let sig = match key_pair.key_type {
        rxrpl_crypto::KeyType::Ed25519 => {
            rxrpl_crypto::ed25519::sign(message, &key_pair.private_key)
        }
        rxrpl_crypto::KeyType::Secp256k1 => {
            rxrpl_crypto::secp256k1::sign(message, &key_pair.private_key)
        }
    }
    .map_err(|_| ManifestError::MasterSigInvalid)?;
    Ok(sig.as_bytes().to_vec())
}

/// Create a fully signed manifest from a master and ephemeral keypair.
///
/// Produces rippled-compatible STObject bytes containing:
///   sfSequence, sfPublicKey (master), sfSigningPubKey (ephemeral),
///   optional sfDomain, sfSignature (ephemeral sig over body),
///   sfMasterSignature (master sig over body).
///
/// Both signatures cover `HashPrefix::MANIFEST || sfSequence || sfPublicKey ||
/// sfSigningPubKey || sfDomain?` (i.e. everything except the two signature
/// fields themselves), matching rippled `Manifest::makeManifest`.
///
/// The returned bytes are accepted by `parse_and_verify`.
pub fn create_signed(
    master_keypair: &rxrpl_crypto::KeyPair,
    ephemeral_keypair: &rxrpl_crypto::KeyPair,
    sequence: u32,
    domain: Option<&str>,
) -> Result<Vec<u8>, ManifestError> {
    let master_pk = master_keypair.public_key.as_bytes();
    let ephemeral_pk = ephemeral_keypair.public_key.as_bytes();

    let signing_data = build_signing_data(sequence, master_pk, ephemeral_pk, domain);

    let ephemeral_sig = sign_with_keypair(&signing_data, ephemeral_keypair)?;
    let master_sig = sign_with_keypair(&signing_data, master_keypair)?;

    Ok(build_manifest_bytes(
        sequence,
        master_pk,
        ephemeral_pk,
        &ephemeral_sig,
        &master_sig,
        domain,
    ))
}

/// Stores the latest manifest for each validator master key.
///
/// Tracks the ephemeral -> master key mapping and detects revocations.
#[derive(Debug, Default)]
pub struct ManifestStore {
    /// Latest manifest per master public key (hex-encoded for map key).
    manifests: HashMap<String, Manifest>,
    /// Ephemeral public key -> master public key mapping.
    ephemeral_to_master: HashMap<String, PublicKey>,
    /// Revoked master keys.
    revoked: HashMap<String, ()>,
}

impl ManifestStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hex-encode a public key for use as a map key.
    fn key_hex(pk: &PublicKey) -> String {
        hex::encode(pk.as_bytes())
    }

    /// Apply a verified manifest.
    ///
    /// Returns `true` if the manifest was accepted (newer than existing).
    /// Returns `false` if rejected (older/same sequence, or already revoked).
    pub fn apply(&mut self, manifest: Manifest) -> bool {
        let master_hex = Self::key_hex(&manifest.master_public_key);

        // Already revoked: reject everything
        if self.revoked.contains_key(&master_hex) {
            return false;
        }

        // Check sequence: must be strictly newer
        if let Some(existing) = self.manifests.get(&master_hex) {
            if manifest.sequence <= existing.sequence {
                return false;
            }
            // Remove old ephemeral mapping
            if let Some(ref old_eph) = existing.ephemeral_public_key {
                let old_eph_hex = Self::key_hex(old_eph);
                self.ephemeral_to_master.remove(&old_eph_hex);
            }
        }

        // Handle revocation
        if manifest.is_revoked() {
            // Remove any ephemeral mapping
            if let Some(ref existing) = self.manifests.get(&master_hex) {
                if let Some(ref old_eph) = existing.ephemeral_public_key {
                    let old_eph_hex = Self::key_hex(old_eph);
                    self.ephemeral_to_master.remove(&old_eph_hex);
                }
            }
            self.revoked.insert(master_hex.clone(), ());
            self.manifests.insert(master_hex, manifest);
            return true;
        }

        // Install new ephemeral mapping
        if let Some(ref eph_pk) = manifest.ephemeral_public_key {
            let eph_hex = Self::key_hex(eph_pk);
            self.ephemeral_to_master
                .insert(eph_hex, manifest.master_public_key.clone());
        }

        self.manifests.insert(master_hex, manifest);
        true
    }

    /// Look up the master public key for a given ephemeral key.
    pub fn master_key_for_ephemeral(&self, ephemeral_pk: &PublicKey) -> Option<&PublicKey> {
        let eph_hex = Self::key_hex(ephemeral_pk);
        self.ephemeral_to_master.get(&eph_hex)
    }

    /// Get the current manifest for a master key.
    pub fn get_manifest(&self, master_pk: &PublicKey) -> Option<&Manifest> {
        let hex = Self::key_hex(master_pk);
        self.manifests.get(&hex)
    }

    /// Check if a master key is revoked.
    pub fn is_revoked(&self, master_pk: &PublicKey) -> bool {
        let hex = Self::key_hex(master_pk);
        self.revoked.contains_key(&hex)
    }

    /// Get the current ephemeral public key for a master key.
    pub fn current_ephemeral_key(&self, master_pk: &PublicKey) -> Option<&PublicKey> {
        let hex = Self::key_hex(master_pk);
        self.manifests
            .get(&hex)
            .and_then(|m| m.ephemeral_public_key.as_ref())
    }

    /// Number of tracked validators (including revoked).
    pub fn len(&self) -> usize {
        self.manifests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }

    /// Get all raw manifest bytes for relay to peers.
    pub fn all_raw_manifests(&self) -> Vec<Vec<u8>> {
        self.manifests.values().map(|m| m.raw.clone()).collect()
    }
}

/// Process a batch of raw manifest stobject bytes.
///
/// Parses, verifies, and applies each manifest to the store.
/// Returns the number of manifests successfully applied.
pub fn process_manifest_batch(store: &mut ManifestStore, raw_manifests: &[Vec<u8>]) -> usize {
    let mut applied = 0;
    for raw in raw_manifests {
        match parse_and_verify(raw) {
            Ok(manifest) => {
                if store.apply(manifest) {
                    applied += 1;
                }
            }
            Err(e) => {
                tracing::debug!("manifest parse/verify failed: {}", e);
            }
        }
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a signed manifest for testing.
    fn make_test_manifest(
        sequence: u32,
        master_seed_phrase: &str,
        ephemeral_seed_phrase: &str,
    ) -> (Vec<u8>, PublicKey, PublicKey) {
        let master_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase(master_seed_phrase),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let eph_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase(ephemeral_seed_phrase),
            rxrpl_crypto::KeyType::Ed25519,
        );

        // Both signatures sign the same data: everything except the two sigs
        let signing_data = build_signing_data(
            sequence,
            master_kp.public_key.as_bytes(),
            eph_kp.public_key.as_bytes(),
            None,
        );

        // Sign with ephemeral key
        let eph_sig = rxrpl_crypto::ed25519::sign(&signing_data, &eph_kp.private_key).unwrap();

        // Sign with master key (same signing data)
        let master_sig =
            rxrpl_crypto::ed25519::sign(&signing_data, &master_kp.private_key).unwrap();

        // Build the full manifest bytes
        let raw = build_manifest_bytes(
            sequence,
            master_kp.public_key.as_bytes(),
            eph_kp.public_key.as_bytes(),
            eph_sig.as_bytes(),
            master_sig.as_bytes(),
            None,
        );

        (raw, master_kp.public_key.clone(), eph_kp.public_key.clone())
    }

    #[test]
    fn parse_and_verify_valid_manifest() {
        let (raw, master_pk, eph_pk) = make_test_manifest(1, "master1", "ephemeral1");
        let manifest = parse_and_verify(&raw).unwrap();
        assert_eq!(manifest.sequence, 1);
        assert_eq!(manifest.master_public_key, master_pk);
        assert_eq!(manifest.ephemeral_public_key.as_ref().unwrap(), &eph_pk);
        assert!(!manifest.is_revoked());
    }

    #[test]
    fn parse_rejects_tampered_manifest() {
        let (mut raw, _, _) = make_test_manifest(1, "master2", "ephemeral2");
        // Tamper with sequence byte (first field after field header)
        if raw.len() > 3 {
            raw[3] ^= 0xFF;
        }
        assert!(parse_and_verify(&raw).is_err());
    }

    #[test]
    fn manifest_store_accepts_newer_sequence() {
        let mut store = ManifestStore::new();

        let (raw1, master_pk, eph_pk1) = make_test_manifest(1, "master3", "ephemeral3a");
        let m1 = parse_and_verify(&raw1).unwrap();
        assert!(store.apply(m1));

        // Verify ephemeral mapping
        assert_eq!(
            store.master_key_for_ephemeral(&eph_pk1).unwrap(),
            &master_pk
        );

        // Newer sequence replaces
        let (raw2, _, eph_pk2) = make_test_manifest(2, "master3", "ephemeral3b");
        let m2 = parse_and_verify(&raw2).unwrap();
        assert!(store.apply(m2));

        // Old ephemeral no longer maps
        assert!(store.master_key_for_ephemeral(&eph_pk1).is_none());
        // New ephemeral maps
        assert_eq!(
            store.master_key_for_ephemeral(&eph_pk2).unwrap(),
            &master_pk
        );
    }

    #[test]
    fn manifest_store_rejects_older_sequence() {
        let mut store = ManifestStore::new();

        let (raw2, _, _) = make_test_manifest(2, "master4", "ephemeral4a");
        let m2 = parse_and_verify(&raw2).unwrap();
        assert!(store.apply(m2));

        let (raw1, _, _) = make_test_manifest(1, "master4", "ephemeral4b");
        let m1 = parse_and_verify(&raw1).unwrap();
        assert!(!store.apply(m1)); // rejected: older
    }

    #[test]
    fn manifest_store_rejects_same_sequence() {
        let mut store = ManifestStore::new();

        let (raw, _, _) = make_test_manifest(5, "master5", "ephemeral5");
        let m = parse_and_verify(&raw).unwrap();
        assert!(store.apply(m));

        let (raw2, _, _) = make_test_manifest(5, "master5", "ephemeral5");
        let m2 = parse_and_verify(&raw2).unwrap();
        assert!(!store.apply(m2)); // rejected: same sequence
    }

    #[test]
    fn manifest_store_revocation() {
        let mut store = ManifestStore::new();

        let (raw1, master_pk, eph_pk) = make_test_manifest(1, "master6", "ephemeral6");
        let m1 = parse_and_verify(&raw1).unwrap();
        assert!(store.apply(m1));

        // Create revocation manifest (sequence = MAX)
        let master_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("master6"),
            rxrpl_crypto::KeyType::Ed25519,
        );

        // For revocation, ephemeral key can be empty but we still need valid sigs
        let signing_data = build_signing_data(
            MANIFEST_REVOKED_SEQ,
            master_kp.public_key.as_bytes(),
            &[0u8; 33], // placeholder ephemeral
            None,
        );

        let master_sig =
            rxrpl_crypto::ed25519::sign(&signing_data, &master_kp.private_key).unwrap();

        // For a revocation, we build with empty ephemeral sig
        // The parser should accept it since sequence == MAX
        let revoke_manifest = Manifest {
            sequence: MANIFEST_REVOKED_SEQ,
            master_public_key: master_pk.clone(),
            ephemeral_public_key: None,
            domain: None,
            raw: vec![], // not important for store test
        };

        assert!(store.apply(revoke_manifest));
        assert!(store.is_revoked(&master_pk));

        // Old ephemeral mapping removed
        assert!(store.master_key_for_ephemeral(&eph_pk).is_none());

        // New manifests rejected after revocation
        let (raw3, _, _) = make_test_manifest(100, "master6", "ephemeral6c");
        let m3 = parse_and_verify(&raw3).unwrap();
        assert!(!store.apply(m3));

        let _ = master_sig; // used for the manual revocation above
    }

    #[test]
    fn process_manifest_batch_counts() {
        let mut store = ManifestStore::new();

        let (raw1, _, _) = make_test_manifest(1, "batchA", "batchEphA");
        let (raw2, _, _) = make_test_manifest(1, "batchB", "batchEphB");

        let count = process_manifest_batch(&mut store, &[raw1, raw2, vec![0xFF]]);
        assert_eq!(count, 2); // 2 valid, 1 invalid
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn manifest_store_get_manifest() {
        let mut store = ManifestStore::new();
        let (raw, master_pk, _) = make_test_manifest(3, "getM", "getE");
        let m = parse_and_verify(&raw).unwrap();
        store.apply(m);

        let retrieved = store.get_manifest(&master_pk).unwrap();
        assert_eq!(retrieved.sequence, 3);
    }

    #[test]
    fn manifest_store_current_ephemeral() {
        let mut store = ManifestStore::new();
        let (raw, master_pk, eph_pk) = make_test_manifest(1, "curE_m", "curE_e");
        let m = parse_and_verify(&raw).unwrap();
        store.apply(m);

        let current = store.current_ephemeral_key(&master_pk).unwrap();
        assert_eq!(current, &eph_pk);
    }

    #[test]
    fn create_signed_round_trip_ed25519_no_domain() {
        let master = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("create_signed_master_ed"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let ephemeral = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("create_signed_eph_ed"),
            rxrpl_crypto::KeyType::Ed25519,
        );

        let raw = create_signed(&master, &ephemeral, 7, None).expect("create_signed");
        let parsed = parse_and_verify(&raw).expect("parse_and_verify");

        assert_eq!(parsed.sequence, 7);
        assert_eq!(parsed.master_public_key, master.public_key);
        assert_eq!(
            parsed.ephemeral_public_key.as_ref().unwrap(),
            &ephemeral.public_key
        );
        assert!(parsed.domain.is_none());
        assert!(!parsed.is_revoked());
        assert_eq!(parsed.raw, raw);
    }

    #[test]
    fn create_signed_round_trip_with_domain() {
        let master = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("create_signed_master_dom"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let ephemeral = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("create_signed_eph_dom"),
            rxrpl_crypto::KeyType::Ed25519,
        );

        let raw = create_signed(&master, &ephemeral, 42, Some("example.com"))
            .expect("create_signed");
        let parsed = parse_and_verify(&raw).expect("parse_and_verify");

        assert_eq!(parsed.sequence, 42);
        assert_eq!(parsed.domain.as_deref(), Some("example.com"));
        assert_eq!(parsed.master_public_key, master.public_key);
        assert_eq!(
            parsed.ephemeral_public_key.as_ref().unwrap(),
            &ephemeral.public_key
        );
    }

    #[test]
    fn parse_rejects_manifest_with_invalid_utf8_domain() {
        // Build a valid manifest with master + ephemeral keys signed correctly,
        // but place invalid UTF-8 bytes (0xFF, 0xFE) in the sfDomain VL field.
        // parse_raw must reject it with ManifestError::InvalidDomain instead of
        // silently substituting U+FFFD (which would allow domain impersonation).
        let master_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("invalid_utf8_domain_master"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let eph_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("invalid_utf8_domain_eph"),
            rxrpl_crypto::KeyType::Ed25519,
        );

        let invalid_domain: &[u8] = &[0xFF, 0xFE];

        // Build signing data manually with the raw (non-UTF-8) domain bytes,
        // matching the on-wire field order used by build_signing_data.
        let prefix = rxrpl_crypto::hash_prefix::HashPrefix::MANIFEST.to_bytes();
        let mut signing_data = Vec::with_capacity(256);
        signing_data.extend_from_slice(&prefix);
        stobject::put_uint32(&mut signing_data, 1, 1);
        stobject::put_vl(&mut signing_data, 1, master_kp.public_key.as_bytes());
        stobject::put_vl(&mut signing_data, 3, eph_kp.public_key.as_bytes());
        stobject::put_vl(&mut signing_data, 7, invalid_domain);

        let eph_sig =
            rxrpl_crypto::ed25519::sign(&signing_data, &eph_kp.private_key).unwrap();
        let master_sig =
            rxrpl_crypto::ed25519::sign(&signing_data, &master_kp.private_key).unwrap();

        // Build the wire-format manifest with the invalid-UTF-8 domain bytes.
        let mut buf = Vec::with_capacity(256);
        stobject::put_uint32(&mut buf, 1, 1);
        stobject::put_vl(&mut buf, 1, master_kp.public_key.as_bytes());
        stobject::put_vl(&mut buf, 3, eph_kp.public_key.as_bytes());
        stobject::put_vl(&mut buf, 7, invalid_domain);
        stobject::put_vl(&mut buf, 6, eph_sig.as_bytes());
        stobject::put_vl(&mut buf, 4, master_sig.as_bytes());

        match parse_raw(&buf) {
            Err(ManifestError::InvalidDomain) => {}
            other => panic!(
                "expected InvalidDomain error for non-UTF-8 sfDomain, got {:?}",
                other.map(|_| "Ok").map_err(|e| format!("{:?}", e))
            ),
        }
    }

    #[test]
    fn create_signed_round_trip_secp256k1_master_ed25519_ephemeral() {
        // rippled allows mixing key types: master often secp256k1, ephemeral ed25519.
        let master = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("create_signed_master_secp"),
            rxrpl_crypto::KeyType::Secp256k1,
        );
        let ephemeral = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("create_signed_eph_mixed"),
            rxrpl_crypto::KeyType::Ed25519,
        );

        let raw = create_signed(&master, &ephemeral, 3, None).expect("create_signed");
        let parsed = parse_and_verify(&raw).expect("parse_and_verify");

        assert_eq!(parsed.sequence, 3);
        assert_eq!(parsed.master_public_key, master.public_key);
        assert_eq!(
            parsed.ephemeral_public_key.as_ref().unwrap(),
            &ephemeral.public_key
        );
    }
}
