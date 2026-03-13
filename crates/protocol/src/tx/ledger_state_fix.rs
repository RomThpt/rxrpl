use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A LedgerStateFix transaction repairs ledger state inconsistencies.
    LedgerStateFix => TransactionType::LedgerStateFix,
    {
        "LedgerFixType" => ledger_fix_type: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Owner" => owner: Option<String>
    }
}
