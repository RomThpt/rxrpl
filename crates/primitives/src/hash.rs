use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::PrimitivesError;

macro_rules! define_hash {
    ($name:ident, $len:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub struct $name(pub [u8; $len]);

        impl $name {
            pub const ZERO: Self = Self([0u8; $len]);
            pub const LEN: usize = $len;

            pub fn new(bytes: [u8; $len]) -> Self {
                Self(bytes)
            }

            pub fn from_slice(slice: &[u8]) -> Result<Self, PrimitivesError> {
                let arr: [u8; $len] =
                    slice
                        .try_into()
                        .map_err(|_| PrimitivesError::InvalidLength {
                            expected: $len,
                            got: slice.len(),
                        })?;
                Ok(Self(arr))
            }

            pub fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }

            pub fn is_zero(&self) -> bool {
                self.0.iter().all(|&b| b == 0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), hex::encode(self.0))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", hex::encode_upper(self.0))
            }
        }

        impl FromStr for $name {
            type Err = PrimitivesError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let bytes =
                    hex::decode(s).map_err(|e| PrimitivesError::InvalidHex(e.to_string()))?;
                Self::from_slice(&bytes)
            }
        }

        impl AsRef<[u8]> for $name {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }

        impl From<[u8; $len]> for $name {
            fn from(bytes: [u8; $len]) -> Self {
                Self(bytes)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(&hex::encode_upper(self.0))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let s = String::deserialize(deserializer)?;
                Self::from_str(&s).map_err(serde::de::Error::custom)
            }
        }
    };
}

define_hash!(Hash128, 16);
define_hash!(Hash160, 20);
define_hash!(Hash192, 24);
define_hash!(Hash256, 32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash256_from_hex() {
        let hex_str = "4BC50C9B0D8515D3EAAE1E74B29A95804346C491EE1A95BF25E4AAB854A6A652";
        let hash = Hash256::from_str(hex_str).unwrap();
        assert_eq!(hash.to_string(), hex_str);
    }

    #[test]
    fn hash256_zero() {
        let hash = Hash256::ZERO;
        assert!(hash.is_zero());
        assert_eq!(
            hash.to_string(),
            "0000000000000000000000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn hash256_serde_roundtrip() {
        let hex_str = "4BC50C9B0D8515D3EAAE1E74B29A95804346C491EE1A95BF25E4AAB854A6A652";
        let hash = Hash256::from_str(hex_str).unwrap();
        let json = serde_json::to_string(&hash).unwrap();
        let decoded: Hash256 = serde_json::from_str(&json).unwrap();
        assert_eq!(hash, decoded);
    }

    #[test]
    fn hash160_from_hex() {
        let hex_str = "88a5a57c829f40f25ea83385bbde6c3d8b4ca082";
        let hash = Hash160::from_str(hex_str).unwrap();
        assert_eq!(hash.to_string(), hex_str.to_uppercase());
    }

    #[test]
    fn invalid_hex_length() {
        assert!(Hash256::from_str("ABCD").is_err());
    }

    #[test]
    fn invalid_hex_chars() {
        assert!(
            Hash256::from_str("ZZZZ0C9B0D8515D3EAAE1E74B29A95804346C491EE1A95BF25E4AAB854A6A652")
                .is_err()
        );
    }
}
