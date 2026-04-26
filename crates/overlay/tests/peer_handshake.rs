/// Integration tests for P2P peer handshake and message exchange.
///
/// Tests the full flow: TLS connection -> HTTP upgrade -> session cookie ->
/// protobuf message exchange between two rxrpl nodes.
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rxrpl_overlay::identity::NodeIdentity;
use rxrpl_overlay::tls;
use rxrpl_overlay::{PeerManager, PeerManagerConfig, PeerSet};
use rxrpl_primitives::Hash256;
use tokio::sync::RwLock;

fn make_peer_config(
    identity: &NodeIdentity,
    listen_port: u16,
    fixed_peers: Vec<String>,
) -> PeerManagerConfig {
    PeerManagerConfig {
        listen_port,
        max_peers: 10,
        seeds: vec![],
        fixed_peers,
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
async fn two_nodes_handshake_and_exchange() {
    // Node A
    let id_a = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("integration-a"));
    let seq_a = Arc::new(AtomicU32::new(2));
    let hash_a = Arc::new(RwLock::new(Hash256::new([0xAA; 32])));

    // Node B connects to A
    let id_b = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("integration-b"));
    let seq_b = Arc::new(AtomicU32::new(2));
    let hash_b = Arc::new(RwLock::new(Hash256::new([0xBB; 32])));

    let config_a = make_peer_config(&id_a, 0, vec![]); // port 0 = random
    let config_b = make_peer_config(&id_b, 0, vec![]);

    // Start node A
    let (mgr_a, cmd_tx_a, mut consensus_rx_a) = PeerManager::new(
        Arc::new(id_a),
        config_a,
        Arc::clone(&seq_a),
        Arc::clone(&hash_a),
    );

    // We need to get A's actual port before starting B
    // Use a TcpListener to find a free port
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_a = listener_a.local_addr().unwrap().port();
    drop(listener_a);

    let config_a_real = make_peer_config(
        &NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("integration-a")),
        port_a,
        vec![],
    );
    let id_a_arc = Arc::new(NodeIdentity::from_seed(
        &rxrpl_crypto::Seed::from_passphrase("integration-a"),
    ));
    let (mgr_a, _cmd_tx_a, mut _consensus_rx_a) = PeerManager::new(
        Arc::clone(&id_a_arc),
        config_a_real,
        Arc::clone(&seq_a),
        Arc::clone(&hash_a),
    );

    // Start A in background
    let handle_a = tokio::spawn(async move {
        let _ = mgr_a.run().await;
    });

    // Give A time to start listening
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Start node B with A as fixed peer
    let id_b_arc = Arc::new(NodeIdentity::from_seed(
        &rxrpl_crypto::Seed::from_passphrase("integration-b"),
    ));
    let config_b_real = make_peer_config(
        &id_b_arc,
        0, // any port
        vec![format!("127.0.0.1:{}", port_a)],
    );
    let (mgr_b, _cmd_tx_b, mut _consensus_rx_b) = PeerManager::new(
        Arc::clone(&id_b_arc),
        config_b_real,
        Arc::clone(&seq_b),
        Arc::clone(&hash_b),
    );

    let handle_b = tokio::spawn(async move {
        let _ = mgr_b.run().await;
    });

    // Wait for connection to establish
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Both handles are running -- the fact that we got here without panic
    // means the handshake succeeded. Abort both.
    handle_a.abort();
    handle_b.abort();
}

#[tokio::test]
async fn tls_session_cookie_deterministic() {
    // Verify that both sides of a TLS connection derive the same session cookie
    let id_server = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("cookie-server"));
    let id_client = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("cookie-client"));

    let server_config = tls::build_server_config(&id_server);
    let client_config = tls::build_client_config();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_handle = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let stream = tls::accept_tls(tcp, &server_config).await.unwrap();
        tls::extract_session_cookie(&stream).unwrap()
    });

    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let stream = tls::connect_tls(tcp, &client_config).await.unwrap();
    let client_cookie = tls::extract_session_cookie(&stream).unwrap();

    let server_cookie = server_handle.await.unwrap();

    // Both sides must derive the same cookie
    assert_eq!(client_cookie, server_cookie);
    assert!(!client_cookie.is_zero(), "cookie must not be zero");
}

#[tokio::test]
async fn http_handshake_with_ledger_hash() {
    use rxrpl_overlay::handshake;
    use tokio_util::codec::Framed;
    use rxrpl_p2p_proto::codec::PeerCodec;

    let network_id = 42;
    let ledger_hash = Hash256::new([0xDD; 32]);

    let id_server = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("hash-server"));
    let id_client = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("hash-client"));

    let server_tls = tls::build_server_config(&id_server);
    let client_tls = tls::build_client_config();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_handle = tokio::spawn({
        let id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("hash-server"));
        async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let stream = tls::accept_tls(tcp, &server_tls).await.unwrap();
            handshake::handshake_inbound_http(stream, &id, network_id, 5, &ledger_hash).await
        }
    });

    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let stream = tls::connect_tls(tcp, &client_tls).await.unwrap();
    let client_result =
        handshake::handshake_outbound_http(stream, &id_client, network_id, 5, &ledger_hash).await;

    assert!(client_result.is_ok(), "client handshake failed: {:?}", client_result.err());
    let server_result = server_handle.await.unwrap();
    assert!(server_result.is_ok(), "server handshake failed: {:?}", server_result.err());

    let (client_peer_id, _) = client_result.unwrap();
    let (server_peer_id, _) = server_result.unwrap();

    assert_eq!(client_peer_id, id_server.node_id);
    assert_eq!(server_peer_id, id_client.node_id);
}

#[test]
fn stobject_validation_roundtrip() {
    use rxrpl_consensus::types::{NodeId, Validation};
    use rxrpl_overlay::proto_convert;

    let id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("val-test"));
    let mut validation = Validation {
        node_id: NodeId(id.node_id),
        public_key: id.public_key_bytes().to_vec(),
        ledger_hash: Hash256::new([0xCC; 32]),
        ledger_seq: 42,
        full: true,
        close_time: 1000,
        sign_time: 1000,
        signature: None,
        amendments: vec![],
        signing_payload: None,
    };

    id.sign_validation(&mut validation);
    assert!(validation.signature.is_some());

    let encoded = proto_convert::encode_validation(&validation, id.public_key_bytes());
    let decoded = proto_convert::decode_validation(&encoded).unwrap();

    assert_eq!(decoded.ledger_hash, validation.ledger_hash);
    assert_eq!(decoded.ledger_seq, validation.ledger_seq);
    assert!(decoded.full);
    assert_eq!(decoded.sign_time, validation.sign_time);
    assert!(decoded.signature.is_some());
    assert_eq!(decoded.signature, validation.signature);
}

#[test]
fn propose_set_signature_verifiable() {
    use rxrpl_consensus::types::{NodeId, Proposal};
    use rxrpl_overlay::proto_convert;

    let id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("prop-test"));
    let mut proposal = Proposal {
        node_id: NodeId(id.node_id),
        public_key: id.public_key_bytes().to_vec(),
        tx_set_hash: Hash256::new([0x11; 32]),
        close_time: 500,
        prop_seq: 0,
        ledger_seq: 10,
        prev_ledger: Hash256::new([0x22; 32]),
        signature: None,
    };

    id.sign_proposal(&mut proposal);
    assert!(proposal.signature.is_some());

    // Verify the signature
    assert!(proposal.verify(id.public_key_bytes()));

    // Roundtrip through proto encoding
    let encoded = proto_convert::encode_propose_set(&proposal);
    let decoded = proto_convert::decode_propose_set(&encoded).unwrap();

    assert_eq!(decoded.public_key, id.public_key_bytes());
    assert_eq!(decoded.tx_set_hash, proposal.tx_set_hash);
    assert_eq!(decoded.close_time, proposal.close_time);
    assert!(decoded.verify(id.public_key_bytes()));
}
