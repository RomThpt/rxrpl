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

    /// Sign a consensus validation with this node's key.
    pub fn sign_validation(&self, validation: &mut rxrpl_consensus::types::Validation) {
        validation.sign(&self.key_pair.private_key, self.key_pair.key_type);
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
}
