//! Verifies that the operator-facing config templates under `config/`
//! parse cleanly into `NodeConfig`. If a new field is added to the
//! schema without updating these templates, this test should be updated
//! in the same PR (per the B1 contract in PLAN.md).

use std::path::PathBuf;

use rxrpl_config::load_config;

fn template(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../config");
    p.push(name);
    p
}

#[test]
fn mainnet_template_parses() {
    let cfg = load_config(template("rxrpl-mainnet.toml")).expect("mainnet template parses");
    assert_eq!(cfg.network.network_id, 0);
    assert_eq!(cfg.database.backend, "rocksdb");
    assert!(cfg.peer.tls_enabled);
    assert!(!cfg.peer.seeds.is_empty());
}

#[test]
fn testnet_template_parses() {
    let cfg = load_config(template("rxrpl-testnet.toml")).expect("testnet template parses");
    assert_eq!(cfg.network.network_id, 1);
    assert_eq!(cfg.database.backend, "rocksdb");
    assert!(cfg.peer.tls_enabled);
}

#[test]
fn standalone_template_parses() {
    let cfg = load_config(template("rxrpl-standalone.toml")).expect("standalone template parses");
    assert_eq!(cfg.database.backend, "memory");
    assert_eq!(cfg.peer.max_peers, 0);
    assert!(!cfg.peer.tls_enabled);
    assert_eq!(cfg.database.online_delete, 0);
}
