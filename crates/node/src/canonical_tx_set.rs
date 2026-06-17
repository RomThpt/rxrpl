//! Canonical transaction-set ordering, matching rippled's `CanonicalTXSet`.
//!
//! A consensus transaction set is agreed upon as an unordered collection keyed
//! by transaction id. rippled does not apply it in id order: it builds each
//! closed ledger by applying the set in `CanonicalTXSet` order. Same-account
//! transactions keep their `Sequence` order, while the inter-account order is
//! salted by the set's SHAMap root hash so the apply order cannot be biased by
//! choosing transaction ids. Reproducing this order is required for the
//! resulting `account_hash` to match the validated chain when replaying
//! transactions forward (play-forward sync).

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use serde_json::Value;

/// Reorder a transaction set `(tx_id, canonical_blob)` into rippled's canonical
/// apply order, salted by the set's SHAMap root hash.
pub fn canonical_order(
    set_hash: Hash256,
    mut txs: Vec<(Hash256, Vec<u8>)>,
) -> Vec<(Hash256, Vec<u8>)> {
    txs.sort_by_cached_key(|(txid, blob)| sort_key(set_hash, *txid, blob));
    txs
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct SortKey {
    salted_account: [u8; 32],
    sequence: u32,
    txid: Hash256,
}

fn sort_key(set_hash: Hash256, txid: Hash256, blob: &[u8]) -> SortKey {
    let (account, sequence) = decode_account_and_seq(blob);

    // rippled: uint256 key = 0; memcpy(key, account, 20); key ^= salt.
    let mut salted_account = [0u8; 32];
    salted_account[..20].copy_from_slice(&account);
    for (b, s) in salted_account.iter_mut().zip(set_hash.as_bytes().iter()) {
        *b ^= *s;
    }

    SortKey {
        salted_account,
        sequence,
        txid,
    }
}

/// Extract the signing account (20 bytes) and `Sequence` from a canonical blob.
/// A blob that fails to decode, or a pseudo-transaction with the zero account
/// and no sequence, sorts deterministically by `(zero, 0, txid)`.
fn decode_account_and_seq(blob: &[u8]) -> ([u8; 20], u32) {
    let json = match rxrpl_codec::binary::decode(blob) {
        Ok(v) => v,
        Err(_) => return ([0u8; 20], 0),
    };
    let account = json
        .get("Account")
        .and_then(Value::as_str)
        .and_then(|a| decode_account_id(a).ok())
        .map(|id| *id.as_bytes())
        .unwrap_or([0u8; 20]);
    let sequence = json.get("Sequence").and_then(Value::as_u64).unwrap_or(0) as u32;
    (account, sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_codec::address::classic::encode_account_id;
    use rxrpl_primitives::AccountId;

    fn blob(account: AccountId, sequence: u32) -> Vec<u8> {
        let json = serde_json::json!({
            "Account": encode_account_id(&account),
            "Sequence": sequence,
        });
        rxrpl_codec::binary::encode(&json).expect("encode test blob")
    }

    fn txid(byte: u8) -> Hash256 {
        Hash256::new([byte; 32])
    }

    #[test]
    fn same_account_ordered_by_sequence_not_txid() {
        let acct = AccountId([0x11; 20]);
        // High txid carries the low sequence: id order and sequence order disagree.
        let txs = vec![
            (txid(0xff), blob(acct, 1)),
            (txid(0x01), blob(acct, 3)),
            (txid(0x80), blob(acct, 2)),
        ];
        let ordered = canonical_order(txid(0xaa), txs);
        let seqs: Vec<u32> = ordered
            .iter()
            .map(|(_, b)| decode_account_and_seq(b).1)
            .collect();
        assert_eq!(seqs, vec![1, 2, 3]);
    }

    #[test]
    fn order_is_deterministic_for_a_given_salt() {
        let a = AccountId([0x22; 20]);
        let b = AccountId([0x33; 20]);
        let build = || {
            vec![
                (txid(0x10), blob(a, 1)),
                (txid(0x20), blob(b, 1)),
                (txid(0x30), blob(a, 2)),
            ]
        };
        let salt = txid(0x5e);
        let first = canonical_order(salt, build());
        let second = canonical_order(salt, build());
        let ids = |v: &[(Hash256, Vec<u8>)]| v.iter().map(|(h, _)| *h).collect::<Vec<_>>();
        assert_eq!(ids(&first), ids(&second));
        // Account a's two txs stay in sequence order relative to each other.
        let pos = |v: &[(Hash256, Vec<u8>)], id: Hash256| {
            v.iter().position(|(h, _)| *h == id).unwrap()
        };
        assert!(pos(&first, txid(0x10)) < pos(&first, txid(0x30)));
    }

    #[test]
    fn salt_can_change_inter_account_order() {
        let a = AccountId([0x01; 20]);
        let b = AccountId([0xfe; 20]);
        let build = || vec![(txid(0xa1), blob(a, 1)), (txid(0xb2), blob(b, 1))];
        let order_under = |salt: Hash256| {
            canonical_order(salt, build())
                .into_iter()
                .map(|(_, bl)| decode_account_and_seq(&bl).0)
                .collect::<Vec<_>>()
        };
        // Two salts chosen so the salted-account comparison flips. The salt
        // XORs the high bytes, so a salt close to b's account pulls it ahead.
        let mut salt_favoring_b = [0u8; 32];
        salt_favoring_b[..20].copy_from_slice(&[0xfe; 20]);
        let with_zero_salt = order_under(Hash256::new([0u8; 32]));
        let with_b_salt = order_under(Hash256::new(salt_favoring_b));
        assert_ne!(with_zero_salt, with_b_salt);
    }
}
