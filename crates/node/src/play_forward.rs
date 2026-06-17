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
use rxrpl_crypto::hash_prefix::HashPrefix;
use rxrpl_crypto::sha512_half::sha512_half;
use rxrpl_ledger::{Ledger, LedgerHeader};
use rxrpl_primitives::Hash256;
use rxrpl_tx_engine::{FeeSettings, TxEngine};
use serde_json::Value;

use crate::canonical_tx_set::canonical_order;
use crate::error::NodeError;

/// A transaction set as `(txid, canonical_blob)` pairs.
pub type TxSet = Vec<(Hash256, Vec<u8>)>;

/// Compute the transaction id of a canonical (no-metadata) transaction blob:
/// `SHA512Half(TXN\0 || blob)`. This equals the leaf hash a transaction takes
/// in the consensus set SHAMap, so the set of these ids reconstructs the salt.
pub fn transaction_id(blob: &[u8]) -> Hash256 {
    sha512_half(&[&HashPrefix::TRANSACTION_ID.to_bytes(), blob])
}

/// Parse a `result.ledger` JSON object (from a `ledger` RPC call) into a
/// `LedgerHeader`. Numeric fields may arrive as JSON numbers or strings
/// depending on the server; both are accepted.
pub fn parse_header(l: &Value) -> Result<LedgerHeader, NodeError> {
    let hash32 = |v: Option<&Value>, what: &str| -> Result<Hash256, NodeError> {
        let s = v
            .and_then(Value::as_str)
            .ok_or_else(|| NodeError::Server(format!("missing {what}")))?;
        let b = hex::decode(s).map_err(|e| NodeError::Server(format!("bad {what} hex: {e}")))?;
        let arr: [u8; 32] = b
            .as_slice()
            .try_into()
            .map_err(|_| NodeError::Server(format!("{what} not 32 bytes")))?;
        Ok(Hash256::new(arr))
    };
    let num = |v: Option<&Value>| -> u64 {
        v.and_then(|x| {
            x.as_u64()
                .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0)
    };

    let mut header = LedgerHeader::new();
    header.sequence = num(l.get("ledger_index")) as u32;
    header.drops = num(l.get("total_coins"));
    header.parent_hash = hash32(l.get("parent_hash"), "parent_hash")?;
    header.tx_hash = hash32(l.get("transaction_hash"), "transaction_hash")?;
    header.account_hash = hash32(l.get("account_hash"), "account_hash")?;
    header.parent_close_time = num(l.get("parent_close_time")) as u32;
    header.close_time = num(l.get("close_time")) as u32;
    header.close_time_resolution = num(l.get("close_time_resolution")) as u8;
    header.close_flags = num(l.get("close_flags")) as u8;
    header.hash = hash32(l.get("ledger_hash"), "ledger_hash")?;
    Ok(header)
}

/// Parse the `result` of a `ledger` RPC call made with
/// `transactions: true, expand: true, binary: true` into the consensus set
/// hash (the canonical-ordering salt) and the `(txid, tx_blob)` pairs to
/// replay. The blobs are the signed transactions without metadata, exactly
/// what `replay_forward` re-applies.
pub fn parse_tx_set(result: &Value) -> Result<(Hash256, TxSet), NodeError> {
    let entries = result
        .get("ledger")
        .and_then(|l| l.get("transactions"))
        .and_then(Value::as_array)
        .ok_or_else(|| NodeError::Server("missing ledger.transactions in response".into()))?;

    let mut txs = Vec::with_capacity(entries.len());
    for entry in entries {
        // Binary+expand entries are `{tx_blob, meta}`; a bare hex string can
        // also appear when a server inlines the blob directly.
        let blob_hex = entry
            .get("tx_blob")
            .or(Some(entry))
            .and_then(Value::as_str)
            .ok_or_else(|| NodeError::Server("transaction entry missing tx_blob".into()))?;
        let blob = hex::decode(blob_hex)
            .map_err(|e| NodeError::Server(format!("bad tx_blob hex: {e}")))?;
        let txid = transaction_id(&blob);
        txs.push((txid, blob));
    }

    let ids: Vec<Hash256> = txs.iter().map(|(id, _)| *id).collect();
    let set_hash = rxrpl_shamap::transaction_set_root(&ids);
    Ok((set_hash, txs))
}

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
    txs: TxSet,
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

    #[test]
    fn parse_tx_set_extracts_blobs_and_salt() {
        let built = [
            payment(1, AccountId([0xaa; 20]), 1_000_000_000),
            payment(2, AccountId([0xbb; 20]), 2_000_000_000),
        ];
        let result = serde_json::json!({
            "ledger": {
                "transactions": built
                    .iter()
                    .map(|(_, blob)| serde_json::json!({
                        "tx_blob": hex::encode_upper(blob),
                        "meta": "",
                    }))
                    .collect::<Vec<_>>(),
            }
        });

        let (set_hash, txs) = parse_tx_set(&result).expect("parse");
        assert_eq!(txs.len(), 2);
        // Recovered blobs match the originals, and ids are the canonical txid.
        for ((got_id, got_blob), (_, blob)) in txs.iter().zip(built.iter()) {
            assert_eq!(got_blob, blob);
            assert_eq!(*got_id, transaction_id(blob));
        }
        let ids: Vec<Hash256> = txs.iter().map(|(id, _)| *id).collect();
        assert_eq!(set_hash, rxrpl_shamap::transaction_set_root(&ids));
        assert!(!set_hash.is_zero());
    }

    #[test]
    fn parse_header_reads_numbers_and_strings() {
        let h32 = |b: u8| hex::encode_upper([b; 32]);
        let l = serde_json::json!({
            "ledger_index": 104972441u64,        // number
            "total_coins": "99988765432100000",  // string
            "parent_hash": h32(0x11),
            "transaction_hash": h32(0x22),
            "account_hash": h32(0x33),
            "parent_close_time": 781234560u64,
            "close_time": "781234563",
            "close_time_resolution": 10u64,
            "close_flags": 0u64,
            "ledger_hash": h32(0x44),
        });
        let header = parse_header(&l).expect("parse header");
        assert_eq!(header.sequence, 104972441);
        assert_eq!(header.drops, 99988765432100000);
        assert_eq!(header.close_time, 781234563);
        assert_eq!(header.close_time_resolution, 10);
        assert_eq!(header.account_hash, Hash256::new([0x33; 32]));
        assert_eq!(header.hash, Hash256::new([0x44; 32]));
    }
}
