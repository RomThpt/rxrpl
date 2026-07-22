//! Blob-fed segment replay for the `xrpl-state-compare` differential lab.
//!
//! The lab captures raw mainnet bytes once (a full-state checkpoint every 25k
//! ledgers, plus tx+metadata+header for every ledger) into implementation-neutral
//! `XSCP` packs, shards the range into checkpoint-seeded segments, and replays
//! each segment through a client under test. This module is rxrpl's segment
//! worker core: it seeds parent state from a checkpoint `STATE` pack, then
//! replays every ledger in the segment through the same byte-exact engine as
//! [`replay_forward`], carrying state forward. For each ledger it reports the
//! three hashes (computed vs the mainnet header) and each transaction's emitted
//! metadata, which the orchestration layer diffs against mainnet metadata to
//! localise a divergence to ledger -> tx -> object -> field. No RPC, no network.

use std::collections::BTreeMap;

use rxrpl_ledger::{Ledger, LedgerHeader};
use rxrpl_primitives::Hash256;
use rxrpl_shamap::{SHAMap, transaction_set_root};

use crate::play_forward::{all_handlers_engine, fees_for_ledger, replay_forward, transaction_id};

/// `XSCP` pack framing (mirror of `xrpl-state-compare` `core/pack.py`).
const MAGIC: &[u8; 4] = b"XSCP";
const VERSION: u8 = 1;
const KIND_STATE: u8 = 1;
const KIND_LEDGER: u8 = 2;

/// A decoded checkpoint `STATE` pack: the full mainnet state at `checkpoint_seq`.
pub struct StatePack {
    pub checkpoint_seq: u64,
    /// `(ledger index, raw serialized SLE)` pairs, byte-identical to what
    /// `ledger_data binary` returns — exactly what the account-state SHAMap hashes.
    pub entries: Vec<([u8; 32], Vec<u8>)>,
}

/// One transaction inside a `LEDGER` pack: raw signed tx + mainnet metadata.
pub struct TxRecord {
    pub tx_hash: [u8; 32],
    pub tx_blob: Vec<u8>,
    pub meta_blob: Vec<u8>,
}

/// One ledger inside a `LEDGER` pack: raw header + its transactions in canonical
/// (pack) order.
pub struct LedgerRecord {
    pub seq: u64,
    pub header_blob: Vec<u8>,
    pub txs: Vec<TxRecord>,
}

/// Per-tx replay result: the emitted metadata to diff against mainnet's.
pub struct TxReport {
    pub txid: Hash256,
    /// Our engine's emitted metadata blob (mainnet `AffectedNodes` shape).
    pub meta_blob: Vec<u8>,
}

/// Per-ledger replay result: the three hashes computed vs the mainnet header.
pub struct LedgerReport {
    pub seq: u64,
    pub account_hash_match: bool,
    pub tx_hash_match: bool,
    pub ledger_hash_match: bool,
    pub drops_match: bool,
    pub applied: usize,
    pub failed: usize,
    pub computed_account_hash: Hash256,
    pub expected_account_hash: Hash256,
    pub computed_tx_hash: Hash256,
    pub expected_tx_hash: Hash256,
    pub txs: Vec<TxReport>,
}

fn take<'a>(b: &'a [u8], off: &mut usize, n: usize) -> Result<&'a [u8], String> {
    let end = off.checked_add(n).ok_or("length overflow")?;
    let s = b.get(*off..end).ok_or("truncated pack")?;
    *off = end;
    Ok(s)
}

fn read_u32(b: &[u8], off: &mut usize) -> Result<u32, String> {
    Ok(u32::from_be_bytes(take(b, off, 4)?.try_into().unwrap()))
}

fn read_u64(b: &[u8], off: &mut usize) -> Result<u64, String> {
    Ok(u64::from_be_bytes(take(b, off, 8)?.try_into().unwrap()))
}

fn read_header(b: &[u8], off: &mut usize, kind: u8) -> Result<(), String> {
    if take(b, off, 4)? != MAGIC {
        return Err("bad magic".into());
    }
    if take(b, off, 1)?[0] != VERSION {
        return Err("bad version".into());
    }
    if take(b, off, 1)?[0] != kind {
        return Err("wrong pack kind".into());
    }
    Ok(())
}

/// Decode a `STATE` pack: header, `checkpoint_seq`, `entry_count`, then
/// `entry_count x { index[32], data_len u32, data }`.
pub fn unpack_state(blob: &[u8]) -> Result<StatePack, String> {
    let mut off = 0;
    read_header(blob, &mut off, KIND_STATE)?;
    let checkpoint_seq = read_u64(blob, &mut off)?;
    let n = read_u32(blob, &mut off)? as usize;
    let mut entries = Vec::with_capacity(n);
    for _ in 0..n {
        let index: [u8; 32] = take(blob, &mut off, 32)?.try_into().unwrap();
        let dlen = read_u32(blob, &mut off)? as usize;
        let data = take(blob, &mut off, dlen)?.to_vec();
        entries.push((index, data));
    }
    Ok(StatePack {
        checkpoint_seq,
        entries,
    })
}

/// Decode a `LEDGER` pack: header, `batch_start`, `ledger_count`, then each
/// ledger `{ seq u64, header_len u32, header, tx_count u32, txs }` where each tx
/// is `{ tx_hash[32], tx_blob_len u32, tx_blob, meta_len u32, meta_blob }`.
pub fn unpack_ledger_batch(blob: &[u8]) -> Result<Vec<LedgerRecord>, String> {
    let mut off = 0;
    read_header(blob, &mut off, KIND_LEDGER)?;
    let _batch_start = read_u64(blob, &mut off)?;
    let count = read_u32(blob, &mut off)? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let seq = read_u64(blob, &mut off)?;
        let hlen = read_u32(blob, &mut off)? as usize;
        let header_blob = take(blob, &mut off, hlen)?.to_vec();
        let tx_count = read_u32(blob, &mut off)? as usize;
        let mut txs = Vec::with_capacity(tx_count);
        for _ in 0..tx_count {
            let tx_hash: [u8; 32] = take(blob, &mut off, 32)?.try_into().unwrap();
            let blen = read_u32(blob, &mut off)? as usize;
            let tx_blob = take(blob, &mut off, blen)?.to_vec();
            let mlen = read_u32(blob, &mut off)? as usize;
            let meta_blob = take(blob, &mut off, mlen)?.to_vec();
            txs.push(TxRecord {
                tx_hash,
                tx_blob,
                meta_blob,
            });
        }
        out.push(LedgerRecord {
            seq,
            header_blob,
            txs,
        });
    }
    Ok(out)
}

/// Read one variable-length segment (rippled VL prefix) from `b` at `off`.
fn read_vl(b: &[u8], off: &mut usize) -> Option<Vec<u8>> {
    let b0 = *b.get(*off)? as usize;
    *off += 1;
    let len = if b0 <= 192 {
        b0
    } else if b0 <= 240 {
        let b1 = *b.get(*off)? as usize;
        *off += 1;
        193 + (b0 - 193) * 256 + b1
    } else if b0 <= 254 {
        let b1 = *b.get(*off)? as usize;
        let b2 = *b.get(*off + 1)? as usize;
        *off += 2;
        12481 + (b0 - 241) * 65536 + b1 * 256 + b2
    } else {
        return None;
    };
    let seg = b.get(*off..*off + len)?.to_vec();
    *off += len;
    Some(seg)
}

/// Extract our emitted metadata for each transaction from the closed ledger's
/// transaction SHAMap. Each leaf is `VL(tx) || VL(meta)`; the leaf key is the
/// transaction id, so we look each tx up by the id recomputed from its blob.
fn emitted_meta(ledger: &Ledger, txids: &[Hash256]) -> Vec<TxReport> {
    let mut out = Vec::with_capacity(txids.len());
    for txid in txids {
        let meta_blob = ledger
            .tx_map
            .get(txid)
            .and_then(|leaf| {
                let mut off = 0;
                let _tx = read_vl(leaf, &mut off)?;
                read_vl(leaf, &mut off)
            })
            .unwrap_or_default();
        out.push(TxReport {
            txid: *txid,
            meta_blob,
        });
    }
    out
}

/// Replay one checkpoint-seeded segment from its `XSCP` packs.
///
/// `state_pack` is the `STATE` pack whose `checkpoint_seq == start`; `ledger_packs`
/// are the `LEDGER` packs covering `start..=end` (the checkpoint ledger `start`
/// supplies the parent header, then `start+1..=end` are replayed). Returns one
/// [`LedgerReport`] per replayed ledger, carrying state forward so a divergence at
/// ledger N is contaminated only by N (segments seed from real mainnet state).
pub fn replay_segment(
    state_pack: &[u8],
    ledger_packs: &[&[u8]],
    start: u64,
    end: u64,
) -> Result<Vec<LedgerReport>, String> {
    let sp = unpack_state(state_pack)?;
    if sp.checkpoint_seq != start {
        return Err(format!(
            "state pack checkpoint {} != segment start {start}",
            sp.checkpoint_seq
        ));
    }

    let mut state = SHAMap::account_state();
    for (index, data) in &sp.entries {
        state
            .put(Hash256::new(*index), data.clone())
            .map_err(|e| format!("state seed put failed: {e:?}"))?;
    }

    let mut ledgers: BTreeMap<u64, LedgerRecord> = BTreeMap::new();
    for pack in ledger_packs {
        for rec in unpack_ledger_batch(pack)? {
            ledgers.insert(rec.seq, rec);
        }
    }

    let seed = ledgers
        .get(&start)
        .ok_or_else(|| format!("checkpoint ledger {start} missing from ledger packs"))?;
    let parent_hdr =
        LedgerHeader::from_raw_bytes(&seed.header_blob).ok_or("checkpoint header decode failed")?;
    if state.root_hash() != parent_hdr.account_hash {
        return Err(format!(
            "seed state root {} != checkpoint account_hash {}",
            state.root_hash(),
            parent_hdr.account_hash
        ));
    }

    let mut parent = Ledger::from_catchup(start as u32, parent_hdr.hash, state);
    parent.header = parent_hdr;

    let engine = all_handlers_engine();
    let mut reports = Vec::with_capacity((end - start) as usize);

    for seq in (start + 1)..=end {
        let rec = ledgers
            .get(&seq)
            .ok_or_else(|| format!("ledger {seq} missing from ledger packs"))?;
        let hdr =
            LedgerHeader::from_raw_bytes(&rec.header_blob).ok_or("ledger header decode failed")?;

        let txids: Vec<Hash256> = rec.txs.iter().map(|t| transaction_id(&t.tx_blob)).collect();
        let txset = rec
            .txs
            .iter()
            .zip(&txids)
            .map(|(t, id)| (*id, t.tx_blob.clone()))
            .collect();
        let set_hash = transaction_set_root(&txids);
        let fees = fees_for_ledger(&parent);

        let outcome = replay_forward(&parent, set_hash, txset, &hdr, &engine, &fees)
            .map_err(|e| format!("replay of ledger {seq} failed: {e:?}"))?;

        reports.push(LedgerReport {
            seq,
            account_hash_match: outcome.account_hash_match,
            tx_hash_match: outcome.tx_hash_match,
            ledger_hash_match: outcome.ledger_hash_match,
            drops_match: outcome.drops_match,
            applied: outcome.applied,
            failed: outcome.failed,
            computed_account_hash: outcome.ledger.header.account_hash,
            expected_account_hash: hdr.account_hash,
            computed_tx_hash: outcome.ledger.header.tx_hash,
            expected_tx_hash: hdr.tx_hash,
            txs: emitted_meta(&outcome.ledger, &txids),
        });

        parent = outcome.ledger;
    }

    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-language codec check: decode packs produced by `xrpl-state-compare`'s
    /// Python `core/pack.py` (fixtures generated with `pack_state` / `pack_ledger_batch`).
    /// Run `just gen-xscp-fixtures` (or the snippet in the worker docs) to refresh.
    #[test]
    #[ignore = "needs /tmp/xscp_*.fixture from the Python codec"]
    fn decodes_python_packs() {
        let sp = unpack_state(&std::fs::read("/tmp/xscp_state.fixture").unwrap()).unwrap();
        assert_eq!(sp.checkpoint_seq, 999);
        assert_eq!(sp.entries.len(), 2);
        let index0: [u8; 32] = std::array::from_fn(|i| i as u8);
        assert_eq!(sp.entries[0].0, index0);
        assert_eq!(sp.entries[0].1, vec![0xAA, 0xBB, 0xCC]);
        assert_eq!(sp.entries[1].0, [1u8; 32]);
        assert_eq!(sp.entries[1].1, vec![0x01, 0x02]);

        let lb = unpack_ledger_batch(&std::fs::read("/tmp/xscp_ledger.fixture").unwrap()).unwrap();
        assert_eq!(lb.len(), 1);
        assert_eq!(lb[0].seq, 1000);
        assert_eq!(lb[0].header_blob, b"HDR0");
        assert_eq!(lb[0].txs.len(), 1);
        assert_eq!(lb[0].txs[0].tx_hash, [9u8; 32]);
        assert_eq!(lb[0].txs[0].tx_blob, b"TXBLOB");
        assert_eq!(lb[0].txs[0].meta_blob, b"METAB");
    }

    /// Malformed packs are rejected, not silently mis-decoded.
    #[test]
    fn rejects_bad_framing() {
        assert!(unpack_state(b"XSCP\x01\x02").is_err()); // wrong kind
        assert!(unpack_state(b"NOPE\x01\x01").is_err()); // bad magic
        assert!(unpack_ledger_batch(b"XSCP\x01\x02\x00").is_err()); // truncated
    }
}
