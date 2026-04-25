use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use crate::command::OverlayCommand;
use crate::peer_set::PeerSet;

/// Peer discovery via GetPeers protocol and seed nodes.
pub struct PeerDiscovery {
    seeds: Vec<String>,
    known_peers: Arc<RwLock<HashSet<String>>>,
    peer_set: Arc<PeerSet>,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<OverlayCommand>,
    max_peers: usize,
    interval: Duration,
}

impl PeerDiscovery {
    pub fn new(
        seeds: Vec<String>,
        peer_set: Arc<PeerSet>,
        cmd_tx: tokio::sync::mpsc::UnboundedSender<OverlayCommand>,
        max_peers: usize,
    ) -> Self {
        Self {
            seeds,
            known_peers: Arc::new(RwLock::new(HashSet::new())),
            peer_set,
            cmd_tx,
            max_peers,
            interval: Duration::from_secs(60),
        }
    }

    /// Bootstrap by connecting to seed nodes.
    pub async fn bootstrap(&self) {
        for seed in &self.seeds {
            if self.peer_set.len() >= self.max_peers {
                break;
            }
            tracing::info!("bootstrapping from seed: {}", seed);
            let _ = self
                .cmd_tx
                .send(OverlayCommand::ConnectTo { addr: seed.clone() });
            self.known_peers.write().await.insert(seed.clone());
        }
    }

    /// Run the periodic discovery loop.
    ///
    /// Monitors peer count and re-bootstraps from seeds if all peers
    /// are lost. Peer discovery with rippled happens passively via
    /// TMEndpoints messages that rippled sends automatically.
    pub async fn run_loop(&self) {
        let mut interval = tokio::time::interval(self.interval);
        interval.tick().await; // skip first immediate tick

        loop {
            interval.tick().await;

            let current_count = self.peer_set.len();
            if current_count >= self.max_peers {
                continue;
            }

            if current_count == 0 {
                self.bootstrap().await;
            }
        }
    }

    /// Process a received Peers response, connecting to new peers.
    pub async fn handle_peers_response(&self, peers: Vec<(String, u16)>) {
        // Bound the number of fresh addresses any single TMEndpoints message
        // can contribute, and the total set we remember. Without these, a
        // malicious peer can advertise tens of thousands of fake addresses
        // and force us into an unbounded HashSet (audit finding H7).
        const MAX_KNOWN_PEERS: usize = 50_000;
        const MAX_NEW_PER_RESPONSE: usize = 100;

        let mut accepted = 0usize;
        for (ip, port) in peers {
            if accepted >= MAX_NEW_PER_RESPONSE {
                break;
            }
            let addr = format!("{}:{}", ip, port);
            if self.peer_set.len() >= self.max_peers {
                break;
            }
            let mut known = self.known_peers.write().await;
            if known.contains(&addr) {
                continue;
            }
            if known.len() >= MAX_KNOWN_PEERS {
                tracing::debug!(
                    "known_peers cap reached ({}); ignoring further announcements",
                    MAX_KNOWN_PEERS
                );
                break;
            }
            known.insert(addr.clone());
            drop(known);
            accepted += 1;

            tracing::debug!("discovered new peer: {}", addr);
            let _ = self.cmd_tx.send(OverlayCommand::ConnectTo { addr });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bootstrap_sends_connect_commands() {
        let seeds = vec!["127.0.0.1:51235".to_string(), "127.0.0.1:51236".to_string()];
        let peer_set = Arc::new(PeerSet::new(10));
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();

        let discovery = PeerDiscovery::new(seeds, peer_set, cmd_tx, 10);
        discovery.bootstrap().await;

        // Should receive two ConnectTo commands
        let mut count = 0;
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                OverlayCommand::ConnectTo { .. } => count += 1,
                _ => panic!("unexpected command"),
            }
        }
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn handle_peers_response_deduplicates() {
        let peer_set = Arc::new(PeerSet::new(10));
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();

        let discovery = PeerDiscovery::new(vec![], peer_set, cmd_tx, 10);

        let peers = vec![
            ("10.0.0.1".to_string(), 51235),
            ("10.0.0.2".to_string(), 51235),
        ];
        discovery.handle_peers_response(peers.clone()).await;
        // Same peers again -- should be deduplicated
        discovery.handle_peers_response(peers).await;

        let mut count = 0;
        while cmd_rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 2); // only the first batch
    }

    #[tokio::test]
    async fn respects_max_peers() {
        let peer_set = Arc::new(PeerSet::new(1));
        // Fill peer_set to capacity
        let info = Arc::new(crate::peer_set::PeerInfo {
            node_id: rxrpl_primitives::Hash256::new([0x01; 32]),
            address: "127.0.0.1:9999".to_string(),
            inbound: false,
            ledger_seq: std::sync::atomic::AtomicU32::new(0),
            reputation: crate::reputation::PeerReputation::new(),
            scoring: crate::peer_score::PeerScore::new(),
            rate_limiter: crate::rate_limiter::PeerRateLimiter::default(),
            software: crate::peer_set::PeerSoftware::Unknown,
        });
        peer_set.add(info);

        let seeds = vec!["127.0.0.1:51235".to_string()];
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();

        let discovery = PeerDiscovery::new(seeds, peer_set, cmd_tx, 1);
        discovery.bootstrap().await;

        // Should not send any ConnectTo since we are at max_peers
        assert!(cmd_rx.try_recv().is_err());
    }
}
