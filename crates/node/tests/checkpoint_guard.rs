//! Security regression tests for the checkpoint bootstrap guard (C2).
//!
//! `--starting-ledger` (Seq or Recent) must refuse to start when no
//! trusted UNL is configured. Without the UNL filter, the
//! `CheckpointAnchor` would accept any 28 distinct keys as quorum, letting
//! an attacker bootstrap us onto a forged chain (Sybil).

use rxrpl_config::NodeConfig;
use rxrpl_node::{Node, StartingLedger};

const GENESIS_ADDR: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

#[tokio::test]
async fn refuses_starting_ledger_without_unl_sites() {
    let config = NodeConfig::default();
    // Default has empty validator_list_sites and validator_list_keys.
    let node = Node::new_standalone(config, GENESIS_ADDR).unwrap();

    let result = node
        .run_networked(3600, None, Some(StartingLedger::Seq(100)))
        .await;
    let err = result.expect_err("should refuse without UNL config");
    let msg = format!("{err}");
    assert!(
        msg.contains("validator_list_sites") || msg.contains("UNL"),
        "expected UNL-related error, got: {msg}"
    );
}

#[tokio::test]
async fn refuses_recent_without_unl_sites() {
    let config = NodeConfig::default();
    let node = Node::new_standalone(config, GENESIS_ADDR).unwrap();

    let result = node
        .run_networked(3600, None, Some(StartingLedger::Recent))
        .await;
    let err = result.expect_err("should refuse Recent without UNL config");
    let msg = format!("{err}");
    assert!(
        msg.contains("validator_list_sites") || msg.contains("UNL"),
        "expected UNL-related error, got: {msg}"
    );
}

#[tokio::test]
async fn refuses_when_require_trusted_disabled() {
    // Even with sites + keys configured, if `require_trusted_validators` is
    // false, the validation aggregator does not enforce the trust filter,
    // so the anchor would happily accept Sybil keys.
    let mut config = NodeConfig::default();
    config.validators.validator_list_sites = vec!["https://vl.ripple.com/".into()];
    config.validators.validator_list_keys =
        vec!["ED2677ABFFD1B33AC6FBC3062B71F1E8397A1505E1C42C64D11AD1B28FF73F4734".into()];
    config.validators.require_trusted_validators = false;
    let node = Node::new_standalone(config, GENESIS_ADDR).unwrap();

    let result = node
        .run_networked(3600, None, Some(StartingLedger::Seq(100)))
        .await;
    assert!(
        result.is_err(),
        "should refuse when require_trusted_validators is false"
    );
}
