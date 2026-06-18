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

/// Build the amendment `Rules` in force for a ledger from its `Amendments`
/// state object, the way rippled derives them. Returns empty rules (every
/// amendment off) when the object is absent — correct for pre-amendment
/// ledgers. This is the source of truth for amendment-gated apply logic, so
/// replaying or applying onto a ledger reproduces its era's behaviour.
pub fn rules_for_ledger(ledger: &Ledger) -> Rules {
    let enabled = ledger
        .get_state(&rxrpl_protocol::keylet::amendments())
        .and_then(|b| rxrpl_ledger::sle_codec::decode_state(b).ok())
        .and_then(|v| {
            v.get("Amendments")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str())
                        .filter_map(|s| hex::decode(s).ok())
                        .filter_map(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
                        .map(Hash256::new)
                        .collect::<Vec<_>>()
                })
        })
        .unwrap_or_default();
    Rules::from_enabled(enabled)
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

    // Amendments in force are those enabled in the (inherited) parent state, so
    // each replayed ledger applies with its own era's amendment-gated logic.
    let rules = rules_for_ledger(&ledger);

    // Decode once, in canonical order. Anything that fails to decode is dropped.
    let total = txs.len();
    let mut pending: Vec<Value> = canonical_order(set_hash, txs)
        .into_iter()
        .filter_map(|(_id, blob)| rxrpl_codec::binary::decode(&blob).ok())
        .collect();

    // rippled applies the set over multiple passes: each pass applies the
    // still-pending transactions in canonical order, deferring any that return
    // a retriable (`ter`) result to the next pass. The loop ends when a pass
    // resolves nothing more, so a transaction whose precondition is satisfied
    // only by a later-canonical transaction still applies. Without this, a
    // single pass would drop such transactions and diverge from the chain.
    let mut applied = 0usize;
    loop {
        let before = pending.len();
        let mut deferred = Vec::new();
        for json in std::mem::take(&mut pending) {
            match tx_engine.apply(&json, &mut ledger, &rules, fees) {
                Ok(result) if result.is_retryable() => deferred.push(json),
                Ok(result) => {
                    if result.is_claimed() {
                        applied += 1;
                    }
                }
                Err(_) => {}
            }
        }
        pending = deferred;
        if pending.is_empty() || pending.len() == before {
            break;
        }
    }
    let failed = total - applied;

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
    fn rules_for_ledger_reads_enabled_amendments() {
        use rxrpl_amendment::feature::feature_id;
        let mut ledger = Ledger::genesis();
        let sorted = feature_id("SortedDirectories");
        // No Amendments object yet -> nothing enabled (pre-amendment ledgers).
        assert!(!rules_for_ledger(&ledger).enabled(&sorted));

        let amendments = serde_json::json!({
            "LedgerEntryType": "Amendments",
            "Amendments": [hex::encode_upper(sorted.as_bytes())],
            "Flags": 0,
        });
        let bytes = rxrpl_ledger::sle_codec::encode_sle(&serde_json::to_vec(&amendments).unwrap())
            .unwrap();
        ledger
            .put_state(rxrpl_protocol::keylet::amendments(), bytes)
            .unwrap();

        assert!(rules_for_ledger(&ledger).enabled(&sorted));
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

    /// Live fidelity check: our canonical ordering must reproduce rippled's
    /// real apply order (`metaData.TransactionIndex`) on a mainnet ledger.
    ///
    /// Ignored — it needs network access to a full-history rippled. Run with:
    /// `RXRPL_PLAY_FORWARD_RPC=http://host:5005 cargo test -p rxrpl-node \
    ///  --lib canonical_order_matches_mainnet_apply_order -- --ignored --nocapture`
    #[test]
    #[ignore = "hits a live mainnet RPC server"]
    fn canonical_order_matches_mainnet_apply_order() {
        let Ok(url) = std::env::var("RXRPL_PLAY_FORWARD_RPC") else {
            eprintln!("RXRPL_PLAY_FORWARD_RPC unset; skipping live check");
            return;
        };
        let ledger_index: u64 = std::env::var("RXRPL_PLAY_FORWARD_LEDGER")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(104983000);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let resp: Value = rt.block_on(async {
            let client = reqwest::Client::builder()
                .danger_accept_invalid_certs(true)
                .build()
                .unwrap();
            client
                .post(&url)
                .json(&serde_json::json!({
                    "method": "ledger",
                    "params": [{
                        "ledger_index": ledger_index,
                        "transactions": true,
                        "expand": true,
                        "binary": false,
                    }]
                }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap()
        });

        let entries = resp["result"]["ledger"]["transactions"]
            .as_array()
            .expect("transactions array");
        assert!(!entries.is_empty(), "ledger has no transactions");

        let mut set: TxSet = Vec::new();
        let mut expected: Vec<(u64, Hash256)> = Vec::new();
        for t in entries {
            let account = t["Account"].as_str().expect("Account");
            let sequence = t["Sequence"].as_u64().unwrap_or(0);
            let txid_bytes: [u8; 32] = hex::decode(t["hash"].as_str().expect("hash"))
                .unwrap()
                .try_into()
                .unwrap();
            let txid = Hash256::new(txid_bytes);
            let apply_index = t["metaData"]["TransactionIndex"]
                .as_u64()
                .expect("TransactionIndex");
            let blob = rxrpl_codec::binary::encode(&serde_json::json!({
                "Account": account,
                "Sequence": sequence,
            }))
            .unwrap();
            set.push((txid, blob));
            expected.push((apply_index, txid));
        }

        let ids: Vec<Hash256> = set.iter().map(|(id, _)| *id).collect();
        let set_hash = rxrpl_shamap::transaction_set_root(&ids);
        let parent_hash = parse_header(&resp["result"]["ledger"])
            .map(|h| h.parent_hash)
            .unwrap_or(Hash256::ZERO);
        let ledger_hash = parse_header(&resp["result"]["ledger"])
            .map(|h| h.hash)
            .unwrap_or(Hash256::ZERO);

        expected.sort_by_key(|(idx, _)| *idx);
        let want: Vec<Hash256> = expected.into_iter().map(|(_, id)| id).collect();

        // rippled's final order is a concatenation of retry passes, each
        // strictly increasing in OUR canonical index. The correct salt
        // minimises the number of such runs; a wrong salt scrambles the
        // inter-account order into near-random descents. The set hash must
        // beat every other candidate, proving it is rippled's salt.
        let runs = |salt: Hash256| -> usize {
            let got: Vec<Hash256> = canonical_order(salt, set.clone())
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            let idx: std::collections::HashMap<Hash256, usize> =
                got.iter().enumerate().map(|(i, h)| (*h, i)).collect();
            let s: Vec<usize> = want.iter().map(|h| idx[h]).collect();
            s.windows(2).filter(|w| w[1] <= w[0]).count() + 1
        };
        let set_runs = runs(set_hash);
        eprintln!(
            "ledger #{ledger_index}: {} txs | runs: set_hash={set_runs} zero={} parent={} ledger={}",
            want.len(),
            runs(Hash256::ZERO),
            runs(parent_hash),
            runs(ledger_hash),
        );
        assert!(
            set_runs < runs(Hash256::ZERO)
                && set_runs < runs(parent_hash)
                && set_runs < runs(ledger_hash),
            "set hash must be the best canonical-ordering salt"
        );
        assert!(
            set_runs <= want.len() / 4 + 2,
            "too many retry passes ({set_runs}); canonical sort likely wrong"
        );
    }

    /// Full end-to-end fidelity check against real mainnet data: bootstrap a
    /// parent ledger's state, play its successor's transaction set forward, and
    /// require the result to reproduce the validated header byte-for-byte.
    ///
    /// Uses an early ledger (small state, no active amendments) so the parent
    /// state downloads in one pass and empty `Rules` are correct. Ignored —
    /// needs network. Run with:
    /// `RXRPL_PLAY_FORWARD_RPC=http://host:5005 cargo test -p rxrpl-node \
    ///  --lib play_forward_end_to_end_mainnet -- --ignored --nocapture`
    #[test]
    #[ignore = "hits a live mainnet RPC server"]
    fn play_forward_end_to_end_mainnet() {
        let Ok(url) = std::env::var("RXRPL_PLAY_FORWARD_RPC") else {
            eprintln!("RXRPL_PLAY_FORWARD_RPC unset; skipping");
            return;
        };
        let next: u32 = std::env::var("RXRPL_PLAY_FORWARD_LEDGER")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(316000);
        let parent_seq = next - 1;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap();
        let rpc = |params: serde_json::Value| -> Value {
            rt.block_on(async {
                client
                    .post(&url)
                    .json(&params)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap()
            })
        };

        // 1. Parent header + full account state (paginated), verify root.
        let parent_resp = rpc(serde_json::json!({
            "method":"ledger",
            "params":[{"ledger_index":parent_seq,"transactions":false,"expand":false}]
        }));
        let parent_header = parse_header(&parent_resp["result"]["ledger"]).expect("parent header");

        let mut state = rxrpl_shamap::SHAMap::account_state();
        let mut marker: Option<String> = None;
        loop {
            let mut p = serde_json::json!({"ledger_index":parent_seq,"binary":true,"limit":2048});
            if let Some(m) = &marker {
                p["marker"] = serde_json::Value::String(m.clone());
            }
            let r = rpc(serde_json::json!({"method":"ledger_data","params":[p]}));
            let result = &r["result"];
            for e in result["state"].as_array().unwrap() {
                let key: [u8; 32] = hex::decode(e["index"].as_str().unwrap())
                    .unwrap()
                    .try_into()
                    .unwrap();
                let data = hex::decode(e["data"].as_str().unwrap()).unwrap();
                state.put(Hash256::new(key), data).unwrap();
            }
            marker = result["marker"].as_str().map(|s| s.to_string());
            if marker.is_none() {
                break;
            }
        }
        assert_eq!(
            state.root_hash(),
            parent_header.account_hash,
            "downloaded parent state root must equal validated account_hash"
        );

        let mut parent = Ledger::from_catchup(parent_seq, parent_header.hash, state);
        parent.header = parent_header;

        // Read the ledger's real FeeSettings (reserves differ per era) instead
        // of defaults, so reserve checks reproduce the chain.
        let fees = parent
            .state_map
            .get(&rxrpl_protocol::keylet::fee_settings())
            .and_then(|b| rxrpl_codec::binary::decode(b).ok())
            .map(|fs| FeeSettings {
                base_fee: fs
                    .get("BaseFee")
                    .and_then(|v| v.as_str())
                    .and_then(|s| u64::from_str_radix(s, 16).ok())
                    .unwrap_or(10),
                reserve_base: fs
                    .get("ReserveBase")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10_000_000),
                reserve_increment: fs
                    .get("ReserveIncrement")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50_000_000),
            })
            // Early ledgers predate the FeeSettings SLE; reserves were protocol
            // constants (200 XRP base, 50 XRP per owner) at that era.
            .unwrap_or(FeeSettings {
                base_fee: 10,
                reserve_base: 200_000_000,
                reserve_increment: 50_000_000,
            });

        // 2. Successor header (binary:false → JSON fields) and transaction set
        //    (binary:true → tx_blobs). The header is a binary blob under
        //    `ledger_data` when binary:true, so the two need separate calls.
        let next_hdr_resp = rpc(serde_json::json!({
            "method":"ledger",
            "params":[{"ledger_index":next,"transactions":false,"expand":false}]
        }));
        let next_header = parse_header(&next_hdr_resp["result"]["ledger"]).expect("next header");
        let next_txs_resp = rpc(serde_json::json!({
            "method":"ledger",
            "params":[{"ledger_index":next,"transactions":true,"expand":true,"binary":true}]
        }));
        let (set_hash, txs) = parse_tx_set(&next_txs_resp["result"]).expect("tx set");

        // 3. Play forward and verify against the validated header.
        let outcome = replay_forward(&parent, set_hash, txs, &next_header, &full_engine(), &fees)
            .expect("replay");

        eprintln!(
            "ledger #{next}: applied {}/{} | account_hash={} tx_hash={} drops={} ledger_hash={}",
            outcome.applied,
            outcome.applied + outcome.failed,
            outcome.account_hash_match,
            outcome.tx_hash_match,
            outcome.drops_match,
            outcome.ledger_hash_match,
        );

        // Count how many state entries differ from rippled's validated ledger.
        // The parent state was proven byte-exact above, so any difference is
        // attributable purely to applying this ledger's transactions — a
        // regression tracker for tx-engine / metadata fidelity. Full byte
        // fidelity (account_hash / tx_hash) is the remaining play-forward work.
        let mut theirs: std::collections::HashMap<String, String> = Default::default();
        let mut marker: Option<String> = None;
        loop {
            let mut p = serde_json::json!({"ledger_index":next,"binary":true,"limit":2048});
            if let Some(m) = &marker {
                p["marker"] = serde_json::Value::String(m.clone());
            }
            let r = rpc(serde_json::json!({"method":"ledger_data","params":[p]}));
            for e in r["result"]["state"].as_array().unwrap() {
                theirs.insert(
                    e["index"].as_str().unwrap().to_uppercase(),
                    e["data"].as_str().unwrap().to_uppercase(),
                );
            }
            marker = r["result"]["marker"].as_str().map(|s| s.to_string());
            if marker.is_none() {
                break;
            }
        }
        let mut diffs = 0;
        outcome.ledger.state_map.for_each(&mut |k, v| {
            let key = hex::encode_upper(k.as_bytes());
            if theirs.get(&key) != Some(&hex::encode_upper(v)) {
                diffs += 1;
            }
        });
        eprintln!("state entries differing from rippled: {diffs}");

        // Byte-exact fidelity: every transaction applies, and the replay
        // reproduces the validated header's account_hash, tx_hash, ledger_hash
        // and total coins. Holds for the supported mainnet ledgers (#268000,
        // #300000, #316000); a ledger exercising not-yet-faithful transactor
        // logic would trip this and the `diffs` counter above.
        assert_eq!(outcome.failed, 0, "every transaction in the set must apply");
        assert!(outcome.is_faithful(), "replay must reproduce the validated header");
        assert_eq!(diffs, 0, "no state entry may differ from rippled");
    }

    /// Diagnostic: per-transaction metadata-blob comparison against rippled.
    /// Replays a mainnet ledger, then for each transaction compares our binary
    /// metadata leaf to rippled's `meta` blob byte-for-byte. On mismatch it
    /// decodes both and prints the differing fields. This isolates the
    /// remaining `tx_hash` divergence (offer / directory metadata).
    ///
    /// Ignored — needs network. Run with:
    /// `RXRPL_PLAY_FORWARD_RPC=http://host:5005 RXRPL_PLAY_FORWARD_LEDGER=316000 \
    ///  cargo test -p rxrpl-node --lib offer_meta_diff_mainnet -- --ignored --nocapture`
    #[test]
    #[ignore = "hits a live mainnet RPC server"]
    fn offer_meta_diff_mainnet() {
        let Ok(url) = std::env::var("RXRPL_PLAY_FORWARD_RPC") else {
            eprintln!("RXRPL_PLAY_FORWARD_RPC unset; skipping");
            return;
        };
        let next: u32 = std::env::var("RXRPL_PLAY_FORWARD_LEDGER")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(316000);
        let parent_seq = next - 1;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap();
        let rpc = |params: serde_json::Value| -> Value {
            rt.block_on(async {
                client.post(&url).json(&params).send().await.unwrap().json().await.unwrap()
            })
        };

        let parent_resp = rpc(serde_json::json!({
            "method":"ledger",
            "params":[{"ledger_index":parent_seq,"transactions":false,"expand":false}]
        }));
        let parent_header = parse_header(&parent_resp["result"]["ledger"]).expect("parent header");
        let mut state = rxrpl_shamap::SHAMap::account_state();
        let mut marker: Option<String> = None;
        loop {
            let mut p = serde_json::json!({"ledger_index":parent_seq,"binary":true,"limit":2048});
            if let Some(m) = &marker {
                p["marker"] = serde_json::Value::String(m.clone());
            }
            let r = rpc(serde_json::json!({"method":"ledger_data","params":[p]}));
            for e in r["result"]["state"].as_array().unwrap() {
                let key: [u8; 32] =
                    hex::decode(e["index"].as_str().unwrap()).unwrap().try_into().unwrap();
                let data = hex::decode(e["data"].as_str().unwrap()).unwrap();
                state.put(Hash256::new(key), data).unwrap();
            }
            marker = r["result"]["marker"].as_str().map(|s| s.to_string());
            if marker.is_none() {
                break;
            }
        }
        assert_eq!(state.root_hash(), parent_header.account_hash, "parent state root");
        let mut parent = Ledger::from_catchup(parent_seq, parent_header.hash, state);
        parent.header = parent_header;
        let fees = FeeSettings {
            base_fee: 10,
            reserve_base: 200_000_000,
            reserve_increment: 50_000_000,
        };

        let next_hdr_resp = rpc(serde_json::json!({
            "method":"ledger",
            "params":[{"ledger_index":next,"transactions":false,"expand":false}]
        }));
        let next_header = parse_header(&next_hdr_resp["result"]["ledger"]).expect("next header");
        let bin_resp = rpc(serde_json::json!({
            "method":"ledger",
            "params":[{"ledger_index":next,"transactions":true,"expand":true,"binary":true}]
        }));
        let (set_hash, txs) = parse_tx_set(&bin_resp["result"]).expect("tx set");
        let outcome = replay_forward(&parent, set_hash, txs, &next_header, &full_engine(), &fees)
            .expect("replay");
        eprintln!(
            "ledger #{next}: tx_hash_match={} account_hash_match={}",
            outcome.tx_hash_match, outcome.account_hash_match
        );

        // rippled meta blob per txid.
        let mut their_meta: std::collections::HashMap<Hash256, Vec<u8>> = Default::default();
        for entry in bin_resp["result"]["ledger"]["transactions"].as_array().unwrap() {
            let blob = hex::decode(entry["tx_blob"].as_str().unwrap()).unwrap();
            let meta = hex::decode(entry["meta"].as_str().unwrap()).unwrap();
            their_meta.insert(transaction_id(&blob), meta);
        }

        let mut leaves: Vec<(Hash256, Vec<u8>)> = Vec::new();
        outcome.ledger.tx_map.for_each(&mut |k, v| leaves.push((*k, v.to_vec())));
        let mut mismatches = 0;
        for (txid, leaf) in &leaves {
            let (_tx, our_meta_json) =
                rxrpl_codec::binary::decode_tx_leaf(leaf).expect("decode our leaf");
            let our_meta = rxrpl_codec::binary::encode(&our_meta_json).expect("encode our meta");
            let their = their_meta.get(txid).expect("rippled meta for txid");
            if &our_meta == their {
                continue;
            }
            mismatches += 1;
            let their_json = rxrpl_codec::binary::decode(their).expect("decode rippled meta");
            eprintln!("\n=== META MISMATCH tx {} ===", hex::encode_upper(txid.as_bytes()));
            eprintln!("TxIndex ours={} theirs={}",
                our_meta_json["TransactionIndex"], their_json["TransactionIndex"]);
            diff_json("", &our_meta_json, &their_json);
        }
        eprintln!("\n{mismatches}/{} transactions have mismatched metadata", leaves.len());
    }

    /// Recursively print where `ours` and `theirs` differ (ours=LEFT, theirs=RIGHT).
    fn diff_json(path: &str, ours: &Value, theirs: &Value) {
        match (ours, theirs) {
            (Value::Object(a), Value::Object(b)) => {
                let mut keys: std::collections::BTreeSet<&String> = a.keys().collect();
                keys.extend(b.keys());
                for k in keys {
                    let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                    match (a.get(k), b.get(k)) {
                        (Some(x), Some(y)) => diff_json(&p, x, y),
                        (Some(x), None) => eprintln!("  ONLY-OURS  {p} = {x}"),
                        (None, Some(y)) => eprintln!("  ONLY-THEIRS {p} = {y}"),
                        (None, None) => {}
                    }
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                if a.len() != b.len() {
                    eprintln!("  LEN  {path}: ours={} theirs={}", a.len(), b.len());
                }
                for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                    diff_json(&format!("{path}[{i}]"), x, y);
                }
            }
            _ => {
                if ours != theirs {
                    eprintln!("  DIFF {path}: ours={ours} theirs={theirs}");
                }
            }
        }
    }
}
