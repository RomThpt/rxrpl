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
            v.get("Amendments").and_then(|a| a.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str())
                    .filter_map(|s| hex::decode(s).ok())
                    .filter_map(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
                    .map(Hash256::new)
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();
    let mut enabled = enabled;
    // SortedDirectories was retired (permanently baked in) by the time of the
    // modern lending/vault amendments, so it is no longer listed in the
    // Amendments object even though directories are kept sorted. Re-enable it
    // whenever a clearly post-retirement amendment is active.
    let single_asset_vault = rxrpl_amendment::feature::feature_id("SingleAssetVault");
    let sorted_directories = rxrpl_amendment::feature::feature_id("SortedDirectories");
    if enabled.contains(&single_asset_vault) && !enabled.contains(&sorted_directories) {
        enabled.push(sorted_directories);
    }
    Rules::from_enabled(enabled)
}

/// Read the era-correct fee and reserve settings from a ledger's `FeeSettings`
/// SLE. Early ledgers (pre-2014) predate that object, where reserves were the
/// protocol constants (200 XRP base, 50 XRP per owner).
pub fn fees_for_ledger(ledger: &Ledger) -> FeeSettings {
    ledger
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
        .unwrap_or(FeeSettings {
            base_fee: 10,
            reserve_base: 200_000_000,
            reserve_increment: 50_000_000,
        })
}

/// Fetch a validated ledger's header, transaction-set root and transaction set
/// (canonical blobs without metadata) over RPC. This is the transaction-set
/// source for play-forward sync: bounded by the ledger's transaction count, not
/// its ~19M state entries.
pub async fn fetch_ledger_for_replay(
    rpc_url: &str,
    ledger_index: u32,
) -> Result<(LedgerHeader, Hash256, TxSet), Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .danger_accept_invalid_certs(true)
        .build()?;
    let call = |params: Value| {
        let client = client.clone();
        let rpc_url = rpc_url.to_string();
        async move {
            client
                .post(&rpc_url)
                .json(&params)
                .send()
                .await?
                .json::<Value>()
                .await
        }
    };
    // The header needs JSON fields (parent_hash, close_time, …) — only present
    // with binary:false. The transaction set needs binary:true to get the
    // canonical no-metadata blobs. These are two distinct RPC shapes.
    let hdr_resp = call(serde_json::json!({
        "method": "ledger",
        "params": [{ "ledger_index": ledger_index, "transactions": false, "expand": false }]
    }))
    .await?;
    let header = parse_header(
        hdr_resp
            .get("result")
            .and_then(|r| r.get("ledger"))
            .ok_or("missing result.ledger in header response")?,
    )?;
    let txs_resp = call(serde_json::json!({
        "method": "ledger",
        "params": [{ "ledger_index": ledger_index, "transactions": true, "expand": true, "binary": true }]
    }))
    .await?;
    let (set_hash, txs) = parse_tx_set(
        txs_resp
            .get("result")
            .ok_or("missing result in tx-set response")?,
    )?;
    Ok((header, set_hash, txs))
}

/// Advance from a held `base` ledger up to `to_seq` (inclusive) by fetching each
/// successor's validated transaction set over RPC and replaying it forward onto
/// the running parent state. Returns the replayed ledgers in order. Stops with
/// an error at the first replay that is not byte-faithful to its validated
/// header, so the caller can fall back to P2P state acquisition.
pub async fn catchup_via_replay(
    rpc_url: &str,
    base: Ledger,
    to_seq: u32,
    tx_engine: &TxEngine,
) -> Result<Vec<Ledger>, NodeError> {
    let mut chain = Vec::new();
    let mut parent = base;
    for seq in (parent.header.sequence + 1)..=to_seq {
        let (header, set_hash, txs) = fetch_ledger_for_replay(rpc_url, seq)
            .await
            .map_err(|e| NodeError::Server(format!("fetch #{seq} for replay: {e}")))?;
        let fees = fees_for_ledger(&parent);
        let outcome = replay_forward(&parent, set_hash, txs, &header, tx_engine, &fees)?;
        if !outcome.is_faithful() {
            return Err(NodeError::Server(format!(
                "replay #{seq} unfaithful (account_hash={} tx_hash={} ledger_hash={} drops={})",
                outcome.account_hash_match,
                outcome.tx_hash_match,
                outcome.ledger_hash_match,
                outcome.drops_match,
            )));
        }
        parent = outcome.ledger.clone();
        chain.push(outcome.ledger);
    }
    Ok(chain)
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
    use rxrpl_codec::address::classic::{decode_account_id, encode_account_id};
    use rxrpl_primitives::AccountId;
    use rxrpl_protocol::keylet;
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

    /// Targeted single-transaction oracle: validate one mainnet transaction
    /// byte-exact WITHOUT bootstrapping the full ~19M-entry state. Fetches only
    /// the SLEs the tx touches or reads (affected nodes + FeeSettings +
    /// Amendments + the tx's accounts) at the parent ledger, applies the single
    /// tx, and compares every affected SLE to rippled's stored bytes at ledger N.
    ///
    /// Run with:
    /// `RXRPL_PLAY_FORWARD_RPC=http://host:5005 RXRPL_PLAY_FORWARD_LEDGER=N \
    ///  RXRPL_PLAY_FORWARD_TXHASH=<hash> cargo test -p rxrpl-node --lib \
    ///  single_tx_oracle_mainnet -- --ignored --nocapture`
    #[test]
    #[ignore = "hits a live mainnet RPC server"]
    fn single_tx_oracle_mainnet() {
        let Ok(url) = std::env::var("RXRPL_PLAY_FORWARD_RPC") else {
            eprintln!("RXRPL_PLAY_FORWARD_RPC unset; skipping");
            return;
        };
        let (Some(n), Ok(txhash)) = (
            std::env::var("RXRPL_PLAY_FORWARD_LEDGER")
                .ok()
                .and_then(|s| s.parse::<u32>().ok()),
            std::env::var("RXRPL_PLAY_FORWARD_TXHASH"),
        ) else {
            eprintln!("RXRPL_PLAY_FORWARD_LEDGER / _TXHASH unset; skipping");
            return;
        };
        let parent = n - 1;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap();
        // Public load-balanced RPC clusters intermittently return non-JSON
        // (rate-limit / 503) on rapid sequential POSTs. Retry with backoff so a
        // transient hiccup on any of the many per-tx calls doesn't fail the run.
        let rpc = |params: serde_json::Value| -> Value {
            rt.block_on(async {
                for attempt in 0..8u32 {
                    if let Ok(resp) = client.post(&url).json(&params).send().await {
                        if let Ok(v) = resp.json::<Value>().await {
                            return v;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(
                        400 * u64::from(attempt + 1),
                    ))
                    .await;
                }
                panic!("rpc failed after retries: {params}");
            })
        };

        // Target tx as a canonical blob (binary) -> our JSON, exactly like replay.
        let txs_resp = rpc(serde_json::json!({
            "method":"ledger","params":[{"ledger_index":n,"transactions":true,"expand":true,"binary":true}]
        }));
        let (_set_hash, txs) = parse_tx_set(&txs_resp["result"]).expect("tx set");
        let want_id: Hash256 = txhash.parse().expect("txhash");
        let blob = txs
            .into_iter()
            .find(|(id, _)| *id == want_id)
            .map(|(_, b)| b)
            .expect("tx not in ledger");
        let tx_json = rxrpl_codec::binary::decode(&blob).expect("decode tx");

        // Affected SLE keys + classification from the expanded metadata.
        let meta_resp = rpc(serde_json::json!({
            "method":"ledger","params":[{"ledger_index":n,"transactions":true,"expand":true}]
        }));
        let entries = meta_resp["result"]["ledger"]["transactions"]
            .as_array()
            .expect("transactions");
        let txm = entries
            .iter()
            .find(|t| t["hash"].as_str() == Some(&txhash))
            .expect("tx meta");
        let mut affected: Vec<(String, String)> = Vec::new(); // (key, nodeType)
        for node in txm["metaData"]["AffectedNodes"].as_array().unwrap() {
            for nt in ["CreatedNode", "ModifiedNode", "DeletedNode"] {
                if let Some(e) = node.get(nt) {
                    affected.push((
                        e["LedgerIndex"].as_str().unwrap().to_uppercase(),
                        nt.to_string(),
                    ));
                }
            }
        }

        // Read-set = affected keys + FeeSettings + Amendments (for Rules) + every
        // AccountRoot any apply might read. The latter is any account the tx or an
        // affected entry references — Account/Destination, every issuer (Amount,
        // TrustSet LimitAmount, NFToken Issuer), owners, authorized accounts, the
        // HighLimit/LowLimit issuers of touched trust lines, etc. Collected by
        // walking the tx JSON and the affected nodes' fields for r-addresses.
        let mut read_keys: std::collections::BTreeSet<String> =
            affected.iter().map(|(k, _)| k.clone()).collect();
        read_keys.insert(keylet::fee_settings().to_string().to_uppercase());
        read_keys.insert(keylet::amendments().to_string().to_uppercase());
        let mut stack: Vec<&Value> = vec![&tx_json];
        for node in txm["metaData"]["AffectedNodes"].as_array().unwrap() {
            for nt in ["CreatedNode", "ModifiedNode", "DeletedNode"] {
                if let Some(e) = node.get(nt) {
                    for f in ["FinalFields", "NewFields", "PreviousFields"] {
                        if let Some(ff) = e.get(f) {
                            stack.push(ff);
                        }
                    }
                }
            }
        }
        while let Some(v) = stack.pop() {
            match v {
                Value::String(s) => {
                    if s.starts_with('r') && s.len() >= 25 {
                        if let Ok(id) = decode_account_id(s) {
                            read_keys.insert(keylet::account(&id).to_string().to_uppercase());
                        }
                    }
                }
                Value::Array(a) => stack.extend(a.iter()),
                Value::Object(o) => stack.extend(o.values()),
                _ => {}
            }
        }

        // MPToken transactors read the MPTokenIssuance (id = seq||issuer) without
        // listing it in AffectedNodes; derive and seed its SLE key.
        if let Some(idhex) = tx_json
            .get("MPTokenIssuanceID")
            .and_then(|v| v.as_str())
            .filter(|s| s.len() == 48)
        {
            if let Ok(b) = hex::decode(idhex) {
                let seq = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                if let Ok(iss) = rxrpl_primitives::AccountId::from_slice(&b[4..24]) {
                    read_keys.insert(
                        keylet::mptoken_issuance(&iss, seq)
                            .to_string()
                            .to_uppercase(),
                    );
                }
            }
        }

        // XChain transactors read the Bridge SLE (keyed per door) without listing
        // it in AffectedNodes; derive and seed both candidate keylets.
        if let Some(bridge) = tx_json.get("XChainBridge") {
            for (door_f, issue_f) in [
                ("LockingChainDoor", "LockingChainIssue"),
                ("IssuingChainDoor", "IssuingChainIssue"),
            ] {
                if let (Some(d), Some(iss)) = (
                    bridge.get(door_f).and_then(|v| v.as_str()),
                    bridge.get(issue_f),
                ) {
                    if let Ok(did) = decode_account_id(d) {
                        read_keys.insert(
                            rxrpl_tx_engine::bridge_helpers::bridge_keylet_for_door(&did, iss)
                                .to_string()
                                .to_uppercase(),
                        );
                        // The attestation transactors read the door's SignerList
                        // SLE (witnesses + quorum); it is not in AffectedNodes.
                        read_keys.insert(keylet::signer_list(&did).to_string().to_uppercase());
                    }
                }
            }
        }

        // LoanBroker/Vault transactors read the referenced Vault SLE (by its
        // 32-byte VaultID keylet) without listing it in AffectedNodes. Seed it
        // from the tx, and from any affected object that carries a VaultID
        // (e.g. a LoanBroker referenced only by LoanBrokerID).
        if let Some(vid) = tx_json.get("VaultID").and_then(|v| v.as_str()) {
            read_keys.insert(vid.to_uppercase());
        }
        for node in txm["metaData"]["AffectedNodes"].as_array().unwrap() {
            for wrap in ["CreatedNode", "ModifiedNode", "DeletedNode"] {
                for fields in ["FinalFields", "NewFields"] {
                    // The referenced Vault, and the LoanBroker referenced by a
                    // Loan (which carries only LoanBrokerID), are read but not
                    // always listed in AffectedNodes.
                    if let Some(vid) = node[wrap][fields]["VaultID"].as_str() {
                        read_keys.insert(vid.to_uppercase());
                    }
                    if let Some(bid) = node[wrap][fields]["LoanBrokerID"].as_str() {
                        read_keys.insert(bid.to_uppercase());
                    }
                }
            }
        }

        // An entry created or removed on a non-root directory page touches only
        // that page; the root (page 0) is left unchanged and so is absent from
        // AffectedNodes. dirAdd needs the root to walk to the chain's last page,
        // so seed the RootIndex of every affected directory.
        for node in txm["metaData"]["AffectedNodes"].as_array().unwrap() {
            for nt in ["CreatedNode", "ModifiedNode", "DeletedNode"] {
                let Some(e) = node.get(nt) else { continue };
                if e.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("DirectoryNode") {
                    continue;
                }
                for f in ["FinalFields", "NewFields"] {
                    if let Some(root) = e
                        .get(f)
                        .and_then(|ff| ff.get("RootIndex"))
                        .and_then(|v| v.as_str())
                    {
                        read_keys.insert(root.to_uppercase());
                    }
                }
            }
        }

        // A TrustSet may read a trust line it leaves unchanged (already in the
        // requested state), so the line is absent from AffectedNodes and would
        // not be seeded — the handler would then recreate it and over-count the
        // owner reserve. Seed the line the LimitAmount names.
        let currency_bytes = |c: &str| -> [u8; 20] {
            let mut b = [0u8; 20];
            if c.len() == 3 {
                b[12..15].copy_from_slice(c.as_bytes());
            } else if c.len() == 40 {
                if let Ok(d) = hex::decode(c) {
                    if d.len() == 20 {
                        b.copy_from_slice(&d);
                    }
                }
            }
            b
        };
        if tx_json.get("TransactionType").and_then(|v| v.as_str()) == Some("TrustSet") {
            if let Some(lim) = tx_json.get("LimitAmount") {
                if let (Some(a), Some(iss), Some(cur)) = (
                    tx_json.get("Account").and_then(|v| v.as_str()),
                    lim.get("issuer").and_then(|v| v.as_str()),
                    lim.get("currency").and_then(|v| v.as_str()),
                ) {
                    if let (Ok(aid), Ok(iid)) = (decode_account_id(a), decode_account_id(iss)) {
                        read_keys.insert(
                            keylet::trust_line(&aid, &iid, &currency_bytes(cur))
                                .to_string()
                                .to_uppercase(),
                        );
                    }
                }
            }
        }

        // A sell NFTokenCreateOffer reads the seller's NFTokenPage to verify
        // ownership, but creating an offer does not modify the page, so it is
        // absent from AffectedNodes and would not be seeded — the ownership
        // walk would then fail with tecNO_ENTRY. Seed the seller's full page
        // chain from the parent ledger (account_objects), unchanged by the tx.
        if tx_json.get("TransactionType").and_then(|v| v.as_str()) == Some("NFTokenCreateOffer")
            && tx_json.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) & 1 != 0
        {
            if let Some(acct) = tx_json.get("Account").and_then(|v| v.as_str()) {
                let r = rpc(serde_json::json!({
                    "method":"account_objects",
                    "params":[{"account":acct,"type":"nft_page","ledger_index":parent}]
                }));
                if let Some(objs) = r["result"]["account_objects"].as_array() {
                    for o in objs {
                        if let Some(idx) = o.get("index").and_then(|v| v.as_str()) {
                            read_keys.insert(idx.to_uppercase());
                        }
                    }
                }
            }
        }

        // AMMVote recomputes the trading fee as the LP-weighted average over
        // every account already in the AMM's VoteSlots: applyVote calls
        // ammLPHolds(entryAccount) for each one, reading that account's LP-token
        // trust line. Those lines are read-only, so they are absent from the tx
        // AffectedNodes and would not be seeded — every existing voter would then
        // read 0 LP and be wrongly evicted. Seed each voter's (and the auction
        // slot account's) LP trust line from the parent ledger so the average and
        // eviction match the chain.
        if tx_json.get("TransactionType").and_then(|v| v.as_str()) == Some("AMMVote") {
            if let (Some(a1), Some(a2)) = (tx_json.get("Asset"), tx_json.get("Asset2")) {
                if let Ok(amm_key) = rxrpl_tx_engine::amm_helpers::compute_amm_key(a1, a2) {
                    let amm_idx = amm_key.to_string().to_uppercase();
                    let r = rpc(serde_json::json!({
                        "method":"ledger_entry","params":[{"index":amm_idx,"ledger_index":parent}]
                    }));
                    let amm = &r["result"]["node"];
                    if let (Some(amm_acct), Some(lp_cur)) = (
                        amm.get("Account").and_then(|v| v.as_str()),
                        amm.get("LPTokenBalance")
                            .and_then(|b| b.get("currency"))
                            .and_then(|v| v.as_str()),
                    ) {
                        if let (Ok(amm_id), Ok(cur_bytes)) = (
                            decode_account_id(amm_acct),
                            hex::decode(lp_cur)
                                .map_err(|_| ())
                                .and_then(|b| <[u8; 20]>::try_from(b.as_slice()).map_err(|_| ())),
                        ) {
                            let mut voters: Vec<String> = amm
                                .get("VoteSlots")
                                .and_then(|v| v.as_array())
                                .map(|slots| {
                                    slots
                                        .iter()
                                        .filter_map(|s| {
                                            s.get("VoteEntry")
                                                .unwrap_or(s)
                                                .get("Account")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            if let Some(a) = amm
                                .get("AuctionSlot")
                                .and_then(|au| au.get("Account"))
                                .and_then(|v| v.as_str())
                            {
                                voters.push(a.to_string());
                            }
                            // applyVote also reads the voter's own LP line
                            // (lpTokensNew) before it has a vote slot; seed it too.
                            if let Some(a) = tx_json.get("Account").and_then(|v| v.as_str()) {
                                voters.push(a.to_string());
                            }
                            for voter in voters {
                                if let Ok(vid) = decode_account_id(&voter) {
                                    read_keys.insert(
                                        keylet::trust_line(&vid, &amm_id, &cur_bytes)
                                            .to_string()
                                            .to_uppercase(),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // AMMClawback withdraws the holder's full LP, then directSends the
        // clawed `Asset` from holder to issuer. The holder's trust line for
        // that Asset nets to zero change (withdrawn then clawed) so it is
        // absent from AffectedNodes and would not be seeded — the directSend
        // would then fail with tecNO_ENTRY. Seed the holder<->issuer line.
        if tx_json.get("TransactionType").and_then(|v| v.as_str()) == Some("AMMClawback") {
            if let (Some(holder), Some(asset)) = (
                tx_json.get("Holder").and_then(|v| v.as_str()),
                tx_json.get("Asset"),
            ) {
                if let (Some(cur), Some(iss)) = (
                    asset.get("currency").and_then(|v| v.as_str()),
                    asset.get("issuer").and_then(|v| v.as_str()),
                ) {
                    if let (Ok(hid), Ok(iid)) = (decode_account_id(holder), decode_account_id(iss))
                    {
                        read_keys.insert(
                            keylet::trust_line(&hid, &iid, &currency_bytes(cur))
                                .to_string()
                                .to_uppercase(),
                        );
                    }
                }
            }
        }

        // Seed a partial state map from the parent ledger.
        let mut state = rxrpl_shamap::SHAMap::account_state();
        for key in &read_keys {
            let r = rpc(serde_json::json!({
                "method":"ledger_entry","params":[{"index":key,"ledger_index":parent,"binary":true}]
            }));
            if let Some(hex_node) = r["result"]["node_binary"].as_str() {
                let kb: [u8; 32] = hex::decode(key).unwrap().try_into().unwrap();
                state
                    .put(Hash256::new(kb), hex::decode(hex_node).unwrap())
                    .unwrap();
            }
        }

        // Override affected entries with their exact PRE-tx state, reconstructed
        // from metadata (FinalFields overlaid with PreviousFields). The
        // parent-ledger value is stale whenever an account was already touched by
        // an earlier transaction in the same ledger N — its Sequence and balances
        // would differ, failing the sequence check or drifting amounts. The
        // metadata captures the value the target tx actually saw.
        for node in txm["metaData"]["AffectedNodes"].as_array().unwrap() {
            for nt in ["ModifiedNode", "DeletedNode"] {
                let Some(e) = node.get(nt) else {
                    continue;
                };
                let Some(let_type) = e.get("LedgerEntryType").and_then(|v| v.as_str()) else {
                    continue;
                };
                // DirectoryNode metadata omits the `Indexes` array; reconstructing
                // from it would wipe the directory's existing entries. Keep the
                // parent-ledger seed (fetched above) which carries full Indexes.
                if let_type == "DirectoryNode" {
                    continue;
                }
                let mut pre = e
                    .get("FinalFields")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let key = e["LedgerIndex"].as_str().unwrap().to_uppercase();
                let kb: [u8; 32] = hex::decode(&key).unwrap().try_into().unwrap();
                if let Some(obj) = pre.as_object_mut() {
                    if let Some(prev) = e.get("PreviousFields").and_then(|v| v.as_object()) {
                        for (k, v) in prev {
                            obj.insert(k.clone(), v.clone());
                        }
                    }
                    obj.insert("LedgerEntryType".into(), Value::String(let_type.into()));
                    // FinalFields omits the threaded PreviousTxnID/LgrSeq; carry
                    // them over from the parent-ledger seed so the central
                    // stamping has a field to overwrite (its value is irrelevant).
                    // When the entry was created earlier in this same ledger
                    // there is no parent seed — add a placeholder for threaded
                    // types so stamping still fires (DirectoryNode et al. carry
                    // no such field and must be left alone).
                    let threaded = !matches!(
                        let_type,
                        "DirectoryNode" | "LedgerHashes" | "Amendments" | "FeeSettings"
                    );
                    let seed = state
                        .get(&Hash256::new(kb))
                        .and_then(|b| rxrpl_codec::binary::decode(b).ok());
                    if threaded {
                        if let Some(seed) = &seed {
                            for f in ["PreviousTxnID", "PreviousTxnLgrSeq"] {
                                if let Some(v) = seed.get(f) {
                                    obj.insert(f.into(), v.clone());
                                }
                            }
                        }
                    }
                    if threaded && !obj.contains_key("PreviousTxnID") {
                        obj.insert("PreviousTxnID".into(), Value::String("0".repeat(64)));
                        obj.insert("PreviousTxnLgrSeq".into(), Value::from(0u32));
                    }
                    // A field in FinalFields that is absent from both
                    // PreviousFields and the parent-ledger seed was *added* by
                    // this tx, so it was not part of the pre-tx state — drop it
                    // (e.g. an NFTokenPage's PreviousPageMin when a page splits).
                    if let Some(seed_obj) = seed.as_ref().and_then(|s| s.as_object()) {
                        let prev_keys: std::collections::BTreeSet<&String> = e
                            .get("PreviousFields")
                            .and_then(|v| v.as_object())
                            .map(|o| o.keys().collect())
                            .unwrap_or_default();
                        let added: Vec<String> = obj
                            .keys()
                            .filter(|k| {
                                !prev_keys.contains(k)
                                    && !seed_obj.contains_key(k.as_str())
                                    && !matches!(
                                        k.as_str(),
                                        "LedgerEntryType" | "PreviousTxnID" | "PreviousTxnLgrSeq"
                                    )
                            })
                            .cloned()
                            .collect();
                        for k in added {
                            obj.remove(&k);
                        }
                    }
                }
                let Ok(json_bytes) = serde_json::to_vec(&pre) else {
                    continue;
                };
                let Ok(bin) = rxrpl_ledger::sle_codec::encode_sle(&json_bytes) else {
                    continue;
                };
                let kb: [u8; 32] = hex::decode(&key).unwrap().try_into().unwrap();
                state.put(Hash256::new(kb), bin).unwrap();
            }
        }

        // Order-book crossing walks the book directory pages to find offers.
        // Map each seeded offer to its `BookDirectory` page and guarantee that
        // page lists the offer's index. The parent-ledger page is stale when the
        // offer was created or moved by another tx in this same ledger (it would
        // omit the entry, so the walk would miss it); patch the page (or build a
        // minimal one) so every affected offer is reachable.
        let mut dir_offers: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for key in &read_keys {
            let kb: [u8; 32] = hex::decode(key).unwrap().try_into().unwrap();
            let Some(node) = state.get(&Hash256::new(kb)) else {
                continue;
            };
            let Ok(j) = rxrpl_codec::binary::decode(node) else {
                continue;
            };
            if j.get("LedgerEntryType").and_then(|t| t.as_str()) == Some("Offer") {
                if let Some(bd) = j.get("BookDirectory").and_then(|v| v.as_str()) {
                    dir_offers
                        .entry(bd.to_uppercase())
                        .or_default()
                        .push(key.clone());
                }
            }
        }
        for (key, offers) in &dir_offers {
            let r = rpc(serde_json::json!({
                "method":"ledger_entry","params":[{"index":key,"ledger_index":parent,"binary":true}]
            }));
            let mut page = r["result"]["node_binary"]
                .as_str()
                .and_then(|h| hex::decode(h).ok())
                .and_then(|b| rxrpl_codec::binary::decode(&b).ok())
                .unwrap_or_else(|| {
                    serde_json::json!({
                        "LedgerEntryType": "DirectoryNode",
                        "Flags": 0,
                        "RootIndex": key,
                        "Indexes": [],
                    })
                });
            if let Some(arr) = page.get_mut("Indexes").and_then(|v| v.as_array_mut()) {
                for off in offers {
                    if !arr.iter().any(|x| x.as_str() == Some(off.as_str())) {
                        arr.push(Value::String(off.clone()));
                    }
                }
            }
            if let Ok(b) = serde_json::to_vec(&page) {
                if let Ok(bin) = rxrpl_ledger::sle_codec::encode_sle(&b) {
                    let kb: [u8; 32] = hex::decode(key).unwrap().try_into().unwrap();
                    state.put(Hash256::new(kb), bin).unwrap();
                }
            }
        }

        let parent_hdr = rpc(serde_json::json!({
            "method":"ledger","params":[{"ledger_index":parent,"transactions":false,"expand":false}]
        }));
        let parent_header = parse_header(&parent_hdr["result"]["ledger"]).expect("parent header");
        let mut base = Ledger::from_catchup(parent, parent_header.hash, state);
        base.header = parent_header;
        let mut open = Ledger::new_open(&base);
        // The open ledger's close time (the standalone advances by one
        // resolution per ledger_accept); transactors that stamp the current
        // time (e.g. LoanSet StartDate) read this.
        open.header.close_time =
            base.header.close_time + base.header.close_time_resolution.max(1) as u32;
        let rules = rules_for_ledger(&open);
        let fees = fees_for_ledger(&base);

        let res = full_engine().apply(&tx_json, &mut open, &rules, &fees);
        eprintln!("apply result: {res:?}");

        // Compare each affected SLE to the state OUR tx produced, taken from the
        // target tx's own metadata (FinalFields / NewFields), not ledger_entry@N.
        // The on-chain value at N reflects every transaction in the ledger, so it
        // is wrong for any SLE that a *later* tx in N also touched; the metadata
        // records the value as our tx left it.
        let non_threaded = |t: &str| {
            matches!(
                t,
                "DirectoryNode" | "LedgerHashes" | "Amendments" | "FeeSettings"
            )
        };
        let txid_upper = txhash.to_uppercase();
        let mut mismatches = 0;
        for node in txm["metaData"]["AffectedNodes"].as_array().unwrap() {
            let (nt, e) = if let Some(e) = node.get("CreatedNode") {
                ("CreatedNode", e)
            } else if let Some(e) = node.get("ModifiedNode") {
                ("ModifiedNode", e)
            } else if let Some(e) = node.get("DeletedNode") {
                ("DeletedNode", e)
            } else {
                continue;
            };
            let key = e["LedgerIndex"].as_str().unwrap().to_uppercase();
            let kb: [u8; 32] = hex::decode(&key).unwrap().try_into().unwrap();
            let ours = open.state_map.get(&Hash256::new(kb)).map(hex::encode_upper);
            let let_type = e
                .get("LedgerEntryType")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let theirs = if nt == "DeletedNode" {
                None
            } else if let_type == "DirectoryNode" {
                // DirectoryNode metadata omits Indexes, so compare against the
                // real on-chain entry at N. But owner directories on a busy
                // account are re-modified by later txs in the ledger; when the
                // on-chain page's PreviousTxnID is not our tx, its Indexes reflect
                // those later txs and our tx's effect cannot be isolated — skip.
                let r = rpc(serde_json::json!({
                    "method":"ledger_entry","params":[{"index":key,"ledger_index":n,"binary":true}]
                }));
                let th = r["result"]["node_binary"]
                    .as_str()
                    .map(|s| s.to_uppercase());
                let dir_last_tx = th
                    .as_deref()
                    .and_then(|h| hex::decode(h).ok())
                    .and_then(|b| rxrpl_codec::binary::decode(&b).ok())
                    .and_then(|j| {
                        j.get("PreviousTxnID")
                            .and_then(|v| v.as_str().map(|s| s.to_uppercase()))
                    });
                if dir_last_tx.as_deref() == Some(txid_upper.as_str()) {
                    th
                } else {
                    eprintln!("  SKIP-DIR {key}: re-modified by a later tx in the ledger");
                    ours.clone()
                }
            } else {
                let fields = if nt == "CreatedNode" {
                    "NewFields"
                } else {
                    "FinalFields"
                };
                // A ModifiedNode that only threads PreviousTxnID (a pure "touch")
                // carries no FinalFields; reconstruct the full SLE from the pre-tx
                // seed so it is not compared against a degenerate field set.
                let non_empty = e
                    .get(fields)
                    .and_then(|v| v.as_object())
                    .map(|o| !o.is_empty())
                    .unwrap_or(false);
                let mut post = if non_empty {
                    e.get(fields).cloned().unwrap()
                } else if nt == "ModifiedNode" {
                    base.state_map
                        .get(&Hash256::new(kb))
                        .and_then(|b| rxrpl_codec::binary::decode(b).ok())
                        .unwrap_or_else(|| serde_json::json!({}))
                } else {
                    serde_json::json!({})
                };
                if let Some(obj) = post.as_object_mut() {
                    obj.insert("LedgerEntryType".into(), Value::String(let_type.into()));
                    // A created AccountRoot always carries Balance, but the tx
                    // metadata omits it when it is exactly zero (e.g. an AMM
                    // pseudo-account funded only with IOU legs). Default it so the
                    // reconstructed SLE matches the real on-chain entry.
                    if nt == "CreatedNode"
                        && let_type == "AccountRoot"
                        && !obj.contains_key("Balance")
                    {
                        obj.insert("Balance".into(), Value::String("0".into()));
                    }
                    if !non_threaded(let_type) {
                        obj.insert("PreviousTxnID".into(), Value::String(txid_upper.clone()));
                        obj.insert("PreviousTxnLgrSeq".into(), Value::from(n));
                    }
                }
                serde_json::to_vec(&post)
                    .ok()
                    .and_then(|b| rxrpl_ledger::sle_codec::encode_sle(&b).ok())
                    .map(hex::encode_upper)
            };

            if ours.as_deref() != theirs.as_deref() {
                mismatches += 1;
                eprintln!("  DIFF {key} ({nt} {let_type})");
                let dj = |h: Option<&str>| -> serde_json::Value {
                    h.and_then(|h| hex::decode(h).ok())
                        .and_then(|b| rxrpl_codec::binary::decode(&b).ok())
                        .unwrap_or(serde_json::Value::Null)
                };
                let oj = dj(ours.as_deref());
                let tj = dj(theirs.as_deref());
                if let (Some(o), Some(t)) = (oj.as_object(), tj.as_object()) {
                    let mut keys: std::collections::BTreeSet<&String> = o.keys().collect();
                    keys.extend(t.keys());
                    for k in keys {
                        if o.get(k) != t.get(k) {
                            eprintln!("      {k}: ours={:?} theirs={:?}", o.get(k), t.get(k));
                        }
                    }
                } else {
                    eprintln!("    ours:   {}", ours.as_deref().unwrap_or("<absent>"));
                    eprintln!("    theirs: {}", theirs.as_deref().unwrap_or("<absent>"));
                }
            }
        }
        eprintln!("affected={} mismatches={mismatches}", affected.len());
        assert_eq!(
            mismatches, 0,
            "every affected SLE must match its tx metadata"
        );
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
        let bytes =
            rxrpl_ledger::sle_codec::encode_sle(&serde_json::to_vec(&amendments).unwrap()).unwrap();
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
            let ours = hex::encode_upper(v);
            if theirs.get(&key) != Some(&ours) {
                diffs += 1;
                let typ = rxrpl_codec::binary::decode(v)
                    .ok()
                    .and_then(|j| {
                        j.get("LedgerEntryType")
                            .and_then(|t| t.as_str().map(String::from))
                    })
                    .unwrap_or_else(|| "?".into());
                eprintln!("  DIFF {key} ({typ})");
                eprintln!("    ours:   {ours}");
                eprintln!(
                    "    theirs: {}",
                    theirs.get(&key).map(String::as_str).unwrap_or("<absent>")
                );
                if let (Ok(o), Some(t)) = (
                    rxrpl_codec::binary::decode(v),
                    theirs
                        .get(&key)
                        .and_then(|h| hex::decode(h).ok())
                        .and_then(|b| rxrpl_codec::binary::decode(&b).ok()),
                ) {
                    eprintln!("    ours json:   {o}");
                    eprintln!("    theirs json: {t}");
                }
            }
        });
        eprintln!("state entries differing from rippled: {diffs}");

        // Byte-exact fidelity: every transaction applies, and the replay
        // reproduces the validated header's account_hash, tx_hash, ledger_hash
        // and total coins. Holds for the supported mainnet ledgers (#268000,
        // #300000, #316000, #338500 full-fill + #346750 partial-fill offer
        // crossing); a ledger exercising not-yet-faithful transactor logic
        // would trip this and the `diffs` counter above.
        assert_eq!(outcome.failed, 0, "every transaction in the set must apply");
        assert!(
            outcome.is_faithful(),
            "replay must reproduce the validated header"
        );
        assert_eq!(diffs, 0, "no state entry may differ from rippled");
    }

    /// Multi-ledger play-forward: bootstrap one base ledger's state, then follow
    /// the chain by replaying each successor's transaction set over RPC. Proves
    /// the node can *track* mainnet (not just replay a single step). Each step
    /// must stay byte-faithful or `catchup_via_replay` errors out.
    ///
    /// Run with (base held = RXRPL_PLAY_FORWARD_LEDGER, steps = COUNT):
    /// `RXRPL_PLAY_FORWARD_RPC=http://host:5005 RXRPL_PLAY_FORWARD_LEDGER=267999 \
    ///  RXRPL_PLAY_FORWARD_COUNT=3 cargo test -p rxrpl-node --lib \
    ///  catchup_via_replay_mainnet -- --ignored --nocapture`
    #[test]
    #[ignore = "hits a live mainnet RPC server"]
    fn catchup_via_replay_mainnet() {
        let Ok(url) = std::env::var("RXRPL_PLAY_FORWARD_RPC") else {
            eprintln!("RXRPL_PLAY_FORWARD_RPC unset; skipping");
            return;
        };
        let base_seq: u32 = std::env::var("RXRPL_PLAY_FORWARD_LEDGER")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(267999);
        let count: u32 = std::env::var("RXRPL_PLAY_FORWARD_COUNT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);

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

        let base_resp = rpc(serde_json::json!({
            "method":"ledger",
            "params":[{"ledger_index":base_seq,"transactions":false,"expand":false}]
        }));
        let base_header = parse_header(&base_resp["result"]["ledger"]).expect("base header");

        let mut state = rxrpl_shamap::SHAMap::account_state();
        let mut marker: Option<String> = None;
        loop {
            let mut p = serde_json::json!({"ledger_index":base_seq,"binary":true,"limit":2048});
            if let Some(m) = &marker {
                p["marker"] = serde_json::Value::String(m.clone());
            }
            let r = rpc(serde_json::json!({"method":"ledger_data","params":[p]}));
            for e in r["result"]["state"].as_array().unwrap() {
                let key: [u8; 32] = hex::decode(e["index"].as_str().unwrap())
                    .unwrap()
                    .try_into()
                    .unwrap();
                let data = hex::decode(e["data"].as_str().unwrap()).unwrap();
                state.put(Hash256::new(key), data).unwrap();
            }
            marker = r["result"]["marker"].as_str().map(|s| s.to_string());
            if marker.is_none() {
                break;
            }
        }
        assert_eq!(
            state.root_hash(),
            base_header.account_hash,
            "downloaded base state root must equal validated account_hash"
        );

        let mut base = Ledger::from_catchup(base_seq, base_header.hash, state);
        base.header = base_header;

        let to_seq = base_seq + count;
        let chain = rt
            .block_on(catchup_via_replay(&url, base, to_seq, &full_engine()))
            .expect("catchup_via_replay must stay faithful across the range");

        eprintln!(
            "play-forward tracked #{}..=#{}: {} ledgers, tip account_hash={}",
            base_seq + 1,
            to_seq,
            chain.len(),
            chain.last().unwrap().header.account_hash,
        );
        assert_eq!(
            chain.len() as u32,
            count,
            "must replay every ledger in range"
        );
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
                let key: [u8; 32] = hex::decode(e["index"].as_str().unwrap())
                    .unwrap()
                    .try_into()
                    .unwrap();
                let data = hex::decode(e["data"].as_str().unwrap()).unwrap();
                state.put(Hash256::new(key), data).unwrap();
            }
            marker = r["result"]["marker"].as_str().map(|s| s.to_string());
            if marker.is_none() {
                break;
            }
        }
        assert_eq!(
            state.root_hash(),
            parent_header.account_hash,
            "parent state root"
        );
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
        for entry in bin_resp["result"]["ledger"]["transactions"]
            .as_array()
            .unwrap()
        {
            let blob = hex::decode(entry["tx_blob"].as_str().unwrap()).unwrap();
            let meta = hex::decode(entry["meta"].as_str().unwrap()).unwrap();
            their_meta.insert(transaction_id(&blob), meta);
        }

        let mut leaves: Vec<(Hash256, Vec<u8>)> = Vec::new();
        outcome
            .ledger
            .tx_map
            .for_each(&mut |k, v| leaves.push((*k, v.to_vec())));
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
            eprintln!(
                "\n=== META MISMATCH tx {} ===",
                hex::encode_upper(txid.as_bytes())
            );
            eprintln!(
                "TxIndex ours={} theirs={}",
                our_meta_json["TransactionIndex"], their_json["TransactionIndex"]
            );
            diff_json("", &our_meta_json, &their_json);
        }
        eprintln!(
            "\n{mismatches}/{} transactions have mismatched metadata",
            leaves.len()
        );
    }

    /// Recursively print where `ours` and `theirs` differ (ours=LEFT, theirs=RIGHT).
    fn diff_json(path: &str, ours: &Value, theirs: &Value) {
        match (ours, theirs) {
            (Value::Object(a), Value::Object(b)) => {
                let mut keys: std::collections::BTreeSet<&String> = a.keys().collect();
                keys.extend(b.keys());
                for k in keys {
                    let p = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
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
