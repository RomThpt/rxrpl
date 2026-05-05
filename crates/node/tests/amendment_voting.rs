use rxrpl_amendment::feature::{Feature, feature_id};
use rxrpl_amendment::voting::{self, AmendmentAction, FLAG_LEDGER_INTERVAL};
use rxrpl_amendment::{AmendmentTable, FeatureRegistry, Rules, is_flag_ledger};
use rxrpl_ledger::Ledger;
use rxrpl_node::Node;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::keylet;
use rxrpl_tx_engine::{FeeSettings, TransactorRegistry, TxEngine};
use serde_json::Value;

fn make_engine_with_pseudo() -> TxEngine {
    let mut registry = TransactorRegistry::new();
    rxrpl_tx_engine::handlers::register_pseudo(&mut registry);
    TxEngine::new_without_sig_check(registry)
}

fn test_registry() -> FeatureRegistry {
    let mut reg = FeatureRegistry::new();
    reg.register(Feature::new("TestAmendmentA", true));
    reg.register(Feature::new("TestAmendmentB", true));
    reg.register(Feature::new("TestAmendmentC", false)); // not voted for
    reg.register(Feature::retired("RetiredAmendment"));
    reg
}

/// Verify that amendment voting on a non-flag ledger is a no-op.
#[test]
fn voting_skipped_on_non_flag_ledger() {
    let reg = test_registry();
    let mut table = AmendmentTable::new(&reg, 1000);
    let engine = make_engine_with_pseudo();
    let fees = FeeSettings::default();

    let mut ledger = Ledger::genesis();

    // Sequence 100 is not a flag ledger
    let own_votes = table.get_votes();
    let rules = Node::apply_amendment_voting(
        &mut ledger,
        &engine,
        &mut table,
        &fees,
        10,
        &[own_votes],
        1000,
        100, // not a flag ledger
    );

    // No amendment pseudo-txs should have been applied
    let amendments_key = keylet::amendments();
    assert!(ledger.get_state(&amendments_key).is_none());

    // Rules should still reflect initial state (only retired enabled)
    let retired_id = reg.id_for_name("RetiredAmendment").unwrap();
    assert!(rules.enabled(&retired_id));
}

/// Test that on a flag ledger with enough votes, amendments gain majority.
#[test]
fn flag_ledger_amendments_gain_majority() {
    let reg = test_registry();
    let mut table = AmendmentTable::new(&reg, 100_000); // large window
    let engine = make_engine_with_pseudo();
    let fees = FeeSettings::default();

    let mut ledger = Ledger::genesis();

    let id_a = reg.id_for_name("TestAmendmentA").unwrap();
    let id_b = reg.id_for_name("TestAmendmentB").unwrap();

    // Simulate 10 validators all voting for TestAmendmentA and TestAmendmentB
    let validator_votes: Vec<Vec<Hash256>> = (0..10).map(|_| vec![id_a, id_b]).collect();

    let _rules = Node::apply_amendment_voting(
        &mut ledger,
        &engine,
        &mut table,
        &fees,
        10,
        &validator_votes,
        1000,
        FLAG_LEDGER_INTERVAL, // flag ledger
    );

    // Both amendments should now have majority recorded
    assert!(table.has_majority(&id_a));
    assert!(table.has_majority(&id_b));

    // The Amendments SLE should exist with Majorities entries
    let amendments_key = keylet::amendments();
    let data = ledger.get_state(&amendments_key).unwrap();
    let obj: Value = rxrpl_ledger::sle_codec::decode_state(data).unwrap();
    let majorities = obj["Majorities"].as_array().unwrap();
    assert_eq!(majorities.len(), 2);
}

/// Test full lifecycle: gain majority -> wait majority window -> activate.
#[test]
fn amendment_activation_lifecycle() {
    let reg = test_registry();
    let majority_time = FLAG_LEDGER_INTERVAL; // activate after 1 more flag ledger
    let mut table = AmendmentTable::new(&reg, majority_time);
    let engine = make_engine_with_pseudo();
    let fees = FeeSettings::default();

    let id_a = reg.id_for_name("TestAmendmentA").unwrap();

    // Step 1: Gain majority on first flag ledger
    let mut ledger1 = Ledger::genesis();
    let validator_votes: Vec<Vec<Hash256>> = (0..10).map(|_| vec![id_a]).collect();

    Node::apply_amendment_voting(
        &mut ledger1,
        &engine,
        &mut table,
        &fees,
        10,
        &validator_votes,
        1000,
        FLAG_LEDGER_INTERVAL,
    );

    assert!(table.has_majority(&id_a));
    assert!(!table.is_enabled(&id_a));

    // Step 2: On the next flag ledger after majority_time, amendment activates
    let mut ledger2 = Ledger::genesis();
    Node::apply_amendment_voting(
        &mut ledger2,
        &engine,
        &mut table,
        &fees,
        10,
        &validator_votes,
        2000,
        FLAG_LEDGER_INTERVAL + majority_time, // past majority window
    );

    // Amendment should now be enabled
    assert!(table.is_enabled(&id_a));

    // The Amendments SLE should have the amendment in the Amendments list
    let amendments_key = keylet::amendments();
    let data = ledger2.get_state(&amendments_key).unwrap();
    let obj: Value = rxrpl_ledger::sle_codec::decode_state(data).unwrap();
    let amendments = obj["Amendments"].as_array().unwrap();
    let id_hex = hex::encode(id_a.as_bytes()).to_uppercase();
    assert!(
        amendments.iter().any(|v| v.as_str() == Some(&id_hex)),
        "Amendment {} not found in Amendments list: {:?}",
        id_hex,
        amendments
    );

    // Rules snapshot should reflect the activation
    let rules = table.build_rules();
    assert!(rules.enabled(&id_a));
}

/// Test that losing majority removes from Majorities and resets the timer.
#[test]
fn amendment_loses_majority() {
    let reg = test_registry();
    let mut table = AmendmentTable::new(&reg, 100_000);
    let engine = make_engine_with_pseudo();
    let fees = FeeSettings::default();

    let id_a = reg.id_for_name("TestAmendmentA").unwrap();

    // Step 1: Gain majority
    let mut ledger1 = Ledger::genesis();
    let votes_strong: Vec<Vec<Hash256>> = (0..10).map(|_| vec![id_a]).collect();

    Node::apply_amendment_voting(
        &mut ledger1,
        &engine,
        &mut table,
        &fees,
        10,
        &votes_strong,
        1000,
        FLAG_LEDGER_INTERVAL,
    );
    assert!(table.has_majority(&id_a));

    // Step 2: Lose majority (only 5 of 10 vote)
    let mut ledger2 = Ledger::genesis();
    let votes_weak: Vec<Vec<Hash256>> = (0..5).map(|_| vec![id_a]).collect();

    Node::apply_amendment_voting(
        &mut ledger2,
        &engine,
        &mut table,
        &fees,
        10,
        &votes_weak,
        2000,
        FLAG_LEDGER_INTERVAL * 2,
    );

    // Majority should be cleared
    assert!(!table.has_majority(&id_a));
    assert!(!table.is_enabled(&id_a));
}

/// Test that the voting module correctly counts votes from multiple validators.
#[test]
fn vote_counting_from_validations() {
    let id1 = feature_id("FeatureX");
    let id2 = feature_id("FeatureY");
    let id3 = feature_id("FeatureZ");

    let validator_votes = vec![
        vec![id1, id2, id3],
        vec![id1, id2],
        vec![id1],
        vec![id2, id3],
        vec![id1, id3],
    ];

    let counts = voting::count_votes(&validator_votes);
    assert_eq!(counts[&id1], 4); // 4 out of 5 validators
    assert_eq!(counts[&id2], 3); // 3 out of 5 validators
    assert_eq!(counts[&id3], 3); // 3 out of 5 validators
}

/// Test that pseudo-tx JSON generation matches what EnableAmendmentTransactor expects.
#[test]
fn pseudo_tx_format_compatible_with_transactor() {
    let id = feature_id("TestAmendment");
    let engine = make_engine_with_pseudo();
    let fees = FeeSettings::default();
    let rules = Rules::new();

    // Test all three action types
    for action in &[
        AmendmentAction::GotMajority {
            amendment_id: id,
            close_time: 1000,
        },
        AmendmentAction::LostMajority { amendment_id: id },
        AmendmentAction::Activate { amendment_id: id },
    ] {
        let tx = voting::make_enable_amendment_tx(action);
        assert_eq!(tx["TransactionType"], "EnableAmendment");
        assert!(tx["Amendment"].is_string());

        // Verify the amendment hash is a valid 64-char hex string
        let amendment_hex = tx["Amendment"].as_str().unwrap();
        assert_eq!(amendment_hex.len(), 64);
        assert!(amendment_hex.chars().all(|c| c.is_ascii_hexdigit()));

        // Verify the engine can apply it to a genesis ledger
        let mut ledger = Ledger::genesis();
        let result = engine.apply(&tx, &mut ledger, &rules, &fees);
        assert!(
            result.is_ok(),
            "engine.apply failed for {:?}: {:?}",
            action,
            result
        );
    }
}

/// Verify is_flag_ledger boundary conditions.
#[test]
fn flag_ledger_boundaries() {
    assert!(!is_flag_ledger(0));
    assert!(!is_flag_ledger(1));
    assert!(!is_flag_ledger(255));
    assert!(is_flag_ledger(256));
    assert!(!is_flag_ledger(257));
    assert!(is_flag_ledger(512));
    assert!(is_flag_ledger(1024));
    assert!(is_flag_ledger(256 * 100));
}
