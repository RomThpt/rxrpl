//! rippled-compatible peer `/crawl` endpoint.
//!
//! Network explorers (e.g. livenet.xrpl.org) discover the topology by issuing
//! `GET /crawl` over HTTPS on the peer port and reading `overlay.active`. We
//! serve the same version-2 JSON so this node appears on the explorer and acts
//! as a discovery hop for its own peers.

use std::sync::Arc;

use base64::Engine;
use serde_json::{Map, Value, json};

use crate::handshake::encode_node_public_key;
use crate::identity::NodeIdentity;
use crate::peer_set::{PeerInfo, PeerSoftware};

/// Server-level fields for the crawl response that only the node layer knows
/// (the overlay has no view of the ledger window or process uptime).
#[derive(Clone, Debug)]
pub struct CrawlServerSnapshot {
    pub build_version: String,
    pub server_state: String,
    pub complete_ledgers: String,
    pub uptime_secs: u64,
}

/// Supplies the [`CrawlServerSnapshot`] for the `server` object. Implemented by
/// the RPC server's `ServerContext` so the crawl reuses the exact data
/// `server_info` reports. Sync by design: the crawl is served from the inbound
/// accept path, so the implementor must not block.
pub trait CrawlInfo: Send + Sync {
    fn crawl_snapshot(&self) -> CrawlServerSnapshot;
}

/// Whether an HTTP request target addresses the crawl endpoint.
pub fn is_crawl_request(path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    matches!(path, "/crawl" | "/crawl/")
}

/// Build the version-2 crawl JSON document.
pub fn build_crawl_json(
    identity: &NodeIdentity,
    network_id: u32,
    peers: &[Arc<PeerInfo>],
    server: Option<&CrawlServerSnapshot>,
) -> Value {
    let active: Vec<Value> = peers.iter().map(|p| peer_entry(p)).collect();

    let mut server_obj = Map::new();
    if let Some(s) = server {
        server_obj.insert("build_version".into(), json!(s.build_version));
        server_obj.insert("complete_ledgers".into(), json!(s.complete_ledgers));
        server_obj.insert("server_state".into(), json!(s.server_state));
        server_obj.insert("uptime".into(), json!(s.uptime_secs));
    }
    server_obj.insert("network_id".into(), json!(network_id));
    server_obj.insert("peers".into(), json!(peers.len()));
    server_obj.insert(
        "pubkey_node".into(),
        json!(encode_node_public_key(identity.public_key_bytes())),
    );

    json!({
        "version": 2,
        "server": Value::Object(server_obj),
        "overlay": { "active": active },
    })
}

/// Serialize the crawl JSON into a complete HTTP/1.1 `200 OK` response.
pub fn build_response_bytes(doc: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(doc).unwrap_or_default();
    let mut out = format!(
        "HTTP/1.1 200 OK\r\nServer: rxrpl\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(&body);
    out
}

fn peer_entry(peer: &PeerInfo) -> Value {
    let (ip, port) = split_address(&peer.address);

    let mut entry = Map::new();
    entry.insert("ip".into(), json!(ip));
    entry.insert(
        "public_key".into(),
        json!(base64::engine::general_purpose::STANDARD.encode(&peer.public_key)),
    );
    entry.insert(
        "type".into(),
        json!(if peer.inbound { "in" } else { "out" }),
    );
    entry.insert(
        "uptime".into(),
        json!(peer.connected_at.elapsed().as_secs()),
    );
    if let Some(version) = software_version(&peer.software) {
        entry.insert("version".into(), json!(version));
    }
    // Only outbound peers carry a routable listening port (the address we
    // dialed). An inbound peer's socket port is the ephemeral source port,
    // which rippled likewise omits.
    if !peer.inbound {
        if let Some(port) = port {
            entry.insert("port".into(), json!(port));
        }
    }
    Value::Object(entry)
}

fn split_address(address: &str) -> (&str, Option<u16>) {
    match address.rsplit_once(':') {
        Some((ip, port)) => (ip, port.parse().ok()),
        None => (address, None),
    }
}

fn software_version(software: &PeerSoftware) -> Option<String> {
    match software {
        PeerSoftware::Rxrpl(v) => Some(format!("rxrpl-{v}")),
        PeerSoftware::Rippled(v) => Some(format!("rippled-{v}")),
        PeerSoftware::Other(s) => Some(s.clone()),
        PeerSoftware::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::time::Instant;

    use crate::peer_score::PeerScore;
    use crate::reputation::PeerReputation;
    use rxrpl_primitives::Hash256;

    fn peer(addr: &str, inbound: bool, software: PeerSoftware) -> Arc<PeerInfo> {
        Arc::new(PeerInfo {
            node_id: Hash256::new([1; 32]),
            address: addr.to_string(),
            inbound,
            public_key: vec![0x03; 33],
            connected_at: Instant::now(),
            ledger_seq: AtomicU32::new(0),
            reputation: PeerReputation::new(),
            scoring: PeerScore::new(),
            rate_limiter: crate::rate_limiter::PeerRateLimiter::default(),
            software,
        })
    }

    #[test]
    fn crawl_path_matching() {
        assert!(is_crawl_request("/crawl"));
        assert!(is_crawl_request("/crawl/"));
        assert!(is_crawl_request("/crawl?foo=1"));
        assert!(!is_crawl_request("/"));
        assert!(!is_crawl_request("/crawlx"));
    }

    #[test]
    fn inbound_entry_has_no_port_outbound_does() {
        let id = NodeIdentity::generate();
        let peers = vec![
            peer("1.2.3.4:40000", true, PeerSoftware::Rippled("3.1.3".into())),
            peer("5.6.7.8:51235", false, PeerSoftware::Rxrpl("0.1.0".into())),
        ];
        let doc = build_crawl_json(&id, 0, &peers, None);
        let active = doc["overlay"]["active"].as_array().unwrap();

        let inbound = &active[0];
        assert_eq!(inbound["ip"], "1.2.3.4");
        assert_eq!(inbound["type"], "in");
        assert_eq!(inbound["version"], "rippled-3.1.3");
        assert!(inbound.get("port").is_none());

        let outbound = &active[1];
        assert_eq!(outbound["ip"], "5.6.7.8");
        assert_eq!(outbound["type"], "out");
        assert_eq!(outbound["version"], "rxrpl-0.1.0");
        assert_eq!(outbound["port"], 51235);
    }

    #[test]
    fn public_key_is_base64_of_33_bytes() {
        let id = NodeIdentity::generate();
        let peers = vec![peer("1.2.3.4:40000", true, PeerSoftware::Unknown)];
        let doc = build_crawl_json(&id, 0, &peers, None);
        let pk = doc["overlay"]["active"][0]["public_key"].as_str().unwrap();
        // 33 bytes -> 44 base64 chars, no padding (33 is divisible by 3).
        assert_eq!(pk.len(), 44);
        assert!(!pk.contains('='));
        // Unknown software -> version field omitted.
        assert!(doc["overlay"]["active"][0].get("version").is_none());
    }

    #[test]
    fn server_object_carries_snapshot_and_overlay_fields() {
        let id = NodeIdentity::generate();
        let snap = CrawlServerSnapshot {
            build_version: "9.9.9".into(),
            server_state: "full".into(),
            complete_ledgers: "1-100".into(),
            uptime_secs: 42,
        };
        let doc = build_crawl_json(&id, 1024, &[], Some(&snap));
        assert_eq!(doc["version"], 2);
        assert_eq!(doc["server"]["build_version"], "9.9.9");
        assert_eq!(doc["server"]["complete_ledgers"], "1-100");
        assert_eq!(doc["server"]["server_state"], "full");
        assert_eq!(doc["server"]["uptime"], 42);
        assert_eq!(doc["server"]["network_id"], 1024);
        assert_eq!(doc["server"]["peers"], 0);
        assert!(
            doc["server"]["pubkey_node"]
                .as_str()
                .unwrap()
                .starts_with('n')
        );
    }

    #[test]
    fn response_bytes_are_well_formed_http() {
        let id = NodeIdentity::generate();
        let doc = build_crawl_json(&id, 0, &[], None);
        let bytes = build_response_bytes(&doc);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Type: application/json\r\n"));
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        let cl: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(cl, body.len());
    }
}
