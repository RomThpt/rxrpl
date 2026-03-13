use rand::RngCore;
use sha2::{Digest, Sha512};
use zeroize::Zeroize;

/// A 16-byte seed used for key derivation. Zeroized on drop.
#[derive(Clone, Zeroize)]
#[zeroize(drop)]
pub struct Seed(pub [u8; 16]);

impl Seed {
    /// Generate a cryptographically random seed.
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// Create a seed from a passphrase (SHA-512, first 16 bytes).
    pub fn from_passphrase(passphrase: &str) -> Self {
        let hash = Sha512::digest(passphrase.as_bytes());
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&hash[..16]);
        Self(bytes)
    }

    /// Create from raw bytes.
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl std::fmt::Debug for Seed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Seed([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_seeds_differ() {
        let s1 = Seed::random();
        let s2 = Seed::random();
        assert_ne!(s1.0, s2.0);
    }

    #[test]
    fn passphrase_deterministic() {
        let s1 = Seed::from_passphrase("masterpassphrase");
        let s2 = Seed::from_passphrase("masterpassphrase");
        assert_eq!(s1.0, s2.0);
    }
}
