use rxrpl_crypto::{KeyPair, KeyType, Seed};
use rxrpl_primitives::Hash256;

/// Node identity (keypair and derived node ID).
pub struct NodeIdentity {
    key_pair: KeyPair,
    /// The node ID derived from the public key (SHA-512-Half).
    pub node_id: Hash256,
}

impl NodeIdentity {
    /// Generate a random node identity.
    pub fn generate() -> Self {
        let key_pair = KeyPair::generate(KeyType::Ed25519);
        let node_id = rxrpl_crypto::sha512_half::sha512_half(&[key_pair.public_key.as_bytes()]);
        Self { key_pair, node_id }
    }

    /// Create a deterministic identity from a seed.
    pub fn from_seed(seed: &Seed) -> Self {
        let key_pair = KeyPair::from_seed(seed, KeyType::Ed25519);
        let node_id = rxrpl_crypto::sha512_half::sha512_half(&[key_pair.public_key.as_bytes()]);
        Self { key_pair, node_id }
    }

    /// Get the raw public key bytes (33 bytes, 0xED prefix).
    pub fn public_key_bytes(&self) -> &[u8] {
        self.key_pair.public_key.as_bytes()
    }

    /// Sign data with this node's private key.
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        rxrpl_crypto::ed25519::sign(data, &self.key_pair.private_key)
            .map(|sig| sig.as_bytes().to_vec())
            .unwrap_or_default()
    }

    /// Sign a consensus proposal with this node's key.
    pub fn sign_proposal(&self, proposal: &mut rxrpl_consensus::types::Proposal) {
        proposal.sign(&self.key_pair.private_key);
    }

    /// Sign a consensus validation with this node's key.
    pub fn sign_validation(&self, validation: &mut rxrpl_consensus::types::Validation) {
        validation.sign(&self.key_pair.private_key);
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
        assert_eq!(id.public_key_bytes()[0], 0xED);
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
        assert!(rxrpl_crypto::ed25519::verify(
            data,
            id.public_key_bytes(),
            &sig
        ));
    }
}
