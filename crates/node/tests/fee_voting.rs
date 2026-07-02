//! Integration tests for flag-ledger fee/reserve voting (`Node::apply_fee_voting`).
//!
//! These exercise the node-level driver end to end: tally the votes, build the
//! `SetFee` pseudo-transaction, and apply it to the ledger via the tx-engine.
//! The pure tally (`VotableValue`) is unit-tested in `rxrpl_amendment::fee_voting`;
//! here we assert the committed ledger effect and the consensus-safety no-ops.
//!
//! Committed ledger state is canonical binary, so `FeeSettings` values are read
//! back through `sle_codec::decode_state`, not `serde_json`.

use rxrpl_amendment::Rules;
use rxrpl_amendment::fee_voting::{FeeSettingsVote, FeeVoteEntry};
use rxrpl_ledger::Ledger;
use rxrpl_node::Node;
use rxrpl_protocol::keylet;
use rxrpl_tx_engine::{FeeSettings, TransactorRegistry, TxEngine};
use serde_json::Value;

fn make_engine() -> TxEngine {
    let mut registry = TransactorRegistry::new();
    rxrpl_tx_engine::handlers::register_pseudo(&mut registry);
    TxEngine::new_without_sig_check(registry)
}

fn current_of(fees: &FeeSettings) -> FeeSettingsVote {
    FeeSettingsVote {
        base_fee: fees.base_fee,
        reserve_base: fees.reserve_base,
        reserve_increment: fees.reserve_increment,
    }
}

/// Rules with `XRPFees` enabled, so voting emits the modern drops variant
/// (`BaseFeeDrops`, an XRP amount) rather than the legacy `BaseFee` UInt64.
fn drops_rules() -> Rules {
    Rules::from_enabled([rxrpl_amendment::feature_id("XRPFees")])
}

/// Read the committed `FeeSettings.BaseFeeDrops` (canonical binary -> JSON).
/// The drops field is an XRP amount, rendered as a decimal string or number.
fn committed_base_fee(ledger: &Ledger) -> Option<u64> {
    let data = ledger.get_state(&keylet::fee_settings())?;
    let obj = rxrpl_ledger::sle_codec::decode_state(data).expect("FeeSettings decodes");
    match &obj["BaseFeeDrops"] {
        Value::String(s) => s.parse::<u64>().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

/// Off a flag ledger, fee voting never runs — even with a differing target.
#[test]
fn skipped_on_non_flag_ledger() {
    let engine = make_engine();
    let fees = FeeSettings::default();
    let rules = Rules::new();
    let mut ledger = Ledger::genesis();

    let cur = current_of(&fees);
    let target = FeeSettingsVote {
        base_fee: cur.base_fee + 5,
        ..cur
    };
    // seq 100 is not a flag ledger (100 % 256 != 0).
    Node::apply_fee_voting(&mut ledger, &engine, &fees, &rules, target, &[], 100);

    assert!(
        ledger.get_state(&keylet::fee_settings()).is_none(),
        "no SetFee should be applied off a flag ledger"
    );
}

/// With `target == current` (the default, unconfigured node) the vote range
/// collapses to a single point, so no peer vote can move it and no `SetFee` is
/// produced — the consensus-safe no-op that keeps a non-voting node unchanged.
#[test]
fn no_op_when_target_equals_current_even_with_peer_votes() {
    let engine = make_engine();
    let fees = FeeSettings::default();
    let rules = Rules::new();
    let mut ledger = Ledger::genesis();

    let cur = current_of(&fees);
    // Peers push a higher base fee, but our target == current.
    let peer = FeeVoteEntry {
        base_fee: Some(cur.base_fee + 100),
        reserve_base: None,
        reserve_increment: None,
    };
    Node::apply_fee_voting(
        &mut ledger,
        &engine,
        &fees,
        &rules,
        cur,
        &[peer, peer, peer],
        256,
    );

    assert!(
        ledger.get_state(&keylet::fee_settings()).is_none(),
        "target==current must never move, regardless of peer votes"
    );
}

/// On a flag ledger with a configured target, the self-vote alone drives the
/// value to the target and a `SetFee` rewrites the `FeeSettings` object.
#[test]
fn applies_setfee_on_flag_ledger_when_configured() {
    let engine = make_engine();
    let fees = FeeSettings::default();
    let rules = drops_rules();
    let mut ledger = Ledger::genesis();

    let cur = current_of(&fees);
    let target = FeeSettingsVote {
        base_fee: cur.base_fee + 5,
        ..cur
    };
    Node::apply_fee_voting(&mut ledger, &engine, &fees, &rules, target, &[], 256);

    assert_eq!(
        committed_base_fee(&ledger),
        Some(cur.base_fee + 5),
        "the SetFee should have written BaseFee = target"
    );
}

/// A peer super-majority for an in-range value beats our lone self-vote,
/// exactly as rippled's weighted median does.
#[test]
fn peer_majority_beats_self_vote() {
    let engine = make_engine();
    let fees = FeeSettings::default();
    let rules = drops_rules();
    let mut ledger = Ledger::genesis();

    let cur = current_of(&fees);
    // We want +10; four peers want +2 (in range) -> consensus is +2.
    let target = FeeSettingsVote {
        base_fee: cur.base_fee + 10,
        ..cur
    };
    let peer = FeeVoteEntry {
        base_fee: Some(cur.base_fee + 2),
        reserve_base: Some(cur.reserve_base),
        reserve_increment: Some(cur.reserve_increment),
    };
    Node::apply_fee_voting(
        &mut ledger,
        &engine,
        &fees,
        &rules,
        target,
        &[peer, peer, peer, peer],
        256,
    );

    assert_eq!(
        committed_base_fee(&ledger),
        Some(cur.base_fee + 2),
        "peer majority (+2) must win over our self-vote (+10)"
    );
}

// --- Byte-exact SetFee serialization vs a rippled 3.2.0 oracle ---
//
// The SetFee pseudo-tx must serialize identically to rippled's
// `FeeVoteImpl::doVoting` STTx, or the transaction-tree hash forks cross-impl.
// Oracle bytes were captured from rippled 3.2.0 (source + a real mainnet SetFee
// whose txid recomputes exactly). Scenario: BaseFee 20 drops, ReserveBase 10 XRP
// (10_000_000), ReserveIncrement 2 XRP (2_000_000), flag ledger 256 -> tx in 257.

#[test]
fn setfee_drops_serialization_is_byte_exact_vs_rippled() {
    let new = FeeSettingsVote {
        base_fee: 20,
        reserve_base: 10_000_000,
        reserve_increment: 2_000_000,
    };
    let tx = rxrpl_amendment::fee_voting::make_set_fee_tx(new, 256, true);
    let bytes = rxrpl_codec::binary::encode(&tx).expect("SetFee encodes");
    let expected = "120065240000000026000001016840000000000000006016400000000000001460174000000000989680601840000000001e8480730081140000000000000000000000000000000000000000";
    assert_eq!(
        hex::encode(&bytes),
        expected,
        "drops SetFee must match rippled byte-for-byte"
    );
}

#[test]
fn setfee_legacy_serialization_is_byte_exact_vs_rippled() {
    let new = FeeSettingsVote {
        base_fee: 20,
        reserve_base: 10_000_000,
        reserve_increment: 2_000_000,
    };
    let tx = rxrpl_amendment::fee_voting::make_set_fee_tx(new, 256, false);
    let bytes = rxrpl_codec::binary::encode(&tx).expect("SetFee encodes");
    let expected = "12006524000000002600000101201e0000000a201f009896802020001e8480350000000000000014684000000000000000730081140000000000000000000000000000000000000000";
    assert_eq!(
        hex::encode(&bytes),
        expected,
        "legacy SetFee must match rippled byte-for-byte"
    );
}
