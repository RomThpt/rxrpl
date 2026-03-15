use rxrpl_codec::address::classic::{decode_account_id, encode_classic_address_from_pubkey};
use rxrpl_crypto::{KeyPair, KeyType, Seed};
use rxrpl_ledger::Ledger;
use rxrpl_node::Node;
use rxrpl_protocol::{keylet, TransactionResult};
use rxrpl_tx_engine::{FeeSettings, TransactorRegistry, TxEngine};
use serde_json::Value;

fn make_engine() -> TxEngine {
    let mut registry = TransactorRegistry::new();
    rxrpl_tx_engine::handlers::register_phase_a(&mut registry);
    TxEngine::new_without_sig_check(registry)
}

fn genesis_keypair() -> KeyPair {
    let seed = Seed::from_passphrase("genesis");
    KeyPair::from_seed(&seed, KeyType::Ed25519)
}

fn dest_keypair() -> KeyPair {
    let seed = Seed::from_passphrase("destination");
    KeyPair::from_seed(&seed, KeyType::Ed25519)
}

fn read_account_balance(ledger: &Ledger, address: &str) -> u64 {
    let account_id = decode_account_id(address).unwrap();
    let key = keylet::account(&account_id);
    let data = ledger.get_state(&key).unwrap();
    let account: Value = serde_json::from_slice(data).unwrap();
    account["Balance"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap()
}

/// Full lifecycle: genesis -> apply tx -> close -> apply tx -> close.
#[test]
fn full_ledger_lifecycle() {
    let genesis_kp = genesis_keypair();
    let genesis_addr = encode_classic_address_from_pubkey(genesis_kp.public_key.as_bytes());

    let dest_kp = dest_keypair();
    let dest_addr = encode_classic_address_from_pubkey(dest_kp.public_key.as_bytes());

    let engine = make_engine();
    let fees = FeeSettings::default();

    // Step 1: Create genesis with funded account
    let closed_genesis = Node::genesis_with_funded_account(&genesis_addr).unwrap();
    assert!(closed_genesis.is_closed());
    assert_eq!(closed_genesis.header.sequence, 1);
    let genesis_balance = read_account_balance(&closed_genesis, &genesis_addr);
    assert!(genesis_balance > 0);

    // Step 2: Open ledger #2
    let mut ledger = Ledger::new_open(&closed_genesis);
    assert!(ledger.is_open());
    assert_eq!(ledger.header.sequence, 2);
    assert_eq!(ledger.header.parent_hash, closed_genesis.header.hash);

    // Step 3: Apply a Payment (creates destination account)
    let payment_amount: u64 = 50_000_000; // 50 XRP
    let fee: u64 = 12;
    let tx1 = serde_json::json!({
        "TransactionType": "Payment",
        "Account": genesis_addr,
        "Destination": dest_addr,
        "Amount": payment_amount.to_string(),
        "Fee": fee.to_string(),
        "Sequence": 1,
    });

    let result = Node::apply_transaction(&mut ledger, &engine, &tx1, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Verify balances
    let sender_balance = read_account_balance(&ledger, &genesis_addr);
    assert_eq!(sender_balance, genesis_balance - payment_amount - fee);

    let receiver_balance = read_account_balance(&ledger, &dest_addr);
    assert_eq!(receiver_balance, payment_amount);

    // Step 4: Close ledger #2
    let hash_2 = Node::close_ledger(&mut ledger, 100).unwrap();
    assert!(ledger.is_closed());
    assert!(!hash_2.is_zero());

    let closed_2 = ledger;

    // Step 5: Open ledger #3
    let mut ledger_3 = Ledger::new_open(&closed_2);
    assert_eq!(ledger_3.header.sequence, 3);
    assert_eq!(ledger_3.header.parent_hash, hash_2);

    // Step 6: Apply another Payment (dest -> genesis, so dest already exists)
    let tx2 = serde_json::json!({
        "TransactionType": "Payment",
        "Account": dest_addr,
        "Destination": genesis_addr,
        "Amount": "10000000",
        "Fee": "12",
        "Sequence": 1,
    });

    let result2 = Node::apply_transaction(&mut ledger_3, &engine, &tx2, &fees).unwrap();
    assert_eq!(result2, TransactionResult::TesSuccess);

    // Step 7: Close ledger #3
    let hash_3 = Node::close_ledger(&mut ledger_3, 200).unwrap();
    assert!(!hash_3.is_zero());

    // Verify parent hash chain
    assert_eq!(ledger_3.header.parent_hash, hash_2);
    assert_ne!(hash_2, hash_3);

    // Verify final balances
    let final_sender = read_account_balance(&ledger_3, &genesis_addr);
    let final_receiver = read_account_balance(&ledger_3, &dest_addr);

    // Genesis: started with genesis_balance, sent 50M+12, received 10M
    assert_eq!(
        final_sender,
        genesis_balance - payment_amount - fee + 10_000_000
    );
    // Dest: received 50M, sent 10M+12
    assert_eq!(final_receiver, payment_amount - 10_000_000 - 12);
}

/// Verify that tx_map contains entries after transactions.
#[test]
fn tx_map_records_transactions() {
    let genesis_kp = genesis_keypair();
    let genesis_addr = encode_classic_address_from_pubkey(genesis_kp.public_key.as_bytes());
    let dest_kp = dest_keypair();
    let dest_addr = encode_classic_address_from_pubkey(dest_kp.public_key.as_bytes());

    let engine = make_engine();
    let fees = FeeSettings::default();

    let closed_genesis = Node::genesis_with_funded_account(&genesis_addr).unwrap();
    let mut ledger = Ledger::new_open(&closed_genesis);

    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": genesis_addr,
        "Destination": dest_addr,
        "Amount": "1000000",
        "Fee": "10",
        "Sequence": 1,
    });

    let result = Node::apply_transaction(&mut ledger, &engine, &tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Verify tx is in tx_map using its hash
    let tx_hash = rxrpl_protocol::tx::compute_tx_hash(&tx).unwrap();
    assert!(ledger.tx_map.has(&tx_hash));

    // Close and verify tx_hash is non-zero
    Node::close_ledger(&mut ledger, 100).unwrap();
    assert!(!ledger.header.tx_hash.is_zero());
}

/// Verify that applying to a closed ledger fails.
#[test]
fn apply_to_closed_ledger_fails() {
    let genesis_kp = genesis_keypair();
    let genesis_addr = encode_classic_address_from_pubkey(genesis_kp.public_key.as_bytes());
    let dest_kp = dest_keypair();
    let dest_addr = encode_classic_address_from_pubkey(dest_kp.public_key.as_bytes());

    let engine = make_engine();
    let fees = FeeSettings::default();

    let mut closed_genesis = Node::genesis_with_funded_account(&genesis_addr).unwrap();

    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": genesis_addr,
        "Destination": dest_addr,
        "Amount": "1000000",
        "Fee": "10",
        "Sequence": 1,
    });

    let result = Node::apply_transaction(&mut closed_genesis, &engine, &tx, &fees);
    assert!(result.is_err());
}
