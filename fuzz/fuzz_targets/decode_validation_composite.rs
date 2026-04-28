#![no_main]
//! Composite fuzz target for the full validation decode pipeline.
//!
//! `validation_deser` exercises the lower-level STObject parser by
//! feeding the raw bytes through every per-field decoder. This target
//! drives the *composite* decoder
//! [`rxrpl_overlay::proto_convert::decode_validation`] directly, which
//! chains `prost::Message::decode::<TmValidation>` with the STObject
//! walk, the canonical signing-payload reconstruction, and the field-id
//! dispatch table. Bugs that only surface from the composition (e.g.
//! length-prefix arithmetic between protobuf and STObject layers) need
//! a target that calls `decode_validation` as its only sink.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = rxrpl_overlay::proto_convert::decode_validation(data);
});
