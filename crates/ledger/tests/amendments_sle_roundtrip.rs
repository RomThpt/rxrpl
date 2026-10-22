//! The `Amendments` singleton is hashed into every ledger's state tree, so its
//! binary encoding must match rippled byte-for-byte. Its `Majorities` STArray
//! wraps each entry in an inner `Majority` object; a regression that dropped the
//! wrapper (or emitted an empty array instead of omitting the field) would make
//! the codec fall back to raw JSON and silently fork the account_hash whenever
//! an amendment holds majority.
//!
//! Fixture: the real mainnet Amendments SLE at ledger 104193793 (contains one
//! `Majority` with `CloseTime` = parent close time), hash-verified on-chain.

use rxrpl_ledger::sle_codec::{decode_sle, encode_sle};

const AMENDMENTS_SLE: &str = include_str!("fixtures/amendments_sle_104193793.hex");

#[test]
fn amendments_sle_with_majority_roundtrips_byte_exact() {
    let bin = hex::decode(AMENDMENTS_SLE.trim()).unwrap();

    let json_bytes = decode_sle(&bin).expect("decode");
    let value: serde_json::Value = serde_json::from_slice(&json_bytes).unwrap();

    // The STArray element is wrapped in a `Majority` object.
    let majority = &value["Majorities"][0]["Majority"];
    assert_eq!(
        majority["Amendment"].as_str().unwrap(),
        "303ACB16CF8DBD3B5C34F131A9D19A7DE01AE05F480A8A682B869D1B4AAC8CFC"
    );
    assert_eq!(majority["CloseTime"].as_u64().unwrap(), 831959361);

    // Re-encoding must reproduce the exact on-chain bytes (real binary, not a
    // JSON fallback).
    let reencoded = encode_sle(&json_bytes).expect("encode");
    assert_eq!(reencoded, bin, "Amendments SLE must round-trip byte-exact");
    assert_eq!(
        reencoded[0], 0x11,
        "must be binary (LedgerEntryType marker)"
    );
}
