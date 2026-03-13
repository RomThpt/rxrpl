use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ProtocolError;
use crate::types::LedgerEntryType;

/// Trait implemented by all ledger object types.
pub trait LedgerObject: Serialize + for<'de> Deserialize<'de> + Clone + std::fmt::Debug {
    fn ledger_entry_type() -> LedgerEntryType;
    fn common(&self) -> &CommonLedgerFields;

    fn to_json(&self) -> Result<Value, ProtocolError> {
        serde_json::to_value(self).map_err(ProtocolError::Json)
    }

    fn from_json(value: &Value) -> Result<Self, ProtocolError>
    where
        Self: Sized,
    {
        serde_json::from_value(value.clone()).map_err(ProtocolError::Json)
    }
}

/// Fields common to all ledger entries.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CommonLedgerFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<u32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_txn_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "PreviousTxnLgrSeq")]
    pub previous_txn_lgr_seq: Option<u32>,
}
