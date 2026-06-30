//! On-demand soak test: drive standalone consensus for N ledger closes and
//! assert the RPC-observable per-ledger invariants hold the whole way.
//!
//! Marked `#[ignore]` so normal CI is unaffected; run on demand with:
//!   cargo test -p rxrpl-node --test soak_standalone -- --ignored --nocapture
//!
//! The close count defaults to 60 (~1 minute at 1-second closes) and can be
//! overridden via the `RXRPL_SOAK_CLOSES` env var (e.g. `RXRPL_SOAK_CLOSES=8`
//! for a quick ~8-second smoke run).
//!
//! Invariants checked over RPC, every close:
//!   - the closed-ledger sequence strictly increases;
//!   - each closed ledger hash is non-zero and differs from its parent;
//!   - the current (open) ledger index stays ahead of the last closed one;
//!   - the RPC server keeps responding (no panic/hang) for the whole run.

use rxrpl_config::NodeConfig;
use rxrpl_node::Node;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::time::Duration;

const GENESIS_ADDR: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

fn available_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

async fn rpc_call(addr: &SocketAddr, method: &str, params: Value) -> Value {
    let client = reqwest::Client::new();
    let body = json!({
        "method": method,
        "params": [params],
    });

    let resp = client
        .post(format!("http://{addr}/"))
        .json(&body)
        .send()
        .await
        .expect("rpc request should send");

    resp.json::<Value>()
        .await
        .expect("rpc response should decode as json")
}

#[tokio::test]
#[ignore = "soak: run on demand with --ignored"]
async fn soak_standalone_closes() {
    let target_closes: u32 = std::env::var("RXRPL_SOAK_CLOSES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    assert!(target_closes > 0, "RXRPL_SOAK_CLOSES must be positive");

    let port = available_port();
    let mut config = NodeConfig::default();
    config.server.bind = format!("127.0.0.1:{port}").parse().unwrap();
    config.server.ws_bind = None;

    let node = Node::new_standalone(config, GENESIS_ADDR).unwrap();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    // 1-second closes, as in `standalone_auto_close`.
    let handle = tokio::spawn(async move {
        node.run_standalone(1).await.unwrap();
    });

    // Let the server bind and the genesis settle.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let zero_hash = "0".repeat(64);

    // Baseline: the last closed ledger (genesis closes to seq 1).
    let base = rpc_call(&addr, "ledger_closed", json!({})).await;
    assert_eq!(
        base["result"]["status"], "success",
        "baseline ledger_closed must succeed: {:?}",
        base["result"]
    );
    let mut last_seq = base["result"]["ledger_index"]
        .as_u64()
        .expect("ledger_closed must report a numeric ledger_index");
    let mut last_hash = base["result"]["ledger_hash"]
        .as_str()
        .expect("ledger_closed must report a ledger_hash")
        .to_string();
    assert_ne!(last_hash, zero_hash, "genesis closed hash must be non-zero");

    let mut observed: u32 = 0;
    let mut polls_since_advance: u32 = 0;
    // Bound consecutive stuck polls so a hung node fails instead of running
    // forever. A healthy 1-second close advances within ~4 polls (250ms each);
    // this bound (~25s of no progress) only trips on a genuine hang.
    const MAX_STUCK_POLLS: u32 = 100;

    while observed < target_closes {
        tokio::time::sleep(Duration::from_millis(250)).await;

        let resp = rpc_call(&addr, "ledger_closed", json!({})).await;
        let result = &resp["result"];
        assert_eq!(
            result["status"], "success",
            "ledger_closed must keep succeeding: {result:?}"
        );

        let seq = result["ledger_index"]
            .as_u64()
            .expect("ledger_index must be numeric");
        let hash = result["ledger_hash"]
            .as_str()
            .expect("ledger_hash must be a string")
            .to_string();

        if seq == last_seq {
            polls_since_advance += 1;
            assert!(
                polls_since_advance < MAX_STUCK_POLLS,
                "no new close after {polls_since_advance} polls (stuck at seq {last_seq})"
            );
            continue;
        }

        // A new close landed: assert the per-ledger invariants.
        assert!(
            seq > last_seq,
            "sequence must strictly increase: {seq} !> {last_seq}"
        );
        assert_ne!(
            hash, zero_hash,
            "closed ledger #{seq} hash must be non-zero"
        );
        assert_ne!(
            hash, last_hash,
            "closed ledger #{seq} hash must differ from previous #{last_seq}"
        );

        // Cross-check: the current open index is ahead of the last close.
        let cur = rpc_call(&addr, "ledger", json!({ "ledger_index": "current" })).await;
        let cur_idx = cur["result"]["ledger"]["ledger_index"]
            .as_u64()
            .expect("current ledger_index must be numeric");
        assert!(
            cur_idx > seq,
            "current open index {cur_idx} must be ahead of last closed {seq}"
        );

        // Count every closed seq, even if more than one advanced between polls.
        observed += (seq - last_seq) as u32;
        last_seq = seq;
        last_hash = hash;
        polls_since_advance = 0;
    }

    println!("soak ok: observed {observed} closes, final closed seq = {last_seq}");

    handle.abort();
}
