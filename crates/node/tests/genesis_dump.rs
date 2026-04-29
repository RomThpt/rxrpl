use rxrpl_codec::address;

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
