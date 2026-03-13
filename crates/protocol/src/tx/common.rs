use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ProtocolError;
use crate::types::TransactionType;

/// Trait implemented by all transaction types.
pub trait Transaction: Serialize + for<'de> Deserialize<'de> + Clone + std::fmt::Debug {
    /// The static transaction type discriminant.
    fn transaction_type() -> TransactionType;

    /// Access the common fields shared by all transactions.
    fn common(&self) -> &CommonFields;

    /// Mutable access to the common fields.
    fn common_mut(&mut self) -> &mut CommonFields;

    /// Serialize to a JSON Value suitable for binary encoding.
    /// Injects the `TransactionType` field automatically.
    fn to_json(&self) -> Result<Value, ProtocolError> {
        let mut v = serde_json::to_value(self).map_err(ProtocolError::Json)?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "TransactionType".to_string(),
                Value::String(Self::transaction_type().as_str().to_string()),
            );
        }
        Ok(v)
    }

    /// Deserialize from a JSON Value.
    fn from_json(value: &Value) -> Result<Self, ProtocolError>
    where
        Self: Sized,
    {
        serde_json::from_value(value.clone()).map_err(ProtocolError::Json)
    }
}

/// Fields common to all XRPL transactions.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CommonFields {
    /// The classic address of the sender.
    pub account: String,

    /// The fee in drops.
    pub fee: String,

    /// The sequence number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u32>,

    /// Transaction flags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<u32>,

    /// Last valid ledger sequence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ledger_sequence: Option<u32>,

    /// Transaction memos.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memos: Option<Vec<Memo>>,

    /// Multi-signers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signers: Option<Vec<Signer>>,

    /// Source tag for identifying the originator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_tag: Option<u32>,

    /// Public key of the signer (hex).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing_pub_key: Option<String>,

    /// Use a ticket instead of sequence number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket_sequence: Option<u32>,

    /// The signature (hex).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub txn_signature: Option<String>,

    /// Network ID for sidechains.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_id: Option<u32>,

    /// Hash of the previous transaction from this account.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_txn_id: Option<String>,
}

impl CommonFields {
    pub fn new(account: impl Into<String>, fee: impl Into<String>) -> Self {
        Self {
            account: account.into(),
            fee: fee.into(),
            ..Default::default()
        }
    }
}

/// A memo attached to a transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Memo {
    pub memo: MemoInner,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MemoInner {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memo_type: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub memo_data: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub memo_format: Option<String>,
}

/// A signer in a multi-signed transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Signer {
    pub signer: SignerInner,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SignerInner {
    pub account: String,
    pub txn_signature: String,
    pub signing_pub_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_fields_default() {
        let cf = CommonFields::new("rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk", "12");
        assert_eq!(cf.account, "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk");
        assert_eq!(cf.fee, "12");
        assert!(cf.sequence.is_none());
    }

    #[test]
    fn memo_serde() {
        let memo = Memo {
            memo: MemoInner {
                memo_type: Some("746578742F706C61696E".to_string()),
                memo_data: Some("48656C6C6F".to_string()),
                memo_format: None,
            },
        };
        let json = serde_json::to_value(&memo).unwrap();
        assert_eq!(json["Memo"]["MemoType"], "746578742F706C61696E");
        let decoded: Memo = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.memo.memo_type, memo.memo.memo_type);
    }
}
