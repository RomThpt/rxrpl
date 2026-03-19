#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Try to decode, if successful re-encode and verify roundtrip
    if let Ok(json) = rxrpl_codec::binary::decode(data) {
        if let Ok(re_encoded) = rxrpl_codec::binary::encode(&json) {
            // Decode again and compare JSON values
            if let Ok(json2) = rxrpl_codec::binary::decode(&re_encoded) {
                assert_eq!(json, json2, "roundtrip mismatch");
            }
        }
    }
});
