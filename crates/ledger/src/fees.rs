//! Fee settings extraction from a ledger's state map.
//!
//! Reads the `FeeSettings` ledger entry and provides typed access to
//! base fee, reserve base, and reserve increment values.

use rxrpl_protocol::keylet;
use rxrpl_protocol::ledger::LedgerObjectKind;
use rxrpl_shamap::SHAMap;

/// Fee parameters extracted from a ledger's FeeSettings entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerFees {
    /// Base transaction fee in drops.
    pub base_fee: u64,
    /// Base reserve requirement in drops.
    pub reserve_base: u64,
    /// Per-object reserve increment in drops.
    pub reserve_increment: u64,
}

impl Default for LedgerFees {
    fn default() -> Self {
        Self {
            base_fee: 10,
            reserve_base: 10_000_000,
            reserve_increment: 2_000_000,
        }
    }
}

impl LedgerFees {
    /// Read fee settings from the ledger state map.
    ///
    /// Uses `keylet::fee_settings()` to locate the FeeSettings entry.
    /// Returns default values if the entry is not found or cannot be parsed.
    pub fn from_ledger_state(map: &SHAMap) -> LedgerFees {
        let key = keylet::fee_settings();

        let Some(bytes) = map.get(&key) else {
            return LedgerFees::default();
        };

        let json_value = match crate::sle_codec::decode_state(bytes) {
            Ok(v) => v,
            Err(_) => return LedgerFees::default(),
        };
        let obj: LedgerObjectKind = match serde_json::from_value(json_value) {
            Ok(v) => v,
            Err(_) => return LedgerFees::default(),
        };

        match obj {
            LedgerObjectKind::FeeSettings(fs) => {
                let base_fee = fs
                    .base_fee
                    .as_deref()
                    .and_then(|s| u64::from_str_radix(s, 16).ok())
                    .unwrap_or(10);
                let reserve_base = fs.reserve_base.map(u64::from).unwrap_or(10_000_000);
                let reserve_increment = fs.reserve_increment.map(u64::from).unwrap_or(2_000_000);

                LedgerFees {
                    base_fee,
                    reserve_base,
                    reserve_increment,
                }
            }
            _ => LedgerFees::default(),
        }
    }

    /// Compute the total reserve for an account with `owner_count` owned objects.
    pub fn account_reserve(&self, owner_count: u32) -> u64 {
        self.reserve_base + self.reserve_increment * u64::from(owner_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_no_fee_settings_entry() {
        let map = SHAMap::account_state();
        let fees = LedgerFees::from_ledger_state(&map);

        assert_eq!(fees.base_fee, 10);
        assert_eq!(fees.reserve_base, 10_000_000);
        assert_eq!(fees.reserve_increment, 2_000_000);
    }

    #[test]
    fn account_reserve_calculation() {
        let fees = LedgerFees::default();
        assert_eq!(fees.account_reserve(0), 10_000_000);
        assert_eq!(fees.account_reserve(1), 12_000_000);
        assert_eq!(fees.account_reserve(5), 20_000_000);
    }
}
