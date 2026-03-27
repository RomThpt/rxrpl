use rxrpl_ledger::Ledger;
use rxrpl_primitives::Hash256;

use crate::fees::FeeSettings;
use crate::view::read_view::ReadView;

/// Read-only view backed by a `Ledger`.
pub struct LedgerView<'a> {
    ledger: &'a Ledger,
    fees: FeeSettings,
}

impl<'a> LedgerView<'a> {
    pub fn new(ledger: &'a Ledger) -> Self {
        Self {
            fees: FeeSettings::default(),
            ledger,
        }
    }

    pub fn with_fees(ledger: &'a Ledger, fees: FeeSettings) -> Self {
        Self { ledger, fees }
    }
}

impl ReadView for LedgerView<'_> {
    fn read(&self, key: &Hash256) -> Option<Vec<u8>> {
        let raw = self.ledger.get_state(key)?;
        match rxrpl_ledger::sle_codec::decode_sle(raw) {
            Ok(json_bytes) => Some(json_bytes),
            Err(_) => Some(raw.to_vec()),
        }
    }

    fn exists(&self, key: &Hash256) -> bool {
        self.ledger.has_state(key)
    }

    fn seq(&self) -> u32 {
        self.ledger.header.sequence
    }

    fn fees(&self) -> &FeeSettings {
        &self.fees
    }

    fn drops(&self) -> u64 {
        self.ledger.header.drops
    }

    fn parent_close_time(&self) -> u32 {
        self.ledger.header.parent_close_time
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_view_reads_state() {
        let mut ledger = Ledger::genesis();
        let key = Hash256::new([0xAA; 32]);
        ledger.put_state(key, vec![1, 2, 3]).unwrap();

        let view = LedgerView::new(&ledger);
        assert_eq!(view.read(&key), Some(vec![1, 2, 3]));
        assert!(view.exists(&key));
        assert_eq!(view.seq(), 1);
    }

    #[test]
    fn ledger_view_missing_key() {
        let ledger = Ledger::genesis();
        let view = LedgerView::new(&ledger);
        let key = Hash256::new([0xBB; 32]);
        assert_eq!(view.read(&key), None);
        assert!(!view.exists(&key));
    }
}
