#[test]
fn dump_full_genesis() {
    let genesis = rxrpl_node::Node::genesis_with_funded_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh").unwrap();
    eprintln!("FULL_GENESIS account_hash={} hash={}", genesis.header.account_hash, genesis.header.hash);
}
