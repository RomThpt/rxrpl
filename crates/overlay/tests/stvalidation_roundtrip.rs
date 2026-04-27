//! Property tests for STValidation encode/decode round-trip.
//!
//! Generates randomised `Validation` structs (T11), signs each via
//! `NodeIdentity::sign_validation` (T09), encodes via `encode_validation`
//! (T07/T09), decodes via `decode_validation` (T10), and asserts that the
//! decoded struct preserves every SOTemplate field. Also asserts that the
//! signing payload is idempotent across `encode -> decode -> encode`.
//!
//! # Notes on which fields round-trip
//!
//! - `close_time` is NOT emitted by `sign_validation` (see the doc comment
//!   on that function: rxrpl-locally-built validations skip sfCloseTime).
//!   The decoder falls back to `close_time = sign_time` when the wire
//!   payload omits the field. This test follows that contract: we assert
//!   `decoded.close_time == original.sign_time`.
//! - All other SOTemplate fields (sfFlags, sfLedgerSequence, sfSigningTime,
//!   sfLedgerHash, sfLoadFee, sfReserveBase, sfReserveIncrement, sfBaseFee,
//!   sfCookie, sfServerVersion, sfConsensusHash, sfValidatedHash,
//!   sfBaseFeeDrops, sfReserveBaseDrops, sfReserveIncrementDrops,
//!   sfSigningPubKey, sfSignature, sfAmendments) round-trip byte-for-byte.

use proptest::prelude::*;
use rxrpl_consensus::types::{NodeId, Validation};
use rxrpl_overlay::identity::NodeIdentity;
use rxrpl_overlay::proto_convert::{decode_validation, encode_validation};
use rxrpl_primitives::Hash256;

fn arb_hash() -> impl Strategy<Value = Hash256> {
    any::<[u8; 32]>().prop_map(Hash256::new)
}

fn arb_amendments() -> impl Strategy<Value = Vec<Hash256>> {
    proptest::collection::vec(arb_hash(), 0..6)
}

/// Strategy for a fully-populated `Validation` with arbitrary values for
/// every optional SOTemplate field. The `node_id`, `public_key`,
/// `signature`, and `signing_payload` are filled in by the test (they
/// depend on the signing key).
fn arb_validation() -> impl Strategy<Value = Validation> {
    (
        // 12-tuple limit forces grouping
        (
            any::<u32>(),                        // ledger_seq
            any::<u32>(),                        // close_time (will be overwritten on decode)
            any::<u32>(),                        // sign_time
            arb_hash(),                          // ledger_hash
            any::<bool>(),                       // full
            arb_amendments(),                    // amendments
        ),
        (
            proptest::option::of(any::<u32>()),  // load_fee
            proptest::option::of(any::<u64>()),  // base_fee
            proptest::option::of(any::<u32>()),  // reserve_base
            proptest::option::of(any::<u32>()),  // reserve_increment
            proptest::option::of(any::<u64>()),  // cookie
            proptest::option::of(arb_hash()),    // consensus_hash
            proptest::option::of(arb_hash()),    // validated_hash
            proptest::option::of(any::<u64>()),  // server_version
            proptest::option::of(any::<u64>()),  // base_fee_drops (XRP amount, top bit cleared by encoder)
            proptest::option::of(any::<u64>()),  // reserve_base_drops
            proptest::option::of(any::<u64>()),  // reserve_increment_drops
        ),
    )
        .prop_map(|((seq, ct, st, lh, full, amends), opts)| {
            let (lf, bf, rb, ri, ck, ch, vh, sv, bfd, rbd, rid) = opts;
            // XRP amounts: rippled encodes drops as a 64-bit value with the
            // top bit set to indicate "XRP-native" and the second-top bit set
            // to indicate "positive". The encoder asserts drops fit in 62 bits
            // (max 4.6e18). Mask to 62 bits to stay within the valid range.
            let mask = (1u64 << 62) - 1;
            Validation {
                ledger_seq: seq,
                close_time: ct,
                sign_time: st,
                ledger_hash: lh,
                full,
                amendments: amends,
                load_fee: lf,
                base_fee: bf,
                reserve_base: rb,
                reserve_increment: ri,
                cookie: ck,
                consensus_hash: ch,
                validated_hash: vh,
                server_version: sv,
                base_fee_drops: bfd.map(|v| v & mask),
                reserve_base_drops: rbd.map(|v| v & mask),
                reserve_increment_drops: rid.map(|v| v & mask),
                ..Default::default()
            }
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Every SOTemplate field on a randomly-generated `Validation` survives
    /// `encode_validation -> decode_validation` without loss (modulo the
    /// `close_time` quirk documented at the top of this file).
    #[test]
    fn validation_encode_decode_roundtrip(mut v in arb_validation()) {
        let id = NodeIdentity::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("proptest-validator")
        );
        v.public_key = id.public_key_bytes().to_vec();
        v.node_id = NodeId(id.node_id);
        id.sign_validation(&mut v);

        let bytes = encode_validation(&v, &v.public_key);
        let decoded = decode_validation(&bytes).expect("decode failed");

        prop_assert_eq!(decoded.ledger_seq, v.ledger_seq);
        prop_assert_eq!(decoded.sign_time, v.sign_time);
        // sfCloseTime is not emitted by sign_validation; decoder fallback
        // sets close_time = sign_time when the wire payload omits it.
        prop_assert_eq!(decoded.close_time, v.sign_time);
        prop_assert_eq!(decoded.ledger_hash, v.ledger_hash);
        prop_assert_eq!(decoded.full, v.full);
        prop_assert_eq!(decoded.load_fee, v.load_fee);
        prop_assert_eq!(decoded.base_fee, v.base_fee);
        prop_assert_eq!(decoded.reserve_base, v.reserve_base);
        prop_assert_eq!(decoded.reserve_increment, v.reserve_increment);
        prop_assert_eq!(decoded.cookie, v.cookie);
        prop_assert_eq!(decoded.consensus_hash, v.consensus_hash);
        prop_assert_eq!(decoded.validated_hash, v.validated_hash);
        prop_assert_eq!(decoded.server_version, v.server_version);
        prop_assert_eq!(decoded.base_fee_drops, v.base_fee_drops);
        prop_assert_eq!(decoded.reserve_base_drops, v.reserve_base_drops);
        prop_assert_eq!(decoded.reserve_increment_drops, v.reserve_increment_drops);
        prop_assert_eq!(decoded.amendments, v.amendments);
        prop_assert_eq!(decoded.public_key, v.public_key);
        prop_assert_eq!(decoded.node_id, v.node_id);
        prop_assert_eq!(decoded.signature, v.signature);
        // The strip-result must round-trip byte-for-byte: any divergence
        // here would silently break signature verification on receive.
        prop_assert_eq!(decoded.signing_payload, v.signing_payload);
    }

    /// `encode -> decode -> encode` is idempotent: the second encode must
    /// produce a byte-identical wire image. This is the contract that
    /// guarantees signature verification works after relay.
    #[test]
    fn validation_signing_payload_idempotent(mut v in arb_validation()) {
        let id = NodeIdentity::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("proptest-validator")
        );
        v.public_key = id.public_key_bytes().to_vec();
        v.node_id = NodeId(id.node_id);
        id.sign_validation(&mut v);

        let bytes1 = encode_validation(&v, &v.public_key);
        let decoded = decode_validation(&bytes1).expect("decode failed");
        let bytes2 = encode_validation(&decoded, &decoded.public_key);
        prop_assert_eq!(bytes1, bytes2);
    }
}
