use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::PrimitivesError;

/// XRPL currency code.
///
/// Standard codes are 3-character ASCII (e.g., "USD", "EUR").
/// Non-standard codes are arbitrary 20-byte values (160-bit hex).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum CurrencyCode {
    /// 3-character standard currency code (ISO 4217-like).
    /// Stored as 3 ASCII bytes.
    Standard([u8; 3]),
    /// Non-standard 20-byte (160-bit) currency code.
    NonStandard([u8; 20]),
}

impl CurrencyCode {
    /// XRP is represented as all zeros in the binary format,
    /// but typically compared by name.
    pub const XRP: Self = Self::Standard(*b"XRP");

    /// Convert to the 20-byte binary representation used in XRPL serialization.
    pub fn to_bytes(&self) -> [u8; 20] {
        match self {
            Self::Standard(code) => {
                let mut bytes = [0u8; 20];
                bytes[12] = code[0];
                bytes[13] = code[1];
                bytes[14] = code[2];
                bytes
            }
            Self::NonStandard(bytes) => *bytes,
        }
    }

    /// Parse from 20-byte binary representation.
    pub fn from_bytes(bytes: [u8; 20]) -> Self {
        // Check if it's a standard code: bytes 0-11 and 15-19 are zero,
        // and bytes 12-14 are ASCII printable.
        let is_standard = bytes[..12].iter().all(|&b| b == 0)
            && bytes[15..].iter().all(|&b| b == 0)
            && bytes[12..15].iter().all(|&b| b.is_ascii_graphic());

        if is_standard {
            Self::Standard([bytes[12], bytes[13], bytes[14]])
        } else {
            Self::NonStandard(bytes)
        }
    }

    /// Returns true if this is the XRP currency (all zeros or "XRP").
    pub fn is_xrp(&self) -> bool {
        match self {
            Self::Standard(code) => code == b"XRP",
            Self::NonStandard(bytes) => bytes.iter().all(|&b| b == 0),
        }
    }
}

impl fmt::Debug for CurrencyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Standard(code) => {
                write!(
                    f,
                    "CurrencyCode({})",
                    std::str::from_utf8(code).unwrap_or("???")
                )
            }
            Self::NonStandard(bytes) => write!(f, "CurrencyCode({})", hex::encode(bytes)),
        }
    }
}

impl fmt::Display for CurrencyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Standard(code) => {
                write!(f, "{}", std::str::from_utf8(code).unwrap_or("???"))
            }
            Self::NonStandard(bytes) => write!(f, "{}", hex::encode_upper(bytes)),
        }
    }
}

impl FromStr for CurrencyCode {
    type Err = PrimitivesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() == 3 && s.is_ascii() {
            let bytes: [u8; 3] = s.as_bytes().try_into().unwrap();
            Ok(Self::Standard(bytes))
        } else if s.len() == 40 {
            let decoded =
                hex::decode(s).map_err(|e| PrimitivesError::InvalidCurrency(e.to_string()))?;
            let bytes: [u8; 20] = decoded
                .try_into()
                .map_err(|_| PrimitivesError::InvalidCurrency(s.to_string()))?;
            Ok(Self::NonStandard(bytes))
        } else {
            Err(PrimitivesError::InvalidCurrency(s.to_string()))
        }
    }
}

impl Serialize for CurrencyCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CurrencyCode {
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
    fn standard_currency_roundtrip() {
        let usd = CurrencyCode::from_str("USD").unwrap();
        assert_eq!(usd.to_string(), "USD");

        let bytes = usd.to_bytes();
        let decoded = CurrencyCode::from_bytes(bytes);
        assert_eq!(usd, decoded);
    }

    #[test]
    fn xrp_detection() {
        let xrp = CurrencyCode::XRP;
        assert!(xrp.is_xrp());

        let usd = CurrencyCode::from_str("USD").unwrap();
        assert!(!usd.is_xrp());
    }

    #[test]
    fn non_standard_currency() {
        let hex_str = "0158415500000000C1F76FF6ECB0BAC600000000";
        let currency = CurrencyCode::from_str(hex_str).unwrap();
        assert!(matches!(currency, CurrencyCode::NonStandard(_)));
        assert_eq!(currency.to_string(), hex_str.to_uppercase());
    }

    #[test]
    fn binary_representation() {
        let usd = CurrencyCode::from_str("USD").unwrap();
        let bytes = usd.to_bytes();
        assert_eq!(bytes[12], b'U');
        assert_eq!(bytes[13], b'S');
        assert_eq!(bytes[14], b'D');
        assert!(bytes[..12].iter().all(|&b| b == 0));
        assert!(bytes[15..].iter().all(|&b| b == 0));
    }
}
