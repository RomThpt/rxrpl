use rxrpl_codec::address::classic::{decode_account_id, encode_classic_address_from_pubkey};
use rxrpl_crypto::{KeyPair, KeyType, Seed};
use rxrpl_ledger::Ledger;
use rxrpl_node::Node;
use rxrpl_protocol::{TransactionResult, keylet};
use rxrpl_tx_engine::{FeeSettings, TransactorRegistry, TxEngine};
use serde_json::Value;

fn make_engine() -> TxEngine {
    let mut registry = TransactorRegistry::new();
    rxrpl_tx_engine::handlers::register_phase_a(&mut registry);
    rxrpl_tx_engine::handlers::register_phase_b(&mut registry);
    rxrpl_tx_engine::handlers::register_phase_c1(&mut registry);
    rxrpl_tx_engine::handlers::register_phase_c2(&mut registry);
    rxrpl_tx_engine::handlers::register_phase_c3(&mut registry);
    rxrpl_tx_engine::handlers::register_phase_d1(&mut registry);
    rxrpl_tx_engine::handlers::register_phase_d2(&mut registry);
    rxrpl_tx_engine::handlers::register_phase_e(&mut registry);
    TxEngine::new_without_sig_check(registry)
}

fn genesis_keypair() -> KeyPair {
    let seed = Seed::from_passphrase("genesis");
    KeyPair::from_seed(&seed, KeyType::Ed25519)
}

fn read_owner_count(ledger: &Ledger, address: &str) -> u32 {
    let account_id = decode_account_id(address).unwrap();
    let key = keylet::account(&account_id);
    let data = ledger.get_state(&key).unwrap();
    let account: Value = serde_json::from_slice(data).unwrap();
    account["OwnerCount"].as_u64().unwrap() as u32
}

fn setup_funded_account() -> (Ledger, String) {
    let genesis_kp = genesis_keypair();
    let genesis_addr = encode_classic_address_from_pubkey(genesis_kp.public_key.as_bytes());

    let closed_genesis = Node::genesis_with_funded_account(&genesis_addr).unwrap();
    let ledger = Ledger::new_open(&closed_genesis);

    (ledger, genesis_addr)
}

// -- DIDSet: create a DID entry --

#[test]
fn did_set_creates_entry() {
    let (mut ledger, genesis_addr) = setup_funded_account();
    let engine = make_engine();
    let fees = FeeSettings::default();

    let tx = serde_json::json!({
        "TransactionType": "DIDSet",
        "Account": genesis_addr,
        "URI": hex::encode("https://example.com/did").to_uppercase(),
        "Fee": "12",
        "Sequence": 1,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Verify DID entry exists
    let account_id = decode_account_id(&genesis_addr).unwrap();
    let did_key = keylet::did(&account_id);
    let data = ledger.get_state(&did_key).unwrap();
    let did: Value = serde_json::from_slice(data).unwrap();
    assert_eq!(did["LedgerEntryType"].as_str().unwrap(), "DID");

    assert_eq!(read_owner_count(&ledger, &genesis_addr), 1);
}

// -- OracleSet: create an oracle entry --

#[test]
fn oracle_set_creates_entry() {
    let (mut ledger, genesis_addr) = setup_funded_account();
    let engine = make_engine();
    let fees = FeeSettings::default();

    let tx = serde_json::json!({
        "TransactionType": "OracleSet",
        "Account": genesis_addr,
        "OracleDocumentID": 1,
        "Provider": hex::encode("chainlink").to_uppercase(),
        "LastUpdateTime": 1000,
        "PriceDataSeries": [
            {
                "PriceData": {
                    "BaseAsset": "XRP",
                    "QuoteAsset": "USD",
                    "AssetPrice": "740",
                    "Scale": 3
                }
            }
        ],
        "Fee": "12",
        "Sequence": 1,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Verify Oracle entry exists
    let account_id = decode_account_id(&genesis_addr).unwrap();
    let oracle_key = keylet::oracle(&account_id, 1);
    let data = ledger.get_state(&oracle_key).unwrap();
    let oracle: Value = serde_json::from_slice(data).unwrap();
    assert_eq!(oracle["LedgerEntryType"].as_str().unwrap(), "Oracle");

    assert_eq!(read_owner_count(&ledger, &genesis_addr), 1);
}

// -- MPTokenIssuanceCreate: create an MPToken issuance --

#[test]
fn mptoken_issuance_create_succeeds() {
    let (mut ledger, genesis_addr) = setup_funded_account();
    let engine = make_engine();
    let fees = FeeSettings::default();

    let tx = serde_json::json!({
        "TransactionType": "MPTokenIssuanceCreate",
        "Account": genesis_addr,
        "AssetScale": 2,
        "MaximumAmount": "1000000",
        "Fee": "12",
        "Sequence": 1,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Verify MPTokenIssuance entry exists (keyed by account + sequence before increment)
    let account_id = decode_account_id(&genesis_addr).unwrap();
    let issuance_key = keylet::mptoken_issuance(&account_id, 1);
    let data = ledger.get_state(&issuance_key).unwrap();
    let issuance: Value = serde_json::from_slice(data).unwrap();
    assert_eq!(
        issuance["LedgerEntryType"].as_str().unwrap(),
        "MPTokenIssuance"
    );
    assert_eq!(issuance["AssetScale"].as_u64().unwrap(), 2);

    assert_eq!(read_owner_count(&ledger, &genesis_addr), 1);
}

// -- Transaction indexing: verify SQLite indexing on ledger close --

#[test]
fn tx_indexing_on_ledger_close() {
    let genesis_kp = genesis_keypair();
    let genesis_addr = encode_classic_address_from_pubkey(genesis_kp.public_key.as_bytes());

    let config = rxrpl_config::NodeConfig::default();
    let node = Node::new_standalone(config, &genesis_addr).unwrap();

    let engine = node.tx_engine();
    let fees = node.fees();
    let store = node.tx_store().unwrap();

    // Apply a DIDSet transaction
    {
        let mut ledger = node.ledger().blocking_write();
        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": genesis_addr,
            "URI": hex::encode("https://example.com/did").to_uppercase(),
            "Fee": "12",
            "Sequence": 1,
        });
        let result = Node::apply_transaction(&mut ledger, engine, &tx, fees).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Close the ledger
        ledger.close(100, 0).unwrap();
        let closed = ledger.clone();

        // Index transactions
        Node::index_ledger_transactions(store, &closed);

        // Open next ledger
        *ledger = Ledger::new_open(&closed);
    }

    // Verify transaction was indexed
    let account_id = decode_account_id(&genesis_addr).unwrap();
    let tx_hashes = store
        .get_account_transactions(account_id.as_bytes(), 10)
        .unwrap();
    assert_eq!(tx_hashes.len(), 1);

    // Verify we can retrieve the full transaction record
    let record = store.get_transaction(&tx_hashes[0]).unwrap().unwrap();
    assert_eq!(record.ledger_seq, 2); // ledger #2 (after genesis)
}
