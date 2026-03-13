/// Cross-validated hash vectors from rippled (C++) and goXRPLd (Go).
///
/// These test vectors are from the TestBuildAndTear test in goXRPLd,
/// which itself matches the rippled C++ implementation.
///
/// Map type: Transaction (leaves use TransactionNoMeta hashing)
/// Values: intToBytes(k) = 32 bytes filled with byte(k)
///   e.g. key 0 -> data [0x00; 32], key 3 -> data [0x03; 32]
use std::str::FromStr;

use rxrpl_primitives::Hash256;
use rxrpl_shamap::SHAMap;

/// The 8 test keys (from goXRPLd TestBuildAndTear).
fn test_keys() -> Vec<Hash256> {
    [
        "b92891fe4ef6cee585fdc6fda1e09eb4d386363158ec3321b8123e5a772c6ca8",
        "b92881fe4ef6cee585fdc6fda1e09eb4d386363158ec3321b8123e5a772c6ca8",
        "b92691fe4ef6cee585fdc6fda1e09eb4d386363158ec3321b8123e5a772c6ca8",
        "b92791fe4ef6cee585fdc6fda1e09eb4d386363158ec3321b8123e5a772c6ca8",
        "b91891fe4ef6cee585fdc6fda1e09eb4d386363158ec3321b8123e5a772c6ca8",
        "b99891fe4ef6cee585fdc6fda1e09eb4d386363158ec3321b8123e5a772c6ca8",
        "f22891fe4ef6cee585fdc6fda1e09eb4d386363158ec3321b8123e5a772c6ca8",
        "292891fe4ef6cee585fdc6fda1e09eb4d386363158ec3321b8123e5a772c6ca8",
    ]
    .iter()
    .map(|s| Hash256::from_str(s).unwrap())
    .collect()
}

/// Expected root hashes after inserting each key in order.
fn expected_hashes() -> Vec<Hash256> {
    [
        "B7387CFEA0465759ADC718E8C42B52D2309D179B326E239EB5075C64B6281F7F",
        "FBC195A9592A54AB44010274163CB6BA95F497EC5BA0A8831845467FB2ECE266",
        "4E7D2684B65DFD48937FFB775E20175C43AF0C94066F7D5679F51AE756795B75",
        "7A2F312EB203695FFD164E038E281839EEF06A1B99BFC263F3CECC6C74F93E07",
        "395A6691A372387A703FB0F2C6D2C405DAF307D0817F8F0E207596462B0E3A3E",
        "D044C0A696DE3169CC70AE216A1564D69DE96582865796142CE7D98A84D9DDE4",
        "76DCC77C4027309B5A91AD164083264D70B77B5E43E08AEDA5EBF94361143615",
        "DF4220E93ADC6F5569063A01B4DC79F8DB9553B6A3222ADE23DEA02BBE7230E5",
    ]
    .iter()
    .map(|s| Hash256::from_str(s).unwrap())
    .collect()
}

/// Value for key index k: 32 bytes filled with byte(k).
fn int_to_bytes(k: u8) -> Vec<u8> {
    vec![k; 32]
}

#[test]
fn build_and_verify_root_hashes() {
    let keys = test_keys();
    let expected = expected_hashes();
    let mut map = SHAMap::transaction();

    for (i, key) in keys.iter().enumerate() {
        map.put(*key, int_to_bytes(i as u8)).unwrap();
        let root = map.root_hash();
        assert_eq!(
            root, expected[i],
            "root hash mismatch after inserting key {i}\n  got:      {root}\n  expected: {}",
            expected[i]
        );
    }
}

#[test]
fn tear_down_and_verify_root_hashes() {
    let keys = test_keys();
    let expected = expected_hashes();
    let mut map = SHAMap::transaction();

    // Build the full tree
    for (i, key) in keys.iter().enumerate() {
        map.put(*key, int_to_bytes(i as u8)).unwrap();
    }
    assert_eq!(map.root_hash(), *expected.last().unwrap());

    // Delete in reverse order, verifying hash reverts at each step
    for i in (0..keys.len()).rev() {
        map.delete(&keys[i]).unwrap();
        if i == 0 {
            assert_eq!(
                map.root_hash(),
                Hash256::ZERO,
                "empty tree should have zero root hash"
            );
        } else {
            assert_eq!(
                map.root_hash(),
                expected[i - 1],
                "root hash mismatch after deleting key {i}\n  got:      {}\n  expected: {}",
                map.root_hash(),
                expected[i - 1]
            );
        }
    }

    assert!(map.is_empty());
}

#[test]
fn final_empty_tree_has_zero_hash() {
    let keys = test_keys();
    let mut map = SHAMap::transaction();

    for (i, key) in keys.iter().enumerate() {
        map.put(*key, int_to_bytes(i as u8)).unwrap();
    }

    for key in &keys {
        map.delete(key).unwrap();
    }

    assert_eq!(map.root_hash(), Hash256::ZERO);
    assert!(map.is_empty());
}

#[test]
fn large_scale_insert_and_retrieve() {
    let mut map = SHAMap::transaction();
    let mut keys = Vec::new();

    for i in 0u32..1000 {
        let mut key_bytes = [0u8; 32];
        // Spread keys across the tree using a simple hash-like pattern
        let b = i.to_be_bytes();
        key_bytes[0] = b[0];
        key_bytes[1] = b[1];
        key_bytes[2] = b[2];
        key_bytes[3] = b[3];
        // Add some entropy in later bytes to avoid trivial patterns
        key_bytes[4] = (i.wrapping_mul(0x9E3779B9) >> 24) as u8;
        key_bytes[5] = (i.wrapping_mul(0x9E3779B9) >> 16) as u8;

        let key = Hash256::new(key_bytes);
        let data = vec![
            (i & 0xFF) as u8,
            ((i >> 8) & 0xFF) as u8,
        ];
        map.put(key, data).unwrap();
        keys.push(key);
    }

    // Verify all 1000 entries can be retrieved
    for (i, key) in keys.iter().enumerate() {
        let i = i as u32;
        let expected = vec![
            (i & 0xFF) as u8,
            ((i >> 8) & 0xFF) as u8,
        ];
        assert_eq!(
            map.get(key),
            Some(expected.as_slice()),
            "failed to retrieve key {i}"
        );
    }

    // Root hash should be non-zero and deterministic
    let h1 = map.root_hash();
    assert!(!h1.is_zero());

    // Build the same tree in a different order (reversed) and verify same hash
    let mut map2 = SHAMap::transaction();
    for (i, key) in keys.iter().enumerate().rev() {
        let i = i as u32;
        let data = vec![
            (i & 0xFF) as u8,
            ((i >> 8) & 0xFF) as u8,
        ];
        map2.put(*key, data).unwrap();
    }
    assert_eq!(map2.root_hash(), h1, "insertion order should not affect root hash");
}

#[test]
fn insert_order_independence_with_test_keys() {
    let keys = test_keys();
    let mut map1 = SHAMap::transaction();
    let mut map2 = SHAMap::transaction();

    // Forward order
    for (i, key) in keys.iter().enumerate() {
        map1.put(*key, int_to_bytes(i as u8)).unwrap();
    }

    // Reverse order
    for (i, key) in keys.iter().enumerate().rev() {
        map2.put(*key, int_to_bytes(i as u8)).unwrap();
    }

    assert_eq!(map1.root_hash(), map2.root_hash());
}
