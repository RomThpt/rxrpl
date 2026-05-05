use std::collections::VecDeque;
use std::sync::Arc;

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_config::ServerConfig;
use rxrpl_ledger::Ledger;
use rxrpl_protocol::keylet;
use rxrpl_rpc_server::ServerContext;
use rxrpl_tx_engine::{FeeSettings, TransactorRegistry, TxEngine};
use serde_json::{Value, json};
use tokio::sync::RwLock;

const GENESIS_ADDR: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

fn make_engine() -> TxEngine {
    let mut registry = TransactorRegistry::new();
    rxrpl_tx_engine::handlers::register_phase_a(&mut registry);
    TxEngine::new_without_sig_check(registry)
}

fn genesis_funded_ledger() -> Ledger {
    let mut genesis = Ledger::genesis();

    let account_id = decode_account_id(GENESIS_ADDR).unwrap();
    let key = keylet::account(&account_id);

    let account = json!({
        "LedgerEntryType": "AccountRoot",
        "Account": GENESIS_ADDR,
        "Balance": genesis.header.drops.to_string(),
        "Sequence": 1,
        "OwnerCount": 0,
        "Flags": 0,
    });
    let data = serde_json::to_vec(&account).unwrap();
    genesis.put_state(key, data).unwrap();
    genesis.close(0, 0).unwrap();

    Ledger::new_open(&genesis)
}

fn test_ctx_with_ledger() -> Arc<ServerContext> {
    let ledger = genesis_funded_ledger();
    let engine = make_engine();
    let fees = FeeSettings::default();

    let mut closed = VecDeque::new();
    // Add the genesis as a closed ledger
    let genesis_closed = {
        let mut g = Ledger::genesis();
        let account_id = decode_account_id(GENESIS_ADDR).unwrap();
        let key = keylet::account(&account_id);
        let account = json!({
            "LedgerEntryType": "AccountRoot",
            "Account": GENESIS_ADDR,
            "Balance": g.header.drops.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        let data = serde_json::to_vec(&account).unwrap();
        g.put_state(key, data).unwrap();
        g.close(0, 0).unwrap();
        g
    };
    closed.push_back(genesis_closed);

    ServerContext::with_node_state(
        ServerConfig::default(),
        Arc::new(RwLock::new(ledger)),
        Arc::new(RwLock::new(closed)),
        Arc::new(engine),
        Arc::new(fees),
        None,
        None,
        None,
    )
}

// -- account_info tests --

#[tokio::test]
async fn account_info_existing_account() {
    let ctx = test_ctx_with_ledger();
    let params = json!({ "account": GENESIS_ADDR });

    let result = rxrpl_rpc_server::handlers::account_info(params, &ctx)
        .await
        .unwrap();

    assert!(result["account_data"]["Balance"].as_str().is_some());
    assert_eq!(result["account_data"]["Account"], GENESIS_ADDR);
    assert_eq!(result["ledger_current_index"], 2);
}

#[tokio::test]
async fn account_info_nonexistent_account() {
    use rxrpl_rpc_server::RpcServerError;
    let ctx = test_ctx_with_ledger();
    let params = json!({ "account": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe" });

    let err = rxrpl_rpc_server::handlers::account_info(params, &ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, RpcServerError::AccountNotFound));
    assert_eq!(err.token(), "actNotFound");
    assert_eq!(err.numeric_code(), 19);
}

#[tokio::test]
#[allow(non_snake_case)]
async fn account_info_malformed_account_returns_token_actMalformed() {
    use rxrpl_rpc_server::RpcServerError;
    let ctx = test_ctx_with_ledger();
    let params = json!({ "account": "not_a_valid_address" });

    let err = rxrpl_rpc_server::handlers::account_info(params, &ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, RpcServerError::AccountMalformed));
    assert_eq!(err.token(), "actMalformed");
    assert_eq!(err.numeric_code(), 35);
}

#[tokio::test]
async fn account_info_missing_param() {
    let ctx = test_ctx_with_ledger();
    let params = json!({});

    let result = rxrpl_rpc_server::handlers::account_info(params, &ctx).await;
    assert!(result.is_err());
}

// -- ledger tests --

#[tokio::test]
async fn ledger_current() {
    let ctx = test_ctx_with_ledger();
    let params = json!({ "ledger_index": "current" });

    let result = rxrpl_rpc_server::handlers::ledger(params, &ctx)
        .await
        .unwrap();

    assert_eq!(result["ledger"]["ledger_index"], 2);
    assert_eq!(result["ledger"]["closed"], false);
}

#[tokio::test]
async fn ledger_closed() {
    let ctx = test_ctx_with_ledger();
    let params = json!({ "ledger_index": "closed" });

    let result = rxrpl_rpc_server::handlers::ledger(params, &ctx)
        .await
        .unwrap();

    assert_eq!(result["ledger"]["ledger_index"], 1);
    assert_eq!(result["ledger"]["closed"], true);
}

#[tokio::test]
async fn ledger_by_index() {
    let ctx = test_ctx_with_ledger();
    let params = json!({ "ledger_index": "1" });

    let result = rxrpl_rpc_server::handlers::ledger(params, &ctx)
        .await
        .unwrap();

    assert_eq!(result["ledger"]["ledger_index"], 1);
}

#[tokio::test]
async fn ledger_not_found() {
    use rxrpl_rpc_server::RpcServerError;
    let ctx = test_ctx_with_ledger();
    let params = json!({ "ledger_index": "999" });

    let err = rxrpl_rpc_server::handlers::ledger(params, &ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, RpcServerError::LedgerNotFound));
    assert_eq!(err.token(), "lgrNotFound");
}

#[tokio::test]
async fn ledger_not_found_with_numeric_index() {
    use rxrpl_rpc_server::RpcServerError;
    let ctx = test_ctx_with_ledger();
    // rippled accepts ledger_index as a JSON number; rxrpl must as well
    // and must error rather than silently fall back to current.
    let params = json!({ "ledger_index": 999_999_999u64 });

    let err = rxrpl_rpc_server::handlers::ledger(params, &ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, RpcServerError::LedgerNotFound));
}

// -- ledger_closed tests --

#[tokio::test]
async fn ledger_closed_handler() {
    let ctx = test_ctx_with_ledger();
    let params = json!({});

    let result = rxrpl_rpc_server::handlers::ledger_closed(params, &ctx)
        .await
        .unwrap();

    assert_eq!(result["ledger_index"], 1);
    assert!(result["ledger_hash"].as_str().is_some());
}

// -- fee tests --

#[tokio::test]
async fn fee_handler() {
    let ctx = test_ctx_with_ledger();
    let params = json!({});

    let result = rxrpl_rpc_server::handlers::fee(params, &ctx).await.unwrap();

    assert_eq!(result["drops"]["base_fee"], "10");
    assert_eq!(result["ledger_current_index"], 2);
}

// -- server_info tests --

#[tokio::test]
async fn server_info_with_ledger() {
    let ctx = test_ctx_with_ledger();
    let params = json!({});

    let result = rxrpl_rpc_server::handlers::server_info(params, &ctx)
        .await
        .unwrap();

    assert_eq!(result["info"]["complete_ledgers"], "1-1");
    assert_eq!(result["info"]["ledger_current_index"], 2);
}

// -- ledger_current tests --

#[tokio::test]
async fn ledger_current_handler() {
    let ctx = test_ctx_with_ledger();
    let params = json!({});

    let result = rxrpl_rpc_server::handlers::ledger_current(params, &ctx)
        .await
        .unwrap();

    assert_eq!(result["ledger_current_index"], 2);
}

// -- tx tests --

#[tokio::test]
async fn tx_not_found() {
    let ctx = test_ctx_with_ledger();
    let hash = "0".repeat(64);
    let params = json!({ "transaction": hash });

    let result = rxrpl_rpc_server::handlers::tx(params, &ctx).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn tx_invalid_hash() {
    use rxrpl_rpc_server::RpcServerError;
    let ctx = test_ctx_with_ledger();
    let params = json!({ "transaction": "not-a-hash" });

    let err = rxrpl_rpc_server::handlers::tx(params, &ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, RpcServerError::NotImplemented));
    assert_eq!(err.token(), "notImpl");
}

// -- domain attestation RPC tests (F-B4) --

#[tokio::test]
async fn server_info_includes_domain_verification_when_attached() {
    let mut ctx = test_ctx_with_ledger();
    let snap = Arc::new(RwLock::new(json!({
        "local": {
            "verified": true,
            "domain": "xrpl.example.com",
            "status": "verified",
            "last_check": 1700000000u64
        },
        "validators": []
    })));
    ctx.attach_domain_attestation_status(snap);
    let result = rxrpl_rpc_server::handlers::server_info(json!({}), &ctx)
        .await
        .unwrap();
    assert_eq!(result["info"]["domain_verification"]["status"], "verified");
    assert_eq!(
        result["info"]["domain_verification"]["domain"],
        "xrpl.example.com"
    );
}

#[tokio::test]
async fn server_info_omits_domain_verification_when_unattached() {
    let ctx = test_ctx_with_ledger();
    let result = rxrpl_rpc_server::handlers::server_info(json!({}), &ctx)
        .await
        .unwrap();
    assert!(result["info"].get("domain_verification").is_none());
}

#[tokio::test]
async fn validators_exposes_validator_domains_array() {
    let mut ctx = test_ctx_with_ledger();
    let snap = Arc::new(RwLock::new(json!({
        "local": {},
        "validators": [
            {
                "public_key": "ED1234",
                "domain": "v1.example.com",
                "verification_status": "verified",
                "last_verified": 1700000000u64
            }
        ]
    })));
    ctx.attach_domain_attestation_status(snap);
    let result = rxrpl_rpc_server::handlers::validators(json!({}), &ctx)
        .await
        .unwrap();
    let arr = result["validator_domains"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["public_key"], "ED1234");
    assert_eq!(arr[0]["verification_status"], "verified");
}

#[tokio::test]
async fn validators_returns_empty_domains_when_unattached() {
    let ctx = test_ctx_with_ledger();
    let result = rxrpl_rpc_server::handlers::validators(json!({}), &ctx)
        .await
        .unwrap();
    assert_eq!(result["validator_domains"], json!([]));
}
