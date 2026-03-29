use rxrpl_crypto::{KeyPair, KeyType, Seed};
use rxrpl_primitives::Hash256;

/// Node identity (keypair and derived node ID).
pub struct NodeIdentity {
    key_pair: KeyPair,
    /// The node ID derived from the public key (SHA-512-Half).
    pub node_id: Hash256,
}

impl NodeIdentity {
    /// Generate a random node identity (secp256k1 for rippled compatibility).
    pub fn generate() -> Self {
        let key_pair = KeyPair::generate(KeyType::Secp256k1);
        let node_id = rxrpl_crypto::sha512_half::sha512_half(&[key_pair.public_key.as_bytes()]);
        Self { key_pair, node_id }
    }

    /// Create a deterministic identity from a seed (secp256k1 for rippled compatibility).
    pub fn from_seed(seed: &Seed) -> Self {
        let key_pair = KeyPair::from_seed(seed, KeyType::Secp256k1);
        let node_id = rxrpl_crypto::sha512_half::sha512_half(&[key_pair.public_key.as_bytes()]);
        Self { key_pair, node_id }
    }

    /// Get the raw public key bytes (33 bytes).
    pub fn public_key_bytes(&self) -> &[u8] {
        self.key_pair.public_key.as_bytes()
    }

    /// Get the key type used by this identity.
    pub fn key_type(&self) -> KeyType {
        self.key_pair.key_type
    }

    /// Sign data with this node's private key (hashes before signing).
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        match self.key_pair.key_type {
            KeyType::Ed25519 => rxrpl_crypto::ed25519::sign(data, &self.key_pair.private_key)
                .map(|sig| sig.as_bytes().to_vec())
                .unwrap_or_default(),
            KeyType::Secp256k1 => rxrpl_crypto::secp256k1::sign(data, &self.key_pair.private_key)
                .map(|sig| sig.as_bytes().to_vec())
                .unwrap_or_default(),
        }
    }

    /// Sign a pre-hashed 32-byte digest directly (no additional hashing).
    ///
    /// Used for protocols like the rippled HTTP upgrade handshake where
    /// the session cookie is already a hash.
    pub fn sign_digest(&self, digest: &[u8; 32]) -> Vec<u8> {
        match self.key_pair.key_type {
            KeyType::Ed25519 => rxrpl_crypto::ed25519::sign(digest, &self.key_pair.private_key)
                .map(|sig| sig.as_bytes().to_vec())
                .unwrap_or_default(),
            KeyType::Secp256k1 => {
                rxrpl_crypto::secp256k1::sign_digest(digest, &self.key_pair.private_key)
                    .map(|sig| sig.as_bytes().to_vec())
                    .unwrap_or_default()
            }
        }
    }

    /// Sign a consensus proposal with this node's key.
    pub fn sign_proposal(&self, proposal: &mut rxrpl_consensus::types::Proposal) {
        proposal.sign(&self.key_pair.private_key, self.key_pair.key_type);
    }

    /// Sign a consensus validation with this node's key (STObject format).
    ///
    /// Produces a signature over the STObject signing hash:
    /// SHA-512-Half(HashPrefix::validation || STObject_without_signature)
    pub fn sign_validation(&self, validation: &mut rxrpl_consensus::types::Validation) {
        use crate::stobject;

        // HashPrefix::validation = 'V','A','L',0 = 0x56414C00
        const HASH_PREFIX_VALIDATION: [u8; 4] = [0x56, 0x41, 0x4C, 0x00];

        // Build signing data: prefix + STObject fields (without sfSignature)
        let mut signing_data = Vec::with_capacity(128);
        signing_data.extend_from_slice(&HASH_PREFIX_VALIDATION);

        // sfFlags (UINT32, field 2)
        let flags: u32 = if validation.full { 0x80000001 } else { 0x00000000 };
        stobject::put_uint32(&mut signing_data, 2, flags);

        // sfLedgerSequence (UINT32, field 6)
        stobject::put_uint32(&mut signing_data, 6, validation.ledger_seq);

        // sfSigningTime (UINT32, field 9)
        stobject::put_uint32(&mut signing_data, 9, validation.sign_time);

        // sfLedgerHash (UINT256, field 1)
        stobject::put_hash256(&mut signing_data, 1, validation.ledger_hash.as_bytes());

        // sfSigningPubKey (VL, field 3)
        stobject::put_vl(&mut signing_data, 3, self.public_key_bytes());

        // Sign: SHA-512-Half(signing_data) then ECDSA
        let sig = rxrpl_crypto::secp256k1::sign(&signing_data, &self.key_pair.private_key)
            .map(|s| s.as_bytes().to_vec());
        if let Ok(sig) = sig {
            validation.signature = Some(sig);
        }
    }

    /// Get the private key bytes (for signing operations).
    pub fn private_key(&self) -> &[u8] {
        &self.key_pair.private_key
    }
}

/// Verify a validation's STObject signature against the embedded public key.
///
/// Reconstructs the same signing data that `NodeIdentity::sign_validation` produces:
/// SHA-512-Half(HashPrefix::validation || STObject fields without sfSignature)
/// then verifies the signature with the validation's public key.
///
/// Returns `false` if the signature is missing, the public key is empty,
/// or the signature does not match.
pub fn verify_validation_signature(validation: &rxrpl_consensus::types::Validation) -> bool {
    use crate::stobject;

    let sig = match &validation.signature {
        Some(s) => s,
        None => return false,
    };

    if validation.public_key.is_empty() {
        return false;
    }

    // HashPrefix::validation = 'V','A','L',0 = 0x56414C00
    const HASH_PREFIX_VALIDATION: [u8; 4] = [0x56, 0x41, 0x4C, 0x00];

    // Build signing data: prefix + STObject fields (without sfSignature)
    // Must match the exact field order used by sign_validation.
    let mut signing_data = Vec::with_capacity(128);
    signing_data.extend_from_slice(&HASH_PREFIX_VALIDATION);

    // sfFlags (UINT32, field 2)
    let flags: u32 = if validation.full { 0x80000001 } else { 0x00000000 };
    stobject::put_uint32(&mut signing_data, 2, flags);

    // sfLedgerSequence (UINT32, field 6)
    stobject::put_uint32(&mut signing_data, 6, validation.ledger_seq);

    // sfSigningTime (UINT32, field 9)
    stobject::put_uint32(&mut signing_data, 9, validation.sign_time);

    // sfLedgerHash (UINT256, field 1)
    stobject::put_hash256(&mut signing_data, 1, validation.ledger_hash.as_bytes());

    // sfSigningPubKey (VL, field 3)
    stobject::put_vl(&mut signing_data, 3, &validation.public_key);

    // Verify: the signature was produced by signing signing_data with secp256k1
    // (sign_validation always uses secp256k1 regardless of key type prefix,
    // because NodeIdentity is always secp256k1 for rippled compatibility).
    let is_ed25519 = validation.public_key.first() == Some(&0xED);
    if is_ed25519 {
        rxrpl_crypto::ed25519::verify(&signing_data, &validation.public_key, sig)
    } else {
        rxrpl_crypto::secp256k1::verify(&signing_data, &validation.public_key, sig)
    }
}

impl std::fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeIdentity")
            .field("node_id", &self.node_id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_identity() {
        let id = NodeIdentity::generate();
        assert!(!id.node_id.is_zero());
        assert_eq!(id.public_key_bytes().len(), 33);
        // secp256k1 compressed key starts with 0x02 or 0x03
        assert!(id.public_key_bytes()[0] == 0x02 || id.public_key_bytes()[0] == 0x03);
    }

    #[test]
    fn from_seed_deterministic() {
        let seed = Seed::from_passphrase("test-node");
        let id1 = NodeIdentity::from_seed(&seed);
        let seed2 = Seed::from_passphrase("test-node");
        let id2 = NodeIdentity::from_seed(&seed2);
        assert_eq!(id1.node_id, id2.node_id);
    }

    #[test]
    fn sign_produces_valid_signature() {
        let id = NodeIdentity::generate();
        let data = b"test message";
        let sig = id.sign(data);
        assert!(!sig.is_empty());
        assert!(rxrpl_crypto::secp256k1::verify(
            data,
            id.public_key_bytes(),
            &sig
        ));
    }

    #[test]
    fn validation_sign_verify_roundtrip() {
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let id = NodeIdentity::generate();
        let mut validation = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
        };

        // Unsigned validation should fail verification
        assert!(!verify_validation_signature(&validation));

        // Sign and verify
        id.sign_validation(&mut validation);
        assert!(validation.signature.is_some());
        assert!(verify_validation_signature(&validation));
    }

    #[test]
    fn validation_tampered_fails_verify() {
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let id = NodeIdentity::generate();
        let mut validation = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
        };

        id.sign_validation(&mut validation);

        // Tamper with ledger hash
        validation.ledger_hash = Hash256::new([0xDD; 32]);
        assert!(!verify_validation_signature(&validation));
    }

    #[test]
    fn validation_wrong_key_fails_verify() {
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let id1 = NodeIdentity::generate();
        let id2 = NodeIdentity::generate();

        let mut validation = Validation {
            node_id: NodeId(id1.node_id),
            public_key: id1.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
        };

        id1.sign_validation(&mut validation);
        assert!(verify_validation_signature(&validation));

        // Replace public key with a different node's key -- should fail
        validation.public_key = id2.public_key_bytes().to_vec();
        assert!(!verify_validation_signature(&validation));
    }

    #[test]
    fn validation_missing_signature_fails_verify() {
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let id = NodeIdentity::generate();
        let validation = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
        };

        assert!(!verify_validation_signature(&validation));
    }

    #[test]
    fn validation_empty_pubkey_fails_verify() {
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let id = NodeIdentity::generate();
        let mut validation = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
        };

        id.sign_validation(&mut validation);
        // Clear public key
        validation.public_key = Vec::new();
        assert!(!verify_validation_signature(&validation));
    }
}
