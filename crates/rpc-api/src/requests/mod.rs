use serde::{Deserialize, Serialize};
use serde_json::Value;

/// account_info request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountInfoRequest {
    pub account: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer_lists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<bool>,
}

/// account_tx request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountTxRequest {
    pub account: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index_min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index_max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forward: Option<bool>,
}

/// account_lines request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountLinesRequest {
    pub account: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<Value>,
}

/// account_objects request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountObjectsRequest {
    pub account: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<Value>,
}

/// account_offers request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountOffersRequest {
    pub account: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<Value>,
}

/// ledger request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transactions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expand: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounts: Option<bool>,
}

/// ledger_entry request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerEntryRequest {
    #[serde(flatten)]
    pub entry_type: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_hash: Option<String>,
}

/// tx request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxRequest {
    pub transaction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_ledger: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_ledger: Option<u32>,
}

/// submit request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitRequest {
    pub tx_blob: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail_hard: Option<bool>,
}

/// server_info request (no params).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServerInfoRequest {}

/// fee request (no params).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FeeRequest {}

/// subscribe request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubscribeRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streams: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounts_proposed: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub books: Option<Vec<Value>>,
}

/// unsubscribe request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnsubscribeRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streams: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounts: Option<Vec<String>>,
}

/// wallet_propose request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletProposeRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
}

/// book_offers request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BookOffersRequest {
    pub taker_gets: Value,
    pub taker_pays: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index: Option<Value>,
}

/// channel_authorize request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelAuthorizeRequest {
    pub channel_id: String,
    pub amount: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_type: Option<String>,
}

/// channel_verify request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelVerifyRequest {
    pub channel_id: String,
    pub amount: String,
    pub public_key: String,
    pub signature: String,
}

/// deposit_authorized request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DepositAuthorizedRequest {
    pub source_account: String,
    pub destination_account: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index: Option<Value>,
}

/// nft_buy_offers / nft_sell_offers request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NftOffersRequest {
    pub nft_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<Value>,
}

/// ping request (no params).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PingRequest {}

/// random request (no params).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RandomRequest {}
