use super::*;

#[test]
fn parse_node_seed_hex() {
    let s = "0123456789ABCDEF0123456789ABCDEF";
    let bytes = parse_node_seed(s).unwrap();
    assert_eq!(bytes.len(), 16);
    assert_eq!(bytes[0], 0x01);
    assert_eq!(bytes[15], 0xEF);
}

#[test]
fn parse_node_seed_base58() {
    // xrpl-hive's first DefaultValidator seed (rippled-style base58).
    let s = "sneWFZcEqA8TUA5BmJ38xsqaR7dFb";
    let bytes = parse_node_seed(s).unwrap();
    assert_eq!(bytes.len(), 16);
}

#[test]
fn parse_node_seed_garbage_rejected() {
    assert!(parse_node_seed("not a seed").is_err());
}

#[test]
fn build_validator_identity_returns_none_for_empty_config() {
    let cfg = rxrpl_config::ValidatorIdentityConfig::default();
    let id = build_validator_identity(&cfg).unwrap();
    assert!(id.is_none(), "empty validator_identity config = no signer");
}

#[test]
fn build_validator_identity_loads_two_key_from_seeds() {
    let cfg = rxrpl_config::ValidatorIdentityConfig {
        master_secret: Some("sneWFZcEqA8TUA5BmJ38xsqaR7dFb".into()),
        ephemeral_seed: Some(
            // 32 hex chars = 16 raw seed bytes (distinct from the master).
            "FFEEDDCCBBAA99887766554433221100".into(),
        ),
        ..Default::default()
    };
    let id = build_validator_identity(&cfg).unwrap().expect("id present");

    assert_ne!(
        id.master_pubkey().as_bytes(),
        id.signing_pubkey().as_bytes(),
        "two-key load must produce distinct keys"
    );
}

#[test]
fn build_validator_identity_rejects_master_without_ephemeral() {
    let cfg = rxrpl_config::ValidatorIdentityConfig {
        master_secret: Some("sneWFZcEqA8TUA5BmJ38xsqaR7dFb".into()),
        ephemeral_seed: None,
        ..Default::default()
    };
    let err = build_validator_identity(&cfg).expect_err("must reject");
    let s = format!("{err}");
    assert!(
        s.contains("ephemeral_seed"),
        "error must point at the missing field, got: {s}"
    );
}

#[test]
fn build_validator_identity_loads_from_token() {
    use base64::Engine;

    // Build a real rippled-style validator token: a master-signed manifest
    // plus the ephemeral signing secret, then load it through the config path.
    let master_seed = rxrpl_crypto::Seed::from_passphrase("token-e2e-master");
    let signing_seed = rxrpl_crypto::Seed::from_passphrase("token-e2e-signing");
    let id = rxrpl_overlay::identity::ValidatorIdentity::two_key_typed(
        &master_seed,
        rxrpl_crypto::KeyType::Secp256k1,
        &signing_seed,
        rxrpl_crypto::KeyType::Secp256k1,
    );
    let manifest = id.sign_manifest(1, None).expect("manifest");
    let signing_secret = id.signing_keypair().private_key.clone();

    let b64 = base64::engine::general_purpose::STANDARD;
    let inner = format!(
        r#"{{"manifest":"{}","validation_secret_key":"{}"}}"#,
        b64.encode(&manifest),
        hex::encode(&signing_secret),
    );
    let token = b64.encode(inner.as_bytes());

    let cfg = rxrpl_config::ValidatorIdentityConfig {
        validator_token: Some(token),
        ..Default::default()
    };
    let loaded = build_validator_identity(&cfg).unwrap().expect("id present");

    assert_eq!(
        loaded.master_pubkey().as_bytes(),
        id.master_pubkey().as_bytes(),
        "token load must recover the master public key"
    );
    assert_eq!(
        loaded.signing_pubkey().as_bytes(),
        id.signing_pubkey().as_bytes(),
        "token load must recover the ephemeral signing key"
    );
    assert!(
        loaded.master_keypair().is_none(),
        "token identity carries no master secret"
    );
}

#[test]
fn create_node() {
    let config = NodeConfig::default();
    let node = Node::new(config).unwrap();
    assert!(!node.is_running());
}

#[test]
fn genesis_with_funded_account() {
    let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    let genesis = Node::genesis_with_funded_account(address).unwrap();

    assert!(genesis.is_closed());
    assert!(!genesis.header.hash.is_zero());

    // Verify account exists with full XRP supply
    let account_id = decode_account_id(address).unwrap();
    let key = keylet::account(&account_id);
    let data = genesis.get_state(&key).unwrap();
    let account: Value = rxrpl_ledger::sle_codec::decode_state(data).unwrap();
    assert_eq!(
        account["Balance"].as_str().unwrap(),
        genesis.header.drops.to_string()
    );
}

#[test]
fn new_standalone_creates_open_ledger() {
    let config = NodeConfig::default();
    let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    let node = Node::new_standalone(config, address).unwrap();

    // Ledger should be open at sequence 2
    let ledger = node.ledger.blocking_read();
    assert!(ledger.is_open());
    assert_eq!(ledger.header.sequence, 2);

    // Should have genesis in closed history
    let closed = node.closed_ledgers.blocking_read();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].header.sequence, 1);
}

#[test]
fn networked_genesis_stock_layout_has_fee_and_amendments() {
    let config = NodeConfig::default();
    assert!(!config.network.genesis_amendments_disabled);
    let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    let node = Node::new_standalone(config, address).unwrap();

    let closed = node.closed_ledgers.blocking_read();
    let genesis = &closed[0];
    assert!(genesis.get_state(&keylet::fee_settings()).is_some());
    assert!(genesis.get_state(&keylet::amendments()).is_some());
}

#[test]
fn networked_genesis_master_only_layout_omits_fee_and_amendments() {
    let mut config = NodeConfig::default();
    config.network.genesis_amendments_disabled = true;
    let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    let node = Node::new_standalone(config, address).unwrap();

    let closed = node.closed_ledgers.blocking_read();
    let genesis = &closed[0];
    assert!(genesis.get_state(&keylet::fee_settings()).is_none());
    assert!(genesis.get_state(&keylet::amendments()).is_none());

    let account_id = decode_account_id(address).unwrap();
    assert!(genesis.get_state(&keylet::account(&account_id)).is_some());
}

#[test]
fn genesis_includes_fee_settings() {
    let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    let genesis = Node::genesis_with_funded_account(address).unwrap();

    let fee_key = keylet::fee_settings();
    let data = genesis
        .get_state(&fee_key)
        .expect("FeeSettings missing from genesis");
    let fee: Value = rxrpl_ledger::sle_codec::decode_state(data).unwrap();
    assert_eq!(fee["LedgerEntryType"].as_str().unwrap(), "FeeSettings");
    assert_eq!(fee["ReserveBase"], 10_000_000);
    assert_eq!(fee["ReserveIncrement"], 2_000_000);
}

#[test]
fn genesis_hash_deterministic() {
    let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    let genesis1 = Node::genesis_with_funded_account(address).unwrap();
    let genesis2 = Node::genesis_with_funded_account(address).unwrap();

    assert_eq!(genesis1.header.hash, genesis2.header.hash);
    assert_eq!(genesis1.header.account_hash, genesis2.header.account_hash);
    assert!(!genesis1.header.hash.is_zero());
}

/// Genesis hash must match rippled-2.6.2 for the same standard master
/// account. Captured empirically from `rippled --standalone --start
/// --quorum=1` with network_id=10000 and the well-known genesis account
/// `rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh`. See issue #76.
#[test]
fn genesis_hash_matches_rippled_2_6_2() {
    let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    let genesis = Node::genesis_with_funded_account(address).unwrap();

    let actual_hash = hex::encode_upper(genesis.header.hash.as_bytes());
    let actual_account_hash = hex::encode_upper(genesis.header.account_hash.as_bytes());

    assert_eq!(
        actual_account_hash, "3791BF543E5B77A17BC454F7A0720E4615760F457135F399DE67C54D7929546D",
        "genesis account_hash diverges from rippled-2.6.2"
    );
    assert_eq!(
        actual_hash, "E158C218A9AF027957A54ECD7D25F4AD35C90B2AAF8DE4956723A17D80F5B3F4",
        "genesis ledger hash diverges from rippled-2.6.2"
    );
}

#[test]
fn compute_quorum_standard_unl() {
    // 35 validators (typical mainnet UNL) → 28 quorum (80%)
    assert_eq!(Node::compute_quorum(35), 28);
}

#[test]
fn compute_quorum_small_list() {
    assert_eq!(Node::compute_quorum(10), 8);
    assert_eq!(Node::compute_quorum(5), 4);
    assert_eq!(Node::compute_quorum(1), 1);
}

#[test]
fn compute_quorum_rounds_up() {
    // 7 * 0.8 = 5.6 → ceil → 6
    assert_eq!(Node::compute_quorum(7), 6);
    // 3 * 0.8 = 2.4 → ceil → 3
    assert_eq!(Node::compute_quorum(3), 3);
}

#[test]
fn compute_quorum_zero_returns_one() {
    assert_eq!(Node::compute_quorum(0), 1);
}

#[test]
fn close_ledger_clamps_close_time_above_parent() {
    // Reproduces the hive consensus-test divergence:
    // two consecutive ledgers closed within the same 10s resolution
    // bucket must NOT carry equal close_time. rippled's effCloseTime
    // (LedgerTiming.h) clamps close_time > parent_close_time + 1s, and
    // rxrpl must do the same to produce byte-identical ledger headers.
    let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    let parent_close = 832_077_840u32;

    let mut child = {
        let parent = {
            let mut g = Node::genesis_with_funded_account(address).unwrap();
            g.header.close_time = parent_close;
            g.header.close_time_resolution = 10;
            g
        };
        Ledger::new_open(&parent)
    };

    // Simulate "wall clock 832_077_842" — would round to 832_077_840
    // (the parent's bucket) without the clamp. After the clamp it
    // must be strictly greater than parent_close.
    let raw_close = parent_close + 2;
    Node::close_ledger(&mut child, raw_close).unwrap();

    assert!(
        child.header.close_time > parent_close,
        "close_time must be > parent_close_time, got close={} parent={}",
        child.header.close_time,
        parent_close,
    );
}

#[test]
fn quorum_auto_set_integration() {
    // Simulate the full flow: ValidatorListReceived → compute_quorum → update_quorum
    // This tests the exact code path from the select! handler.
    use rxrpl_consensus::types::{NodeId as CNodeId, Validation};
    use rxrpl_overlay::validation_aggregator::ValidationAggregator;

    let configured_quorum: Option<usize> = None; // auto mode
    let mut val_aggregator = ValidationAggregator::new(1);

    // Simulate receiving a ValidatorList with 35 validators
    let validator_count = 35usize;
    if configured_quorum.is_none() && validator_count > 0 {
        let new_quorum = Node::compute_quorum(validator_count);
        val_aggregator.update_quorum(new_quorum);
    }

    // Now quorum should be 28. Sending 27 validations should NOT reach quorum.
    let hash = Hash256::new([0xAA; 32]);
    for i in 1..=27u8 {
        let v = Validation {
            node_id: CNodeId(Hash256::new([i; 32])),
            public_key: Vec::new(),
            ledger_hash: hash,
            ledger_seq: 100,
            full: true,
            close_time: 100,
            sign_time: 100,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };
        assert!(val_aggregator.add_validation_at(v, 100).is_none());
    }

    // 28th validation reaches quorum
    let v28 = Validation {
        node_id: CNodeId(Hash256::new([28; 32])),
        public_key: Vec::new(),
        ledger_hash: hash,
        ledger_seq: 100,
        full: true,
        close_time: 100,
        sign_time: 100,
        signature: None,
        amendments: vec![],
        signing_payload: None,
        ..Default::default()
    };
    let result = val_aggregator.add_validation_at(v28, 100);
    assert!(result.is_some());
    assert_eq!(result.unwrap().validation_count, 28);
}

#[test]
fn quorum_not_overridden_when_configured() {
    // When quorum is explicitly configured, ValidatorListReceived should NOT change it
    use rxrpl_overlay::validation_aggregator::ValidationAggregator;

    let configured_quorum: Option<usize> = Some(5); // explicit
    let mut val_aggregator = ValidationAggregator::new(5);

    let validator_count = 35usize;
    // This guard prevents override — same as in the select! handler
    if configured_quorum.is_none() && validator_count > 0 {
        let new_quorum = Node::compute_quorum(validator_count);
        val_aggregator.update_quorum(new_quorum);
    }

    // Quorum should still be 5, not 28
    let hash = Hash256::new([0xBB; 32]);
    for i in 1..=4u8 {
        let v = rxrpl_consensus::types::Validation {
            node_id: rxrpl_consensus::types::NodeId(Hash256::new([i; 32])),
            public_key: Vec::new(),
            ledger_hash: hash,
            ledger_seq: 200,
            full: true,
            close_time: 100,
            sign_time: 100,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };
        assert!(val_aggregator.add_validation_at(v, 100).is_none());
    }
    let v5 = rxrpl_consensus::types::Validation {
        node_id: rxrpl_consensus::types::NodeId(Hash256::new([5; 32])),
        public_key: Vec::new(),
        ledger_hash: hash,
        ledger_seq: 200,
        full: true,
        close_time: 100,
        sign_time: 100,
        signature: None,
        amendments: vec![],
        signing_payload: None,
        ..Default::default()
    };
    assert!(val_aggregator.add_validation_at(v5, 100).is_some());
}

#[test]
fn genesis_binary_encoding_deterministic() {
    let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    let genesis1 = Node::genesis_with_funded_account(address).unwrap();
    let genesis2 = Node::genesis_with_funded_account(address).unwrap();

    // Verify the raw binary data for the account root is identical
    let account_id = decode_account_id(address).unwrap();
    let key = keylet::account(&account_id);
    let data1 = genesis1.get_state(&key).unwrap();
    let data2 = genesis2.get_state(&key).unwrap();
    assert_eq!(data1, data2, "binary encoding must be deterministic");
}

#[cfg(unix)]
fn write_seed_file_with_mode(dir: &std::path::Path, mode: u32) -> std::path::PathBuf {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("seed");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(b"00112233445566778899aabbccddeeff").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn validator_enabled_with_seed_file_loads_seed() {
    let dir = tempfile::tempdir().unwrap();
    let seed_path = write_seed_file_with_mode(dir.path(), 0o600);
    let mut config = NodeConfig::default();
    config.validators.enabled = true;
    config.validators.seed_file = Some(seed_path);

    let node = Node::new(config).expect("node creation should succeed");
    let seed = node.validation_seed().expect("seed should be loaded");
    assert_eq!(seed.as_bytes()[0], 0x00);
    assert_eq!(seed.as_bytes()[15], 0xff);
}

#[cfg(unix)]
#[test]
fn validator_disabled_with_seed_file_logs_warning_and_drops() {
    let dir = tempfile::tempdir().unwrap();
    let seed_path = write_seed_file_with_mode(dir.path(), 0o600);
    let mut config = NodeConfig::default();
    config.validators.enabled = false;
    config.validators.seed_file = Some(seed_path);

    let node = Node::new(config).expect("node creation should succeed");
    assert!(node.validation_seed().is_none());
}

#[cfg(unix)]
#[test]
fn loose_seed_file_mode_fails_node_creation() {
    let dir = tempfile::tempdir().unwrap();
    let seed_path = write_seed_file_with_mode(dir.path(), 0o644);
    let mut config = NodeConfig::default();
    config.validators.enabled = true;
    config.validators.seed_file = Some(seed_path);

    let err = match Node::new(config) {
        Ok(_) => panic!("expected node creation to fail with loose seed mode"),
        Err(e) => e,
    };
    assert!(matches!(err, NodeError::SeedFile(_)), "got {err:?}");
}

#[test]
fn collect_nftoken_ids_finds_ids_in_tx_and_meta() {
    let id_a = "00080000".to_string() + &"A".repeat(56);
    let id_b = "00080000".to_string() + &"B".repeat(56);
    // id in tx_json (burn/offer style) and a different id nested in meta
    // AffectedNodes (mint style).
    let record = serde_json::json!({
        "tx_json": { "TransactionType": "NFTokenBurn", "NFTokenID": id_a },
        "meta": {
            "AffectedNodes": [
                { "CreatedNode": { "NewFields": { "NFTokens": [ { "NFTokenID": id_b } ] } } }
            ]
        }
    });
    let mut out = std::collections::HashSet::new();
    Node::collect_nftoken_ids(&record, &mut out);
    assert_eq!(out.len(), 2);
    assert!(out.contains(&id_a));
    assert!(out.contains(&id_b));
}

#[test]
fn collect_nftoken_ids_ignores_malformed() {
    let record = serde_json::json!({
        "tx_json": { "NFTokenID": "tooshort" },
        "other": { "NFTokenID": 12345 }
    });
    let mut out = std::collections::HashSet::new();
    Node::collect_nftoken_ids(&record, &mut out);
    assert!(out.is_empty());
}
