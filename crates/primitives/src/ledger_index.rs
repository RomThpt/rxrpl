use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A ledger sequence number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct LedgerIndex(pub u32);

impl LedgerIndex {
    pub fn new(index: u32) -> Self {
        Self(index)
    }

    pub fn as_u32(&self) -> u32 {
        self.0
    }
}

impl fmt::Display for LedgerIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u32> for LedgerIndex {
    fn from(v: u32) -> Self {
        Self(v)
    }
}

/// A ledger specifier that can be either a numeric index or a named shortcut.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LedgerSpecifier {
    Index(u32),
    Shortcut(LedgerShortcut),
    Hash(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LedgerShortcut {
    Validated,
    Current,
    Closed,
}

impl FromStr for LedgerSpecifier {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "validated" => Ok(Self::Shortcut(LedgerShortcut::Validated)),
            "current" => Ok(Self::Shortcut(LedgerShortcut::Current)),
            "closed" => Ok(Self::Shortcut(LedgerShortcut::Closed)),
            _ => {
                if let Ok(idx) = s.parse::<u32>() {
                    Ok(Self::Index(idx))
                } else if s.len() == 64 {
                    Ok(Self::Hash(s.to_string()))
                } else {
                    Err(format!("invalid ledger specifier: {s}"))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_specifier_variants() {
        assert_eq!(
            LedgerSpecifier::from_str("validated").unwrap(),
            LedgerSpecifier::Shortcut(LedgerShortcut::Validated)
        );
        assert_eq!(
            LedgerSpecifier::from_str("12345").unwrap(),
            LedgerSpecifier::Index(12345)
        );
    }
}
