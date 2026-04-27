#![no_main]
use libfuzzer_sys::fuzz_target;
use rxrpl_overlay::stobject;

fuzz_target!(|data: &[u8]| {
    let _ = stobject::decode_field_id(data);
    if data.len() > 1 {
        let mid = data.len() / 2;
        let _ = stobject::decode_vl_length(&data[..mid]);
        let _ = stobject::decode_vl_length(&data[mid..]);
    }
});
