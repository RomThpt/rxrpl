//! Play-forward replay: advance one ledger by applying its validated
//! transaction set, instead of re-acquiring the moving tip's full state.
//!
//! After the node holds a verified base ledger (e.g. an RPC-bootstrapped
//! snapshot), the cheap way to follow the chain is to fetch each successor's
//! transaction set (bounded by the ledger's transaction count, not the ~19M
//! state entries) and re-apply it on top of the known parent state. The
//! result is checked against the trusted validated header: a faithful replay
//! reproduces the header's `account_hash`, `transaction_hash`, total coins and
//! ledger hash byte-for-byte.

use rxrpl_amendment::Rules;
use rxrpl_ledger::{Ledger, LedgerHeader};
use rxrpl_primitives::Hash256;
use rxrpl_tx_engine::{FeeSettings, TxEngine};

use crate::canonical_tx_set::canonical_order;
use crate::error::NodeError;

/// Result of replaying a transaction set forward onto a parent ledger.
pub struct ReplayOutcome {
    /// The reconstructed closed ledger.
    pub ledger: Ledger,
    /// Transactions that applied successfully.
    pub applied: usize,
    /// Transactions that failed to decode or apply.
    pub failed: usize,
    pub account_hash_match: bool,
    pub tx_hash_match: bool,
    pub ledger_hash_match: bool,
    pub drops_match: bool,
}

impl ReplayOutcome {
    /// True when the replay reproduced every hash and the total coins of the
    /// validated header — i.e. the forward step is provably correct.
    pub fn is_faithful(&self) -> bool {
        self.account_hash_match && self.tx_hash_match && self.ledger_hash_match && self.drops_match
    }
}

/// Replay `txs` (the validated transaction set of `parent.sequence + 1`) onto
/// the parent ledger and verify the result against the trusted `header`.
///
/// `set_hash` is the consensus transaction set's SHAMap root (transactions
/// without metadata) — the salt rippled uses for canonical apply ordering. It
/// is distinct from `header.tx_hash`, which is the closed ledger's
/// transaction tree root over transactions *with* metadata.
pub fn replay_forward(
    parent: &Ledger,
    set_hash: Hash256,
    txs: Vec<(Hash256, Vec<u8>)>,
    header: &LedgerHeader,
    tx_engine: &TxEngine,
    fees: &FeeSettings,
) -> Result<ReplayOutcome, NodeError> {
    let mut ledger = Ledger::new_open(parent);
    // Adopt the validated header's adaptive close-time resolution; `new_open`
    // inherits the parent's, which can differ from the chain's chosen value.
    ledger.header.close_time_resolution = header.close_time_resolution;

    let rules = Rules::new();
    let mut applied = 0usize;
    let mut failed = 0usize;
    for (_txid, blob) in canonical_order(set_hash, txs) {
        let json = match rxrpl_codec::binary::decode(&blob) {
            Ok(v) => v,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        match tx_engine.apply(&json, &mut ledger, &rules, fees) {
            Ok(result) if result.is_success() => applied += 1,
            _ => failed += 1,
        }
    }

    ledger
        .close(header.close_time, header.close_flags)
        .map_err(|e| NodeError::Server(format!("replay close failed: {e}")))?;

    Ok(ReplayOutcome {
        account_hash_match: ledger.header.account_hash == header.account_hash,
        tx_hash_match: ledger.header.tx_hash == header.tx_hash,
        ledger_hash_match: ledger.header.hash == header.hash,
        drops_match: ledger.header.drops == header.drops,
        applied,
        failed,
        ledger,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_codec::address::classic::encode_account_id;
    use rxrpl_primitives::AccountId;
    use rxrpl_tx_engine::{TransactorRegistry, handlers};

    const MASTER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn full_engine() -> TxEngine {
        let mut r = TransactorRegistry::new();
        handlers::register_phase_a(&mut r);
        handlers::register_phase_b(&mut r);
        handlers::register_phase_c1(&mut r);
        handlers::register_phase_c2(&mut r);
        handlers::register_phase_c3(&mut r);
        handlers::register_phase_d1(&mut r);
        handlers::register_phase_d2(&mut r);
        handlers::register_phase_e(&mut r);
        handlers::register_phase_f(&mut r);
        handlers::register_pseudo(&mut r);
        TxEngine::new_without_sig_check(r)
    }

    fn payment(seq: u32, dest: AccountId, amount_drops: u64) -> (Hash256, Vec<u8>) {
        let json = serde_json::json!({
            "TransactionType": "Payment",
            "Account": MASTER,
            "Destination": encode_account_id(&dest),
            "Amount": amount_drops.to_string(),
            "Sequence": seq,
            "Fee": "10",
            "SigningPubKey": "",
        });
        let blob = rxrpl_codec::binary::encode(&json).expect("encode payment");
        let txid = rxrpl_crypto::sha512_half::sha512_half(&[&blob]);
        (txid, blob)
    }

    fn master_genesis() -> Ledger {
        crate::node::Node::genesis_with_master_account_only(MASTER).expect("genesis")
    }

    #[test]
    fn faithful_replay_reproduces_the_validated_header() {
        let parent = master_genesis();
        let engine = full_engine();
        let fees = FeeSettings::default();
        let salt = Hash256::new([0x5e; 32]);

        let txs = vec![
            payment(1, AccountId([0xaa; 20]), 1_000_000_000),
            payment(2, AccountId([0xbb; 20]), 2_000_000_000),
        ];

        // Pass 1: derive the "validated" header by replaying against a blank
        // target (matches all false), then trust the produced header.
        let blank = LedgerHeader::new();
        let first = replay_forward(&parent, salt, txs.clone(), &blank, &engine, &fees)
            .expect("first replay");
        assert_eq!(first.applied, 2, "both payments should apply");
        assert_eq!(first.failed, 0);
        let truth = first.ledger.header.clone();
        assert_ne!(truth.account_hash, parent.header.account_hash);
        assert!(!truth.tx_hash.is_zero());

        // Pass 2: replaying the same set against the produced header is faithful.
        let second =
            replay_forward(&parent, salt, txs, &truth, &engine, &fees).expect("second replay");
        assert!(second.is_faithful(), "replay must reproduce the header");
        assert_eq!(second.applied, 2);
    }

    #[test]
    fn replay_is_deterministic() {
        let parent = master_genesis();
        let engine = full_engine();
        let fees = FeeSettings::default();
        let salt = Hash256::new([0x17; 32]);
        let txs = vec![
            payment(1, AccountId([0x01; 20]), 500_000_000),
            payment(2, AccountId([0x02; 20]), 700_000_000),
        ];
        let blank = LedgerHeader::new();
        let a = replay_forward(&parent, salt, txs.clone(), &blank, &engine, &fees).unwrap();
        let b = replay_forward(&parent, salt, txs, &blank, &engine, &fees).unwrap();
        assert_eq!(a.ledger.header.hash, b.ledger.header.hash);
        assert_eq!(a.ledger.header.account_hash, b.ledger.header.account_hash);
    }
}
