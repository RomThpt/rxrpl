//! Integration tests for manifest publishing (B1) + end-to-end peer
//! reception via the proactive-broadcast + gossip-relay path (B6).

use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use rxrpl_overlay::identity::{NodeIdentity, ValidatorIdentity};
use rxrpl_overlay::manifest::parse_and_verify;
use rxrpl_overlay::tls;
use rxrpl_overlay::{ConsensusMessage, PeerManager, PeerManagerConfig};
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

/// B6: end-to-end. Node A boots with a ValidatorIdentity (and a local
/// manifest set on its PeerManager). Node B has none. A and B handshake;
/// after the handshake the proactive-broadcast (B2) sends A's manifest
/// to B; B's standard `Manifests` handler verifies + applies it and emits
/// `ConsensusMessage::ManifestApplied` on its consensus channel.
///
/// We assert B observes the application carrying A's master public key
/// within a reasonable window — proves the full
/// `sign_manifest -> set_local -> handshake -> broadcast -> parse_and_verify
/// -> apply` chain works on real TCP/TLS sockets.
#[tokio::test]
async fn two_nodes_handshake_and_node_b_receives_node_a_manifest() {
    // Node A: full validator identity + local manifest.
    let p2p_id_a =
        NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("b6-a-p2p"));
    let validator_id_a = ValidatorIdentity::two_key(
        &rxrpl_crypto::Seed::from_passphrase("b6-a-master"),
        &rxrpl_crypto::Seed::from_passphrase("b6-a-signing"),
    );
    let manifest_a_bytes = validator_id_a
        .sign_manifest(1, Some("b6-a.example.com"))
        .expect("sign_manifest");
    let manifest_a = parse_and_verify(&manifest_a_bytes).expect("verify");
    let expected_master = manifest_a.master_public_key.clone();

    // Reserve a free port for A.
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_a = listener_a.local_addr().unwrap().port();
    drop(listener_a);

    let id_for_tls_a =
        NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("b6-a-p2p"));
    let cfg_a = PeerManagerConfig {
        listen_port: port_a,
        max_peers: 4,
        seeds: vec![],
        fixed_peers: vec![],
        network_id: 555,
        tls_server: tls::build_server_config(&id_for_tls_a),
        tls_client: tls::build_client_config(),
        cluster_enabled: false,
        cluster_node_name: String::new(),
        cluster_members: Vec::new(),
        cluster_broadcast_interval_secs: 5,
    };
    let (mut mgr_a, _cmd_tx_a, _consensus_rx_a) = PeerManager::new(
        Arc::new(p2p_id_a),
        cfg_a,
        Arc::new(AtomicU32::new(1)),
        Arc::new(RwLock::new(Hash256::new([0xAA; 32]))),
    );
    mgr_a.set_local_manifest(manifest_a);

    // Node B: no validator identity, only acts as receiver.
    let p2p_id_b =
        NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("b6-b-p2p"));
    let id_for_tls_b =
        NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("b6-b-p2p"));
    let cfg_b = PeerManagerConfig {
        listen_port: 0,
        max_peers: 4,
        seeds: vec![],
        fixed_peers: vec![format!("127.0.0.1:{port_a}")],
        network_id: 555,
        tls_server: tls::build_server_config(&id_for_tls_b),
        tls_client: tls::build_client_config(),
        cluster_enabled: false,
        cluster_node_name: String::new(),
        cluster_members: Vec::new(),
        cluster_broadcast_interval_secs: 5,
    };
    let (mgr_b, _cmd_tx_b, mut consensus_rx_b) = PeerManager::new(
        Arc::new(p2p_id_b),
        cfg_b,
        Arc::new(AtomicU32::new(1)),
        Arc::new(RwLock::new(Hash256::new([0xBB; 32]))),
    );

    let handle_a = tokio::spawn(async move {
        let _ = mgr_a.run().await;
    });
    let handle_b = tokio::spawn(async move {
        let _ = mgr_b.run().await;
    });

    // Wait for B to observe a ManifestApplied carrying A's master key.
    let observed = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            match consensus_rx_b.recv().await {
                Some(ConsensusMessage::ManifestApplied { master_key, .. })
                    if master_key == expected_master =>
                {
                    return true;
                }
                Some(_) => continue,
                None => return false,
            }
        }
    })
    .await
    .ok()
    .unwrap_or(false);

    handle_a.abort();
    handle_b.abort();

    assert!(
        observed,
        "node B must apply the manifest broadcast by node A within 8s"
    );
}
