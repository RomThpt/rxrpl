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

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use rxrpl_ledger::{Ledger, LedgerHeader};
use rxrpl_primitives::Hash256;
use rxrpl_shamap::{SHAMap, SHAMapError, transaction_set_root};

use crate::play_forward::{
    TxSet, all_handlers_engine, fees_for_ledger, replay_forward, replay_forward_observed_order,
    transaction_id,
};

/// `XSCP` pack framing (mirror of `xrpl-state-compare` `core/pack.py`).
const MAGIC: &[u8; 4] = b"XSCP";
const VERSION: u8 = 1;
const KIND_STATE: u8 = 1;
const KIND_LEDGER: u8 = 2;

/// A decoded checkpoint `STATE` pack: the full mainnet state at `checkpoint_seq`.
#[derive(Debug)]
pub struct StatePack {
    pub checkpoint_seq: u64,
    /// `(ledger index, raw serialized SLE)` pairs, byte-identical to what
    /// `ledger_data binary` returns — exactly what the account-state SHAMap hashes.
    pub entries: Vec<([u8; 32], Vec<u8>)>,
}

/// One transaction inside a `LEDGER` pack: raw signed tx + mainnet metadata.
#[derive(Debug)]
pub struct TxRecord {
    pub tx_hash: [u8; 32],
    pub tx_blob: Vec<u8>,
    pub meta_blob: Vec<u8>,
}

/// One ledger inside a `LEDGER` pack: raw header + its transactions in canonical
/// (pack) order.
#[derive(Debug)]
pub struct LedgerRecord {
    pub seq: u64,
    pub header_blob: Vec<u8>,
    pub txs: Vec<TxRecord>,
}

/// Per-tx replay result: the emitted metadata to diff against mainnet's.
#[derive(Debug)]
pub struct TxReport {
    pub txid: Hash256,
    /// Our engine's emitted metadata blob (mainnet `AffectedNodes` shape).
    pub meta_blob: Vec<u8>,
}

/// Per-ledger replay result: the three hashes computed vs the mainnet header.
pub struct LedgerReport {
    pub seq: u64,
    /// True when the closed-ledger `TransactionIndex` order was required because
    /// the original consensus candidate-set salt was absent from the XSCP pack.
    pub observed_order_replay: bool,
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
    pub computed_ledger_hash: Hash256,
    pub expected_ledger_hash: Hash256,
    pub computed_drops: u64,
    pub expected_drops: u64,
    pub txs: Vec<TxReport>,
}

/// Aggregate result emitted after the final audited ledger (or an early stop).
pub struct ReplaySummary {
    pub checkpoint: u64,
    pub start: u64,
    pub end: u64,
    pub processed: usize,
    pub applied: usize,
    pub failed: usize,
    pub account_hash_mismatches: usize,
    pub tx_hash_mismatches: usize,
    pub ledger_hash_mismatches: usize,
    pub drops_mismatches: usize,
    pub observed_order_replays: usize,
    pub stopped_early: bool,
}

fn observed_order_txset(rec: &LedgerRecord) -> Result<TxSet, String> {
    let mut ordered = Vec::with_capacity(rec.txs.len());
    for tx in &rec.txs {
        let meta = rxrpl_codec::binary::decode(&tx.meta_blob)
            .map_err(|e| format!("invalid metadata for observed-order replay: {e:?}"))?;
        let index = meta
            .get("TransactionIndex")
            .and_then(serde_json::Value::as_u64)
            .ok_or("metadata missing TransactionIndex")?;
        ordered.push((index, transaction_id(&tx.tx_blob), tx.tx_blob.clone()));
    }
    ordered.sort_by_key(|(index, _, _)| *index);
    if ordered.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err("duplicate metadata TransactionIndex".into());
    }
    Ok(ordered
        .into_iter()
        .map(|(_, txid, blob)| (txid, blob))
        .collect())
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
    if checkpoint_seq == 0 || checkpoint_seq > u64::from(u32::MAX) {
        return Err(format!(
            "checkpoint {checkpoint_seq} is outside the valid u32 ledger range"
        ));
    }
    let n = read_u32(blob, &mut off)? as usize;
    if n > blob.len().saturating_sub(off) / 36 {
        return Err("state entry count exceeds remaining pack bytes".into());
    }
    let mut entries = Vec::with_capacity(n);
    let mut indexes = BTreeSet::new();
    for _ in 0..n {
        let index: [u8; 32] = take(blob, &mut off, 32)?.try_into().unwrap();
        if !indexes.insert(index) {
            return Err(format!(
                "duplicate state index {}",
                hex::encode_upper(index)
            ));
        }
        let dlen = read_u32(blob, &mut off)? as usize;
        let data = take(blob, &mut off, dlen)?.to_vec();
        entries.push((index, data));
    }
    if off != blob.len() {
        return Err(format!(
            "trailing bytes after STATE pack: {}",
            blob.len() - off
        ));
    }
    Ok(StatePack {
        checkpoint_seq,
        entries,
    })
}

/// Stream an account-state SHAMap as an `XSCP` STATE pack.
///
/// The entry count is patched after the ordered leaf walk, so exporting a
/// multi-gigabyte checkpoint does not require a second copy in memory.
pub fn write_state_pack<W: Write + Seek>(
    writer: &mut W,
    checkpoint_seq: u64,
    state: &SHAMap,
) -> Result<u32, String> {
    if checkpoint_seq == 0 || checkpoint_seq > u64::from(u32::MAX) {
        return Err(format!(
            "checkpoint {checkpoint_seq} is outside the valid u32 ledger range"
        ));
    }
    writer.write_all(MAGIC).map_err(|e| e.to_string())?;
    writer
        .write_all(&[VERSION, KIND_STATE])
        .map_err(|e| e.to_string())?;
    writer
        .write_all(&checkpoint_seq.to_be_bytes())
        .map_err(|e| e.to_string())?;
    let count_pos = writer.stream_position().map_err(|e| e.to_string())?;
    writer
        .write_all(&0u32.to_be_bytes())
        .map_err(|e| e.to_string())?;

    let mut count = 0u32;
    for (index, data) in state.iter_ref() {
        let len = u32::try_from(data.len()).map_err(|_| "state entry exceeds u32 length")?;
        writer
            .write_all(index.as_bytes())
            .map_err(|e| e.to_string())?;
        writer
            .write_all(&len.to_be_bytes())
            .map_err(|e| e.to_string())?;
        writer.write_all(data).map_err(|e| e.to_string())?;
        count = count
            .checked_add(1)
            .ok_or("state entry count exceeds u32")?;
    }
    let end = writer.stream_position().map_err(|e| e.to_string())?;
    writer
        .seek(SeekFrom::Start(count_pos))
        .map_err(|e| e.to_string())?;
    writer
        .write_all(&count.to_be_bytes())
        .map_err(|e| e.to_string())?;
    writer
        .seek(SeekFrom::Start(end))
        .map_err(|e| e.to_string())?;
    Ok(count)
}

/// Decode a `LEDGER` pack: header, `batch_start`, `ledger_count`, then each
/// ledger `{ seq u64, header_len u32, header, tx_count u32, txs }` where each tx
/// is `{ tx_hash[32], tx_blob_len u32, tx_blob, meta_len u32, meta_blob }`.
pub fn unpack_ledger_batch(blob: &[u8]) -> Result<Vec<LedgerRecord>, String> {
    let mut off = 0;
    read_header(blob, &mut off, KIND_LEDGER)?;
    let batch_start = read_u64(blob, &mut off)?;
    let count = read_u32(blob, &mut off)? as usize;
    if count == 0 {
        return Err("empty LEDGER pack".into());
    }
    if count > blob.len().saturating_sub(off) / 16 {
        return Err("ledger count exceeds remaining pack bytes".into());
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let seq = read_u64(blob, &mut off)?;
        if seq == 0 || seq > u64::from(u32::MAX) {
            return Err(format!(
                "ledger {seq} is outside the valid u32 ledger range"
            ));
        }
        let expected_seq = batch_start
            .checked_add(i as u64)
            .ok_or("ledger sequence overflow")?;
        if seq != expected_seq {
            return Err(format!(
                "non-contiguous LEDGER pack: record {seq} != expected {expected_seq}"
            ));
        }
        let hlen = read_u32(blob, &mut off)? as usize;
        let header_blob = take(blob, &mut off, hlen)?.to_vec();
        let tx_count = read_u32(blob, &mut off)? as usize;
        if tx_count > blob.len().saturating_sub(off) / 40 {
            return Err(format!(
                "transaction count for ledger {seq} exceeds remaining pack bytes"
            ));
        }
        let mut txs = Vec::with_capacity(tx_count);
        let mut tx_hashes = BTreeSet::new();
        for _ in 0..tx_count {
            let tx_hash: [u8; 32] = take(blob, &mut off, 32)?.try_into().unwrap();
            if !tx_hashes.insert(tx_hash) {
                return Err(format!(
                    "duplicate packed transaction hash {} in ledger {seq}",
                    hex::encode_upper(tx_hash)
                ));
            }
            let blen = read_u32(blob, &mut off)? as usize;
            let tx_blob = take(blob, &mut off, blen)?.to_vec();
            let computed = transaction_id(&tx_blob);
            if computed.as_bytes() != &tx_hash {
                return Err(format!(
                    "packed transaction hash mismatch in ledger {seq}: packed {} computed {computed}",
                    hex::encode_upper(tx_hash)
                ));
            }
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
    if off != blob.len() {
        return Err(format!(
            "trailing bytes after LEDGER pack: {}",
            blob.len() - off
        ));
    }
    Ok(out)
}

/// Read one variable-length segment (rippled VL prefix) from `b` at `off`.
fn read_vl<'a>(b: &'a [u8], off: &mut usize) -> Result<&'a [u8], String> {
    let b0 = take(b, off, 1)?[0] as usize;
    let len = if b0 <= 192 {
        b0
    } else if b0 <= 240 {
        let b1 = take(b, off, 1)?[0] as usize;
        193 + (b0 - 193) * 256 + b1
    } else if b0 <= 254 {
        let prefix = take(b, off, 2)?;
        let b1 = prefix[0] as usize;
        let b2 = prefix[1] as usize;
        12481 + (b0 - 241) * 65536 + b1 * 256 + b2
    } else {
        return Err("invalid VL length prefix".into());
    };
    take(b, off, len)
}

/// Extract our emitted metadata for each transaction from the closed ledger's
/// transaction SHAMap. Each leaf is `VL(tx) || VL(meta)`; the leaf key is the
/// transaction id, so we look each tx up by the id recomputed from its blob.
fn emitted_meta(ledger: &Ledger, txs: &[TxRecord]) -> Result<Vec<TxReport>, String> {
    let mut out = Vec::with_capacity(txs.len());
    for tx in txs {
        let txid = transaction_id(&tx.tx_blob);
        let Some(leaf) = ledger.tx_map.get(&txid) else {
            continue;
        };
        let mut off = 0;
        let emitted_tx = read_vl(leaf, &mut off)
            .map_err(|e| format!("invalid emitted transaction leaf {txid}: {e}"))?;
        let meta_blob = read_vl(leaf, &mut off)
            .map_err(|e| format!("invalid emitted metadata leaf {txid}: {e}"))?;
        if emitted_tx != tx.tx_blob {
            return Err(format!("emitted transaction blob differs for {txid}"));
        }
        if off != leaf.len() {
            return Err(format!(
                "trailing bytes in emitted transaction leaf {txid}: {}",
                leaf.len() - off
            ));
        }
        out.push(TxReport {
            txid,
            meta_blob: meta_blob.to_vec(),
        });
    }
    Ok(out)
}

/// Replay one checkpoint-seeded segment from its `XSCP` packs.
///
/// `state_pack` is the `STATE` pack for the parent `checkpoint`; `ledger_packs`
/// cover `checkpoint..=end`, including the parent header. The audited range is
/// exactly `start..=end`, where `checkpoint + 1 == start`.
pub fn replay_segment(
    state_pack: &[u8],
    ledger_packs: &[&[u8]],
    checkpoint: u64,
    start: u64,
    end: u64,
) -> Result<Vec<LedgerReport>, String> {
    let mut reports = Vec::new();
    replay_segment_with(state_pack, ledger_packs, checkpoint, start, end, |report| {
        reports.push(report);
        Ok(())
    })?;
    Ok(reports)
}

/// Streaming variant of [`replay_segment`]. The callback runs once per audited
/// ledger, including the first ledger that causes an early stop.
pub fn replay_segment_with<F>(
    state_pack: &[u8],
    ledger_packs: &[&[u8]],
    checkpoint: u64,
    start: u64,
    end: u64,
    emit: F,
) -> Result<ReplaySummary, String>
where
    F: FnMut(LedgerReport) -> Result<(), String>,
{
    replay_segment_with_state(state_pack, ledger_packs, checkpoint, start, end, emit)
        .map(|(summary, _)| summary)
}

/// Replay a segment and return the latest byte-exact ledger state suitable for
/// seeding the next segment. On a fatal divergence this is the faithful parent,
/// never the divergent child.
pub fn replay_segment_with_state<F>(
    state_pack: &[u8],
    ledger_packs: &[&[u8]],
    checkpoint: u64,
    start: u64,
    end: u64,
    emit: F,
) -> Result<(ReplaySummary, Ledger), String>
where
    F: FnMut(LedgerReport) -> Result<(), String>,
{
    validate_range(checkpoint, start, end)?;
    let state = seed_state_from_slice(state_pack, checkpoint)?;
    replay_segment_with_seeded_state(state, ledger_packs, checkpoint, start, end, emit)
}

/// Seed directly from a resident pack, avoiding a second collection of all
/// entries and indexes while the SHAMap is being built.
fn seed_state_from_slice(blob: &[u8], checkpoint: u64) -> Result<SHAMap, String> {
    let mut off = 0;
    read_header(blob, &mut off, KIND_STATE)?;
    let checkpoint_seq = read_u64(blob, &mut off)?;
    if checkpoint_seq == 0 || checkpoint_seq > u64::from(u32::MAX) {
        return Err(format!(
            "checkpoint {checkpoint_seq} is outside the valid u32 ledger range"
        ));
    }
    let n = read_u32(blob, &mut off)? as usize;
    if n > blob.len().saturating_sub(off) / 36 {
        return Err("state entry count exceeds remaining pack bytes".into());
    }

    let mut state = SHAMap::account_state();
    for _ in 0..n {
        let index: [u8; 32] = take(blob, &mut off, 32)?.try_into().unwrap();
        let key = Hash256::new(index);
        if state.has(&key) {
            return Err(format!(
                "duplicate state index {}",
                hex::encode_upper(index)
            ));
        }
        let dlen = read_u32(blob, &mut off)? as usize;
        let data = take(blob, &mut off, dlen)?.to_vec();
        state
            .put(key, data)
            .map_err(|e| format!("state seed put failed: {e:?}"))?;
    }
    if off != blob.len() {
        return Err(format!(
            "trailing bytes after STATE pack: {}",
            blob.len() - off
        ));
    }
    if checkpoint_seq != checkpoint {
        return Err(format!(
            "state pack checkpoint {checkpoint_seq} != requested checkpoint {checkpoint}"
        ));
    }
    Ok(state)
}

/// Replay using a STATE pack read from disk, retaining only the SHAMap and one
/// entry payload in memory while seeding.
pub fn replay_segment_with_state_file<F>(
    path: &Path,
    ledger_packs: &[&[u8]],
    checkpoint: u64,
    start: u64,
    end: u64,
    emit: F,
) -> Result<(ReplaySummary, Ledger), String>
where
    F: FnMut(LedgerReport) -> Result<(), String>,
{
    validate_range(checkpoint, start, end)?;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    let state = seed_state_from_reader(&mut reader, checkpoint)?;
    replay_segment_with_seeded_state(state, ledger_packs, checkpoint, start, end, emit)
}

fn read_state_field<const N: usize>(reader: &mut impl Read) -> Result<[u8; N], String> {
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            "truncated pack".to_string()
        } else {
            e.to_string()
        }
    })?;
    Ok(bytes)
}

fn seed_state_from_reader<R: Read + Seek>(
    reader: &mut R,
    checkpoint: u64,
) -> Result<SHAMap, String> {
    let start = reader.stream_position().map_err(|e| e.to_string())?;
    let end = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    reader
        .seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;

    if read_state_field::<4>(reader)? != *MAGIC {
        return Err("bad magic".into());
    }
    if read_state_field::<1>(reader)?[0] != VERSION {
        return Err("bad version".into());
    }
    if read_state_field::<1>(reader)?[0] != KIND_STATE {
        return Err("wrong pack kind".into());
    }
    let checkpoint_seq = u64::from_be_bytes(read_state_field(reader)?);
    if checkpoint_seq == 0 || checkpoint_seq > u64::from(u32::MAX) {
        return Err(format!(
            "checkpoint {checkpoint_seq} is outside the valid u32 ledger range"
        ));
    }
    if checkpoint_seq != checkpoint {
        return Err(format!(
            "state pack checkpoint {checkpoint_seq} != requested checkpoint {checkpoint}"
        ));
    }
    let count = u32::from_be_bytes(read_state_field(reader)?) as u64;
    let mut remaining = end.saturating_sub(reader.stream_position().map_err(|e| e.to_string())?);
    if count > remaining / 36 {
        return Err("state entry count exceeds remaining pack bytes".into());
    }

    let mut state = SHAMap::account_state();
    for _ in 0..count {
        let index = read_state_field::<32>(reader)?;
        let key = Hash256::new(index);
        let len = u32::from_be_bytes(read_state_field(reader)?) as u64;
        remaining = remaining.checked_sub(36).ok_or("truncated pack")?;
        if len > remaining {
            return Err("truncated pack".into());
        }
        let mut data = vec![0; len as usize];
        reader.read_exact(&mut data).map_err(|e| e.to_string())?;
        remaining -= len;
        match state.insert(key, data) {
            Ok(()) => {}
            Err(SHAMapError::DuplicateKey) => {
                return Err(format!(
                    "duplicate state index {}",
                    hex::encode_upper(index)
                ));
            }
            Err(e) => return Err(format!("state seed put failed: {e:?}")),
        }
    }
    if remaining != 0 {
        return Err(format!("trailing bytes after STATE pack: {remaining}"));
    }
    Ok(state)
}

fn replay_segment_with_seeded_state<F>(
    mut state: SHAMap,
    ledger_packs: &[&[u8]],
    checkpoint: u64,
    start: u64,
    end: u64,
    mut emit: F,
) -> Result<(ReplaySummary, Ledger), String>
where
    F: FnMut(LedgerReport) -> Result<(), String>,
{
    let mut ledgers: BTreeMap<u64, LedgerRecord> = BTreeMap::new();
    for pack in ledger_packs {
        for rec in unpack_ledger_batch(pack)? {
            let seq = rec.seq;
            if ledgers.insert(seq, rec).is_some() {
                return Err(format!("duplicate ledger {seq} across LEDGER packs"));
            }
        }
    }

    for seq in checkpoint..=end {
        if !ledgers.contains_key(&seq) {
            return Err(format!("ledger {seq} missing from ledger packs"));
        }
    }

    let mut previous_hash = None;
    for seq in checkpoint..=end {
        let rec = ledgers
            .get(&seq)
            .ok_or_else(|| format!("ledger {seq} missing from ledger packs"))?;
        let hdr = decode_header(seq, &rec.header_blob)?;
        if let Some(parent_hash) = previous_hash
            && hdr.parent_hash != parent_hash
        {
            return Err(format!(
                "ledger {seq} parent hash {} != previous ledger hash {parent_hash}",
                hdr.parent_hash
            ));
        }
        previous_hash = Some(hdr.hash);
    }

    let seed = ledgers
        .get(&checkpoint)
        .ok_or_else(|| format!("checkpoint ledger {checkpoint} missing from ledger packs"))?;
    let parent_hdr = decode_header(checkpoint, &seed.header_blob)?;
    if state.root_hash() != parent_hdr.account_hash {
        return Err(format!(
            "seed state root {} != checkpoint account_hash {}",
            state.root_hash(),
            parent_hdr.account_hash
        ));
    }

    let mut parent = Ledger::from_catchup(checkpoint as u32, parent_hdr.hash, state);
    parent.header = parent_hdr;

    let engine = all_handlers_engine();
    let mut summary = ReplaySummary {
        checkpoint,
        start,
        end,
        processed: 0,
        applied: 0,
        failed: 0,
        account_hash_mismatches: 0,
        tx_hash_mismatches: 0,
        ledger_hash_mismatches: 0,
        drops_mismatches: 0,
        observed_order_replays: 0,
        stopped_early: false,
    };

    for seq in start..=end {
        let rec = ledgers
            .get(&seq)
            .ok_or_else(|| format!("ledger {seq} missing from ledger packs"))?;
        let hdr = decode_header(seq, &rec.header_blob)?;

        let txids: Vec<Hash256> = rec.txs.iter().map(|t| transaction_id(&t.tx_blob)).collect();
        let txset = rec
            .txs
            .iter()
            .zip(&txids)
            .map(|(t, id)| (*id, t.tx_blob.clone()))
            .collect();
        let set_hash = transaction_set_root(&txids);
        let fees = fees_for_ledger(&parent);

        let canonical_outcome = replay_forward(&parent, set_hash, txset, &hdr, &engine, &fees)
            .map_err(|e| format!("replay of ledger {seq} failed: {e:?}"))?;
        let mut outcome = canonical_outcome;
        let mut observed_order_replay = false;
        if (!outcome.is_faithful() || outcome.failed != 0)
            && let Ok(ordered) = observed_order_txset(rec)
            && let Ok(observed) =
                replay_forward_observed_order(&parent, ordered, &hdr, &engine, &fees)
            && observed.is_faithful()
            && observed.failed == 0
        {
            outcome = observed;
            observed_order_replay = true;
        }

        let txs = emitted_meta(&outcome.ledger, &rec.txs)?;
        if txs.len() != outcome.applied {
            let missing: Vec<String> = rec
                .txs
                .iter()
                .map(|tx| transaction_id(&tx.tx_blob))
                .filter(|id| outcome.ledger.tx_map.get(id).is_none())
                .map(|id| id.to_string())
                .collect();
            return Err(format!(
                "ledger {seq} emitted {} transaction leaves for {} applied transactions; missing transaction ids: {}",
                txs.len(),
                outcome.applied,
                missing.join(", ")
            ));
        }
        let report = LedgerReport {
            seq,
            observed_order_replay,
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
            computed_ledger_hash: outcome.ledger.header.hash,
            expected_ledger_hash: hdr.hash,
            computed_drops: outcome.ledger.header.drops,
            expected_drops: hdr.drops,
            txs,
        };

        summary.processed += 1;
        summary.applied += report.applied;
        summary.failed += report.failed;
        summary.account_hash_mismatches += usize::from(!report.account_hash_match);
        summary.tx_hash_mismatches += usize::from(!report.tx_hash_match);
        summary.ledger_hash_mismatches += usize::from(!report.ledger_hash_match);
        summary.drops_mismatches += usize::from(!report.drops_match);
        summary.observed_order_replays += usize::from(report.observed_order_replay);
        let must_stop = !report.account_hash_match || !report.drops_match || report.failed != 0;

        // Seed the next ledger's parent hash from mainnet's real value rather
        // than our computed one. rxrpl's canonical-ordering tx_hash artifact
        // makes our `ledger_hash` differ from the chain's even when the account
        // state is byte-exact; carried forward, that wrong hash cascades into the
        // next ledger's `LedgerHashes` skip-list SLE (appended at close with the
        // parent hash), producing a false `account_hash` divergence for every
        // subsequent ledger. Pinning the real parent hash isolates genuine state
        // divergences from that known tx-tree artifact — matching what the
        // fresh-seed oracle does.
        let mut carried = outcome.ledger;
        carried.header.hash = hdr.hash;
        emit(report)?;

        if must_stop {
            summary.stopped_early = seq != end;
            break;
        }
        parent = carried;
    }

    Ok((summary, parent))
}

fn validate_range(checkpoint: u64, start: u64, end: u64) -> Result<(), String> {
    for (name, seq) in [("checkpoint", checkpoint), ("start", start), ("end", end)] {
        if seq == 0 || seq > u64::from(u32::MAX) {
            return Err(format!(
                "{name} {seq} is outside the valid u32 ledger range"
            ));
        }
    }
    if start > end {
        return Err(format!("segment start {start} is greater than end {end}"));
    }
    if checkpoint.checked_add(1) != Some(start) {
        return Err(format!(
            "checkpoint {checkpoint} must be the parent of start {start}"
        ));
    }
    Ok(())
}

fn decode_header(seq: u64, blob: &[u8]) -> Result<LedgerHeader, String> {
    let header = LedgerHeader::from_raw_bytes(blob)
        .ok_or_else(|| format!("ledger {seq} header decode failed"))?;
    if u64::from(header.sequence) != seq {
        return Err(format!(
            "ledger record sequence {seq} != header sequence {}",
            header.sequence
        ));
    }
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_pack(checkpoint: u64, entries: &[([u8; 32], &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(KIND_STATE);
        out.extend_from_slice(&checkpoint.to_be_bytes());
        out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (index, data) in entries {
            out.extend_from_slice(index);
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(data);
        }
        out
    }

    #[test]
    fn streamed_state_pack_round_trips() {
        let mut state = SHAMap::account_state();
        state
            .put(Hash256::new([2; 32]), b"second".to_vec())
            .unwrap();
        state.put(Hash256::new([1; 32]), b"first".to_vec()).unwrap();
        let mut out = std::io::Cursor::new(Vec::new());

        assert_eq!(write_state_pack(&mut out, 42, &state).unwrap(), 2);
        let decoded = unpack_state(out.get_ref()).unwrap();
        assert_eq!(decoded.checkpoint_seq, 42);
        assert_eq!(decoded.entries[0], ([1; 32], b"first".to_vec()));
        assert_eq!(decoded.entries[1], ([2; 32], b"second".to_vec()));
    }

    #[test]
    fn reader_seed_matches_byte_slice_state() {
        let pack = state_pack(42, &[([2; 32], b"second"), ([1; 32], b"first")]);
        let decoded = unpack_state(&pack).unwrap();
        let mut expected = SHAMap::account_state();
        for (index, data) in decoded.entries {
            expected.insert(Hash256::new(index), data).unwrap();
        }
        let mut slice_seeded = seed_state_from_slice(&pack, 42).unwrap();
        assert_eq!(slice_seeded.root_hash(), expected.root_hash());
        assert_eq!(
            slice_seeded.get(&Hash256::new([1; 32])),
            Some(&b"first"[..])
        );
        assert_eq!(
            slice_seeded.get(&Hash256::new([2; 32])),
            Some(&b"second"[..])
        );
        let mut reader = std::io::Cursor::new(pack);
        let mut seeded = seed_state_from_reader(&mut reader, 42).unwrap();
        assert_eq!(seeded.root_hash(), expected.root_hash());
        assert_eq!(seeded.get(&Hash256::new([1; 32])), Some(&b"first"[..]));
        assert_eq!(seeded.get(&Hash256::new([2; 32])), Some(&b"second"[..]));
    }

    #[test]
    fn slice_seed_preserves_decoder_errors_and_order() {
        let mut bad_magic = state_pack(7, &[]);
        bad_magic[0] = b'N';
        let mut bad_version = state_pack(7, &[]);
        bad_version[4] = 2;
        let mut bad_kind = state_pack(7, &[]);
        bad_kind[5] = KIND_LEDGER;
        let mut bad_count = state_pack(7, &[]);
        bad_count[14..18].copy_from_slice(&1u32.to_be_bytes());
        let mut truncated = state_pack(7, &[([1; 32], b"payload")]);
        truncated.pop();
        let mut duplicate_before_length = state_pack(7, &[([3; 32], b"a"), ([3; 32], b"b")]);
        duplicate_before_length.truncate(18 + 36 + 1 + 32);
        let mut trailing_wrong_checkpoint = state_pack(8, &[([1; 32], b"a")]);
        trailing_wrong_checkpoint.push(0);
        let mut trailing = state_pack(7, &[([1; 32], b"a")]);
        trailing.push(0);
        let cases = [
            bad_magic,
            bad_version,
            bad_kind,
            state_pack(0, &[]),
            state_pack(u64::from(u32::MAX) + 1, &[]),
            bad_count,
            truncated,
            duplicate_before_length,
            trailing,
            trailing_wrong_checkpoint,
            state_pack(8, &[([1; 32], b"a")]),
        ];
        for pack in cases {
            let expected = match unpack_state(&pack) {
                Err(error) => error,
                Ok(decoded) => format!(
                    "state pack checkpoint {} != requested checkpoint 7",
                    decoded.checkpoint_seq
                ),
            };
            assert_eq!(seed_state_from_slice(&pack, 7).unwrap_err(), expected);
        }
    }

    #[test]
    fn reader_seed_uses_constant_number_of_seeks() {
        struct CountingReader {
            inner: std::io::Cursor<Vec<u8>>,
            seeks: usize,
        }

        impl Read for CountingReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.inner.read(buf)
            }
        }

        impl Seek for CountingReader {
            fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
                self.seeks += 1;
                self.inner.seek(pos)
            }
        }

        let mut reader = CountingReader {
            inner: std::io::Cursor::new(state_pack(
                42,
                &[
                    ([1; 32], b"first"),
                    ([2; 32], b"second"),
                    ([3; 32], b"third"),
                ],
            )),
            seeks: 0,
        };
        seed_state_from_reader(&mut reader, 42).unwrap();
        assert_eq!(reader.seeks, 4);
    }

    #[test]
    fn reader_seed_rejects_malformed_state_packs() {
        let mut bad_magic = state_pack(7, &[]);
        bad_magic[0] = b'N';
        let mut bad_version = state_pack(7, &[]);
        bad_version[4] = 2;
        let mut bad_kind = state_pack(7, &[]);
        bad_kind[5] = KIND_LEDGER;
        let mut bad_count = state_pack(7, &[]);
        bad_count[14..18].copy_from_slice(&1u32.to_be_bytes());
        let mut truncated_payload = state_pack(7, &[([1; 32], b"payload")]);
        truncated_payload.pop();
        let mut oversized_length = state_pack(7, &[([1; 32], b"a")]);
        oversized_length[50..54].copy_from_slice(&u32::MAX.to_be_bytes());
        let mut truncated_second_header = state_pack(7, &[([1; 32], &[0; 50]), ([2; 32], b"")]);
        truncated_second_header.truncate(18 + 36 + 50 + 22);
        let mut trailing = state_pack(7, &[]);
        trailing.push(0);
        let mut trailing_after_entry = state_pack(7, &[([1; 32], b"a")]);
        trailing_after_entry.push(0);
        let cases = [
            (bad_magic, "bad magic"),
            (bad_version, "bad version"),
            (bad_kind, "wrong pack kind"),
            (state_pack(0, &[]), "outside the valid u32 ledger range"),
            (state_pack(8, &[]), "!= requested checkpoint"),
            (bad_count, "entry count exceeds remaining"),
            (truncated_payload, "truncated pack"),
            (oversized_length, "truncated pack"),
            (truncated_second_header, "truncated pack"),
            (
                state_pack(7, &[([3; 32], b"a"), ([3; 32], b"b")]),
                "duplicate state index",
            ),
            (trailing, "trailing bytes"),
            (trailing_after_entry, "trailing bytes"),
        ];
        for (pack, expected) in cases {
            let error = seed_state_from_reader(&mut std::io::Cursor::new(pack), 7).unwrap_err();
            assert!(
                error.contains(expected),
                "expected {expected:?}, got {error:?}"
            );
        }
        assert_eq!(
            seed_state_from_reader(&mut std::io::Cursor::new(b"XSCP"), 7).unwrap_err(),
            "truncated pack"
        );
    }

    #[test]
    fn file_seed_replay_matches_byte_slice_replay() {
        let mut parent_state = SHAMap::account_state();
        parent_state
            .insert(Hash256::new([1; 32]), b"account".to_vec())
            .unwrap();
        let mut parent_header = LedgerHeader::new();
        parent_header.sequence = 1;
        parent_header.account_hash = parent_state.root_hash();
        parent_header.hash = parent_header.compute_hash();
        let mut parent = Ledger::from_catchup(1, parent_header.hash, parent_state);
        parent.header = parent_header;
        let expected = replay_empty(&parent).ledger.header;
        let state = state_pack(1, &[([1; 32], b"account")]);
        let pack = ledger_pack(
            1,
            &[
                (1, parent.header.to_raw_bytes(), Vec::new()),
                (2, expected.to_raw_bytes(), Vec::new()),
            ],
        );
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&state).unwrap();
        let (memory_summary, mut memory_resume) =
            replay_segment_with_state(&state, &[&pack], 1, 2, 2, |_| Ok(())).unwrap();
        let (file_summary, mut file_resume) =
            replay_segment_with_state_file(file.path(), &[&pack], 1, 2, 2, |_| Ok(())).unwrap();
        assert_eq!(file_summary.processed, memory_summary.processed);
        assert_eq!(file_summary.stopped_early, memory_summary.stopped_early);
        assert_eq!(
            file_summary.account_hash_mismatches,
            memory_summary.account_hash_mismatches
        );
        assert_eq!(file_resume.header.hash, memory_resume.header.hash);
        assert_eq!(
            file_resume.state_map.root_hash(),
            memory_resume.state_map.root_hash()
        );
        let mut file_output = std::io::Cursor::new(Vec::new());
        let mut memory_output = std::io::Cursor::new(Vec::new());
        write_state_pack(
            &mut file_output,
            u64::from(file_resume.header.sequence),
            &file_resume.state_map,
        )
        .unwrap();
        write_state_pack(
            &mut memory_output,
            u64::from(memory_resume.header.sequence),
            &memory_resume.state_map,
        )
        .unwrap();
        assert_eq!(file_output.into_inner(), memory_output.into_inner());
    }

    fn ledger_pack(batch_start: u64, records: &[(u64, Vec<u8>, Vec<TxRecord>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(KIND_LEDGER);
        out.extend_from_slice(&batch_start.to_be_bytes());
        out.extend_from_slice(&(records.len() as u32).to_be_bytes());
        for (seq, header, txs) in records {
            out.extend_from_slice(&seq.to_be_bytes());
            out.extend_from_slice(&(header.len() as u32).to_be_bytes());
            out.extend_from_slice(header);
            out.extend_from_slice(&(txs.len() as u32).to_be_bytes());
            for tx in txs {
                out.extend_from_slice(&tx.tx_hash);
                out.extend_from_slice(&(tx.tx_blob.len() as u32).to_be_bytes());
                out.extend_from_slice(&tx.tx_blob);
                out.extend_from_slice(&(tx.meta_blob.len() as u32).to_be_bytes());
                out.extend_from_slice(&tx.meta_blob);
            }
        }
        out
    }

    fn tx_record(blob: &[u8]) -> TxRecord {
        TxRecord {
            tx_hash: *transaction_id(blob).as_bytes(),
            tx_blob: blob.to_vec(),
            meta_blob: vec![0xF1],
        }
    }

    #[test]
    fn observed_order_uses_metadata_transaction_index() {
        let record = |blob: &[u8], index: u64| TxRecord {
            tx_hash: *transaction_id(blob).as_bytes(),
            tx_blob: blob.to_vec(),
            meta_blob: rxrpl_codec::binary::encode(&serde_json::json!({
                "TransactionIndex": index,
                "TransactionResult": 0,
                "AffectedNodes": [],
            }))
            .unwrap(),
        };
        let ledger = LedgerRecord {
            seq: 2,
            header_blob: Vec::new(),
            txs: vec![
                record(b"third", 2),
                record(b"first", 0),
                record(b"second", 1),
            ],
        };

        let ordered = observed_order_txset(&ledger).unwrap();
        let blobs: Vec<Vec<u8>> = ordered.into_iter().map(|(_, blob)| blob).collect();
        assert_eq!(
            blobs,
            [b"first".to_vec(), b"second".to_vec(), b"third".to_vec()]
        );
    }

    fn empty_parent(sequence: u32) -> Ledger {
        let mut state = SHAMap::account_state();
        let mut header = LedgerHeader::new();
        header.sequence = sequence;
        header.account_hash = state.root_hash();
        header.hash = header.compute_hash();
        let mut ledger = Ledger::from_catchup(sequence, header.hash, state);
        ledger.header = header;
        ledger
    }

    fn replay_empty(parent: &Ledger) -> crate::play_forward::ReplayOutcome {
        let mut header = LedgerHeader::new();
        header.sequence = parent.header.sequence + 1;
        header.parent_hash = parent.header.hash;
        header.parent_close_time = parent.header.close_time;
        header.close_time = parent.header.close_time + 10;
        header.close_time_resolution = 10;
        replay_forward(
            parent,
            Hash256::ZERO,
            Vec::new(),
            &header,
            &all_handlers_engine(),
            &fees_for_ledger(parent),
        )
        .unwrap()
    }

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

    #[test]
    fn rejects_trailing_pack_bytes_and_duplicate_state_indexes() {
        let mut state = state_pack(7, &[]);
        state.push(0);
        assert!(unpack_state(&state).unwrap_err().contains("trailing bytes"));

        let duplicate = state_pack(7, &[([3; 32], b"a"), ([3; 32], b"b")]);
        assert!(
            unpack_state(&duplicate)
                .unwrap_err()
                .contains("duplicate state index")
        );

        let mut ledgers = ledger_pack(7, &[(7, b"header".to_vec(), Vec::new())]);
        ledgers.push(0);
        assert!(
            unpack_ledger_batch(&ledgers)
                .unwrap_err()
                .contains("trailing bytes")
        );
    }

    #[test]
    fn rejects_non_contiguous_ledgers_and_bad_packed_tx_hash() {
        let discontinuous = ledger_pack(7, &[(8, b"header".to_vec(), Vec::new())]);
        assert!(
            unpack_ledger_batch(&discontinuous)
                .unwrap_err()
                .contains("non-contiguous")
        );

        let mut bad_tx = tx_record(b"transaction");
        bad_tx.tx_hash = [9; 32];
        let pack = ledger_pack(7, &[(7, b"header".to_vec(), vec![bad_tx])]);
        assert!(
            unpack_ledger_batch(&pack)
                .unwrap_err()
                .contains("packed transaction hash mismatch")
        );
    }

    #[test]
    fn rejects_duplicate_transactions() {
        let tx = tx_record(b"transaction");
        let duplicate = TxRecord {
            tx_hash: tx.tx_hash,
            tx_blob: tx.tx_blob.clone(),
            meta_blob: tx.meta_blob.clone(),
        };
        let pack = ledger_pack(7, &[(7, b"header".to_vec(), vec![tx, duplicate])]);
        assert!(
            unpack_ledger_batch(&pack)
                .unwrap_err()
                .contains("duplicate packed transaction")
        );
    }

    #[test]
    fn emitted_meta_requires_exact_two_vl_segments() {
        let tx = tx_record(b"transaction");
        let txid = transaction_id(&tx.tx_blob);
        let mut ledger = Ledger::genesis();
        let mut leaf = rxrpl_codec::binary::encode_vl(&tx.tx_blob);
        leaf.extend_from_slice(&rxrpl_codec::binary::encode_vl(b"metadata"));
        ledger.add_transaction(txid, leaf.clone()).unwrap();
        let reports = emitted_meta(&ledger, &[tx_record(b"transaction")]).unwrap();
        assert_eq!(reports[0].meta_blob, b"metadata");

        let mut trailing_ledger = Ledger::genesis();
        leaf.push(0);
        trailing_ledger.add_transaction(txid, leaf).unwrap();
        assert!(
            emitted_meta(&trailing_ledger, &[tx_record(b"transaction")])
                .unwrap_err()
                .contains("trailing bytes")
        );

        let mut wrong_tx_ledger = Ledger::genesis();
        let mut wrong_leaf = rxrpl_codec::binary::encode_vl(b"different");
        wrong_leaf.extend_from_slice(&rxrpl_codec::binary::encode_vl(b"metadata"));
        wrong_tx_ledger.add_transaction(txid, wrong_leaf).unwrap();
        assert!(
            emitted_meta(&wrong_tx_ledger, &[tx_record(b"transaction")])
                .unwrap_err()
                .contains("blob differs")
        );
    }

    #[test]
    fn validates_parent_checkpoint_and_u32_bounds() {
        assert!(validate_range(9, 10, 12).is_ok());
        assert!(validate_range(10, 10, 12).is_err());
        assert!(validate_range(9, 12, 10).is_err());
        assert!(validate_range(9, 10, u64::from(u32::MAX) + 1).is_err());

        let oversized_state = state_pack(u64::from(u32::MAX) + 1, &[]);
        assert!(unpack_state(&oversized_state).is_err());
        let oversized_ledger = ledger_pack(
            u64::from(u32::MAX) + 1,
            &[(u64::from(u32::MAX) + 1, Vec::new(), Vec::new())],
        );
        assert!(unpack_ledger_batch(&oversized_ledger).is_err());
    }

    #[test]
    fn tx_hash_only_divergence_continues_with_pinned_parent_hash() {
        let parent = empty_parent(1);
        let first = replay_empty(&parent);
        let mut expected_first = first.ledger.header.clone();
        expected_first.tx_hash = Hash256::new([7; 32]);
        expected_first.hash = expected_first.compute_hash();

        let mut carried = first.ledger;
        carried.header.hash = expected_first.hash;
        let second = replay_empty(&carried);
        let expected_second = second.ledger.header.clone();

        let state = state_pack(1, &[]);
        let pack = ledger_pack(
            1,
            &[
                (1, parent.header.to_raw_bytes(), Vec::new()),
                (2, expected_first.to_raw_bytes(), Vec::new()),
                (3, expected_second.to_raw_bytes(), Vec::new()),
            ],
        );
        let mut reports = Vec::new();
        let (summary, resume) = replay_segment_with_state(&state, &[&pack], 1, 2, 3, |report| {
            reports.push(report);
            Ok(())
        })
        .unwrap();

        assert_eq!(reports.len(), 2);
        assert!(!reports[0].tx_hash_match);
        assert!(reports[0].account_hash_match);
        assert!(reports[0].drops_match);
        assert!(reports[1].account_hash_match);
        assert_eq!(summary.tx_hash_mismatches, 1);
        assert!(!summary.stopped_early);
        assert_eq!(resume.header.sequence, 3);
    }

    #[test]
    fn state_divergence_stops_after_first_report() {
        let parent = empty_parent(1);
        let first = replay_empty(&parent);
        let mut divergent = first.ledger.header.clone();
        divergent.account_hash = Hash256::new([8; 32]);
        divergent.hash = divergent.compute_hash();
        let mut next = LedgerHeader::new();
        next.sequence = 3;
        next.parent_hash = divergent.hash;
        next.hash = next.compute_hash();

        let state = state_pack(1, &[]);
        let pack = ledger_pack(
            1,
            &[
                (1, parent.header.to_raw_bytes(), Vec::new()),
                (2, divergent.to_raw_bytes(), Vec::new()),
                (3, next.to_raw_bytes(), Vec::new()),
            ],
        );
        let mut reports = Vec::new();
        let (summary, resume) = replay_segment_with_state(&state, &[&pack], 1, 2, 3, |report| {
            reports.push(report);
            Ok(())
        })
        .unwrap();

        assert_eq!(reports.len(), 1);
        assert!(!reports[0].account_hash_match);
        assert!(summary.stopped_early);
        assert_eq!(resume.header.sequence, 1);
    }

    #[test]
    fn failed_transaction_stops_after_first_report() {
        let parent = empty_parent(1);
        let first = replay_empty(&parent);
        let expected = first.ledger.header.clone();
        let mut next = LedgerHeader::new();
        next.sequence = 3;
        next.parent_hash = expected.hash;
        next.hash = next.compute_hash();

        let state = state_pack(1, &[]);
        let pack = ledger_pack(
            1,
            &[
                (1, parent.header.to_raw_bytes(), Vec::new()),
                (
                    2,
                    expected.to_raw_bytes(),
                    vec![tx_record(b"not an STObject")],
                ),
                (3, next.to_raw_bytes(), Vec::new()),
            ],
        );
        let mut reports = Vec::new();
        let summary = replay_segment_with(&state, &[&pack], 1, 2, 3, |report| {
            reports.push(report);
            Ok(())
        })
        .unwrap();

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].failed, 1);
        assert_eq!(summary.failed, 1);
        assert!(summary.stopped_early);
    }
}
