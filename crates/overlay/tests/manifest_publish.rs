//! B1 integration tests: PeerManager exposes a local manifest set by Node
//! at boot, and the manifest is parseable & verifiable by the same code that
//! handles peer-supplied manifests (so peers can ingest ours symmetrically).

use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use rxrpl_overlay::identity::{NodeIdentity, ValidatorIdentity};
use rxrpl_overlay::manifest::parse_and_verify;
use rxrpl_overlay::tls;
use rxrpl_overlay::{PeerManager, PeerManagerConfig};
use rxrpl_primitives::Hash256;
use tokio::sync::RwLock;

fn make_peer_config(identity: &NodeIdentity, listen_port: u16) -> PeerManagerConfig {
    PeerManagerConfig {
        listen_port,
        max_peers: 10,
        seeds: vec![],
        fixed_peers: vec![],
        network_id: 99999,
        tls_server: tls::build_server_config(identity),
        tls_client: tls::build_client_config(),
        cluster_enabled: false,
        cluster_node_name: String::new(),
        cluster_members: Vec::new(),
        cluster_broadcast_interval_secs: 5,
    }
}

#[tokio::test]
async fn peer_manager_publishes_local_manifest_signed_by_validator_identity() {
    let p2p_id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("b1-p2p"));
    let validator_id = ValidatorIdentity::two_key(
        &rxrpl_crypto::Seed::from_passphrase("b1-master"),
        &rxrpl_crypto::Seed::from_passphrase("b1-signing"),
    );
    let manifest_bytes = validator_id
        .sign_manifest(1, Some("b1.example.com"))
        .expect("manifest must build");
    let manifest = parse_and_verify(&manifest_bytes).expect("our own manifest verifies");

    let (mut mgr, _cmd_tx, _consensus_rx) = PeerManager::new(
        Arc::new(p2p_id),
        make_peer_config(
            &NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("b1-p2p")),
            0,
        ),
        Arc::new(AtomicU32::new(1)),
        Arc::new(RwLock::new(Hash256::new([0xAA; 32]))),
    );

    assert!(mgr.local_manifest().is_none(), "no local manifest before set");

    mgr.set_local_manifest(manifest);

    let local = mgr.local_manifest().expect("local manifest after set");
    assert_eq!(local.sequence, 1, "manifest sequence preserved");
    assert_eq!(
        local.master_public_key.as_bytes(),
        validator_id.master_pubkey().as_bytes(),
        "local manifest's master must match ValidatorIdentity master"
    );
    assert_eq!(
        local.ephemeral_public_key.as_ref().unwrap().as_bytes(),
        validator_id.signing_pubkey().as_bytes(),
        "local manifest's ephemeral must match ValidatorIdentity signing"
    );

    // Symmetric verifiability: a peer receiving our manifest bytes must
    // accept them via the same parse_and_verify path it uses for any peer
    // manifest.
    let raw_bytes = local.raw.clone();
    let reparsed = parse_and_verify(&raw_bytes).expect("raw bytes must round-trip");
    assert_eq!(reparsed.sequence, 1);
}
