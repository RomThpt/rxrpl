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

use crate::manifest::{self, ManifestStore};

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

    let sequence = blob
        .get("sequence")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let expiration = blob
        .get("expiration")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let validators_arr = blob
        .get("validators")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ValidatorListError::BlobDecode("missing validators array".into()))?;

    let mut validators = Vec::with_capacity(validators_arr.len());
    let mut validator_manifests = Vec::new();

    for entry in validators_arr {
        // Extract master public key
        if let Some(pk_hex) = entry
            .get("validation_public_key")
            .and_then(|v| v.as_str())
        {
            if let Ok(pk_bytes) = hex::decode(pk_hex) {
                if let Ok(pk) = PublicKey::from_slice(&pk_bytes) {
                    validators.push(pk);
                }
            }
        }

        // Extract individual validator manifest
        if let Some(manifest_b64) = entry.get("manifest").and_then(|v| v.as_str()) {
            if let Ok(manifest_bytes) = base64::engine::general_purpose::STANDARD.decode(manifest_b64)
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

/// Track trusted VL publishers and their latest sequence numbers.
#[derive(Debug, Default)]
pub struct ValidatorListTracker {
    /// Publisher master key hex -> latest sequence seen.
    latest_sequences: std::collections::HashMap<String, u64>,
    /// Set of trusted publisher master key hex strings.
    trusted_publishers: std::collections::HashSet<String>,
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
        let master_sig =
            rxrpl_crypto::ed25519::sign(&signing_data, &pub_kp.private_key).unwrap();

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
        let (manifest, blob, mut sig) =
            make_test_vl("vl_pub2", "vl_eph2", &["val1"], 1);

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
        let (manifest, mut blob, sig) =
            make_test_vl("vl_pub3", "vl_eph3", &["val1"], 1);

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
        assert!(!tracker.record_sequence(&pk, 5));  // same
        assert!(!tracker.record_sequence(&pk, 3));  // older
        assert!(tracker.record_sequence(&pk, 10));   // newer
    }

    #[test]
    fn validator_list_tracker_trusted() {
        let mut tracker = ValidatorListTracker::new();
        let pk = PublicKey(vec![0xED; 33]);

        assert!(!tracker.is_trusted_publisher(&pk));
        tracker.add_trusted_publisher(&pk);
        assert!(tracker.is_trusted_publisher(&pk));
    }
}
