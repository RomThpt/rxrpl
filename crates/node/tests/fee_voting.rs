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

/// Read the committed `FeeSettings.BaseFee` (canonical binary -> JSON). The
/// field may render as a decimal string, hex string, or number depending on the
/// codec; accept all three.
fn committed_base_fee(ledger: &Ledger) -> Option<u64> {
    let data = ledger.get_state(&keylet::fee_settings())?;
    let obj = rxrpl_ledger::sle_codec::decode_state(data).expect("FeeSettings decodes");
    match &obj["BaseFee"] {
        Value::String(s) => s
            .parse::<u64>()
            .or_else(|_| u64::from_str_radix(s, 16))
            .ok(),
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
    let rules = Rules::new();
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
    let rules = Rules::new();
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
