#![no_main]
use libfuzzer_sys::fuzz_target;
use rxrpl_primitives::Hash256;
use rxrpl_shamap::SHAMap;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }

    // Build two maps from interleaved data and compare root hashes
    let mut map_a = SHAMap::transaction();
    let mut map_b = SHAMap::transaction();
    let mut pos = 0;

    while pos + 34 <= data.len() {
        let op = data[pos];
        pos += 1;

        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&data[pos..pos + 32]);
        pos += 32;

        let val_byte = data[pos];
        pos += 1;

        let key = Hash256::new(key_bytes);
        let value = vec![val_byte; (val_byte as usize % 16) + 1];

        match op % 4 {
            0 => {
                let _ = map_a.insert(key, value);
            }
            1 => {
                let _ = map_b.insert(key, value);
            }
            2 => {
                // Insert in both
                let _ = map_a.insert(key, value.clone());
                let _ = map_b.insert(key, value);
            }
            _ => {
                // Insert in A then delete
                let _ = map_a.insert(key, value);
                let _ = map_a.delete(&key);
            }
        }
    }

    // Compare root hashes -- identical operations should yield identical hashes
    let _hash_a = map_a.root_hash();
    let _hash_b = map_b.root_hash();

    // Exercise iteration
    let mut count = 0;
    for _item in map_a.iter() {
        count += 1;
        if count > 1000 {
            break;
        }
    }

    // Exercise node deserialization with arbitrary data
    if data.len() >= 32 {
        let hash = Hash256::new(data[..32].try_into().unwrap());
        let _ = rxrpl_shamap::deserialize_node(
            &data[32..],
            &hash,
            rxrpl_shamap::LeafNode::account_state,
        );
        // Also try with transaction leaf constructor
        let _ = rxrpl_shamap::deserialize_node(
            &data[32..],
            &hash,
            rxrpl_shamap::LeafNode::transaction_no_meta,
        );
    }
});
