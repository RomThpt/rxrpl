use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::PrimitivesError;
use crate::hash::Hash160;

/// A 20-byte XRPL account identifier.
///
/// This is the raw account ID (RIPEMD-160(SHA-256(public_key))).
/// For the human-readable classic address (e.g., "rN7n..."), use the codec crate.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct AccountId(pub [u8; 20]);

impl AccountId {
    pub const ZERO: Self = Self([0u8; 20]);

    pub fn new(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    pub fn from_slice(slice: &[u8]) -> Result<Self, PrimitivesError> {
        let arr: [u8; 20] = slice
            .try_into()
            .map_err(|_| PrimitivesError::InvalidLength {
                expected: 20,
                got: slice.len(),
            })?;
        Ok(Self(arr))
    }

    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }

    pub fn to_hash160(&self) -> Hash160 {
        Hash160(self.0)
    }
}

impl From<Hash160> for AccountId {
    fn from(hash: Hash160) -> Self {
        Self(hash.0)
    }
}

impl From<[u8; 20]> for AccountId {
    fn from(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8]> for AccountId {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AccountId({})", hex::encode(self.0))
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode_upper(self.0))
    }
}

impl FromStr for AccountId {
    type Err = PrimitivesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(s).map_err(|e| PrimitivesError::InvalidHex(e.to_string()))?;
        Self::from_slice(&bytes)
    }
}

impl Serialize for AccountId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&hex::encode_upper(self.0))
    }
}

impl<'de> Deserialize<'de> for AccountId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        // Accept both hex and classic address format (just hex for primitives)
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_id_from_hex() {
        let hex_str = "88a5a57c829f40f25ea83385bbde6c3d8b4ca082";
        let id = AccountId::from_str(hex_str).unwrap();
        assert_eq!(id.to_string(), hex_str.to_uppercase());
    }

    #[test]
    fn account_id_roundtrip() {
        let bytes = [1u8; 20];
        let id = AccountId::new(bytes);
        let hex_str = id.to_string();
        let id2 = AccountId::from_str(&hex_str).unwrap();
        assert_eq!(id, id2);
    }
}
