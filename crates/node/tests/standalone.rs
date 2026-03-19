use rxrpl_codec::address::classic::encode_classic_address_from_pubkey;
use rxrpl_config::NodeConfig;
use rxrpl_crypto::{KeyPair, KeyType, Seed};
use rxrpl_node::Node;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;

const GENESIS_ADDR: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

fn available_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

async fn rpc_call(addr: &SocketAddr, method: &str, params: Value) -> Value {
    let client = reqwest::Client::new();
    let body = json!({
        "method": method,
        "params": [params],
    });

    let resp = client
        .post(format!("http://{addr}/"))
        .json(&body)
        .send()
        .await
        .unwrap();

    resp.json::<Value>().await.unwrap()
}

#[tokio::test]
async fn standalone_account_info() {
    let port = available_port();
    let mut config = NodeConfig::default();
    config.server.bind = format!("127.0.0.1:{port}").parse().unwrap();

    let node = Node::new_standalone(config, GENESIS_ADDR).unwrap();

    // Spawn standalone without the close loop (use very long interval)
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    tokio::spawn(async move {
        node.run_standalone(3600).await.unwrap();
    });

    // Give the server a moment to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Test account_info
    let resp = rpc_call(&addr, "account_info", json!({ "account": GENESIS_ADDR })).await;
    let result = &resp["result"];
    assert_eq!(result["status"], "success");
    assert_eq!(result["account_data"]["Account"], GENESIS_ADDR);
    assert!(result["account_data"]["Balance"].as_str().is_some());

    // Test account_info for nonexistent account
    let resp = rpc_call(
        &addr,
        "account_info",
        json!({ "account": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe" }),
    )
    .await;
    assert_eq!(resp["result"]["status"], "error");
}

#[tokio::test]
async fn standalone_ledger_and_fee() {
    let port = available_port();
    let mut config = NodeConfig::default();
    config.server.bind = format!("127.0.0.1:{port}").parse().unwrap();

    let node = Node::new_standalone(config, GENESIS_ADDR).unwrap();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    tokio::spawn(async move {
        node.run_standalone(3600).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Test ledger current
    let resp = rpc_call(&addr, "ledger", json!({ "ledger_index": "current" })).await;
    let result = &resp["result"];
    assert_eq!(result["status"], "success");
    assert_eq!(result["ledger"]["ledger_index"], 2);
    assert_eq!(result["ledger"]["closed"], false);

    // Test ledger closed
    let resp = rpc_call(&addr, "ledger", json!({ "ledger_index": "closed" })).await;
    let result = &resp["result"];
    assert_eq!(result["status"], "success");
    assert_eq!(result["ledger"]["ledger_index"], 1);

    // Test ledger_closed
    let resp = rpc_call(&addr, "ledger_closed", json!({})).await;
    let result = &resp["result"];
    assert_eq!(result["status"], "success");
    assert_eq!(result["ledger_index"], 1);

    // Test fee
    let resp = rpc_call(&addr, "fee", json!({})).await;
    let result = &resp["result"];
    assert_eq!(result["status"], "success");
    assert_eq!(result["drops"]["base_fee"], "10");

    // Test server_info
    let resp = rpc_call(&addr, "server_info", json!({})).await;
    let result = &resp["result"];
    assert_eq!(result["status"], "success");
    assert_eq!(result["info"]["complete_ledgers"], "1-1");
}

#[tokio::test]
async fn standalone_auto_close() {
    let port = available_port();
    let mut config = NodeConfig::default();
    config.server.bind = format!("127.0.0.1:{port}").parse().unwrap();

    let node = Node::new_standalone(config, GENESIS_ADDR).unwrap();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    // Use 1-second close interval
    tokio::spawn(async move {
        node.run_standalone(1).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Initial state: current=2, last closed=1
    let resp = rpc_call(&addr, "ledger", json!({ "ledger_index": "current" })).await;
    assert_eq!(resp["result"]["ledger"]["ledger_index"], 2);

    // Wait for at least one close
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    // After close: current should be >= 3
    let resp = rpc_call(&addr, "ledger", json!({ "ledger_index": "current" })).await;
    let current_seq = resp["result"]["ledger"]["ledger_index"].as_u64().unwrap();
    assert!(current_seq >= 3, "expected >= 3, got {current_seq}");

    // Closed ledger should have a non-zero hash
    let resp = rpc_call(&addr, "ledger_closed", json!({})).await;
    let hash = resp["result"]["ledger_hash"].as_str().unwrap();
    assert_ne!(hash, "0".repeat(64));
}

#[tokio::test]
async fn standalone_ledger_current() {
    let port = available_port();
    let mut config = NodeConfig::default();
    config.server.bind = format!("127.0.0.1:{port}").parse().unwrap();

    let node = Node::new_standalone(config, GENESIS_ADDR).unwrap();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    tokio::spawn(async move {
        node.run_standalone(3600).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let resp = rpc_call(&addr, "ledger_current", json!({})).await;
    let result = &resp["result"];
    assert_eq!(result["status"], "success");
    assert_eq!(result["ledger_current_index"], 2);
}

#[tokio::test]
async fn standalone_sign_submit_verify() {
    let port = available_port();
    let mut config = NodeConfig::default();
    config.server.bind = format!("127.0.0.1:{port}").parse().unwrap();

    let node = Node::new_standalone(config, GENESIS_ADDR).unwrap();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    tokio::spawn(async move {
        node.run_standalone(3600).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Derive destination keypair
    let dest_seed = Seed::from_passphrase("destination");
    let dest_kp = KeyPair::from_seed(&dest_seed, KeyType::Ed25519);
    let dest_addr = encode_classic_address_from_pubkey(dest_kp.public_key.as_bytes());

    // Use a dummy keypair for signing (sig verification is disabled in standalone)
    let dummy_seed = Seed::from_passphrase("dummy_signer");
    let dummy_kp = KeyPair::from_seed(&dummy_seed, KeyType::Ed25519);

    // Get genesis balance before
    let resp = rpc_call(&addr, "account_info", json!({ "account": GENESIS_ADDR })).await;
    let genesis_balance_before: u64 = resp["result"]["account_data"]["Balance"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // Build and sign a Payment transaction
    let payment_amount: u64 = 50_000_000;
    let fee: u64 = 12;
    let tx = json!({
        "TransactionType": "Payment",
        "Account": GENESIS_ADDR,
        "Destination": dest_addr,
        "Amount": payment_amount.to_string(),
        "Fee": fee.to_string(),
        "Sequence": 1,
    });

    let signed = rxrpl_protocol::tx::sign(&tx, &dummy_kp).unwrap();
    let tx_blob = rxrpl_protocol::tx::serialize_signed(&signed).unwrap();

    // Submit
    let resp = rpc_call(&addr, "submit", json!({ "tx_blob": tx_blob })).await;
    let result = &resp["result"];
    assert_eq!(result["status"], "success");
    assert_eq!(result["engine_result"], "tesSUCCESS");

    // Verify genesis balance decreased
    let resp = rpc_call(&addr, "account_info", json!({ "account": GENESIS_ADDR })).await;
    let genesis_balance_after: u64 = resp["result"]["account_data"]["Balance"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        genesis_balance_after,
        genesis_balance_before - payment_amount - fee
    );

    // Verify destination account created with correct balance
    let resp = rpc_call(&addr, "account_info", json!({ "account": dest_addr })).await;
    let result = &resp["result"];
    assert_eq!(result["status"], "success");
    let dest_balance: u64 = result["account_data"]["Balance"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(dest_balance, payment_amount);
}

#[tokio::test]
async fn standalone_submit_queues_and_close_clears() {
    let port = available_port();
    let mut config = NodeConfig::default();
    config.server.bind = format!("127.0.0.1:{port}").parse().unwrap();

    let node = Node::new_standalone(config, GENESIS_ADDR).unwrap();
    let tx_queue = Arc::clone(node.tx_queue());
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    // Use a 2-second close interval so we can observe the queue before and after close
    tokio::spawn(async move {
        node.run_standalone(2).await.unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Derive destination keypair
    let dest_seed = Seed::from_passphrase("e2e_dest");
    let dest_kp = KeyPair::from_seed(&dest_seed, KeyType::Ed25519);
    let dest_addr = encode_classic_address_from_pubkey(dest_kp.public_key.as_bytes());

    let dummy_seed = Seed::from_passphrase("dummy_signer");
    let dummy_kp = KeyPair::from_seed(&dummy_seed, KeyType::Ed25519);

    // Submit first Payment
    let tx1 = json!({
        "TransactionType": "Payment",
        "Account": GENESIS_ADDR,
        "Destination": dest_addr,
        "Amount": "50000000",
        "Fee": "12",
        "Sequence": 1,
    });
    let signed1 = rxrpl_protocol::tx::sign(&tx1, &dummy_kp).unwrap();
    let blob1 = rxrpl_protocol::tx::serialize_signed(&signed1).unwrap();

    let resp = rpc_call(&addr, "submit", json!({ "tx_blob": blob1 })).await;
    assert_eq!(resp["result"]["engine_result"], "tesSUCCESS");

    // Queue should have 1 transaction
    {
        let q = tx_queue.read().await;
        assert_eq!(q.len(), 1, "expected 1 tx in queue after first submit");
    }

    // Submit second Payment (sequence 2, different destination to avoid dup)
    let dest_seed2 = Seed::from_passphrase("e2e_dest2");
    let dest_kp2 = KeyPair::from_seed(&dest_seed2, KeyType::Ed25519);
    let dest_addr2 = encode_classic_address_from_pubkey(dest_kp2.public_key.as_bytes());

    let tx2 = json!({
        "TransactionType": "Payment",
        "Account": GENESIS_ADDR,
        "Destination": dest_addr2,
        "Amount": "30000000",
        "Fee": "12",
        "Sequence": 2,
    });
    let signed2 = rxrpl_protocol::tx::sign(&tx2, &dummy_kp).unwrap();
    let blob2 = rxrpl_protocol::tx::serialize_signed(&signed2).unwrap();

    let resp = rpc_call(&addr, "submit", json!({ "tx_blob": blob2 })).await;
    assert_eq!(resp["result"]["engine_result"], "tesSUCCESS");

    // Queue should have 2 transactions
    {
        let q = tx_queue.read().await;
        assert_eq!(q.len(), 2, "expected 2 txs in queue after second submit");
    }

    // Wait for the ledger close (2s interval + margin)
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // After close, queue should be empty
    {
        let q = tx_queue.read().await;
        assert_eq!(q.len(), 0, "expected empty queue after ledger close");
    }
}
