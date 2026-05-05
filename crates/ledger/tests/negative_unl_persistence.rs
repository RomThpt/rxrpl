//! Integration test for batch C-B7: ensure the NegativeUNL ledger entry
//! survives ledger close and can be reconstructed from the SHAMap state
//! leaves (mirroring the catchup / restart code path).

use rxrpl_ledger::{sle_codec, Ledger};
use rxrpl_protocol::keylet;
use rxrpl_shamap::SHAMap;
use serde_json::json;

#[test]
fn negative_unl_survives_ledger_close_and_reconstruction() {
    let mut ledger = Ledger::genesis();

    // Build a NegativeUNL SLE with one disabled validator entry. Use the
    // same JSON shape that `UNLModify` handlers emit so the binary codec
    // round-trips unambiguously.
    let validator_key = "ED0000000000000000000000000000000000000000000000000000000000000005";
    let nunl_json = json!({
        "LedgerEntryType": "NegativeUNL",
        "Flags": 0,
        "DisabledValidators": [
            {
                "DisabledValidator": {
                    "PublicKey": validator_key,
                    "FirstLedgerSequence": 256u32,
                }
            }
        ],
    });
    let nunl_bytes = serde_json::to_vec(&nunl_json).unwrap();
    let key = keylet::negative_unl();
    ledger.put_state(key, nunl_bytes).unwrap();

    // Closing must not drop the entry.
    ledger.close(100, 0).unwrap();
    let after_close = ledger
        .get_state(&key)
        .expect("NegativeUNL must survive ledger close");
    let decoded = sle_codec::decode_state(after_close).unwrap();
    assert_eq!(decoded["LedgerEntryType"], "NegativeUNL");
    let disabled = decoded["DisabledValidators"].as_array().unwrap();
    assert_eq!(disabled.len(), 1);
    assert_eq!(
        disabled[0]["DisabledValidator"]["PublicKey"]
            .as_str()
            .unwrap(),
        validator_key,
    );

    // Reconstruct from leaves -- the path used after a node restart when
    // catching up state from peers / disk. The NegativeUNL must be
    // present and decode correctly.
    let mut leaves = Vec::new();
    ledger.state_map.for_each(&mut |k, d| {
        leaves.push((k.as_bytes().to_vec(), d.to_vec()));
    });
    let reconstructed_state = SHAMap::from_leaf_nodes(&leaves).unwrap();
    let reconstructed = Ledger::from_catchup(
        ledger.header.sequence,
        ledger.header.hash,
        reconstructed_state,
    );

    let reloaded = reconstructed
        .get_state(&key)
        .expect("NegativeUNL must be reloadable after reconstruction");
    let reloaded_value = sle_codec::decode_state(reloaded).unwrap();
    assert_eq!(reloaded_value["LedgerEntryType"], "NegativeUNL");
    assert_eq!(
        reloaded_value["DisabledValidators"]
            .as_array()
            .unwrap()
            .len(),
        1,
    );
}
