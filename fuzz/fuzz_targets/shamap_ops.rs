#![no_main]
use libfuzzer_sys::fuzz_target;
use rxrpl_primitives::Hash256;
use rxrpl_shamap::SHAMap;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }

    let mut map = SHAMap::transaction();
    let mut pos = 0;

    while pos + 33 <= data.len() {
        let op = data[pos];
        pos += 1;

        let mut key_bytes = [0u8; 32];
        let copy_len = 32.min(data.len() - pos);
        key_bytes[..copy_len].copy_from_slice(&data[pos..pos + copy_len]);
        pos += copy_len;

        match op % 3 {
            0 => {
                // Insert with some value bytes
                let val_len = if pos < data.len() {
                    (data[pos] as usize) % 64
                } else {
                    0
                };
                pos += 1;
                if pos > data.len() {
                    break;
                }
                let end = (pos + val_len).min(data.len());
                let value = data[pos..end].to_vec();
                pos = end;
                let key = Hash256::new(key_bytes);
                let _ = map.insert(key, value);
            }
            1 => {
                // Delete
                let key = Hash256::new(key_bytes);
                let _ = map.delete(&key);
            }
            _ => {
                // Lookup
                let key = Hash256::new(key_bytes);
                let _ = map.get(&key);
            }
        }
    }

    // Compute hash to exercise the tree
    let _ = map.root_hash();
});
