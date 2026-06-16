#![no_main]
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use rxrpl_overlay::ledger_sync::{LedgerSyncer, object_blob_to_wire};
use rxrpl_primitives::Hash256;
use rxrpl_shamap::InMemoryNodeStore;

// Fuzz the confluence catchup wire-decoding surface: the bytes a peer (rippled)
// sends in TMLedgerData / TMGetObjectByHash replies, which rxrpl parses while
// reconstructing ledger state. A panic here is a remotely-triggerable DoS, so
// every malformed shape must be rejected gracefully.
fuzz_target!(|data: &[u8]| {
    // 1. GetObjectByHash NodeObject blob -> SHAMap wire conversion.
    let _ = object_blob_to_wire(data);

    // 2. Full feed path: decode_wire_node + add_raw_node + the memoized
    //    missing_nodes / collect_missing frontier walk, driven by adversarial
    //    node bytes.
    let store = Arc::new(InMemoryNodeStore::new());
    let mut syncer = LedgerSyncer::new();
    let seq = 2u32;
    let target = Hash256::new([0x11u8; 32]);
    syncer.set_ledger_hash(seq, target);
    let _ = syncer.start_incremental_sync(seq, target, store);

    // Split the input into 2-byte length-prefixed wire nodes so one input can
    // feed several malformed nodes in a single round.
    let mut nodes: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut i = 0usize;
    while i + 2 <= data.len() && nodes.len() < 64 {
        let len = (((data[i] as usize) << 8) | data[i + 1] as usize).min(data.len() - i - 2);
        let start = i + 2;
        let end = start + len;
        nodes.push((Vec::new(), data[start..end].to_vec()));
        i = end;
    }
    if nodes.is_empty() {
        nodes.push((Vec::new(), data.to_vec()));
    }
    let _ = syncer.feed_nodes(seq, &nodes);
});
