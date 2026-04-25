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

// -- NFToken lifecycle: mint -> create sell offer -> accept offer -> verify -> burn --

#[test]
fn nftoken_full_lifecycle() {
    let (mut ledger, genesis_addr, dest_addr) = setup_funded_pair();
    let engine = make_engine();
    let fees = FeeSettings::default();

    // 1. Mint NFToken
    let uri_hex = hex::encode("https://example.com/nft/1").to_uppercase();
    let mint_tx = serde_json::json!({
        "TransactionType": "NFTokenMint",
        "Account": genesis_addr,
        "NFTokenTaxon": 42,
        "URI": uri_hex,
        "Fee": "12",
        "Sequence": 2,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &mint_tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);
    assert_eq!(read_owner_count(&ledger, &genesis_addr), 1);

    // Get token ID from page
    let genesis_id = decode_account_id(&genesis_addr).unwrap();
    let page_key = keylet::nftoken_page_min(&genesis_id);
    let page_data = ledger.get_state(&page_key).unwrap();
    let page: Value = rxrpl_ledger::sle_codec::decode_state(page_data).unwrap();
    let nftoken_id = page["NFTokens"][0]["NFTokenID"]
        .as_str()
        .unwrap()
        .to_string();

    // 2. Create sell offer
    let offer_tx = serde_json::json!({
        "TransactionType": "NFTokenCreateOffer",
        "Account": genesis_addr,
        "NFTokenID": nftoken_id,
        "Amount": "5000000",
        "Flags": 1, // sell
        "Fee": "12",
        "Sequence": 3,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &offer_tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);
    assert_eq!(read_owner_count(&ledger, &genesis_addr), 2); // token + offer

    // Get offer ID
    let offer_key = keylet::nftoken_offer(&genesis_id, 3);
    let offer_id = hex::encode(offer_key.as_bytes()).to_uppercase();

    // 3. Accept sell offer (dest buys)
    let accept_tx = serde_json::json!({
        "TransactionType": "NFTokenAcceptOffer",
        "Account": dest_addr,
        "NFTokenSellOffer": offer_id,
        "Fee": "12",
        "Sequence": 1,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &accept_tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // 4. Verify token moved to dest
    let dest_id = decode_account_id(&dest_addr).unwrap();
    let dest_page_key = keylet::nftoken_page_min(&dest_id);
    let dest_page_data = ledger.get_state(&dest_page_key).unwrap();
    let dest_page: Value = rxrpl_ledger::sle_codec::decode_state(dest_page_data).unwrap();
    let dest_tokens = dest_page["NFTokens"].as_array().unwrap();
    assert_eq!(dest_tokens.len(), 1);
    assert_eq!(dest_tokens[0]["NFTokenID"].as_str().unwrap(), nftoken_id);

    // Verify genesis no longer has the token
    assert!(ledger.get_state(&page_key).is_none());

    // Verify XRP transferred
    let dest_balance = read_account_balance(&ledger, &dest_addr);
    assert_eq!(dest_balance, 100_000_000 - 5_000_000 - 12); // paid 5M + accept fee

    // 5. Burn token (new owner burns it)
    let burn_tx = serde_json::json!({
        "TransactionType": "NFTokenBurn",
        "Account": dest_addr,
        "NFTokenID": nftoken_id,
        "Fee": "12",
        "Sequence": 2,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &burn_tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Verify token page deleted
    assert!(ledger.get_state(&dest_page_key).is_none());
}

// -- NFToken cancel: mint -> create offer -> cancel -> verify deleted --

#[test]
fn nftoken_cancel_offer_lifecycle() {
    let (mut ledger, genesis_addr, _dest_addr) = setup_funded_pair();
    let engine = make_engine();
    let fees = FeeSettings::default();

    // Mint
    let mint_tx = serde_json::json!({
        "TransactionType": "NFTokenMint",
        "Account": genesis_addr,
        "NFTokenTaxon": 0,
        "Fee": "12",
        "Sequence": 2,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &mint_tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    let genesis_id = decode_account_id(&genesis_addr).unwrap();
    let page_key = keylet::nftoken_page_min(&genesis_id);
    let page_data = ledger.get_state(&page_key).unwrap();
    let page: Value = rxrpl_ledger::sle_codec::decode_state(page_data).unwrap();
    let nftoken_id = page["NFTokens"][0]["NFTokenID"]
        .as_str()
        .unwrap()
        .to_string();

    // Create offer
    let offer_tx = serde_json::json!({
        "TransactionType": "NFTokenCreateOffer",
        "Account": genesis_addr,
        "NFTokenID": nftoken_id,
        "Amount": "1000000",
        "Flags": 1,
        "Fee": "12",
        "Sequence": 3,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &offer_tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);
    assert_eq!(read_owner_count(&ledger, &genesis_addr), 2);

    let offer_key = keylet::nftoken_offer(&genesis_id, 3);
    let offer_id = hex::encode(offer_key.as_bytes()).to_uppercase();

    // Cancel offer
    let cancel_tx = serde_json::json!({
        "TransactionType": "NFTokenCancelOffer",
        "Account": genesis_addr,
        "NFTokenOffers": [offer_id],
        "Fee": "12",
        "Sequence": 4,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &cancel_tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // Verify offer deleted
    assert!(ledger.get_state(&offer_key).is_none());
    assert_eq!(read_owner_count(&ledger, &genesis_addr), 1); // only the token remains
}

// -- Clawback lifecycle: trust set -> IOU payment -> clawback -> verify balance --

#[test]
fn clawback_lifecycle() {
    let (mut ledger, genesis_addr, dest_addr) = setup_funded_pair();
    let engine = make_engine();
    let fees = FeeSettings::default();

    // 0. Enable AllowTrustLineClawback on the issuer (asf=16); rxrpl
    // mirrors rippled in requiring this flag before any Clawback.
    let allow_clawback_tx = serde_json::json!({
        "TransactionType": "AccountSet",
        "Account": genesis_addr,
        "SetFlag": 16,
        "Fee": "12",
        "Sequence": 1,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &allow_clawback_tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // 1. Create trust line (dest trusts genesis for USD)
    let trust_tx = serde_json::json!({
        "TransactionType": "TrustSet",
        "Account": dest_addr,
        "LimitAmount": {
            "currency": "USD",
            "issuer": genesis_addr,
            "value": "1000"
        },
        "Fee": "12",
        "Sequence": 1,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &trust_tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // 2. Issue USD from genesis to dest (IOU payment)
    // In a simplified model, we directly set the trust line balance.
    // The Payment handler only supports XRP, so we manually set the balance.
    let genesis_id = decode_account_id(&genesis_addr).unwrap();
    let dest_id = decode_account_id(&dest_addr).unwrap();
    let currency_bytes = rxrpl_tx_engine::helpers::currency_to_bytes("USD");
    let tl_key = keylet::trust_line(&genesis_id, &dest_id, &currency_bytes);

    // Read existing trust line and set balance
    let tl_data = ledger.get_state(&tl_key).unwrap();
    let mut tl: Value = rxrpl_ledger::sle_codec::decode_state(tl_data).unwrap();

    let is_genesis_low = genesis_id.as_bytes() < dest_id.as_bytes();
    let balance_value = if is_genesis_low { "100" } else { "-100" };
    tl["Balance"]["value"] = Value::String(balance_value.to_string());
    let json_bytes = serde_json::to_vec(&tl).unwrap();
    let binary = rxrpl_ledger::sle_codec::encode_sle(&json_bytes).unwrap();
    ledger.put_state(tl_key, binary).unwrap();

    // 3. Clawback 30 USD (genesis sequence is now 3 after AccountSet+TrustSet=no,
    // wait — TrustSet bumps dest, not genesis; genesis seq is 2)
    let clawback_tx = serde_json::json!({
        "TransactionType": "Clawback",
        "Account": genesis_addr,
        "Amount": {
            "currency": "USD",
            "issuer": dest_addr,
            "value": "30"
        },
        "Fee": "12",
        "Sequence": 2,
    });
    let result = Node::apply_transaction(&mut ledger, &engine, &clawback_tx, &fees).unwrap();
    assert_eq!(result, TransactionResult::TesSuccess);

    // 4. Verify balance reduced to 70
    let tl_data = ledger.get_state(&tl_key).unwrap();
    let tl: Value = rxrpl_ledger::sle_codec::decode_state(tl_data).unwrap();
    let balance: f64 = tl["Balance"]["value"].as_str().unwrap().parse().unwrap();
    let holder_balance = if is_genesis_low { balance } else { -balance };
    assert!((holder_balance - 70.0).abs() < 0.001);
}
