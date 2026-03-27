#![no_main]
use libfuzzer_sys::fuzz_target;

use prost::Message;

fuzz_target!(|data: &[u8]| {
    // Fuzz protobuf TmValidation decoding
    let _ = rxrpl_p2p_proto::proto::TmValidation::decode(data);

    // Fuzz the full validation decode pipeline (protobuf + STObject parsing)
    let _ = rxrpl_overlay::proto_convert::decode_validation(data);

    // Fuzz propose set decode
    let _ = rxrpl_overlay::proto_convert::decode_propose_set(data);

    // Fuzz other P2P message decoders
    let _ = rxrpl_overlay::proto_convert::decode_status_change(data);
    let _ = rxrpl_overlay::proto_convert::decode_hello(data);
    let _ = rxrpl_overlay::proto_convert::decode_ping(data);
    let _ = rxrpl_overlay::proto_convert::decode_get_ledger(data);
    let _ = rxrpl_overlay::proto_convert::decode_ledger_data(data);
    let _ = rxrpl_overlay::proto_convert::decode_peers(data);
    let _ = rxrpl_overlay::proto_convert::decode_manifest(data);
    let _ = rxrpl_overlay::proto_convert::decode_manifests(data);
    let _ = rxrpl_overlay::proto_convert::decode_endpoints(data);
    let _ = rxrpl_overlay::proto_convert::decode_have_set(data);
    let _ = rxrpl_overlay::proto_convert::decode_get_objects(data);
    let _ = rxrpl_overlay::proto_convert::decode_squelch(data);
    let _ = rxrpl_overlay::proto_convert::decode_validator_list(data);
    let _ = rxrpl_overlay::proto_convert::decode_validator_list_collection(data);
    let _ = rxrpl_overlay::proto_convert::decode_have_transactions(data);
    let _ = rxrpl_overlay::proto_convert::decode_transactions(data);
    let _ = rxrpl_overlay::proto_convert::decode_transaction(data);
});
