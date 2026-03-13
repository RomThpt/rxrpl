use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AccountRoot {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub account: String,
    pub balance: String,
    pub sequence: u32,
    pub owner_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_txn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regular_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tick_size: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nftoken_minter: Option<String>,
}

impl LedgerObject for AccountRoot {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::AccountRoot
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_account_root() {
        let json = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Balance": "1000000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0
        });
        let ar = AccountRoot::from_json(&json).unwrap();
        assert_eq!(ar.account, "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk");
        assert_eq!(ar.balance, "1000000000");
        assert_eq!(ar.sequence, 1);
    }
}
