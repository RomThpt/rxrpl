use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::PrimitivesError;

/// An XRPL public key (33 bytes: prefix byte + 32 bytes).
///
/// - Ed25519: 0xED prefix
/// - Secp256k1: 0x02 or 0x03 prefix (compressed)
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PublicKey(pub Vec<u8>);

impl PublicKey {
    pub const ED25519_PREFIX: u8 = 0xED;

    pub fn new(bytes: Vec<u8>) -> Result<Self, PrimitivesError> {
        if bytes.len() != 33 {
            return Err(PrimitivesError::InvalidLength {
                expected: 33,
                got: bytes.len(),
            });
        }
        Ok(Self(bytes))
    }

    pub fn from_slice(slice: &[u8]) -> Result<Self, PrimitivesError> {
        Self::new(slice.to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn is_ed25519(&self) -> bool {
        self.0.first() == Some(&Self::ED25519_PREFIX)
    }

    pub fn is_secp256k1(&self) -> bool {
        matches!(self.0.first(), Some(0x02) | Some(0x03))
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", hex::encode(&self.0))
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode_upper(&self.0))
    }
}

impl FromStr for PublicKey {
    type Err = PrimitivesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(s).map_err(|e| PrimitivesError::InvalidHex(e.to_string()))?;
        Self::new(bytes)
    }
}

impl Serialize for PublicKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&hex::encode_upper(&self.0))
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// A cryptographic signature (variable length, typically DER-encoded for secp256k1
/// or 64 bytes for ed25519).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Signature(pub Vec<u8>);

impl Signature {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature({})", hex::encode(&self.0))
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode_upper(&self.0))
    }
}

impl FromStr for Signature {
    type Err = PrimitivesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(s).map_err(|e| PrimitivesError::InvalidHex(e.to_string()))?;
        Ok(Self(bytes))
    }
}

impl Serialize for Signature {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&hex::encode_upper(&self.0))
    }
}

impl<'de> Deserialize<'de> for Signature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_key_ed25519() {
        let hex_str = "ED9434799226374926EDA3B54B1B461B4ABF7237962EAE18528FEA67595397FA32";
        let key = PublicKey::from_str(hex_str).unwrap();
        assert!(key.is_ed25519());
        assert!(!key.is_secp256k1());
        assert_eq!(key.as_bytes().len(), 33);
    }

    #[test]
    fn public_key_secp256k1() {
        let hex_str = "035f6ddbd6afc5f2cb3d7d08005577580dcc92ac5292d5d4f36683152691933e59";
        let key = PublicKey::from_str(hex_str).unwrap();
        assert!(key.is_secp256k1());
        assert!(!key.is_ed25519());
    }

    #[test]
    fn public_key_invalid_length() {
        assert!(PublicKey::from_str("ABCD").is_err());
    }
}
