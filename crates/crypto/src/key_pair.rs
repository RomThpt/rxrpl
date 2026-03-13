use rxrpl_primitives::PublicKey;

use crate::key_type::KeyType;
use crate::seed::Seed;

/// A keypair consisting of public key and private key bytes.
pub struct KeyPair {
    pub public_key: PublicKey,
    pub private_key: Vec<u8>,
    pub key_type: KeyType,
}

impl KeyPair {
    /// Generate a new random keypair.
    pub fn generate(key_type: KeyType) -> Self {
        let seed = Seed::random();
        Self::from_seed(&seed, key_type)
    }

    /// Derive a keypair from a seed.
    pub fn from_seed(seed: &Seed, key_type: KeyType) -> Self {
        let (public_key, private_key) = match key_type {
            KeyType::Secp256k1 => crate::secp256k1::derive_keypair(seed, false),
            KeyType::Ed25519 => crate::ed25519::derive_keypair(seed),
        };
        Self {
            public_key,
            private_key,
            key_type,
        }
    }
}

impl Drop for KeyPair {
    fn drop(&mut self) {
        // Best-effort zeroing of private key material
        for byte in &mut self.private_key {
            *byte = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_secp256k1() {
        let kp = KeyPair::generate(KeyType::Secp256k1);
        assert!(kp.public_key.is_secp256k1());
        assert_eq!(kp.key_type, KeyType::Secp256k1);
    }

    #[test]
    fn generate_ed25519() {
        let kp = KeyPair::generate(KeyType::Ed25519);
        assert!(kp.public_key.is_ed25519());
        assert_eq!(kp.key_type, KeyType::Ed25519);
    }

    #[test]
    fn from_seed_deterministic() {
        let seed = Seed::from_passphrase("test");
        let kp1 = KeyPair::from_seed(&seed, KeyType::Ed25519);
        let seed2 = Seed::from_passphrase("test");
        let kp2 = KeyPair::from_seed(&seed2, KeyType::Ed25519);
        assert_eq!(kp1.public_key, kp2.public_key);
    }
}
