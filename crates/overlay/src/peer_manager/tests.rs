use super::*;

#[test]
fn fetch_pack_query_replies_with_ancestor_headers() {
    // Late-joining rippled fires `TMGetObjectByHash{type=otFETCH_PACK,
    // query=true, ledger_hash=H}` after acquiring a validated head;
    // without a reply it never backfills history and complete_ledgers
    // stays "empty". Verify rxrpl emits a reply containing
    // (parent_hash, parent_seq, "LWR\0"||raw_header) for each ancestor.
    let mut mgr = make_test_peer_manager();
    let peer_id = mgr.1;
    let mut rx = mgr.2;
    let m = &mut mgr.0;

    // Build a 3-ledger chain: g (genesis) -> l1 -> l2.
    let mut g = rxrpl_ledger::Ledger::genesis();
    g.close(0, 0).unwrap();
    let mut l1 = rxrpl_ledger::Ledger::new_open(&g);
    l1.close(g.header.close_time + 4, 0).unwrap();
    let mut l2 = rxrpl_ledger::Ledger::new_open(&l1);
    l2.close(l1.header.close_time + 4, 0).unwrap();

    struct ChainProvider {
        by_hash: std::collections::HashMap<Hash256, rxrpl_ledger::Ledger>,
    }
    impl crate::ledger_provider::LedgerProvider for ChainProvider {
        fn get_by_hash(&self, h: &Hash256) -> Option<rxrpl_ledger::Ledger> {
            self.by_hash.get(h).cloned()
        }
        fn get_by_seq(&self, _: u32) -> Option<rxrpl_ledger::Ledger> {
            None
        }
        fn latest_closed(&self) -> Option<rxrpl_ledger::Ledger> {
            None
        }
    }
    let mut by_hash = std::collections::HashMap::new();
    by_hash.insert(g.header.hash, g.clone());
    by_hash.insert(l1.header.hash, l1.clone());
    by_hash.insert(l2.header.hash, l2.clone());
    m.set_ledger_provider(Arc::new(ChainProvider { by_hash }));

    let payload = proto_convert::encode_get_objects_query(
        6, // otFETCH_PACK
        true,
        Some(&l2.header.hash),
        vec![],
    );
    let decoded = proto_convert::decode_get_objects(&payload).expect("decode query");
    m.handle_get_objects_query(peer_id, decoded);

    let msg = rx.try_recv().expect("expected fetch pack reply");
    assert_eq!(msg.msg_type, MessageType::GetObjects);
    let reply = proto_convert::decode_get_objects(&msg.payload).expect("decode reply");
    assert_eq!(reply.r#type, Some(6));
    assert_eq!(reply.query, Some(false));
    // Two ancestors: l1 then g.
    assert_eq!(reply.objects.len(), 2);
    let first = &reply.objects[0];
    assert_eq!(first.ledger_seq, Some(l1.header.sequence));
    assert_eq!(
        first.hash.as_deref(),
        Some(l1.header.hash.as_bytes().as_slice())
    );
    let data = first.data.as_deref().unwrap();
    assert_eq!(
        &data[..4],
        b"LWR\0",
        "must lead with HashPrefix::ledgerMaster"
    );
    assert_eq!(data.len(), 4 + 118);
}

#[test]
fn encode_shamap_wire_node_inner_appends_inner_tag() {
    // Inner node storage = 16*32 child hashes; wire = same || 0x02.
    // Tagged byte must NOT depend on the leaf wireType argument since
    // inner nodes are tree-agnostic.
    let storage = vec![0xAB; 16 * 32];
    let wire = encode_shamap_wire_node(&storage, true, WIRE_TYPE_ACCOUNT_STATE);
    assert_eq!(wire.len(), 16 * 32 + 1);
    assert_eq!(&wire[..16 * 32], &storage[..]);
    assert_eq!(*wire.last().unwrap(), WIRE_TYPE_INNER);

    let wire_tx = encode_shamap_wire_node(&storage, true, WIRE_TYPE_TX_WITH_META);
    assert_eq!(*wire_tx.last().unwrap(), WIRE_TYPE_INNER);
}

/// A leaf whose data is exactly 480 bytes serializes to 512 storage bytes,
/// colliding with an inner node's 16×32 layout. With the explicit `is_inner`
/// flag it must still be tagged as a leaf, not an inner.
#[test]
fn encode_shamap_wire_node_480_byte_leaf_is_not_misread_as_inner() {
    let key = vec![0x55u8; 32];
    let data = vec![0x66u8; 480];
    let mut storage = Vec::new();
    storage.extend_from_slice(&key);
    storage.extend_from_slice(&data);
    assert_eq!(
        storage.len(),
        16 * 32,
        "this leaf collides with inner length"
    );

    let wire = encode_shamap_wire_node(&storage, false, WIRE_TYPE_ACCOUNT_STATE);
    assert_eq!(&wire[..480], &data[..]);
    assert_eq!(&wire[480..512], &key[..]);
    assert_eq!(*wire.last().unwrap(), WIRE_TYPE_ACCOUNT_STATE);
}

#[test]
fn encode_shamap_wire_node_account_state_leaf_reorders_and_tags() {
    // State leaf storage = key[32] || data; wire = data || key || 0x01.
    let key = vec![0x11u8; 32];
    let data = vec![0x22u8; 50];
    let mut storage = Vec::new();
    storage.extend_from_slice(&key);
    storage.extend_from_slice(&data);
    let wire = encode_shamap_wire_node(&storage, false, WIRE_TYPE_ACCOUNT_STATE);
    assert_eq!(wire.len(), 32 + 50 + 1);
    assert_eq!(&wire[..50], &data[..]);
    assert_eq!(&wire[50..82], &key[..]);
    assert_eq!(*wire.last().unwrap(), WIRE_TYPE_ACCOUNT_STATE);
}

#[test]
fn encode_shamap_wire_node_tx_leaf_uses_with_meta_tag() {
    // Tx leaf storage = key[32] || data; wire = data || key || 0x04.
    let key = vec![0x33u8; 32];
    let data = vec![0x44u8; 80];
    let mut storage = Vec::new();
    storage.extend_from_slice(&key);
    storage.extend_from_slice(&data);
    let wire = encode_shamap_wire_node(&storage, false, WIRE_TYPE_TX_WITH_META);
    assert_eq!(&wire[..80], &data[..]);
    assert_eq!(&wire[80..112], &key[..]);
    assert_eq!(*wire.last().unwrap(), WIRE_TYPE_TX_WITH_META);
}

#[test]
fn decode_validator_blob_extracts_count() {
    use base64::Engine;
    let json = serde_json::json!({
        "sequence": 1,
        "expiration": 999999999,
        "validators": [
            {"validation_public_key": "ED0001", "manifest": "AA=="},
            {"validation_public_key": "ED0002", "manifest": "BB=="},
            {"validation_public_key": "ED0003", "manifest": "CC=="},
        ]
    });
    let blob = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json).unwrap());
    assert_eq!(base64_decode_validator_blob(blob.as_bytes()), Ok(3));
}

#[test]
fn decode_validator_blob_invalid_base64() {
    assert_eq!(base64_decode_validator_blob(b"!!!invalid!!!"), Err(()));
}

#[test]
fn decode_validator_blob_no_validators_key() {
    use base64::Engine;
    let json = serde_json::json!({"sequence": 1});
    let blob = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json).unwrap());
    assert_eq!(base64_decode_validator_blob(blob.as_bytes()), Err(()));
}

#[test]
fn quorum_auto_compute_from_validator_count() {
    // Simulate the quorum calculation from node.rs:
    // new_quorum = ceil(count * 0.8)
    let count = 35usize;
    let quorum = (count as f64 * 0.8).ceil() as usize;
    assert_eq!(quorum, 28);

    let count = 10usize;
    let quorum = (count as f64 * 0.8).ceil() as usize;
    assert_eq!(quorum, 8);

    let count = 1usize;
    let quorum = (count as f64 * 0.8).ceil() as usize;
    assert_eq!(quorum, 1);
}

#[test]
fn backoff_exponential_increase() {
    let mut b = ReconnectBackoff::new();
    assert_eq!(b.next_delay(), Duration::from_secs(1));
    assert_eq!(b.next_delay(), Duration::from_secs(2));
    assert_eq!(b.next_delay(), Duration::from_secs(4));
    assert_eq!(b.next_delay(), Duration::from_secs(8));
    assert_eq!(b.next_delay(), Duration::from_secs(16));
}

#[test]
fn backoff_caps_at_max() {
    let mut b = ReconnectBackoff::new();
    // 1, 2, 4, 8, 16, 30, 30, ...
    for _ in 0..5 {
        b.next_delay();
    }
    assert_eq!(b.next_delay(), Duration::from_secs(30));
    assert_eq!(b.next_delay(), Duration::from_secs(30));
}

#[test]
fn backoff_reset_restores_initial() {
    let mut b = ReconnectBackoff::new();
    b.next_delay();
    b.next_delay();
    b.next_delay();
    assert_eq!(b.attempt(), 3);

    b.reset();
    assert_eq!(b.attempt(), 0);
    assert_eq!(b.next_delay(), Duration::from_secs(1));
}

#[test]
fn backoff_attempt_counter() {
    let mut b = ReconnectBackoff::new();
    assert_eq!(b.attempt(), 0);
    b.next_delay();
    assert_eq!(b.attempt(), 1);
    b.next_delay();
    assert_eq!(b.attempt(), 2);
}

/// LedgerProvider that always returns None — simulates an unknown ledger.
struct EmptyLedgerProvider;
impl crate::ledger_provider::LedgerProvider for EmptyLedgerProvider {
    fn get_by_hash(&self, _hash: &Hash256) -> Option<rxrpl_ledger::Ledger> {
        None
    }
    fn get_by_seq(&self, _seq: u32) -> Option<rxrpl_ledger::Ledger> {
        None
    }
    fn latest_closed(&self) -> Option<rxrpl_ledger::Ledger> {
        None
    }
}

fn make_test_peer_manager() -> (PeerManager, Hash256, mpsc::Receiver<PeerMessage>) {
    use crate::identity::NodeIdentity;
    use crate::peer_set::{PeerInfo, PeerSoftware};
    use crate::tls;
    use std::sync::atomic::AtomicU32;
    use tokio::sync::RwLock;

    let id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("test-peer-mgr"));
    let id_arc = Arc::new(id);
    let config = PeerManagerConfig {
        listen_port: 0,
        max_peers: 4,
        reserved_outbound_slots: 0,
        max_peers_per_ip: usize::MAX,
        seeds: vec![],
        fixed_peers: vec![],
        network_id: 1,
        tls_server: tls::build_server_config(&id_arc),
        tls_client: tls::build_client_config(),
        cluster_enabled: false,
        cluster_node_name: String::new(),
        cluster_members: vec![],
        cluster_broadcast_interval_secs: 5,
    };
    let (mut mgr, _cmd_tx, _consensus_rx) = PeerManager::new(
        id_arc,
        config,
        Arc::new(AtomicU32::new(0)),
        Arc::new(RwLock::new(Hash256::ZERO)),
    );

    let peer_id = Hash256::new([0xAB; 32]);
    let (tx, rx) = mpsc::channel::<PeerMessage>(8);
    let info = Arc::new(PeerInfo {
        node_id: peer_id,
        address: "127.0.0.1:0".into(),
        inbound: false,
        public_key: vec![0x03; 33],
        connected_at: std::time::Instant::now(),
        ledger_seq: AtomicU32::new(0),
        reputation: crate::reputation::PeerReputation::new(),
        scoring: crate::peer_score::PeerScore::new(),
        rate_limiter: crate::rate_limiter::PeerRateLimiter::default(),
        software: PeerSoftware::Unknown,
    });
    mgr.peer_handles.insert(
        peer_id,
        crate::peer_handle::PeerHandle {
            node_id: peer_id,
            info,
            tx,
        },
    );
    (mgr, peer_id, rx)
}

/// Like [`make_test_peer_manager`] but keeps the consensus receiver alive so a
/// test can observe the bounded overlay->consensus channel directly.
fn make_test_peer_manager_with_consensus() -> (PeerManager, mpsc::Receiver<ConsensusMessage>) {
    use crate::identity::NodeIdentity;
    use crate::tls;
    use std::sync::atomic::AtomicU32;
    use tokio::sync::RwLock;

    let id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("test-consensus-cap"));
    let id_arc = Arc::new(id);
    let config = PeerManagerConfig {
        listen_port: 0,
        max_peers: 4,
        reserved_outbound_slots: 0,
        max_peers_per_ip: usize::MAX,
        seeds: vec![],
        fixed_peers: vec![],
        network_id: 1,
        tls_server: tls::build_server_config(&id_arc),
        tls_client: tls::build_client_config(),
        cluster_enabled: false,
        cluster_node_name: String::new(),
        cluster_members: vec![],
        cluster_broadcast_interval_secs: 5,
    };
    let (mgr, _cmd_tx, consensus_rx) = PeerManager::new(
        id_arc,
        config,
        Arc::new(AtomicU32::new(0)),
        Arc::new(RwLock::new(Hash256::ZERO)),
    );
    (mgr, consensus_rx)
}

#[tokio::test]
async fn consensus_channel_sheds_when_consumer_stalled() {
    // A stalled consumer (consensus_rx never drained) must not let the bounded
    // overlay->consensus channel grow without limit: once full, forwarding
    // sheds (and counts) instead of OOMing the node under a tx/ledger-data
    // burst. This is the M0 hardening fix's whole point.
    let (mgr, mut consensus_rx) = make_test_peer_manager_with_consensus();

    // Fill exactly to capacity -- nothing shed while space remains.
    for _ in 0..CONSENSUS_CHANNEL_CAP {
        mgr.forward_to_consensus(ConsensusMessage::ValidatorListReceived { validator_count: 1 });
    }
    assert_eq!(
        mgr.consensus_dropped(),
        0,
        "must not shed while capacity remains"
    );

    // Overflow: each further send is shed and counted, and the buffer never
    // grows past capacity.
    for _ in 0..16 {
        mgr.forward_to_consensus(ConsensusMessage::ValidatorListReceived { validator_count: 1 });
    }
    assert_eq!(
        mgr.consensus_dropped(),
        16,
        "overflow must shed rather than grow the queue"
    );

    let mut buffered = 0;
    while consensus_rx.try_recv().is_ok() {
        buffered += 1;
    }
    assert_eq!(
        buffered, CONSENSUS_CHANNEL_CAP,
        "bounded channel must hold at most its capacity"
    );
}

#[tokio::test]
async fn consensus_channel_delivers_normal_flow() {
    // With a live consumer the bounded channel behaves transparently: the
    // message is delivered and nothing is shed.
    let (mgr, mut consensus_rx) = make_test_peer_manager_with_consensus();

    mgr.forward_to_consensus(ConsensusMessage::ValidatorListReceived { validator_count: 7 });

    match consensus_rx.recv().await {
        Some(ConsensusMessage::ValidatorListReceived { validator_count }) => {
            assert_eq!(validator_count, 7);
        }
        Some(_) => panic!("delivered the wrong ConsensusMessage variant"),
        None => panic!("channel closed without delivering"),
    }
    assert_eq!(mgr.consensus_dropped(), 0, "normal flow must not shed");
}

#[tokio::test]
async fn peer_manager_get_ledger_unknown_returns_no_response() {
    let (mut mgr, peer_id, mut rx) = make_test_peer_manager();
    mgr.set_ledger_provider(Arc::new(EmptyLedgerProvider));

    // Build a TMGetLedger request for an unknown ledger by seq.
    let payload = proto_convert::encode_get_ledger(
        1, // liBASE
        None, 999_999, // unknown seq
        42,      // cookie
    );

    mgr.handle_get_ledger(peer_id, &payload);

    // Allow any spawned task time (none expected); then assert nothing queued.
    match rx.try_recv() {
        Err(mpsc::error::TryRecvError::Empty) => {}
        Ok(msg) => panic!(
            "expected no TMLedgerData on not-found, got msg_type={:?}",
            msg.msg_type
        ),
        Err(e) => panic!("unexpected channel state: {e:?}"),
    }
}

#[tokio::test]
async fn peer_manager_get_tx_set_unknown_returns_no_response() {
    let (mgr, peer_id, mut rx) = make_test_peer_manager();
    // No tx_sets cache configured -> always miss.
    let unknown_tx_set = Hash256::new([0xCD; 32]);

    mgr.handle_get_tx_set(peer_id, unknown_tx_set.as_bytes(), &[], Some(7));

    match rx.try_recv() {
        Err(mpsc::error::TryRecvError::Empty) => {}
        Ok(msg) => panic!(
            "expected no TMLedgerData on tx-set not-found, got msg_type={:?}",
            msg.msg_type
        ),
        Err(e) => panic!("unexpected channel state: {e:?}"),
    }
}

/// LedgerProvider that always returns the genesis ledger — lets us drive
/// the liBASE response path of `handle_get_ledger`.
struct GenesisLedgerProvider;
impl crate::ledger_provider::LedgerProvider for GenesisLedgerProvider {
    fn get_by_hash(&self, _hash: &Hash256) -> Option<rxrpl_ledger::Ledger> {
        Some(rxrpl_ledger::Ledger::genesis())
    }
    fn get_by_seq(&self, _seq: u32) -> Option<rxrpl_ledger::Ledger> {
        Some(rxrpl_ledger::Ledger::genesis())
    }
    fn latest_closed(&self) -> Option<rxrpl_ledger::Ledger> {
        Some(rxrpl_ledger::Ledger::genesis())
    }
}

#[tokio::test]
async fn handle_get_ledger_propagates_cookie_when_present() {
    let (mut mgr, peer_id, mut rx) = make_test_peer_manager();
    mgr.set_ledger_provider(Arc::new(GenesisLedgerProvider));

    // liBASE request (itype=0) with explicit cookie 123.
    let payload = proto_convert::encode_get_ledger(0, None, 0, 123);
    mgr.handle_get_ledger(peer_id, &payload);

    let msg = rx.try_recv().expect("expected TMLedgerData response");
    assert_eq!(msg.msg_type, MessageType::LedgerData);
    let decoded = proto_convert::decode_ledger_data(&msg.payload).expect("decode");
    assert_eq!(decoded.request_cookie, Some(123));
}

#[tokio::test]
async fn handle_get_ledger_omits_cookie_when_absent_in_request() {
    let (mut mgr, peer_id, mut rx) = make_test_peer_manager();
    mgr.set_ledger_provider(Arc::new(GenesisLedgerProvider));

    // liBASE request with no cookie (encode_get_ledger drops cookie==0).
    let payload = proto_convert::encode_get_ledger(0, None, 0, 0);
    mgr.handle_get_ledger(peer_id, &payload);

    let msg = rx.try_recv().expect("expected TMLedgerData response");
    assert_eq!(msg.msg_type, MessageType::LedgerData);
    let decoded = proto_convert::decode_ledger_data(&msg.payload).expect("decode");
    assert!(
        decoded.request_cookie.is_none(),
        "cookie must be absent on wire when source request had no cookie; \
             rippled drops payload with set-but-unknown cookie via 'Unable to route'"
    );
}

#[test]
fn ledger_data_round_trip_preserves_node_payload() {
    let hash = Hash256::new([0x11; 32]);
    let payload = vec![0x42u8; 118];
    let nodes = vec![(vec![0x01u8; 33], payload.clone())];

    let bytes = proto_convert::encode_ledger_data(&hash, 12345, 1, nodes, Some(99));
    let decoded = proto_convert::decode_ledger_data(&bytes).expect("decode");

    assert_eq!(decoded.nodes.len(), 1, "nodes_size must be 1, not 0");
    assert_eq!(
        decoded.nodes[0].nodedata.as_ref().map(|d| d.len()),
        Some(118),
        "nodedata length must round-trip"
    );
    assert_eq!(decoded.nodes[0].nodedata.as_deref(), Some(&payload[..]));
    assert_eq!(decoded.ledger_seq, 12345);
    assert_eq!(decoded.ledger_info_type, 1);
    assert_eq!(decoded.request_cookie, Some(99));
}

/// B2: when a local manifest is set, `send_local_manifest_to(peer)`
/// pushes a `Manifests` message containing our manifest into the
/// peer's write channel. Verified by inserting a fake PeerHandle
/// (whose `tx` we own the receiver of) and reading back the bytes.
#[tokio::test]
async fn send_local_manifest_to_pushes_our_manifest_into_peer_channel() {
    use crate::identity::ValidatorIdentity;
    use crate::manifest::parse_and_verify;
    use crate::peer_handle::PeerHandle;
    use crate::tls;
    use std::sync::atomic::AtomicU32;
    use tokio::sync::RwLock as TokioRwLock;

    let p2p_id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("b2-p2p"));
    let validator_id = ValidatorIdentity::two_key(
        &rxrpl_crypto::Seed::from_passphrase("b2-master"),
        &rxrpl_crypto::Seed::from_passphrase("b2-signing"),
    );
    let manifest_bytes = validator_id.sign_manifest(1, None).expect("sign_manifest");
    let manifest = parse_and_verify(&manifest_bytes).expect("parse_and_verify");
    let expected_master = manifest.master_public_key.clone();

    let cfg = PeerManagerConfig {
        listen_port: 0,
        max_peers: 4,
        reserved_outbound_slots: 0,
        max_peers_per_ip: usize::MAX,
        seeds: vec![],
        fixed_peers: vec![],
        network_id: 12345,
        tls_server: tls::build_server_config(&NodeIdentity::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("b2-p2p"),
        )),
        tls_client: tls::build_client_config(),
        cluster_enabled: false,
        cluster_node_name: String::new(),
        cluster_members: Vec::new(),
        cluster_broadcast_interval_secs: 5,
    };
    let (mut mgr, _cmd_tx, _consensus_rx) = PeerManager::new(
        Arc::new(p2p_id),
        cfg,
        Arc::new(AtomicU32::new(1)),
        Arc::new(TokioRwLock::new(Hash256::new([0; 32]))),
    );
    mgr.set_local_manifest(manifest);

    // Inject a fake peer with an mpsc whose receiver we keep.
    let peer_id = Hash256::new([0xCC; 32]);
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let info = Arc::new(crate::peer_set::PeerInfo {
        node_id: peer_id,
        address: "127.0.0.1:0".into(),
        inbound: true,
        public_key: vec![0x03; 33],
        connected_at: std::time::Instant::now(),
        ledger_seq: AtomicU32::new(0),
        reputation: crate::reputation::PeerReputation::new(),
        scoring: crate::peer_score::PeerScore::new(),
        rate_limiter: crate::rate_limiter::PeerRateLimiter::default(),
        software: crate::peer_set::PeerSoftware::Unknown,
    });
    mgr.peer_handles.insert(
        peer_id,
        PeerHandle {
            node_id: peer_id,
            info,
            tx,
        },
    );

    // Act
    mgr.send_local_manifest_to(&peer_id);

    // Assert
    let msg = rx
        .recv()
        .await
        .expect("send_local_manifest_to must enqueue a message");
    assert_eq!(msg.msg_type, MessageType::Manifests);
    let parsed = crate::proto_convert::decode_manifests(&msg.payload)
        .expect("payload is a valid TmManifests");
    assert_eq!(parsed.len(), 1, "exactly one manifest in the broadcast");
    let inner = parsed[0].stobject.as_ref().expect("inner stobject");
    let reparsed = parse_and_verify(inner).expect("inner manifest verifies");
    assert_eq!(
        reparsed.master_public_key, expected_master,
        "broadcast carries our master pubkey"
    );
}

/// B3: when peer A delivers a Manifests message, the manager applies
/// the manifest AND re-broadcasts it to every other connected peer
/// (so the network converges on validator key bindings without
/// linear handshake-distance lag).
///
/// Verifies:
///   - applied to local manifest_store
///   - delivered to peer B's channel
///   - NOT echoed back to peer A
#[tokio::test]
async fn dispatch_manifest_relays_to_other_peers_but_not_back_to_sender() {
    use crate::identity::ValidatorIdentity;
    use crate::peer_handle::PeerHandle;
    use crate::tls;
    use std::sync::atomic::AtomicU32;
    use tokio::sync::RwLock as TokioRwLock;

    // Build a freshly-signed manifest that A will deliver.
    let validator_id = ValidatorIdentity::two_key(
        &rxrpl_crypto::Seed::from_passphrase("b3-foreign-master"),
        &rxrpl_crypto::Seed::from_passphrase("b3-foreign-signing"),
    );
    let raw_manifest = validator_id.sign_manifest(1, None).expect("sign_manifest");
    let payload = crate::proto_convert::encode_manifests(vec![raw_manifest]);

    let p2p_id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("b3-p2p"));
    let cfg = PeerManagerConfig {
        listen_port: 0,
        max_peers: 4,
        reserved_outbound_slots: 0,
        max_peers_per_ip: usize::MAX,
        seeds: vec![],
        fixed_peers: vec![],
        network_id: 12345,
        tls_server: tls::build_server_config(&NodeIdentity::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("b3-p2p"),
        )),
        tls_client: tls::build_client_config(),
        cluster_enabled: false,
        cluster_node_name: String::new(),
        cluster_members: Vec::new(),
        cluster_broadcast_interval_secs: 5,
    };
    let (mut mgr, _cmd_tx, _consensus_rx) = PeerManager::new(
        Arc::new(p2p_id),
        cfg,
        Arc::new(AtomicU32::new(1)),
        Arc::new(TokioRwLock::new(Hash256::new([0; 32]))),
    );

    let id_a = Hash256::new([0xAA; 32]);
    let id_b = Hash256::new([0xBB; 32]);
    let (tx_a, mut rx_a) = tokio::sync::mpsc::channel(4);
    let (tx_b, mut rx_b) = tokio::sync::mpsc::channel(4);
    for (id, tx) in [(id_a, tx_a), (id_b, tx_b)] {
        let info = Arc::new(crate::peer_set::PeerInfo {
            node_id: id,
            address: "127.0.0.1:0".into(),
            inbound: true,
            public_key: vec![0x03; 33],
            connected_at: std::time::Instant::now(),
            ledger_seq: AtomicU32::new(0),
            reputation: crate::reputation::PeerReputation::new(),
            scoring: crate::peer_score::PeerScore::new(),
            rate_limiter: crate::rate_limiter::PeerRateLimiter::default(),
            software: crate::peer_set::PeerSoftware::Unknown,
        });
        // Register in peer_set so dispatch_message recognises the sender
        // for reputation accounting (otherwise peer_info is None and
        // some paths short-circuit).
        mgr.peer_set.add(Arc::clone(&info));
        mgr.peer_handles.insert(
            id,
            PeerHandle {
                node_id: id,
                info,
                tx,
            },
        );
    }

    mgr.dispatch_message(id_a, MessageType::Manifests, &payload);

    // B receives the relayed payload (with a hard timeout so a RED
    // test fails fast instead of hanging the test runner).
    let relayed = tokio::time::timeout(std::time::Duration::from_millis(500), rx_b.recv())
        .await
        .expect("peer B must receive the relayed manifest within 500ms")
        .expect("channel must not close");
    assert_eq!(relayed.msg_type, MessageType::Manifests);
    assert_eq!(relayed.payload, payload);

    // A must NOT receive an echo.
    match rx_a.try_recv() {
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
        other => panic!(
            "sender peer A must not receive an echo, got: {:?}",
            other.is_ok()
        ),
    }

    // And the manifest is applied locally.
    assert_eq!(mgr.manifest_store.len(), 1, "manifest applied locally");
}

/// Calling `send_local_manifest_to` without a local manifest is a
/// no-op; the peer's write channel stays empty.
#[tokio::test]
async fn send_local_manifest_to_is_noop_without_local_manifest() {
    use crate::peer_handle::PeerHandle;
    use crate::tls;
    use std::sync::atomic::AtomicU32;
    use tokio::sync::RwLock as TokioRwLock;

    let p2p_id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("b2-noop"));
    let cfg = PeerManagerConfig {
        listen_port: 0,
        max_peers: 4,
        reserved_outbound_slots: 0,
        max_peers_per_ip: usize::MAX,
        seeds: vec![],
        fixed_peers: vec![],
        network_id: 12345,
        tls_server: tls::build_server_config(&NodeIdentity::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("b2-noop"),
        )),
        tls_client: tls::build_client_config(),
        cluster_enabled: false,
        cluster_node_name: String::new(),
        cluster_members: Vec::new(),
        cluster_broadcast_interval_secs: 5,
    };
    let (mut mgr, _cmd_tx, _consensus_rx) = PeerManager::new(
        Arc::new(p2p_id),
        cfg,
        Arc::new(AtomicU32::new(1)),
        Arc::new(TokioRwLock::new(Hash256::new([0; 32]))),
    );

    let peer_id = Hash256::new([0xDD; 32]);
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let info = Arc::new(crate::peer_set::PeerInfo {
        node_id: peer_id,
        address: "127.0.0.1:0".into(),
        inbound: true,
        public_key: vec![0x03; 33],
        connected_at: std::time::Instant::now(),
        ledger_seq: AtomicU32::new(0),
        reputation: crate::reputation::PeerReputation::new(),
        scoring: crate::peer_score::PeerScore::new(),
        rate_limiter: crate::rate_limiter::PeerRateLimiter::default(),
        software: crate::peer_set::PeerSoftware::Unknown,
    });
    mgr.peer_handles.insert(
        peer_id,
        PeerHandle {
            node_id: peer_id,
            info,
            tx,
        },
    );

    mgr.send_local_manifest_to(&peer_id);

    // Channel must stay empty.
    match rx.try_recv() {
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
        other => panic!("expected empty channel, got {:?}", other.is_ok()),
    }
}

fn signed_validation() -> Validation {
    use crate::identity::NodeIdentity;
    use rxrpl_consensus::types::NodeId;

    let id = NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_passphrase("val-signer"));
    let mut v = Validation {
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
        ..Default::default()
    };
    id.sign_validation(&mut v);
    v
}

#[tokio::test]
async fn verified_validation_forwarded_only_when_valid() {
    let (mut mgr, mut consensus_rx) = make_test_peer_manager_with_consensus();
    let peer_id = Hash256::new([0xAB; 32]);
    let v = signed_validation();
    let payload = proto_convert::encode_validation(&v, &v.public_key);

    // A validation whose signature verified is forwarded to consensus.
    mgr.handle_verified_validation(peer_id, payload.clone(), v.clone(), true);
    assert!(matches!(
        consensus_rx.try_recv(),
        Ok(ConsensusMessage::Validation(_))
    ));

    // One that failed verification is dropped, never reaching consensus.
    mgr.handle_verified_validation(peer_id, payload, v, false);
    assert!(consensus_rx.try_recv().is_err());
}

#[tokio::test]
async fn validation_dispatch_offloads_signature_verify() {
    let (mut mgr, _consensus_rx) = make_test_peer_manager_with_consensus();
    let peer_id = Hash256::new([0xAB; 32]);
    let v = signed_validation();
    let payload = proto_convert::encode_validation(&v, &v.public_key);

    mgr.dispatch_message(peer_id, MessageType::Validation, &payload);

    // The verify worker only runs inside run(); in this unit context the job
    // waits in the queue, proving dispatch offloaded the signature check off the
    // event loop instead of verifying inline.
    let job = mgr
        .validation_verify_rx
        .as_mut()
        .expect("verify receiver present")
        .try_recv()
        .expect("verify job queued");
    assert_eq!(job.validation.ledger_seq, 42);
    assert!(crate::identity::verify_validation_signature(&job.validation));
}

#[tokio::test]
async fn validation_offload_removes_verify_from_event_loop() {
    // The event loop's per-validation cost drops from "decode + verify" to
    // "decode + enqueue": the signature verify now runs on the worker. This
    // asserts the loop path is cheaper than the inline path it replaced, which
    // is what unblocks the sync pipeline during a validation burst.
    let (mut mgr, _consensus_rx) = make_test_peer_manager_with_consensus();
    let peer_id = Hash256::new([0xAB; 32]);
    let v = signed_validation();
    let payload = proto_convert::encode_validation(&v, &v.public_key);
    const N: usize = 2000;

    let t0 = std::time::Instant::now();
    for _ in 0..N {
        mgr.dispatch_message(peer_id, MessageType::Validation, &payload);
        let _ = mgr.validation_verify_rx.as_mut().unwrap().try_recv();
    }
    let offload = t0.elapsed();

    let t1 = std::time::Instant::now();
    for _ in 0..N {
        let _ = proto_convert::decode_validation(&payload);
        let _ = crate::identity::verify_validation_signature(&v);
    }
    let inline = t1.elapsed();

    eprintln!("event-loop per-validation over {N}: offload {offload:?} vs inline {inline:?}");
    assert!(
        offload < inline,
        "offloading the verify must make the event-loop path cheaper: offload={offload:?} inline={inline:?}"
    );
}
