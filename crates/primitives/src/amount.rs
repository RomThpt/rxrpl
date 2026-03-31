use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::account_id::AccountId;
use crate::currency::CurrencyCode;
use crate::error::PrimitivesError;

/// Maximum XRP supply in drops (100 billion XRP).
const MAX_XRP_DROPS: i64 = 100_000_000_000_000_000;

/// Drops per XRP.
const DROPS_PER_XRP: i64 = 1_000_000;

/// XRP amount in drops. 1 XRP = 1,000,000 drops.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct XrpAmount(pub i64);

impl XrpAmount {
    pub const ZERO: Self = Self(0);

    pub fn from_drops(drops: i64) -> Result<Self, PrimitivesError> {
        if drops.abs() > MAX_XRP_DROPS {
            return Err(PrimitivesError::InvalidAmount(format!(
                "XRP amount {drops} exceeds maximum"
            )));
        }
        Ok(Self(drops))
    }

    pub fn from_xrp(xrp: i64) -> Result<Self, PrimitivesError> {
        let drops = xrp
            .checked_mul(DROPS_PER_XRP)
            .ok_or(PrimitivesError::Overflow)?;
        Self::from_drops(drops)
    }

    pub fn drops(&self) -> i64 {
        self.0
    }

    pub fn is_negative(&self) -> bool {
        self.0 < 0
    }

    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }

    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.0.checked_add(rhs.0).and_then(|v| {
            if v.abs() <= MAX_XRP_DROPS {
                Some(Self(v))
            } else {
                None
            }
        })
    }

    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.0.checked_sub(rhs.0).and_then(|v| {
            if v.abs() <= MAX_XRP_DROPS {
                Some(Self(v))
            } else {
                None
            }
        })
    }
}

impl fmt::Debug for XrpAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "XrpAmount({} drops)", self.0)
    }
}

impl fmt::Display for XrpAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for XrpAmount {
    type Err = PrimitivesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let drops: i64 = s
            .parse()
            .map_err(|_| PrimitivesError::InvalidAmount(s.to_string()))?;
        Self::from_drops(drops)
    }
}

/// IOU/token amount with mantissa-exponent representation.
///
/// XRPL uses a custom decimal format: value = mantissa * 10^exponent
/// Mantissa: up to 16 significant digits
/// Exponent: -96 to +80
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedAmount {
    /// The string representation of the value (mantissa * 10^exponent).
    /// Stored as string to preserve exact decimal representation.
    pub value: String,
    pub currency: CurrencyCode,
    pub issuer: AccountId,
}

impl IssuedAmount {
    pub fn new(value: impl Into<String>, currency: CurrencyCode, issuer: AccountId) -> Self {
        Self {
            value: value.into(),
            currency,
            issuer,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.value == "0" || self.value == "0.0"
    }
}

impl fmt::Display for IssuedAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.value, self.currency, self.issuer)
    }
}

/// An XRPL amount -- either XRP (native) or an issued currency/token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Amount {
    Xrp(XrpAmount),
    Issued(IssuedAmount),
}

impl Amount {
    pub fn xrp(drops: i64) -> Result<Self, PrimitivesError> {
        Ok(Self::Xrp(XrpAmount::from_drops(drops)?))
    }

    pub fn issued(value: impl Into<String>, currency: CurrencyCode, issuer: AccountId) -> Self {
        Self::Issued(IssuedAmount::new(value, currency, issuer))
    }

    pub fn is_xrp(&self) -> bool {
        matches!(self, Self::Xrp(_))
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xrp(a) => write!(f, "{a} drops"),
            Self::Issued(a) => write!(f, "{a}"),
        }
    }
}

impl Serialize for Amount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Xrp(a) => serializer.serialize_str(&a.to_string()),
            Self::Issued(a) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("value", &a.value)?;
                map.serialize_entry("currency", &a.currency)?;
                map.serialize_entry("issuer", &a.issuer)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde_json::Value;
        let v = Value::deserialize(deserializer)?;
        match v {
            Value::String(s) => {
                let xrp = XrpAmount::from_str(&s).map_err(serde::de::Error::custom)?;
                Ok(Self::Xrp(xrp))
            }
            Value::Object(map) => {
                let value = map
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| serde::de::Error::missing_field("value"))?
                    .to_string();
                let currency_str = map
                    .get("currency")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| serde::de::Error::missing_field("currency"))?;
                let currency =
                    CurrencyCode::from_str(currency_str).map_err(serde::de::Error::custom)?;
                let issuer_str = map
                    .get("issuer")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| serde::de::Error::missing_field("issuer"))?;
                let issuer = AccountId::from_str(issuer_str).map_err(serde::de::Error::custom)?;
                Ok(Self::Issued(IssuedAmount {
                    value,
                    currency,
                    issuer,
                }))
            }
            _ => Err(serde::de::Error::custom(
                "expected string or object for Amount",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrp_amount_from_drops() {
        let amount = XrpAmount::from_drops(1_000_000).unwrap();
        assert_eq!(amount.drops(), 1_000_000);
        assert_eq!(amount.to_string(), "1000000");
    }

    #[test]
    fn xrp_amount_overflow() {
        assert!(XrpAmount::from_drops(MAX_XRP_DROPS + 1).is_err());
    }

    #[test]
    fn xrp_amount_arithmetic() {
        let a = XrpAmount::from_drops(100).unwrap();
        let b = XrpAmount::from_drops(200).unwrap();
        let sum = a.checked_add(b).unwrap();
        assert_eq!(sum.drops(), 300);

        let diff = b.checked_sub(a).unwrap();
        assert_eq!(diff.drops(), 100);
    }

    #[test]
    fn xrp_amount_negative() {
        let a = XrpAmount::from_drops(-500).unwrap();
        assert!(a.is_negative());
    }

    #[test]
    fn amount_xrp_serde() {
        let amount = Amount::xrp(1_000_000).unwrap();
        let json = serde_json::to_string(&amount).unwrap();
        assert_eq!(json, "\"1000000\"");

        let decoded: Amount = serde_json::from_str(&json).unwrap();
        assert_eq!(amount, decoded);
    }

    #[test]
    fn amount_issued_serde() {
        let issuer = AccountId::from_str("88a5a57c829f40f25ea83385bbde6c3d8b4ca082").unwrap();
        let amount = Amount::issued("100.50", CurrencyCode::from_str("USD").unwrap(), issuer);

        let json = serde_json::to_string(&amount).unwrap();
        let decoded: Amount = serde_json::from_str(&json).unwrap();
        assert_eq!(amount, decoded);
    }

    #[test]
    fn xrp_from_xrp() {
        let amount = XrpAmount::from_xrp(1).unwrap();
        assert_eq!(amount.drops(), 1_000_000);
    }

    // --- Edge case tests ---

    #[test]
    fn xrp_negative_add() {
        let a = XrpAmount::from_drops(100).unwrap();
        let b = XrpAmount::from_drops(-50).unwrap();
        let result = a.checked_add(b).unwrap();
        assert_eq!(result.drops(), 50);
    }

    #[test]
    fn xrp_max_supply_overflow() {
        let max = XrpAmount::from_drops(MAX_XRP_DROPS).unwrap();
        let one = XrpAmount::from_drops(1).unwrap();
        assert!(max.checked_add(one).is_none());
    }

    #[test]
    fn xrp_underflow() {
        let a = XrpAmount::from_drops(-MAX_XRP_DROPS).unwrap();
        let b = XrpAmount::from_drops(-1).unwrap();
        assert!(a.checked_add(b).is_none());
    }

    #[test]
    fn xrp_sub_overflow() {
        let a = XrpAmount::from_drops(-MAX_XRP_DROPS).unwrap();
        let b = XrpAmount::from_drops(1).unwrap();
        assert!(a.checked_sub(b).is_none());
    }

    #[test]
    fn xrp_zero_operations() {
        let zero = XrpAmount::ZERO;
        let a = XrpAmount::from_drops(100).unwrap();
        assert_eq!(zero.checked_add(a).unwrap().drops(), 100);
        assert_eq!(a.checked_sub(a).unwrap().drops(), 0);
    }

    #[test]
    fn xrp_from_xrp_overflow() {
        // i64::MAX / 1_000_000 will overflow when multiplied back
        assert!(XrpAmount::from_xrp(i64::MAX).is_err());
    }

    #[test]
    fn xrp_negative_from_drops() {
        let neg = XrpAmount::from_drops(-1_000_000).unwrap();
        assert!(neg.is_negative());
        assert_eq!(neg.drops(), -1_000_000);
    }
}
