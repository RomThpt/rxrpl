use std::fmt;

/// Cryptographic key type used in XRPL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyType {
    Secp256k1,
    Ed25519,
}

impl fmt::Display for KeyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Secp256k1 => write!(f, "secp256k1"),
            Self::Ed25519 => write!(f, "ed25519"),
        }
    }
}

impl KeyType {
    /// Detect key type from public key bytes.
    /// Ed25519: 33 bytes, first byte 0xED.
    /// Secp256k1: 33 bytes, first byte 0x02 or 0x03.
    pub fn from_public_key(pubkey: &[u8]) -> Option<Self> {
        if pubkey.len() != 33 {
            return None;
        }
        match pubkey[0] {
            0xED => Some(Self::Ed25519),
            0x02 | 0x03 => Some(Self::Secp256k1),
            _ => None,
        }
    }
}
