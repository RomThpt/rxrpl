//! LIVE 3-node consensus cluster driven by a DYNAMIC validator list (VL)
//! served over local HTTP.
//!
//! This is the end-to-end counterpart to `overlay/tests/dynamic_vl_trust_e2e.rs`
//! (which proves the trust plumbing in isolation, no live nodes). Here we spin
//! up three real `Node::run_networked` instances, cross-wire them as fixed
//! peers, and point every node at a single local HTTP endpoint that serves a
//! REAL publisher-signed VL listing the three cluster validators' MASTER keys
//! (each with its real ephemeral->master manifest). No static `[validators]`
//! trust list is configured — the ONLY source of trust is the dynamic VL.
//!
//! Consistency contract (the crux): the (master, ephemeral) secp256k1 keypairs
//! embedded in the VL manifests are derived from the SAME 16-byte seeds that
//! each node is configured with via `validator_identity.master_secret` /
//! `ephemeral_seed` (hex form). We build both from a single
//! `ValidatorIdentity::two_key_typed(Secp256k1)` per validator, exactly the
//! derivation `build_validator_identity` performs in `node.rs`, so the VL and
//! the node agree byte-for-byte on every key.
//!
//! SUCCESS = all three nodes report the SAME `validated_ledger.hash` at
//! `seq >= 2` within the deadline. On timeout the test dumps each node's last
//! server_info (peers, quorum, complete_ledgers, validated tip) so the failure
//! is diagnosable, and the captured `RUST_LOG` tracing shows peer handshakes,
//! VL fetch, and consensus rounds.
//!
//! Run:  cargo test -p rxrpl-node --test dynamic_vl_live_cluster -- --nocapture

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use rxrpl_config::NodeConfig;
use rxrpl_crypto::{KeyPair, KeyType, Seed};
use rxrpl_node::Node;
use rxrpl_overlay::identity::ValidatorIdentity;
use rxrpl_overlay::manifest;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const GENESIS: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
const NETWORK_ID: u32 = 42;

// Fixed high ports (single-test binary; run with --test-threads=1 if needed).
const PEER_PORTS: [u16; 3] = [39211, 39212, 39213];
const RPC_PORTS: [u16; 3] = [39221, 39222, 39223];

const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

// ---------------------------------------------------------------------------
// Key material
// ---------------------------------------------------------------------------

/// Deterministic, non-zero 16-byte seed entropy for a given salt.
fn seed16(salt: u8) -> [u8; 16] {
    let mut s = [0u8; 16];
    for (i, b) in s.iter_mut().enumerate() {
        *b = salt.wrapping_mul(37).wrapping_add(i as u8).wrapping_add(1);
    }
    s
}

/// ed25519 keypair from a passphrase (publisher identity — the VL path accepts
/// ed25519 publishers, matching mainnet's vl.ripple.com).
fn kp(seed: &str) -> KeyPair {
    KeyPair::from_seed(&Seed::from_passphrase(seed), KeyType::Ed25519)
}

/// A cluster validator: the pubkey + real manifest that go into the VL, plus
/// the hex seeds that go into the node config. Built from ONE
/// `ValidatorIdentity` so the VL and the node derive identical keys.
struct ClusterValidator {
    master_pub: Vec<u8>,
    manifest_bytes: Vec<u8>,
    master_secret_hex: String,
    ephemeral_seed_hex: String,
}

fn make_validator(master_salt: u8, ephemeral_salt: u8) -> ClusterValidator {
    let mseed = seed16(master_salt);
    let eseed = seed16(ephemeral_salt);
    // Exactly what node.rs `build_validator_identity` does for the config's
    // (master_secret, ephemeral_seed) hex pair: secp256k1 validator derivation.
    let vid = ValidatorIdentity::two_key_typed(
        &Seed::from_bytes(mseed),
        KeyType::Secp256k1,
        &Seed::from_bytes(eseed),
        KeyType::Secp256k1,
    );
    // sequence 1, no domain — same sequence the node's own local manifest uses
    // when `validator_identity.sequence == 1`, so manifest stores never see a
    // stale/conflicting sequence for the same ephemeral->master binding.
    let manifest_bytes = vid.sign_manifest(1, None).expect("sign validator manifest");
    ClusterValidator {
        master_pub: vid.master_pubkey().as_bytes().to_vec(),
        manifest_bytes,
        master_secret_hex: hex::encode(mseed),
        ephemeral_seed_hex: hex::encode(eseed),
    }
}

/// Build the full VL HTTP payload JSON string that the fetcher consumes:
/// `{public_key, manifest, blob, signature, version}`. The blob lists the 3
/// validator masters + their manifests; it is signed (decoded bytes) with the
/// publisher's EPHEMERAL key, matching `validator_list::verify_and_parse`.
fn build_vl_payload(
    pub_master: &KeyPair,
    pub_eph: &KeyPair,
    validators: &[ClusterValidator],
) -> String {
    let publisher_manifest =
        manifest::create_signed(pub_master, pub_eph, 1, None).expect("publisher manifest");

    let entries: Vec<Value> = validators
        .iter()
        .map(|v| {
            json!({
                "validation_public_key": hex::encode_upper(&v.master_pub),
                "manifest": B64.encode(&v.manifest_bytes),
            })
        })
        .collect();

    let blob = json!({
        "sequence": 1u64,
        "expiration": 4_000_000_000u64,
        "validators": entries,
    });
    let blob_raw = serde_json::to_vec(&blob).expect("serialize blob");
    let blob_b64 = B64.encode(&blob_raw);
    // Publisher signs the DECODED blob bytes (see verify_and_parse).
    let sig = rxrpl_crypto::ed25519::sign(&blob_raw, &pub_eph.private_key).expect("sign blob");

    let payload = json!({
        "public_key": hex::encode_upper(pub_master.public_key.as_bytes()),
        "manifest": B64.encode(&publisher_manifest),
        "blob": blob_b64,
        "signature": hex::encode(sig.as_bytes()),
        "version": 1,
    });
    serde_json::to_string(&payload).expect("serialize payload")
}

// ---------------------------------------------------------------------------
// Minimal HTTP server for the VL (no new dependency; raw TcpListener).
// ---------------------------------------------------------------------------

async fn serve_vl_forever(listener: TcpListener, body: String) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    loop {
        let (mut sock, _peer) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => continue,
        };
        let resp = response.clone();
        tokio::spawn(async move {
            // Best-effort drain of the request line + headers so the client
            // side of the socket is happy before we reply.
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
            // Connection: close — dropping the socket ends the response.
        });
    }
}

// ---------------------------------------------------------------------------
// Node config
// ---------------------------------------------------------------------------

fn make_config(
    idx: usize,
    validator: &ClusterValidator,
    vl_site: &str,
    publisher_master_hex: &str,
    data_dir: std::path::PathBuf,
) -> NodeConfig {
    let mut cfg = NodeConfig::default();

    // RPC on a distinct loopback port; no WS (avoid the shared 0.0.0.0:6006).
    cfg.server.bind = format!("127.0.0.1:{}", RPC_PORTS[idx]).parse().unwrap();
    cfg.server.ws_bind = None;
    cfg.server.admin_ips = vec!["127.0.0.1".into()];

    // P2P: distinct port, cross-wired fixed peers, deterministic node identity.
    cfg.peer.port = PEER_PORTS[idx];
    cfg.peer.fixed_peers = (0..3)
        .filter(|j| *j != idx)
        .map(|j| format!("127.0.0.1:{}", PEER_PORTS[j]))
        .collect();
    cfg.peer.node_seed = Some(hex::encode(seed16(100 + idx as u8)));
    cfg.peer.seeds = vec![];
    // peer.tls_enabled left at default (true): localhost self-signed, no
    // hostname check.

    // Dynamic VL: sites + publisher key ONLY. No static validators.trusted.
    cfg.validators.enabled = true;
    cfg.validators.trusted = vec![];
    cfg.validators.validator_list_sites = vec![vl_site.to_string()];
    cfg.validators.validator_list_keys = vec![publisher_master_hex.to_string()];
    cfg.validators.require_trusted_validators = true;
    cfg.validators.quorum = Some(2);

    // Our own signing identity — SAME seeds embedded in the VL manifest.
    cfg.validator_identity.master_secret = Some(validator.master_secret_hex.clone());
    cfg.validator_identity.ephemeral_seed = Some(validator.ephemeral_seed_hex.clone());
    cfg.validator_identity.sequence = 1;

    // Same genesis + same network for all three. Use the amendment-free
    // genesis (master AccountRoot only) so ledger #2 does not trip the
    // amendment-blocked halt on a pre-activated amendment this build does not
    // know — a halt would suppress validation/proposals and prevent the
    // dynamic-VL aggregator quorum path from ever firing.
    cfg.network.network_id = NETWORK_ID;
    cfg.network.genesis_amendments_disabled = true;

    cfg.database.backend = "memory".into();
    cfg.database.path = data_dir;

    cfg
}

// ---------------------------------------------------------------------------
// RPC polling
// ---------------------------------------------------------------------------

struct NodeStatus {
    reachable: bool,
    peers: u64,
    quorum: u64,
    complete_ledgers: String,
    current_index: u64,
    validated_seq: u64,
    validated_hash: String,
}

async fn poll_server_info(client: &reqwest::Client, rpc_port: u16) -> NodeStatus {
    let url = format!("http://127.0.0.1:{}/", rpc_port);
    let body = json!({"method": "server_info", "params": [ {} ]});
    let mut st = NodeStatus {
        reachable: false,
        peers: 0,
        quorum: 0,
        complete_ledgers: String::new(),
        current_index: 0,
        validated_seq: 0,
        validated_hash: String::new(),
    };
    let resp = match client
        .post(&url)
        .json(&body)
        .timeout(Duration::from_secs(5))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return st,
    };
    let v: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return st,
    };
    let info = &v["result"]["info"];
    if !info.is_object() {
        return st;
    }
    st.reachable = true;
    st.peers = info["peers"].as_u64().unwrap_or(0);
    st.quorum = info["validation_quorum"].as_u64().unwrap_or(0);
    st.complete_ledgers = info["complete_ledgers"].as_str().unwrap_or("").to_string();
    st.current_index = info["ledger_current_index"].as_u64().unwrap_or(0);
    let vl = &info["validated_ledger"];
    st.validated_seq = vl["seq"].as_u64().unwrap_or(0);
    st.validated_hash = vl["hash"].as_str().unwrap_or("").to_string();
    st
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

// Live 3-process-style cluster: ~140s wall clock (dominated by the first-close
// grace + real close intervals), so it runs on demand, not in the default CI
// gate. Run with: `cargo test -p rxrpl-node --test dynamic_vl_live_cluster --
// --ignored --nocapture`. The fast regression guard for the underlying fix
// (VlFetcher -> ValidatorListVerified -> engine UNL) is the unit test
// `forwards_verified_vl_to_consensus` in crates/overlay/src/vl_fetcher.rs.
#[ignore = "live multi-node cluster, ~140s; run on demand"]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn dynamic_vl_live_cluster_converges() {
    // Capture rxrpl's own tracing to the test's stdout (visible with
    // --nocapture). RUST_LOG overrides the default `info`.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();

    // 1. Generate 3 validator keypairs (secp256k1) + 1 publisher (ed25519).
    let validators = vec![
        make_validator(10, 11),
        make_validator(20, 21),
        make_validator(30, 31),
    ];
    let publisher_master = kp("dynvl_cluster_publisher_master");
    let publisher_ephemeral = kp("dynvl_cluster_publisher_ephemeral");
    let publisher_master_hex = hex::encode(publisher_master.public_key.as_bytes());

    println!("== cluster validators (VL masters) ==");
    for (i, v) in validators.iter().enumerate() {
        println!("  v{i} master = {}", hex::encode(&v.master_pub));
    }
    println!("  publisher master = {publisher_master_hex}");

    // 2. Build the signed VL payload and PRE-FLIGHT verify it exactly as the
    //    fetcher will, so a malformed VL is caught here (not silently downstream).
    let vl_payload = build_vl_payload(&publisher_master, &publisher_ephemeral, &validators);
    {
        use rxrpl_overlay::validator_list;
        let payload: Value = serde_json::from_str(&vl_payload).unwrap();
        let manifest_bytes = B64.decode(payload["manifest"].as_str().unwrap()).unwrap();
        let mut store = manifest::ManifestStore::new();
        let parsed = validator_list::verify_and_parse(
            &manifest_bytes,
            payload["blob"].as_str().unwrap().as_bytes(),
            payload["signature"].as_str().unwrap().as_bytes(),
            &mut store,
        )
        .expect("PRE-FLIGHT: served VL must pass verify_and_parse");
        assert_eq!(parsed.validators.len(), 3, "VL lists the 3 cluster masters");
        assert_eq!(
            hex::encode(parsed.publisher_master_key.as_bytes()),
            publisher_master_hex,
            "VL publisher master matches the configured trust key"
        );
        println!(
            "PRE-FLIGHT ok: VL seq={} validators={} publisher={}",
            parsed.sequence,
            parsed.validators.len(),
            hex::encode(parsed.publisher_master_key.as_bytes())
        );
    }

    // 3. Start the local VL HTTP server (bind port 0 → learn the real port).
    let vl_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let vl_port = vl_listener.local_addr().unwrap().port();
    let vl_site = format!("http://127.0.0.1:{}/vl", vl_port);
    println!("VL served at {vl_site}");
    tokio::spawn(serve_vl_forever(vl_listener, vl_payload));

    // Sanity: fetch it once ourselves the way the node will.
    {
        let c = reqwest::Client::new();
        let got = c
            .get(&vl_site)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect("GET vl")
            .text()
            .await
            .expect("vl body");
        let j: Value = serde_json::from_str(&got).expect("vl body is JSON");
        assert!(j["blob"].is_string(), "served VL has a blob");
        println!("VL fetch sanity ok ({} bytes)", got.len());
    }

    // 4. Build 3 configs + spawn 3 run_networked tasks.
    let mut tmp_dirs = Vec::new();
    let mut nodes: Vec<Arc<Node>> = Vec::new();
    let mut handles = Vec::new();
    for (idx, v) in validators.iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = make_config(
            idx,
            v,
            &vl_site,
            &publisher_master_hex,
            dir.path().to_path_buf(),
        );
        tmp_dirs.push(dir);
        let node = Arc::new(Node::new_standalone(cfg, GENESIS).expect("build node"));
        let n = Arc::clone(&node);
        let handle = tokio::spawn(async move {
            if let Err(e) = n.run_networked(1, None, None).await {
                eprintln!("node run_networked error: {e}");
            }
        });
        nodes.push(node);
        handles.push(handle);
        println!(
            "spawned node {idx}: peer=127.0.0.1:{} rpc=127.0.0.1:{}",
            PEER_PORTS[idx], RPC_PORTS[idx]
        );
    }

    // 5. Poll each node's RPC up to the deadline. First close is gated by the
    //    ~60s cross-impl grace (fixed_peers non-empty), so allow generous time.
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(150);
    let mut converged: Option<(u64, String)> = None;
    let mut last: Vec<NodeStatus> = Vec::new();

    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let mut statuses = Vec::new();
        for &p in &RPC_PORTS {
            statuses.push(poll_server_info(&client, p).await);
        }
        let line: Vec<String> = statuses
            .iter()
            .map(|s| {
                if !s.reachable {
                    "down".to_string()
                } else {
                    format!(
                        "peers={} q={} cur={} val=#{}/{}",
                        s.peers,
                        s.quorum,
                        s.current_index,
                        s.validated_seq,
                        if s.validated_hash.is_empty() {
                            "-"
                        } else {
                            &s.validated_hash[..8.min(s.validated_hash.len())]
                        }
                    )
                }
            })
            .collect();
        println!(
            "[t+{:>3}s] {}",
            150 - (deadline - Instant::now()).as_secs().min(150),
            line.join("  |  ")
        );

        let all_reachable = statuses.iter().all(|s| s.reachable);
        // Require the validated tip to ADVANCE past the very first close
        // (seq >= 3): a frozen node can report an identical seq=2 via the
        // local-close fallback, but only the dynamic-VL aggregator quorum
        // path can keep advancing the network-validated tip round after round.
        let all_advanced = statuses.iter().all(|s| s.validated_seq >= 3);
        let hashes: Vec<&str> = statuses.iter().map(|s| s.validated_hash.as_str()).collect();
        let all_same = !hashes[0].is_empty() && hashes.iter().all(|h| *h == hashes[0]);

        last = statuses;
        if all_reachable && all_advanced && all_same {
            converged = Some((last[0].validated_seq, last[0].validated_hash.clone()));
            break;
        }
    }

    // 6. Report + assert.
    println!("\n================ FINAL STATE ================");
    for (i, s) in last.iter().enumerate() {
        if s.reachable {
            println!(
                "node {i}: peers={} quorum={} current_index={} complete={} validated=#{} hash={}",
                s.peers,
                s.quorum,
                s.current_index,
                s.complete_ledgers,
                s.validated_seq,
                s.validated_hash
            );
        } else {
            println!("node {i}: UNREACHABLE (RPC {})", RPC_PORTS[i]);
        }
    }

    match converged {
        Some((seq, hash)) => {
            println!("\nCONVERGED: all 3 nodes validated seq={seq} hash={hash}");
            assert!(
                seq >= 3,
                "converged seq must be >= 3 (tip advanced via quorum)"
            );
            for (i, s) in last.iter().enumerate() {
                assert!(s.reachable, "node {i} reachable");
                assert_eq!(s.validated_hash, hash, "node {i} agrees on validated hash");
                assert!(s.validated_seq >= 3, "node {i} validated seq >= 3");
            }
        }
        None => {
            println!("\nDID NOT CONVERGE within deadline. See per-node state above and the");
            println!("captured tracing for peer handshakes / VL fetch / consensus rounds.");
            panic!("cluster did not reach a common validated ledger at seq>=2");
        }
    }
}
