#![no_main]
use libfuzzer_sys::fuzz_target;

use prost::Message;
use rxrpl_primitives::Hash256;

fuzz_target!(|data: &[u8]| {
    // Fuzz protobuf decoding of consensus ProposeSet messages
    let _ = rxrpl_p2p_proto::proto::TmProposeSet::decode(data);

    // Fuzz protobuf decoding of StatusChange messages
    let _ = rxrpl_p2p_proto::proto::TmStatusChange::decode(data);

    // Fuzz TxSet construction from arbitrary hashes
    if data.len() >= 32 {
        let mut hashes = Vec::new();
        let mut pos = 0;
        while pos + 32 <= data.len() {
            let mut buf = [0u8; 32];
            buf.copy_from_slice(&data[pos..pos + 32]);
            hashes.push(Hash256::new(buf));
            pos += 32;
        }
        if !hashes.is_empty() {
            let set = rxrpl_consensus::types::TxSet::new(hashes);
            let _ = set.hash;
            let _ = set.len();
        }
    }

    // Fuzz DisputedTx threshold logic with arbitrary data
    if data.len() >= 34 {
        let key = Hash256::new(data[..32].try_into().unwrap());
        let our_vote = data[32] & 1 == 1;
        let mut tx = rxrpl_consensus::types::DisputedTx::new(key, our_vote);

        let mut pos = 33;
        while pos + 33 <= data.len() {
            let mut node_key = [0u8; 32];
            node_key.copy_from_slice(&data[pos..pos + 32]);
            let vote = data[pos + 32] & 1 == 1;
            tx.vote(
                rxrpl_consensus::types::NodeId(Hash256::new(node_key)),
                vote,
            );
            pos += 33;
        }

        // Exercise threshold logic with different thresholds
        for threshold in [0, 25, 50, 75, 100] {
            let _ = tx.should_include(threshold);
        }
        let _ = tx.yay_count();
        let _ = tx.nay_count();
    }
});
