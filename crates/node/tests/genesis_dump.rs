use rxrpl_codec::address;
use rxrpl_shamap::SHAMap;
use rxrpl_storage::{MemoryNodeDatabase, NodeStore};
use std::sync::Arc;

#[test]
fn dump_isolated_shamap_with_vs_without_store() {
    let key = rxrpl_protocol::keylet::account(&address::decode_account_id("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh").unwrap());
    let sle_hex = "1100612200000000240000000125000000002D0000000055000000000000000000000000000000000000000000000000000000000000000062416345785D8A00008114B5F762798A53D543A014CAF8B297CFF8F2F937E8";
    let sle: Vec<u8> = (0..sle_hex.len()).step_by(2).map(|i| u8::from_str_radix(&sle_hex[i..i+2], 16).unwrap()).collect();

    let mut m1 = SHAMap::account_state();
    m1.put(key, sle.clone()).unwrap();
    eprintln!("WITHOUT_STORE root_hash={}", m1.root_hash());

    let store: std::sync::Arc<dyn rxrpl_storage::NodeStore> = Arc::new(MemoryNodeDatabase::new());
    let mut m2 = SHAMap::account_state_with_store(store);
    m2.put(key, sle.clone()).unwrap();
    eprintln!("WITH_STORE root_hash={}", m2.root_hash());
}

#[test]
fn dump_full_genesis() {
    let genesis = rxrpl_node::Node::genesis_with_funded_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh").unwrap();
    let key = rxrpl_protocol::keylet::account(&address::decode_account_id("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh").unwrap());
    let sle_bytes = genesis.get_state(&key).unwrap();
    eprintln!("FULL_GENESIS account_hash={} hash={} sle_len={} sle={}", genesis.header.account_hash, genesis.header.hash, sle_bytes.len(), sle_bytes.iter().map(|b| format!("{:02X}", b)).collect::<String>());
}

#[test]
fn dump_via_new_standalone() {
    let mut config = rxrpl_config::NodeConfig::default();
    config.database.backend = "memory".to_string();
    let node = rxrpl_node::Node::new_standalone(config, "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh").unwrap();
    let closed = node.closed_ledgers().blocking_read();
    if let Some(g) = closed.iter().find(|l| l.header.sequence == 1) {
        let key = rxrpl_protocol::keylet::account(&address::decode_account_id("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh").unwrap());
        match g.get_state(&key) {
            Some(b) => eprintln!("VIA_NEW_STANDALONE account_hash={} hash={} sle_len={} sle={}", g.header.account_hash, g.header.hash, b.len(), b.iter().map(|x| format!("{:02X}", x)).collect::<String>()),
            None => eprintln!("VIA_NEW_STANDALONE: no SLE for genesis account"),
        }
    } else {
        eprintln!("VIA_NEW_STANDALONE: no genesis in closed_ledgers");
    }
}
