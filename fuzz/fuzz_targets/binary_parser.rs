#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz the binary codec parser with arbitrary bytes
    let _ = rxrpl_codec::binary::decode(data);
});
