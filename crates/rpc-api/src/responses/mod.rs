use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Common fields in most responses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BaseResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_current_index: Option<u32>,
}

/// account_info response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountInfoResponse {
    pub account_data: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer_lists: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_data: Option<Value>,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// account_tx response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountTxResponse {
    pub account: String,
    pub transactions: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<Value>,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// account_lines response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountLinesResponse {
    pub account: String,
    pub lines: Vec<TrustLine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<Value>,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// Trust line entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustLine {
    pub account: String,
    pub balance: String,
    pub currency: String,
    pub limit: String,
    pub limit_peer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_ripple: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_ripple_peer: Option<bool>,
}

/// server_info response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerInfoResponse {
    pub info: ServerInfo,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// Server info details.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complete_ledgers: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validated_ledger: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_quorum: Option<u32>,
    #[serde(flatten)]
    pub extra: Value,
}

/// tx response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxResponse {
    #[serde(flatten)]
    pub tx: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validated: Option<bool>,
}

/// submit response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_result_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_result_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_blob: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_json: Option<Value>,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// fee response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeeResponse {
    pub current_ledger_size: Option<String>,
    pub current_queue_size: Option<String>,
    pub drops: Option<FeeDrops>,
    pub expected_ledger_size: Option<String>,
    pub ledger_current_index: Option<u32>,
    pub levels: Option<FeeLevels>,
    pub max_queue_size: Option<String>,
    #[serde(flatten)]
    pub base: BaseResponse,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeeDrops {
    pub base_fee: Option<String>,
    pub median_fee: Option<String>,
    pub minimum_fee: Option<String>,
    pub open_ledger_fee: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeeLevels {
    pub median_level: Option<String>,
    pub minimum_level: Option<String>,
    pub open_ledger_level: Option<String>,
    pub reference_level: Option<String>,
}

/// wallet_propose response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletProposeResponse {
    pub account_id: Option<String>,
    pub key_type: Option<String>,
    pub master_key: Option<String>,
    pub master_seed: Option<String>,
    pub master_seed_hex: Option<String>,
    pub public_key: Option<String>,
    pub public_key_hex: Option<String>,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// account_objects response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountObjectsResponse {
    pub account: String,
    pub account_objects: Vec<AccountObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<Value>,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// A single ledger object owned by an account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountObject {
    #[serde(rename = "LedgerEntryType")]
    pub ledger_entry_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    #[serde(flatten)]
    pub fields: Value,
}

/// account_offers response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountOffersResponse {
    pub account: String,
    pub offers: Vec<AccountOffer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<Value>,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// A single offer owned by an account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountOffer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u32>,
    pub taker_gets: Value,
    pub taker_pays: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration: Option<u32>,
}

/// account_currencies response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountCurrenciesResponse {
    pub receive_currencies: Vec<String>,
    pub send_currencies: Vec<String>,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// account_nfts response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountNftsResponse {
    pub account: String,
    pub account_nfts: Vec<AccountNft>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<Value>,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// A single NFT owned by an account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountNft {
    #[serde(rename = "Flags")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<u32>,
    #[serde(rename = "Issuer")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(rename = "NFTokenID")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nftoken_id: Option<String>,
    #[serde(rename = "NFTokenTaxon")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nftoken_taxon: Option<u32>,
    #[serde(rename = "URI")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

/// book_offers response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BookOffersResponse {
    pub offers: Vec<BookOffer>,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// A single order book offer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BookOffer {
    #[serde(rename = "Account")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(rename = "TakerGets")]
    pub taker_gets: Value,
    #[serde(rename = "TakerPays")]
    pub taker_pays: Value,
    #[serde(rename = "Sequence")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    #[serde(rename = "Flags")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<u32>,
    #[serde(flatten)]
    pub extra: Value,
}

/// ledger_closed response.
///
/// Does NOT flatten `BaseResponse` because its `ledger_hash` / `ledger_index`
/// fields overlap. We include the relevant fields directly instead.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerClosedResponse {
    pub ledger_hash: String,
    pub ledger_index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validated: Option<bool>,
}

/// ledger response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerResponse {
    pub ledger: LedgerInfo,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// Ledger details.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close_time: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_index: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_coins: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transactions: Option<Vec<Value>>,
    #[serde(flatten)]
    pub extra: Value,
}

/// amm_info response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AmmInfoResponse {
    pub amm: Value,
    #[serde(flatten)]
    pub base: BaseResponse,
}

/// Subscription event types.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SubscriptionEvent {
    #[serde(rename = "ledgerClosed")]
    LedgerClosed(LedgerClosedEvent),
    #[serde(rename = "transaction")]
    Transaction(TransactionEvent),
    #[serde(other)]
    Unknown,
}

/// Ledger closed event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerClosedEvent {
    pub fee_base: Option<u64>,
    pub fee_ref: Option<u64>,
    pub ledger_hash: Option<String>,
    pub ledger_index: Option<u32>,
    pub ledger_time: Option<u64>,
    pub reserve_base: Option<u64>,
    pub reserve_inc: Option<u64>,
    pub txn_count: Option<u32>,
    pub validated_ledgers: Option<String>,
}

/// Transaction subscription event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransactionEvent {
    pub engine_result: Option<String>,
    pub engine_result_code: Option<i32>,
    pub engine_result_message: Option<String>,
    pub ledger_hash: Option<String>,
    pub ledger_index: Option<u32>,
    pub transaction: Option<Value>,
    pub meta: Option<Value>,
    pub validated: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_server_info_response() {
        let json = r#"{
            "info": {
                "build_version": "1.9.4",
                "complete_ledgers": "32570-75000000",
                "server_state": "full"
            },
            "status": "success"
        }"#;
        let resp: ServerInfoResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.info.build_version.unwrap(), "1.9.4");
        assert_eq!(resp.base.status.unwrap(), "success");
    }

    #[test]
    fn deserialize_account_objects_response() {
        let json = r#"{
            "account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "account_objects": [
                {
                    "LedgerEntryType": "RippleState",
                    "index": "ABC123",
                    "Balance": { "value": "100" }
                }
            ],
            "status": "success",
            "validated": true
        }"#;
        let resp: AccountObjectsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.account, "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh");
        assert_eq!(resp.account_objects.len(), 1);
        assert_eq!(resp.account_objects[0].ledger_entry_type, "RippleState");
        assert_eq!(resp.account_objects[0].index.as_deref(), Some("ABC123"));
    }

    #[test]
    fn deserialize_account_offers_response() {
        let json = r#"{
            "account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "offers": [
                {
                    "flags": 0,
                    "seq": 1,
                    "taker_gets": "1000000",
                    "taker_pays": { "currency": "USD", "issuer": "rIssuer", "value": "10" },
                    "quality": "0.00001"
                }
            ],
            "status": "success"
        }"#;
        let resp: AccountOffersResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.offers.len(), 1);
        assert_eq!(resp.offers[0].seq, Some(1));
    }

    #[test]
    fn deserialize_account_currencies_response() {
        let json = r#"{
            "receive_currencies": ["USD", "EUR"],
            "send_currencies": ["BTC"],
            "status": "success"
        }"#;
        let resp: AccountCurrenciesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.receive_currencies, vec!["USD", "EUR"]);
        assert_eq!(resp.send_currencies, vec!["BTC"]);
    }

    #[test]
    fn deserialize_account_nfts_response() {
        let json = r#"{
            "account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "account_nfts": [
                {
                    "Flags": 8,
                    "Issuer": "rIssuer",
                    "NFTokenID": "000800006203F49C21D5D6E022CB16DE3538F248662FC73C00000001",
                    "NFTokenTaxon": 0,
                    "URI": "68747470733A2F2F"
                }
            ],
            "status": "success"
        }"#;
        let resp: AccountNftsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.account_nfts.len(), 1);
        assert_eq!(resp.account_nfts[0].flags, Some(8));
        assert!(resp.account_nfts[0].nftoken_id.is_some());
    }

    #[test]
    fn deserialize_book_offers_response() {
        let json = r#"{
            "offers": [
                {
                    "Account": "rSeller",
                    "TakerGets": "5000000",
                    "TakerPays": { "currency": "USD", "issuer": "rIssuer", "value": "5" },
                    "Sequence": 42,
                    "quality": "0.000001",
                    "Flags": 0
                }
            ],
            "status": "success"
        }"#;
        let resp: BookOffersResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.offers.len(), 1);
        assert_eq!(resp.offers[0].account.as_deref(), Some("rSeller"));
        assert_eq!(resp.offers[0].sequence, Some(42));
    }

    #[test]
    fn deserialize_ledger_closed_response() {
        let json = r#"{
            "ledger_hash": "ABCD1234",
            "ledger_index": 75000000,
            "status": "success"
        }"#;
        let resp: LedgerClosedResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.ledger_hash, "ABCD1234");
        assert_eq!(resp.ledger_index, 75000000);
        assert_eq!(resp.status.as_deref(), Some("success"));
    }

    #[test]
    fn deserialize_ledger_response() {
        let json = r#"{
            "ledger": {
                "accepted": true,
                "close_time": 700000000,
                "hash": "LEDGERHASH",
                "ledger_index": "75000000",
                "total_coins": "99999999999999990"
            },
            "status": "success",
            "validated": true
        }"#;
        let resp: LedgerResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.ledger.accepted, Some(true));
        assert_eq!(resp.ledger.hash.as_deref(), Some("LEDGERHASH"));
        assert_eq!(resp.ledger.ledger_index.as_deref(), Some("75000000"));
        assert_eq!(resp.base.validated, Some(true));
    }

    #[test]
    fn deserialize_amm_info_response() {
        let json = r#"{
            "amm": {
                "account": "rAMMAccount1111111111111111111",
                "trading_fee": 500,
                "lp_token": {
                    "value": "1000",
                    "currency": "03930D02208264E2E40EC1B0C09E4DB96EE197B1",
                    "issuer": "rAMMAccount1111111111111111111"
                }
            },
            "status": "success"
        }"#;
        let resp: AmmInfoResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.amm["account"], "rAMMAccount1111111111111111111");
        assert_eq!(resp.base.status.as_deref(), Some("success"));
    }

    #[test]
    fn deserialize_subscription_event_ledger() {
        let json = r#"{
            "type": "ledgerClosed",
            "fee_base": 10,
            "ledger_hash": "ABC123",
            "ledger_index": 12345,
            "ledger_time": 700000000,
            "txn_count": 5
        }"#;
        let event: SubscriptionEvent = serde_json::from_str(json).unwrap();
        match event {
            SubscriptionEvent::LedgerClosed(e) => {
                assert_eq!(e.ledger_index, Some(12345));
            }
            _ => panic!("expected LedgerClosed"),
        }
    }
}
