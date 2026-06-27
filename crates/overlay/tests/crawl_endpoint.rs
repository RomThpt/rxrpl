//! End-to-end test of the peer-port `/crawl` endpoint: a TLS client issues
//! `GET /crawl` against a running `PeerManager` and receives version-2 JSON,
//! exactly as a network explorer's crawler would.

use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use rxrpl_overlay::identity::NodeIdentity;
use rxrpl_overlay::{CrawlInfo, CrawlServerSnapshot, PeerManager, PeerManagerConfig, tls};
use rxrpl_primitives::Hash256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

struct FakeCrawlInfo;

impl CrawlInfo for FakeCrawlInfo {
    fn crawl_snapshot(&self) -> CrawlServerSnapshot {
        CrawlServerSnapshot {
            build_version: "test-1.2.3".into(),
            server_state: "full".into(),
            complete_ledgers: "1-42".into(),
            uptime_secs: 99,
        }
    }
}

fn config(identity: &NodeIdentity, port: u16) -> PeerManagerConfig {
    PeerManagerConfig {
        listen_port: port,
        max_peers: 10,
        seeds: vec![],
        fixed_peers: vec![],
        network_id: 1234,
        tls_server: tls::build_server_config(identity),
        tls_client: tls::build_client_config(),
        cluster_enabled: false,
        cluster_node_name: String::new(),
        cluster_members: Vec::new(),
        cluster_broadcast_interval_secs: 5,
    }
}

#[tokio::test]
async fn crawl_endpoint_serves_version2_json() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let identity = Arc::new(NodeIdentity::from_seed(
        &rxrpl_crypto::Seed::from_passphrase("crawl-node"),
    ));
    let (mut mgr, _cmd_tx, _consensus_rx) = PeerManager::new(
        Arc::clone(&identity),
        config(&identity, port),
        Arc::new(AtomicU32::new(0)),
        Arc::new(RwLock::new(Hash256::ZERO)),
    );
    mgr.set_crawl_info(Arc::new(FakeCrawlInfo));

    let handle = tokio::spawn(async move {
        let _ = mgr.run().await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client_cfg = tls::build_client_config();
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let mut stream = tls::connect_tls(tcp, &client_cfg).await.unwrap();
    stream
        .write_all(b"GET /crawl HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    stream.flush().await.unwrap();

    let mut buf = Vec::new();
    // The server sends `Connection: close` and drops the stream after replying,
    // so read_to_end returns once the response is fully delivered.
    let _ = tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut buf)).await;
    handle.abort();

    let text = String::from_utf8_lossy(&buf);
    assert!(
        text.starts_with("HTTP/1.1 200 OK"),
        "unexpected response: {text}"
    );
    let body = text.split_once("\r\n\r\n").expect("response has body").1;
    let doc: serde_json::Value = serde_json::from_str(body).expect("valid JSON body");

    assert_eq!(doc["version"], 2);
    assert_eq!(doc["server"]["build_version"], "test-1.2.3");
    assert_eq!(doc["server"]["complete_ledgers"], "1-42");
    assert_eq!(doc["server"]["network_id"], 1234);
    assert!(
        doc["server"]["pubkey_node"]
            .as_str()
            .unwrap()
            .starts_with('n')
    );
    assert!(doc["overlay"]["active"].is_array());
}
