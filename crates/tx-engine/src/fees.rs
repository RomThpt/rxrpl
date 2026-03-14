/// Fee settings for a ledger, derived from the FeeSettings ledger object.
#[derive(Clone, Debug)]
pub struct FeeSettings {
    /// Base fee in drops for a reference transaction.
    pub base_fee: u64,
    /// Reserve base in drops (account reserve).
    pub reserve_base: u64,
    /// Reserve increment in drops (per owner count).
    pub reserve_increment: u64,
}

impl FeeSettings {
    /// Account reserve: base + (owner_count * increment).
    pub fn account_reserve(&self, owner_count: u32) -> u64 {
        self.reserve_base + (owner_count as u64 * self.reserve_increment)
    }
}

impl Default for FeeSettings {
    fn default() -> Self {
        Self {
            base_fee: 10,
            reserve_base: 10_000_000, // 10 XRP
            reserve_increment: 2_000_000, // 2 XRP
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_fees() {
        let fees = FeeSettings::default();
        assert_eq!(fees.base_fee, 10);
        assert_eq!(fees.reserve_base, 10_000_000);
        assert_eq!(fees.reserve_increment, 2_000_000);
    }

    #[test]
    fn account_reserve_calculation() {
        let fees = FeeSettings::default();
        assert_eq!(fees.account_reserve(0), 10_000_000);
        assert_eq!(fees.account_reserve(1), 12_000_000);
        assert_eq!(fees.account_reserve(5), 20_000_000);
    }
}
