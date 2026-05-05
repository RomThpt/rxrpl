//! Parser for the `LedgerHashes` SLE (the skip-list / hash chain used to
//! bootstrap a node from a known recent ledger without downloading every
//! ledger from genesis).
//!
//! Two flavors live in every closed ledger:
//!
//! 1. **Recent skip-list** at [`rxrpl_protocol::keylet::skip()`] — contains
//!    the hashes of up to the 256 most recent ledgers (FIFO, oldest first).
//! 2. **Historical batch** at [`rxrpl_protocol::keylet::skip_seq(seq)`] —
//!    every 65536 ledgers a new SLE is added that records the hash of every
//!    256-th ledger in that 65536-ledger batch. Walking these chains back
//!    lets a node jump from a recent validated ledger to any earlier one
//!    without fetching the full chain.
//!
//! ### Wire format (`LedgerHashes` SLE JSON)
//!
//! ```json
//! {
//!   "LedgerEntryType":     "LedgerHashes",
//!   "LastLedgerSequence":  90123456,
//!   "Hashes":              ["ABCDEF...", "012345...", ...]
//! }
//! ```
//!
//! `Hashes` is in chronological order (index 0 = oldest), so for a recent
//! skip-list the entry covering ledger N is at index `N - last + len - 1`
//! when present.

use std::str::FromStr;

use rxrpl_primitives::Hash256;
use serde_json::Value;

use crate::error::LedgerError;
use crate::sle_codec::decode_state;

/// A parsed `LedgerHashes` SLE.
#[derive(Clone, Debug)]
pub struct SkipListEntry {
    /// The most recent ledger sequence covered by this SLE.
    pub last_ledger_sequence: u32,
    /// Hashes in chronological order (oldest first). Length is at most 256.
    pub hashes: Vec<Hash256>,
}

impl SkipListEntry {
    /// Decode a `LedgerHashes` SLE from its raw state-map bytes.
    pub fn from_state_bytes(bytes: &[u8]) -> Result<Self, LedgerError> {
        let value = decode_state(bytes)
            .map_err(|e| LedgerError::Codec(format!("decode skip-list SLE: {e}")))?;
        Self::from_json(&value)
    }

    /// Parse a `LedgerHashes` SLE from its JSON representation.
    pub fn from_json(value: &Value) -> Result<Self, LedgerError> {
        let kind = value.get("LedgerEntryType").and_then(|v| v.as_str());
        if kind != Some("LedgerHashes") {
            return Err(LedgerError::Codec(format!(
                "expected LedgerHashes SLE, got {:?}",
                kind
            )));
        }
        let last_ledger_sequence = value
            .get("LastLedgerSequence")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| LedgerError::Codec("missing LastLedgerSequence".into()))?
            as u32;
        let arr = value
            .get("Hashes")
            .and_then(|v| v.as_array())
            .ok_or_else(|| LedgerError::Codec("missing Hashes array".into()))?;
        let hashes: Result<Vec<Hash256>, LedgerError> = arr
            .iter()
            .map(|h| {
                h.as_str()
                    .ok_or_else(|| LedgerError::Codec("non-string hash entry".into()))
                    .and_then(|s| {
                        Hash256::from_str(s)
                            .map_err(|e| LedgerError::Codec(format!("bad hash hex: {e}")))
                    })
            })
            .collect();

        Ok(Self {
            last_ledger_sequence,
            hashes: hashes?,
        })
    }

    /// Sequence number of the *first* (oldest) hash in [`Self::hashes`].
    pub fn first_seq(&self) -> u32 {
        // hashes[0] corresponds to (last_ledger_sequence - hashes.len() + 1)
        let len = self.hashes.len() as u32;
        self.last_ledger_sequence
            .saturating_sub(len.saturating_sub(1))
    }

    /// Returns the recorded hash for `seq`, if it falls inside this SLE.
    pub fn hash_for_seq(&self, seq: u32) -> Option<Hash256> {
        if seq > self.last_ledger_sequence {
            return None;
        }
        let first = self.first_seq();
        if seq < first {
            return None;
        }
        let idx = (seq - first) as usize;
        self.hashes.get(idx).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sle_json(last: u32, count: usize) -> Value {
        let hashes: Vec<String> = (0..count)
            .map(|i| {
                let mut bytes = [0u8; 32];
                bytes[31] = i as u8;
                Hash256::new(bytes).to_string()
            })
            .collect();
        serde_json::json!({
            "LedgerEntryType": "LedgerHashes",
            "LastLedgerSequence": last,
            "Hashes": hashes,
        })
    }

    #[test]
    fn parses_well_formed_sle() {
        let value = make_sle_json(1000, 5);
        let entry = SkipListEntry::from_json(&value).unwrap();
        assert_eq!(entry.last_ledger_sequence, 1000);
        assert_eq!(entry.hashes.len(), 5);
        assert_eq!(entry.first_seq(), 996);
    }

    #[test]
    fn hash_for_seq_within_range() {
        let value = make_sle_json(1000, 5);
        let entry = SkipListEntry::from_json(&value).unwrap();
        // hashes[0] is for ledger 996, hashes[4] is for 1000.
        assert!(entry.hash_for_seq(996).is_some());
        assert!(entry.hash_for_seq(1000).is_some());
        assert_eq!(entry.hash_for_seq(996).unwrap().as_bytes()[31], 0);
        assert_eq!(entry.hash_for_seq(1000).unwrap().as_bytes()[31], 4);
    }

    #[test]
    fn hash_for_seq_out_of_range() {
        let value = make_sle_json(1000, 5);
        let entry = SkipListEntry::from_json(&value).unwrap();
        assert!(entry.hash_for_seq(995).is_none());
        assert!(entry.hash_for_seq(1001).is_none());
    }

    #[test]
    fn rejects_wrong_entry_type() {
        let value = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "LastLedgerSequence": 1,
            "Hashes": [],
        });
        assert!(SkipListEntry::from_json(&value).is_err());
    }

    #[test]
    fn handles_short_skip_list() {
        let value = make_sle_json(2, 1);
        let entry = SkipListEntry::from_json(&value).unwrap();
        assert_eq!(entry.first_seq(), 2);
        assert!(entry.hash_for_seq(2).is_some());
    }
}
