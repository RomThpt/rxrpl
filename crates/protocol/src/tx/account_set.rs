use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An AccountSet transaction modifies the properties of an account.
    AccountSet => TransactionType::AccountSet,
    {
        #[serde(skip_serializing_if = "Option::is_none")]
        "ClearFlag" => clear_flag: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Domain" => domain: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "EmailHash" => email_hash: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "MessageKey" => message_key: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "SetFlag" => set_flag: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "TransferRate" => transfer_rate: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "TickSize" => tick_size: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "NFTokenMinter" => nftoken_minter: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "WalletLocator" => wallet_locator: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "WalletSize" => wallet_size: Option<u32>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "AccountSet",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "SetFlag": 8
        });
        let tx = AccountSet::from_json(&json).unwrap();
        assert_eq!(tx.set_flag, Some(8));
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["SetFlag"], 8);
    }
}
