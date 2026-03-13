use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Amm {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub account: String,
    pub asset: Value,
    #[serde(rename = "Asset2")]
    pub asset2: Value,
    #[serde(rename = "LPTokenBalance")]
    pub lp_token_balance: Value,
    pub trading_fee: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auction_slot: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vote_slots: Option<Vec<Value>>,
}

impl LedgerObject for Amm {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::AMM
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "LedgerEntryType": "AMM",
            "Account": "rAMMAccount1111111111111111111",
            "Asset": { "currency": "XRP" },
            "Asset2": { "currency": "USD", "issuer": "rIssuer111111111111111111111" },
            "LPTokenBalance": {
                "value": "1000",
                "currency": "03930D02208264E2E40EC1B0C09E4DB96EE197B1",
                "issuer": "rAMMAccount1111111111111111111"
            },
            "TradingFee": 500
        });
        let obj: Amm = serde_json::from_value(json).unwrap();
        assert_eq!(obj.account, "rAMMAccount1111111111111111111");
        assert_eq!(obj.trading_fee, 500);
        let rt = serde_json::to_value(&obj).unwrap();
        assert_eq!(rt["TradingFee"], 500);
    }
}
