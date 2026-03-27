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
    let account: Value = rxrpl_ledger::sle_codec::decode_state(data).unwrap();
    account["Balance"].as_str().unwrap().parse::<u64>().unwrap()
}

fn read_owner_count(ledger: &Ledger, address: &str) -> u32 {
    let account_id = decode_account_id(address).unwrap();
    let key = keylet::account(&account_id);
    let data = ledger.get_state(&key).unwrap();
    let account: Value = rxrpl_ledger::sle_codec::decode_state(data).unwrap();
    account["OwnerCount"].as_u64().unwrap() as u32
}

fn setup_funded_pair() -> (Ledger, String, String) {
    let genesis_kp = genesis_keypair();
    let genesis_addr = encode_classic_address_from_pubkey(genesis_kp.public_key.as_bytes());
    let dest_kp = dest_keypair();
    let dest_addr = encode_classic_address_from_pubkey(dest_kp.public_key.as_bytes());

    let engine = make_engine();
    let fees = FeeSettings::default();

    let closed_genesis = Node::genesis_with_funded_account(&genesis_addr).unwrap();
    let mut ledger = Ledger::new_open(&closed_genesis);

    // Fund destination
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": genesis_addr,
        "Destination": dest_addr,
        "Amount": "100000000",
        "Fee": "12",
        "Sequence": 1,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    (ledger, genesis_addr, dest_addr)
}

// -- Escrow lifecycle tests --

#[test]
fn escrow_create_finish_lifecycle() {
    let (mut ledger, genesis_addr, dest_addr) = setup_funded_pair();
    let engine = make_engine();
    let fees = FeeSettings::default();

    let genesis_balance = read_account_balance(&ledger, &genesis_addr);

    // Create escrow
    let tx = serde_json::json!({
        "TransactionType": "EscrowCreate",
        "Account": genesis_addr,
        "Destination": dest_addr,
        "Amount": "10000000",
        "Fee": "12",
        "Sequence": 2,
        "FinishAfter": 100,
    });

    let result = Node::apply_transaction(&mut ledger, &engine, &tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    let after_create = read_account_balance(&ledger, &genesis_addr);
    assert_eq!(after_create, genesis_balance - 10_000_000 - 12);
    assert_eq!(read_owner_count(&ledger, &genesis_addr), 1);

    // Close and open new ledger with parent_close_time >= FinishAfter
    ledger.close(200, 0).unwrap();
    let mut ledger2 = Ledger::new_open(&ledger);

    // Finish escrow
    let tx2 = serde_json::json!({
        "TransactionType": "EscrowFinish",
        "Account": genesis_addr,
        "Owner": genesis_addr,
        "OfferSequence": 2,
        "Fee": "12",
        "Sequence": 3,
    });

    let result2 = Node::apply_transaction(&mut ledger2, &engine, &tx2, &fees).unwrap();
    assert_eq!(result2, TransactionResult::TesSuccess);

    // Destination got the escrowed amount
    let dest_balance = read_account_balance(&ledger2, &dest_addr);
    assert_eq!(dest_balance, 100_000_000 + 10_000_000);

    // Owner count back to 0
    assert_eq!(read_owner_count(&ledger2, &genesis_addr), 0);
}

#[test]
fn escrow_create_cancel_lifecycle() {
    let (mut ledger, genesis_addr, dest_addr) = setup_funded_pair();
    let engine = make_engine();
    let fees = FeeSettings::default();

    let genesis_balance = read_account_balance(&ledger, &genesis_addr);

    // Create escrow with CancelAfter
    let tx = serde_json::json!({
        "TransactionType": "EscrowCreate",
        "Account": genesis_addr,
        "Destination": dest_addr,
        "Amount": "10000000",
        "Fee": "12",
        "Sequence": 2,
        "FinishAfter": 100,
        "CancelAfter": 200,
    });

    let result = Node::apply_transaction(&mut ledger, &engine, &tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Close and open with parent_close_time >= CancelAfter
    ledger.close(300, 0).unwrap();
    let mut ledger2 = Ledger::new_open(&ledger);

    // Cancel escrow
    let tx2 = serde_json::json!({
        "TransactionType": "EscrowCancel",
        "Account": genesis_addr,
        "Owner": genesis_addr,
        "OfferSequence": 2,
        "Fee": "12",
        "Sequence": 3,
    });

    let result2 = Node::apply_transaction(&mut ledger2, &engine, &tx2, &fees).unwrap();
    assert_eq!(result2, TransactionResult::TesSuccess);

    // Genesis got amount back (minus fees for both txs)
    let final_balance = read_account_balance(&ledger2, &genesis_addr);
    assert_eq!(final_balance, genesis_balance - 12 - 12);
    assert_eq!(read_owner_count(&ledger2, &genesis_addr), 0);
}

#[test]
fn escrow_finish_too_early_fails() {
    let (mut ledger, genesis_addr, dest_addr) = setup_funded_pair();
    let engine = make_engine();
    let fees = FeeSettings::default();

    // Create escrow with FinishAfter = 1000
    let tx = serde_json::json!({
        "TransactionType": "EscrowCreate",
        "Account": genesis_addr,
        "Destination": dest_addr,
        "Amount": "10000000",
        "Fee": "12",
        "Sequence": 2,
        "FinishAfter": 1000,
    });

    let result = Node::apply_transaction(&mut ledger, &engine, &tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Close with parent_close_time < FinishAfter
    ledger.close(500, 0).unwrap();
    let mut ledger2 = Ledger::new_open(&ledger);

    // Try to finish -- should fail
    let tx2 = serde_json::json!({
        "TransactionType": "EscrowFinish",
        "Account": genesis_addr,
        "Owner": genesis_addr,
        "OfferSequence": 2,
        "Fee": "12",
        "Sequence": 3,
    });

    let result2 = Node::apply_transaction(&mut ledger2, &engine, &tx2, &fees).unwrap();
    assert_eq!(result2, TransactionResult::TecNoPermission);
}

// -- Check lifecycle tests --

#[test]
fn check_create_cash_lifecycle() {
    let (mut ledger, genesis_addr, dest_addr) = setup_funded_pair();
    let engine = make_engine();
    let fees = FeeSettings::default();

    let genesis_balance = read_account_balance(&ledger, &genesis_addr);
    let dest_balance = read_account_balance(&ledger, &dest_addr);

    // Create check
    let tx = serde_json::json!({
        "TransactionType": "CheckCreate",
        "Account": genesis_addr,
        "Destination": dest_addr,
        "SendMax": "5000000",
        "Fee": "12",
        "Sequence": 2,
    });

    let result = Node::apply_transaction(&mut ledger, &engine, &tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);
    assert_eq!(read_owner_count(&ledger, &genesis_addr), 1);

    // Compute check ID
    let genesis_id = decode_account_id(&genesis_addr).unwrap();
    let check_key = keylet::check(&genesis_id, 2); // seq was 2 at create time
    let check_id = hex::encode(check_key.as_bytes());

    // Cash check (dest cashes it)
    let tx2 = serde_json::json!({
        "TransactionType": "CheckCash",
        "Account": dest_addr,
        "CheckID": check_id,
        "Amount": "3000000",
        "Fee": "12",
        "Sequence": 1,
    });

    let result2 = Node::apply_transaction(&mut ledger, &engine, &tx2, &fees).unwrap();
    assert_eq!(result2, TransactionResult::TesSuccess);

    // Genesis debited 3M (check) + 12 (create fee) + 12 (engine deducted for cash? no, dest pays)
    let final_genesis = read_account_balance(&ledger, &genesis_addr);
    assert_eq!(final_genesis, genesis_balance - 12 - 3_000_000);

    // Dest credited 3M - 12 (fee for cash tx)
    let final_dest = read_account_balance(&ledger, &dest_addr);
    assert_eq!(final_dest, dest_balance + 3_000_000 - 12);

    assert_eq!(read_owner_count(&ledger, &genesis_addr), 0);
}

#[test]
fn check_create_cancel_lifecycle() {
    let (mut ledger, genesis_addr, dest_addr) = setup_funded_pair();
    let engine = make_engine();
    let fees = FeeSettings::default();

    // Create check
    let tx = serde_json::json!({
        "TransactionType": "CheckCreate",
        "Account": genesis_addr,
        "Destination": dest_addr,
        "SendMax": "5000000",
        "Fee": "12",
        "Sequence": 2,
    });

    let result = Node::apply_transaction(&mut ledger, &engine, &tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    let genesis_id = decode_account_id(&genesis_addr).unwrap();
    let check_key = keylet::check(&genesis_id, 2);
    let check_id = hex::encode(check_key.as_bytes());

    // Cancel check (source cancels)
    let tx2 = serde_json::json!({
        "TransactionType": "CheckCancel",
        "Account": genesis_addr,
        "CheckID": check_id,
        "Fee": "12",
        "Sequence": 3,
    });

    let result2 = Node::apply_transaction(&mut ledger, &engine, &tx2, &fees).unwrap();
    assert_eq!(result2, TransactionResult::TesSuccess);
    assert_eq!(read_owner_count(&ledger, &genesis_addr), 0);
}

// -- PaymentChannel lifecycle tests --

#[test]
fn payment_channel_create_fund_claim_lifecycle() {
    let (mut ledger, genesis_addr, dest_addr) = setup_funded_pair();
    let engine = make_engine();
    let fees = FeeSettings::default();

    let genesis_balance = read_account_balance(&ledger, &genesis_addr);
    let dest_balance = read_account_balance(&ledger, &dest_addr);

    // Create payment channel
    let tx = serde_json::json!({
        "TransactionType": "PaymentChannelCreate",
        "Account": genesis_addr,
        "Destination": dest_addr,
        "Amount": "10000000",
        "SettleDelay": 86400,
        "PublicKey": "0330E7FC9D56BB25D6893BA3F317AE5BCF33B3291BD63DB32654A313222F7FD020",
        "Fee": "12",
        "Sequence": 2,
    });

    let result = Node::apply_transaction(&mut ledger, &engine, &tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    let after_create = read_account_balance(&ledger, &genesis_addr);
    assert_eq!(after_create, genesis_balance - 10_000_000 - 12);
    assert_eq!(read_owner_count(&ledger, &genesis_addr), 1);

    // Compute channel ID
    let genesis_id = decode_account_id(&genesis_addr).unwrap();
    let dest_id = decode_account_id(&dest_addr).unwrap();
    let channel_key = keylet::pay_channel(&genesis_id, &dest_id, 2);
    let channel_id = hex::encode(channel_key.as_bytes());

    // Fund channel
    let tx2 = serde_json::json!({
        "TransactionType": "PaymentChannelFund",
        "Account": genesis_addr,
        "Channel": channel_id,
        "Amount": "5000000",
        "Fee": "12",
        "Sequence": 3,
    });

    let result2 = Node::apply_transaction(&mut ledger, &engine, &tx2, &fees).unwrap();
    assert_eq!(result2, TransactionResult::TesSuccess);

    let after_fund = read_account_balance(&ledger, &genesis_addr);
    assert_eq!(
        after_fund,
        genesis_balance - 10_000_000 - 12 - 5_000_000 - 12
    );

    // Claim part of channel (dest claims 3M)
    let tx3 = serde_json::json!({
        "TransactionType": "PaymentChannelClaim",
        "Account": dest_addr,
        "Channel": channel_id,
        "Balance": "3000000",
        "Fee": "12",
        "Sequence": 1,
    });

    let result3 = Node::apply_transaction(&mut ledger, &engine, &tx3, &fees).unwrap();
    assert_eq!(result3, TransactionResult::TesSuccess);

    let after_claim = read_account_balance(&ledger, &dest_addr);
    assert_eq!(after_claim, dest_balance + 3_000_000 - 12);
}

// -- DepositPreauth lifecycle tests --

#[test]
fn deposit_preauth_authorize_unauthorize() {
    let (mut ledger, genesis_addr, dest_addr) = setup_funded_pair();
    let engine = make_engine();
    let fees = FeeSettings::default();

    // Authorize
    let tx = serde_json::json!({
        "TransactionType": "DepositPreauth",
        "Account": genesis_addr,
        "Authorize": dest_addr,
        "Fee": "12",
        "Sequence": 2,
    });

    let result = Node::apply_transaction(&mut ledger, &engine, &tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);
    assert_eq!(read_owner_count(&ledger, &genesis_addr), 1);

    // Verify entry exists
    let genesis_id = decode_account_id(&genesis_addr).unwrap();
    let dest_id = decode_account_id(&dest_addr).unwrap();
    let dp_key = keylet::deposit_preauth(&genesis_id, &dest_id);
    assert!(ledger.has_state(&dp_key));

    // Unauthorize
    let tx2 = serde_json::json!({
        "TransactionType": "DepositPreauth",
        "Account": genesis_addr,
        "Unauthorize": dest_addr,
        "Fee": "12",
        "Sequence": 3,
    });

    let result2 = Node::apply_transaction(&mut ledger, &engine, &tx2, &fees).unwrap();
    assert_eq!(result2, TransactionResult::TesSuccess);
    assert_eq!(read_owner_count(&ledger, &genesis_addr), 0);
    assert!(!ledger.has_state(&dp_key));
}
