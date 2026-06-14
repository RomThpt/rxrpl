//! Deterministic catchup throughput benchmark.
//!
//! Mainnet catchup tuning was impossible to measure: single live runs vary
//! wildly with peer quality and tip activity, so two strategies could not be
//! compared. This models a peer serving a large account-state SHAMap with
//! rippled-like `getNodeFat` semantics -- fat subtrees up to `query_depth`,
//! capped per response, with at most `node_cap` node-ids per request, fanned
//! across `peers` parallel requests per round -- and drives the real
//! `LedgerSyncer` catchup loop to completion.
//!
//! One "round" is one fan-out of parallel requests whose replies are all fed
//! back, i.e. one network round-trip. Fewer rounds = faster wall clock. The
//! matrix below isolates the effect of fan-out, batch size and query depth so
//! catchup strategies can be A/B'd reproducibly, without mainnet noise.
//!
//! Run: `cargo run --release --example catchup_bench`

use std::sync::Arc;
use std::time::Instant;

use rxrpl_overlay::ledger_sync::{FeedResult, LedgerSyncer};
use rxrpl_primitives::Hash256;
use rxrpl_shamap::{InMemoryNodeStore, NodeId, SHAMap};

const WIRE_TYPE_ACCOUNT_STATE: u8 = 1;
const WIRE_TYPE_INNER: u8 = 2;

/// Build a full account-state SHAMap with `count` well-spread leaves.
fn build_server(count: usize, item_bytes: usize) -> (SHAMap, Hash256) {
    let store = Arc::new(InMemoryNodeStore::new());
    let mut map = SHAMap::account_state_with_store(store);
    for i in 0..count {
        let key = rxrpl_crypto::sha512_half::sha512_half(&[b"k", &(i as u64).to_be_bytes()]);
        let data = vec![(i & 0xFF) as u8; item_bytes];
        map.put(key, data).unwrap();
    }
    let root = map.root_hash();
    (map, root)
}

/// Child SHAMapNodeID for `branch`, mirroring `SHAMap::collect_missing`.
fn child_node_id(parent: NodeId, branch: u8) -> NodeId {
    let depth = parent.depth();
    let mut path = *parent.id().as_bytes();
    let bi = (depth / 2) as usize;
    if depth & 1 == 0 {
        path[bi] = (path[bi] & 0x0F) | (branch << 4);
    } else {
        path[bi] = (path[bi] & 0xF0) | branch;
    }
    NodeId::new(depth + 1, &Hash256::new(path))
}

/// rippled TMLedgerNode wire form: inner = content||0x02, leaf = data||key||0x01.
fn wire_of(bytes: &[u8], is_inner: bool) -> Vec<u8> {
    let mut w = Vec::with_capacity(bytes.len() + 1);
    if is_inner {
        w.extend_from_slice(bytes);
        w.push(WIRE_TYPE_INNER);
    } else {
        w.extend_from_slice(&bytes[32..]);
        w.extend_from_slice(&bytes[..32]);
        w.push(WIRE_TYPE_ACCOUNT_STATE);
    }
    w
}

/// Collect a fat subtree rooted at `id` down `depth` levels into `out`, stopping
/// at `cap` total nodes (models `getNodeFat`).
fn collect_fat(
    server: &SHAMap,
    id: NodeId,
    depth: u8,
    cap: usize,
    horizon: Option<u8>,
    out: &mut Vec<(Vec<u8>, Vec<u8>)>,
) {
    if out.len() >= cap {
        return;
    }
    // Models an aged target: rippled keeps recent ledgers fully in memory but
    // flushes deeper nodes of older ones, so deep requests return nothing.
    if let Some(h) = horizon {
        if id.depth() > h {
            return;
        }
    }
    let Some((_h, bytes, is_inner)) = server.node_at(id) else {
        return;
    };
    out.push((id.to_wire_bytes(), wire_of(&bytes, is_inner)));
    if is_inner && depth > 0 {
        for b in 0..16u8 {
            let off = b as usize * 32;
            if bytes[off..off + 32] != [0u8; 32] {
                collect_fat(server, child_node_id(id, b), depth - 1, cap, horizon, out);
                if out.len() >= cap {
                    return;
                }
            }
        }
    }
}

/// Serve one peer request: fat subtrees for each requested id, capped overall.
fn serve(
    server: &SHAMap,
    ids: &[NodeId],
    depth: u8,
    resp_cap: usize,
    horizon: Option<u8>,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for &id in ids {
        if out.len() >= resp_cap {
            break;
        }
        collect_fat(server, id, depth, resp_cap, horizon, &mut out);
    }
    out
}

struct Cfg {
    label: &'static str,
    peers: usize,
    batch: usize,
    node_cap: usize, // max node-ids per request (server-side cap)
    depth: u8,
    resp_cap: usize,     // max nodes per response (response-size cap)
    horizon: Option<u8>, // max depth the (aged) target still serves
}

struct Stats {
    completed: bool,
    rounds: u32,
    requests: u64,
    served: u64,
    leaves: usize,
}

fn run(server: &SHAMap, root: Hash256, cfg: &Cfg) -> Stats {
    let store = Arc::new(InMemoryNodeStore::new());
    let mut syncer = LedgerSyncer::new();
    let seq = 2u32;
    syncer.set_ledger_hash(seq, root);
    let _ = syncer.start_incremental_sync(seq, root, store);

    let mut rounds = 0u32;
    let mut requests = 0u64;
    let mut served = 0u64;
    let mut completed = false;
    let mut leaves = 0usize;
    let mut stall_rounds = 0u32;
    let mut last_added = 0u64;

    while rounds < 1_000_000 {
        let mut missing = syncer.get_missing_node_ids(seq);
        if missing.is_empty() {
            if let Some(l) = syncer.try_complete_sync(seq) {
                completed = true;
                leaves = l.len();
            }
            break;
        }
        // Stall detection: no new nodes for many rounds = wall (target won't
        // serve what is still missing). Mirrors the mainnet plateau.
        if syncer.lifetime_added() == last_added {
            stall_rounds += 1;
            if stall_rounds > 50 {
                break;
            }
        } else {
            stall_rounds = 0;
            last_added = syncer.lifetime_added();
        }
        missing.truncate(cfg.batch);

        // Fan out across up to `peers` parallel requests of <= node_cap ids each.
        let mut all: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut chunk_start = 0;
        let mut peer = 0;
        while chunk_start < missing.len() && peer < cfg.peers {
            let end = (chunk_start + cfg.node_cap).min(missing.len());
            let ids: Vec<NodeId> = missing[chunk_start..end]
                .iter()
                .map(|m| m.node_id)
                .collect();
            let resp = serve(server, &ids, cfg.depth, cfg.resp_cap, cfg.horizon);
            requests += 1;
            served += resp.len() as u64;
            all.extend(resp);
            chunk_start = end;
            peer += 1;
        }

        if let FeedResult::Complete(l) = syncer.feed_nodes(seq, &all) {
            completed = true;
            leaves = l.len();
            break;
        }
        rounds += 1;
    }

    Stats {
        completed,
        rounds,
        requests,
        served,
        leaves,
    }
}

fn main() {
    let count: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50_000);
    let item_bytes = 80usize;

    println!("Building server: {count} account-state leaves...");
    let t = Instant::now();
    let (server, root) = build_server(count, item_bytes);
    println!("built in {:?}, root={}", t.elapsed(), root);

    let configs = [
        Cfg {
            label: "baseline   p3 b512 d3",
            peers: 3,
            batch: 512,
            node_cap: 128,
            depth: 3,
            resp_cap: 500,
            horizon: None,
        },
        Cfg {
            label: "morepeers  p8 b1024 d3",
            peers: 8,
            batch: 1024,
            node_cap: 128,
            depth: 3,
            resp_cap: 500,
            horizon: None,
        },
        Cfg {
            label: "deeper     p3 b512 d4",
            peers: 3,
            batch: 512,
            node_cap: 128,
            depth: 4,
            resp_cap: 500,
            horizon: None,
        },
        Cfg {
            label: "bigresp    resp_cap=2000",
            peers: 8,
            batch: 1024,
            node_cap: 128,
            depth: 4,
            resp_cap: 2000,
            horizon: None,
        },
        // Aged target: server only serves down to a shallow depth -> the deep
        // frontier can never be fetched. Reproduces the mainnet wall in-process:
        // throughput knobs do not matter, the catchup simply cannot complete.
        Cfg {
            label: "AGED horizon<=4 p8",
            peers: 8,
            batch: 1024,
            node_cap: 128,
            depth: 4,
            resp_cap: 2000,
            horizon: Some(4),
        },
        Cfg {
            label: "AGED horizon<=4 p3",
            peers: 3,
            batch: 512,
            node_cap: 128,
            depth: 3,
            resp_cap: 500,
            horizon: Some(4),
        },
    ];

    println!(
        "\n{:<24} {:>8} {:>8} {:>9} {:>10} {:>7}",
        "config", "rounds", "reqs", "served", "leaves", "done"
    );
    for cfg in &configs {
        let t = Instant::now();
        let s = run(&server, root, cfg);
        println!(
            "{:<24} {:>8} {:>8} {:>9} {:>10} {:>7}  ({:?})",
            cfg.label,
            s.rounds,
            s.requests,
            s.served,
            s.leaves,
            s.completed,
            t.elapsed()
        );
    }
    println!("\nrounds = network round-trips to complete (lower is faster).");
    println!("served = total nodes returned (incl. redundant); reqs = peer requests issued.");
}
