//! Security regression test for the solo-validation guard (B1d).
//!
//! A validating node (validation seed configured) on the networked path
//! must refuse to start when its UNL would be empty -- no parseable
//! `[validators]` key and no validator-list publisher site. An empty UNL
//! puts the consensus engine in solo mode, where it accepts its own
//! ledgers as validated with no quorum: on a public network, a silent
//! fork. Legitimate solo validation goes through `run_standalone`.

#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use rxrpl_config::NodeConfig;
use rxrpl_node::Node;

const GENESIS_ADDR: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

fn write_seed_file_0600() -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("rxrpl_b1d_seed_{}", std::process::id()));
    let mut f = std::fs::File::create(&path).expect("create seed file");
    f.write_all(b"00112233445566778899aabbccddeeff\n")
        .expect("write seed");
    let mut perms = f.metadata().expect("metadata").permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(&path, perms).expect("chmod 0600");
    path
}

#[tokio::test]
async fn refuses_validating_with_empty_unl() {
    let seed_path = write_seed_file_0600();

    let mut config = NodeConfig::default();
    config.validators.enabled = true;
    config.validators.seed_file = Some(seed_path.clone());
    // Default: empty `trusted` and empty `validator_list_sites` -> the UNL
    // would be empty, so a validating node would close ledgers solo.

    let node = Node::new_standalone(config, GENESIS_ADDR).expect("node builds");
    let result = node.run_networked(3600, None, None).await;

    let _ = std::fs::remove_file(&seed_path);

    let err = result.expect_err("validating node with empty UNL must be refused");
    let msg = format!("{err}");
    assert!(
        msg.contains("trusted validators") || msg.contains("standalone"),
        "expected solo-validation refusal, got: {msg}"
    );
}
