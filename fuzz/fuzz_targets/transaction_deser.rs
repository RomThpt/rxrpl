#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz transaction binary deserialization
    let _ = rxrpl_codec::binary::decode(data);

    // Also try parsing as JSON and encoding to binary
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(s) {
            let _ = rxrpl_codec::binary::encode(&json);
        }
    }
});
